//! `rustmy._core.ResultStream`: the rows of a streamed result set, read from
//! the socket in batches with the GIL released and converted batch by batch.
//!
//! The connection's lock is taken only while a batch is read, never while
//! Python code runs, so other commands may run between batches: they discard
//! the rows still on the wire. The stream notices through the connection's
//! command generation and refuses to hand out anything more, so a result set
//! is either complete or fails loudly, never silently truncated.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::MutexGuard;
use std::sync::TryLockError;

use pyo3::prelude::*;
use pyo3::types::PyTuple;
use rustmy_proto::ReadOutcome;
use rustmy_proto::RowSet;
use rustmy_proto::StreamId;

use crate::connection::Connection;
use crate::connection::Failure;
use crate::convert::Converter;
use crate::errors::InterfaceError;
use crate::errors::ProgrammingError;

/// Why a stream stopped handing out rows before the end of its result set.
#[derive(Debug, Clone, Copy)]
enum Abort {
    /// Another command discarded the rows still on the wire.
    Discarded,
    /// A row could not be converted; the rest of the result set is dropped.
    Conversion,
}

#[derive(Debug)]
struct State {
    id: StreamId,
    /// Connection generation the result set belongs to.
    generation: u64,
    converter: Converter,
    /// Raw rows of the batch being converted (kept for its allocations).
    raw: RowSet,
    /// Converted rows not handed out yet.
    batch: VecDeque<Py<PyAny>>,
    batch_bytes: usize,
    /// The terminator (or an error) has been read: no rows left on the wire.
    done: bool,
    /// Error that ended the result set, raised once the rows read before it
    /// have been handed out.
    error: Option<PyErr>,
    abort: Option<Abort>,
    /// Rows handed out or skipped so far.
    rownumber: usize,
}

#[pyclass(frozen, module = "rustmy._core")]
#[derive(Debug)]
pub(crate) struct ResultStream {
    conn: Py<Connection>,
    state: Mutex<State>,
}

impl ResultStream {
    pub(crate) fn new(
        conn: Py<Connection>,
        id: StreamId,
        generation: u64,
        converter: Converter,
        raw: RowSet,
        batch_bytes: usize,
    ) -> Self {
        Self {
            conn,
            state: Mutex::new(State {
                id,
                generation,
                converter,
                raw,
                batch: VecDeque::new(),
                batch_bytes: batch_bytes.max(1),
                done: false,
                error: None,
                abort: None,
                rownumber: 0,
            }),
        }
    }

    fn lock(&self) -> PyResult<MutexGuard<'_, State>> {
        match self.state.try_lock() {
            Ok(guard) => Ok(guard),
            Err(TryLockError::WouldBlock) => Err(ProgrammingError::new_err((
                0,
                "result set is in use by another thread",
            ))),
            Err(TryLockError::Poisoned(poisoned)) => Ok(poisoned.into_inner()),
        }
    }

    /// Hands out up to `size` rows; an error that ends the result set is
    /// raised only once no rows were collected, so none are lost.
    fn take(&self, py: Python<'_>, size: usize) -> PyResult<Vec<Py<PyAny>>> {
        let conn = self.conn.get();
        let mut state = self.lock()?;
        state.check(conn)?;
        let mut out = Vec::new();
        while out.len() < size {
            state.fill(py, conn, out.is_empty())?;
            let n = (size - out.len()).min(state.batch.len());
            if n == 0 {
                break;
            }
            out.extend(state.batch.drain(..n));
            state.rownumber += n;
        }
        state.finish_if_exhausted(conn);
        Ok(out)
    }
}

impl State {
    fn exhausted(&self) -> bool {
        self.done && self.batch.is_empty() && self.error.is_none()
    }

    /// Fails if the rows were discarded by another command (checked before
    /// every fetch, even with rows at hand, so the outcome never depends on
    /// the batch size) or the stream was aborted.
    fn check(&mut self, conn: &Connection) -> PyResult<()> {
        if let Some(abort) = self.abort {
            return Err(abort_error(abort));
        }
        if self.exhausted() || conn.check_stream(self.generation)? {
            return Ok(());
        }
        self.abort(conn, Abort::Discarded);
        Err(abort_error(Abort::Discarded))
    }

    fn abort(&mut self, conn: &Connection, abort: Abort) {
        self.abort = Some(abort);
        self.batch.clear();
        self.error = None;
        self.done = true;
        conn.stream_finished(self.generation);
    }

    fn finish_if_exhausted(&self, conn: &Connection) {
        if self.exhausted() {
            conn.stream_finished(self.generation);
        }
    }

    /// Reads batches until there are rows at hand or none are left. A
    /// pending error is raised only with `raise_error`.
    fn fill(&mut self, py: Python<'_>, conn: &Connection, raise_error: bool) -> PyResult<()> {
        while self.batch.is_empty() {
            if self.error.is_some() {
                if !raise_error {
                    return Ok(());
                }
                let err = self.error.take();
                conn.stream_finished(self.generation);
                return err.map_or(Ok(()), Err);
            }
            if self.done {
                return Ok(());
            }
            self.read_batch(py, conn)?;
        }
        Ok(())
    }

    /// Reads one batch with the GIL released, then converts it.
    fn read_batch(&mut self, py: Python<'_>, conn: &Connection) -> PyResult<()> {
        let (id, budget) = (self.id, self.batch_bytes);
        let raw = &mut self.raw;
        raw.clear();
        let outcome = conn.run_stream(py, self.generation, |c| c.read_rows(id, raw, budget));
        match outcome {
            Ok(ReadOutcome::More) => {}
            Ok(ReadOutcome::Done { .. }) => self.done = true,
            Err(Failure::Busy) => return Err(conn.failure_to_pyerr(Failure::Busy)),
            Err(Failure::Discarded) => {
                self.abort(conn, Abort::Discarded);
                return Err(abort_error(Abort::Discarded));
            }
            // A server error (or a lost connection) ends the result set;
            // the rows read before it are still handed out first.
            Err(failure) => {
                self.done = true;
                self.error = Some(conn.failure_to_pyerr(failure));
            }
        }
        let batch = &mut self.batch;
        let converted = self
            .converter
            .convert(py, &self.raw, |row| batch.push_back(row.unbind()));
        self.raw.clear();
        if let Err(err) = converted {
            self.abort(conn, Abort::Conversion);
            return Err(err);
        }
        Ok(())
    }
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

#[pymethods]
impl ResultStream {
    /// Next row, or `None` at the end of the result set.
    fn fetchone(&self, py: Python<'_>) -> PyResult<Option<Py<PyAny>>> {
        let conn = self.conn.get();
        let mut state = self.lock()?;
        state.check(conn)?;
        state.fill(py, conn, true)?;
        let row = state.batch.pop_front();
        if row.is_some() {
            state.rownumber += 1;
        }
        state.finish_if_exhausted(conn);
        Ok(row)
    }

    /// Up to `size` rows.
    fn fetchmany<'py>(&self, py: Python<'py>, size: usize) -> PyResult<Bound<'py, PyTuple>> {
        PyTuple::new(py, self.take(py, size)?)
    }

    /// Every remaining row.
    fn fetchall<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyTuple>> {
        PyTuple::new(py, self.take(py, usize::MAX)?)
    }

    /// Moves `count` rows forward without converting the ones still on the
    /// wire; returns how many were skipped (fewer at the end).
    fn skip(&self, py: Python<'_>, count: usize) -> PyResult<usize> {
        let conn = self.conn.get();
        let mut state = self.lock()?;
        state.check(conn)?;
        let in_batch = count.min(state.batch.len());
        state.batch.drain(..in_batch);
        let mut skipped = in_batch;
        if skipped < count && !state.done {
            let (id, generation) = (state.id, state.generation);
            let wanted = count - skipped;
            match conn.run_stream(py, generation, |c| c.skip_rows(id, wanted)) {
                Ok((n, outcome)) => {
                    skipped += n;
                    state.done = matches!(outcome, ReadOutcome::Done { .. });
                }
                Err(Failure::Busy) => return Err(conn.failure_to_pyerr(Failure::Busy)),
                Err(Failure::Discarded) => {
                    state.abort(conn, Abort::Discarded);
                    return Err(abort_error(Abort::Discarded));
                }
                Err(failure) => {
                    state.done = true;
                    state.error = Some(conn.failure_to_pyerr(failure));
                }
            }
        }
        state.rownumber += skipped;
        if skipped < count {
            state.fill(py, conn, true)?;
        }
        state.finish_if_exhausted(conn);
        Ok(skipped)
    }

    /// Drops the rest of the result set, reading what is still on the wire
    /// (with the GIL released) so the server can move on. Idempotent.
    fn close(&self, py: Python<'_>) -> PyResult<()> {
        let conn = self.conn.get();
        let mut state = self.lock()?;
        state.batch.clear();
        state.error = None;
        if !state.done {
            state.done = true;
            let (id, generation) = (state.id, state.generation);
            // Any failure here also shows on the next command (a server
            // error belongs to rows nobody will read), except contention.
            if let Err(Failure::Busy) =
                conn.run_stream(py, generation, |c| c.skip_rows(id, usize::MAX))
            {
                state.done = false;
                return Err(conn.failure_to_pyerr(Failure::Busy));
            }
        }
        conn.stream_finished(state.generation);
        Ok(())
    }

    /// Index of the next row: rows handed out or skipped so far.
    #[getter]
    fn rownumber(&self) -> PyResult<usize> {
        Ok(self.lock()?.rownumber)
    }

    fn __iter__(slf: Bound<'_, Self>) -> Bound<'_, Self> {
        slf
    }

    fn __next__(&self, py: Python<'_>) -> PyResult<Option<Py<PyAny>>> {
        self.fetchone(py)
    }
}
