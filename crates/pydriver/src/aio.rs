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

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::MutexGuard;
use std::sync::PoisonError;
use std::task::Context;
use std::task::Poll;
use std::task::Waker;

use pyo3::IntoPyObjectExt;
use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use pyo3::types::PyByteArray;
use pyo3::types::PyBytes;
use pyo3::types::PyString;
use pyo3::types::PyTuple;
use rustmy_proto::Client;
use rustmy_proto::Error as ProtoError;
use rustmy_proto::Input;
use rustmy_proto::Opts;
use rustmy_proto::ReadOutcome;
use rustmy_proto::SslMode;
use rustmy_proto::TlsOpts;
use rustmy_proto::Transport;
use rustmy_proto::constants::status;

use crate::charset::Codec;
use crate::connection::utf8_encoder;
use crate::convert::Converter;
use crate::convert::description;
use crate::convert::result_to_py;
use crate::errors::Phase;
use crate::errors::from_proto;
use crate::params::Args;

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

/// What an operation ends with: the connection to go on with (none once it
/// is unusable) and the value for the command's future.
struct Outcome {
    client: Option<Box<Client>>,
    value: PyResult<Py<PyAny>>,
}

type Operation = Pin<Box<dyn Future<Output = Outcome> + Send>>;

enum State {
    /// Waiting for a command.
    Idle(Box<Client>),
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
    codec: Arc<Codec>,
    target: Arc<str>,
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
            inner.state = outcome.client.map_or(State::Closed, State::Idle);
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
        operation: impl FnOnce(Box<Client>, LoopIo) -> Operation,
    ) -> PyResult<Py<PyAny>> {
        let mut inner = self.lock();
        let client = match std::mem::replace(&mut inner.state, State::Closed) {
            State::Idle(client) => client,
            State::Running(running) => {
                inner.state = State::Running(running);
                return Err(PyRuntimeError::new_err(
                    "another command is running on this connection",
                ));
            }
            State::Closed => return Err(self.closed_error()),
        };
        let waiter = self
            .event_loop
            .bind(py)
            .call_method0("create_future")?
            .unbind();
        inner.waiter = Some(waiter.clone_ref(py));
        inner.state = State::Running(operation(client, LoopIo(Arc::clone(&self.shared))));
        self.advance(py, &mut inner)?;
        Ok(waiter)
    }

    fn closed_error(&self) -> PyErr {
        to_pyerr(
            io::Error::from(io::ErrorKind::NotConnected).into(),
            Phase::Command,
            &self.target,
            &self.codec,
        )
    }
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
    client: Box<Client>,
    value: Result<Py<PyAny>, Failure>,
    codec: &Codec,
    target: &str,
) -> Outcome {
    match value {
        Ok(value) => Outcome {
            client: Some(client),
            value: Ok(value),
        },
        Err(Failure::Python(err)) => Outcome {
            client: Some(client),
            value: Err(err),
        },
        Err(Failure::Proto(err)) => {
            let broken = matches!(err, ProtoError::Io(_) | ProtoError::Protocol(_));
            Outcome {
                client: (!broken).then_some(client),
                value: Err(to_pyerr(err, Phase::Command, target, codec)),
            }
        }
    }
}

/// Runs `sql` and reads its rows in batches, converting each as it comes:
/// `(description, rows, rowcount, last_insert_id, warnings)`, as
/// `Connection.execute` returns it.
async fn execute(
    client: &mut Client,
    io: &mut LoopIo,
    sql: &[u8],
    as_dict: bool,
    codec: &Arc<Codec>,
) -> Result<Py<PyAny>, Failure> {
    let (mut head, id) = client.query_streamed(io, sql).await?;
    let Some(id) = id else {
        return Ok(Python::attach(|py| {
            result_to_py(py, &head, as_dict, codec)
        })?);
    };
    let converter = Python::attach(|py| Converter::new(py, &head.columns, as_dict, codec, None))?;
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
    /// A connection about to be opened on `event_loop`: the handshake starts
    /// once the loop's transport is connected (`connection_made`), and the
    /// future `connected()` returns resolves when it ends. `unix` says
    /// whether the stream is a Unix socket.
    #[new]
    #[pyo3(signature = (event_loop, target, user, password, database = None, unix = false))]
    fn new(
        py: Python<'_>,
        event_loop: Py<PyAny>,
        target: &str,
        user: &str,
        password: &str,
        database: Option<&str>,
        unix: bool,
    ) -> PyResult<Self> {
        let codec = Codec::new(py, "utf8mb4")?;
        let opts = Opts {
            user: user.as_bytes().to_vec(),
            password: password.to_owned(),
            database: database.map(|d| d.as_bytes().to_vec()),
            tls: TlsOpts {
                mode: SslMode::Disabled,
                ..TlsOpts::default()
            },
            ..Opts::default()
        };
        let shared = Arc::new(Mutex::new(Shared::default()));
        let target: Arc<str> = Arc::from(target);
        let waiter = event_loop.bind(py).call_method0("create_future")?.unbind();
        let handshake: Operation = {
            let mut io = LoopIo(Arc::clone(&shared));
            let codec = Arc::clone(&codec);
            let target = Arc::clone(&target);
            let none = py.None();
            Box::pin(async move {
                match Client::connect(&mut io, &opts, unix).await {
                    Ok(client) => Outcome {
                        client: Some(Box::new(client)),
                        value: Ok(none),
                    },
                    Err(err) => Outcome {
                        client: None,
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
            codec,
            target,
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

    /// Starts `query` and returns the future of its result, as
    /// `Connection.execute` returns it.
    #[pyo3(signature = (query, args = None, as_dict = false))]
    fn execute(
        &self,
        py: Python<'_>,
        query: &Bound<'_, PyString>,
        args: Option<&Bound<'_, PyAny>>,
        as_dict: bool,
    ) -> PyResult<Py<PyAny>> {
        let no_backslash_escapes = match &self.lock().state {
            State::Idle(client) => client.status_flags() & status::NO_BACKSLASH_ESCAPES != 0,
            _ => false,
        };
        let template = query.to_str()?;
        let sql = match args.filter(|a| !a.is_none()) {
            None => template.as_bytes().to_vec(),
            Some(args) => {
                let mut out = Vec::with_capacity(template.len() + 64);
                utf8_encoder(no_backslash_escapes).interpolate(
                    py,
                    &mut out,
                    template,
                    &Args::from_py(args),
                )?;
                out
            }
        };
        let codec = Arc::clone(&self.codec);
        let target = Arc::clone(&self.target);
        self.start(py, move |mut client, mut io| {
            Box::pin(async move {
                let value = execute(&mut client, &mut io, &sql, as_dict, &codec).await;
                outcome(client, value, &codec, &target)
            })
        })
    }

    /// Closes the stream, sending `COM_QUIT` first when no command is
    /// running; a running one is dropped (its future is left as it is: the
    /// caller cancelled it, or will see the connection closed).
    fn close(&self, py: Python<'_>) -> PyResult<()> {
        let mut inner = self.lock();
        inner.waiter = None;
        if let State::Idle(mut client) = std::mem::replace(&mut inner.state, State::Closed) {
            let mut io = LoopIo(Arc::clone(&self.shared));
            let mut quit = std::pin::pin!(client.quit(&mut io));
            let _ = quit.as_mut().poll(&mut Context::from_waker(Waker::noop()));
        }
        lock(&self.shared).shutdown = true;
        self.flush(py, &inner)?;
        inner.transport = None;
        Ok(())
    }
}
