//! A connection's protocol without a stream of its own: the handshake and
//! every command as an `async fn` over a [`Transport`]. [`crate::Conn`] runs
//! them over a blocking stream (see `transport.rs`), which is where they
//! are documented; an event loop runs them with a transport of its own.

use std::fs::File;
use std::io;
use std::path::Path;
use std::path::PathBuf;

use crate::auth::AuthPlugin;
use crate::auth::parsec_max_factor;
use crate::binary::Execute;
use crate::buf::put_nul_bytes;
use crate::compress::Compression;
use crate::conn::Opts;
use crate::conn::PipelineTotals;
use crate::conn::Statement;
use crate::conn::StatementCache;
use crate::conn::load_public_key;
use crate::conn::local_infile_dir;
use crate::constants::command;
use crate::constants::status;
use crate::error::Error;
use crate::error::Result;
use crate::infile;
use crate::infile::Infile;
use crate::login::Auth;
use crate::login::AuthSettings;
use crate::login::AuthStep;
use crate::login::Login;
use crate::packet::ReplyStart;
use crate::packet::Wire;
use crate::packet::release;
use crate::result::QueryResult;
use crate::result::RowSet;
use crate::session::HeadStep;
use crate::session::Prepared;
use crate::session::ReadOutcome;
use crate::session::ReplyStep;
use crate::session::Session;
use crate::session::StreamId;
use crate::sql::local_infile_names;
use crate::transport::StartTls;
use crate::transport::Transport;

/// The server could not re-prepare a statement after a schema change.
const ER_NEED_REPREPARE: u16 = 1615;
/// The server holds `max_prepared_stmt_count` statements across all its
/// connections.
const ER_MAX_PREPARED_STMT_COUNT_REACHED: u16 = 1461;

/// The protocol state of an authenticated connection.
#[derive(Debug)]
pub struct Client {
    pub(crate) wire: Wire,
    /// Protocol state between commands: status, the pending result set.
    pub(crate) session: Session,
    pub(crate) server_version: String,
    pub(crate) connection_id: u32,
    /// Default plugin and nonce from the greeting, reused by `COM_CHANGE_USER`.
    pub(crate) default_plugin: AuthPlugin,
    pub(crate) greeting_nonce: Vec<u8>,
    /// What authentication needs besides the password, also for
    /// `COM_CHANGE_USER`.
    pub(crate) auth: AuthSettings,
    /// Session collation sent in the handshake, also for `COM_CHANGE_USER`.
    pub(crate) collation: u8,
    /// The server's default collation, from its greeting.
    pub(crate) server_collation: u8,
    /// Scratch buffer for control packets.
    pub(crate) buf: Vec<u8>,
    /// Statements prepared by `execute_cached`, by SQL.
    pub(crate) statements: StatementCache,
    /// Compression in use, negotiated in the handshake.
    pub(crate) compression: Option<Compression>,
    /// `Opts::local_infile_dir`, resolved.
    pub(crate) local_infile_dir: Option<PathBuf>,
    /// Local files the current command may send.
    pub(crate) infile: Infile,
}

impl Client {
    /// Status flags from the last OK/EOF packet (see [`crate::constants::status`]).
    #[must_use]
    pub const fn status_flags(&self) -> u16 {
        self.session.status
    }

    /// Whether the server has more results queued for the last query.
    #[must_use]
    pub const fn has_more_results(&self) -> bool {
        self.session.has_more_results()
    }

    /// Compression in use, if any.
    #[must_use]
    pub const fn compression(&self) -> Option<Compression> {
        self.compression
    }

    #[must_use]
    pub fn server_version(&self) -> &str {
        &self.server_version
    }

    #[must_use]
    pub const fn connection_id(&self) -> u32 {
        self.connection_id
    }

    /// Collation id requested in the handshake.
    #[must_use]
    pub const fn collation(&self) -> u8 {
        self.collation
    }

    /// The server's default character set, from its greeting.
    #[must_use]
    pub fn server_charset(&self) -> Option<&'static str> {
        crate::charset::charset_of(self.server_collation)
    }

    /// Opens a session over `io`, a stream just connected to the server:
    /// greeting, TLS when agreed on, handshake and authentication, as
    /// [`crate::Conn::connect`] does over its own. `secure` says whether the
    /// stream is already confidential (a Unix socket).
    ///
    /// # Errors
    ///
    /// As [`crate::Conn::connect`].
    pub async fn connect<T: StartTls>(io: &mut T, opts: &Opts, secure: bool) -> Result<Self> {
        // Settings first: a bad one fails before anything goes on the wire.
        let local_infile_dir = local_infile_dir(opts)?;
        let mut wire = Wire::default();
        let mut buf = Vec::new();
        wire.read_into(io, &mut buf).await?;
        let login = Login::new(opts, &buf, local_infile_dir.is_some())?;
        if let Some(request) = login.ssl_request() {
            // SSLRequest goes out in plain text, then TLS starts on the same
            // stream and the sequence continues inside it.
            wire.write_packet(io, &request).await?;
            if !wire.is_idle() {
                return Err(Error::protocol("unexpected data before the TLS handshake"));
            }
            io.start_tls(&opts.tls, &opts.host).await?;
        }
        let secure = secure || io.is_tls();
        Self::authenticate(io, wire, login, opts, buf, local_infile_dir, secure).await
    }

    /// The handshake from the response to the greeting on, once TLS (if
    /// any) is up: authentication, then the session as agreed.
    pub(crate) async fn authenticate<T: Transport>(
        io: &mut T,
        mut wire: Wire,
        login: Login,
        opts: &Opts,
        buf: Vec<u8>,
        local_infile_dir: Option<PathBuf>,
        secure: bool,
    ) -> Result<Self> {
        let auth = AuthSettings {
            pinned_key: opts
                .server_public_key
                .as_deref()
                .map(load_public_key)
                .transpose()?,
            get_server_public_key: opts.get_server_public_key,
            parsec_max_factor: parsec_max_factor(opts.connect_timeout),
            secure,
        };
        let (response, exchange) = login.response(opts, &auth)?;
        let Login {
            greeting,
            capabilities,
            collation,
            compression,
            plugin,
            ..
        } = login;
        wire.write_packet(io, &response).await?;
        let mut client = Self {
            wire,
            session: Session::new(capabilities, greeting.status),
            server_version: greeting.server_version,
            connection_id: greeting.connection_id,
            default_plugin: plugin,
            greeting_nonce: greeting.nonce,
            auth,
            collation,
            server_collation: greeting.collation,
            buf,
            statements: StatementCache::new(opts.statement_cache),
            compression,
            local_infile_dir,
            infile: Infile::Refuse,
        };
        client
            .auth_exchange(io, exchange, opts.password.as_bytes())
            .await?;
        client.wire.authenticated();
        if let Some(algorithm) = compression {
            client.wire.compress(algorithm)?;
        }
        Ok(client)
    }

    /// Runs the server-driven part of authentication (auth switches and
    /// plugin-specific round trips) until OK or ERR.
    pub(crate) async fn auth_exchange<T: Transport>(
        &mut self,
        io: &mut T,
        mut auth: Auth,
        password: &[u8],
    ) -> Result<()> {
        loop {
            self.read_packet(io).await?;
            match auth.step(&self.buf, password, &self.auth)? {
                AuthStep::Send(reply) => self.wire.write_packet(io, &reply).await?,
                AuthStep::Read => {}
                AuthStep::Done { status } => {
                    self.session.status = status;
                    return Ok(());
                }
            }
        }
    }

    async fn read_packet<T: Transport>(&mut self, io: &mut T) -> Result<()> {
        self.buf.clear();
        self.wire.read_into(io, &mut self.buf).await.map(|_| ())
    }

    /// [`crate::Conn::query`], over `io`.
    ///
    /// # Errors
    ///
    /// As [`crate::Conn::query`].
    pub async fn query<T: Transport>(&mut self, io: &mut T, sql: &[u8]) -> Result<QueryResult> {
        let (result, stream) = self.query_streamed(io, sql).await?;
        self.read_all(io, result, stream).await
    }

    /// [`crate::Conn::query_streamed`], over `io`.
    ///
    /// # Errors
    ///
    /// As [`crate::Conn::query_streamed`].
    pub async fn query_streamed<T: Transport>(
        &mut self,
        io: &mut T,
        sql: &[u8],
    ) -> Result<(QueryResult, Option<StreamId>)> {
        self.drain_results(io).await?;
        self.infile = self.allowed_files(sql);
        self.wire.write_command(io, command::QUERY, sql).await?;
        self.session.binary_reply = false;
        self.read_head(io).await
    }

    /// [`crate::Conn::prepare`], over `io`.
    ///
    /// # Errors
    ///
    /// As [`crate::Conn::prepare`].
    pub async fn prepare<T: Transport>(&mut self, io: &mut T, sql: &[u8]) -> Result<Statement> {
        self.drain_results(io).await?;
        self.wire
            .write_command(io, command::STMT_PREPARE, sql)
            .await?;
        let mut reply = Prepared::default();
        loop {
            self.read_packet(io).await?;
            if let Some(statement) = self.session.prepare_step(&mut reply, &self.buf)? {
                return Ok(statement);
            }
        }
    }

    /// [`crate::Conn::execute_streamed`], over `io`.
    ///
    /// # Errors
    ///
    /// As [`crate::Conn::execute_streamed`].
    pub async fn execute_streamed<T: Transport>(
        &mut self,
        io: &mut T,
        payload: &[u8],
    ) -> Result<(QueryResult, Option<StreamId>)> {
        self.drain_results(io).await?;
        self.wire
            .write_command(io, command::STMT_EXECUTE, payload)
            .await?;
        self.session.binary_reply = true;
        self.read_head(io).await
    }

    /// [`crate::Conn::execute`], over `io`.
    ///
    /// # Errors
    ///
    /// As [`crate::Conn::execute`].
    pub async fn execute<T: Transport>(
        &mut self,
        io: &mut T,
        payload: &[u8],
    ) -> Result<QueryResult> {
        let (result, stream) = self.execute_streamed(io, payload).await?;
        self.read_all(io, result, stream).await
    }

    /// [`crate::Conn::close_statement`], over `io`.
    ///
    /// # Errors
    ///
    /// As [`crate::Conn::close_statement`].
    pub async fn close_statement<T: Transport>(&mut self, io: &mut T, id: u32) -> Result<()> {
        self.drain_results(io).await?;
        self.wire
            .write_command(io, command::STMT_CLOSE, &id.to_le_bytes())
            .await
    }

    /// [`crate::Conn::execute_cached_streamed`], over `io`.
    ///
    /// # Errors
    ///
    /// As [`crate::Conn::execute_cached_streamed`].
    pub async fn execute_cached_streamed<T: Transport>(
        &mut self,
        io: &mut T,
        sql: &[u8],
        params: &Execute,
    ) -> Result<(QueryResult, Option<StreamId>)> {
        let id = self.cached_statement(io, sql, params).await?;
        match self.execute_streamed(io, &params.finish(id)).await {
            // The server could not re-prepare it after a schema change:
            // prepare it again, once.
            Err(Error::Server {
                code: ER_NEED_REPREPARE,
                ..
            }) => {
                if let Some(id) = self.statements.remove(sql) {
                    self.close_statement(io, id).await?;
                }
                let id = self.cached_statement(io, sql, params).await?;
                self.execute_streamed(io, &params.finish(id)).await
            }
            other => other,
        }
    }

    /// [`crate::Conn::execute_cached`], over `io`.
    ///
    /// # Errors
    ///
    /// As [`crate::Conn::execute_cached`].
    pub async fn execute_cached<T: Transport>(
        &mut self,
        io: &mut T,
        sql: &[u8],
        params: &Execute,
    ) -> Result<QueryResult> {
        let (result, stream) = self.execute_cached_streamed(io, sql, params).await?;
        self.read_all(io, result, stream).await
    }

    /// The id of `sql` prepared on this connection, preparing it (and
    /// closing the least recently used statement) if needed.
    async fn cached_statement<T: Transport>(
        &mut self,
        io: &mut T,
        sql: &[u8],
        params: &Execute,
    ) -> Result<u32> {
        let expected = if let Some(statement) = self.statements.get(sql) {
            (statement.id, statement.params)
        } else {
            if let Some(oldest) = self.statements.evict() {
                self.close_statement(io, oldest).await?;
            }
            let statement = match self.prepare(io, sql).await {
                // The server's statements are all taken, some by this
                // connection's cache: free the least recently used one and
                // try once more.
                Err(Error::Server {
                    code: ER_MAX_PREPARED_STMT_COUNT_REACHED,
                    ..
                }) if !self.statements.is_empty() => {
                    if let Some(oldest) = self.statements.evict_oldest() {
                        self.close_statement(io, oldest).await?;
                    }
                    self.prepare(io, sql).await?
                }
                other => other?,
            };
            let found = (statement.id, statement.params);
            self.statements.insert(sql.to_vec(), statement);
            found
        };
        let (id, placeholders) = expected;
        if usize::from(placeholders) != params.len() {
            return Err(Error::Parameters {
                expected: placeholders,
                given: params.len(),
            });
        }
        Ok(id)
    }

    /// [`crate::Conn::pipeline`], over `io`.
    ///
    /// # Errors
    ///
    /// As [`crate::Conn::pipeline`].
    pub async fn pipeline<'a, T: Transport>(
        &mut self,
        io: &mut T,
        statements: impl IntoIterator<Item = &'a [u8]>,
    ) -> Result<PipelineTotals> {
        let replies = self.send_pipeline(io, command::QUERY, statements).await?;
        let mut totals = PipelineTotals::default();
        self.read_replies(io, &replies, &mut totals).await?;
        Ok(totals)
    }

    /// [`crate::Conn::pipeline_streamed`], over `io`.
    ///
    /// # Errors
    ///
    /// As [`crate::Conn::pipeline_streamed`].
    pub async fn pipeline_streamed<'a, T: Transport>(
        &mut self,
        io: &mut T,
        statements: impl IntoIterator<Item = &'a [u8]>,
        last: &'a [u8],
    ) -> Result<(PipelineTotals, QueryResult, Option<StreamId>)> {
        let replies = self
            .send_pipeline(io, command::QUERY, statements.into_iter().chain([last]))
            .await?;
        self.finish_pipeline_streamed(io, &replies).await
    }

    /// [`crate::Conn::execute_many_cached`], over `io`.
    ///
    /// # Errors
    ///
    /// As [`crate::Conn::execute_many_cached`].
    pub async fn execute_many_cached<T: Transport>(
        &mut self,
        io: &mut T,
        sql: &[u8],
        params: &[Execute],
    ) -> Result<PipelineTotals> {
        let payloads = self.execute_payloads(io, sql, params).await?;
        let replies = self
            .send_pipeline(
                io,
                command::STMT_EXECUTE,
                payloads.iter().map(Vec::as_slice),
            )
            .await?;
        let mut totals = PipelineTotals::default();
        self.read_replies(io, &replies, &mut totals).await?;
        Ok(totals)
    }

    /// [`crate::Conn::execute_many_cached_streamed`], over `io`.
    ///
    /// # Errors
    ///
    /// As [`crate::Conn::execute_many_cached_streamed`].
    pub async fn execute_many_cached_streamed<T: Transport>(
        &mut self,
        io: &mut T,
        sql: &[u8],
        params: &[Execute],
        last: &Execute,
    ) -> Result<(PipelineTotals, QueryResult, Option<StreamId>)> {
        let mut payloads = self.execute_payloads(io, sql, params).await?;
        payloads.extend(
            self.execute_payloads(io, sql, std::slice::from_ref(last))
                .await?,
        );
        let replies = self
            .send_pipeline(
                io,
                command::STMT_EXECUTE,
                payloads.iter().map(Vec::as_slice),
            )
            .await?;
        self.finish_pipeline_streamed(io, &replies).await
    }

    /// `COM_STMT_EXECUTE` payloads of `sql` (prepared if needed) for each
    /// item of `params`.
    async fn execute_payloads<T: Transport>(
        &mut self,
        io: &mut T,
        sql: &[u8],
        params: &[Execute],
    ) -> Result<Vec<Vec<u8>>> {
        let mut payloads = Vec::with_capacity(params.len());
        for item in params {
            let id = self.cached_statement(io, sql, item).await?;
            payloads.push(item.finish(id));
        }
        Ok(payloads)
    }

    /// Reads every reply of a pipelined batch but the last one's head, as
    /// `pipeline_streamed` leaves it.
    async fn finish_pipeline_streamed<T: Transport>(
        &mut self,
        io: &mut T,
        replies: &[ReplyStart],
    ) -> Result<(PipelineTotals, QueryResult, Option<StreamId>)> {
        let (&last, others) = replies
            .split_last()
            .ok_or_else(|| Error::protocol("empty pipeline"))?;
        let mut totals = PipelineTotals::default();
        self.read_replies(io, others, &mut totals).await?;
        self.wire.expect_reply(last);
        if let Some((_, err)) = totals.error.take() {
            // The last statement ran too: read its reply before reporting.
            match self.read_reply(io, &mut totals).await {
                Ok(()) | Err(Error::Server { .. }) => return Err(err),
                Err(other) => return Err(other),
            }
        }
        let (head, stream) = self.read_head(io).await?;
        Ok((totals, head, stream))
    }

    /// Sends one `cmd` (`COM_QUERY` or `COM_STMT_EXECUTE`) per payload in a
    /// single write; returns where each reply starts.
    async fn send_pipeline<'a, T: Transport>(
        &mut self,
        io: &mut T,
        cmd: u8,
        payloads: impl IntoIterator<Item = &'a [u8]>,
    ) -> Result<Vec<ReplyStart>> {
        self.drain_results(io).await?;
        let payloads: Vec<&[u8]> = payloads.into_iter().collect();
        self.infile = match payloads.as_slice() {
            [only] if cmd == command::QUERY => self.allowed_files(only),
            [_] => Infile::Refuse,
            _ => Infile::Pipelined,
        };
        let mut replies = Vec::new();
        self.wire
            .write_commands(io, cmd, payloads.iter().copied(), &mut replies)
            .await?;
        self.session.binary_reply = cmd == command::STMT_EXECUTE;
        Ok(replies)
    }

    /// Reads the replies starting at `replies`, adding them to `totals`.
    async fn read_replies<T: Transport>(
        &mut self,
        io: &mut T,
        replies: &[ReplyStart],
        totals: &mut PipelineTotals,
    ) -> Result<()> {
        for (index, &start) in replies.iter().enumerate() {
            self.wire.expect_reply(start);
            match self.read_reply(io, totals).await {
                Ok(()) => {}
                Err(err @ Error::Server { .. }) => {
                    if totals.error.is_none() {
                        totals.error = Some((index, err));
                    }
                }
                Err(err) => return Err(err),
            }
        }
        Ok(())
    }

    /// Reads one statement's whole reply (every result, rows dropped),
    /// adding its first result's rowcount to `totals`.
    async fn read_reply<T: Transport>(
        &mut self,
        io: &mut T,
        totals: &mut PipelineTotals,
    ) -> Result<()> {
        let mut reply = self.session.reply();
        let ended = loop {
            let packet = self.read_packet(io).await;
            let step =
                self.session
                    .reply_step(&mut reply, packet.map(|()| self.buf.as_slice()), totals);
            match step {
                Ok(ReplyStep::Read) => {}
                Ok(ReplyStep::File(name)) => {
                    let step = match self.local_infile(io, &name).await {
                        Ok((result, stream)) => self
                            .session
                            .reply_result(&mut reply, &result, stream, totals),
                        Err(err) => Err(err),
                    };
                    if !matches!(step, Ok(ReplyStep::Read)) {
                        break step.map(|_| ());
                    }
                }
                Ok(ReplyStep::Done) => break Ok(()),
                Err(err) => break Err(err),
            }
        };
        // A large row dropped may have grown the buffer.
        release(&mut self.buf);
        ended
    }

    /// [`crate::Conn::read_rows`], over `io`.
    ///
    /// # Errors
    ///
    /// As [`crate::Conn::read_rows`].
    pub async fn read_rows<T: Transport>(
        &mut self,
        io: &mut T,
        id: StreamId,
        rows: &mut RowSet,
        max_bytes: usize,
    ) -> Result<ReadOutcome> {
        let pending = self.session.pending_rows(id)?;
        let limit = rows.data_len().saturating_add(max_bytes);
        loop {
            // The rows the buffer holds are taken without I/O; only a
            // refill waits for the transport.
            let packet = match self.wire.try_read_into(rows.data_mut()) {
                Ok(Some(range)) => Ok(range),
                Ok(None) => match self.wire.refill(io, rows.data_mut()).await {
                    Ok(()) => continue,
                    Err(err) => Err(err),
                },
                Err(err) => Err(err),
            };
            if let Some(warnings) = self.session.row(pending, rows, packet)? {
                return Ok(ReadOutcome::Done { warnings });
            }
            if rows.data_len() >= limit {
                return Ok(ReadOutcome::More);
            }
        }
    }

    /// [`crate::Conn::skip_rows`], over `io`.
    ///
    /// # Errors
    ///
    /// As [`crate::Conn::skip_rows`].
    pub async fn skip_rows<T: Transport>(
        &mut self,
        io: &mut T,
        id: StreamId,
        limit: usize,
    ) -> Result<(usize, ReadOutcome)> {
        let pending = self.session.pending_rows(id)?;
        let mut skipped = 0;
        while skipped < limit {
            let packet = self.read_packet(io).await;
            let end = self
                .session
                .skip(pending, packet.map(|()| self.buf.as_slice()));
            match end {
                Ok(None) => skipped += 1,
                Ok(Some(warnings)) => {
                    // A large row skipped may have grown the buffer.
                    release(&mut self.buf);
                    return Ok((skipped, ReadOutcome::Done { warnings }));
                }
                Err(err) => {
                    release(&mut self.buf);
                    return Err(err);
                }
            }
        }
        Ok((skipped, ReadOutcome::More))
    }

    /// Reads the remaining rows of `stream` into `result`.
    async fn read_all<T: Transport>(
        &mut self,
        io: &mut T,
        mut result: QueryResult,
        stream: Option<StreamId>,
    ) -> Result<QueryResult> {
        if let Some(id) = stream
            && let ReadOutcome::Done { warnings } =
                self.read_rows(io, id, &mut result.rows, usize::MAX).await?
        {
            result.warnings = warnings;
        }
        Ok(result)
    }

    /// [`crate::Conn::next_result`], over `io`.
    ///
    /// # Errors
    ///
    /// As [`crate::Conn::next_result`].
    pub async fn next_result<T: Transport>(&mut self, io: &mut T) -> Result<Option<QueryResult>> {
        match self.next_result_streamed(io).await? {
            Some((result, stream)) => self.read_all(io, result, stream).await.map(Some),
            None => Ok(None),
        }
    }

    /// [`crate::Conn::next_result_streamed`], over `io`.
    ///
    /// # Errors
    ///
    /// As [`crate::Conn::next_result_streamed`].
    pub async fn next_result_streamed<T: Transport>(
        &mut self,
        io: &mut T,
    ) -> Result<Option<(QueryResult, Option<StreamId>)>> {
        self.discard_pending_rows(io).await?;
        if self.session.has_more_results() {
            self.read_head(io).await.map(Some)
        } else {
            Ok(None)
        }
    }

    /// Reads the start of a statement's response: the OK packet, or the
    /// column definitions of a result set, whose rows stay pending.
    async fn read_head<T: Transport>(
        &mut self,
        io: &mut T,
    ) -> Result<(QueryResult, Option<StreamId>)> {
        let mut head = self.session.head();
        loop {
            self.read_packet(io).await?;
            match self.session.head_step(&mut head, &self.buf)? {
                HeadStep::Read => {}
                HeadStep::Done(result, stream) => return Ok((result, stream)),
                HeadStep::File(name) => return self.local_infile(io, &name).await,
            }
        }
    }

    /// Local files `sql` may make the server ask for.
    fn allowed_files(&self, sql: &[u8]) -> Infile {
        if self.local_infile_dir.is_none() {
            return Infile::Refuse;
        }
        let no_backslash_escapes = self.session.status & status::NO_BACKSLASH_ESCAPES != 0;
        Infile::Allow(local_infile_names(sql, no_backslash_escapes))
    }

    /// Answers the server's request for the local file `name` (`LOAD DATA
    /// LOCAL INFILE`) with the file, or with an empty one if it is not
    /// allowed, and reads the statement's reply.
    async fn local_infile<T: Transport>(
        &mut self,
        io: &mut T,
        name: &[u8],
    ) -> Result<(QueryResult, Option<StreamId>)> {
        if matches!(self.infile, Infile::Pipelined) {
            io.shutdown();
            return Err(Error::protocol(
                "the server asked for a local file in the middle of a pipelined batch",
            ));
        }
        let refused = match infile::open(self.local_infile_dir.as_deref(), &self.infile, name) {
            Ok((path, file)) => {
                self.send_file(io, &path, file).await?;
                None
            }
            Err(reason) => Some(reason),
        };
        if refused.is_some() {
            // An empty file ends the request; the statement loads nothing.
            self.wire.write_packet(io, &[]).await?;
        }
        self.read_packet(io).await?;
        infile::outcome(refused, self.session.file_reply(&self.buf)).map(|result| (result, None))
    }

    /// Sends `file` in packets and the empty packet that ends it. If the
    /// file cannot be read to the end, the connection is closed instead,
    /// so the server aborts the statement rather than loading part of it.
    async fn send_file<T: Transport>(
        &mut self,
        io: &mut T,
        path: &Path,
        mut file: File,
    ) -> Result<()> {
        let mut chunk = vec![0; infile::CHUNK];
        loop {
            let len = match infile::read_full(&mut file, &mut chunk) {
                Ok(len) => len,
                Err(err) => {
                    io.shutdown();
                    return Err(io::Error::new(
                        err.kind(),
                        format!("reading {}: {err}", path.display()),
                    )
                    .into());
                }
            };
            if len == 0 {
                return self.wire.write_packet(io, &[]).await;
            }
            self.wire
                .write_packet(io, chunk.get(..len).unwrap_or_default())
                .await?;
        }
    }

    /// [`crate::Conn::select_db`], over `io`.
    ///
    /// # Errors
    ///
    /// As [`crate::Conn::select_db`].
    pub async fn select_db<T: Transport>(&mut self, io: &mut T, database: &[u8]) -> Result<()> {
        self.drain_results(io).await?;
        self.wire
            .write_command(io, command::INIT_DB, database)
            .await?;
        self.expect_ok(io).await
    }

    /// [`crate::Conn::reset_connection`], over `io`.
    ///
    /// # Errors
    ///
    /// As [`crate::Conn::reset_connection`].
    pub async fn reset_connection<T: Transport>(&mut self, io: &mut T) -> Result<()> {
        self.drain_results(io).await?;
        // The reset closes every prepared statement.
        self.statements.clear();
        self.wire
            .write_command(io, command::RESET_CONNECTION, &[])
            .await?;
        self.expect_ok(io).await
    }

    /// [`crate::Conn::change_user`], over `io`.
    ///
    /// # Errors
    ///
    /// As [`crate::Conn::change_user`].
    pub async fn change_user<T: Transport>(
        &mut self,
        io: &mut T,
        user: &[u8],
        password: &str,
        database: Option<&[u8]>,
    ) -> Result<()> {
        self.drain_results(io).await?;
        // The new session has none of the old one's prepared statements.
        self.statements.clear();
        let plugin = self.default_plugin;
        let (auth_response, auth) = Auth::start(
            plugin,
            self.greeting_nonce.clone(),
            password.as_bytes(),
            &self.auth,
        )?;
        let auth_len = u8::try_from(auth_response.len())
            .map_err(|_| Error::protocol("authentication response too long"))?;

        let mut payload = Vec::with_capacity(64 + user.len());
        put_nul_bytes(&mut payload, user);
        payload.push(auth_len);
        payload.extend_from_slice(&auth_response);
        put_nul_bytes(&mut payload, database.unwrap_or_default());
        payload.extend_from_slice(&u16::from(self.collation).to_le_bytes());
        put_nul_bytes(&mut payload, plugin.name());
        self.wire
            .write_command(io, command::CHANGE_USER, &payload)
            .await?;
        self.auth_exchange(io, auth, password.as_bytes()).await
    }

    /// Discards unread rows of a streamed result set and unread results of
    /// a previous multi-result query, so the connection never gets out of
    /// sync.
    async fn drain_results<T: Transport>(&mut self, io: &mut T) -> Result<()> {
        self.discard_pending_rows(io).await?;
        while self.session.has_more_results() {
            // Queued results nobody asked for: their rows are never decoded.
            if self.read_head(io).await?.1.is_some() {
                self.discard_pending_rows(io).await?;
            }
        }
        // What the previous command allowed ends with its results.
        self.infile = Infile::Refuse;
        Ok(())
    }

    /// Reads and drops the rows of the pending result set, if any. A server
    /// error among them ends that result set and is not reported: it
    /// belongs to a result nobody reads any more.
    async fn discard_pending_rows<T: Transport>(&mut self, io: &mut T) -> Result<()> {
        let Some(id) = self.session.pending() else {
            return Ok(());
        };
        match self.skip_rows(io, id, usize::MAX).await {
            Ok(_) | Err(Error::Server { .. }) => Ok(()),
            Err(err) => Err(err),
        }
    }

    /// Reads the reply to a command that answers OK or ERR.
    async fn expect_ok<T: Transport>(&mut self, io: &mut T) -> Result<()> {
        self.read_packet(io).await?;
        self.session.ok_reply(&self.buf)
    }

    /// [`crate::Conn::ping`], over `io`.
    ///
    /// # Errors
    ///
    /// As [`crate::Conn::ping`].
    pub async fn ping<T: Transport>(&mut self, io: &mut T) -> Result<()> {
        self.drain_results(io).await?;
        self.wire.write_command(io, command::PING, &[]).await?;
        self.expect_ok(io).await
    }

    /// Sends `COM_QUIT`; errors are ignored, the connection is being
    /// discarded either way.
    ///
    /// # Errors
    ///
    /// None: the result is for symmetry with the other commands.
    pub async fn quit<T: Transport>(&mut self, io: &mut T) -> Result<()> {
        let _ = self.wire.write_command(io, command::QUIT, &[]).await;
        Ok(())
    }
}
