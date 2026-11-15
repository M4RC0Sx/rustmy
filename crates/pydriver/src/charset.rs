//! The session's character set: encoding queries and decoding results.
//!
//! UTF-8 takes the fast path it always had. Single-byte character sets use
//! a 256-entry table built at connect time from Python's own codec, so
//! results are identical to Python's while every cell is decoded in Rust.
//! Multi-byte legacy character sets call `CPython`'s decoder directly (the
//! only `unsafe` in the crate's charset handling). Every supported
//! character set is ASCII-compatible (checked when the codec is built), so
//! ASCII text skips the tables and the decoder.

use std::ffi::CStr;
use std::sync::Arc;
use std::sync::LazyLock;

use pyo3::ffi;
use pyo3::prelude::*;
use pyo3::types::PyBytes;
use pyo3::types::PyString;

use crate::errors::DataError;
use crate::errors::NotSupportedError;

/// How a MySQL character set maps to Python.
#[derive(Debug, Clone, Copy)]
enum Family {
    Utf8,
    /// `utf8mb3`: UTF-8 without 4-byte characters.
    Utf8Mb3,
    SingleByte,
    MultiByte,
}

/// A supported MySQL character set and its Python codec.
#[derive(Debug, Clone, Copy)]
struct Spec {
    name: &'static str,
    python: &'static CStr,
    family: Family,
}

const fn spec(name: &'static str, python: &'static CStr, family: Family) -> Spec {
    Spec {
        name,
        python,
        family,
    }
}

/// MySQL character sets with a Python codec. `latin1` is MySQL's own
/// definition: cp1252 plus the five bytes cp1252 leaves undefined, which
/// MySQL maps to the C1 controls of the same value.
const SUPPORTED: &[Spec] = &[
    spec("utf8mb4", c"utf8", Family::Utf8),
    spec("utf8mb3", c"utf8", Family::Utf8Mb3),
    spec("ascii", c"ascii", Family::SingleByte),
    spec("latin1", c"cp1252", Family::SingleByte),
    spec("latin2", c"iso8859_2", Family::SingleByte),
    spec("latin5", c"iso8859_9", Family::SingleByte),
    spec("latin7", c"iso8859_13", Family::SingleByte),
    spec("cp1250", c"cp1250", Family::SingleByte),
    spec("cp1251", c"cp1251", Family::SingleByte),
    spec("cp1256", c"cp1256", Family::SingleByte),
    spec("cp1257", c"cp1257", Family::SingleByte),
    spec("cp850", c"cp850", Family::SingleByte),
    spec("cp852", c"cp852", Family::SingleByte),
    spec("cp866", c"cp866", Family::SingleByte),
    spec("koi8r", c"koi8_r", Family::SingleByte),
    spec("koi8u", c"koi8_u", Family::SingleByte),
    spec("greek", c"iso8859_7", Family::SingleByte),
    spec("hebrew", c"iso8859_8", Family::SingleByte),
    spec("macroman", c"mac_roman", Family::SingleByte),
    spec("macce", c"mac_latin2", Family::SingleByte),
    spec("tis620", c"tis_620", Family::SingleByte),
    spec("big5", c"big5", Family::MultiByte),
    spec("cp932", c"cp932", Family::MultiByte),
    spec("euckr", c"euc_kr", Family::MultiByte),
    spec("gb2312", c"gb2312", Family::MultiByte),
    spec("gb18030", c"gb18030", Family::MultiByte),
    spec("gbk", c"gbk", Family::MultiByte),
    spec("sjis", c"shift_jis", Family::MultiByte),
    spec("ujis", c"euc_jp", Family::MultiByte),
];

/// Bytes MySQL's latin1 defines and cp1252 does not.
const LATIN1_C1: [u8; 5] = [0x81, 0x8D, 0x8F, 0x90, 0x9D];

/// Why a character set cannot be the connection's, for the error message.
fn unsupported_reason(name: &str) -> &'static str {
    match name {
        "binary" => "rustmy decodes text columns, so the connection needs a text character set",
        "eucjpms" => "Python has no exact codec for it; use utf8mb4 or ujis",
        "ucs2" | "utf16" | "utf16le" | "utf32" => {
            "the server does not accept it as a connection character set \
             (columns in it work with any connection character set)"
        }
        "armscii8" | "dec8" | "hp8" | "swe7" | "keybcs2" | "geostd8" => {
            "Python has no codec for it"
        }
        _ => "it is not a MySQL character set",
    }
}

/// The table of a single-byte character set.
#[derive(Debug)]
struct SingleByte {
    decode: [Option<char>; 256],
    /// Non-ASCII characters and their byte, sorted by character.
    encode: Vec<(char, u8)>,
}

#[derive(Debug)]
enum Kind {
    Utf8 { mb3: bool },
    SingleByte(Box<SingleByte>),
    MultiByte,
}

/// The connection's character set.
#[derive(Debug)]
pub(crate) struct Codec {
    /// MySQL name, as sent in `SET NAMES`.
    name: &'static str,
    /// Python codec name.
    python: &'static CStr,
    kind: Kind,
}

/// UTF-8 (`utf8mb4`), the default.
pub(crate) static UTF8: LazyLock<Arc<Codec>> = LazyLock::new(|| {
    Arc::new(Codec {
        name: "utf8mb4",
        python: c"utf8",
        kind: Kind::Utf8 { mb3: false },
    })
});

impl Codec {
    /// The codec for the MySQL character set `name` (`"utf8"` means
    /// `utf8mb4`, as in `PyMySQL`).
    pub(crate) fn new(py: Python<'_>, name: &str) -> PyResult<Arc<Self>> {
        let lower = name.to_ascii_lowercase();
        let wanted = if lower == "utf8" { "utf8mb4" } else { &lower };
        let Some(&spec) = SUPPORTED.iter().find(|s| s.name == wanted) else {
            return Err(NotSupportedError::new_err((
                0,
                format!(
                    "character set {name:?} is not supported: {}",
                    unsupported_reason(wanted)
                ),
            )));
        };
        if matches!(spec.family, Family::Utf8) {
            return Ok(Arc::clone(&UTF8));
        }
        let kind = match spec.family {
            Family::Utf8 | Family::Utf8Mb3 => Kind::Utf8 { mb3: true },
            Family::SingleByte => Kind::SingleByte(Box::new(single_byte_table(py, spec)?)),
            Family::MultiByte => {
                check_ascii_compatible(py, spec)?;
                Kind::MultiByte
            }
        };
        Ok(Arc::new(Self {
            name: spec.name,
            python: spec.python,
            kind,
        }))
    }

    /// MySQL name of the character set.
    pub(crate) const fn name(&self) -> &'static str {
        self.name
    }

    /// Whether text needs no conversion (UTF-8 with 4-byte characters).
    pub(crate) const fn is_utf8mb4(&self) -> bool {
        matches!(self.kind, Kind::Utf8 { mb3: false })
    }

    /// Python codec name (`utf8` for UTF-8, as other drivers report it).
    pub(crate) fn python_name(&self) -> &'static str {
        self.python.to_str().unwrap_or_default()
    }

    /// Whether `bytes` parameters must be written as `X'…'`: escaping raw
    /// bytes is unsafe in multi-byte legacy character sets, where a byte
    /// string could pair a lead byte with the escaping backslash and let a
    /// quote through (CVE-2006-2753).
    pub(crate) const fn needs_hex_binary(&self) -> bool {
        matches!(self.kind, Kind::MultiByte)
    }

    /// Decodes text sent by the server.
    pub(crate) fn decode<'py>(
        &self,
        py: Python<'py>,
        bytes: &[u8],
    ) -> PyResult<Bound<'py, PyString>> {
        match &self.kind {
            Kind::Utf8 { .. } => {
                let text = std::str::from_utf8(bytes).map_err(|e| {
                    DataError::new_err((0, format!("invalid UTF-8 in text column: {e}")))
                })?;
                Ok(PyString::new(py, text))
            }
            _ if bytes.is_ascii() => Ok(PyString::new(py, ascii(bytes))),
            Kind::SingleByte(table) => {
                let mut text = String::with_capacity(bytes.len() * 2);
                for &byte in bytes {
                    let c = table
                        .decode
                        .get(usize::from(byte))
                        .copied()
                        .flatten()
                        .ok_or_else(|| self.undecodable(byte))?;
                    text.push(c);
                }
                Ok(PyString::new(py, &text))
            }
            Kind::MultiByte => self.decode_with_python(py, bytes),
        }
    }

    /// Decodes server text that must not fail (names, messages), replacing
    /// what cannot be decoded.
    pub(crate) fn decode_lenient(&self, py: Python<'_>, bytes: &[u8]) -> String {
        if let Ok(text) = self.decode(py, bytes)
            && let Ok(text) = text.to_str()
        {
            return text.to_owned();
        }
        String::from_utf8_lossy(bytes).into_owned()
    }

    /// Decodes a statement this connection built (for display: `mogrify`,
    /// `_executed`), keeping bytes that are not text, such as binary
    /// literals, as surrogate escapes.
    pub(crate) fn decode_statement<'py>(
        &self,
        py: Python<'py>,
        bytes: &[u8],
    ) -> PyResult<Bound<'py, PyAny>> {
        PyBytes::new(py, bytes).call_method1("decode", (self.python, "surrogateescape"))
    }

    /// Appends `text` encoded in this character set.
    pub(crate) fn encode_into(
        &self,
        py: Python<'_>,
        out: &mut Vec<u8>,
        text: &str,
    ) -> PyResult<()> {
        match &self.kind {
            Kind::Utf8 { mb3: false } => out.extend_from_slice(text.as_bytes()),
            Kind::Utf8 { mb3: true } => {
                if let Some(c) = text.chars().find(|c| c.len_utf8() == 4) {
                    return Err(self.unencodable(c));
                }
                out.extend_from_slice(text.as_bytes());
            }
            _ if text.is_ascii() => out.extend_from_slice(text.as_bytes()),
            Kind::SingleByte(table) => {
                for c in text.chars() {
                    if c.is_ascii() {
                        out.push(u8::try_from(c).unwrap_or_default());
                    } else {
                        let at = table
                            .encode
                            .binary_search_by_key(&c, |&(ch, _)| ch)
                            .map_err(|_| self.unencodable(c))?;
                        let (_, byte) = table.encode.get(at).copied().unwrap_or_default();
                        out.push(byte);
                    }
                }
            }
            Kind::MultiByte => {
                let encoded = PyString::new(py, text)
                    .call_method1("encode", (self.python, "strict"))
                    .map_err(|err| self.encode_error(py, &err))?;
                out.extend_from_slice(encoded.cast::<PyBytes>()?.as_bytes());
            }
        }
        Ok(())
    }

    fn decode_with_python<'py>(
        &self,
        py: Python<'py>,
        bytes: &[u8],
    ) -> PyResult<Bound<'py, PyString>> {
        let len = isize::try_from(bytes.len())
            .map_err(|_| DataError::new_err((0, "text value too large to decode")))?;
        // SAFETY: `bytes` is valid for reads of `len` bytes, both C strings
        // are NUL-terminated and outlive the call, and `py` proves the GIL
        // (or thread state, on free-threaded builds) is held.
        let ptr = unsafe {
            ffi::PyUnicode_Decode(
                bytes.as_ptr().cast(),
                len,
                self.python.as_ptr(),
                c"strict".as_ptr(),
            )
        };
        // SAFETY: `PyUnicode_Decode` returns a new reference, or null with
        // an exception set: what `from_owned_ptr_or_err` takes ownership of.
        let decoded = unsafe { Bound::from_owned_ptr_or_err(py, ptr) }.map_err(|err| {
            DataError::new_err((0, format!("cannot decode text as {}: {err}", self.name)))
        })?;
        Ok(decoded.cast_into::<PyString>()?)
    }

    fn undecodable(&self, byte: u8) -> PyErr {
        DataError::new_err((
            0,
            format!(
                "byte 0x{byte:02x} is not a character in the connection's character set {}",
                self.name
            ),
        ))
    }

    fn unencodable(&self, c: char) -> PyErr {
        DataError::new_err((
            0,
            format!(
                "character {c:?} (U+{:04X}) cannot be encoded in the connection's \
                 character set {}",
                u32::from(c),
                self.name
            ),
        ))
    }

    fn encode_error(&self, py: Python<'_>, err: &PyErr) -> PyErr {
        let value = err.value(py);
        let bad = value
            .getattr("object")
            .and_then(|object| {
                let start = value.getattr("start")?;
                object.get_item(start)?.extract::<String>()
            })
            .ok()
            .and_then(|s| s.chars().next());
        match bad {
            Some(c) => self.unencodable(c),
            None => DataError::new_err((0, format!("cannot encode as {}: {err}", self.name))),
        }
    }
}

/// `bytes` as `&str`; only called on ASCII, which is valid UTF-8.
fn ascii(bytes: &[u8]) -> &str {
    std::str::from_utf8(bytes).unwrap_or_default()
}

/// Decodes every byte with Python's codec; bytes it rejects stay undefined.
fn single_byte_table(py: Python<'_>, spec: Spec) -> PyResult<SingleByte> {
    let mut decode = [None; 256];
    for (byte, slot) in (0..=u8::MAX).zip(decode.iter_mut()) {
        let decoded = PyBytes::new(py, &[byte]).call_method1("decode", (spec.python, "strict"));
        if let Ok(text) = decoded.and_then(|d| d.extract::<String>()) {
            let mut chars = text.chars();
            if let (Some(c), None) = (chars.next(), chars.next()) {
                *slot = Some(c);
            }
        }
    }
    if spec.name == "latin1" {
        for byte in LATIN1_C1 {
            if let Some(slot) = decode.get_mut(usize::from(byte)) {
                *slot = Some(char::from(byte));
            }
        }
    }
    let ascii_compatible = (0..128u8)
        .zip(decode.iter())
        .all(|(byte, c)| *c == Some(char::from(byte)));
    if !ascii_compatible {
        return Err(not_ascii_compatible(spec));
    }
    let mut encode: Vec<(char, u8)> = (0..=u8::MAX)
        .zip(decode.iter())
        .filter_map(|(byte, c)| c.filter(|c| !c.is_ascii()).map(|c| (c, byte)))
        .collect();
    encode.sort_by_key(|&(c, _)| c);
    encode.dedup_by_key(|&mut (c, _)| c);
    Ok(SingleByte { decode, encode })
}

fn check_ascii_compatible(py: Python<'_>, spec: Spec) -> PyResult<()> {
    let ascii: Vec<u8> = (0..128).collect();
    let decoded = PyBytes::new(py, &ascii)
        .call_method1("decode", (spec.python, "strict"))?
        .extract::<String>()?;
    if decoded.as_bytes() == ascii.as_slice() {
        Ok(())
    } else {
        Err(not_ascii_compatible(spec))
    }
}

fn not_ascii_compatible(spec: Spec) -> PyErr {
    NotSupportedError::new_err((
        0,
        format!(
            "character set {} is not supported: Python's codec does not map ASCII to itself",
            spec.name
        ),
    ))
}
