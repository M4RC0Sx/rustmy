//! `rustmy._core.PoolCore`: the pools' accounts ([`rustmy_proto::pool`])
//! for the Python facades (`rustmy.Pool`, `rustmy.aio.create_pool`), with
//! the time taken here.

use std::sync::Mutex;
use std::sync::MutexGuard;
use std::sync::PoisonError;
use std::time::Duration;
use std::time::Instant;

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use rustmy_proto::pool;
use rustmy_proto::pool::Acquire;
use rustmy_proto::pool::Effects;
use rustmy_proto::pool::Grant;
use rustmy_proto::pool::PoolConfig;

/// `(close, grants, open, failed)`: the connections to close, the waiters
/// that may go on (`(ticket, id)`, `id` `None` for room to open one), how
/// many to open for the minimum, and the waiters to fail.
type PyEffects = (Vec<u64>, Vec<(u64, Option<u64>)>, usize, Vec<u64>);

fn to_py(effects: Effects) -> PyEffects {
    let grants = effects
        .grants
        .into_iter()
        .map(|grant| match grant {
            Grant::Lend { ticket, id } => (ticket, Some(id)),
            Grant::Open { ticket } => (ticket, None),
        })
        .collect();
    (effects.close, grants, effects.open, effects.failed)
}

/// What `acquire` decided: `LEND` (the id, and whether to ping it first),
/// `OPEN`, `WAIT` (the ticket) or `CLOSED`.
const LEND: u8 = 0;
const OPEN: u8 = 1;
const WAIT: u8 = 2;
const CLOSED: u8 = 3;

fn duration(seconds: Option<f64>, name: &str) -> PyResult<Option<Duration>> {
    seconds
        .map(|seconds| {
            Duration::try_from_secs_f64(seconds).map_err(|_| {
                PyValueError::new_err(format!("{name} must be a non-negative number of seconds"))
            })
        })
        .transpose()
}

#[pyclass(module = "rustmy._core", name = "PoolCore", frozen)]
pub(crate) struct PyPoolCore(Mutex<pool::PoolCore>);

impl PyPoolCore {
    fn lock(&self) -> MutexGuard<'_, pool::PoolCore> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[pymethods]
impl PyPoolCore {
    #[new]
    #[pyo3(signature = (minsize, maxsize, max_lifetime = None, idle_timeout = None, pre_ping = None))]
    fn new(
        minsize: usize,
        maxsize: usize,
        max_lifetime: Option<f64>,
        idle_timeout: Option<f64>,
        pre_ping: Option<f64>,
    ) -> PyResult<Self> {
        let config = PoolConfig {
            min: minsize,
            max: maxsize,
            max_lifetime: duration(max_lifetime, "pool_recycle")?,
            idle_timeout: duration(idle_timeout, "idle_timeout")?,
            pre_ping: duration(pre_ping, "pre_ping")?,
        };
        pool::PoolCore::new(config)
            .map(|core| Self(Mutex::new(core)))
            .map_err(PyValueError::new_err)
    }

    fn start(&self) -> PyEffects {
        to_py(self.lock().start())
    }

    /// `(kind, value, ping, effects)`: see `LEND`, `OPEN`, `WAIT` and
    /// `CLOSED`.
    fn acquire(&self) -> (u8, u64, bool, PyEffects) {
        let (acquire, effects) = self.lock().acquire(Instant::now());
        let (kind, value, ping) = match acquire {
            Acquire::Lend { id, ping } => (LEND, id, ping),
            Acquire::Open => (OPEN, 0, false),
            Acquire::Wait(ticket) => (WAIT, ticket, false),
            Acquire::Closed => (CLOSED, 0, false),
        };
        (kind, value, ping, to_py(effects))
    }

    fn opened(&self) -> u64 {
        self.lock().opened(Instant::now())
    }

    fn idle_opened(&self) -> (u64, PyEffects) {
        let (id, effects) = self.lock().idle_opened(Instant::now());
        (id, to_py(effects))
    }

    fn open_failed(&self) -> PyEffects {
        to_py(self.lock().open_failed())
    }

    #[pyo3(signature = (conn_id, reuse, /))]
    fn release(&self, conn_id: u64, reuse: bool) -> PyEffects {
        to_py(self.lock().release(conn_id, Instant::now(), reuse))
    }

    fn cancel(&self, ticket: u64) -> bool {
        self.lock().cancel(ticket)
    }

    fn close(&self) -> PyEffects {
        to_py(self.lock().close())
    }

    fn clear(&self) -> PyEffects {
        to_py(self.lock().clear())
    }

    fn expire_idle(&self) -> PyEffects {
        to_py(self.lock().expire_idle(Instant::now()))
    }

    #[getter]
    fn size(&self) -> usize {
        self.lock().size()
    }

    #[getter]
    fn freesize(&self) -> usize {
        self.lock().idle_count()
    }

    #[getter]
    fn waiting(&self) -> usize {
        self.lock().waiting_count()
    }

    #[getter]
    fn minsize(&self) -> usize {
        self.lock().config().min
    }

    #[getter]
    fn maxsize(&self) -> usize {
        self.lock().config().max
    }

    #[getter]
    fn closed(&self) -> bool {
        self.lock().is_closed()
    }

    #[getter]
    fn drained(&self) -> bool {
        self.lock().is_drained()
    }

    #[classattr]
    const LEND: u8 = LEND;
    #[classattr]
    const OPEN: u8 = OPEN;
    #[classattr]
    const WAIT: u8 = WAIT;
    #[classattr]
    const CLOSED: u8 = CLOSED;
}
