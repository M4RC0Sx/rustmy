//! A blocking connection: transport, handshake, authentication and commands.

use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::fs::File;
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

use crate::auth::AuthPlugin;
use crate::auth::parse_public_key;
use crate::auth::parsec_max_factor;
use crate::binary::Execute;
use crate::buf::Reader;
use crate::buf::put_nul_bytes;
use crate::column::Column;
use crate::compress::Compression;
use crate::constants::capability;
use crate::constants::command;
use crate::constants::status;
use crate::error::Error;
use crate::error::Result;
use crate::login::Auth;
use crate::login::AuthSettings;
use crate::login::AuthStep;
use crate::login::Login;
use crate::packet::PacketStream;
use crate::packet::ReplyStart;
use crate::packet::release;
use crate::result::QueryResult;
use crate::result::RowPacket;
use crate::result::RowSet;
use crate::result::accept_row_packet;
use crate::result::parse_eof;
use crate::result::parse_err;
use crate::result::parse_ok;
use crate::result::row_terminator;
use crate::sql::local_infile_names;
use crate::tls::TlsOpts;
use crate::tls::client_config;
use crate::tls::server_name;
use crate::tls::tls_error;

/// The server could not re-prepare a statement after a schema change.
const ER_NEED_REPREPARE: u16 = 1615;
/// The server holds `max_prepared_stmt_count` statements across all its
/// connections.
const ER_MAX_PREPARED_STMT_COUNT_REACHED: u16 = 1461;

/// Idle time before TCP sends the first keepalive probe, and time between
/// probes (where the platform lets us set it). A connection idle in a pool
/// behind a NAT or a load balancer that dropped it fails once the probes go
/// unanswered (about 2.5 minutes with Linux's 9 probes), instead of hanging
/// the next read for as long as the operating system's retransmissions
/// last; the probes also keep such middleboxes from dropping it.
const KEEPALIVE_TIME: Duration = Duration::from_secs(60);
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(10);

/// Bytes of a local file sent per packet (`LOAD DATA LOCAL INFILE`). Measured
/// from 4 to 64 KiB (benches/README.md): flat on a local link, while over a
/// slow compressed one 16 KiB was 5% faster with zstd than 64 KiB.
const LOCAL_INFILE_CHUNK: usize = 16 * 1024;

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
fn local_infile_dir(opts: &Opts) -> Result<Option<PathBuf>> {
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

/// Fills `buf` from `file` unless it ends first; returns the bytes read.
fn read_full(file: &mut File, buf: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while let Some(rest) = buf.get_mut(filled..).filter(|rest| !rest.is_empty()) {
        match file.read(rest) {
            Ok(0) => break,
            Ok(read) => filled += read,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(err) => return Err(err),
        }
    }
    Ok(filled)
}

fn load_public_key(path: &Path) -> Result<rsa::PublicKey> {
    let pem = fs::read(path).map_err(|e| {
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
struct StatementCache {
    capacity: usize,
    entries: HashMap<Vec<u8>, (Statement, u64)>,
    clock: u64,
}

impl StatementCache {
    fn new(capacity: usize) -> Self {
        Self {
            // The statement being run is always kept.
            capacity: capacity.max(1),
            ..Self::default()
        }
    }

    fn get(&mut self, sql: &[u8]) -> Option<&Statement> {
        self.clock += 1;
        let clock = self.clock;
        self.entries.get_mut(sql).map(|(statement, used)| {
            *used = clock;
            &*statement
        })
    }

    /// The statement to close to make room for one more, if the cache is
    /// full.
    fn evict(&mut self) -> Option<u32> {
        if self.entries.len() < self.capacity {
            return None;
        }
        self.evict_oldest()
    }

    /// Forgets the least recently used statement and returns its id.
    fn evict_oldest(&mut self) -> Option<u32> {
        let oldest = self
            .entries
            .iter()
            .min_by_key(|(_, (_, used))| *used)
            .map(|(sql, _)| sql.clone())?;
        self.entries
            .remove(&oldest)
            .map(|(statement, _)| statement.id)
    }

    fn insert(&mut self, sql: Vec<u8>, statement: Statement) {
        self.clock += 1;
        self.entries.insert(sql, (statement, self.clock));
    }

    fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Forgets every statement: the server has closed them.
    fn clear(&mut self) {
        self.entries.clear();
    }

    fn remove(&mut self, sql: &[u8]) -> Option<u32> {
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

/// Which local files the server may ask for during the current command.
#[derive(Debug)]
enum Infile {
    /// None.
    Refuse,
    /// Those the statement names (`LOAD DATA LOCAL INFILE 'name'`).
    Allow(Vec<Vec<u8>>),
    /// None, and a request breaks the connection: the server would read
    /// the statements sent after this one as the file's contents.
    Pipelined,
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
    /// What authentication needs besides the password, also for
    /// `COM_CHANGE_USER`.
    auth: AuthSettings,
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
    /// Whether the reply being read comes from `COM_STMT_EXECUTE` (its
    /// result sets, queued ones included, have binary rows).
    binary_reply: bool,
    /// Statements prepared by [`Conn::execute_cached`], by SQL.
    statements: StatementCache,
    /// Compression in use, negotiated in the handshake.
    compression: Option<Compression>,
    /// [`Opts::local_infile_dir`], resolved.
    local_infile_dir: Option<PathBuf>,
    /// Local files the current command may send.
    infile: Infile,
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
        conn.stream
            .get_ref()
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
        let auth = AuthSettings {
            pinned_key: opts
                .server_public_key
                .as_deref()
                .map(load_public_key)
                .transpose()?,
            get_server_public_key: opts.get_server_public_key,
            parsec_max_factor: parsec_max_factor(opts.connect_timeout),
            secure: stream.get_ref().is_secure(),
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
        let mut conn = Self {
            stream,
            capabilities,
            status: greeting.status,
            server_version: greeting.server_version,
            connection_id: greeting.connection_id,
            default_plugin: plugin,
            greeting_nonce: greeting.nonce,
            auth,
            collation,
            server_collation: greeting.collation,
            buf,
            pending: None,
            last_stream_id: 0,
            binary_reply: false,
            statements: StatementCache::new(opts.statement_cache),
            compression,
            local_infile_dir,
            infile: Infile::Refuse,
        };
        conn.stream.write_packet(&response)?;
        conn.auth_exchange(exchange, opts.password.as_bytes())?;
        conn.stream.authenticated();
        if let Some(algorithm) = compression {
            conn.stream.compress(algorithm)?;
        }
        Ok(conn)
    }

    /// Compression in use, if any.
    #[must_use]
    pub const fn compression(&self) -> Option<Compression> {
        self.compression
    }

    /// Whether the connection is encrypted with TLS.
    #[must_use]
    pub fn is_tls(&self) -> bool {
        matches!(self.stream.get_ref(), Stream::Tls(_))
    }

    /// Runs the server-driven part of authentication (auth switches and
    /// plugin-specific round trips) until OK or ERR.
    fn auth_exchange(&mut self, mut auth: Auth, password: &[u8]) -> Result<()> {
        loop {
            self.read_packet()?;
            match auth.step(&self.buf, password, &self.auth)? {
                AuthStep::Send(reply) => self.stream.write_packet(&reply)?,
                AuthStep::Read => {}
                AuthStep::Done { status } => {
                    self.status = status;
                    return Ok(());
                }
            }
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
        self.infile = self.allowed_files(sql);
        self.stream.write_command(command::QUERY, sql)?;
        self.binary_reply = false;
        self.read_head()
    }

    /// Prepares `sql` (with `?` placeholders) on the server
    /// (`COM_STMT_PREPARE`).
    ///
    /// # Errors
    ///
    /// I/O failures, protocol violations, or the server's error (a syntax
    /// error, too many prepared statements...).
    pub fn prepare(&mut self, sql: &[u8]) -> Result<Statement> {
        self.drain_results()?;
        self.stream.write_command(command::STMT_PREPARE, sql)?;
        self.read_packet()?;
        match self.buf.first() {
            Some(0x00) => {}
            Some(0xFF) => return Err(parse_err(&self.buf)),
            _ => return Err(Error::protocol("unexpected reply to COM_STMT_PREPARE")),
        }
        let mut r = Reader::new(&self.buf);
        r.skip(1)?;
        let id = r.u32()?;
        let columns = usize::from(r.u16()?);
        let params = r.u16()?;
        let deprecate_eof = self.capabilities & capability::DEPRECATE_EOF != 0;
        // Parameter definitions say nothing useful (every type is "string"
        // before the first execution).
        for _ in 0..params {
            self.read_packet()?;
        }
        if params > 0 && !deprecate_eof {
            self.read_packet()?;
        }
        let mut definitions = Vec::with_capacity(columns.min(4096));
        for _ in 0..columns {
            self.read_packet()?;
            definitions.push(Column::parse(&self.buf)?);
        }
        if columns > 0 && !deprecate_eof {
            self.read_packet()?;
        }
        Ok(Statement {
            id,
            params,
            columns: definitions,
        })
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
        self.drain_results()?;
        self.stream.write_command(command::STMT_EXECUTE, payload)?;
        self.binary_reply = true;
        self.read_head()
    }

    /// Like [`Conn::execute_streamed`], reading every row.
    ///
    /// # Errors
    ///
    /// As [`Conn::execute_streamed`].
    pub fn execute(&mut self, payload: &[u8]) -> Result<QueryResult> {
        let (result, stream) = self.execute_streamed(payload)?;
        self.read_all(result, stream)
    }

    /// Frees a prepared statement on the server (`COM_STMT_CLOSE`, which
    /// has no reply).
    ///
    /// # Errors
    ///
    /// I/O failures.
    pub fn close_statement(&mut self, id: u32) -> Result<()> {
        self.drain_results()?;
        self.stream
            .write_command(command::STMT_CLOSE, &id.to_le_bytes())
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
        let id = self.cached_statement(sql, params)?;
        match self.execute_streamed(&params.finish(id)) {
            // The server could not re-prepare it after a schema change:
            // prepare it again, once.
            Err(Error::Server {
                code: ER_NEED_REPREPARE,
                ..
            }) => {
                if let Some(id) = self.statements.remove(sql) {
                    self.close_statement(id)?;
                }
                let id = self.cached_statement(sql, params)?;
                self.execute_streamed(&params.finish(id))
            }
            other => other,
        }
    }

    /// Like [`Conn::execute_cached_streamed`], reading every row.
    ///
    /// # Errors
    ///
    /// As [`Conn::execute_cached_streamed`].
    pub fn execute_cached(&mut self, sql: &[u8], params: &Execute) -> Result<QueryResult> {
        let (result, stream) = self.execute_cached_streamed(sql, params)?;
        self.read_all(result, stream)
    }

    /// The id of `sql` prepared on this connection, preparing it (and
    /// closing the least recently used statement) if needed.
    fn cached_statement(&mut self, sql: &[u8], params: &Execute) -> Result<u32> {
        let expected = if let Some(statement) = self.statements.get(sql) {
            (statement.id, statement.params)
        } else {
            if let Some(oldest) = self.statements.evict() {
                self.close_statement(oldest)?;
            }
            let statement = match self.prepare(sql) {
                // The server's statements are all taken, some by this
                // connection's cache: free the least recently used one and
                // try once more.
                Err(Error::Server {
                    code: ER_MAX_PREPARED_STMT_COUNT_REACHED,
                    ..
                }) if !self.statements.is_empty() => {
                    if let Some(oldest) = self.statements.evict_oldest() {
                        self.close_statement(oldest)?;
                    }
                    self.prepare(sql)?
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
        match self.stream.get_ref() {
            #[cfg(unix)]
            Stream::Unix(_) => 8 * 1024,
            _ => 16 * 1024,
        }
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
        let replies = self.send_pipeline(command::QUERY, statements)?;
        let mut totals = PipelineTotals::default();
        self.read_replies(&replies, &mut totals)?;
        Ok(totals)
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
        let replies = self.send_pipeline(command::QUERY, statements.into_iter().chain([last]))?;
        self.finish_pipeline_streamed(&replies)
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
        let payloads = self.execute_payloads(sql, params)?;
        let replies =
            self.send_pipeline(command::STMT_EXECUTE, payloads.iter().map(Vec::as_slice))?;
        let mut totals = PipelineTotals::default();
        self.read_replies(&replies, &mut totals)?;
        Ok(totals)
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
        let mut payloads = self.execute_payloads(sql, params)?;
        payloads.extend(self.execute_payloads(sql, std::slice::from_ref(last))?);
        let replies =
            self.send_pipeline(command::STMT_EXECUTE, payloads.iter().map(Vec::as_slice))?;
        self.finish_pipeline_streamed(&replies)
    }

    /// `COM_STMT_EXECUTE` payloads of `sql` (prepared if needed) for each
    /// item of `params`.
    fn execute_payloads(&mut self, sql: &[u8], params: &[Execute]) -> Result<Vec<Vec<u8>>> {
        let mut payloads = Vec::with_capacity(params.len());
        for item in params {
            let id = self.cached_statement(sql, item)?;
            payloads.push(item.finish(id));
        }
        Ok(payloads)
    }

    /// Reads every reply of a pipelined batch but the last one's head, as
    /// [`Conn::pipeline_streamed`] leaves it.
    fn finish_pipeline_streamed(
        &mut self,
        replies: &[ReplyStart],
    ) -> Result<(PipelineTotals, QueryResult, Option<StreamId>)> {
        let (&last, others) = replies
            .split_last()
            .ok_or_else(|| Error::protocol("empty pipeline"))?;
        let mut totals = PipelineTotals::default();
        self.read_replies(others, &mut totals)?;
        self.stream.expect_reply(last);
        if let Some((_, err)) = totals.error.take() {
            // The last statement ran too: read its reply before reporting.
            match self.read_reply(&mut totals) {
                Ok(()) | Err(Error::Server { .. }) => return Err(err),
                Err(other) => return Err(other),
            }
        }
        let (head, stream) = self.read_head()?;
        Ok((totals, head, stream))
    }

    /// Sends one `cmd` (`COM_QUERY` or `COM_STMT_EXECUTE`) per payload in a
    /// single write; returns where each reply starts.
    fn send_pipeline<'a>(
        &mut self,
        cmd: u8,
        payloads: impl IntoIterator<Item = &'a [u8]>,
    ) -> Result<Vec<ReplyStart>> {
        self.drain_results()?;
        let payloads: Vec<&[u8]> = payloads.into_iter().collect();
        self.infile = match payloads.as_slice() {
            [only] if cmd == command::QUERY => self.allowed_files(only),
            [_] => Infile::Refuse,
            _ => Infile::Pipelined,
        };
        let mut replies = Vec::new();
        self.stream
            .write_commands(cmd, payloads.iter().copied(), &mut replies)?;
        self.binary_reply = cmd == command::STMT_EXECUTE;
        Ok(replies)
    }

    /// Reads the replies starting at `replies`, adding them to `totals`.
    fn read_replies(&mut self, replies: &[ReplyStart], totals: &mut PipelineTotals) -> Result<()> {
        for (index, &start) in replies.iter().enumerate() {
            self.stream.expect_reply(start);
            match self.read_reply(totals) {
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
    fn read_reply(&mut self, totals: &mut PipelineTotals) -> Result<()> {
        let mut first = true;
        loop {
            let (result, stream) = self.read_head()?;
            let count = if let Some(id) = stream {
                u64::try_from(self.skip_rows(id, usize::MAX)?.0).unwrap_or(u64::MAX)
            } else {
                totals.last_insert_id = result.last_insert_id;
                result.affected_rows
            };
            if first {
                totals.rowcount += count;
                first = false;
            }
            if !self.has_more_results() {
                return Ok(());
            }
        }
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
                    // A large row skipped may have grown the buffer.
                    release(&mut self.buf);
                    return Ok((
                        skipped,
                        ReadOutcome::Done {
                            warnings: end.warnings,
                        },
                    ));
                }
                Err(err) => {
                    self.pending = None;
                    release(&mut self.buf);
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
            Some(0xFB) => self.local_infile(),
            Some(_) => {
                let count = Reader::new(&self.buf).lenenc_len()?;
                self.read_columns(count, self.binary_reply)
            }
            None => Err(Error::protocol("empty response packet")),
        }
    }

    /// Local files `sql` may make the server ask for.
    fn allowed_files(&self, sql: &[u8]) -> Infile {
        if self.local_infile_dir.is_none() {
            return Infile::Refuse;
        }
        let no_backslash_escapes = self.status & status::NO_BACKSLASH_ESCAPES != 0;
        Infile::Allow(local_infile_names(sql, no_backslash_escapes))
    }

    /// Answers the server's request for a local file (`LOAD DATA LOCAL
    /// INFILE`, the request in `buf`) with the file, or with an empty one
    /// if it is not allowed, and reads the statement's reply.
    fn local_infile(&mut self) -> Result<(QueryResult, Option<StreamId>)> {
        if matches!(self.infile, Infile::Pipelined) {
            self.stream.get_mut().shutdown();
            return Err(Error::protocol(
                "the server asked for a local file in the middle of a pipelined batch",
            ));
        }
        let name = self.buf.get(1..).unwrap_or_default().to_vec();
        let refused = match self.open_local_file(&name) {
            Ok((path, file)) => {
                self.send_file(&path, file)?;
                None
            }
            Err(reason) => Some(reason),
        };
        if refused.is_some() {
            // An empty file ends the request; the statement loads nothing.
            self.stream.write_packet(&[])?;
        }
        self.read_packet()?;
        let reply = match self.buf.first() {
            Some(0x00) => {
                let ok = parse_ok(&self.buf)?;
                self.status = ok.status;
                Ok((QueryResult::from_ok(ok), None))
            }
            Some(0xFF) => Err(parse_err(&self.buf)),
            _ => Err(Error::protocol(
                "expected an OK or ERR packet after a local file",
            )),
        };
        match (refused, reply) {
            (_, Err(err @ (Error::Io(_) | Error::Protocol(_)))) | (None, Err(err)) => Err(err),
            (Some(reason), _) => Err(Error::LocalInfile(reason)),
            (None, reply) => reply,
        }
    }

    /// Opens the local file `name` (as the server asked for it) if this
    /// command may send it, or says why not. Nothing on disk is touched
    /// unless the statement named the file.
    fn open_local_file(&self, name: &[u8]) -> std::result::Result<(PathBuf, File), String> {
        let shown = String::from_utf8_lossy(name);
        let Some(dir) = &self.local_infile_dir else {
            return Err(format!(
                "the server asked for {shown:?}, but LOAD DATA LOCAL INFILE is disabled \
                 (local_infile_dir allows the files of a directory)"
            ));
        };
        let named = match &self.infile {
            Infile::Allow(names) => names.iter().any(|allowed| allowed == name),
            Infile::Refuse | Infile::Pipelined => false,
        };
        if !named {
            return Err(format!(
                "the server asked for {shown:?}, which the statement being run does not name"
            ));
        }
        let path = std::str::from_utf8(name)
            .map_err(|_| format!("the file name {shown:?} is not valid UTF-8"))?;
        // Relative names start at the working directory, as in other clients.
        let resolved = fs::canonicalize(path).map_err(|err| format!("{path}: {err}"))?;
        if !resolved.starts_with(dir) {
            return Err(format!(
                "{path} is outside local_infile_dir ({})",
                dir.display()
            ));
        }
        // Checked before opening too: opening a FIFO blocks until something
        // writes to it, with the connection held. The descriptor is checked
        // again below, in case the path changed in between.
        let is_file = fs::metadata(&resolved)
            .map_err(|err| format!("{path}: {err}"))?
            .is_file();
        if !is_file {
            return Err(format!("{path} is not a regular file"));
        }
        let file = File::open(&resolved).map_err(|err| format!("{path}: {err}"))?;
        let is_file = file
            .metadata()
            .map_err(|err| format!("{path}: {err}"))?
            .is_file();
        if !is_file {
            return Err(format!("{path} is not a regular file"));
        }
        Ok((resolved, file))
    }

    /// Sends `file` in packets and the empty packet that ends it. If the
    /// file cannot be read to the end, the connection is closed instead,
    /// so the server aborts the statement rather than loading part of it.
    fn send_file(&mut self, path: &Path, mut file: File) -> Result<()> {
        let mut chunk = vec![0; LOCAL_INFILE_CHUNK];
        loop {
            let len = match read_full(&mut file, &mut chunk) {
                Ok(len) => len,
                Err(err) => {
                    self.stream.get_mut().shutdown();
                    return Err(io::Error::new(
                        err.kind(),
                        format!("reading {}: {err}", path.display()),
                    )
                    .into());
                }
            };
            if len == 0 {
                return self.stream.write_packet(&[]);
            }
            self.stream
                .write_packet(chunk.get(..len).unwrap_or_default())?;
        }
    }

    fn read_columns(
        &mut self,
        count: usize,
        binary: bool,
    ) -> Result<(QueryResult, Option<StreamId>)> {
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
        Ok((QueryResult::rowset(columns, binary), Some(id)))
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
        // The reset closes every prepared statement.
        self.statements.clear();
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
        self.stream.write_command(command::CHANGE_USER, &payload)?;
        self.auth_exchange(auth, password.as_bytes())
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
        // What the previous command allowed ends with its results.
        self.infile = Infile::Refuse;
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
