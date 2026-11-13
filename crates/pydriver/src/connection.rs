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

use crate::convert::result_to_py;
use crate::errors::InterfaceError;
use crate::errors::OperationalError;
use crate::errors::Phase;
use crate::errors::ProgrammingError;
use crate::errors::client_code;
use crate::errors::from_proto;

#[derive(Debug)]
enum State {
    Open(Conn),
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
                if matches!(err, ProtoError::Io(_) | ProtoError::Protocol(_)) {
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

    /// Next result of a multi-result query, or `None` when there is none.
    fn next_result(&self, py: Python<'_>) -> PyResult<Option<Py<PyAny>>> {
        self.with_conn(py, Conn::next_result)?
            .map(|result| result_to_py(py, &result))
            .transpose()
    }

    fn ping(&self, py: Python<'_>) -> PyResult<()> {
        self.with_conn(py, Conn::ping)
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
) -> PyResult<Connection> {
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
    };
    let conn = py
        .detach(|| Conn::connect(&opts))
        .map_err(|err| from_proto(err, Phase::Connect, &target))?;
    Ok(Connection {
        state: Mutex::new(State::Open(conn)),
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
