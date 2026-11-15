//! Text-protocol values to Python objects.
//!
//! Each column's converter (and, for dict rows, each key) is chosen once per
//! result set; rows are then converted in a tight loop straight from the
//! contiguous row buffer. Byte parsing lives in `rustmy_proto::text`; this
//! module only builds objects.

use std::collections::HashSet;
use std::sync::Arc;

use pyo3::IntoPyObjectExt;
use pyo3::prelude::*;
use pyo3::sync::PyOnceLock;
use pyo3::types::PyBytes;
use pyo3::types::PyDate;
use pyo3::types::PyDateTime;
use pyo3::types::PyDelta;
use pyo3::types::PyDict;
use pyo3::types::PyList;
use pyo3::types::PyString;
use pyo3::types::PyTuple;
use pyo3::types::PyType;
use rustmy_proto::Column;
use rustmy_proto::QueryResult;
use rustmy_proto::RowSet;
use rustmy_proto::constants::ColumnType;
use rustmy_proto::constants::column_flag;
use rustmy_proto::text;

use crate::charset::Codec;
use crate::errors::DataError;

/// Python value a column is converted to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Int,
    UInt,
    Float,
    Decimal,
    Date,
    DateTime,
    Time,
    Text,
    Bytes,
    Null,
    /// A `conv=` decoder (index into `Converter::custom`), called with the
    /// cell as `str` (`text`) or `bytes`.
    Custom {
        index: usize,
        text: bool,
    },
}

/// `conv=` entries that differ from the defaults (see
/// `rustmy.converters.split_conv`).
#[derive(Debug)]
pub(crate) struct Custom {
    /// Column type to decoder, or to `[(flag mask, decoder or None)]`.
    pub(crate) decoders: Option<Py<PyDict>>,
    /// Python type to `encoder(value, conv) -> literal`.
    pub(crate) encoders: Option<Py<PyDict>>,
    /// The whole `conv=` mapping, second argument of every encoder.
    pub(crate) conv: Py<PyAny>,
}

/// Column types whose custom decoders get `str` (as in `MySQLdb`); the rest
/// get `bytes`.
fn decodes_from_text(column_type: ColumnType) -> bool {
    matches!(
        column_type,
        ColumnType::DECIMAL
            | ColumnType::NEWDECIMAL
            | ColumnType::TIMESTAMP
            | ColumnType::DATETIME
            | ColumnType::TIME
            | ColumnType::DATE
    )
}

/// The custom decoder for `column`, if any: a callable entry, or the first
/// `(mask, decoder)` pair whose mask matches the column's flags (or is not
/// an int). The binary flag follows the column's collation, as in `MySQLdb`.
fn custom_decoder<'py>(
    decoders: &Bound<'py, PyDict>,
    column: &Column,
) -> PyResult<Option<Bound<'py, PyAny>>> {
    let Some(entry) = decoders.get_item(column.column_type.0)? else {
        return Ok(None);
    };
    if entry.is_callable() {
        return Ok(Some(entry));
    }
    let mut flags = i64::from(column.flags);
    if column.is_binary() {
        flags |= i64::from(column_flag::BINARY);
    } else {
        flags &= !i64::from(column_flag::BINARY);
    }
    for pair in entry.try_iter()? {
        let (mask, decoder): (Bound<'py, PyAny>, Bound<'py, PyAny>) = pair?.extract()?;
        let matches = mask.extract::<i64>().map_or(true, |mask| mask & flags != 0);
        if matches {
            return Ok((!decoder.is_none()).then_some(decoder));
        }
    }
    Ok(None)
}

fn kind(column: &Column) -> Kind {
    match column.column_type {
        ColumnType::LONGLONG if column.is_unsigned() => Kind::UInt,
        ColumnType::TINY
        | ColumnType::SHORT
        | ColumnType::LONG
        | ColumnType::INT24
        | ColumnType::LONGLONG
        | ColumnType::YEAR => Kind::Int,
        ColumnType::FLOAT | ColumnType::DOUBLE => Kind::Float,
        ColumnType::DECIMAL | ColumnType::NEWDECIMAL => Kind::Decimal,
        ColumnType::DATE | ColumnType::NEWDATE => Kind::Date,
        ColumnType::DATETIME
        | ColumnType::DATETIME2
        | ColumnType::TIMESTAMP
        | ColumnType::TIMESTAMP2 => Kind::DateTime,
        ColumnType::TIME | ColumnType::TIME2 => Kind::Time,
        ColumnType::NULL => Kind::Null,
        ColumnType::BIT | ColumnType::GEOMETRY | ColumnType::VECTOR => Kind::Bytes,
        // JSON is always text, whatever collation the server reports.
        ColumnType::JSON => Kind::Text,
        _ if column.is_binary() => Kind::Bytes,
        _ => Kind::Text,
    }
}

static DECIMAL: PyOnceLock<Py<PyType>> = PyOnceLock::new();

/// `decimal.Decimal`, imported once.
pub(crate) fn decimal_type(py: Python<'_>) -> PyResult<&Bound<'_, PyType>> {
    DECIMAL.import(py, "decimal", "Decimal")
}

/// How the rows of one result set become Python objects, decided once from
/// its columns.
#[derive(Debug)]
pub(crate) struct Converter {
    kinds: Vec<Kind>,
    decimal: Option<Py<PyType>>,
    /// One key per column for dict rows; `None` for tuple rows.
    keys: Option<Vec<Py<PyString>>>,
    /// The session's character set: text (JSON included) arrives in it.
    codec: Arc<Codec>,
    /// `conv=` decoders used by this result's columns.
    custom: Vec<Py<PyAny>>,
}

impl Converter {
    pub(crate) fn new(
        py: Python<'_>,
        columns: &[Column],
        as_dict: bool,
        codec: &Arc<Codec>,
        custom: Option<&Custom>,
    ) -> PyResult<Self> {
        let mut kinds: Vec<Kind> = columns.iter().map(kind).collect();
        let mut custom_decoders = Vec::new();
        if let Some(decoders) = custom.and_then(|c| c.decoders.as_ref()) {
            let decoders = decoders.bind(py);
            for (column, slot) in columns.iter().zip(kinds.iter_mut()) {
                if let Some(decoder) = custom_decoder(decoders, column)? {
                    *slot = Kind::Custom {
                        index: custom_decoders.len(),
                        text: decodes_from_text(column.column_type),
                    };
                    custom_decoders.push(decoder.unbind());
                }
            }
        }
        let decimal = if kinds.contains(&Kind::Decimal) {
            Some(decimal_type(py)?.clone().unbind())
        } else {
            None
        };
        Ok(Self {
            kinds,
            decimal,
            keys: as_dict.then(|| dict_keys(py, columns, codec)),
            codec: Arc::clone(codec),
            custom: custom_decoders,
        })
    }

    /// Converts every row of `rows`, handing each object to `push`.
    pub(crate) fn convert<'py>(
        &self,
        py: Python<'py>,
        rows: &RowSet,
        mut push: impl FnMut(Bound<'py, PyAny>),
    ) -> PyResult<()> {
        let decimal = self.decimal.as_ref().map(|d| d.bind(py));
        let codec = &*self.codec;
        let custom = &self.custom;
        let cell_value = |cell: Option<&[u8]>, kind: Kind| match (cell, kind) {
            (None, _) => Ok(py.None().into_bound(py)),
            (Some(bytes), Kind::Custom { index, text }) => {
                custom_value(py, custom, index, text, bytes)
            }
            (Some(bytes), _) => value(py, kind, bytes, decimal, codec),
        };
        if let Some(keys) = &self.keys {
            for row in rows.rows() {
                let dict = PyDict::new(py);
                for ((cell, &kind), key) in row.cells().zip(&self.kinds).zip(keys) {
                    dict.set_item(key.bind(py), cell_value(cell, kind)?)?;
                }
                push(dict.into_any());
            }
        } else {
            let mut values = Vec::with_capacity(self.kinds.len());
            for row in rows.rows() {
                for (cell, &kind) in row.cells().zip(&self.kinds) {
                    values.push(cell_value(cell, kind)?);
                }
                push(PyTuple::new(py, values.drain(..))?.into_any());
            }
        }
        Ok(())
    }
}

/// Dict keys: the column name, or `table.name` for a name already taken
/// (as other MySQL drivers do; a name taken twice keeps the last value).
/// Interned, so lookups with literal keys compare by identity.
fn dict_keys(py: Python<'_>, columns: &[Column], codec: &Codec) -> Vec<Py<PyString>> {
    let mut seen = HashSet::with_capacity(columns.len());
    columns
        .iter()
        .map(|c| {
            let column = codec.decode_lenient(py, &c.name);
            let name = if seen.contains(column.as_str()) {
                format!("{}.{column}", codec.decode_lenient(py, &c.table))
            } else {
                column
            };
            let key = PyString::intern(py, &name).unbind();
            seen.insert(name);
            key
        })
        .collect()
}

/// `(description, rows, rowcount, last_insert_id, warnings)`. For statements
/// without a result set, description and rows are `None` and rowcount is the
/// number of affected rows. Rows are dicts with `as_dict`, else tuples.
pub(crate) fn result_to_py(
    py: Python<'_>,
    result: &QueryResult,
    as_dict: bool,
    codec: &Arc<Codec>,
) -> PyResult<Py<PyAny>> {
    if result.columns.is_empty() {
        return (
            py.None(),
            py.None(),
            result.affected_rows,
            result.last_insert_id,
            result.warnings,
        )
            .into_py_any(py);
    }
    let converter = Converter::new(py, &result.columns, as_dict, codec, None)?;
    let mut rows = Vec::with_capacity(result.rows.len());
    converter.convert(py, &result.rows, |row| rows.push(row))?;
    (
        description(py, &result.columns, codec)?,
        PyList::new(py, rows)?,
        result.rows.len(),
        result.last_insert_id,
        result.warnings,
    )
        .into_py_any(py)
}

/// PEP 249 `cursor.description`: one 7-item tuple per column.
pub(crate) fn description<'py>(
    py: Python<'py>,
    columns: &[Column],
    codec: &Codec,
) -> PyResult<Bound<'py, PyTuple>> {
    let items = columns
        .iter()
        .map(|c| {
            (
                codec.decode_lenient(py, &c.name),
                c.column_type.0,
                py.None(),
                c.length,
                c.length,
                c.decimals,
                c.flags & column_flag::NOT_NULL == 0,
            )
                .into_pyobject(py)
        })
        .collect::<PyResult<Vec<_>>>()?;
    PyTuple::new(py, items)
}

fn value<'py>(
    py: Python<'py>,
    kind: Kind,
    bytes: &[u8],
    decimal: Option<&Bound<'py, PyType>>,
    codec: &Codec,
) -> PyResult<Bound<'py, PyAny>> {
    let invalid = || {
        DataError::new_err((
            0,
            format!(
                "invalid {kind:?} value {:?}",
                String::from_utf8_lossy(bytes)
            ),
        ))
    };
    match kind {
        Kind::Int => text::parse_i64(bytes)
            .ok_or_else(invalid)?
            .into_bound_py_any(py),
        Kind::UInt => text::parse_u64(bytes)
            .ok_or_else(invalid)?
            .into_bound_py_any(py),
        Kind::Float => text::parse_f64(bytes)
            .ok_or_else(invalid)?
            .into_bound_py_any(py),
        Kind::Decimal => decimal.ok_or_else(invalid)?.call1((utf8(py, bytes)?,)),
        Kind::Date => Ok(date(py, bytes)),
        Kind::DateTime => Ok(datetime(py, bytes)),
        Kind::Time => time(py, bytes).ok_or_else(invalid),
        Kind::Text => codec.decode(py, bytes).map(Bound::into_any),
        // `Custom` cells go through `custom_value` instead.
        Kind::Bytes | Kind::Custom { .. } => Ok(PyBytes::new(py, bytes).into_any()),
        Kind::Null => Ok(py.None().into_bound(py)),
    }
}

/// Calls the `conv=` decoder `index` on a cell.
fn custom_value<'py>(
    py: Python<'py>,
    decoders: &[Py<PyAny>],
    index: usize,
    text: bool,
    bytes: &[u8],
) -> PyResult<Bound<'py, PyAny>> {
    let decoder = decoders
        .get(index)
        .ok_or_else(|| DataError::new_err((0, "custom decoder missing")))?
        .bind(py);
    if text {
        decoder.call1((utf8(py, bytes)?,))
    } else {
        decoder.call1((PyBytes::new(py, bytes),))
    }
}

fn utf8<'py>(py: Python<'py>, bytes: &[u8]) -> PyResult<Bound<'py, PyString>> {
    let s = std::str::from_utf8(bytes)
        .map_err(|e| DataError::new_err((0, format!("invalid UTF-8 in text column: {e}"))))?;
    Ok(PyString::new(py, s))
}

/// Zero or otherwise invalid dates, which MySQL can store under permissive
/// SQL modes, become `None` (as in other MySQL drivers) rather than errors.
fn date<'py>(py: Python<'py>, bytes: &[u8]) -> Bound<'py, PyAny> {
    text::parse_date(bytes)
        .and_then(|d| PyDate::new(py, i32::from(d.year), d.month, d.day).ok())
        .map_or_else(|| py.None().into_bound(py), Bound::into_any)
}

/// Naive `datetime.datetime`: the server stores no time zone.
fn datetime<'py>(py: Python<'py>, bytes: &[u8]) -> Bound<'py, PyAny> {
    text::parse_datetime(bytes)
        .and_then(|v| {
            let d = v.date;
            PyDateTime::new(
                py,
                i32::from(d.year),
                d.month,
                d.day,
                v.hour,
                v.minute,
                v.second,
                v.micros,
                None,
            )
            .ok()
        })
        .map_or_else(|| py.None().into_bound(py), Bound::into_any)
}

/// `TIME` as `datetime.timedelta` (it can be negative or exceed 24 h).
fn time<'py>(py: Python<'py>, bytes: &[u8]) -> Option<Bound<'py, PyAny>> {
    let t = text::parse_time(bytes)?;
    let seconds = i32::from(t.hours) * 3600 + i32::from(t.minutes) * 60 + i32::from(t.seconds);
    let micros = i32::try_from(t.micros).ok()?;
    let sign = if t.negative { -1 } else { 1 };
    PyDelta::new(py, 0, sign * seconds, sign * micros, true)
        .ok()
        .map(Bound::into_any)
}
