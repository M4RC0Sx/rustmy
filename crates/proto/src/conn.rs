//! A blocking connection: transport, handshake, authentication and commands.

use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::io;
use std::io::Read;
use std::io::Write;
use std::net::Shutdown;
use std::net::TcpStream;
use std::net::ToSocketAddrs;
#[cfg(unix)]
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;

use socket2::SockRef;
use socket2::TcpKeepalive;

use crate::auth::parse_public_key;
use crate::binary::Execute;
use crate::client::Client;
use crate::column::Column;
use crate::compress::Compression;
use crate::error::Error;
use crate::error::Result;
use crate::login::Login;
use crate::packet::PacketStream;
use crate::result::QueryResult;
use crate::result::RowSet;
use crate::session::ReadOutcome;
use crate::session::StreamId;
use crate::tls::TlsOpts;
use crate::tls::client_config;
use crate::tls::server_name;
use crate::tls::tls_error;
use crate::transport::Blocking;
use crate::transport::Close;
use crate::transport::Transport;
use crate::transport::block_on;

/// Idle time before TCP sends the first keepalive probe, and time between
/// probes (where the platform lets us set it). A connection idle in a pool
/// behind a NAT or a load balancer that dropped it fails once the probes go
/// unanswered (about 2.5 minutes with Linux's 9 probes), instead of hanging
/// the next read for as long as the operating system's retransmissions
/// last; the probes also keep such middleboxes from dropping it.
const KEEPALIVE_TIME: Duration = Duration::from_secs(60);
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(10);

/// Default size of the prepared-statement cache: the server allows 16,382
/// prepared statements across all its connections by default
/// (`max_prepared_stmt_count`), so a few hundred pooled connections must
/// fit.
pub const DEFAULT_STATEMENT_CACHE: usize = 32;

/// Connection parameters. Their `Debug` output never shows the password.
#[derive(Clone)]
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
    /// Bounds opening the TCP connection and every read and write until
    /// authentication ends (a server that accepts the connection but never
    /// greets fails too); after that, `read_timeout` and `write_timeout`
    /// apply.
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
    /// Whether to ask the server for its RSA public key when the password
    /// must be encrypted and no key is pinned. Off, such accounts fail over
    /// plain TCP instead of trusting a key whoever answers sends.
    pub get_server_public_key: bool,
    /// Prepared statements kept per connection ([`Conn::execute_cached`]).
    pub statement_cache: usize,
    /// Compressed protocol, if the server offers it: zstd falls back to
    /// zlib, and no compression at all if the server offers neither.
    pub compress: Option<Compression>,
    /// Allows `LOAD DATA LOCAL INFILE` of the files inside this directory
    /// (resolved when connecting, symbolic links included). A request is
    /// answered only if the statement being run names that file and the
    /// file, resolved too, is inside the directory; any other is refused
    /// with [`Error::LocalInfile`] before touching the disk.
    pub local_infile_dir: Option<PathBuf>,
}

impl fmt::Debug for Opts {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Destructured so that a new field cannot be left out silently.
        let Self {
            host,
            port,
            unix_socket,
            user,
            password,
            database,
            connect_timeout,
            read_timeout,
            write_timeout,
            client_flag,
            charset,
            tls,
            server_public_key,
            get_server_public_key,
            statement_cache,
            compress,
            local_infile_dir,
        } = self;
        let password = if password.is_empty() { "" } else { "<hidden>" };
        f.debug_struct("Opts")
            .field("host", host)
            .field("port", port)
            .field("unix_socket", unix_socket)
            .field("user", &String::from_utf8_lossy(user))
            .field("password", &password)
            .field(
                "database",
                &database.as_deref().map(String::from_utf8_lossy),
            )
            .field("connect_timeout", connect_timeout)
            .field("read_timeout", read_timeout)
            .field("write_timeout", write_timeout)
            .field("client_flag", client_flag)
            .field("charset", charset)
            .field("tls", tls)
            .field("server_public_key", server_public_key)
            .field("get_server_public_key", get_server_public_key)
            .field("statement_cache", statement_cache)
            .field("compress", compress)
            .field("local_infile_dir", local_infile_dir)
            .finish()
    }
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
            get_server_public_key: true,
            statement_cache: DEFAULT_STATEMENT_CACHE,
            compress: None,
            local_infile_dir: None,
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
            return Ok(Self::Unix(UnixStream::connect(path)?));
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
                    SockRef::from(&stream).set_tcp_keepalive(&keepalive())?;
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

    /// Sets the socket's read and write timeouts (`None` waits forever).
    fn set_timeouts(&self, read: Option<Duration>, write: Option<Duration>) -> io::Result<()> {
        match self {
            Self::Tcp(s) => {
                s.set_read_timeout(read)?;
                s.set_write_timeout(write)
            }
            #[cfg(unix)]
            Self::Unix(s) => {
                s.set_read_timeout(read)?;
                s.set_write_timeout(write)
            }
            Self::Tls(s) => {
                s.sock.set_read_timeout(read)?;
                s.sock.set_write_timeout(write)
            }
            #[cfg(feature = "fuzzing")]
            Self::Mock(_) => Ok(()),
        }
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

impl Close for Stream {
    fn close(&mut self) {
        self.shutdown();
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

/// TCP keepalive settings ([`KEEPALIVE_TIME`]).
fn keepalive() -> TcpKeepalive {
    let keepalive = TcpKeepalive::new().with_time(KEEPALIVE_TIME);
    #[cfg(any(
        target_os = "android",
        target_os = "freebsd",
        target_os = "ios",
        target_os = "linux",
        target_os = "macos",
        target_os = "netbsd",
        target_os = "windows",
    ))]
    let keepalive = keepalive.with_interval(KEEPALIVE_INTERVAL);
    keepalive
}

/// [`Opts::local_infile_dir`] resolved, or why it cannot be used.
pub(crate) fn local_infile_dir(opts: &Opts) -> Result<Option<PathBuf>> {
    opts.local_infile_dir
        .as_deref()
        .map(allowed_directory)
        .transpose()
}

fn allowed_directory(path: &Path) -> Result<PathBuf> {
    let resolved = fs::canonicalize(path)
        .map_err(|err| Error::Config(format!("local_infile_dir {}: {err}", path.display())))?;
    if !resolved.is_dir() {
        return Err(Error::Config(format!(
            "local_infile_dir {} is not a directory",
            path.display()
        )));
    }
    Ok(resolved)
}

pub(crate) fn load_public_key(path: &Path) -> Result<rsa::PublicKey> {
    let pem = fs::read(path).map_err(|e| {
        Error::Config(format!(
            "cannot read server_public_key {}: {e}",
            path.display()
        ))
    })?;
    parse_public_key(&pem)
        .map_err(|e| Error::Config(format!("server_public_key {}: {e}", path.display())))
}

/// A statement prepared on the server ([`Conn::prepare`]).
#[derive(Debug, Clone)]
pub struct Statement {
    /// The server's id for it, valid on this connection only.
    pub id: u32,
    /// Number of `?` placeholders.
    pub params: u16,
    /// Columns of its result set (empty when it returns none).
    pub columns: Vec<Column>,
}

/// Statements prepared by [`Conn::execute_cached`], keyed by their SQL; the
/// least recently used is closed when a new one needs the room.
#[derive(Debug, Default)]
pub(crate) struct StatementCache {
    capacity: usize,
    entries: HashMap<Vec<u8>, (Statement, u64)>,
    clock: u64,
}

impl StatementCache {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            // The statement being run is always kept.
            capacity: capacity.max(1),
            ..Self::default()
        }
    }

    pub(crate) fn get(&mut self, sql: &[u8]) -> Option<&Statement> {
        self.clock += 1;
        let clock = self.clock;
        self.entries.get_mut(sql).map(|(statement, used)| {
            *used = clock;
            &*statement
        })
    }

    /// The statement to close to make room for one more, if the cache is
    /// full.
    pub(crate) fn evict(&mut self) -> Option<u32> {
        if self.entries.len() < self.capacity {
            return None;
        }
        self.evict_oldest()
    }

    /// Forgets the least recently used statement and returns its id.
    pub(crate) fn evict_oldest(&mut self) -> Option<u32> {
        let oldest = self
            .entries
            .iter()
            .min_by_key(|(_, (_, used))| *used)
            .map(|(sql, _)| sql.clone())?;
        self.entries
            .remove(&oldest)
            .map(|(statement, _)| statement.id)
    }

    pub(crate) fn insert(&mut self, sql: Vec<u8>, statement: Statement) {
        self.clock += 1;
        self.entries.insert(sql, (statement, self.clock));
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Forgets every statement: the server has closed them.
    pub(crate) fn clear(&mut self) {
        self.entries.clear();
    }

    pub(crate) fn remove(&mut self, sql: &[u8]) -> Option<u32> {
        self.entries.remove(sql).map(|(statement, _)| statement.id)
    }
}

/// What a pipelined batch did ([`Conn::pipeline`]).
#[derive(Debug, Default)]
pub struct PipelineTotals {
    /// What `execute` would report as the rowcount of each statement, added
    /// up: its affected rows, or the size of its result set (the first one,
    /// for a statement with several).
    pub rowcount: u64,
    /// `LAST_INSERT_ID()` reported by the last OK reply.
    pub last_insert_id: u64,
    /// The first statement the server refused: its index in the batch and
    /// the error.
    pub error: Option<(usize, Error)>,
}

/// An authenticated connection to a MySQL or MariaDB server, over a
/// blocking stream: each command runs the protocol's operation
/// ([`crate::client`]) to its end.
#[derive(Debug)]
pub struct Conn {
    io: Blocking<Stream>,
    client: Client,
}

impl Conn {
    /// Opens the transport, performs the handshake and authenticates.
    ///
    /// # Errors
    ///
    /// I/O failures, protocol violations, authentication errors reported
    /// by the server, or authentication methods not implemented yet.
    pub fn connect(opts: &Opts) -> Result<Self> {
        // Settings first: a bad one fails before anything goes on the wire.
        let local_infile_dir = local_infile_dir(opts)?;
        let transport = Stream::connect(opts)?;
        // Until authentication ends, `connect_timeout` bounds every read and
        // write: a server that accepts but never greets must not hang us.
        transport.set_timeouts(
            opts.connect_timeout.or(opts.read_timeout),
            opts.connect_timeout.or(opts.write_timeout),
        )?;
        let conn = Self::handshake(transport, opts, local_infile_dir)?;
        conn.io
            .0
            .set_timeouts(opts.read_timeout, opts.write_timeout)?;
        Ok(conn)
    }

    /// Handshake and authentication over an already open transport.
    #[cfg(feature = "fuzzing")]
    pub(crate) fn establish(transport: Stream, opts: &Opts) -> Result<Self> {
        Self::handshake(transport, opts, local_infile_dir(opts)?)
    }

    fn handshake(
        transport: Stream,
        opts: &Opts,
        local_infile_dir: Option<PathBuf>,
    ) -> Result<Self> {
        let mut stream = PacketStream::new(transport);
        let mut buf = Vec::new();
        stream.read_into(&mut buf)?;
        let login = Login::new(opts, &buf, local_infile_dir.is_some())?;
        if let Some(request) = login.ssl_request() {
            // SSLRequest goes out in plain text, then the same socket is
            // wrapped in TLS and the sequence continues inside it.
            stream.write_packet(&request)?;
            let (raw, seq) = stream.into_parts()?;
            stream = PacketStream::from_parts(raw.upgrade(&opts.tls, &opts.host)?, seq);
        }
        let secure = stream.get_ref().is_secure();
        let (mut io, wire) = stream.into_wire();
        let client = block_on(Client::authenticate(
            &mut io,
            wire,
            login,
            opts,
            buf,
            local_infile_dir,
            secure,
        ))?;
        Ok(Self { io, client })
    }

    /// Compression in use, if any.
    #[must_use]
    pub const fn compression(&self) -> Option<Compression> {
        self.client.compression
    }

    /// Whether the connection is encrypted with TLS.
    #[must_use]
    pub fn is_tls(&self) -> bool {
        matches!(self.io.0, Stream::Tls(_))
    }

    /// How many bytes of statements [`Conn::pipeline`] may send in one
    /// batch (a single statement larger than this goes alone).
    ///
    /// While a batch is being written the server is already replying, and a
    /// large reply (rows) that fills the socket's buffers stops it reading
    /// until we read, which we only do once the write ends: the batch must
    /// fit in our send buffer, whatever the server does. 16 KiB is Linux's
    /// default for TCP (other systems start higher); Unix sockets on macOS
    /// default to 8 KiB.
    #[must_use]
    pub fn pipeline_budget(&self) -> usize {
        match self.io.0 {
            #[cfg(unix)]
            Stream::Unix(_) => 8 * 1024,
            _ => 16 * 1024,
        }
    }

    /// Whether the server has more results queued for the last query
    /// (multi-statement queries and stored procedures).
    #[must_use]
    pub const fn has_more_results(&self) -> bool {
        self.client.session.has_more_results()
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
        block_on(self.client.query(&mut self.io, sql))
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
        block_on(self.client.query_streamed(&mut self.io, sql))
    }

    /// Prepares `sql` (with `?` placeholders) on the server
    /// (`COM_STMT_PREPARE`).
    ///
    /// # Errors
    ///
    /// I/O failures, protocol violations, or the server's error (a syntax
    /// error, too many prepared statements...).
    pub fn prepare(&mut self, sql: &[u8]) -> Result<Statement> {
        block_on(self.client.prepare(&mut self.io, sql))
    }

    /// Runs a prepared statement (`COM_STMT_EXECUTE`, `payload` from
    /// [`crate::Execute::finish`]) and returns its first result, leaving
    /// rows on the wire as [`Conn::query_streamed`] does. Rows are binary
    /// ([`RowSet::is_binary`]).
    ///
    /// # Errors
    ///
    /// I/O failures, protocol violations or an ERR packet from the server.
    pub fn execute_streamed(&mut self, payload: &[u8]) -> Result<(QueryResult, Option<StreamId>)> {
        block_on(self.client.execute_streamed(&mut self.io, payload))
    }

    /// Like [`Conn::execute_streamed`], reading every row.
    ///
    /// # Errors
    ///
    /// As [`Conn::execute_streamed`].
    pub fn execute(&mut self, payload: &[u8]) -> Result<QueryResult> {
        block_on(self.client.execute(&mut self.io, payload))
    }

    /// Frees a prepared statement on the server (`COM_STMT_CLOSE`, which
    /// has no reply).
    ///
    /// # Errors
    ///
    /// I/O failures.
    pub fn close_statement(&mut self, id: u32) -> Result<()> {
        block_on(self.client.close_statement(&mut self.io, id))
    }

    /// Runs `sql` as a prepared statement with `params`, preparing it first
    /// unless this connection already has (the last
    /// [`Opts::statement_cache`] statements are kept); returns its first
    /// result as [`Conn::execute_streamed`] does.
    ///
    /// # Errors
    ///
    /// [`Error::Parameters`] when `params` does not match the statement's
    /// placeholders, and as [`Conn::prepare`] and [`Conn::execute_streamed`].
    pub fn execute_cached_streamed(
        &mut self,
        sql: &[u8],
        params: &Execute,
    ) -> Result<(QueryResult, Option<StreamId>)> {
        block_on(
            self.client
                .execute_cached_streamed(&mut self.io, sql, params),
        )
    }

    /// Like [`Conn::execute_cached_streamed`], reading every row.
    ///
    /// # Errors
    ///
    /// As [`Conn::execute_cached_streamed`].
    pub fn execute_cached(&mut self, sql: &[u8], params: &Execute) -> Result<QueryResult> {
        block_on(self.client.execute_cached(&mut self.io, sql, params))
    }

    /// Runs `statements` with the text protocol, sending them all before
    /// reading any reply (pipelining): one round trip for the batch instead
    /// of one per statement. Keep the batch within
    /// [`Conn::pipeline_budget`].
    ///
    /// The server runs every statement it receives, so one that fails does
    /// not stop those after it: every reply is read (keeping the connection
    /// in sync) and the first server error is returned with its index in
    /// [`PipelineTotals::error`]. Rows of result sets are read and dropped.
    ///
    /// # Errors
    ///
    /// I/O failures and protocol violations (server errors go in the
    /// totals).
    pub fn pipeline<'a>(
        &mut self,
        statements: impl IntoIterator<Item = &'a [u8]>,
    ) -> Result<PipelineTotals> {
        block_on(self.client.pipeline(&mut self.io, statements))
    }

    /// Like [`Conn::pipeline`] with `last` sent after `statements`, whose
    /// reply is left as [`Conn::query_streamed`] leaves it: its first
    /// result (rows still on the wire), with the rest queued.
    ///
    /// # Errors
    ///
    /// I/O failures, protocol violations, or the first server error of the
    /// batch (once every reply has been read).
    pub fn pipeline_streamed<'a>(
        &mut self,
        statements: impl IntoIterator<Item = &'a [u8]>,
        last: &'a [u8],
    ) -> Result<(PipelineTotals, QueryResult, Option<StreamId>)> {
        block_on(
            self.client
                .pipeline_streamed(&mut self.io, statements, last),
        )
    }

    /// Runs `sql` as a prepared statement once per item of `params`,
    /// pipelined as [`Conn::pipeline`] runs text statements (preparing it
    /// first unless this connection already has, as
    /// [`Conn::execute_cached`] does).
    ///
    /// # Errors
    ///
    /// [`Error::Parameters`] when an item does not match the statement's
    /// placeholders (nothing is sent), as [`Conn::prepare`], I/O failures
    /// and protocol violations (server errors go in the totals).
    pub fn execute_many_cached(
        &mut self,
        sql: &[u8],
        params: &[Execute],
    ) -> Result<PipelineTotals> {
        block_on(self.client.execute_many_cached(&mut self.io, sql, params))
    }

    /// Like [`Conn::execute_many_cached`] with `last` run after `params`,
    /// whose reply is left as [`Conn::execute_cached_streamed`] leaves it.
    ///
    /// # Errors
    ///
    /// As [`Conn::execute_many_cached`], and the first server error of the
    /// batch (once every reply has been read).
    pub fn execute_many_cached_streamed(
        &mut self,
        sql: &[u8],
        params: &[Execute],
        last: &Execute,
    ) -> Result<(PipelineTotals, QueryResult, Option<StreamId>)> {
        block_on(
            self.client
                .execute_many_cached_streamed(&mut self.io, sql, params, last),
        )
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
        block_on(self.client.read_rows(&mut self.io, id, rows, max_bytes))
    }

    /// Reads and drops up to `limit` rows of the streamed result set `id`
    /// without decoding them. Returns how many were skipped.
    ///
    /// # Errors
    ///
    /// As [`Conn::read_rows`].
    pub fn skip_rows(&mut self, id: StreamId, limit: usize) -> Result<(usize, ReadOutcome)> {
        block_on(self.client.skip_rows(&mut self.io, id, limit))
    }

    /// Reads the next queued result, if any.
    ///
    /// # Errors
    ///
    /// I/O failures, protocol violations or an ERR packet from the server.
    pub fn next_result(&mut self) -> Result<Option<QueryResult>> {
        block_on(self.client.next_result(&mut self.io))
    }

    /// Like [`Conn::next_result`], leaving the rows on the wire as
    /// [`Conn::query_streamed`] does. Unread rows of the current result set
    /// are discarded first.
    ///
    /// # Errors
    ///
    /// I/O failures, protocol violations or an ERR packet from the server.
    pub fn next_result_streamed(&mut self) -> Result<Option<(QueryResult, Option<StreamId>)>> {
        block_on(self.client.next_result_streamed(&mut self.io))
    }

    /// Switches the default database (`COM_INIT_DB`). `database` is in the
    /// session's character set.
    ///
    /// # Errors
    ///
    /// I/O failures or an ERR packet (e.g. unknown database).
    pub fn select_db(&mut self, database: &[u8]) -> Result<()> {
        block_on(self.client.select_db(&mut self.io, database))
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
        block_on(self.client.reset_connection(&mut self.io))
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
        block_on(
            self.client
                .change_user(&mut self.io, user, password, database),
        )
    }

    /// Checks that the server is alive.
    ///
    /// # Errors
    ///
    /// I/O failures or an unexpected response.
    pub fn ping(&mut self) -> Result<()> {
        block_on(self.client.ping(&mut self.io))
    }

    /// Sends `COM_QUIT` and closes the socket. Errors are ignored: the
    /// connection is being discarded either way.
    pub fn close(mut self) {
        let _ = block_on(self.client.quit(&mut self.io));
        self.io.shutdown();
    }

    #[must_use]
    pub fn server_version(&self) -> &str {
        &self.client.server_version
    }

    #[must_use]
    pub const fn connection_id(&self) -> u32 {
        self.client.connection_id
    }

    /// Status flags from the last OK/EOF packet (see [`crate::constants::status`]).
    #[must_use]
    pub const fn status_flags(&self) -> u16 {
        self.client.session.status
    }

    /// Capabilities negotiated with the server.
    #[must_use]
    pub const fn capabilities(&self) -> u32 {
        self.client.session.capabilities
    }

    /// Collation id requested in the handshake.
    #[must_use]
    pub const fn collation(&self) -> u8 {
        self.client.collation
    }

    /// The server's default character set, from its greeting. MySQL's
    /// `COM_RESET_CONNECTION` returns the session to it, not to the one
    /// chosen in the handshake (MariaDB keeps the latter).
    #[must_use]
    pub fn server_charset(&self) -> Option<&'static str> {
        crate::charset::charset_of(self.client.server_collation)
    }
}

#[cfg(test)]
mod tests {
    use std::net::TcpListener;

    use socket2::SockRef;

    use super::Opts;
    use super::Statement;
    use super::StatementCache;
    use super::Stream;

    #[test]
    fn tcp_connections_send_keepalive_probes() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let opts = Opts {
            host: "127.0.0.1".to_owned(),
            port: listener.local_addr().unwrap().port(),
            ..Opts::default()
        };
        let Stream::Tcp(stream) = Stream::connect(&opts).unwrap() else {
            panic!("expected a TCP stream");
        };
        let socket = SockRef::from(&stream);
        assert!(socket.keepalive().unwrap());
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            assert_eq!(socket.tcp_keepalive_time().unwrap(), super::KEEPALIVE_TIME);
            assert_eq!(
                socket.tcp_keepalive_interval().unwrap(),
                super::KEEPALIVE_INTERVAL
            );
        }
    }

    fn statement(id: u32) -> Statement {
        Statement {
            id,
            params: 0,
            columns: Vec::new(),
        }
    }

    #[test]
    fn the_cache_frees_its_least_recently_used_statement_on_demand() {
        let mut cache = StatementCache::new(32);
        assert_eq!(cache.evict_oldest(), None);
        cache.insert(b"a".to_vec(), statement(1));
        cache.insert(b"b".to_vec(), statement(2));
        cache.insert(b"c".to_vec(), statement(3));
        // Not full: nothing to make room for.
        assert_eq!(cache.evict(), None);
        assert!(cache.get(b"a").is_some());
        assert_eq!(cache.evict_oldest(), Some(2));
        assert_eq!(cache.evict_oldest(), Some(3));
        assert!(!cache.is_empty());
        assert_eq!(cache.evict_oldest(), Some(1));
        assert!(cache.is_empty());
    }

    #[test]
    fn debug_output_hides_the_password() {
        let opts = Opts {
            user: b"app".to_vec(),
            password: "s3cret-p4ss".to_owned(),
            database: Some(b"shop".to_vec()),
            ..Opts::default()
        };
        let shown = format!("{opts:?}");
        assert!(!shown.contains("s3cret-p4ss"), "{shown}");
        assert!(shown.contains("password: \"<hidden>\""), "{shown}");
        assert!(shown.contains("user: \"app\""), "{shown}");
        assert!(shown.contains("database: Some(\"shop\")"), "{shown}");
        let shown = format!("{:?}", Opts::default());
        assert!(shown.contains("password: \"\""), "{shown}");
    }
}
