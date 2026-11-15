//! A blocking connection: transport, handshake, authentication and commands.

use std::io;
use std::io::Read;
use std::io::Write;
use std::net::Shutdown;
use std::net::TcpStream;
use std::net::ToSocketAddrs;
#[cfg(unix)]
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

use crate::auth::AuthPlugin;
use crate::auth::cleartext_password;
use crate::auth::encrypt_password;
use crate::auth::parse_public_key;
use crate::auth::parsec_response;
use crate::auth::trim_nonce;
use crate::buf::Reader;
use crate::buf::put_nul_bytes;
use crate::charset::handshake_collation;
use crate::column::Column;
use crate::constants::capability;
use crate::constants::command;
use crate::constants::status;
use crate::error::Error;
use crate::error::Result;
use crate::handshake::HandshakeResponse;
use crate::handshake::ServerHandshake;
use crate::handshake::ssl_request;
use crate::packet::PacketStream;
use crate::result::QueryResult;
use crate::result::RowPacket;
use crate::result::RowSet;
use crate::result::accept_row_packet;
use crate::result::parse_eof;
use crate::result::parse_err;
use crate::result::parse_ok;
use crate::result::row_terminator;
use crate::tls::SslMode;
use crate::tls::TlsOpts;
use crate::tls::client_config;
use crate::tls::server_name;
use crate::tls::tls_error;

/// Capabilities requested by default; the effective set is the
/// intersection with what the server offers.
const DEFAULT_CAPABILITIES: u32 = capability::LONG_PASSWORD
    | capability::LONG_FLAG
    | capability::PROTOCOL_41
    | capability::TRANSACTIONS
    | capability::SECURE_CONNECTION
    | capability::MULTI_RESULTS
    | capability::PS_MULTI_RESULTS
    | capability::PLUGIN_AUTH
    | capability::PLUGIN_AUTH_LENENC_CLIENT_DATA
    | capability::DEPRECATE_EOF;

/// `caching_sha2_password`: the server found the password in its cache.
const FAST_AUTH_SUCCESS: u8 = 0x03;
/// `caching_sha2_password`: the server needs the cleartext password.
const PERFORM_FULL_AUTH: u8 = 0x04;
/// `caching_sha2_password`: client asks for the server's RSA public key.
const CACHING_SHA2_REQUEST_PUBLIC_KEY: u8 = 0x02;
/// `sha256_password`: client asks for the server's RSA public key.
const SHA256_REQUEST_PUBLIC_KEY: u8 = 0x01;

/// Connection parameters.
#[derive(Debug, Clone)]
pub struct Opts {
    pub host: String,
    pub port: u16,
    /// Connect through this Unix socket instead of TCP.
    pub unix_socket: Option<PathBuf>,
    /// User name in the session's character set (`charset`): the server
    /// reads it, and `database`, in that character set.
    pub user: Vec<u8>,
    /// Sent as UTF-8 whatever `charset` is: the server compares the bytes
    /// with those the password was set with (from a UTF-8 session).
    pub password: String,
    /// Default database, in the session's character set.
    pub database: Option<Vec<u8>>,
    pub connect_timeout: Option<Duration>,
    pub read_timeout: Option<Duration>,
    pub write_timeout: Option<Duration>,
    /// Extra capability flags, e.g. `MULTI_STATEMENTS` or `FOUND_ROWS`.
    pub client_flag: u32,
    /// Character set of the session, as the server names it (`utf8mb4`,
    /// `latin1`...). Its collation is the server's default when the server's
    /// default character set is this one, else the character set's own.
    pub charset: String,
    pub tls: TlsOpts,
    /// PEM file with the server's RSA public key, used to encrypt the
    /// password over plain TCP instead of asking the server for its key.
    pub server_public_key: Option<PathBuf>,
}

impl Default for Opts {
    fn default() -> Self {
        Self {
            host: "localhost".to_owned(),
            port: 3306,
            unix_socket: None,
            user: Vec::new(),
            password: String::new(),
            database: None,
            connect_timeout: None,
            read_timeout: None,
            write_timeout: None,
            client_flag: 0,
            charset: "utf8mb4".to_owned(),
            tls: TlsOpts::default(),
            server_public_key: None,
        }
    }
}

/// Byte stream to the server.
#[derive(Debug)]
pub(crate) enum Stream {
    Tcp(TcpStream),
    #[cfg(unix)]
    Unix(UnixStream),
    Tls(Box<rustls::StreamOwned<rustls::ClientConnection, TcpStream>>),
    /// Scripted server for fuzzing.
    #[cfg(feature = "fuzzing")]
    Mock(crate::fuzzing::MockStream),
}

impl Stream {
    fn connect(opts: &Opts) -> Result<Self> {
        #[cfg(unix)]
        if let Some(path) = &opts.unix_socket {
            let stream = UnixStream::connect(path)?;
            stream.set_read_timeout(opts.read_timeout)?;
            stream.set_write_timeout(opts.write_timeout)?;
            return Ok(Self::Unix(stream));
        }
        #[cfg(not(unix))]
        if opts.unix_socket.is_some() {
            return Err(Error::unsupported("Unix sockets on this platform"));
        }

        let mut last_err = None;
        for addr in (opts.host.as_str(), opts.port).to_socket_addrs()? {
            let attempt = match opts.connect_timeout {
                Some(timeout) => TcpStream::connect_timeout(&addr, timeout),
                None => TcpStream::connect(addr),
            };
            match attempt {
                Ok(stream) => {
                    // Requests are small and latency-bound: never batch them.
                    stream.set_nodelay(true)?;
                    stream.set_read_timeout(opts.read_timeout)?;
                    stream.set_write_timeout(opts.write_timeout)?;
                    return Ok(Self::Tcp(stream));
                }
                Err(err) => last_err = Some(err),
            }
        }
        Err(last_err
            .unwrap_or_else(|| {
                io::Error::new(io::ErrorKind::NotFound, "host resolved to no address")
            })
            .into())
    }

    /// Whether the transport is already confidential, which allows sending
    /// the cleartext password during `caching_sha2_password` full auth.
    const fn is_secure(&self) -> bool {
        match self {
            Self::Tcp(_) => false,
            #[cfg(unix)]
            Self::Unix(_) => true,
            Self::Tls(_) => true,
            #[cfg(feature = "fuzzing")]
            Self::Mock(_) => false,
        }
    }

    /// Runs the TLS handshake over a plain TCP stream.
    fn upgrade(self, tls: &TlsOpts, host: &str) -> Result<Self> {
        let Self::Tcp(mut tcp) = self else {
            return Err(Error::Tls("TLS is only supported over TCP".to_owned()));
        };
        let mut session = rustls::ClientConnection::new(client_config(tls)?, server_name(host)?)
            .map_err(tls_error)?;
        while session.is_handshaking() {
            session.complete_io(&mut tcp).map_err(tls_error)?;
        }
        Ok(Self::Tls(Box::new(rustls::StreamOwned::new(session, tcp))))
    }

    fn shutdown(&mut self) {
        // Best effort: the peer may already be gone.
        let _ = match self {
            Self::Tcp(s) => s.shutdown(Shutdown::Both),
            #[cfg(unix)]
            Self::Unix(s) => s.shutdown(Shutdown::Both),
            Self::Tls(s) => {
                s.conn.send_close_notify();
                let _ = s.flush();
                s.sock.shutdown(Shutdown::Both)
            }
            #[cfg(feature = "fuzzing")]
            Self::Mock(_) => Ok(()),
        };
    }
}

impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Tcp(s) => s.read(buf),
            #[cfg(unix)]
            Self::Unix(s) => s.read(buf),
            Self::Tls(s) => s.read(buf),
            #[cfg(feature = "fuzzing")]
            Self::Mock(s) => s.read(buf),
        }
    }
}

impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Self::Tcp(s) => s.write(buf),
            #[cfg(unix)]
            Self::Unix(s) => s.write(buf),
            Self::Tls(s) => s.write(buf),
            #[cfg(feature = "fuzzing")]
            Self::Mock(s) => s.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Tcp(s) => s.flush(),
            #[cfg(unix)]
            Self::Unix(s) => s.flush(),
            Self::Tls(s) => s.flush(),
            #[cfg(feature = "fuzzing")]
            Self::Mock(s) => s.flush(),
        }
    }
}

/// First authentication payload for `plugin`.
fn first_response(
    plugin: AuthPlugin,
    password: &[u8],
    nonce: &[u8],
    secure: bool,
    pinned_key: Option<&rsa::PublicKey>,
) -> Result<Vec<u8>> {
    if plugin != AuthPlugin::Sha256 {
        return Ok(plugin.scramble(password, nonce));
    }
    // sha256_password sends the password itself from the start.
    if password.is_empty() {
        Ok(vec![0])
    } else if secure {
        Ok(cleartext_password(password))
    } else if let Some(key) = pinned_key {
        encrypt_password(password, nonce, key)
    } else {
        Ok(vec![SHA256_REQUEST_PUBLIC_KEY])
    }
}

fn load_public_key(path: &std::path::Path) -> Result<rsa::PublicKey> {
    let pem = std::fs::read(path).map_err(|e| {
        Error::Config(format!(
            "cannot read server_public_key {}: {e}",
            path.display()
        ))
    })?;
    parse_public_key(&pem)
        .map_err(|e| Error::Config(format!("server_public_key {}: {e}", path.display())))
}

/// Identifies a result set whose rows are being streamed, so that reading
/// more rows fails once another command has discarded them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamId(u64);

/// Where a streamed result set stands after a read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadOutcome {
    /// More rows (or the terminator) are still on the wire.
    More,
    /// The result set ended; the terminator carried this many warnings.
    Done { warnings: u16 },
}

/// The result set whose rows are still unread on the wire.
#[derive(Debug, Clone, Copy)]
struct Pending {
    id: StreamId,
    deprecate_eof: bool,
}

/// An authenticated connection to a MySQL or MariaDB server.
#[derive(Debug)]
pub struct Conn {
    stream: PacketStream<Stream>,
    capabilities: u32,
    status: u16,
    server_version: String,
    connection_id: u32,
    /// Default plugin and nonce from the greeting, reused by `COM_CHANGE_USER`.
    default_plugin: AuthPlugin,
    greeting_nonce: Vec<u8>,
    /// Pinned RSA key (`Opts::server_public_key`), also for `COM_CHANGE_USER`.
    pinned_key: Option<rsa::PublicKey>,
    /// Session collation sent in the handshake, also for `COM_CHANGE_USER`.
    collation: u8,
    /// The server's default collation, from its greeting.
    server_collation: u8,
    /// Scratch buffer for control packets.
    buf: Vec<u8>,
    /// Streamed result set with rows still on the wire.
    pending: Option<Pending>,
    /// Last [`StreamId`] handed out.
    last_stream_id: u64,
}

impl Conn {
    /// Opens the transport, performs the handshake and authenticates.
    ///
    /// # Errors
    ///
    /// I/O failures, protocol violations, authentication errors reported
    /// by the server, or authentication methods not implemented yet.
    pub fn connect(opts: &Opts) -> Result<Self> {
        Self::establish(Stream::connect(opts)?, opts)
    }

    /// Handshake and authentication over an already open transport.
    pub(crate) fn establish(transport: Stream, opts: &Opts) -> Result<Self> {
        let mut stream = PacketStream::new(transport);
        let mut buf = Vec::new();
        stream.read_into(&mut buf)?;
        let greeting = ServerHandshake::parse(&buf)?;

        if greeting.capabilities & capability::PROTOCOL_41 == 0 {
            return Err(Error::unsupported("servers older than MySQL 4.1"));
        }
        let mut wanted = DEFAULT_CAPABILITIES | opts.client_flag;
        if opts.database.is_some() {
            wanted |= capability::CONNECT_WITH_DB;
        }
        let use_tls = Self::negotiate_tls(opts, &greeting)?;
        if use_tls {
            wanted |= capability::SSL;
        }
        let capabilities = wanted & greeting.capabilities;
        let collation =
            handshake_collation(&opts.charset, greeting.collation, greeting.is_mariadb())
                .ok_or_else(|| {
                    Error::Config(format!("unknown character set {:?}", opts.charset))
                })?;

        if use_tls {
            // SSLRequest goes out in plain text, then the same socket is
            // wrapped in TLS and the sequence continues inside it.
            stream.write_packet(&ssl_request(capabilities, collation))?;
            let (raw, seq) = stream.into_parts()?;
            stream = PacketStream::from_parts(raw.upgrade(&opts.tls, &opts.host)?, seq);
        }

        let mut conn = Self {
            stream,
            capabilities,
            status: greeting.status,
            server_version: greeting.server_version.clone(),
            connection_id: greeting.connection_id,
            // Answer with the server's default plugin when we know it;
            // otherwise offer native auth and let the server switch us.
            default_plugin: AuthPlugin::from_name(&greeting.auth_plugin)
                .unwrap_or(AuthPlugin::Native),
            greeting_nonce: greeting.nonce.clone(),
            pinned_key: opts
                .server_public_key
                .as_deref()
                .map(load_public_key)
                .transpose()?,
            collation,
            server_collation: greeting.collation,
            buf,
            pending: None,
            last_stream_id: 0,
        };
        conn.authenticate(opts)?;
        Ok(conn)
    }

    /// Decides whether to use TLS, per `ssl_mode` and what the server offers.
    fn negotiate_tls(opts: &Opts, greeting: &ServerHandshake) -> Result<bool> {
        let mode = opts.tls.mode;
        if mode == SslMode::Disabled {
            return Ok(false);
        }
        if opts.unix_socket.is_some() {
            // A local socket is already private; certificates name hosts.
            if mode.verifies() {
                return Err(Error::Tls(
                    "certificate verification needs a TCP connection, not a Unix socket".to_owned(),
                ));
            }
            return Ok(false);
        }
        let server_has_tls = greeting.capabilities & capability::SSL != 0;
        if mode.requires_tls() && !server_has_tls {
            return Err(Error::Tls(format!(
                "ssl_mode {mode:?} needs TLS but the server does not support it"
            )));
        }
        Ok(server_has_tls)
    }

    /// Whether the connection is encrypted with TLS.
    #[must_use]
    pub fn is_tls(&self) -> bool {
        matches!(self.stream.get_ref(), Stream::Tls(_))
    }

    fn authenticate(&mut self, opts: &Opts) -> Result<()> {
        let password = opts.password.as_bytes();
        let plugin = self.default_plugin;
        let nonce = self.greeting_nonce.clone();
        let auth_response = self.first_response(plugin, password, &nonce)?;
        let response = HandshakeResponse {
            capabilities: self.capabilities,
            collation: self.collation,
            user: &opts.user,
            auth_response: &auth_response,
            database: opts.database.as_deref(),
            auth_plugin: plugin.name(),
        }
        .encode()?;
        self.stream.write_packet(&response)?;
        self.auth_exchange(password, plugin, nonce)
    }

    fn first_response(&self, plugin: AuthPlugin, password: &[u8], nonce: &[u8]) -> Result<Vec<u8>> {
        let secure = self.stream.get_ref().is_secure();
        first_response(plugin, password, nonce, secure, self.pinned_key.as_ref())
    }

    /// Runs the server-driven part of authentication (auth switches and
    /// plugin-specific round trips) until OK or ERR.
    fn auth_exchange(
        &mut self,
        password: &[u8],
        mut plugin: AuthPlugin,
        mut nonce: Vec<u8>,
    ) -> Result<()> {
        let secure = self.stream.get_ref().is_secure();
        loop {
            self.read_packet()?;
            let reply = match self.buf.first().copied() {
                Some(0x00) => {
                    self.status = parse_ok(&self.buf)?.status;
                    return Ok(());
                }
                Some(0xFF) => return Err(parse_err(&self.buf)),
                Some(0xFE) => {
                    // Auth switch: plugin name, then a fresh nonce.
                    let mut r = Reader::new(&self.buf);
                    r.skip(1)?;
                    let name = r.nul_bytes()?;
                    plugin = AuthPlugin::from_name(name).ok_or_else(|| {
                        Error::unsupported(format!(
                            "authentication plugin {:?}",
                            String::from_utf8_lossy(name)
                        ))
                    })?;
                    let data = r.rest();
                    nonce = if plugin.nul_terminated_nonce() {
                        trim_nonce(data).to_vec()
                    } else {
                        data.to_vec()
                    };
                    self.first_response(plugin, password, &nonce)?
                }
                // AuthMoreData: plugin-specific continuation.
                Some(0x01) => {
                    let data = self.buf.get(1..).unwrap_or_default();
                    match (plugin, data) {
                        (AuthPlugin::CachingSha2, [FAST_AUTH_SUCCESS]) => continue,
                        (AuthPlugin::CachingSha2, [PERFORM_FULL_AUTH]) => {
                            if secure {
                                cleartext_password(password)
                            } else if let Some(key) = &self.pinned_key {
                                encrypt_password(password, &nonce, key)?
                            } else {
                                vec![CACHING_SHA2_REQUEST_PUBLIC_KEY]
                            }
                        }
                        // The public key we asked for.
                        (AuthPlugin::CachingSha2 | AuthPlugin::Sha256, pem) => {
                            encrypt_password(password, &nonce, &parse_public_key(pem)?)?
                        }
                        (AuthPlugin::Parsec, salt) => parsec_response(password, &nonce, salt)?,
                        (AuthPlugin::Native | AuthPlugin::Ed25519, _) => {
                            return Err(Error::protocol(
                                "unexpected AuthMoreData for this authentication plugin",
                            ));
                        }
                    }
                }
                // parsec's salt message may also arrive without the 0x01 prefix.
                Some(b'P') if plugin == AuthPlugin::Parsec => {
                    parsec_response(password, &nonce, &self.buf)?
                }
                _ => return Err(Error::protocol("unexpected packet during authentication")),
            };
            self.stream.write_packet(&reply)?;
        }
    }

    fn read_packet(&mut self) -> Result<()> {
        self.buf.clear();
        self.stream.read_into(&mut self.buf).map(|_| ())
    }

    /// Runs `sql` with the text protocol and returns the first result.
    ///
    /// Any unread results of a previous multi-result query are discarded
    /// first, so the connection never gets out of sync.
    ///
    /// # Errors
    ///
    /// I/O failures, protocol violations or an ERR packet from the server.
    pub fn query(&mut self, sql: &[u8]) -> Result<QueryResult> {
        let (result, stream) = self.query_streamed(sql)?;
        self.read_all(result, stream)
    }

    /// Like [`Conn::query`], but leaves the rows on the wire: the result
    /// has the columns and no rows, and the [`StreamId`] (present when there
    /// is a result set) reads them with [`Conn::read_rows`].
    ///
    /// Until every row is read, the next command discards the rest first.
    ///
    /// # Errors
    ///
    /// I/O failures, protocol violations or an ERR packet from the server.
    pub fn query_streamed(&mut self, sql: &[u8]) -> Result<(QueryResult, Option<StreamId>)> {
        self.drain_results()?;
        self.stream.write_command(command::QUERY, sql)?;
        self.read_head()
    }

    /// Reads rows of the streamed result set `id` into `rows`, stopping
    /// once `rows` holds at least `max_bytes` more bytes than before (always
    /// after one row at least) or at the end of the result set.
    ///
    /// # Errors
    ///
    /// [`Error::ResultDiscarded`] if `id` is not the pending result set,
    /// I/O failures, protocol violations, or an ERR packet that aborted the
    /// result set (which then counts as ended).
    pub fn read_rows(
        &mut self,
        id: StreamId,
        rows: &mut RowSet,
        max_bytes: usize,
    ) -> Result<ReadOutcome> {
        let pending = self.pending_rows(id)?;
        let limit = rows.data_len().saturating_add(max_bytes);
        loop {
            let packet = self
                .stream
                .read_into(rows.data_mut())
                .and_then(|range| accept_row_packet(rows, range, pending.deprecate_eof));
            match packet {
                Ok(RowPacket::Row) => {
                    if rows.data_len() >= limit {
                        return Ok(ReadOutcome::More);
                    }
                }
                Ok(RowPacket::End(end)) => {
                    self.pending = None;
                    self.status = end.status;
                    return Ok(ReadOutcome::Done {
                        warnings: end.warnings,
                    });
                }
                Err(err) => {
                    self.pending = None;
                    return Err(err);
                }
            }
        }
    }

    /// Reads and drops up to `limit` rows of the streamed result set `id`
    /// without decoding them. Returns how many were skipped.
    ///
    /// # Errors
    ///
    /// As [`Conn::read_rows`].
    pub fn skip_rows(&mut self, id: StreamId, limit: usize) -> Result<(usize, ReadOutcome)> {
        let pending = self.pending_rows(id)?;
        let mut skipped = 0;
        while skipped < limit {
            let end = self
                .read_packet()
                .and_then(|()| row_terminator(&self.buf, pending.deprecate_eof));
            match end {
                Ok(None) => skipped += 1,
                Ok(Some(end)) => {
                    self.pending = None;
                    self.status = end.status;
                    return Ok((
                        skipped,
                        ReadOutcome::Done {
                            warnings: end.warnings,
                        },
                    ));
                }
                Err(err) => {
                    self.pending = None;
                    return Err(err);
                }
            }
        }
        Ok((skipped, ReadOutcome::More))
    }

    fn pending_rows(&self, id: StreamId) -> Result<Pending> {
        match self.pending {
            Some(pending) if pending.id == id => Ok(pending),
            _ => Err(Error::ResultDiscarded),
        }
    }

    /// Reads the remaining rows of `stream` into `result`.
    fn read_all(
        &mut self,
        mut result: QueryResult,
        stream: Option<StreamId>,
    ) -> Result<QueryResult> {
        if let Some(id) = stream
            && let ReadOutcome::Done { warnings } =
                self.read_rows(id, &mut result.rows, usize::MAX)?
        {
            result.warnings = warnings;
        }
        Ok(result)
    }

    /// Whether the server has more results queued for the last query
    /// (multi-statement queries and stored procedures).
    #[must_use]
    pub const fn has_more_results(&self) -> bool {
        self.status & status::MORE_RESULTS_EXISTS != 0
    }

    /// Reads the next queued result, if any.
    ///
    /// # Errors
    ///
    /// I/O failures, protocol violations or an ERR packet from the server.
    pub fn next_result(&mut self) -> Result<Option<QueryResult>> {
        self.next_result_streamed()?
            .map(|(result, stream)| self.read_all(result, stream))
            .transpose()
    }

    /// Like [`Conn::next_result`], leaving the rows on the wire as
    /// [`Conn::query_streamed`] does. Unread rows of the current result set
    /// are discarded first.
    ///
    /// # Errors
    ///
    /// I/O failures, protocol violations or an ERR packet from the server.
    pub fn next_result_streamed(&mut self) -> Result<Option<(QueryResult, Option<StreamId>)>> {
        self.discard_pending_rows()?;
        if self.has_more_results() {
            self.read_head().map(Some)
        } else {
            Ok(None)
        }
    }

    /// Reads the start of a statement's response: the OK packet, or the
    /// column definitions of a result set, whose rows stay pending.
    fn read_head(&mut self) -> Result<(QueryResult, Option<StreamId>)> {
        // An error ends the whole response, including queued results.
        self.status &= !status::MORE_RESULTS_EXISTS;
        self.read_packet()?;
        match self.buf.first().copied() {
            Some(0x00) => {
                let ok = parse_ok(&self.buf)?;
                self.status = ok.status;
                Ok((QueryResult::from_ok(ok), None))
            }
            Some(0xFF) => Err(parse_err(&self.buf)),
            Some(0xFB) => {
                // LOCAL INFILE request: refuse by sending an empty file.
                self.stream.write_packet(&[])?;
                self.read_packet()?;
                if self.buf.first() == Some(&0x00) {
                    self.status = parse_ok(&self.buf)?.status;
                }
                Err(Error::unsupported("LOAD DATA LOCAL INFILE"))
            }
            Some(_) => {
                let count = Reader::new(&self.buf).lenenc_len()?;
                self.read_columns(count)
            }
            None => Err(Error::protocol("empty response packet")),
        }
    }

    fn read_columns(&mut self, count: usize) -> Result<(QueryResult, Option<StreamId>)> {
        let deprecate_eof = self.capabilities & capability::DEPRECATE_EOF != 0;
        // Cap the preallocation: `count` comes from the server.
        let mut columns = Vec::with_capacity(count.min(4096));
        for _ in 0..count {
            self.read_packet()?;
            columns.push(Column::parse(&self.buf)?);
        }
        if !deprecate_eof {
            self.read_packet()?;
            if self.buf.first() != Some(&0xFE) {
                return Err(Error::protocol("missing EOF after column definitions"));
            }
            self.status = parse_eof(&self.buf)?.status;
        }
        self.last_stream_id += 1;
        let id = StreamId(self.last_stream_id);
        self.pending = Some(Pending { id, deprecate_eof });
        Ok((QueryResult::rowset(columns), Some(id)))
    }

    /// Switches the default database (`COM_INIT_DB`). `database` is in the
    /// session's character set.
    ///
    /// # Errors
    ///
    /// I/O failures or an ERR packet (e.g. unknown database).
    pub fn select_db(&mut self, database: &[u8]) -> Result<()> {
        self.drain_results()?;
        self.stream.write_command(command::INIT_DB, database)?;
        self.expect_ok()
    }

    /// Resets the session to its just-connected state without
    /// re-authenticating (`COM_RESET_CONNECTION`): rolls back any
    /// transaction, drops temporary tables, clears user variables and
    /// restores session variables (including `autocommit`) to their
    /// defaults.
    ///
    /// # Errors
    ///
    /// I/O failures or an ERR packet.
    pub fn reset_connection(&mut self) -> Result<()> {
        self.drain_results()?;
        self.stream.write_command(command::RESET_CONNECTION, &[])?;
        self.expect_ok()
    }

    /// Re-authenticates as another account on the same connection
    /// (`COM_CHANGE_USER`). The session is reset as with
    /// [`Conn::reset_connection`]. `user` and `database` are in the
    /// character set of [`Conn::collation`], which the packet names (the
    /// password goes as UTF-8, as in the handshake).
    ///
    /// # Errors
    ///
    /// I/O failures, authentication errors reported by the server, or
    /// authentication methods not implemented yet.
    pub fn change_user(
        &mut self,
        user: &[u8],
        password: &str,
        database: Option<&[u8]>,
    ) -> Result<()> {
        self.drain_results()?;
        let plugin = self.default_plugin;
        let nonce = self.greeting_nonce.clone();
        let auth_response = self.first_response(plugin, password.as_bytes(), &nonce)?;
        let auth_len = u8::try_from(auth_response.len())
            .map_err(|_| Error::protocol("authentication response too long"))?;

        let mut payload = Vec::with_capacity(64 + user.len());
        put_nul_bytes(&mut payload, user);
        payload.push(auth_len);
        payload.extend_from_slice(&auth_response);
        put_nul_bytes(&mut payload, database.unwrap_or_default());
        payload.extend_from_slice(&u16::from(self.collation).to_le_bytes());
        put_nul_bytes(&mut payload, plugin.name());
        self.stream.write_command(command::CHANGE_USER, &payload)?;
        self.auth_exchange(password.as_bytes(), plugin, nonce)
    }

    /// Discards unread rows of a streamed result set and unread results of
    /// a previous multi-result query, so the connection never gets out of
    /// sync.
    fn drain_results(&mut self) -> Result<()> {
        self.discard_pending_rows()?;
        while self.has_more_results() {
            // Queued results nobody asked for: their rows are never decoded.
            if self.read_head()?.1.is_some() {
                self.discard_pending_rows()?;
            }
        }
        Ok(())
    }

    /// Reads and drops the rows of the pending result set, if any. A server
    /// error among them ends that result set and is not reported: it
    /// belongs to a result nobody reads any more.
    fn discard_pending_rows(&mut self) -> Result<()> {
        let Some(pending) = self.pending else {
            return Ok(());
        };
        match self.skip_rows(pending.id, usize::MAX) {
            Ok(_) | Err(Error::Server { .. }) => Ok(()),
            Err(err) => Err(err),
        }
    }

    /// Reads the reply to a command that answers OK or ERR.
    fn expect_ok(&mut self) -> Result<()> {
        self.read_packet()?;
        match self.buf.first() {
            Some(0x00) => {
                self.status = parse_ok(&self.buf)?.status;
                Ok(())
            }
            Some(0xFF) => Err(parse_err(&self.buf)),
            _ => Err(Error::protocol("expected an OK or ERR packet")),
        }
    }

    /// Checks that the server is alive.
    ///
    /// # Errors
    ///
    /// I/O failures or an unexpected response.
    pub fn ping(&mut self) -> Result<()> {
        self.drain_results()?;
        self.stream.write_command(command::PING, &[])?;
        self.expect_ok()
    }

    /// Sends `COM_QUIT` and closes the socket. Errors are ignored: the
    /// connection is being discarded either way.
    pub fn close(mut self) {
        let _ = self.stream.write_command(command::QUIT, &[]);
        self.stream.get_mut().shutdown();
    }

    #[must_use]
    pub fn server_version(&self) -> &str {
        &self.server_version
    }

    #[must_use]
    pub const fn connection_id(&self) -> u32 {
        self.connection_id
    }

    /// Status flags from the last OK/EOF packet (see [`crate::constants::status`]).
    #[must_use]
    pub const fn status_flags(&self) -> u16 {
        self.status
    }

    /// Capabilities negotiated with the server.
    #[must_use]
    pub const fn capabilities(&self) -> u32 {
        self.capabilities
    }

    /// Collation id requested in the handshake.
    #[must_use]
    pub const fn collation(&self) -> u8 {
        self.collation
    }

    /// The server's default character set, from its greeting. MySQL's
    /// `COM_RESET_CONNECTION` returns the session to it, not to the one
    /// chosen in the handshake (MariaDB keeps the latter).
    #[must_use]
    pub fn server_charset(&self) -> Option<&'static str> {
        crate::charset::charset_of(self.server_collation)
    }
}
