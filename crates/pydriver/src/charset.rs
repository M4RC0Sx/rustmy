//! The session's character set: encoding queries and decoding results.
//!
//! UTF-8 takes the fast path it always had. Single-byte character sets use
//! a 256-entry table built at connect time from Python's own codec, so
//! every cell is decoded in Rust. Multi-byte legacy character sets call
//! `CPython`'s codec directly (the only `unsafe` in the crate's charset
//! handling). Every supported character set is ASCII-compatible (checked
//! when the codec is built), so ASCII text skips the tables and the codec.
//!
//! The server's definition of a character set wins over Python's codec
//! where they differ (`Fixes`): text the server would store as something
//! else is translated, text it cannot store is refused before sending.

use std::borrow::Cow;
use std::ffi::CStr;
use std::sync::Arc;
use std::sync::LazyLock;

use pyo3::ffi;
use pyo3::prelude::*;
use pyo3::types::PyBytes;
use pyo3::types::PyDict;
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
    fixes: &'static Fixes,
}

const fn spec(name: &'static str, python: &'static CStr, family: Family) -> Spec {
    fixed(name, python, family, &Fixes::NONE)
}

const fn fixed(
    name: &'static str,
    python: &'static CStr,
    family: Family,
    fixes: &'static Fixes,
) -> Spec {
    Spec {
        name,
        python,
        family,
        fixes,
    }
}

/// Where the server's definition of a character set differs from Python's
/// codec. Found by comparing every character both ways on every supported
/// server version (all of them agree) and checked by `tests/test_charsets.py`.
#[derive(Debug)]
struct Fixes {
    /// Single-byte: the server's character for these bytes (`None`: the
    /// server has none).
    bytes: &'static [(u8, Option<char>)],
    /// Multi-byte: characters Python can encode and the server cannot
    /// represent (it would store `?`).
    missing: &'static [char],
    /// Multi-byte: characters the server encodes as Python encodes another
    /// one: `(character, what to hand Python's encoder instead)`.
    encode_as: &'static [(char, char)],
    /// Multi-byte: byte sequences the server reads as a different character
    /// than Python does: `(bytes, Python's character, the server's)`.
    decode_as: &'static [(&'static [u8], char, char)],
}

impl Fixes {
    const NONE: Self = Self {
        bytes: &[],
        missing: &[],
        encode_as: &[],
        decode_as: &[],
    };
}

/// MySQL's latin1 is cp1252 plus the five bytes cp1252 leaves undefined,
/// mapped to the C1 controls of the same value.
const LATIN1: Fixes = Fixes {
    bytes: &[
        (0x81, Some('\u{81}')),
        (0x8D, Some('\u{8D}')),
        (0x8F, Some('\u{8F}')),
        (0x90, Some('\u{90}')),
        (0x9D, Some('\u{9D}')),
    ],
    ..Fixes::NONE
};

/// The server's cp1256 predates the Urdu letters Windows added later.
const CP1256: Fixes = Fixes {
    bytes: &[
        (0x8A, None),
        (0x8F, None),
        (0x98, None),
        (0x9A, None),
        (0x9F, None),
        (0xAA, None),
        (0xC0, None),
        (0xFF, None),
    ],
    ..Fixes::NONE
};

const CP866: Fixes = Fixes {
    bytes: &[(0xFC, Some('\u{207F}')), (0xFD, Some('\u{B2}'))],
    ..Fixes::NONE
};

const KOI8U: Fixes = Fixes {
    bytes: &[(0x95, Some('\u{2022}'))],
    ..Fixes::NONE
};

/// The server's greek is ISO 8859-7:1987: modifier letters where the 2003
/// edition has quotation marks, and no euro, drachma or ypogegrammeni.
const GREEK: Fixes = Fixes {
    bytes: &[
        (0xA1, Some('\u{2BD}')),
        (0xA2, Some('\u{2BC}')),
        (0xA4, None),
        (0xA5, None),
        (0xAA, None),
    ],
    ..Fixes::NONE
};

const HEBREW: Fixes = Fixes {
    bytes: &[(0xAF, Some('\u{203E}'))],
    ..Fixes::NONE
};

const BIG5: Fixes = Fixes {
    missing: &['\u{2CD}', '\u{2574}', '\u{FFE3}'],
    ..Fixes::NONE
};

const CP932: Fixes = Fixes {
    missing: &['\u{80}', '\u{F8F0}', '\u{F8F1}', '\u{F8F2}', '\u{F8F3}'],
    ..Fixes::NONE
};

/// The server reads the full-width backslash's bytes as an ASCII backslash
/// (as text: they still form one two-byte character, so nothing is
/// escaped). `¥` and `‾`, which Python encodes as `\\` and `~`, are caught by
/// the round-trip check in `encode_multibyte`.
const SJIS: Fixes = Fixes {
    missing: &['\u{FF3C}'],
    decode_as: &[(b"\x81\x5F", '\u{FF3C}', '\\')],
    ..Fixes::NONE
};

const UJIS: Fixes = Fixes {
    missing: &['\u{FF3C}'],
    decode_as: &[(b"\xA1\xC0", '\u{FF3C}', '\\')],
    ..Fixes::NONE
};

/// The server follows GB 18030-2005, Python the 2000 edition: two code
/// points swap places.
const GB18030: Fixes = Fixes {
    encode_as: &[('\u{1E3F}', '\u{E7C7}'), ('\u{E7C7}', '\u{1E3F}')],
    decode_as: &[
        (b"\xA8\xBC", '\u{E7C7}', '\u{1E3F}'),
        (b"\x81\x35\xF4\x37", '\u{1E3F}', '\u{E7C7}'),
    ],
    ..Fixes::NONE
};

/// MySQL character sets with a Python codec. `euckr` is the server's: the
/// Unified Hangul Code (cp949), which covers every Hangul syllable, not
/// just the 2350 of EUC-KR (Python's `euc_kr` spells the others as
/// sequences of jamo, which the server stores as such).
const SUPPORTED: &[Spec] = &[
    spec("utf8mb4", c"utf8", Family::Utf8),
    spec("utf8mb3", c"utf8", Family::Utf8Mb3),
    spec("ascii", c"ascii", Family::SingleByte),
    fixed("latin1", c"cp1252", Family::SingleByte, &LATIN1),
    spec("latin2", c"iso8859_2", Family::SingleByte),
    spec("latin5", c"iso8859_9", Family::SingleByte),
    spec("latin7", c"iso8859_13", Family::SingleByte),
    spec("cp1250", c"cp1250", Family::SingleByte),
    spec("cp1251", c"cp1251", Family::SingleByte),
    fixed("cp1256", c"cp1256", Family::SingleByte, &CP1256),
    spec("cp1257", c"cp1257", Family::SingleByte),
    spec("cp850", c"cp850", Family::SingleByte),
    spec("cp852", c"cp852", Family::SingleByte),
    fixed("cp866", c"cp866", Family::SingleByte, &CP866),
    spec("koi8r", c"koi8_r", Family::SingleByte),
    fixed("koi8u", c"koi8_u", Family::SingleByte, &KOI8U),
    fixed("greek", c"iso8859_7", Family::SingleByte, &GREEK),
    fixed("hebrew", c"iso8859_8", Family::SingleByte, &HEBREW),
    spec("macroman", c"mac_roman", Family::SingleByte),
    spec("macce", c"mac_latin2", Family::SingleByte),
    spec("tis620", c"tis_620", Family::SingleByte),
    fixed("big5", c"big5", Family::MultiByte, &BIG5),
    fixed("cp932", c"cp932", Family::MultiByte, &CP932),
    spec("euckr", c"cp949", Family::MultiByte),
    spec("gb2312", c"gb2312", Family::MultiByte),
    fixed("gb18030", c"gb18030", Family::MultiByte, &GB18030),
    spec("gbk", c"gbk", Family::MultiByte),
    fixed("sjis", c"shift_jis", Family::MultiByte, &SJIS),
    fixed("ujis", c"euc_jp", Family::MultiByte, &UJIS),
];

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
    fixes: &'static Fixes,
}

/// UTF-8 (`utf8mb4`), the default.
pub(crate) static UTF8: LazyLock<Arc<Codec>> = LazyLock::new(|| {
    Arc::new(Codec {
        name: "utf8mb4",
        python: c"utf8",
        kind: Kind::Utf8 { mb3: false },
        fixes: &Fixes::NONE,
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
            fixes: spec.fixes,
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
            Kind::MultiByte => {
                let decoded = self.decode_with_python(py, bytes)?;
                let fixes = self.fixes.decode_as;
                if fixes.iter().any(|(seq, ..)| contains(bytes, seq)) {
                    // A byte match may straddle two characters; translating
                    // the decoded text only touches the real ones.
                    return Ok(self.decode_fixes(py, &decoded)?.cast_into::<PyString>()?);
                }
                Ok(decoded)
            }
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
        let decoded =
            PyBytes::new(py, bytes).call_method1("decode", (self.python, "surrogateescape"))?;
        if self.fixes.decode_as.is_empty() {
            Ok(decoded)
        } else {
            self.decode_fixes(py, &decoded)
        }
    }

    /// `text` (decoded by Python's codec) with the server's characters.
    fn decode_fixes<'py>(
        &self,
        py: Python<'py>,
        text: &Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let table = PyDict::new(py);
        for &(_, python, server) in self.fixes.decode_as {
            table.set_item(u32::from(python), u32::from(server))?;
        }
        text.call_method1("translate", (table,))
    }

    /// `text` encoded in this character set.
    pub(crate) fn encode(&self, py: Python<'_>, text: &str) -> PyResult<Vec<u8>> {
        let mut out = Vec::with_capacity(text.len());
        self.encode_into(py, &mut out, text)?;
        Ok(out)
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
            Kind::MultiByte => self.encode_multibyte(py, out, text)?,
        }
        Ok(())
    }

    /// Encodes non-ASCII text with Python's codec, refusing what the server
    /// would read as anything else.
    fn encode_multibyte(&self, py: Python<'_>, out: &mut Vec<u8>, text: &str) -> PyResult<()> {
        if let Some(c) = text.chars().find(|c| self.fixes.missing.contains(c)) {
            return Err(self.unencodable(c));
        }
        let swaps = self.fixes.encode_as;
        let text: Cow<'_, str> = if text
            .chars()
            .any(|c| swaps.iter().any(|&(from, _)| from == c))
        {
            Cow::Owned(
                text.chars()
                    .map(|c| {
                        swaps
                            .iter()
                            .find(|&&(from, _)| from == c)
                            .map_or(c, |&(_, to)| to)
                    })
                    .collect(),
            )
        } else {
            Cow::Borrowed(text)
        };
        let original = PyString::new(py, &text);
        let encoded = original
            .call_method1("encode", (self.python, "strict"))
            .map_err(|err| self.encode_error(py, &err))?;
        let encoded = encoded.cast::<PyBytes>()?.as_bytes();
        // Python's codecs map a few characters one way only: shift_jis and
        // euc_jp encode `¥` as 0x5C, which the server reads as a backslash
        // (an escape: with the quote after it, a way out of the string), and
        // cp932 encodes `¢` as the bytes of the full-width `￠`. Text must
        // come back unchanged, or it is refused.
        let back = self.decode_with_python(py, encoded)?;
        if !PyAnyMethods::eq(back.as_any(), &original)? {
            return Err(self.one_way_error(py, &text));
        }
        out.extend_from_slice(encoded);
        Ok(())
    }

    /// The error for text that does not survive Python's codec both ways,
    /// naming the first character that does not.
    fn one_way_error(&self, py: Python<'_>, text: &str) -> PyErr {
        let mut buf = [0u8; 4];
        let mut survives = |c: char| -> PyResult<bool> {
            let original = PyString::new(py, c.encode_utf8(&mut buf));
            let encoded = original.call_method1("encode", (self.python, "strict"))?;
            let back = self.decode_with_python(py, encoded.cast::<PyBytes>()?.as_bytes())?;
            PyAnyMethods::eq(back.as_any(), &original)
        };
        for c in text.chars().filter(|c| !c.is_ascii()) {
            if !survives(c).unwrap_or(false) {
                return self.unencodable(c);
            }
        }
        DataError::new_err((
            0,
            format!(
                "text cannot be encoded unchanged in the connection's character set {}",
                self.name
            ),
        ))
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

/// Whether `needle` occurs in `haystack`.
fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
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
    for &(byte, c) in spec.fixes.bytes {
        if let Some(slot) = decode.get_mut(usize::from(byte)) {
            *slot = c;
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
