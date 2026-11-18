//! Prepared statements' binary protocol: the parameters of
//! `COM_STMT_EXECUTE`, and the values of binary rows (which
//! [`crate::RowSet`] splits into cells).

use crate::buf::put_lenenc_bytes;
use crate::constants::ColumnType;
use crate::text::Date;
use crate::text::DateTime;
use crate::text::Time;

/// Flag in the second byte of a parameter's type: the integer is unsigned.
const UNSIGNED: u8 = 0x80;

/// Builds the payload of a `COM_STMT_EXECUTE`: push one value per
/// parameter, in order, then [`Execute::finish`].
#[derive(Debug, Default, Clone)]
pub struct Execute {
    nulls: Vec<u8>,
    types: Vec<u8>,
    values: Vec<u8>,
    count: usize,
}

impl Execute {
    /// Starts over for another execution, keeping the allocations.
    pub fn clear(&mut self) {
        self.nulls.clear();
        self.types.clear();
        self.values.clear();
        self.count = 0;
    }

    /// Number of parameters pushed.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.count
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Appends every parameter of `other` after this one's (the rows of a
    /// multi-row statement).
    pub fn append(&mut self, other: &Self) {
        for index in 0..other.count {
            let null = other
                .nulls
                .get(index / 8)
                .is_some_and(|byte| byte & (1 << (index % 8)) != 0);
            if self.count.is_multiple_of(8) {
                self.nulls.push(0);
            }
            if null && let Some(byte) = self.nulls.last_mut() {
                *byte |= 1 << (self.count % 8);
            }
            self.count += 1;
        }
        self.types.extend_from_slice(&other.types);
        self.values.extend_from_slice(&other.values);
    }

    /// Bytes [`Execute::finish`] produces.
    #[must_use]
    pub const fn encoded_len(&self) -> usize {
        10 + self.nulls.len() + 1 + self.types.len() + self.values.len()
    }

    fn push_type(&mut self, column_type: ColumnType, flags: u8) {
        if self.count.is_multiple_of(8) {
            self.nulls.push(0);
        }
        self.types.extend_from_slice(&[column_type.0, flags]);
        self.count += 1;
    }

    pub fn null(&mut self) {
        let bit = self.count % 8;
        self.push_type(ColumnType::NULL, 0);
        if let Some(byte) = self.nulls.last_mut() {
            *byte |= 1 << bit;
        }
    }

    pub fn int(&mut self, value: i64) {
        self.push_type(ColumnType::LONGLONG, 0);
        self.values.extend_from_slice(&value.to_le_bytes());
    }

    pub fn uint(&mut self, value: u64) {
        self.push_type(ColumnType::LONGLONG, UNSIGNED);
        self.values.extend_from_slice(&value.to_le_bytes());
    }

    pub fn double(&mut self, value: f64) {
        self.push_type(ColumnType::DOUBLE, 0);
        self.values.extend_from_slice(&value.to_le_bytes());
    }

    /// A decimal number as its text (`Decimal`).
    pub fn decimal(&mut self, text: &[u8]) {
        self.push_type(ColumnType::NEWDECIMAL, 0);
        put_lenenc_bytes(&mut self.values, text);
    }

    /// Text in the connection's character set.
    pub fn text(&mut self, bytes: &[u8]) {
        self.push_type(ColumnType::VAR_STRING, 0);
        put_lenenc_bytes(&mut self.values, bytes);
    }

    /// Bytes the server must not read as text.
    pub fn blob(&mut self, bytes: &[u8]) {
        self.push_type(ColumnType::BLOB, 0);
        put_lenenc_bytes(&mut self.values, bytes);
    }

    pub fn date(&mut self, year: u16, month: u8, day: u8) {
        self.push_type(ColumnType::DATE, 0);
        self.values.push(4);
        self.values.extend_from_slice(&year.to_le_bytes());
        self.values.extend_from_slice(&[month, day]);
    }

    #[expect(clippy::too_many_arguments, reason = "the fields of a DATETIME")]
    pub fn datetime(
        &mut self,
        year: u16,
        month: u8,
        day: u8,
        hour: u8,
        minute: u8,
        second: u8,
        microsecond: u32,
    ) {
        self.push_type(ColumnType::DATETIME, 0);
        self.values.push(if microsecond == 0 { 7 } else { 11 });
        self.values.extend_from_slice(&year.to_le_bytes());
        self.values
            .extend_from_slice(&[month, day, hour, minute, second]);
        if microsecond != 0 {
            self.values.extend_from_slice(&microsecond.to_le_bytes());
        }
    }

    /// A `TIME`: `days` whole days plus the time of day, negative or not.
    pub fn time(
        &mut self,
        negative: bool,
        days: u32,
        hour: u8,
        minute: u8,
        second: u8,
        microsecond: u32,
    ) {
        self.push_type(ColumnType::TIME, 0);
        self.values.push(if microsecond == 0 { 8 } else { 12 });
        self.values.push(u8::from(negative));
        self.values.extend_from_slice(&days.to_le_bytes());
        self.values.extend_from_slice(&[hour, minute, second]);
        if microsecond != 0 {
            self.values.extend_from_slice(&microsecond.to_le_bytes());
        }
    }

    /// The `COM_STMT_EXECUTE` payload (without the command byte) for
    /// statement `id`.
    #[must_use]
    pub fn finish(&self, id: u32) -> Vec<u8> {
        let mut out =
            Vec::with_capacity(10 + self.nulls.len() + 1 + self.types.len() + self.values.len());
        out.extend_from_slice(&id.to_le_bytes());
        // No cursor; one iteration.
        out.push(0);
        out.extend_from_slice(&1u32.to_le_bytes());
        if self.count > 0 {
            out.extend_from_slice(&self.nulls);
            // The types follow (sent every time: they may change).
            out.push(1);
            out.extend_from_slice(&self.types);
            out.extend_from_slice(&self.values);
        }
        out
    }
}

/// `decimals` of a column whose number of decimals is not fixed (MySQL's
/// `NOT_FIXED_DEC`; MariaDB reports 39 for some expressions).
const NOT_FIXED: u8 = 31;

/// A binary-protocol integer cell (1, 2, 4 or 8 bytes, little-endian),
/// sign-extended.
#[must_use]
pub fn signed(bytes: &[u8]) -> Option<i64> {
    Some(match bytes.len() {
        1 => i64::from(i8::from_le_bytes(bytes.try_into().ok()?)),
        2 => i64::from(i16::from_le_bytes(bytes.try_into().ok()?)),
        4 => i64::from(i32::from_le_bytes(bytes.try_into().ok()?)),
        8 => i64::from_le_bytes(bytes.try_into().ok()?),
        _ => return None,
    })
}

/// A binary-protocol integer cell of an `UNSIGNED` column.
#[must_use]
pub fn unsigned(bytes: &[u8]) -> Option<u64> {
    Some(match bytes.len() {
        1 => u64::from(u8::from_le_bytes(bytes.try_into().ok()?)),
        2 => u64::from(u16::from_le_bytes(bytes.try_into().ok()?)),
        4 => u64::from(u32::from_le_bytes(bytes.try_into().ok()?)),
        8 => u64::from_le_bytes(bytes.try_into().ok()?),
        _ => return None,
    })
}

/// A `FLOAT` (4 bytes) or `DOUBLE` (8 bytes) cell as the number the text
/// protocol would give for a column with `decimals`: the server formats a
/// `FLOAT` with 6 significant digits, and either type with its fixed number
/// of decimals when the column has one.
#[must_use]
pub fn float(bytes: &[u8], decimals: u8) -> Option<f64> {
    let (value, single) = match bytes.len() {
        4 => (f64::from(f32::from_le_bytes(bytes.try_into().ok()?)), true),
        8 => (f64::from_le_bytes(bytes.try_into().ok()?), false),
        _ => return None,
    };
    if decimals < NOT_FIXED {
        format!("{value:.0$}", usize::from(decimals)).parse().ok()
    } else if single {
        format!("{value:.5e}").parse().ok()
    } else {
        Some(value)
    }
}

/// A `DATE` cell; an empty one is the zero date.
#[must_use]
pub fn date(bytes: &[u8]) -> Option<Date> {
    match *bytes {
        [] => Some(Date {
            year: 0,
            month: 0,
            day: 0,
        }),
        [y0, y1, month, day, ..] => Some(Date {
            year: u16::from_le_bytes([y0, y1]),
            month,
            day,
        }),
        _ => None,
    }
}

/// A `DATETIME`/`TIMESTAMP` cell: empty (zero), date only, with seconds,
/// or with microseconds.
#[must_use]
pub fn datetime(bytes: &[u8]) -> Option<DateTime> {
    let date = date(bytes)?;
    let (hour, minute, second, micros) = match *bytes {
        [] | [_, _, _, _] => (0, 0, 0, 0),
        [_, _, _, _, h, m, s] => (h, m, s, 0),
        [_, _, _, _, h, m, s, u0, u1, u2, u3] => (h, m, s, u32::from_le_bytes([u0, u1, u2, u3])),
        _ => return None,
    };
    Some(DateTime {
        date,
        hour,
        minute,
        second,
        micros,
    })
}

/// A `TIME` cell: empty (zero), or sign, days and time of day, with or
/// without microseconds.
#[must_use]
pub fn time(bytes: &[u8]) -> Option<Time> {
    let (negative, days, hours, minutes, seconds, micros) = match *bytes {
        [] => (0, [0; 4], 0, 0, 0, 0),
        [n, d0, d1, d2, d3, h, m, s] => (n, [d0, d1, d2, d3], h, m, s, 0),
        [n, d0, d1, d2, d3, h, m, s, u0, u1, u2, u3] => (
            n,
            [d0, d1, d2, d3],
            h,
            m,
            s,
            u32::from_le_bytes([u0, u1, u2, u3]),
        ),
        _ => return None,
    };
    let hours = u32::from_le_bytes(days)
        .checked_mul(24)?
        .checked_add(u32::from(hours))?;
    Some(Time {
        negative: negative != 0,
        hours: u16::try_from(hours).ok()?,
        minutes,
        seconds,
        micros,
    })
}

/// A binary-protocol cell as the text protocol would send it, for a column
/// of `column_type` (`unsigned`, `decimals`): what `conv=` decoders get.
/// Cells of string-like types are the same in both protocols.
#[must_use]
pub fn to_text(
    column_type: ColumnType,
    unsigned: bool,
    decimals: u8,
    bytes: &[u8],
) -> Option<Vec<u8>> {
    let fraction = |micros: u32| {
        let digits = usize::from(decimals.min(6));
        if decimals >= NOT_FIXED && micros != 0 {
            format!(".{micros:06}")
        } else if digits > 0 && decimals < NOT_FIXED {
            format!(".{micros:06}")
                .get(..=digits)
                .unwrap_or_default()
                .to_owned()
        } else {
            String::new()
        }
    };
    let text = match column_type {
        ColumnType::TINY
        | ColumnType::SHORT
        | ColumnType::LONG
        | ColumnType::INT24
        | ColumnType::LONGLONG
        | ColumnType::YEAR => {
            if unsigned {
                unsigned_text(bytes)?
            } else {
                signed(bytes)?.to_string()
            }
        }
        ColumnType::FLOAT | ColumnType::DOUBLE => {
            let value = float(bytes, decimals)?;
            if decimals < NOT_FIXED {
                format!("{value:.0$}", usize::from(decimals))
            } else {
                value.to_string()
            }
        }
        ColumnType::DATE | ColumnType::NEWDATE => {
            let d = date(bytes)?;
            format!("{:04}-{:02}-{:02}", d.year, d.month, d.day)
        }
        ColumnType::DATETIME | ColumnType::TIMESTAMP => {
            let v = datetime(bytes)?;
            format!(
                "{:04}-{:02}-{:02} {:02}:{:02}:{:02}{}",
                v.date.year,
                v.date.month,
                v.date.day,
                v.hour,
                v.minute,
                v.second,
                fraction(v.micros)
            )
        }
        ColumnType::TIME => {
            let t = time(bytes)?;
            format!(
                "{}{:02}:{:02}:{:02}{}",
                if t.negative { "-" } else { "" },
                t.hours,
                t.minutes,
                t.seconds,
                fraction(t.micros)
            )
        }
        _ => return Some(bytes.to_vec()),
    };
    Some(text.into_bytes())
}

fn unsigned_text(bytes: &[u8]) -> Option<String> {
    unsigned(bytes).map(|v| v.to_string())
}

#[cfg(test)]
mod tests {
    use super::Execute;

    #[test]
    fn append_moves_null_bits() {
        let mut first = Execute::default();
        for i in 0..7 {
            first.int(i);
        }
        let mut second = Execute::default();
        second.int(7);
        second.null();
        second.null();
        first.append(&second);
        let mut direct = Execute::default();
        for i in 0..8 {
            direct.int(i);
        }
        direct.null();
        direct.null();
        assert_eq!(first.finish(9), direct.finish(9));
        assert_eq!(first.len(), 10);
    }

    #[test]
    fn decoders() {
        use super::date;
        use super::datetime;
        use super::float;
        use super::signed;
        use super::time;
        use super::to_text;
        use super::unsigned;
        use crate::constants::ColumnType;

        assert_eq!(signed(&[0xFF]), Some(-1));
        assert_eq!(signed(&[0xFE, 0xFF, 0xFF, 0xFF]), Some(-2));
        assert_eq!(unsigned(&[0xFF, 0xFF]), Some(65_535));
        assert_eq!(signed(&[1, 2, 3]), None);
        assert_eq!(float(&1.1f32.to_le_bytes(), 31), Some(1.1));
        assert_eq!(float(&0.123_456_7_f32.to_le_bytes(), 31), Some(0.123_457));
        assert_eq!(float(&2.345_67_f64.to_le_bytes(), 2), Some(2.35));
        assert_eq!(float(&0.1f64.to_le_bytes(), 31), Some(0.1));
        let zero = date(&[]).unwrap();
        assert_eq!((zero.year, zero.month, zero.day), (0, 0, 0));
        let v = datetime(&[0xEA, 0x07, 11, 17, 1, 2, 3, 0x40, 0xE2, 0x01, 0]).unwrap();
        assert_eq!(
            (v.date.year, v.hour, v.second, v.micros),
            (2026, 1, 3, 123_456)
        );
        let t = time(&[1, 1, 0, 0, 0, 2, 3, 4]).unwrap();
        assert_eq!(
            (t.negative, t.hours, t.minutes, t.seconds),
            (true, 26, 3, 4)
        );
        assert_eq!(
            to_text(
                ColumnType::TIME,
                false,
                2,
                &[1, 1, 0, 0, 0, 2, 3, 4, 0x20, 0xA1, 0x07, 0]
            ),
            Some(b"-26:03:04.50".to_vec())
        );
        assert_eq!(
            to_text(ColumnType::DATETIME, false, 0, &[]),
            Some(b"0000-00-00 00:00:00".to_vec())
        );
        assert_eq!(
            to_text(ColumnType::LONGLONG, true, 0, &u64::MAX.to_le_bytes()),
            Some(b"18446744073709551615".to_vec())
        );
    }
}
