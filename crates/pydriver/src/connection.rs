//! `rustmy._core.Connection`: a thread-aware wrapper around the protocol
//! connection that releases the GIL for all network I/O.

use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::TryLockError;
use std::time::Duration;

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use rustmy_proto::Conn;
use rustmy_proto::Error as ProtoError;
use rustmy_proto::Opts;
use rustmy_proto::SslMode;
use rustmy_proto::TlsOpts;

use std::borrow::Cow;

use pyo3::types::PyBytes;
use pyo3::types::PyString;
use rustmy_proto::constants::status;
use rustmy_proto::sql::insert_values;

use crate::convert::result_to_py;
use crate::errors::InterfaceError;
use crate::errors::OperationalError;
use crate::errors::Phase;
use crate::errors::ProgrammingError;
use crate::errors::client_code;
use crate::errors::from_proto;
use crate::params::Args;
use crate::params::BinaryLiteral;
use crate::params::Encoder;
use crate::params::has_placeholder;
use crate::params::unescape_percent;

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
enum Failure {
    Busy,
    Closed,
    Broken,
    Proto(ProtoError),
}

#[pyclass(frozen, module = "rustmy._core")]
#[derive(Debug)]
pub(crate) struct Connection {
    state: Mutex<State>,
    /// `host:port` or socket path, for error messages.
    target: String,
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
        self.run(py, f, |err| {
            matches!(err, ProtoError::Io(_) | ProtoError::Protocol(_))
        })
    }

    /// Like [`Self::with_conn`], with `breaks` deciding which errors leave
    /// the connection unusable.
    fn run<T, F>(&self, py: Python<'_>, f: F, breaks: fn(&ProtoError) -> bool) -> PyResult<T>
    where
        T: Send,
        F: FnOnce(&mut Conn) -> rustmy_proto::Result<T> + Send,
    {
        py.detach(|| {
            let mut state = match self.state.try_lock() {
                Ok(guard) => guard,
                Err(TryLockError::WouldBlock) => return Err(Failure::Busy),
                // A panic cannot happen inside the protocol code (it is
                // panic-free by lint policy); recover the state regardless.
                Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            };
            let conn = match &mut *state {
                State::Open(conn) => conn,
                State::Broken => return Err(Failure::Broken),
                State::Closed => return Err(Failure::Closed),
            };
            f(conn).map_err(|err| {
                if breaks(&err) {
                    *state = State::Broken;
                }
                Failure::Proto(err)
            })
        })
        .map_err(|failure| self.failure_to_pyerr(failure))
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

    /// Literal writer for this connection's current SQL mode.
    fn encoder(&self) -> PyResult<Encoder> {
        self.peek(|conn| Encoder {
            no_backslash_escapes: conn.status_flags() & status::NO_BACKSLASH_ESCAPES != 0,
            binary: BinaryLiteral::Escaped,
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

    fn failure_to_pyerr(&self, failure: Failure) -> PyErr {
        match failure {
            Failure::Busy => ProgrammingError::new_err((
                0,
                "connection is in use by another thread; use one connection per thread",
            )),
            Failure::Closed => InterfaceError::new_err((0, "connection already closed")),
            Failure::Broken => OperationalError::new_err((
                client_code::SERVER_GONE,
                "MySQL server has gone away (connection broken by an earlier error)",
            )),
            Failure::Proto(err) => from_proto(err, Phase::Command, &self.target),
        }
    }
}

#[pymethods]
impl Connection {
    /// Runs one SQL statement; see `convert::result_to_py` for the result.
    fn query(&self, py: Python<'_>, sql: &[u8]) -> PyResult<Py<PyAny>> {
        let result = self.with_conn(py, |conn| conn.query(sql))?;
        result_to_py(py, &result)
    }

    /// Interpolates `args` (if any) into `query` and runs it. Without
    /// `args` the query is sent as is, so `%` needs no escaping.
    #[pyo3(signature = (query, args = None))]
    fn execute(
        &self,
        py: Python<'_>,
        query: &Bound<'_, PyString>,
        args: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<Py<PyAny>> {
        let template = query.to_str()?.as_bytes();
        let sql: Cow<'_, [u8]> = match args.filter(|a| !a.is_none()) {
            None => Cow::Borrowed(template),
            Some(args) => {
                let mut out = Vec::with_capacity(template.len() + 64);
                self.encoder()?
                    .interpolate(&mut out, template, &Args::from_py(args))?;
                Cow::Owned(out)
            }
        };
        let result = self.with_conn(py, |conn| conn.query(&sql))?;
        result_to_py(py, &result)
    }

    /// The query exactly as `execute` would send it, as `str` (bytes that
    /// are not UTF-8 come back as surrogate escapes).
    #[pyo3(signature = (query, args = None))]
    fn mogrify<'py>(
        &self,
        query: &Bound<'py, PyString>,
        args: Option<&Bound<'py, PyAny>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let Some(args) = args.filter(|a| !a.is_none()) else {
            return Ok(query.clone().into_any());
        };
        let template = query.to_str()?.as_bytes();
        let mut out = Vec::with_capacity(template.len() + 64);
        self.encoder()?
            .interpolate(&mut out, template, &Args::from_py(args))?;
        decode_lossless(query.py(), &out)
    }

    /// Batches a single-row `INSERT`/`REPLACE … VALUES (…)` into multi-row
    /// statements of at most `max_stmt_length` bytes (a row that is larger
    /// on its own still goes alone). Returns `(affected_rows,
    /// last_insert_id)`, `last_insert_id` being `None` when nothing ran, or
    /// `None` when the query cannot be batched and must run row by row.
    fn executemany(
        &self,
        py: Python<'_>,
        query: &Bound<'_, PyString>,
        seq_of_args: &Bound<'_, PyAny>,
        max_stmt_length: usize,
    ) -> PyResult<Option<(u64, Option<u64>)>> {
        let template = query.to_str()?.as_bytes();
        let Some(parts) = insert_values(template) else {
            return Ok(None);
        };
        let prefix_template = template.get(parts.prefix).unwrap_or_default();
        let row_template = template.get(parts.row).unwrap_or_default();
        let suffix_template = template.get(parts.suffix).unwrap_or_default();
        if has_placeholder(prefix_template) || has_placeholder(suffix_template) {
            return Ok(None);
        }
        let encoder = self.encoder()?;
        let prefix = unescape_percent(encoder, prefix_template)?;
        let suffix = unescape_percent(encoder, suffix_template)?;

        let mut sql = prefix.clone();
        let mut row = Vec::new();
        let mut rows_in_batch = 0usize;
        let mut totals = (0, None);
        for args in seq_of_args.try_iter()? {
            row.clear();
            encoder.interpolate(&mut row, row_template, &Args::from_py(&args?))?;
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
        if rows_in_batch > 0 {
            self.flush_batch(py, &mut sql, &suffix, prefix.len(), &mut totals)?;
        }
        Ok(Some(totals))
    }

    /// Next result of a multi-result query, or `None` when there is none.
    fn next_result(&self, py: Python<'_>) -> PyResult<Option<Py<PyAny>>> {
        self.with_conn(py, Conn::next_result)?
            .map(|result| result_to_py(py, &result))
            .transpose()
    }

    fn ping(&self, py: Python<'_>) -> PyResult<()> {
        self.with_conn(py, Conn::ping)
    }

    fn select_db(&self, py: Python<'_>, database: &str) -> PyResult<()> {
        self.with_conn(py, |conn| conn.select_db(database))
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
        // A failed COM_CHANGE_USER makes MySQL drop the connection while
        // MariaDB keeps the old user: treat the connection as gone on every
        // error so behaviour does not depend on the server.
        self.run(
            py,
            |conn| conn.change_user(user, password, database),
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
            if let State::Open(conn) = std::mem::replace(&mut *state, State::Closed) {
                conn.close();
            }
            Ok(())
        })
        .map_err(|failure| self.failure_to_pyerr(failure))
    }

    /// Whether the connection is encrypted with TLS.
    #[getter]
    fn tls(&self) -> PyResult<bool> {
        self.peek(Conn::is_tls)
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
    ssl_mode = "preferred",
    ssl_ca = None,
    ssl_cert = None,
    ssl_key = None,
    server_public_key = None,
))]
#[expect(
    clippy::too_many_arguments,
    reason = "mirrors the keyword-only Python signature one to one"
)]
pub(crate) fn connect(
    py: Python<'_>,
    host: String,
    port: u16,
    user: String,
    password: String,
    database: Option<String>,
    unix_socket: Option<PathBuf>,
    connect_timeout: Option<f64>,
    read_timeout: Option<f64>,
    write_timeout: Option<f64>,
    client_flag: u32,
    ssl_mode: &str,
    ssl_ca: Option<PathBuf>,
    ssl_cert: Option<PathBuf>,
    ssl_key: Option<PathBuf>,
    server_public_key: Option<PathBuf>,
) -> PyResult<Connection> {
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
        tls: TlsOpts {
            mode,
            ca: ssl_ca,
            cert: ssl_cert,
            key: ssl_key,
        },
        server_public_key,
    };
    let conn = py
        .detach(|| Conn::connect(&opts))
        .map_err(|err| from_proto(err, Phase::Connect, &target))?;
    Ok(Connection {
        state: Mutex::new(State::Open(Box::new(conn))),
        target,
    })
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

/// Bytes as `str`, keeping non-UTF-8 bytes as surrogate escapes.
fn decode_lossless<'py>(py: Python<'py>, bytes: &[u8]) -> PyResult<Bound<'py, PyAny>> {
    PyBytes::new(py, bytes).call_method1("decode", ("utf-8", "surrogateescape"))
}

/// `value` as an SQL literal, for a UTF-8 connection.
#[pyfunction]
#[pyo3(signature = (value, no_backslash_escapes = false))]
pub(crate) fn literal<'py>(
    value: &Bound<'py, PyAny>,
    no_backslash_escapes: bool,
) -> PyResult<Bound<'py, PyAny>> {
    let encoder = Encoder {
        no_backslash_escapes,
        binary: BinaryLiteral::Escaped,
    };
    let mut out = Vec::new();
    encoder.literal(&mut out, value)?;
    decode_lossless(value.py(), &out)
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
    let encoder = Encoder {
        no_backslash_escapes,
        binary: BinaryLiteral::Escaped,
    };
    let template = query.to_str()?.as_bytes();
    let mut out = Vec::with_capacity(template.len() + 64);
    encoder.interpolate(&mut out, template, &Args::from_py(args))?;
    decode_lossless(query.py(), &out)
}
