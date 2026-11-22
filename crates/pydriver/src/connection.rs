//! `rustmy._core.Connection`: a thread-aware wrapper around the protocol
//! connection that releases the GIL for all network I/O.

use std::borrow::Cow;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::PoisonError;
use std::sync::TryLockError;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::time::Duration;

use pyo3::IntoPyObjectExt;
use pyo3::exceptions::PyUserWarning;
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::PyBytes;
use pyo3::types::PyDict;
use pyo3::types::PyString;
use pyo3::types::PyTuple;
use rustmy_proto::Compression;
use rustmy_proto::Conn;
use rustmy_proto::Error as ProtoError;
use rustmy_proto::Execute;
use rustmy_proto::Opts;
use rustmy_proto::QueryResult;
use rustmy_proto::ReadOutcome;
use rustmy_proto::SslMode;
use rustmy_proto::StreamId;
use rustmy_proto::TlsOpts;
use rustmy_proto::charset::charset_of;
use rustmy_proto::constants::status;
use rustmy_proto::sql::insert_values;
use rustmy_proto::sql::mentions_local_infile;

use crate::charset::Codec;
use crate::charset::UTF8;
use crate::convert::Converter;
use crate::convert::Custom;
use crate::convert::description;
use crate::convert::result_to_py;
use crate::errors::InterfaceError;
use crate::errors::OperationalError;
use crate::errors::Phase;
use crate::errors::ProgrammingError;
use crate::errors::client_code;
use crate::errors::from_proto;
use crate::params::Args;
use crate::params::Encoder;
use crate::params::has_placeholder;
use crate::params::unescape_percent;
use crate::stream::ResultStream;

#[derive(Debug)]
enum State {
    // Boxed: a connection is much larger than the other states.
    Open(Box<Conn>),
    /// An I/O or protocol error left the stream in an unknown state.
    Broken,
    Closed,
}

/// Why a command could not run, carried out of the GIL-released section and
/// turned into a Python exception once the GIL is held again.
#[derive(Debug)]
pub(crate) enum Failure {
    Busy,
    Closed,
    Broken,
    /// A streamed result set's rows were discarded by another command.
    Discarded,
    Proto(ProtoError),
}

#[pyclass(frozen, module = "rustmy._core")]
#[derive(Debug)]
pub(crate) struct Connection {
    state: Mutex<State>,
    /// `host:port` or socket path, for error messages.
    target: String,
    /// Bumped by every command, so a streamed result set can tell whether
    /// another command ran (and discarded its rows) since it started.
    generation: AtomicU64,
    /// Generation of the streamed result set that still has rows the user
    /// has not fetched, or 0.
    open_stream: AtomicU64,
    /// The session's character set; `set_character_set` replaces it.
    codec: Mutex<Arc<Codec>>,
    /// `conv=` decoders and encoders that differ from the defaults.
    custom: Mutex<Option<Arc<Custom>>>,
    /// Whether `bytes` parameters carry the `_binary` introducer.
    binary_prefix: bool,
}

/// `(affected_rows, last_insert_id, last_statement)` of a batched
/// `executemany`.
type BatchTotals<'py> = (u64, Option<u64>, Option<Bound<'py, PyBytes>>);

/// `(result, rowcount, last_statement)` of a pipelined `executemany`: the
/// last statement's result as `execute` (or `execute_streamed`) returns it,
/// the rowcounts of the statements before it added up, and the last
/// statement as sent.
type PipelinedMany<'py> = (Py<PyAny>, u64, Bound<'py, PyBytes>);

/// How a `PreparedCursor` statement runs: prepared, or as text when it has
/// no placeholders (DDL and introspection, some of which the server cannot
/// prepare or answers differently when prepared).
#[derive(Debug)]
enum Plan {
    Text(Vec<u8>),
    Prepared(Vec<u8>, Execute),
}

impl Plan {
    fn run(&self, conn: &mut Conn) -> rustmy_proto::Result<(QueryResult, Option<StreamId>)> {
        match self {
            Self::Text(sql) => conn.query_streamed(sql),
            Self::Prepared(sql, params) => conn.execute_cached_streamed(sql, params),
        }
    }
}

/// Statements of one pipelined `executemany` window, back to back.
#[derive(Debug, Default)]
struct Window {
    bytes: Vec<u8>,
    ends: Vec<usize>,
}

impl Window {
    const fn len(&self) -> usize {
        self.ends.len()
    }

    fn push(&mut self, statement: &[u8]) {
        self.bytes.extend_from_slice(statement);
        self.ends.push(self.bytes.len());
    }

    fn clear(&mut self) {
        self.bytes.clear();
        self.ends.clear();
    }

    fn statements(&self) -> impl Iterator<Item = &[u8]> + Send {
        let starts = std::iter::once(0).chain(self.ends.iter().copied());
        starts
            .zip(self.ends.iter().copied())
            .map(|(start, end)| self.bytes.get(start..end).unwrap_or_default())
    }
}

const ABANDONED_STREAM_WARNING: &std::ffi::CStr =
    c"previous unbuffered result was left incomplete; its remaining rows were discarded";

/// Response of a statement as it starts: the head (OK packet or column
/// definitions) and, for a result set, the id its rows are read with.
type Head = Option<(QueryResult, Option<StreamId>)>;

/// Errors after which the stream is in an unknown state.
fn breaks_on_io(err: &ProtoError) -> bool {
    matches!(err, ProtoError::Io(_) | ProtoError::Protocol(_))
}

/// Exclusive use of the open connection for one command, which may release
/// and re-acquire the GIL several times (to read a result in batches).
struct Session<'a> {
    conn: &'a mut Conn,
    breaks: fn(&ProtoError) -> bool,
    broken: bool,
}

impl Session<'_> {
    /// Runs `f` with the GIL released.
    fn io<T, F>(&mut self, py: Python<'_>, f: F) -> Result<T, Failure>
    where
        T: Send,
        F: FnOnce(&mut Conn) -> rustmy_proto::Result<T> + Send,
    {
        let conn = &mut *self.conn;
        py.detach(|| f(conn)).map_err(|err| {
            if (self.breaks)(&err) {
                self.broken = true;
            }
            Failure::Proto(err)
        })
    }
}

impl Connection {
    /// Runs `f` on the open connection with the GIL released.
    ///
    /// Concurrent use from another thread fails fast with
    /// `ProgrammingError` instead of interleaving packets on the socket.
    fn with_conn<T, F>(&self, py: Python<'_>, f: F) -> PyResult<T>
    where
        T: Send,
        F: FnOnce(&mut Conn) -> rustmy_proto::Result<T> + Send,
    {
        self.run(py, f, breaks_on_io)
    }

    /// Like [`Self::with_conn`], with `breaks` deciding which errors leave
    /// the connection unusable.
    fn run<T, F>(&self, py: Python<'_>, f: F, breaks: fn(&ProtoError) -> bool) -> PyResult<T>
    where
        T: Send,
        F: FnOnce(&mut Conn) -> rustmy_proto::Result<T> + Send,
    {
        self.locked(py, breaks, |session| {
            session
                .io(py, f)
                .map_err(|failure| self.failure_to_pyerr(failure))
        })
    }

    /// Runs one command with the connection locked throughout, even while
    /// `f` holds the GIL between I/O calls: another thread using the
    /// connection meanwhile fails fast with `ProgrammingError` (the lock is
    /// only ever tried, never waited for, so holding it with the GIL cannot
    /// deadlock). Errors for which `breaks` holds leave the connection
    /// unusable.
    fn locked<T>(
        &self,
        py: Python<'_>,
        breaks: fn(&ProtoError) -> bool,
        f: impl FnOnce(&mut Session<'_>) -> PyResult<T>,
    ) -> PyResult<T> {
        // Warn before running, so that warnings turned into errors leave the
        // streamed result set untouched.
        if self.open_stream.load(Ordering::SeqCst) != 0 {
            warn_abandoned_stream(py)?;
        }
        let mut state = match self.state.try_lock() {
            Ok(guard) => guard,
            Err(TryLockError::WouldBlock) => return Err(self.failure_to_pyerr(Failure::Busy)),
            // A panic cannot happen inside the protocol code (it is
            // panic-free by lint policy); recover the state regardless.
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
        };
        let conn = match &mut *state {
            State::Open(conn) => conn,
            State::Broken => return Err(self.failure_to_pyerr(Failure::Broken)),
            State::Closed => return Err(self.failure_to_pyerr(Failure::Closed)),
        };
        // Any pending streamed rows are about to be discarded.
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.open_stream.store(0, Ordering::SeqCst);
        let mut session = Session {
            conn,
            breaks,
            broken: false,
        };
        let result = f(&mut session);
        if session.broken {
            *state = State::Broken;
        }
        result
    }

    /// Runs a statement and reads its whole result in batches of about
    /// `batch_bytes`: each batch is read with the GIL released and converted
    /// while the server keeps sending the next one, so network and
    /// conversion overlap. Signals are checked between batches, so Ctrl+C
    /// interrupts a long read (the next command discards what is left).
    /// `None` when `start` finds no statement to read (no more results).
    fn buffered<F>(
        &self,
        py: Python<'_>,
        as_dict: bool,
        batch_bytes: usize,
        start: F,
    ) -> PyResult<Option<Py<PyAny>>>
    where
        F: FnOnce(&mut Conn) -> rustmy_proto::Result<Head> + Send,
    {
        let to_pyerr = |failure| self.failure_to_pyerr(failure);
        let codec = self.codec();
        let custom = self.custom();
        self.locked(py, breaks_on_io, |session| {
            // One GIL release covers the statement and its first batch, so
            // a small query costs no more than before.
            let first = session
                .io(py, |conn| {
                    let Some((mut head, id)) = start(conn)? else {
                        return Ok(None);
                    };
                    let outcome = id
                        .map(|id| conn.read_rows(id, &mut head.rows, batch_bytes))
                        .transpose()?;
                    Ok(Some((head, id.zip(outcome))))
                })
                .map_err(to_pyerr)?;
            let Some((mut head, reading)) = first else {
                return Ok(None);
            };
            let Some((id, mut outcome)) = reading else {
                return result_to_py(py, &head, as_dict, &codec).map(Some);
            };
            let converter = Converter::new(py, &head.columns, as_dict, &codec, custom.as_deref())?;
            let mut rows = Vec::new();
            let mut raw = std::mem::take(&mut head.rows);
            loop {
                if let Err(err) = converter.convert(py, &raw, |row| rows.push(row)) {
                    if outcome == ReadOutcome::More {
                        // Keep the connection in sync before reporting.
                        let _ = session.io(py, |conn| conn.skip_rows(id, usize::MAX));
                    }
                    return Err(err);
                }
                if let ReadOutcome::Done { warnings } = outcome {
                    let count = rows.len();
                    return (
                        description(py, &head.columns, &codec)?,
                        PyTuple::new(py, rows)?,
                        count,
                        head.last_insert_id,
                        warnings,
                    )
                        .into_py_any(py)
                        .map(Some);
                }
                py.check_signals()?;
                raw.clear();
                outcome = session
                    .io(py, |conn| conn.read_rows(id, &mut raw, batch_bytes))
                    .map_err(to_pyerr)?;
            }
        })
    }

    /// Runs `f` for the streamed result set started at `generation`, with
    /// the GIL released, failing if another command ran since.
    pub(crate) fn run_stream<T, F>(
        &self,
        py: Python<'_>,
        generation: u64,
        f: F,
    ) -> Result<T, Failure>
    where
        T: Send,
        F: FnOnce(&mut Conn) -> rustmy_proto::Result<T> + Send,
    {
        py.detach(|| {
            let mut state = match self.state.try_lock() {
                Ok(guard) => guard,
                Err(TryLockError::WouldBlock) => return Err(Failure::Busy),
                Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            };
            let conn = match &mut *state {
                State::Open(conn) => conn,
                State::Broken => return Err(Failure::Broken),
                State::Closed => return Err(Failure::Closed),
            };
            if self.generation.load(Ordering::SeqCst) != generation {
                return Err(Failure::Discarded);
            }
            f(conn).map_err(|err| {
                if matches!(err, ProtoError::Io(_) | ProtoError::Protocol(_)) {
                    *state = State::Broken;
                }
                Failure::Proto(err)
            })
        })
    }

    /// Whether a command ran since the streamed result set of `generation`
    /// started; `Err` if the connection is closed or broken.
    pub(crate) fn check_stream(&self, generation: u64) -> PyResult<bool> {
        if self.generation.load(Ordering::SeqCst) == generation {
            return Ok(true);
        }
        match self.state.try_lock().as_deref() {
            Ok(State::Closed) => Err(self.failure_to_pyerr(Failure::Closed)),
            Ok(State::Broken) => Err(self.failure_to_pyerr(Failure::Broken)),
            _ => Ok(false),
        }
    }

    /// The streamed result set of `generation` has no rows left for the
    /// user, so the next command abandons nothing.
    pub(crate) fn stream_finished(&self, generation: u64) {
        let _ =
            self.open_stream
                .compare_exchange(generation, 0, Ordering::SeqCst, Ordering::SeqCst);
    }

    /// Runs a statement whose rows stay on the wire; returns its head and,
    /// when there is a result set, its stream id and generation.
    fn start_stream<F>(
        &self,
        py: Python<'_>,
        f: F,
    ) -> PyResult<(QueryResult, Option<(StreamId, u64)>)>
    where
        F: FnOnce(&mut Conn) -> rustmy_proto::Result<Option<(QueryResult, Option<StreamId>)>>
            + Send,
    {
        let generation = &self.generation;
        let open_stream = &self.open_stream;
        let head = self.with_conn(py, |conn| {
            Ok(f(conn)?.map(|(result, id)| {
                let started = id.map(|id| {
                    let current = generation.load(Ordering::SeqCst);
                    open_stream.store(current, Ordering::SeqCst);
                    (id, current)
                });
                (result, started)
            }))
        })?;
        Ok(head.unwrap_or_default())
    }

    /// `(description, stream, rowcount, last_insert_id, warnings)` for a
    /// streamed statement: rowcount is -1 when there is a result set (its
    /// size is unknown until the end), else the affected rows.
    fn stream_head(
        slf: &Bound<'_, Self>,
        head: QueryResult,
        started: Option<(StreamId, u64)>,
        as_dict: bool,
        batch_bytes: usize,
    ) -> PyResult<Py<PyAny>> {
        let py = slf.py();
        let Some((id, generation)) = started else {
            return (
                py.None(),
                py.None(),
                head.affected_rows,
                head.last_insert_id,
                head.warnings,
            )
                .into_py_any(py);
        };
        let codec = slf.get().codec();
        let custom = slf.get().custom();
        let converter = Converter::new(py, &head.columns, as_dict, &codec, custom.as_deref())?;
        let stream = ResultStream::new(
            slf.clone().unbind(),
            id,
            generation,
            converter,
            head.rows,
            batch_bytes,
        );
        (
            description(py, &head.columns, &codec)?,
            stream,
            -1,
            head.last_insert_id,
            head.warnings,
        )
            .into_py_any(py)
    }

    /// The session's character set.
    pub(crate) fn codec(&self) -> Arc<Codec> {
        Arc::clone(&self.codec.lock().unwrap_or_else(PoisonError::into_inner))
    }

    /// The custom `conv=` converters, if any.
    fn custom(&self) -> Option<Arc<Custom>> {
        self.custom
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// `query` in the session's character set, with `args` interpolated if
    /// given (without them it is sent as is, so `%` needs no escaping).
    fn statement<'a>(
        &self,
        query: &'a Bound<'_, PyString>,
        args: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<Cow<'a, [u8]>> {
        let template = query.to_str()?;
        let encoder = self.encoder()?;
        Ok(match args.filter(|a| !a.is_none()) {
            None if encoder.codec.is_utf8mb4() || template.is_ascii() => {
                Cow::Borrowed(template.as_bytes())
            }
            None => {
                let mut out = Vec::with_capacity(template.len() + 16);
                encoder.codec.encode_into(query.py(), &mut out, template)?;
                Cow::Owned(out)
            }
            Some(args) => {
                let mut out = Vec::with_capacity(template.len() + 64);
                encoder.interpolate(query.py(), &mut out, template, &Args::from_py(args))?;
                Cow::Owned(out)
            }
        })
    }

    /// Reads connection state that needs no I/O, keeping the GIL.
    fn peek<T>(&self, f: impl FnOnce(&Conn) -> T) -> PyResult<T> {
        let state = match self.state.try_lock() {
            Ok(guard) => guard,
            Err(TryLockError::WouldBlock) => return Err(self.failure_to_pyerr(Failure::Busy)),
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
        };
        match &*state {
            State::Open(conn) => Ok(f(conn)),
            State::Broken => Err(self.failure_to_pyerr(Failure::Broken)),
            State::Closed => Err(self.failure_to_pyerr(Failure::Closed)),
        }
    }

    /// Statement writer for this connection's SQL mode and character set.
    /// Sends a pipelined `executemany` window and reads every reply; the
    /// first server error is raised once all of them are read.
    fn run_window(&self, py: Python<'_>, window: &Window) -> PyResult<u64> {
        let totals = self.with_conn(py, |conn| conn.pipeline(window.statements()))?;
        match totals.error {
            Some((_, err)) => Err(self.failure_to_pyerr(Failure::Proto(err))),
            None => Ok(totals.rowcount),
        }
    }

    /// `executemany` of a statement that is not a batchable `INSERT`:
    /// windows of up to `max_pipeline` statements (and at most
    /// `Conn::pipeline_budget` bytes) are sent before their replies are
    /// read, each window read whole before the next one is built, so a
    /// server error stops the batch at the end of its window and the
    /// parameters (a generator, say) may use the connection in between, as
    /// with a loop of `execute()` calls. Signals are checked between
    /// windows. The last statement's reply is read as `execute` (or
    /// `execute_streamed`) reads one. `None` when `seq_of_args` is empty.
    fn pipelined_many<'py>(
        slf: &Bound<'py, Self>,
        query: &Bound<'py, PyString>,
        seq_of_args: &Bound<'py, PyAny>,
        max_pipeline: usize,
        streamed: bool,
        as_dict: bool,
        batch_bytes: usize,
    ) -> PyResult<Option<PipelinedMany<'py>>> {
        let py = slf.py();
        let this = slf.get();
        let template = query.to_str()?;
        let encoder = this.encoder()?;
        let budget = this.peek(Conn::pipeline_budget)?;
        // The server reads whatever follows a LOAD DATA LOCAL as its file:
        // one statement per window.
        let max_pipeline = if mentions_local_infile(template.as_bytes()) {
            1
        } else {
            max_pipeline.max(1)
        };
        let mut window = Window::default();
        let mut statement = Vec::new();
        let mut rowcount = 0;
        for item in seq_of_args.try_iter()? {
            statement.clear();
            let built = item.and_then(|args| {
                encoder.interpolate(py, &mut statement, template, &Args::from_py(&args))
            });
            if let Err(err) = built {
                // The statements before it run, as in a loop of execute()
                // calls.
                if window.len() > 0 {
                    this.run_window(py, &window)?;
                }
                return Err(err);
            }
            if window.len() > 0
                && (window.len() >= max_pipeline || window.bytes.len() + statement.len() > budget)
            {
                rowcount += this.run_window(py, &window)?;
                window.clear();
                py.check_signals()?;
            }
            window.push(&statement);
        }
        let Some(last) = window.statements().last() else {
            return Ok(None);
        };
        let others = window.statements().take(window.len() - 1);
        let mut before = 0;
        let start = |conn: &mut Conn| {
            let (totals, head, id) = conn.pipeline_streamed(others, last)?;
            before = totals.rowcount;
            Ok(Some((head, id)))
        };
        let result = if streamed {
            let (head, started) = this.start_stream(py, start)?;
            Self::stream_head(slf, head, started, as_dict, batch_bytes)?
        } else {
            this.buffered(py, as_dict, batch_bytes, start)?
                .unwrap_or_else(|| py.None())
        };
        Ok(Some((result, rowcount + before, PyBytes::new(py, last))))
    }

    /// How `execute_prepared` runs `query` with `args`: prepared, with
    /// `?` placeholders and the arguments bound to them, or (without
    /// placeholders) as `execute` sends it.
    fn prepared_statement(
        &self,
        py: Python<'_>,
        query: &Bound<'_, PyString>,
        args: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<Plan> {
        let Some(args) = args.filter(|a| !a.is_none()) else {
            return Ok(Plan::Text(self.statement(query, None)?.into_owned()));
        };
        let encoder = self.encoder()?;
        let prepared = encoder.prepared_sql(py, query.to_str()?)?;
        if prepared.slots.is_empty() {
            // Interpolation still turns `%%` into `%` and checks that no
            // argument is left over.
            return Ok(Plan::Text(self.statement(query, Some(args))?.into_owned()));
        }
        let mut params = Execute::default();
        encoder.bind(py, &mut params, &prepared.slots, &Args::from_py(args))?;
        Ok(Plan::Prepared(prepared.sql, params))
    }

    /// Runs one window of a prepared `executemany`; the first server error
    /// is raised once every reply is read.
    fn run_prepared_window(&self, py: Python<'_>, sql: &[u8], window: &[Execute]) -> PyResult<u64> {
        let totals = self.with_conn(py, |conn| conn.execute_many_cached(sql, window))?;
        match totals.error {
            Some((_, err)) => Err(self.failure_to_pyerr(Failure::Proto(err))),
            None => Ok(totals.rowcount),
        }
    }

    /// `executemany` of a `PreparedCursor`: as `pipelined_many`, with each
    /// item bound to the prepared statement (one `COM_STMT_EXECUTE` each)
    /// instead of interpolated. A query without placeholders runs as text,
    /// as `execute_prepared` runs it.
    fn prepared_many<'py>(
        slf: &Bound<'py, Self>,
        query: &Bound<'py, PyString>,
        seq_of_args: &Bound<'py, PyAny>,
        max_pipeline: usize,
        streamed: bool,
        as_dict: bool,
        batch_bytes: usize,
    ) -> PyResult<Option<PipelinedMany<'py>>> {
        let py = slf.py();
        let this = slf.get();
        let template = query.to_str()?;
        let encoder = this.encoder()?;
        let prepared = encoder.prepared_sql(py, template)?;
        if prepared.slots.is_empty() {
            return Self::pipelined_many(
                slf,
                query,
                seq_of_args,
                max_pipeline,
                streamed,
                as_dict,
                batch_bytes,
            );
        }
        let sql = prepared.sql;
        let budget = this.peek(Conn::pipeline_budget)?;
        let max_pipeline = max_pipeline.max(1);
        let mut window: Vec<Execute> = Vec::new();
        let mut window_bytes = 0;
        let mut rowcount = 0;
        let mut last_args = None;
        for item in seq_of_args.try_iter()? {
            let mut params = Execute::default();
            let bound = item.and_then(|args| {
                encoder.bind(py, &mut params, &prepared.slots, &Args::from_py(&args))?;
                Ok(args)
            });
            let args = match bound {
                Ok(args) => args,
                Err(err) => {
                    // The items before it run, as in a loop of execute()
                    // calls.
                    if !window.is_empty() {
                        this.run_prepared_window(py, &sql, &window)?;
                    }
                    return Err(err);
                }
            };
            if !window.is_empty()
                && (window.len() >= max_pipeline || window_bytes + params.encoded_len() > budget)
            {
                rowcount += this.run_prepared_window(py, &sql, &window)?;
                window.clear();
                window_bytes = 0;
                py.check_signals()?;
            }
            window_bytes += params.encoded_len();
            window.push(params);
            last_args = Some(args);
        }
        let (Some(last), Some(last_args)) = (window.pop(), last_args) else {
            return Ok(None);
        };
        let mut before = 0;
        let start = |conn: &mut Conn| {
            let (totals, head, id) = conn.execute_many_cached_streamed(&sql, &window, &last)?;
            before = totals.rowcount;
            Ok(Some((head, id)))
        };
        let result = if streamed {
            let (head, started) = this.start_stream(py, start)?;
            Self::stream_head(slf, head, started, as_dict, batch_bytes)?
        } else {
            this.buffered(py, as_dict, batch_bytes, start)?
                .unwrap_or_else(|| py.None())
        };
        // `_executed`: the last item as `mogrify` shows it.
        let mut shown = Vec::new();
        encoder.interpolate(py, &mut shown, template, &Args::from_py(&last_args))?;
        Ok(Some((result, rowcount + before, PyBytes::new(py, &shown))))
    }

    fn encoder(&self) -> PyResult<Encoder> {
        let no_backslash_escapes =
            self.peek(|conn| conn.status_flags() & status::NO_BACKSLASH_ESCAPES != 0)?;
        Ok(Encoder {
            no_backslash_escapes,
            binary_prefix: self.binary_prefix,
            codec: self.codec(),
            custom: self.custom(),
        })
    }

    /// Runs one statement of an `executemany` batch and resets `sql` to
    /// the prefix for the next one.
    fn flush_batch(
        &self,
        py: Python<'_>,
        sql: &mut Vec<u8>,
        suffix: &[u8],
        prefix_len: usize,
        totals: &mut (u64, Option<u64>),
    ) -> PyResult<()> {
        sql.extend_from_slice(suffix);
        let statement: &[u8] = sql;
        let result = self.with_conn(py, |conn| conn.query(statement))?;
        totals.0 += result.affected_rows;
        totals.1 = Some(result.last_insert_id);
        sql.truncate(prefix_len);
        Ok(())
    }

    pub(crate) fn failure_to_pyerr(&self, failure: Failure) -> PyErr {
        match failure {
            Failure::Busy => ProgrammingError::new_err((
                0,
                "connection is in use by another thread; use one connection per thread",
            )),
            Failure::Discarded => ProgrammingError::new_err((
                0,
                "result set discarded: another command ran on this connection \
                 before all its rows were fetched",
            )),
            Failure::Closed => InterfaceError::new_err((0, "connection already closed")),
            Failure::Broken => OperationalError::new_err((
                client_code::SERVER_GONE,
                "MySQL server has gone away (connection broken by an earlier error)",
            )),
            Failure::Proto(err) => from_proto(err, Phase::Command, &self.target, |message| {
                let codec = self.codec();
                Python::attach(|py| codec.decode_lenient(py, message))
            }),
        }
    }
}

#[pymethods]
impl Connection {
    /// Runs one SQL statement as is; see `convert::result_to_py` for the
    /// result.
    #[pyo3(signature = (sql, batch_bytes = 4096))]
    fn query(
        &self,
        py: Python<'_>,
        sql: &Bound<'_, PyString>,
        batch_bytes: usize,
    ) -> PyResult<Py<PyAny>> {
        let sql = self.statement(sql, None)?;
        let result = self.buffered(py, false, batch_bytes, |conn| {
            conn.query_streamed(&sql).map(Some)
        })?;
        Ok(result.unwrap_or_else(|| py.None()))
    }

    /// Interpolates `args` (if any) into `query` and runs it, reading the
    /// whole result in batches of about `batch_bytes`. Without `args` the
    /// query is sent as is, so `%` needs no escaping. Rows are dicts with
    /// `as_dict`, else tuples.
    #[pyo3(signature = (query, args = None, as_dict = false, batch_bytes = 4096))]
    fn execute(
        &self,
        py: Python<'_>,
        query: &Bound<'_, PyString>,
        args: Option<&Bound<'_, PyAny>>,
        as_dict: bool,
        batch_bytes: usize,
    ) -> PyResult<Py<PyAny>> {
        let sql = self.statement(query, args)?;
        let result = self.buffered(py, as_dict, batch_bytes, |conn| {
            conn.query_streamed(&sql).map(Some)
        })?;
        Ok(result.unwrap_or_else(|| py.None()))
    }

    /// Like `execute`, but the rows stay on the server's side of the socket
    /// and are read in batches of about `batch_bytes` through the returned
    /// `ResultStream`.
    #[pyo3(signature = (query, args = None, as_dict = false, batch_bytes = 4096))]
    fn execute_streamed(
        slf: &Bound<'_, Self>,
        query: &Bound<'_, PyString>,
        args: Option<&Bound<'_, PyAny>>,
        as_dict: bool,
        batch_bytes: usize,
    ) -> PyResult<Py<PyAny>> {
        let this = slf.get();
        let sql = this.statement(query, args)?;
        let (head, started) =
            this.start_stream(slf.py(), |conn| conn.query_streamed(&sql).map(Some))?;
        Self::stream_head(slf, head, started, as_dict, batch_bytes)
    }

    /// Runs `query` as a prepared statement (binary protocol), its
    /// placeholders bound to `args` as typed values; the statement is
    /// prepared once per connection (`statement_cache_size` are kept). A
    /// query without placeholders has nothing to bind and runs as `execute`
    /// runs it. Returns what `execute` returns.
    #[pyo3(signature = (query, args = None, as_dict = false, batch_bytes = 4096))]
    fn execute_prepared(
        &self,
        py: Python<'_>,
        query: &Bound<'_, PyString>,
        args: Option<&Bound<'_, PyAny>>,
        as_dict: bool,
        batch_bytes: usize,
    ) -> PyResult<Py<PyAny>> {
        let plan = self.prepared_statement(py, query, args)?;
        let result = self.buffered(py, as_dict, batch_bytes, |conn| plan.run(conn).map(Some))?;
        Ok(result.unwrap_or_else(|| py.None()))
    }

    /// Like `execute_prepared`, streaming the rows as `execute_streamed`
    /// does.
    #[pyo3(signature = (query, args = None, as_dict = false, batch_bytes = 4096))]
    fn execute_prepared_streamed(
        slf: &Bound<'_, Self>,
        query: &Bound<'_, PyString>,
        args: Option<&Bound<'_, PyAny>>,
        as_dict: bool,
        batch_bytes: usize,
    ) -> PyResult<Py<PyAny>> {
        let this = slf.get();
        let plan = this.prepared_statement(slf.py(), query, args)?;
        let (head, started) = this.start_stream(slf.py(), |conn| plan.run(conn).map(Some))?;
        Self::stream_head(slf, head, started, as_dict, batch_bytes)
    }

    /// The query exactly as `execute` would send it, as `str` (bytes that
    /// are not text, such as binary literals, come back as surrogate
    /// escapes).
    #[pyo3(signature = (query, args = None))]
    fn mogrify<'py>(
        &self,
        query: &Bound<'py, PyString>,
        args: Option<&Bound<'py, PyAny>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let Some(args) = args.filter(|a| !a.is_none()) else {
            return Ok(query.clone().into_any());
        };
        let py = query.py();
        let template = query.to_str()?;
        let encoder = self.encoder()?;
        let mut out = Vec::with_capacity(template.len() + 64);
        encoder.interpolate(py, &mut out, template, &Args::from_py(args))?;
        encoder.codec.decode_statement(py, &out)
    }

    /// `value` as an SQL literal for this connection (its character set
    /// and SQL mode), as `str`.
    fn literal<'py>(&self, value: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
        let encoder = self.encoder()?;
        let mut out = Vec::new();
        encoder.literal(&mut out, value)?;
        encoder.codec.decode_statement(value.py(), &out)
    }

    /// A statement this connection built (`executemany`'s last one), as
    /// `str`, as `mogrify` shows it.
    fn decode_statement<'py>(
        &self,
        py: Python<'py>,
        statement: &[u8],
    ) -> PyResult<Bound<'py, PyAny>> {
        self.codec().decode_statement(py, statement)
    }

    /// Batches a single-row `INSERT`/`REPLACE … VALUES (…)` into multi-row
    /// statements of at most `max_stmt_length` bytes (a row that is larger
    /// on its own still goes alone). Returns `(affected_rows,
    /// last_insert_id, last_statement)`, `last_insert_id` and
    /// `last_statement` being `None` when nothing ran, or `None` when the
    /// query cannot be batched and must run row by row.
    fn executemany<'py>(
        &self,
        py: Python<'py>,
        query: &Bound<'py, PyString>,
        seq_of_args: &Bound<'py, PyAny>,
        max_stmt_length: usize,
    ) -> PyResult<Option<BatchTotals<'py>>> {
        let template = query.to_str()?;
        let Some(parts) = insert_values(template.as_bytes()) else {
            return Ok(None);
        };
        // The parts split at ASCII tokens, hence at character boundaries;
        // anything else runs row by row.
        let (Some(prefix_template), Some(row_template), Some(suffix_template)) = (
            template.get(parts.prefix),
            template.get(parts.row),
            template.get(parts.suffix),
        ) else {
            return Ok(None);
        };
        if has_placeholder(prefix_template.as_bytes())
            || has_placeholder(suffix_template.as_bytes())
        {
            return Ok(None);
        }
        let encoder = self.encoder()?;
        let prefix = unescape_percent(&encoder, py, prefix_template)?;
        let suffix = unescape_percent(&encoder, py, suffix_template)?;

        let mut sql = prefix.clone();
        let mut row = Vec::new();
        let mut rows_in_batch = 0usize;
        let mut totals = (0, None);
        for args in seq_of_args.try_iter()? {
            row.clear();
            encoder.interpolate(py, &mut row, row_template, &Args::from_py(&args?))?;
            if rows_in_batch > 0 && sql.len() + 1 + row.len() + suffix.len() > max_stmt_length {
                self.flush_batch(py, &mut sql, &suffix, prefix.len(), &mut totals)?;
                rows_in_batch = 0;
            }
            if rows_in_batch > 0 {
                sql.push(b',');
            }
            sql.extend_from_slice(&row);
            rows_in_batch += 1;
        }
        let mut last = None;
        if rows_in_batch > 0 {
            sql.extend_from_slice(&suffix);
            last = Some(PyBytes::new(py, &sql));
            sql.truncate(sql.len() - suffix.len());
            self.flush_batch(py, &mut sql, &suffix, prefix.len(), &mut totals)?;
        }
        Ok(Some((totals.0, totals.1, last)))
    }

    /// `executemany` of a statement that is not a batchable `INSERT`, with
    /// the statements pipelined; see `pipelined_many`.
    #[pyo3(signature = (query, seq_of_args, max_pipeline, as_dict = false, batch_bytes = 4096))]
    fn executemany_pipelined<'py>(
        slf: &Bound<'py, Self>,
        query: &Bound<'py, PyString>,
        seq_of_args: &Bound<'py, PyAny>,
        max_pipeline: usize,
        as_dict: bool,
        batch_bytes: usize,
    ) -> PyResult<Option<PipelinedMany<'py>>> {
        Self::pipelined_many(
            slf,
            query,
            seq_of_args,
            max_pipeline,
            false,
            as_dict,
            batch_bytes,
        )
    }

    /// Like `executemany_pipelined`, streaming the last statement's rows as
    /// `execute_streamed` does.
    #[pyo3(signature = (query, seq_of_args, max_pipeline, as_dict = false, batch_bytes = 4096))]
    fn executemany_pipelined_streamed<'py>(
        slf: &Bound<'py, Self>,
        query: &Bound<'py, PyString>,
        seq_of_args: &Bound<'py, PyAny>,
        max_pipeline: usize,
        as_dict: bool,
        batch_bytes: usize,
    ) -> PyResult<Option<PipelinedMany<'py>>> {
        Self::pipelined_many(
            slf,
            query,
            seq_of_args,
            max_pipeline,
            true,
            as_dict,
            batch_bytes,
        )
    }

    /// `executemany` of a single-row `INSERT`/`REPLACE … VALUES (…)` with a
    /// `PreparedCursor`: multi-row prepared statements (`VALUES (?, ?), (?,
    /// ?)…`), as `executemany` batches them as text, with the parameters
    /// still typed. Every statement has the same number of rows (from the
    /// first row's size, within `max_stmt_length` and the server's 65,535
    /// placeholders), so it is prepared once; a batch is cut short when a
    /// row is larger than that size allows, and the last one holds what is
    /// left. Returns what `executemany` returns, or `None` when the query
    /// cannot be batched (before reading `seq_of_args`).
    fn executemany_prepared_insert<'py>(
        &self,
        py: Python<'py>,
        query: &Bound<'py, PyString>,
        seq_of_args: &Bound<'py, PyAny>,
        max_stmt_length: usize,
    ) -> PyResult<Option<BatchTotals<'py>>> {
        let template = query.to_str()?;
        let Some(parts) = insert_values(template.as_bytes()) else {
            return Ok(None);
        };
        let (Some(prefix), Some(row), Some(suffix)) = (
            template.get(parts.prefix),
            template.get(parts.row),
            template.get(parts.suffix),
        ) else {
            return Ok(None);
        };
        if has_placeholder(prefix.as_bytes()) || has_placeholder(suffix.as_bytes()) {
            return Ok(None);
        }
        let encoder = self.encoder()?;
        let row_sql = encoder.prepared_sql(py, row)?;
        if row_sql.slots.is_empty() {
            return Ok(None);
        }
        let prefix_sql = encoder.prepared_sql(py, prefix)?.sql;
        let suffix_sql = encoder.prepared_sql(py, suffix)?.sql;
        let most_rows = (usize::from(u16::MAX) / row_sql.slots.len()).max(1);
        let statement = |rows: usize| {
            let mut sql = prefix_sql.clone();
            for i in 0..rows {
                if i > 0 {
                    sql.push(b',');
                }
                sql.extend_from_slice(&row_sql.sql);
            }
            sql.extend_from_slice(&suffix_sql);
            sql
        };
        let run = |batch: &Execute, rows: usize, totals: &mut (u64, Option<u64>)| {
            let sql = statement(rows);
            let result = self.with_conn(py, |conn| conn.execute_cached(&sql, batch))?;
            totals.0 += result.affected_rows;
            totals.1 = Some(result.last_insert_id);
            PyResult::Ok(())
        };
        let mut rows_per_statement = None;
        let mut batch = Execute::default();
        let mut batch_args = Vec::new();
        let mut totals = (0, None);
        for item in seq_of_args.try_iter()? {
            let args = item?;
            let mut values = Execute::default();
            encoder.bind(py, &mut values, &row_sql.slots, &Args::from_py(&args))?;
            let limit = *rows_per_statement.get_or_insert_with(|| {
                (max_stmt_length / values.encoded_len().max(1)).clamp(1, most_rows)
            });
            if !batch_args.is_empty()
                && (batch_args.len() >= limit
                    || batch.encoded_len() + values.encoded_len() > max_stmt_length)
            {
                run(&batch, batch_args.len(), &mut totals)?;
                batch.clear();
                batch_args.clear();
            }
            batch.append(&values);
            batch_args.push(args);
        }
        if batch_args.is_empty() {
            return Ok(Some((0, None, None)));
        }
        run(&batch, batch_args.len(), &mut totals)?;
        // `_executed`: the last statement, interpolated as `mogrify` shows
        // it.
        let mut shown = unescape_percent(&encoder, py, prefix)?;
        for (i, args) in batch_args.iter().enumerate() {
            if i > 0 {
                shown.push(b',');
            }
            encoder.interpolate(py, &mut shown, row, &Args::from_py(args))?;
        }
        shown.extend_from_slice(&unescape_percent(&encoder, py, suffix)?);
        Ok(Some((totals.0, totals.1, Some(PyBytes::new(py, &shown)))))
    }

    /// `executemany` of a `PreparedCursor`: each item bound to the
    /// prepared statement, pipelined as `executemany_pipelined` does.
    #[pyo3(signature = (query, seq_of_args, max_pipeline, as_dict = false, batch_bytes = 4096))]
    fn executemany_prepared<'py>(
        slf: &Bound<'py, Self>,
        query: &Bound<'py, PyString>,
        seq_of_args: &Bound<'py, PyAny>,
        max_pipeline: usize,
        as_dict: bool,
        batch_bytes: usize,
    ) -> PyResult<Option<PipelinedMany<'py>>> {
        Self::prepared_many(
            slf,
            query,
            seq_of_args,
            max_pipeline,
            false,
            as_dict,
            batch_bytes,
        )
    }

    /// Next result of a multi-result query, or `None` when there is none.
    #[pyo3(signature = (as_dict = false, batch_bytes = 4096))]
    fn next_result(
        &self,
        py: Python<'_>,
        as_dict: bool,
        batch_bytes: usize,
    ) -> PyResult<Option<Py<PyAny>>> {
        self.buffered(py, as_dict, batch_bytes, Conn::next_result_streamed)
    }

    /// Like `next_result`, streaming the rows as `execute_streamed` does.
    #[pyo3(signature = (as_dict = false, batch_bytes = 4096))]
    fn next_result_streamed(
        slf: &Bound<'_, Self>,
        as_dict: bool,
        batch_bytes: usize,
    ) -> PyResult<Option<Py<PyAny>>> {
        let this = slf.get();
        if !this.peek(Conn::has_more_results)? {
            return Ok(None);
        }
        let (head, started) = this.start_stream(slf.py(), Conn::next_result_streamed)?;
        Self::stream_head(slf, head, started, as_dict, batch_bytes).map(Some)
    }

    fn ping(&self, py: Python<'_>) -> PyResult<()> {
        self.with_conn(py, Conn::ping)
    }

    fn select_db(&self, py: Python<'_>, database: &str) -> PyResult<()> {
        let database = self.codec().encode(py, database)?;
        self.with_conn(py, |conn| conn.select_db(&database))
    }

    fn reset_connection(&self, py: Python<'_>) -> PyResult<()> {
        self.with_conn(py, Conn::reset_connection)
    }

    #[pyo3(signature = (user, password, database = None))]
    fn change_user(
        &self,
        py: Python<'_>,
        user: &str,
        password: &str,
        database: Option<&str>,
    ) -> PyResult<()> {
        // The packet names the handshake's collation, and the server reads
        // the user and database in its character set (which may not be the
        // session's after set_character_set()).
        let codec = match self.peek(|conn| charset_of(conn.collation()))? {
            Some(name) if name != self.codec().name() => Codec::new(py, name)?,
            _ => self.codec(),
        };
        let user = codec.encode(py, user)?;
        let database = database.map(|db| codec.encode(py, db)).transpose()?;
        // A failed COM_CHANGE_USER makes MySQL drop the connection while
        // MariaDB keeps the old user: treat the connection as gone on every
        // error so behaviour does not depend on the server.
        self.run(
            py,
            |conn| conn.change_user(&user, password, database.as_deref()),
            |_| true,
        )
    }

    /// Sends `COM_QUIT` and closes the socket. Idempotent.
    fn close(&self, py: Python<'_>) -> PyResult<()> {
        py.detach(|| {
            let mut state = match self.state.try_lock() {
                Ok(guard) => guard,
                Err(TryLockError::WouldBlock) => return Err(Failure::Busy),
                Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            };
            self.generation.fetch_add(1, Ordering::SeqCst);
            self.open_stream.store(0, Ordering::SeqCst);
            if let State::Open(conn) = std::mem::replace(&mut *state, State::Closed) {
                conn.close();
            }
            Ok(())
        })
        .map_err(|failure| self.failure_to_pyerr(failure))
    }

    /// Switches the session to `charset` (`SET NAMES`, with `COLLATE` when
    /// `collation` is given) and encodes and decodes with it from now on.
    #[pyo3(signature = (charset, collation = None))]
    fn set_character_set(
        &self,
        py: Python<'_>,
        charset: &str,
        collation: Option<&str>,
    ) -> PyResult<()> {
        let codec = Codec::new(py, charset)?;
        let sql = format!("SET {}", names_clause(codec.name(), collation)?);
        self.with_conn(py, |conn| conn.query(sql.as_bytes()))?;
        *self.codec.lock().unwrap_or_else(PoisonError::into_inner) = codec;
        Ok(())
    }

    /// Installs the `conv=` decoders and encoders that differ from the
    /// defaults (`rustmy.converters.split_conv`); `conv` is passed to every
    /// encoder as its second argument.
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
        self.peek(Conn::server_charset)
    }

    /// Python codec name of the session's character set.
    #[getter]
    fn encoding(&self) -> &'static str {
        self.codec().python_name()
    }

    /// Whether the connection is encrypted with TLS.
    #[getter]
    fn tls(&self) -> PyResult<bool> {
        self.peek(Conn::is_tls)
    }

    /// Compression in use: `"zlib"`, `"zstd"` or `None`.
    #[getter]
    fn compression(&self) -> PyResult<Option<&'static str>> {
        self.peek(|conn| {
            conn.compression().map(|algorithm| match algorithm {
                Compression::Zlib => "zlib",
                Compression::Zstd { .. } => "zstd",
            })
        })
    }

    #[getter]
    fn closed(&self) -> bool {
        matches!(
            self.state.try_lock().as_deref(),
            Ok(State::Closed | State::Broken)
        )
    }

    #[getter]
    fn server_version(&self) -> PyResult<String> {
        self.peek(|conn| conn.server_version().to_owned())
    }

    #[getter]
    fn connection_id(&self) -> PyResult<u32> {
        self.peek(Conn::connection_id)
    }

    /// Server status flags from the last OK/EOF packet.
    #[getter]
    fn server_status(&self) -> PyResult<u16> {
        self.peek(Conn::status_flags)
    }

    #[getter]
    fn has_more_results(&self) -> PyResult<bool> {
        self.peek(Conn::has_more_results)
    }
}

/// `NAMES <charset>`, with `COLLATE <collation>` when given: the clause of
/// a `SET` statement switching the session's character set. Both names are
/// spliced into SQL, so only letters, digits and underscores are accepted.
#[pyfunction]
#[pyo3(signature = (charset, collation = None))]
pub(crate) fn names_clause(charset: &str, collation: Option<&str>) -> PyResult<String> {
    let mut clause = format!("NAMES {}", sql_name("character set", charset)?);
    if let Some(collation) = collation {
        clause.push_str(" COLLATE ");
        clause.push_str(sql_name("collation", collation)?);
    }
    Ok(clause)
}

fn sql_name<'a>(what: &str, name: &'a str) -> PyResult<&'a str> {
    let valid = !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_');
    if valid {
        Ok(name)
    } else {
        Err(ProgrammingError::new_err((
            0,
            format!("invalid {what} name {name:?}"),
        )))
    }
}

/// Opens an authenticated connection. Python-facing argument handling
/// (defaults, aliases) lives in `rustmy.connections`.
#[pyfunction]
#[pyo3(signature = (
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
pub(crate) fn connect(
    py: Python<'_>,
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
) -> PyResult<Connection> {
    if local_infile && local_infile_dir.is_none() {
        return Err(PyValueError::new_err(
            "local_infile=True needs local_infile_dir: LOAD DATA LOCAL INFILE \
             only sends files from a directory you allow",
        ));
    }
    let compress = match compress {
        None => None,
        Some("zlib") => Some(Compression::Zlib),
        Some("zstd") if (1..=22).contains(&zstd_level) => {
            Some(Compression::Zstd { level: zstd_level })
        }
        Some("zstd") => {
            return Err(PyValueError::new_err(format!(
                "zstd_level must be between 1 and 22, got {zstd_level}"
            )));
        }
        Some(other) => {
            return Err(PyValueError::new_err(format!(
                "invalid compress {other:?}; expected zlib or zstd"
            )));
        }
    };
    let mode = SslMode::parse(ssl_mode).ok_or_else(|| {
        PyValueError::new_err(format!(
            "invalid ssl_mode {ssl_mode:?}; expected disabled, preferred, required, \
             verify_ca or verify_identity"
        ))
    })?;
    let target = unix_socket.as_ref().map_or_else(
        || format!("'{host}:{port}'"),
        |p| format!("'{}'", p.display()),
    );
    let codec = Codec::new(py, charset)?;
    // The server reads both in the session's character set.
    let user = codec.encode(py, user)?;
    let database = database.map(|db| codec.encode(py, db)).transpose()?;
    let opts = Opts {
        host,
        port,
        unix_socket,
        user,
        password,
        database,
        connect_timeout: timeout(connect_timeout, "connect_timeout")?,
        read_timeout: timeout(read_timeout, "read_timeout")?,
        write_timeout: timeout(write_timeout, "write_timeout")?,
        client_flag,
        charset: codec.name().to_owned(),
        tls: TlsOpts {
            mode,
            ca: ssl_ca,
            cert: ssl_cert,
            key: ssl_key,
        },
        server_public_key,
        get_server_public_key,
        statement_cache: statement_cache_size,
        compress,
        local_infile_dir,
    };
    let conn = py.detach(|| Conn::connect(&opts)).map_err(|err| {
        from_proto(err, Phase::Connect, &target, |message| {
            String::from_utf8_lossy(message).into_owned()
        })
    })?;
    Ok(Connection {
        state: Mutex::new(State::Open(Box::new(conn))),
        target,
        generation: AtomicU64::new(0),
        open_stream: AtomicU64::new(0),
        codec: Mutex::new(codec),
        custom: Mutex::new(None),
        binary_prefix,
    })
}

/// Warns that a command is discarding rows the user never fetched, pointing
/// at the first caller outside the `rustmy` package.
fn warn_abandoned_stream(py: Python<'_>) -> PyResult<()> {
    let mut stacklevel = 1;
    let mut frame = py
        .import("sys")
        .and_then(|sys| sys.call_method1("_getframe", (0,)))
        .ok();
    while let Some(f) = frame.filter(|f| !f.is_none()) {
        let internal = f
            .getattr("f_globals")
            .and_then(|globals| globals.get_item("__name__"))
            .and_then(|name| name.extract::<String>())
            .is_ok_and(|name| name == "rustmy" || name.starts_with("rustmy."));
        if !internal {
            break;
        }
        stacklevel += 1;
        frame = f.getattr("f_back").ok();
    }
    PyErr::warn(
        py,
        &py.get_type::<PyUserWarning>(),
        ABANDONED_STREAM_WARNING,
        stacklevel,
    )
}

fn timeout(seconds: Option<f64>, name: &str) -> PyResult<Option<Duration>> {
    seconds
        .map(|s| {
            Duration::try_from_secs_f64(s)
                .ok()
                .filter(|d| !d.is_zero())
                .ok_or_else(|| {
                    PyValueError::new_err(format!(
                        "{name} must be a positive number of seconds, got {s}"
                    ))
                })
        })
        .transpose()
}

/// The UTF-8 statement writer of the module-level helpers.
pub(crate) fn utf8_encoder(no_backslash_escapes: bool) -> Encoder {
    Encoder {
        no_backslash_escapes,
        binary_prefix: true,
        codec: Arc::clone(&UTF8),
        custom: None,
    }
}

/// `value` as an SQL literal, for a UTF-8 connection.
#[pyfunction]
#[pyo3(signature = (value, no_backslash_escapes = false))]
pub(crate) fn literal<'py>(
    value: &Bound<'py, PyAny>,
    no_backslash_escapes: bool,
) -> PyResult<Bound<'py, PyAny>> {
    let encoder = utf8_encoder(no_backslash_escapes);
    let mut out = Vec::new();
    encoder.literal(&mut out, value)?;
    encoder.codec.decode_statement(value.py(), &out)
}

/// `value` escaped for use inside a single-quoted SQL literal.
#[pyfunction]
#[pyo3(signature = (value, no_backslash_escapes = false))]
pub(crate) fn escape_string(value: &str, no_backslash_escapes: bool) -> PyResult<String> {
    let mut out = Vec::with_capacity(value.len() + 8);
    rustmy_proto::sql::escape_into(&mut out, value.as_bytes(), no_backslash_escapes);
    // Escaping only inserts ASCII into valid UTF-8, so this cannot fail.
    String::from_utf8(out).map_err(|e| PyValueError::new_err(e.to_string()))
}

/// `query` with `args` interpolated, for a UTF-8 connection.
#[pyfunction]
#[pyo3(signature = (query, args, no_backslash_escapes = false))]
pub(crate) fn interpolate<'py>(
    query: &Bound<'py, PyString>,
    args: &Bound<'py, PyAny>,
    no_backslash_escapes: bool,
) -> PyResult<Bound<'py, PyAny>> {
    let py = query.py();
    let encoder = utf8_encoder(no_backslash_escapes);
    let template = query.to_str()?;
    let mut out = Vec::with_capacity(template.len() + 64);
    encoder.interpolate(py, &mut out, template, &Args::from_py(args))?;
    encoder.codec.decode_statement(py, &out)
}
