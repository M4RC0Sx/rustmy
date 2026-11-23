//! The asyncio connection (first cut of `rustmy.aio`): the protocol's
//! operations (`rustmy_proto::Client`, `async fn`s over a transport) run on
//! the event loop's thread, with no runtime and no other thread.
//!
//! The object is the core of an `asyncio.BufferedProtocol`: the loop writes
//! what the server sent into its buffer and calls `buffer_updated`, which
//! hands the bytes to the transport ([`LoopIo`]) and polls the running
//! operation once more; an operation waiting for bytes is left pending
//! until the next call. What it writes is passed to the loop's transport
//! after each poll, and its result resolves the command's asyncio future.

use std::collections::VecDeque;
use std::future::Future;
use std::io;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::MutexGuard;
use std::sync::PoisonError;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::task::Context;
use std::task::Poll;
use std::task::Waker;

use pyo3::IntoPyObjectExt;
use pyo3::prelude::*;
use pyo3::types::PyByteArray;
use pyo3::types::PyBytes;
use pyo3::types::PyDict;
use pyo3::types::PyNone;
use pyo3::types::PyString;
use pyo3::types::PyTuple;
use rustmy_proto::Client;
use rustmy_proto::Compression;
use rustmy_proto::Error as ProtoError;
use rustmy_proto::Input;
use rustmy_proto::MaybeTls;
use rustmy_proto::QueryResult;
use rustmy_proto::ReadOutcome;
use rustmy_proto::RowSet;
use rustmy_proto::StartTls;
use rustmy_proto::StreamId;
use rustmy_proto::Transport;
use rustmy_proto::constants::status;

use crate::charset::Codec;
use crate::connection::Plan;
use crate::connection::connect_opts;
use crate::connection::names_clause;
use crate::connection::prepared_plan;
use crate::connection::warn_abandoned_stream;
use crate::convert::Converter;
use crate::convert::Custom;
use crate::convert::description;
use crate::convert::result_to_py;
use crate::errors::InterfaceError;
use crate::errors::Phase;
use crate::errors::ProgrammingError;
use crate::errors::from_proto;
use crate::params::Encoder;
use crate::params::statement;
use rustmy_proto::charset::charset_of;

/// Size of the buffer the event loop reads into.
const LOOP_BUFFER: usize = 256 * 1024;
/// Rows are converted every this many bytes of them.
const BATCH_BYTES: usize = 64 * 1024;

/// What the event loop and the running operation share.
#[derive(Debug, Default)]
struct Shared {
    /// Received and not yet handed to the operation: `pos..`.
    received: Vec<u8>,
    pos: usize,
    /// The stream ended (or failed): reads see its end.
    closed: bool,
    /// Written by the operation, for the loop's transport.
    outbox: Vec<u8>,
    /// The operation asked to close the stream.
    shutdown: bool,
}

fn lock(shared: &Mutex<Shared>) -> MutexGuard<'_, Shared> {
    shared.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The transport the operations see: reads take what the loop received
/// (pending until it has something), writes queue for the loop.
#[derive(Debug, Clone)]
struct LoopIo(Arc<Mutex<Shared>>);

impl Transport for LoopIo {
    fn fill(&mut self, input: &mut Input) -> impl Future<Output = io::Result<usize>> {
        let shared = Arc::clone(&self.0);
        std::future::poll_fn(move |_| {
            let mut shared = lock(&shared);
            let available = shared.received.len().saturating_sub(shared.pos);
            if available == 0 {
                return if shared.closed {
                    Poll::Ready(Ok(0))
                } else {
                    Poll::Pending
                };
            }
            let start = shared.pos;
            let bytes = shared.received.get(start..).unwrap_or_default();
            let n = bytes.len();
            if let Some(dst) = input.spare(n).get_mut(..n) {
                dst.copy_from_slice(bytes);
            }
            input.filled(n);
            shared.received.clear();
            shared.pos = 0;
            Poll::Ready(Ok(n))
        })
    }

    fn read_body(
        &mut self,
        _out: &mut Vec<u8>,
        _len: usize,
    ) -> impl Future<Output = Option<io::Result<usize>>> {
        std::future::ready(None)
    }

    fn write(&mut self, parts: &[&[u8]]) -> impl Future<Output = io::Result<()>> {
        let mut shared = lock(&self.0);
        for part in parts {
            shared.outbox.extend_from_slice(part);
        }
        std::future::ready(Ok(()))
    }

    fn flush(&mut self) -> impl Future<Output = io::Result<()>> {
        std::future::ready(Ok(()))
    }

    fn shutdown(&mut self) {
        lock(&self.0).shutdown = true;
    }
}

/// An open session: the protocol's state and the transport under it (TLS
/// keeps state of its own between commands).
struct Link {
    client: Client,
    io: MaybeTls<LoopIo>,
}

/// What an operation ends with: the connection to go on with (none once it
/// is unusable) and the value for the command's future.
struct Outcome {
    link: Option<Box<Link>>,
    value: PyResult<Py<PyAny>>,
}

type Operation = Pin<Box<dyn Future<Output = Outcome> + Send>>;

enum State {
    /// Waiting for a command.
    Idle(Box<Link>),
    /// A command (or the handshake) is running.
    Running(Operation),
    Closed,
}

struct Inner {
    state: State,
    /// The asyncio future the running operation resolves.
    waiter: Option<Py<PyAny>>,
    /// The loop's transport, once connected.
    transport: Option<Py<PyAny>>,
}

/// Turns an error of the protocol into the driver's exception.
fn to_pyerr(err: ProtoError, phase: Phase, target: &str, codec: &Codec) -> PyErr {
    from_proto(err, phase, target, |message| {
        Python::attach(|py| codec.decode_lenient(py, message))
    })
}

#[pyclass(module = "rustmy._core", frozen)]
pub(crate) struct AioConnection {
    inner: Mutex<Inner>,
    shared: Arc<Mutex<Shared>>,
    buffer: Py<PyByteArray>,
    event_loop: Py<PyAny>,
    /// The session's character set; `set_character_set` replaces it.
    codec: Arc<Mutex<Arc<Codec>>>,
    /// `conv=` decoders and encoders that differ from the defaults.
    custom: Mutex<Option<Arc<Custom>>>,
    target: Arc<str>,
    /// Whether `bytes` parameters carry the `_binary` introducer.
    binary_prefix: bool,
    /// Command generation and the streamed result set still open, shared
    /// with the streams.
    counters: Arc<Counters>,
}

/// Bumped by every command, so a streamed result set can tell whether
/// another command ran (and discarded its rows) since it started; and the
/// generation of the streamed result set whose rows the user has not all
/// fetched yet, or 0.
#[derive(Debug, Default)]
struct Counters {
    generation: AtomicU64,
    open_stream: AtomicU64,
}

impl Counters {
    /// The streamed result set of `generation` has no rows left for the
    /// user, so the next command abandons nothing.
    fn stream_finished(&self, generation: u64) {
        let _ =
            self.open_stream
                .compare_exchange(generation, 0, Ordering::SeqCst, Ordering::SeqCst);
    }
}

impl AioConnection {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Polls the running operation once; resolves its future if it ended.
    /// Then hands what it wrote to the loop's transport.
    fn advance(&self, py: Python<'_>, inner: &mut Inner) -> PyResult<()> {
        if let State::Running(operation) = &mut inner.state
            && let Poll::Ready(outcome) = operation
                .as_mut()
                .poll(&mut Context::from_waker(Waker::noop()))
        {
            inner.state = outcome.link.map_or(State::Closed, State::Idle);
            if let Some(waiter) = inner.waiter.take() {
                resolve(py, waiter.bind(py), outcome.value)?;
            }
        }
        self.flush(py, inner)
    }

    fn flush(&self, py: Python<'_>, inner: &Inner) -> PyResult<()> {
        let (out, shutdown) = {
            let mut shared = lock(&self.shared);
            (std::mem::take(&mut shared.outbox), shared.shutdown)
        };
        if let Some(transport) = &inner.transport {
            let transport = transport.bind(py);
            if !out.is_empty() {
                transport.call_method1("write", (PyBytes::new(py, &out),))?;
            }
            if shutdown {
                transport.call_method0("close")?;
            }
        }
        Ok(())
    }

    /// Starts `operation`, which takes the idle connection, and returns the
    /// future of its result.
    fn start(
        &self,
        py: Python<'_>,
        operation: impl FnOnce(Box<Link>) -> Operation,
    ) -> PyResult<Py<PyAny>> {
        self.start_as(py, None, operation)
    }

    /// Starts `operation`: a command (`stream` is `None`) first warns that
    /// it discards the rows of a streamed result set the user has not all
    /// fetched (before anything changes, so a warning turned into an error
    /// leaves them as they are) and starts a new generation; a stream's own
    /// read (`stream` is its generation) fails if another command ran.
    fn start_as(
        &self,
        py: Python<'_>,
        stream: Option<u64>,
        operation: impl FnOnce(Box<Link>) -> Operation,
    ) -> PyResult<Py<PyAny>> {
        match stream {
            None => {
                if self.counters.open_stream.load(Ordering::SeqCst) != 0
                    && matches!(self.lock().state, State::Idle(_))
                {
                    warn_abandoned_stream(py)?;
                }
            }
            Some(generation) => {
                if self.counters.generation.load(Ordering::SeqCst) != generation {
                    return Err(abort_error(Abort::Discarded));
                }
            }
        }
        let mut inner = self.lock();
        let link = match std::mem::replace(&mut inner.state, State::Closed) {
            State::Idle(link) => link,
            State::Running(running) => {
                inner.state = State::Running(running);
                return Err(busy_error());
            }
            State::Closed => return Err(self.closed_error()),
        };
        let waiter = self
            .event_loop
            .bind(py)
            .call_method0("create_future")?
            .unbind();
        inner.waiter = Some(waiter.clone_ref(py));
        if stream.is_none() {
            // Any pending streamed rows are about to be discarded.
            self.counters.generation.fetch_add(1, Ordering::SeqCst);
            self.counters.open_stream.store(0, Ordering::SeqCst);
        }
        inner.state = State::Running(operation(link));
        self.advance(py, &mut inner)?;
        Ok(waiter)
    }

    /// Starts a statement whose rows stay on the wire: the future gives
    /// `(description, stream, rowcount, last_insert_id, warnings)`, as
    /// `Connection.execute_streamed` returns it (rowcount -1 for a result
    /// set), or `None` when `start` finds nothing (no next result).
    fn stream_command<F>(slf: &Bound<'_, Self>, as_dict: bool, start: F) -> PyResult<Py<PyAny>>
    where
        F: for<'a> FnOnce(&'a mut Client, &'a mut MaybeTls<LoopIo>) -> StartFuture<'a>
            + Send
            + 'static,
    {
        let this = slf.get();
        let codec = this.codec();
        let custom = this.custom();
        let target = Arc::clone(&this.target);
        let counters = Arc::clone(&this.counters);
        let conn = slf.clone().unbind();
        this.start(slf.py(), move |mut link| {
            Box::pin(async move {
                let Link { client, io } = &mut *link;
                let value = match start(client, io).await {
                    Ok(Some((head, id))) => Python::attach(|py| {
                        stream_head(
                            py,
                            conn,
                            head,
                            id,
                            as_dict,
                            &codec,
                            custom.as_deref(),
                            &counters,
                        )
                    })
                    .map_err(Failure::Python),
                    Ok(None) => Ok(none()),
                    Err(err) => Err(err.into()),
                };
                outcome(link, value, &codec, &target)
            })
        })
    }

    fn closed_error(&self) -> PyErr {
        to_pyerr(
            io::Error::from(io::ErrorKind::NotConnected).into(),
            Phase::Command,
            &self.target,
            &self.codec(),
        )
    }

    fn codec(&self) -> Arc<Codec> {
        Arc::clone(&lock_codec(&self.codec))
    }

    fn custom(&self) -> Option<Arc<Custom>> {
        self.custom
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Reads connection state that needs no I/O.
    fn peek<T>(&self, f: impl FnOnce(&Link) -> T) -> PyResult<T> {
        match &self.lock().state {
            State::Idle(link) => Ok(f(link)),
            State::Running(_) => Err(busy_error()),
            State::Closed => Err(self.closed_error()),
        }
    }

    /// How statements are written for this session (its character set,
    /// SQL mode and `conv=` encoders).
    fn encoder(&self) -> PyResult<Encoder> {
        let no_backslash_escapes =
            self.peek(|link| link.client.status_flags() & status::NO_BACKSLASH_ESCAPES != 0)?;
        Ok(Encoder {
            no_backslash_escapes,
            binary_prefix: self.binary_prefix,
            codec: self.codec(),
            custom: self.custom(),
        })
    }

    /// Starts a command reading a result: `start` begins it, and the rows
    /// are read and converted as they come (see [`read_result`]).
    fn result_command<F>(&self, py: Python<'_>, as_dict: bool, start: F) -> PyResult<Py<PyAny>>
    where
        F: for<'a> FnOnce(&'a mut Client, &'a mut MaybeTls<LoopIo>) -> StartFuture<'a>
            + Send
            + 'static,
    {
        let codec = self.codec();
        let custom = self.custom();
        let target = Arc::clone(&self.target);
        self.start(py, move |mut link| {
            Box::pin(async move {
                let Link { client, io } = &mut *link;
                let value = match start(client, io).await {
                    Ok(Some((head, id))) => {
                        read_result(client, io, head, id, as_dict, &codec, custom.as_deref()).await
                    }
                    Ok(None) => Ok(none()),
                    Err(err) => Err(err.into()),
                };
                outcome(link, value, &codec, &target)
            })
        })
    }

    /// Starts a command answered with OK or ERR: its future gives `None`.
    fn simple_command<F>(&self, py: Python<'_>, breaks: bool, run: F) -> PyResult<Py<PyAny>>
    where
        F: for<'a> FnOnce(&'a mut Client, &'a mut MaybeTls<LoopIo>) -> UnitFuture<'a>
            + Send
            + 'static,
    {
        let codec = self.codec();
        let target = Arc::clone(&self.target);
        self.start(py, move |mut link| {
            Box::pin(async move {
                let Link { client, io } = &mut *link;
                let value = run(client, io).await.map(|()| none());
                match value {
                    // A failed `COM_CHANGE_USER` makes MySQL drop the
                    // connection while MariaDB keeps the old user: it is
                    // treated as gone either way.
                    Err(err) if breaks => Outcome {
                        link: None,
                        value: Err(to_pyerr(err, Phase::Command, &target, &codec)),
                    },
                    value => outcome(link, value.map_err(Failure::Proto), &codec, &target),
                }
            })
        })
    }
}

type StartFuture<'a> = Pin<
    Box<
        dyn Future<Output = rustmy_proto::Result<Option<(QueryResult, Option<StreamId>)>>>
            + Send
            + 'a,
    >,
>;
type UnitFuture<'a> = Pin<Box<dyn Future<Output = rustmy_proto::Result<()>> + Send + 'a>>;

/// Python's `None`, as a command's value.
fn none() -> Py<PyAny> {
    Python::attach(|py| PyNone::get(py).to_owned().into_any().unbind())
}

fn lock_codec(codec: &Mutex<Arc<Codec>>) -> MutexGuard<'_, Arc<Codec>> {
    codec.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Another command of this connection has not ended: one at a time, as the
/// blocking connection refuses a second thread.
fn busy_error() -> PyErr {
    ProgrammingError::new_err((
        0,
        "another command is running on this connection; await it first",
    ))
}

/// Resolves `waiter` with `value`, unless it was cancelled meanwhile.
fn resolve(py: Python<'_>, waiter: &Bound<'_, PyAny>, value: PyResult<Py<PyAny>>) -> PyResult<()> {
    if waiter.call_method0("done")?.is_truthy()? {
        return Ok(());
    }
    match value {
        Ok(value) => waiter.call_method1("set_result", (value,))?,
        Err(err) => waiter.call_method1("set_exception", (err.into_value(py),))?,
    };
    Ok(())
}

/// Why a command failed: the protocol's error (some leave the connection
/// unusable) or Python's.
enum Failure {
    Proto(ProtoError),
    Python(PyErr),
}

impl From<ProtoError> for Failure {
    fn from(err: ProtoError) -> Self {
        Self::Proto(err)
    }
}

impl From<PyErr> for Failure {
    fn from(err: PyErr) -> Self {
        Self::Python(err)
    }
}

/// The outcome of a command over `client`: an I/O failure or a protocol
/// violation leaves the stream in an unknown state, so the connection is
/// not used again.
fn outcome(
    link: Box<Link>,
    value: Result<Py<PyAny>, Failure>,
    codec: &Codec,
    target: &str,
) -> Outcome {
    match value {
        Ok(value) => Outcome {
            link: Some(link),
            value: Ok(value),
        },
        Err(Failure::Python(err)) => Outcome {
            link: Some(link),
            value: Err(err),
        },
        Err(Failure::Proto(err)) => {
            let broken = matches!(err, ProtoError::Io(_) | ProtoError::Protocol(_));
            Outcome {
                link: (!broken).then_some(link),
                value: Err(to_pyerr(err, Phase::Command, target, codec)),
            }
        }
    }
}

/// `(description, stream, rowcount, last_insert_id, warnings)` for a
/// streamed statement: the stream reads the rows of the result set `id`
/// (when there is one) as they are fetched.
#[expect(
    clippy::too_many_arguments,
    reason = "the parts of a stream, handed over once"
)]
fn stream_head(
    py: Python<'_>,
    conn: Py<AioConnection>,
    head: QueryResult,
    id: Option<StreamId>,
    as_dict: bool,
    codec: &Arc<Codec>,
    custom: Option<&Custom>,
    counters: &Arc<Counters>,
) -> PyResult<Py<PyAny>> {
    let Some(id) = id else {
        return (
            py.None(),
            py.None(),
            head.affected_rows,
            head.last_insert_id,
            head.warnings,
        )
            .into_py_any(py);
    };
    let converter = Converter::new(py, &head.columns, as_dict, codec, custom)?;
    let generation = counters.generation.load(Ordering::SeqCst);
    counters.open_stream.store(generation, Ordering::SeqCst);
    let stream = AioStream {
        conn,
        counters: Arc::clone(counters),
        state: Arc::new(Mutex::new(StreamState {
            id,
            generation,
            converter,
            raw: head.rows,
            batch: VecDeque::new(),
            done: false,
            error: None,
            abort: None,
            rownumber: 0,
        })),
    };
    (
        description(py, &head.columns, codec)?,
        Py::new(py, stream)?,
        -1_i64,
        head.last_insert_id,
        head.warnings,
    )
        .into_py_any(py)
}

/// Reads the rows of the result set `head` started (the result itself
/// without one) in batches, converting each as it comes:
/// `(description, rows, rowcount, last_insert_id, warnings)`, as
/// `Connection.execute` returns it.
async fn read_result(
    client: &mut Client,
    io: &mut MaybeTls<LoopIo>,
    mut head: QueryResult,
    id: Option<StreamId>,
    as_dict: bool,
    codec: &Arc<Codec>,
    custom: Option<&Custom>,
) -> Result<Py<PyAny>, Failure> {
    let Some(id) = id else {
        return Ok(Python::attach(|py| {
            result_to_py(py, &head, as_dict, codec)
        })?);
    };
    let converter = Python::attach(|py| Converter::new(py, &head.columns, as_dict, codec, custom))?;
    let mut rows: Vec<Py<PyAny>> = Vec::new();
    loop {
        let outcome = client
            .read_rows(io, id, &mut head.rows, BATCH_BYTES)
            .await?;
        let batch =
            Python::attach(|py| converter.convert(py, &head.rows, |row| rows.push(row.unbind())));
        head.rows.clear();
        if let Err(error) = batch {
            if outcome == ReadOutcome::More {
                // Keep the connection in sync before reporting.
                let _ = client.skip_rows(io, id, usize::MAX).await;
            }
            return Err(error.into());
        }
        if let ReadOutcome::Done { warnings } = outcome {
            return Ok(Python::attach(|py| {
                let count = rows.len();
                (
                    description(py, &head.columns, codec)?,
                    PyTuple::new(py, rows)?,
                    count,
                    head.last_insert_id,
                    warnings,
                )
                    .into_py_any(py)
            })?);
        }
    }
}

#[pymethods]
impl AioConnection {
    /// A connection about to be opened on `event_loop`, with the options of
    /// `connect()`: the handshake starts once the loop's transport is
    /// connected (`connection_made`), and the future `connected()` returns
    /// resolves when it ends.
    #[new]
    #[pyo3(signature = (
        event_loop,
        *,
        host,
        port,
        user,
        password,
        database = None,
        unix_socket = None,
        connect_timeout = None,
        read_timeout = None,
        write_timeout = None,
        client_flag = 0,
        charset = "utf8mb4",
        ssl_mode = "preferred",
        ssl_ca = None,
        ssl_cert = None,
        ssl_key = None,
        server_public_key = None,
        get_server_public_key = true,
        binary_prefix = true,
        statement_cache_size = 32,
        compress = None,
        zstd_level = 3,
        local_infile = false,
        local_infile_dir = None,
    ))]
    #[expect(
        clippy::too_many_arguments,
        reason = "mirrors the keyword-only Python signature one to one"
    )]
    fn new(
        py: Python<'_>,
        event_loop: Py<PyAny>,
        host: String,
        port: u16,
        user: &str,
        password: String,
        database: Option<&str>,
        unix_socket: Option<PathBuf>,
        connect_timeout: Option<f64>,
        read_timeout: Option<f64>,
        write_timeout: Option<f64>,
        client_flag: u32,
        charset: &str,
        ssl_mode: &str,
        ssl_ca: Option<PathBuf>,
        ssl_cert: Option<PathBuf>,
        ssl_key: Option<PathBuf>,
        server_public_key: Option<PathBuf>,
        get_server_public_key: bool,
        binary_prefix: bool,
        statement_cache_size: usize,
        compress: Option<&str>,
        zstd_level: u8,
        local_infile: bool,
        local_infile_dir: Option<PathBuf>,
    ) -> PyResult<Self> {
        let unix = unix_socket.is_some();
        let (opts, codec, target) = connect_opts(
            py,
            host,
            port,
            user,
            password,
            database,
            unix_socket,
            connect_timeout,
            read_timeout,
            write_timeout,
            client_flag,
            charset,
            ssl_mode,
            ssl_ca,
            ssl_cert,
            ssl_key,
            server_public_key,
            get_server_public_key,
            statement_cache_size,
            compress,
            zstd_level,
            local_infile,
            local_infile_dir,
        )?;
        let shared = Arc::new(Mutex::new(Shared::default()));
        let target: Arc<str> = Arc::from(target);
        let waiter = event_loop.bind(py).call_method0("create_future")?.unbind();
        let handshake: Operation = {
            let mut io = MaybeTls::new(LoopIo(Arc::clone(&shared)));
            let codec = Arc::clone(&codec);
            let target = Arc::clone(&target);
            let none = py.None();
            Box::pin(async move {
                match Client::connect(&mut io, &opts, unix).await {
                    Ok(client) => Outcome {
                        link: Some(Box::new(Link { client, io })),
                        value: Ok(none),
                    },
                    Err(err) => Outcome {
                        link: None,
                        value: Err(to_pyerr(err, Phase::Connect, &target, &codec)),
                    },
                }
            })
        };
        Ok(Self {
            inner: Mutex::new(Inner {
                state: State::Running(handshake),
                waiter: Some(waiter),
                transport: None,
            }),
            shared,
            buffer: PyByteArray::new_with(py, LOOP_BUFFER, |_| Ok(()))?.unbind(),
            event_loop,
            codec: Arc::new(Mutex::new(codec)),
            custom: Mutex::new(None),
            target,
            binary_prefix,
            counters: Arc::new(Counters::default()),
        })
    }

    /// The future of the handshake.
    fn connected(&self, py: Python<'_>) -> Option<Py<PyAny>> {
        self.lock().waiter.as_ref().map(|w| w.clone_ref(py))
    }

    fn connection_made(&self, py: Python<'_>, transport: Py<PyAny>) -> PyResult<()> {
        let mut inner = self.lock();
        inner.transport = Some(transport);
        self.advance(py, &mut inner)
    }

    fn get_buffer(&self, py: Python<'_>, _sizehint: isize) -> Py<PyByteArray> {
        self.buffer.clone_ref(py)
    }

    fn buffer_updated(&self, py: Python<'_>, nbytes: usize) -> PyResult<()> {
        let mut inner = self.lock();
        {
            let buffer = self.buffer.bind(py);
            // SAFETY: the bytearray is private to this object: only the
            // event loop writes into it (between `get_buffer` and this
            // call, on this thread), and nothing resizes it, so the slice
            // stays valid while it is copied.
            let received = unsafe { buffer.as_bytes() };
            let received = received.get(..nbytes).unwrap_or(received);
            let mut shared = lock(&self.shared);
            shared.received.extend_from_slice(received);
        }
        self.advance(py, &mut inner)
    }

    #[expect(clippy::unused_self, reason = "asyncio calls it on the protocol")]
    fn eof_received(&self) -> bool {
        false
    }

    fn connection_lost(&self, py: Python<'_>, _exc: Option<Py<PyAny>>) -> PyResult<()> {
        let mut inner = self.lock();
        inner.transport = None;
        lock(&self.shared).closed = true;
        self.advance(py, &mut inner)?;
        if matches!(inner.state, State::Idle(_)) {
            inner.state = State::Closed;
        }
        Ok(())
    }

    /// Runs `sql` as it is; the future gives what `execute` gives.
    fn query(&self, py: Python<'_>, sql: &Bound<'_, PyString>) -> PyResult<Py<PyAny>> {
        let sql = statement(&self.encoder()?, sql, None)?.into_owned();
        self.result_command(py, false, move |client, io| {
            Box::pin(async move { client.query_streamed(io, &sql).await.map(Some) })
        })
    }

    /// Starts `query` (with `args` interpolated) and returns the future of
    /// its result, as `Connection.execute` returns it.
    #[pyo3(signature = (query, args = None, as_dict = false))]
    fn execute(
        &self,
        py: Python<'_>,
        query: &Bound<'_, PyString>,
        args: Option<&Bound<'_, PyAny>>,
        as_dict: bool,
    ) -> PyResult<Py<PyAny>> {
        let sql = statement(&self.encoder()?, query, args)?.into_owned();
        self.result_command(py, as_dict, move |client, io| {
            Box::pin(async move { client.query_streamed(io, &sql).await.map(Some) })
        })
    }

    /// As `execute`, as a prepared statement (as `Connection.execute_prepared`).
    #[pyo3(signature = (query, args = None, as_dict = false))]
    fn execute_prepared(
        &self,
        py: Python<'_>,
        query: &Bound<'_, PyString>,
        args: Option<&Bound<'_, PyAny>>,
        as_dict: bool,
    ) -> PyResult<Py<PyAny>> {
        let plan = prepared_plan(py, &self.encoder()?, query, args)?;
        self.result_command(py, as_dict, move |client, io| {
            Box::pin(async move {
                match &plan {
                    Plan::Text(sql) => client.query_streamed(io, sql).await,
                    Plan::Prepared(sql, params) => {
                        client.execute_cached_streamed(io, sql, params).await
                    }
                }
                .map(Some)
            })
        })
    }

    /// As `execute`, leaving the rows on the wire for the stream the
    /// future gives (as `Connection.execute_streamed`).
    #[pyo3(signature = (query, args = None, as_dict = false))]
    fn execute_streamed(
        slf: &Bound<'_, Self>,
        query: &Bound<'_, PyString>,
        args: Option<&Bound<'_, PyAny>>,
        as_dict: bool,
    ) -> PyResult<Py<PyAny>> {
        let sql = statement(&slf.get().encoder()?, query, args)?.into_owned();
        Self::stream_command(slf, as_dict, move |client, io| {
            Box::pin(async move { client.query_streamed(io, &sql).await.map(Some) })
        })
    }

    /// As `next_result`, leaving the rows on the wire (the future gives
    /// what `execute_streamed` gives, or `None`).
    #[pyo3(signature = (as_dict = false))]
    fn next_result_streamed(slf: &Bound<'_, Self>, as_dict: bool) -> PyResult<Py<PyAny>> {
        Self::stream_command(slf, as_dict, |client, io| {
            Box::pin(async move { client.next_result_streamed(io).await })
        })
    }

    /// The next result of a multi-result query: the future gives what
    /// `execute` gives, or `None` when there is none.
    #[pyo3(signature = (as_dict = false))]
    fn next_result(&self, py: Python<'_>, as_dict: bool) -> PyResult<Py<PyAny>> {
        self.result_command(py, as_dict, |client, io| {
            Box::pin(async move { client.next_result_streamed(io).await })
        })
    }

    fn ping(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        self.simple_command(py, false, |client, io| Box::pin(client.ping(io)))
    }

    fn select_db(&self, py: Python<'_>, database: &str) -> PyResult<Py<PyAny>> {
        let database = self.codec().encode(py, database)?;
        self.simple_command(py, false, move |client, io| {
            Box::pin(async move { client.select_db(io, &database).await })
        })
    }

    fn reset_connection(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        self.simple_command(py, false, |client, io| {
            Box::pin(client.reset_connection(io))
        })
    }

    #[pyo3(signature = (user, password, database = None))]
    fn change_user(
        &self,
        py: Python<'_>,
        user: &str,
        password: String,
        database: Option<&str>,
    ) -> PyResult<Py<PyAny>> {
        // The packet names the handshake's collation, and the server reads
        // the user and database in its character set (which may not be the
        // session's after set_character_set()).
        let codec = match self.peek(|link| charset_of(link.client.collation()))? {
            Some(name) if name != self.codec().name() => Codec::new(py, name)?,
            _ => self.codec(),
        };
        let user = codec.encode(py, user)?;
        let database = database.map(|db| codec.encode(py, db)).transpose()?;
        self.simple_command(py, true, move |client, io| {
            Box::pin(async move {
                client
                    .change_user(io, &user, &password, database.as_deref())
                    .await
            })
        })
    }

    /// Switches the session to `charset` (`SET NAMES`, with `COLLATE` when
    /// `collation` is given) and encodes and decodes with it once that ran.
    #[pyo3(signature = (charset, collation = None))]
    fn set_character_set(
        &self,
        py: Python<'_>,
        charset: &str,
        collation: Option<&str>,
    ) -> PyResult<Py<PyAny>> {
        let new_codec = Codec::new(py, charset)?;
        let sql = format!("SET {}", names_clause(new_codec.name(), collation)?);
        let session_codec = Arc::clone(&self.codec);
        self.simple_command(py, false, move |client, io| {
            Box::pin(async move {
                client.query(io, sql.as_bytes()).await?;
                *lock_codec(&session_codec) = new_codec;
                Ok(())
            })
        })
    }

    /// The query exactly as `execute` would send it, as `str`.
    #[pyo3(signature = (query, args = None))]
    fn mogrify<'py>(
        &self,
        query: &Bound<'py, PyString>,
        args: Option<&Bound<'py, PyAny>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        if args.is_none_or(Bound::is_none) {
            return Ok(query.clone().into_any());
        }
        let encoder = self.encoder()?;
        let out = statement(&encoder, query, args)?;
        encoder.codec.decode_statement(query.py(), &out)
    }

    /// `value` as an SQL literal for this connection, as `str`.
    fn literal<'py>(&self, value: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
        let encoder = self.encoder()?;
        let mut out = Vec::new();
        encoder.literal(&mut out, value)?;
        encoder.codec.decode_statement(value.py(), &out)
    }

    /// A statement this connection built, as `str`.
    fn decode_statement<'py>(
        &self,
        py: Python<'py>,
        statement: &[u8],
    ) -> PyResult<Bound<'py, PyAny>> {
        self.codec().decode_statement(py, statement)
    }

    /// Installs the `conv=` decoders and encoders that differ from the
    /// defaults, as `Connection.set_converters` does.
    #[pyo3(signature = (decoders, encoders, conv))]
    fn set_converters(
        &self,
        decoders: Option<Bound<'_, PyDict>>,
        encoders: Option<Bound<'_, PyDict>>,
        conv: Bound<'_, PyAny>,
    ) {
        let custom = (decoders.is_some() || encoders.is_some()).then(|| {
            Arc::new(Custom {
                decoders: decoders.map(Bound::unbind),
                encoders: encoders.map(Bound::unbind),
                conv: conv.unbind(),
            })
        });
        *self.custom.lock().unwrap_or_else(PoisonError::into_inner) = custom;
    }

    /// The session's character set, as MySQL names it.
    #[getter]
    fn charset(&self) -> &'static str {
        self.codec().name()
    }

    /// The server's default character set (from its greeting), if known.
    #[getter]
    fn server_charset(&self) -> PyResult<Option<&'static str>> {
        self.peek(|link| link.client.server_charset())
    }

    /// Python codec name of the session's character set.
    #[getter]
    fn encoding(&self) -> &'static str {
        self.codec().python_name()
    }

    #[getter]
    fn tls(&self) -> PyResult<bool> {
        self.peek(|link| link.io.is_tls())
    }

    #[getter]
    fn compression(&self) -> PyResult<Option<&'static str>> {
        self.peek(|link| {
            link.client.compression().map(|algorithm| match algorithm {
                Compression::Zlib => "zlib",
                Compression::Zstd { .. } => "zstd",
            })
        })
    }

    #[getter]
    fn closed(&self) -> bool {
        matches!(self.lock().state, State::Closed)
    }

    #[getter]
    fn server_version(&self) -> PyResult<String> {
        self.peek(|link| link.client.server_version().to_owned())
    }

    #[getter]
    fn connection_id(&self) -> PyResult<u32> {
        self.peek(|link| link.client.connection_id())
    }

    #[getter]
    fn server_status(&self) -> PyResult<u16> {
        self.peek(|link| link.client.status_flags())
    }

    #[getter]
    fn has_more_results(&self) -> PyResult<bool> {
        self.peek(|link| link.client.has_more_results())
    }

    /// Closes the stream, sending `COM_QUIT` first when no command is
    /// running; a running one is dropped (its future is left as it is: the
    /// caller cancelled it, or will see the connection closed).
    fn close(&self, py: Python<'_>) -> PyResult<()> {
        let mut inner = self.lock();
        inner.waiter = None;
        if let State::Idle(mut link) = std::mem::replace(&mut inner.state, State::Closed) {
            let Link { client, io } = &mut *link;
            {
                let mut quit = std::pin::pin!(client.quit(io));
                let _ = quit.as_mut().poll(&mut Context::from_waker(Waker::noop()));
            }
            io.shutdown();
        }
        lock(&self.shared).shutdown = true;
        self.flush(py, &inner)?;
        inner.transport = None;
        Ok(())
    }
}

/// Why a stream stopped handing out rows before the end of its result set.
#[derive(Debug, Clone, Copy)]
enum Abort {
    /// Another command discarded the rows still on the wire.
    Discarded,
    /// A row could not be converted; the rest of the result set is dropped.
    Conversion,
}

fn abort_error(abort: Abort) -> PyErr {
    match abort {
        Abort::Discarded => ProgrammingError::new_err((
            0,
            "result set discarded: another command ran on this connection before \
             all its rows were fetched",
        )),
        Abort::Conversion => {
            InterfaceError::new_err((0, "result set aborted by an earlier conversion error"))
        }
    }
}

struct StreamState {
    id: StreamId,
    /// Connection generation the result set belongs to.
    generation: u64,
    converter: Converter,
    /// Raw rows of the batch being converted (kept for its allocations).
    raw: RowSet,
    /// Converted rows not handed out yet.
    batch: VecDeque<Py<PyAny>>,
    /// The terminator (or an error) has been read: no rows left on the wire.
    done: bool,
    /// Error that ended the result set, raised once the rows read before it
    /// have been handed out.
    error: Option<PyErr>,
    abort: Option<Abort>,
    /// Rows handed out or skipped so far.
    rownumber: usize,
}

impl StreamState {
    fn exhausted(&self) -> bool {
        self.done && self.batch.is_empty() && self.error.is_none()
    }

    fn abort(&mut self, counters: &Counters, abort: Abort) {
        self.abort = Some(abort);
        self.batch.clear();
        self.error = None;
        self.done = true;
        counters.stream_finished(self.generation);
    }

    /// Fails if the rows were discarded by another command (checked before
    /// every fetch, even with rows at hand, so the outcome never depends on
    /// the batch size) or the stream was aborted.
    fn check(&mut self, counters: &Counters) -> PyResult<()> {
        if let Some(abort) = self.abort {
            return Err(abort_error(abort));
        }
        if self.exhausted() || counters.generation.load(Ordering::SeqCst) == self.generation {
            return Ok(());
        }
        self.abort(counters, Abort::Discarded);
        Err(abort_error(Abort::Discarded))
    }

    fn finish_if_exhausted(&self, counters: &Counters) {
        if self.exhausted() {
            counters.stream_finished(self.generation);
        }
    }

    /// Handles a batch just read (or the error reading it).
    fn read(
        &mut self,
        py: Python<'_>,
        counters: &Counters,
        read: Result<ReadOutcome, PyErr>,
        discarded: bool,
    ) -> PyResult<()> {
        if discarded {
            self.raw.clear();
            self.abort(counters, Abort::Discarded);
            return Err(abort_error(Abort::Discarded));
        }
        match read {
            Ok(ReadOutcome::More) => {}
            Ok(ReadOutcome::Done { .. }) => self.done = true,
            // A server error (or a lost connection) ends the result set;
            // the rows read before it are still handed out first.
            Err(err) => {
                self.done = true;
                self.error = Some(err);
            }
        }
        let batch = &mut self.batch;
        let converted = self
            .converter
            .convert(py, &self.raw, |row| batch.push_back(row.unbind()));
        self.raw.clear();
        if let Err(err) = converted {
            self.abort(counters, Abort::Conversion);
            return Err(err);
        }
        Ok(())
    }
}

/// The rows of a streamed result set (`execute_streamed`), read from the
/// event loop one batch at a time, only when the user asks for more than
/// the rows at hand (the server waits meanwhile: memory stays flat).
///
/// Fetching is two steps, so no future is created while rows are at hand:
/// `take` hands out rows without I/O (or says a read is needed), and `read`
/// starts reading the next batch, its future giving `None`.
#[pyclass(module = "rustmy._core", frozen)]
pub(crate) struct AioStream {
    conn: Py<AioConnection>,
    counters: Arc<Counters>,
    state: Arc<Mutex<StreamState>>,
}

impl AioStream {
    fn lock(&self) -> MutexGuard<'_, StreamState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Starts reading (and dropping) up to `count` rows on the wire; the
    /// future gives how many.
    fn skip_on_wire(&self, py: Python<'_>, count: usize) -> PyResult<Py<PyAny>> {
        let (id, generation) = {
            let state = self.lock();
            (state.id, state.generation)
        };
        let conn = self.conn.get();
        let codec = conn.codec();
        let target = Arc::clone(&conn.target);
        let counters = Arc::clone(&self.counters);
        let state = Arc::clone(&self.state);
        conn.start_as(py, Some(generation), move |mut link| {
            Box::pin(async move {
                let Link { client, io } = &mut *link;
                let skipped = client.skip_rows(io, id, count).await;
                let mut stream = state.lock().unwrap_or_else(PoisonError::into_inner);
                let value = match skipped {
                    Ok((n, outcome)) => {
                        stream.done = matches!(outcome, ReadOutcome::Done { .. });
                        stream.rownumber += n;
                        stream.finish_if_exhausted(&counters);
                        Python::attach(|py| n.into_py_any(py)).map_err(Failure::Python)
                    }
                    Err(ProtoError::ResultDiscarded) => {
                        stream.abort(&counters, Abort::Discarded);
                        Err(Failure::Python(abort_error(Abort::Discarded)))
                    }
                    Err(err) => {
                        stream.done = true;
                        counters.stream_finished(stream.generation);
                        Err(Failure::Proto(err))
                    }
                };
                outcome(link, value, &codec, &target)
            })
        })
    }
}

#[pymethods]
impl AioStream {
    /// Up to `size` rows at hand, without I/O: `None` while fewer are at
    /// hand and more may come (call `read` first). An error that ended the
    /// result set is raised once no rows are left to hand out.
    fn take<'py>(&self, py: Python<'py>, size: usize) -> PyResult<Option<Bound<'py, PyTuple>>> {
        let mut state = self.lock();
        state.check(&self.counters)?;
        if state.batch.len() < size && !state.done {
            return Ok(None);
        }
        let n = size.min(state.batch.len());
        if n == 0
            && let Some(err) = state.error.take()
        {
            self.counters.stream_finished(state.generation);
            return Err(err);
        }
        let rows: Vec<Py<PyAny>> = state.batch.drain(..n).collect();
        state.rownumber += n;
        state.finish_if_exhausted(&self.counters);
        Ok(Some(PyTuple::new(py, rows)?))
    }

    /// Starts reading the next batch of rows; the future gives `None`.
    fn read(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let (id, generation) = {
            let state = self.lock();
            (state.id, state.generation)
        };
        let conn = self.conn.get();
        let codec = conn.codec();
        let target = Arc::clone(&conn.target);
        let counters = Arc::clone(&self.counters);
        let state = Arc::clone(&self.state);
        conn.start_as(py, Some(generation), move |mut link| {
            Box::pin(async move {
                let Link { client, io } = &mut *link;
                // The batch is read outside the lock (no guard across an
                // await), then handed back.
                let mut raw =
                    std::mem::take(&mut state.lock().unwrap_or_else(PoisonError::into_inner).raw);
                let read = client.read_rows(io, id, &mut raw, BATCH_BYTES).await;
                let mut stream = state.lock().unwrap_or_else(PoisonError::into_inner);
                stream.raw = raw;
                let discarded = matches!(read, Err(ProtoError::ResultDiscarded));
                // An I/O failure or protocol violation also leaves the
                // connection unusable, as for any command.
                let broken = matches!(read, Err(ProtoError::Io(_) | ProtoError::Protocol(_)));
                let read = read.map_err(|err| to_pyerr(err, Phase::Command, &target, &codec));
                let value = Python::attach(|py| {
                    stream
                        .read(py, &counters, read, discarded)
                        .map(|()| py.None())
                });
                drop(stream);
                Outcome {
                    link: (!broken).then_some(link),
                    value,
                }
            })
        })
    }

    /// Hands out up to `count` rows at hand as skipped; the rest must be
    /// read off the wire (`skip_wire`). Returns how many were skipped.
    fn skip_ready(&self, count: usize) -> PyResult<usize> {
        let mut state = self.lock();
        state.check(&self.counters)?;
        let n = count.min(state.batch.len());
        state.batch.drain(..n);
        state.rownumber += n;
        state.finish_if_exhausted(&self.counters);
        Ok(n)
    }

    /// Starts dropping up to `count` rows still on the wire, unconverted;
    /// the future gives how many.
    fn skip_wire(&self, py: Python<'_>, count: usize) -> PyResult<Py<PyAny>> {
        self.skip_on_wire(py, count)
    }

    /// Whether rows may still come off the wire.
    #[getter]
    fn pending(&self) -> bool {
        let state = self.lock();
        !state.done && state.abort.is_none()
    }

    /// Drops the rest of the result set: the future (or `None`, when no
    /// rows are left on the wire) ends once they are read.
    fn close(&self, py: Python<'_>) -> PyResult<Option<Py<PyAny>>> {
        let (done, generation) = {
            let mut state = self.lock();
            state.batch.clear();
            state.error = None;
            (state.done || state.abort.is_some(), state.generation)
        };
        if done || self.counters.generation.load(Ordering::SeqCst) != generation {
            self.counters.stream_finished(generation);
            return Ok(None);
        }
        self.skip_on_wire(py, usize::MAX).map(Some)
    }

    /// Index of the next row: rows handed out or skipped so far.
    #[getter]
    fn rownumber(&self) -> usize {
        self.lock().rownumber
    }
}
