//! Parsers for values as the server sends them in the text protocol.
//!
//! Every parser returns `None` on malformed input instead of panicking.

/// Signed integer, e.g. `-42`.
#[must_use]
pub fn parse_i64(bytes: &[u8]) -> Option<i64> {
    let (negative, digits) = match bytes.split_first() {
        Some((b'-', rest)) => (true, rest),
        _ => (false, bytes),
    };
    if digits.is_empty() {
        return None;
    }
    // Accumulate negatively so that i64::MIN parses without overflow.
    let mut acc: i64 = 0;
    for &b in digits {
        acc = acc.checked_mul(10)?.checked_sub(i64::from(digit(b)?))?;
    }
    if negative {
        Some(acc)
    } else {
        acc.checked_neg()
    }
}

/// Unsigned integer (`BIGINT UNSIGNED` exceeds `i64`).
#[must_use]
pub fn parse_u64(bytes: &[u8]) -> Option<u64> {
    if bytes.is_empty() {
        return None;
    }
    bytes.iter().try_fold(0u64, |acc, &b| {
        acc.checked_mul(10)?.checked_add(u64::from(digit(b)?))
    })
}

/// `FLOAT` / `DOUBLE`.
#[must_use]
pub fn parse_f64(bytes: &[u8]) -> Option<f64> {
    std::str::from_utf8(bytes).ok()?.parse().ok()
}

/// Calendar date; components are not range-checked (MySQL can store zero
/// dates such as `0000-00-00`), callers decide what to do with them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Date {
    pub year: u16,
    pub month: u8,
    pub day: u8,
}

/// Date and time of day, without time zone (as stored by the server).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DateTime {
    pub date: Date,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
    pub micros: u32,
}

/// `TIME`: a signed duration of up to ±838:59:59.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Time {
    pub negative: bool,
    pub hours: u16,
    pub minutes: u8,
    pub seconds: u8,
    pub micros: u32,
}

/// `YYYY-MM-DD`
#[must_use]
pub fn parse_date(bytes: &[u8]) -> Option<Date> {
    if bytes.len() != 10 || bytes.get(4) != Some(&b'-') || bytes.get(7) != Some(&b'-') {
        return None;
    }
    Some(Date {
        year: field(bytes, 0..4)?,
        month: field(bytes, 5..7)?,
        day: field(bytes, 8..10)?,
    })
}

/// `YYYY-MM-DD HH:MM:SS[.ffffff]`
#[must_use]
pub fn parse_datetime(bytes: &[u8]) -> Option<DateTime> {
    if bytes.get(10) != Some(&b' ') || bytes.get(13) != Some(&b':') || bytes.get(16) != Some(&b':')
    {
        return None;
    }
    Some(DateTime {
        date: parse_date(bytes.get(..10)?)?,
        hour: field(bytes, 11..13)?,
        minute: field(bytes, 14..16)?,
        second: field(bytes, 17..19)?,
        micros: micros(bytes.get(19..)?)?,
    })
}

/// `[-]H..H:MM:SS[.ffffff]`
#[must_use]
pub fn parse_time(bytes: &[u8]) -> Option<Time> {
    let (negative, rest) = match bytes.split_first() {
        Some((b'-', rest)) => (true, rest),
        _ => (false, bytes),
    };
    let colon = rest.iter().position(|&b| b == b':')?;
    let tail = rest.get(colon..)?;
    if tail.get(3) != Some(&b':') {
        return None;
    }
    Some(Time {
        negative,
        hours: u16::try_from(number(rest.get(..colon)?)?).ok()?,
        minutes: field(tail, 1..3)?,
        seconds: field(tail, 4..6)?,
        micros: micros(tail.get(6..)?)?,
    })
}

fn digit(b: u8) -> Option<u8> {
    let d = b.wrapping_sub(b'0');
    (d <= 9).then_some(d)
}

/// All-digit field.
fn number(bytes: &[u8]) -> Option<u32> {
    if bytes.is_empty() {
        return None;
    }
    bytes.iter().try_fold(0u32, |acc, &b| {
        acc.checked_mul(10)?.checked_add(u32::from(digit(b)?))
    })
}

fn field<T: TryFrom<u32>>(bytes: &[u8], range: std::ops::Range<usize>) -> Option<T> {
    T::try_from(number(bytes.get(range)?)?).ok()
}

/// Fractional seconds (`.f` to `.ffffff`, or nothing) as microseconds.
fn micros(frac: &[u8]) -> Option<u32> {
    match frac {
        [] => Some(0),
        [b'.', digits @ ..] if (1..=6).contains(&digits.len()) => {
            let missing = 6 - u32::try_from(digits.len()).ok()?;
            number(digits)?.checked_mul(10u32.pow(missing))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::Date;
    use super::DateTime;
    use super::Time;
    use super::parse_date;
    use super::parse_datetime;
    use super::parse_f64;
    use super::parse_i64;
    use super::parse_time;
    use super::parse_u64;

    #[test]
    fn integers_at_their_limits() {
        assert_eq!(parse_i64(b"-9223372036854775808"), Some(i64::MIN));
        assert_eq!(parse_i64(b"9223372036854775807"), Some(i64::MAX));
        assert_eq!(parse_i64(b"9223372036854775808"), None);
        assert_eq!(parse_i64(b"-0"), Some(0));
        assert_eq!(parse_u64(b"18446744073709551615"), Some(u64::MAX));
        assert_eq!(parse_u64(b"18446744073709551616"), None);
        for bad in [&b""[..], b"-", b"1a", b" 1", b"+1", b"--1"] {
            assert_eq!(parse_i64(bad), None, "{bad:?}");
        }
        assert_eq!(parse_u64(b"-1"), None);
    }

    #[test]
    fn floats() {
        assert_eq!(parse_f64(b"1.5"), Some(1.5));
        assert_eq!(parse_f64(b"-1e-300"), Some(-1e-300));
        assert_eq!(parse_f64(b"abc"), None);
    }

    #[test]
    fn dates() {
        assert_eq!(
            parse_date(b"2026-11-13"),
            Some(Date {
                year: 2026,
                month: 11,
                day: 13
            })
        );
        assert_eq!(
            parse_date(b"0000-00-00"),
            Some(Date {
                year: 0,
                month: 0,
                day: 0
            })
        );
        for bad in [&b"2026-11-1"[..], b"2026/11/13", b"2026-1x-13", b""] {
            assert_eq!(parse_date(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn datetimes_with_and_without_fraction() {
        let expected = |micros| DateTime {
            date: Date {
                year: 2026,
                month: 11,
                day: 13,
            },
            hour: 21,
            minute: 5,
            second: 9,
            micros,
        };
        assert_eq!(parse_datetime(b"2026-11-13 21:05:09"), Some(expected(0)));
        assert_eq!(
            parse_datetime(b"2026-11-13 21:05:09.25"),
            Some(expected(250_000))
        );
        assert_eq!(
            parse_datetime(b"2026-11-13 21:05:09.000001"),
            Some(expected(1))
        );
        for bad in [
            &b"2026-11-13"[..],
            b"2026-11-13T21:05:09",
            b"2026-11-13 21:05:09.",
            b"2026-11-13 21:05:09.1234567",
        ] {
            assert_eq!(parse_datetime(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn times_including_negative_and_over_24_hours() {
        assert_eq!(
            parse_time(b"-838:59:59.5"),
            Some(Time {
                negative: true,
                hours: 838,
                minutes: 59,
                seconds: 59,
                micros: 500_000
            })
        );
        assert_eq!(
            parse_time(b"01:02:03"),
            Some(Time {
                negative: false,
                hours: 1,
                minutes: 2,
                seconds: 3,
                micros: 0
            })
        );
        for bad in [&b"1:2:3"[..], b":00:00", b"10-00-00", b""] {
            assert_eq!(parse_time(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn every_truncation_is_handled() {
        let full = b"2026-11-13 21:05:09.123456";
        for len in 0..=full.len() {
            let _ = parse_datetime(&full[..len]);
            let _ = parse_time(&full[..len]);
            let _ = parse_date(&full[..len]);
        }
    }
}
