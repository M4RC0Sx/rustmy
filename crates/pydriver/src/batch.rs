//! How `executemany` groups its items into what goes to the server, without
//! I/O: each builder takes the items one by one and hands back a group as
//! soon as it is complete. The blocking connection runs each group as it
//! comes; the asyncio one awaits each, so in both the parameters (a
//! generator, say) may use the connection between groups, and an error
//! stops the batch at the end of its group.

use pyo3::prelude::*;
use rustmy_proto::Execute;
use rustmy_proto::sql::insert_values;
use rustmy_proto::sql::mentions_local_infile;

use crate::params::Args;
use crate::params::Encoder;
use crate::params::PreparedSql;
use crate::params::has_placeholder;
use crate::params::unescape_percent;

/// Statements of one pipelined window, back to back.
#[derive(Debug, Default)]
pub(crate) struct Window {
    bytes: Vec<u8>,
    ends: Vec<usize>,
}

impl Window {
    pub(crate) const fn len(&self) -> usize {
        self.ends.len()
    }

    fn push(&mut self, statement: &[u8]) {
        self.bytes.extend_from_slice(statement);
        self.ends.push(self.bytes.len());
    }

    pub(crate) fn statements(&self) -> impl Iterator<Item = &[u8]> + Send {
        let starts = std::iter::once(0).chain(self.ends.iter().copied());
        starts
            .zip(self.ends.iter().copied())
            .map(|(start, end)| self.bytes.get(start..end).unwrap_or_default())
    }
}

/// A single-row `INSERT`/`REPLACE … VALUES (…)` batched into multi-row
/// statements of at most `max_stmt_length` bytes (a row that is larger on
/// its own still goes alone).
#[derive(Debug)]
pub(crate) struct InsertBatch {
    encoder: Encoder,
    row_template: String,
    prefix_len: usize,
    suffix: Vec<u8>,
    sql: Vec<u8>,
    row: Vec<u8>,
    rows: usize,
    max_stmt_length: usize,
}

impl InsertBatch {
    /// `None` when `template` cannot be batched (it runs row by row).
    pub(crate) fn new(
        py: Python<'_>,
        encoder: Encoder,
        template: &str,
        max_stmt_length: usize,
    ) -> PyResult<Option<Self>> {
        let Some(parts) = insert_values(template.as_bytes()) else {
            return Ok(None);
        };
        // The parts split at ASCII tokens, hence at character boundaries;
        // anything else runs row by row.
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
        let sql = unescape_percent(&encoder, py, prefix)?;
        let suffix = unescape_percent(&encoder, py, suffix)?;
        Ok(Some(Self {
            row_template: row.to_owned(),
            prefix_len: sql.len(),
            suffix,
            sql,
            row: Vec::new(),
            rows: 0,
            max_stmt_length,
            encoder,
        }))
    }

    /// Adds a row; returns the statement to run first when the row does
    /// not fit in the one being built.
    pub(crate) fn add(
        &mut self,
        py: Python<'_>,
        args: &Bound<'_, PyAny>,
    ) -> PyResult<Option<Vec<u8>>> {
        self.row.clear();
        self.encoder
            .interpolate(py, &mut self.row, &self.row_template, &Args::from_py(args))?;
        let mut full = None;
        if self.rows > 0
            && self.sql.len() + 1 + self.row.len() + self.suffix.len() > self.max_stmt_length
        {
            full = Some(self.take());
        }
        if self.rows > 0 {
            self.sql.push(b',');
        }
        self.sql.extend_from_slice(&self.row);
        self.rows += 1;
        Ok(full)
    }

    /// The last statement, if any row is left.
    pub(crate) fn finish(&mut self) -> Option<Vec<u8>> {
        (self.rows > 0).then(|| self.take())
    }

    fn take(&mut self) -> Vec<u8> {
        let mut statement = self.sql.clone();
        statement.extend_from_slice(&self.suffix);
        self.sql.truncate(self.prefix_len);
        self.rows = 0;
        statement
    }
}

/// Any other statement, run once per item: windows of up to
/// `max_pipeline` statements and `budget` bytes, pipelined.
#[derive(Debug)]
pub(crate) struct Pipeline {
    encoder: Encoder,
    template: String,
    /// Most statements per window.
    max_window: usize,
    budget: usize,
    window: Window,
    statement: Vec<u8>,
}

impl Pipeline {
    pub(crate) fn new(
        encoder: Encoder,
        template: &str,
        max_pipeline: usize,
        budget: usize,
    ) -> Self {
        // The server reads whatever follows a LOAD DATA LOCAL as its file:
        // one statement per window.
        let max_pipeline = if mentions_local_infile(template.as_bytes()) {
            1
        } else {
            max_pipeline.max(1)
        };
        Self {
            encoder,
            template: template.to_owned(),
            max_window: max_pipeline,
            budget,
            window: Window::default(),
            statement: Vec::new(),
        }
    }

    /// Adds an item; returns the window to run first when it is full. On
    /// an error, the window built so far ([`Pipeline::take`]) still runs,
    /// as in a loop of `execute()` calls.
    pub(crate) fn add(
        &mut self,
        py: Python<'_>,
        args: &Bound<'_, PyAny>,
    ) -> PyResult<Option<Window>> {
        self.statement.clear();
        self.encoder.interpolate(
            py,
            &mut self.statement,
            &self.template,
            &Args::from_py(args),
        )?;
        let window = &self.window;
        let full = (window.len() > 0
            && (window.len() >= self.max_window
                || window.bytes.len() + self.statement.len() > self.budget))
            .then(|| self.take());
        self.window.push(&self.statement);
        Ok(full)
    }

    /// The window built so far.
    pub(crate) fn take(&mut self) -> Window {
        std::mem::take(&mut self.window)
    }
}

/// A single-row `INSERT`/`REPLACE … VALUES (…)` of a `PreparedCursor`:
/// multi-row prepared statements (`VALUES (?, ?), (?, ?)…`) with the
/// parameters still typed. Every statement has the same number of rows
/// (from the first row's size, within `max_stmt_length` and the server's
/// 65,535 placeholders), so it is prepared once; a batch is cut short when
/// a row is larger than that size allows, and the last one holds what is
/// left.
#[derive(Debug)]
pub(crate) struct PreparedInsertBatch {
    encoder: Encoder,
    prefix: String,
    row: String,
    suffix: String,
    row_sql: PreparedSql,
    prefix_sql: Vec<u8>,
    suffix_sql: Vec<u8>,
    most_rows: usize,
    rows_per_statement: Option<usize>,
    batch: Execute,
    batch_args: Vec<Py<PyAny>>,
    max_stmt_length: usize,
}

/// A prepared statement and its parameters, to run.
#[derive(Debug)]
pub(crate) struct PreparedRun {
    pub(crate) sql: Vec<u8>,
    pub(crate) params: Execute,
}

impl PreparedInsertBatch {
    /// `None` when `template` cannot be batched.
    pub(crate) fn new(
        py: Python<'_>,
        encoder: Encoder,
        template: &str,
        max_stmt_length: usize,
    ) -> PyResult<Option<Self>> {
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
        let row_sql = encoder.prepared_sql(py, row)?;
        if row_sql.slots.is_empty() {
            return Ok(None);
        }
        let prefix_sql = encoder.prepared_sql(py, prefix)?.sql;
        let suffix_sql = encoder.prepared_sql(py, suffix)?.sql;
        let most_rows = (usize::from(u16::MAX) / row_sql.slots.len()).max(1);
        Ok(Some(Self {
            prefix: prefix.to_owned(),
            row: row.to_owned(),
            suffix: suffix.to_owned(),
            row_sql,
            prefix_sql,
            suffix_sql,
            most_rows,
            rows_per_statement: None,
            batch: Execute::default(),
            batch_args: Vec::new(),
            max_stmt_length,
            encoder,
        }))
    }

    fn statement(&self, rows: usize) -> Vec<u8> {
        let mut sql = self.prefix_sql.clone();
        for i in 0..rows {
            if i > 0 {
                sql.push(b',');
            }
            sql.extend_from_slice(&self.row_sql.sql);
        }
        sql.extend_from_slice(&self.suffix_sql);
        sql
    }

    fn take(&mut self) -> PreparedRun {
        let run = PreparedRun {
            sql: self.statement(self.batch_args.len()),
            params: std::mem::take(&mut self.batch),
        };
        self.batch_args.clear();
        run
    }

    /// Adds a row; returns the statement to run first when the row does
    /// not fit in the one being built.
    pub(crate) fn add(
        &mut self,
        py: Python<'_>,
        args: &Bound<'_, PyAny>,
    ) -> PyResult<Option<PreparedRun>> {
        let mut values = Execute::default();
        self.encoder
            .bind(py, &mut values, &self.row_sql.slots, &Args::from_py(args))?;
        let (max_stmt_length, most_rows) = (self.max_stmt_length, self.most_rows);
        let limit = *self.rows_per_statement.get_or_insert_with(|| {
            (max_stmt_length / values.encoded_len().max(1)).clamp(1, most_rows)
        });
        let mut full = None;
        if !self.batch_args.is_empty()
            && (self.batch_args.len() >= limit
                || self.batch.encoded_len() + values.encoded_len() > self.max_stmt_length)
        {
            full = Some(self.take());
        }
        self.batch.append(&values);
        self.batch_args.push(args.clone().unbind());
        Ok(full)
    }

    /// The last statement, if any row is left, with that statement as
    /// `mogrify` shows it (for `_executed`).
    pub(crate) fn finish(&mut self, py: Python<'_>) -> PyResult<Option<(PreparedRun, Vec<u8>)>> {
        if self.batch_args.is_empty() {
            return Ok(None);
        }
        let mut shown = unescape_percent(&self.encoder, py, &self.prefix)?;
        for (i, args) in self.batch_args.iter().enumerate() {
            if i > 0 {
                shown.push(b',');
            }
            self.encoder
                .interpolate(py, &mut shown, &self.row, &Args::from_py(args.bind(py)))?;
        }
        shown.extend_from_slice(&unescape_percent(&self.encoder, py, &self.suffix)?);
        Ok(Some((self.take(), shown)))
    }
}

/// A `PreparedCursor`'s statement run once per item, pipelined as
/// [`Pipeline`] does.
#[derive(Debug)]
pub(crate) struct PreparedPipeline {
    encoder: Encoder,
    template: String,
    prepared: PreparedSql,
    max_pipeline: usize,
    budget: usize,
    window: Vec<Execute>,
    window_bytes: usize,
    last_args: Option<Py<PyAny>>,
}

impl PreparedPipeline {
    /// `None` when the statement has no placeholders (it runs as text).
    pub(crate) fn new(
        py: Python<'_>,
        encoder: Encoder,
        template: &str,
        max_pipeline: usize,
        budget: usize,
    ) -> PyResult<Option<Self>> {
        let prepared = encoder.prepared_sql(py, template)?;
        if prepared.slots.is_empty() {
            return Ok(None);
        }
        Ok(Some(Self {
            encoder,
            template: template.to_owned(),
            prepared,
            max_pipeline: max_pipeline.max(1),
            budget,
            window: Vec::new(),
            window_bytes: 0,
            last_args: None,
        }))
    }

    /// The statement, with `?` placeholders.
    pub(crate) fn sql(&self) -> &[u8] {
        &self.prepared.sql
    }

    /// Adds an item; returns the window to run first when it is full. On
    /// an error, the window built so far ([`PreparedPipeline::take`]) still
    /// runs.
    pub(crate) fn add(
        &mut self,
        py: Python<'_>,
        args: &Bound<'_, PyAny>,
    ) -> PyResult<Option<Vec<Execute>>> {
        let mut params = Execute::default();
        self.encoder
            .bind(py, &mut params, &self.prepared.slots, &Args::from_py(args))?;
        let full = (!self.window.is_empty()
            && (self.window.len() >= self.max_pipeline
                || self.window_bytes + params.encoded_len() > self.budget))
            .then(|| self.take());
        self.window_bytes += params.encoded_len();
        self.window.push(params);
        self.last_args = Some(args.clone().unbind());
        Ok(full)
    }

    /// The window built so far.
    pub(crate) fn take(&mut self) -> Vec<Execute> {
        self.window_bytes = 0;
        std::mem::take(&mut self.window)
    }

    /// The last item as `mogrify` shows it (for `_executed`).
    pub(crate) fn shown_last(&self, py: Python<'_>) -> PyResult<Option<Vec<u8>>> {
        let Some(args) = &self.last_args else {
            return Ok(None);
        };
        let mut shown = Vec::new();
        self.encoder.interpolate(
            py,
            &mut shown,
            &self.template,
            &Args::from_py(args.bind(py)),
        )?;
        Ok(Some(shown))
    }
}
