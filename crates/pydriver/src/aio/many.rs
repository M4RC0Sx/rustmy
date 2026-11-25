//! `executemany` on the event loop. The items are grouped as the blocking
//! connection groups them ([`crate::batch`]) and each group runs as a
//! command of its own: Python awaits once per group, not once per item,
//! and the items are taken from `seq_of_args` only as the groups are
//! built, so a generator works as with the blocking connection.

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::PoisonError;
use std::sync::TryLockError;

use pyo3::prelude::*;
use pyo3::types::PyBytes;
use pyo3::types::PyIterator;
use pyo3::types::PyString;
use rustmy_proto::Execute;
use rustmy_proto::PipelineTotals;
use rustmy_proto::QueryResult;

use super::AioConnection;
use super::Failure;
use super::busy_error;
use super::none;
use super::read_result;
use super::stream_head;
use crate::batch::InsertBatch;
use crate::batch::Pipeline;
use crate::batch::PreparedInsertBatch;
use crate::batch::PreparedPipeline;
use crate::batch::PreparedRun;
use crate::batch::Window;

/// Most bytes of statements in a pipelined window. The loop's transport
/// keeps what the socket cannot take yet, so no size can deadlock here;
/// the windows are as large as over the blocking connection's TCP, which
/// also bounds what has run when a statement fails.
const PIPELINE_BUDGET: usize = 16 * 1024;

/// How the cursor runs its statements.
#[derive(Debug, Clone, Copy)]
pub(super) enum Mode {
    Text,
    Prepared,
    /// As text, the last statement's rows left on the wire.
    Streamed,
}

enum Builder {
    Insert(InsertBatch),
    PreparedInsert(PreparedInsertBatch),
    Pipeline(Pipeline),
    PreparedPipeline(PreparedPipeline, Arc<[u8]>),
}

/// A group that runs before the last one.
enum Group {
    Statement(Vec<u8>),
    Prepared(PreparedRun),
    Window(Window),
    PreparedWindow(Arc<[u8]>, Vec<Execute>),
}

/// The last group of a pipelined batch: its last statement's result is the
/// cursor's.
enum Tail {
    Window(Window),
    PreparedWindow(Arc<[u8]>, Vec<Execute>, Execute),
}

enum Last {
    Group(Group),
    Tail(Tail),
}

impl Builder {
    fn add(&mut self, py: Python<'_>, args: &Bound<'_, PyAny>) -> PyResult<Option<Group>> {
        Ok(match self {
            Self::Insert(batch) => batch.add(py, args)?.map(Group::Statement),
            Self::PreparedInsert(batch) => batch.add(py, args)?.map(Group::Prepared),
            Self::Pipeline(pipeline) => pipeline.add(py, args)?.map(Group::Window),
            Self::PreparedPipeline(pipeline, sql) => pipeline
                .add(py, args)?
                .map(|window| Group::PreparedWindow(Arc::clone(sql), window)),
        })
    }

    /// What still runs when an item fails: a pipeline's window, as in a
    /// loop of `execute()` calls; a batched `INSERT` runs nothing more.
    fn pending(&mut self) -> Option<Group> {
        match self {
            Self::Insert(_) | Self::PreparedInsert(_) => None,
            Self::Pipeline(pipeline) => {
                let window = pipeline.take();
                (window.len() > 0).then_some(Group::Window(window))
            }
            Self::PreparedPipeline(pipeline, sql) => {
                let window = pipeline.take();
                (!window.is_empty()).then(|| Group::PreparedWindow(Arc::clone(sql), window))
            }
        }
    }

    /// The last group and its last statement as `mogrify` shows it, or
    /// `None` when nothing is left.
    fn finish(&mut self, py: Python<'_>) -> PyResult<Option<(Last, Vec<u8>)>> {
        Ok(match self {
            Self::Insert(batch) => batch
                .finish()
                .map(|sql| (sql.clone(), Last::Group(Group::Statement(sql))))
                .map(|(shown, last)| (last, shown)),
            Self::PreparedInsert(batch) => batch
                .finish(py)?
                .map(|(run, shown)| (Last::Group(Group::Prepared(run)), shown)),
            Self::Pipeline(pipeline) => {
                let window = pipeline.take();
                window
                    .statements()
                    .last()
                    .map(<[u8]>::to_vec)
                    .map(|shown| (Last::Tail(Tail::Window(window)), shown))
            }
            Self::PreparedPipeline(pipeline, sql) => {
                let mut window = pipeline.take();
                match (window.pop(), pipeline.shown_last(py)?) {
                    (Some(last), Some(shown)) => Some((
                        Last::Tail(Tail::PreparedWindow(Arc::clone(sql), window, last)),
                        shown,
                    )),
                    _ => None,
                }
            }
        })
    }
}

/// What the batch has done so far.
#[derive(Debug, Default)]
struct Totals {
    /// Affected rows (for a pipeline, of every statement but the last).
    rowcount: u64,
    last_insert_id: Option<u64>,
    /// The last statement, as `mogrify` shows it.
    statement: Option<Vec<u8>>,
}

fn lock_totals(totals: &Mutex<Totals>) -> std::sync::MutexGuard<'_, Totals> {
    totals.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Totals {
    fn add_result(&mut self, result: &QueryResult) {
        self.rowcount += result.affected_rows;
        self.last_insert_id = Some(result.last_insert_id);
    }

    /// A window's replies, all read: its first server error, if any.
    fn add_window(&mut self, window: PipelineTotals) -> Result<(), Failure> {
        if let Some((_, err)) = window.error {
            return Err(err.into());
        }
        self.rowcount += window.rowcount;
        Ok(())
    }
}

struct State {
    builder: Builder,
    /// `None` once every item is taken (or one failed).
    items: Option<Py<PyIterator>>,
    /// An item's error, raised once the window before it has run.
    error: Option<PyErr>,
}

/// An `executemany` in progress: `step()` takes items until a group is
/// complete and starts it, returning the command's future (the last one
/// gives the last statement's result, as `execute` does), or `None` once
/// every item has run. The totals are read when it ends.
#[pyclass(module = "rustmy._core", frozen)]
pub(crate) struct AioMany {
    conn: Py<AioConnection>,
    /// The statement is a batched `INSERT`: no result to read.
    batched: bool,
    as_dict: bool,
    streamed: bool,
    state: Mutex<State>,
    totals: Arc<Mutex<Totals>>,
}

/// Starts grouping `seq_of_args` for `query`: as a batched `INSERT` when it
/// is one (before reading any item), else pipelined.
pub(super) fn executemany(
    slf: &Bound<'_, AioConnection>,
    query: &Bound<'_, PyString>,
    seq_of_args: &Bound<'_, PyAny>,
    max_stmt_length: usize,
    max_pipeline: usize,
    as_dict: bool,
    mode: Mode,
) -> PyResult<AioMany> {
    let py = slf.py();
    let encoder = slf.get().encoder()?;
    let template = query.to_str()?;
    let batched = match mode {
        Mode::Prepared => PreparedInsertBatch::new(py, encoder.clone(), template, max_stmt_length)?
            .map(Builder::PreparedInsert),
        Mode::Text | Mode::Streamed => {
            InsertBatch::new(py, encoder.clone(), template, max_stmt_length)?.map(Builder::Insert)
        }
    };
    let builder = if let Some(builder) = batched {
        builder
    } else {
        let prepared = match mode {
            Mode::Prepared => {
                PreparedPipeline::new(py, encoder.clone(), template, max_pipeline, PIPELINE_BUDGET)?
            }
            Mode::Text | Mode::Streamed => None,
        };
        match prepared {
            Some(pipeline) => {
                let sql = Arc::from(pipeline.sql());
                Builder::PreparedPipeline(pipeline, sql)
            }
            // Without placeholders, a prepared cursor's statement runs
            // as text.
            None => Builder::Pipeline(Pipeline::new(
                encoder,
                template,
                max_pipeline,
                PIPELINE_BUDGET,
            )),
        }
    };
    Ok(AioMany {
        conn: slf.clone().unbind(),
        batched: matches!(builder, Builder::Insert(_) | Builder::PreparedInsert(_)),
        as_dict,
        streamed: matches!(mode, Mode::Streamed),
        state: Mutex::new(State {
            builder,
            items: Some(seq_of_args.try_iter()?.unbind()),
            error: None,
        }),
        totals: Arc::default(),
    })
}

impl AioMany {
    /// Starts a group that runs before the last one (or a batched
    /// `INSERT`'s last statement): its future gives `None`.
    fn run(&self, py: Python<'_>, group: Group) -> PyResult<Py<PyAny>> {
        let totals = Arc::clone(&self.totals);
        self.conn.get().value_command(py, move |client, io| {
            Box::pin(async move {
                match group {
                    Group::Statement(sql) => {
                        let result = client.query(io, &sql).await?;
                        lock_totals(&totals).add_result(&result);
                    }
                    Group::Prepared(run) => {
                        let result = client.execute_cached(io, &run.sql, &run.params).await?;
                        lock_totals(&totals).add_result(&result);
                    }
                    Group::Window(window) => {
                        let replies = client.pipeline(io, window.statements()).await?;
                        lock_totals(&totals).add_window(replies)?;
                    }
                    Group::PreparedWindow(sql, window) => {
                        let replies = client.execute_many_cached(io, &sql, &window).await?;
                        lock_totals(&totals).add_window(replies)?;
                    }
                }
                Ok(none())
            })
        })
    }

    /// Starts a pipeline's last window: its future gives the last
    /// statement's result, as `execute` (or `execute_streamed`) does.
    fn run_tail(&self, py: Python<'_>, tail: Tail) -> PyResult<Py<PyAny>> {
        let this = self.conn.get();
        let codec = this.codec();
        let custom = this.custom();
        let counters = Arc::clone(&this.counters);
        let conn = self.conn.clone_ref(py);
        let totals = Arc::clone(&self.totals);
        let (as_dict, streamed) = (self.as_dict, self.streamed);
        this.value_command(py, move |client, io| {
            Box::pin(async move {
                let (before, head, id) = match &tail {
                    Tail::Window(window) => {
                        let others = window.statements().take(window.len() - 1);
                        let last = window.statements().last().unwrap_or_default();
                        client.pipeline_streamed(io, others, last).await?
                    }
                    Tail::PreparedWindow(sql, window, last) => {
                        client
                            .execute_many_cached_streamed(io, sql, window, last)
                            .await?
                    }
                };
                lock_totals(&totals).rowcount += before.rowcount;
                if streamed {
                    Python::attach(|py| {
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
                    .map_err(Failure::Python)
                } else {
                    read_result(client, io, head, id, as_dict, &codec, custom.as_deref()).await
                }
            })
        })
    }
}

#[pymethods]
impl AioMany {
    /// Takes items until a group is complete and starts it: the future of
    /// that command, or `None` once every item has run. An item that fails
    /// (to be taken, or to be written) raises here, after the window built
    /// before it has run.
    fn step(&self, py: Python<'_>) -> PyResult<Option<Py<PyAny>>> {
        let mut guard = match self.state.try_lock() {
            Ok(guard) => guard,
            Err(TryLockError::WouldBlock) => return Err(busy_error()),
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
        };
        let state = &mut *guard;
        // Before taking any item: a group that could not start would be
        // lost.
        self.conn.get().peek(|_| ())?;
        if let Some(err) = state.error.take() {
            return Err(err);
        }
        let Some(items) = state.items.as_ref().map(|items| items.clone_ref(py)) else {
            return Ok(None);
        };
        let mut items = items.into_bound(py);
        loop {
            let Some(item) = items.next() else {
                state.items = None;
                let Some((last, shown)) = state.builder.finish(py)? else {
                    return Ok(None);
                };
                lock_totals(&self.totals).statement = Some(shown);
                return match last {
                    Last::Group(group) => self.run(py, group),
                    Last::Tail(tail) => self.run_tail(py, tail),
                }
                .map(Some);
            };
            match item.and_then(|args| state.builder.add(py, &args)) {
                Ok(Some(group)) => return self.run(py, group).map(Some),
                Ok(None) => {}
                Err(err) => {
                    state.items = None;
                    let Some(group) = state.builder.pending() else {
                        return Err(err);
                    };
                    state.error = Some(err);
                    return self.run(py, group).map(Some);
                }
            }
        }
    }

    /// Whether the statement was batched into multi-row `INSERT`s.
    #[getter]
    const fn batched(&self) -> bool {
        self.batched
    }

    /// Affected rows: of every statement for a batched `INSERT`, of every
    /// statement but the last for a pipeline.
    #[getter]
    fn rowcount(&self) -> u64 {
        lock_totals(&self.totals).rowcount
    }

    /// `LAST_INSERT_ID()` after a batched `INSERT`'s last statement.
    #[getter]
    fn lastrowid(&self) -> Option<u64> {
        lock_totals(&self.totals).last_insert_id
    }

    /// The last statement sent, as `mogrify` shows it, or `None` when
    /// there were no items.
    #[getter]
    fn statement<'py>(&self, py: Python<'py>) -> Option<Bound<'py, PyBytes>> {
        lock_totals(&self.totals)
            .statement
            .as_deref()
            .map(|statement| PyBytes::new(py, statement))
    }
}
