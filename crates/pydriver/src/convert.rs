//! Text-protocol values to Python objects.
//!
//! Each column's converter is chosen once per result set; rows are then
//! converted in a tight loop straight from the contiguous row buffer. Byte
//! parsing lives in `rustmy_proto::text`; this module only builds objects.

use pyo3::IntoPyObjectExt;
use pyo3::prelude::*;
use pyo3::sync::PyOnceLock;
use pyo3::types::PyBytes;
use pyo3::types::PyDate;
use pyo3::types::PyDateTime;
use pyo3::types::PyDelta;
use pyo3::types::PyList;
use pyo3::types::PyString;
use pyo3::types::PyTuple;
use pyo3::types::PyType;
use rustmy_proto::Column;
use rustmy_proto::QueryResult;
use rustmy_proto::constants::ColumnType;
use rustmy_proto::constants::column_flag;
use rustmy_proto::text;

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

/// `(description, rows, rowcount, last_insert_id, warnings)`. For statements
/// without a result set, description and rows are `None` and rowcount is the
/// number of affected rows.
pub(crate) fn result_to_py(py: Python<'_>, result: &QueryResult) -> PyResult<Py<PyAny>> {
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
    (
        description(py, &result.columns)?,
        rows(py, result)?,
        result.rows.len(),
        result.last_insert_id,
        result.warnings,
    )
        .into_py_any(py)
}

/// PEP 249 `cursor.description`: one 7-item tuple per column.
fn description<'py>(py: Python<'py>, columns: &[Column]) -> PyResult<Bound<'py, PyTuple>> {
    let items = columns
        .iter()
        .map(|c| {
            (
                c.name.as_str(),
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

fn rows<'py>(py: Python<'py>, result: &QueryResult) -> PyResult<Bound<'py, PyList>> {
    let kinds: Vec<Kind> = result.columns.iter().map(kind).collect();
    let decimal = if kinds.contains(&Kind::Decimal) {
        Some(decimal_type(py)?)
    } else {
        None
    };
    let mut out = Vec::with_capacity(result.rows.len());
    let mut values = Vec::with_capacity(kinds.len());
    for row in result.rows.rows() {
        for (cell, &kind) in row.cells().zip(&kinds) {
            values.push(match cell {
                None => py.None().into_bound(py),
                Some(bytes) => value(py, kind, bytes, decimal)?,
            });
        }
        out.push(PyTuple::new(py, values.drain(..))?);
    }
    PyList::new(py, out)
}

fn value<'py>(
    py: Python<'py>,
    kind: Kind,
    bytes: &[u8],
    decimal: Option<&Bound<'py, PyType>>,
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
        Kind::Text => utf8(py, bytes).map(Bound::into_any),
        Kind::Bytes => Ok(PyBytes::new(py, bytes).into_any()),
        Kind::Null => Ok(py.None().into_bound(py)),
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
