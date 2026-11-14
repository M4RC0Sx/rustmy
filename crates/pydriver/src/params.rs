//! Client-side parameter interpolation: Python values to SQL literals, and
//! `%s` / `%(name)s` / `%%` substitution into the query text.
//!
//! Behaviour follows the other MySQL drivers (format paramstyle, literals
//! the server parses back to the same value); the implementation is ours.

use std::fmt;
use std::io::Write as _;

use pyo3::prelude::*;
use pyo3::types::PyBool;
use pyo3::types::PyByteArray;
use pyo3::types::PyBytes;
use pyo3::types::PyDate;
use pyo3::types::PyDateAccess;
use pyo3::types::PyDateTime;
use pyo3::types::PyDelta;
use pyo3::types::PyDeltaAccess;
use pyo3::types::PyDict;
use pyo3::types::PyFloat;
use pyo3::types::PyFrozenSet;
use pyo3::types::PyInt;
use pyo3::types::PyList;
use pyo3::types::PyMapping;
use pyo3::types::PySet;
use pyo3::types::PyString;
use pyo3::types::PyTime;
use pyo3::types::PyTimeAccess;
use pyo3::types::PyTuple;
use rustmy_proto::sql::escape_into;

use crate::convert::decimal_type;
use crate::errors::ProgrammingError;

/// How `bytes` parameters are written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BinaryLiteral {
    /// `_binary'…'` with escaping: compact, safe when the connection
    /// charset is UTF-8 or single-byte.
    Escaped,
    /// `X'…'`: twice the size, but safe with multi-byte legacy charsets
    /// (gbk, big5, sjis…) where an escaped byte string could let a quote
    /// slip through (CVE-2006-2753).
    #[expect(
        dead_code,
        reason = "selected by non-UTF-8 charsets (Phase 2, block 3)"
    )]
    Hex,
}

/// Writes parameters as SQL literals for one connection's settings.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Encoder {
    pub(crate) no_backslash_escapes: bool,
    pub(crate) binary: BinaryLiteral,
}

/// Arguments of one `execute`: a sequence for `%s`, a mapping for
/// `%(name)s`, or a single value treated as a one-item sequence.
pub(crate) enum Args<'py> {
    Positional(Vec<Bound<'py, PyAny>>),
    Named(Bound<'py, PyAny>),
}

impl<'py> Args<'py> {
    pub(crate) fn from_py(args: &Bound<'py, PyAny>) -> Self {
        if args.is_instance_of::<PyDict>() || args.cast::<PyMapping>().is_ok() {
            return Self::Named(args.clone());
        }
        if let Ok(tuple) = args.cast::<PyTuple>() {
            return Self::Positional(tuple.iter().collect());
        }
        if let Ok(list) = args.cast::<PyList>() {
            return Self::Positional(list.iter().collect());
        }
        Self::Positional(vec![args.clone()])
    }

    const fn empty() -> Self {
        Self::Positional(Vec::new())
    }
}

fn programming_error(message: impl Into<String>) -> PyErr {
    ProgrammingError::new_err((0, message.into()))
}

/// `write!` into a byte buffer; writing to a `Vec` cannot fail.
fn push_fmt(out: &mut Vec<u8>, args: fmt::Arguments<'_>) {
    let _ = out.write_fmt(args);
}

impl Encoder {
    /// Appends `template` with its placeholders replaced by `args`.
    pub(crate) fn interpolate(
        self,
        out: &mut Vec<u8>,
        template: &[u8],
        args: &Args<'_>,
    ) -> PyResult<()> {
        let mut next_positional = 0;
        let mut run_start = 0;
        let mut pos = 0;
        while let Some(offset) = template
            .get(pos..)
            .and_then(|rest| rest.iter().position(|&b| b == b'%'))
        {
            let percent = pos + offset;
            out.extend_from_slice(template.get(run_start..percent).unwrap_or_default());
            match template.get(percent + 1) {
                Some(b'%') => {
                    out.push(b'%');
                    pos = percent + 2;
                }
                Some(b's') => {
                    let Args::Positional(values) = args else {
                        return Err(programming_error(
                            "%s placeholder used with mapping parameters; use %(name)s",
                        ));
                    };
                    let value = values.get(next_positional).ok_or_else(|| {
                        programming_error(format!(
                            "not enough parameters: the query has more than {} %s placeholders",
                            values.len()
                        ))
                    })?;
                    next_positional += 1;
                    self.literal(out, value)?;
                    pos = percent + 2;
                }
                Some(b'(') => {
                    let name_start = percent + 2;
                    let close = template
                        .get(name_start..)
                        .and_then(|rest| rest.iter().position(|&b| b == b')'))
                        .map(|i| name_start + i)
                        .ok_or_else(|| programming_error("unterminated %(name)s placeholder"))?;
                    if template.get(close + 1) != Some(&b's') {
                        return Err(programming_error(
                            "unsupported placeholder: only %(name)s is allowed after %(",
                        ));
                    }
                    let name =
                        std::str::from_utf8(template.get(name_start..close).unwrap_or_default())
                            .map_err(|_| {
                                programming_error("placeholder name is not valid UTF-8")
                            })?;
                    let Args::Named(mapping) = args else {
                        return Err(programming_error(format!(
                            "%({name})s placeholder needs mapping parameters"
                        )));
                    };
                    let value = mapping
                        .get_item(name)
                        .map_err(|_| programming_error(format!("missing parameter {name:?}")))?;
                    self.literal(out, &value)?;
                    pos = close + 2;
                }
                other => {
                    let shown = other.map_or_else(
                        || "% at the end of the query".to_owned(),
                        |&b| format!("%{}", char::from(b)),
                    );
                    return Err(programming_error(format!(
                        "unsupported placeholder ({shown}); use %s, %(name)s or %% for a literal %"
                    )));
                }
            }
            run_start = pos;
        }
        out.extend_from_slice(template.get(run_start..).unwrap_or_default());
        if let Args::Positional(values) = args
            && next_positional != values.len()
        {
            return Err(programming_error(format!(
                "not all parameters were used: {} given, {} %s placeholders",
                values.len(),
                next_positional
            )));
        }
        Ok(())
    }

    /// Appends `value` as an SQL literal.
    pub(crate) fn literal(self, out: &mut Vec<u8>, value: &Bound<'_, PyAny>) -> PyResult<()> {
        if value.is_none() {
            out.extend_from_slice(b"NULL");
        } else if let Ok(flag) = value.cast::<PyBool>() {
            out.push(if flag.is_true() { b'1' } else { b'0' });
        } else if let Ok(int) = value.cast::<PyInt>() {
            write_int(out, int)?;
        } else if let Ok(float) = value.cast::<PyFloat>() {
            write_float(out, float.value())?;
        } else if let Ok(text) = value.cast::<PyString>() {
            out.push(b'\'');
            escape_into(out, text.to_str()?.as_bytes(), self.no_backslash_escapes);
            out.push(b'\'');
        } else if let Ok(bytes) = value.cast::<PyBytes>() {
            self.write_binary(out, bytes.as_bytes());
        } else if let Ok(bytes) = value.cast::<PyByteArray>() {
            self.write_binary(out, &bytes.to_vec());
        } else if let Ok(datetime) = value.cast::<PyDateTime>() {
            write_datetime(out, datetime);
        } else if let Ok(date) = value.cast::<PyDate>() {
            push_fmt(
                out,
                format_args!(
                    "'{:04}-{:02}-{:02}'",
                    date.get_year(),
                    date.get_month(),
                    date.get_day()
                ),
            );
        } else if let Ok(time) = value.cast::<PyTime>() {
            out.push(b'\'');
            write_clock(
                out,
                time.get_hour(),
                time.get_minute(),
                time.get_second(),
                time.get_microsecond(),
            );
            out.push(b'\'');
        } else if let Ok(delta) = value.cast::<PyDelta>() {
            write_timedelta(out, delta);
        } else if value.is_instance(decimal_type(value.py())?)? {
            write_decimal(out, value)?;
        } else if value.is_instance_of::<PyTuple>()
            || value.is_instance_of::<PyList>()
            || value.is_instance_of::<PySet>()
            || value.is_instance_of::<PyFrozenSet>()
        {
            out.push(b'(');
            for (i, item) in value.try_iter()?.enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                self.literal(out, &item?)?;
            }
            out.push(b')');
        } else if value.hasattr("tobytes")? && value.get_type().name()? == "memoryview" {
            let bytes = value.call_method0("tobytes")?;
            self.write_binary(out, bytes.cast::<PyBytes>()?.as_bytes());
        } else {
            return Err(programming_error(format!(
                "unsupported parameter type: {}",
                value.get_type().name()?
            )));
        }
        Ok(())
    }

    fn write_binary(self, out: &mut Vec<u8>, bytes: &[u8]) {
        match self.binary {
            BinaryLiteral::Escaped => {
                out.extend_from_slice(b"_binary'");
                escape_into(out, bytes, self.no_backslash_escapes);
                out.push(b'\'');
            }
            BinaryLiteral::Hex => {
                out.reserve(bytes.len() * 2 + 3);
                out.extend_from_slice(b"X'");
                for &byte in bytes {
                    out.push(hex_digit(byte >> 4));
                    out.push(hex_digit(byte & 0x0F));
                }
                out.push(b'\'');
            }
        }
    }
}

/// Lowercase hex digit of a nibble (`0..=15`).
const fn hex_digit(nibble: u8) -> u8 {
    if nibble < 10 {
        b'0' + nibble
    } else {
        b'a' + nibble - 10
    }
}

fn write_int(out: &mut Vec<u8>, int: &Bound<'_, PyInt>) -> PyResult<()> {
    if let Ok(v) = int.extract::<i64>() {
        push_fmt(out, format_args!("{v}"));
    } else if let Ok(v) = int.extract::<u64>() {
        push_fmt(out, format_args!("{v}"));
    } else {
        // Beyond 64 bits (the server rejects or rounds it, as with other
        // drivers). int.__repr__ gives the digits even for subclasses such
        // as IntEnum, whose str() is the member name.
        let digits = int
            .py()
            .get_type::<PyInt>()
            .getattr("__repr__")?
            .call1((int,))?;
        out.extend_from_slice(digits.cast::<PyString>()?.to_str()?.as_bytes());
    }
    Ok(())
}

/// Shortest round-trip digits in exponent form (`1.5e0`): exact, and
/// MySQL types an exponent literal as DOUBLE rather than DECIMAL.
fn write_float(out: &mut Vec<u8>, value: f64) -> PyResult<()> {
    if !value.is_finite() {
        return Err(programming_error(format!(
            "{value} cannot be stored in MySQL"
        )));
    }
    push_fmt(out, format_args!("{value:e}"));
    Ok(())
}

fn write_decimal(out: &mut Vec<u8>, value: &Bound<'_, PyAny>) -> PyResult<()> {
    // Fixed-point notation keeps the value exact (`1E+2` would be a float).
    let text = value.call_method1("__format__", ("f",))?;
    let text = text.cast::<PyString>()?.to_str()?;
    if text.contains("NaN") || text.contains("Infinity") {
        return Err(programming_error(format!(
            "Decimal({text:?}) cannot be stored in MySQL"
        )));
    }
    out.extend_from_slice(text.as_bytes());
    Ok(())
}

fn write_clock(out: &mut Vec<u8>, hour: u8, minute: u8, second: u8, micros: u32) {
    push_fmt(out, format_args!("{hour:02}:{minute:02}:{second:02}"));
    if micros != 0 {
        push_fmt(out, format_args!(".{micros:06}"));
    }
}

/// Wall-clock value; like the other drivers, any `tzinfo` is not sent
/// (MariaDB rejects offsets, and DATETIME columns store no zone).
fn write_datetime(out: &mut Vec<u8>, value: &Bound<'_, PyDateTime>) {
    push_fmt(
        out,
        format_args!(
            "'{:04}-{:02}-{:02} ",
            value.get_year(),
            value.get_month(),
            value.get_day()
        ),
    );
    write_clock(
        out,
        value.get_hour(),
        value.get_minute(),
        value.get_second(),
        value.get_microsecond(),
    );
    out.push(b'\'');
}

/// `[-]H:MM:SS[.ffffff]`, the form MySQL accepts for TIME.
fn write_timedelta(out: &mut Vec<u8>, value: &Bound<'_, PyDelta>) {
    let total = (i128::from(value.get_days()) * 86_400 + i128::from(value.get_seconds()))
        * 1_000_000
        + i128::from(value.get_microseconds());
    let magnitude = total.unsigned_abs();
    let micros = magnitude % 1_000_000;
    let seconds = magnitude / 1_000_000;
    out.push(b'\'');
    if total < 0 {
        out.push(b'-');
    }
    push_fmt(
        out,
        format_args!(
            "{:02}:{:02}:{:02}",
            seconds / 3600,
            seconds / 60 % 60,
            seconds % 60
        ),
    );
    if micros != 0 {
        push_fmt(out, format_args!(".{micros:06}"));
    }
    out.push(b'\'');
}

/// Whether `text` holds a placeholder (anything but `%%`).
pub(crate) fn has_placeholder(text: &[u8]) -> bool {
    let mut pos = 0;
    while let Some(offset) = text
        .get(pos..)
        .and_then(|rest| rest.iter().position(|&b| b == b'%'))
    {
        let percent = pos + offset;
        if text.get(percent + 1) != Some(&b'%') {
            return true;
        }
        pos = percent + 2;
    }
    false
}

/// `template` with `%%` turned into `%` (it holds no placeholders).
pub(crate) fn unescape_percent(encoder: Encoder, template: &[u8]) -> PyResult<Vec<u8>> {
    let mut out = Vec::with_capacity(template.len());
    encoder.interpolate(&mut out, template, &Args::empty())?;
    Ok(out)
}
