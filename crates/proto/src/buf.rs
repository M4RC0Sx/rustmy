//! Bounds-checked decoding of protocol payloads and helpers to encode them.
//!
//! Every read returns `Result`: a truncated or malicious packet yields
//! `Error::Protocol`, never a panic or an out-of-bounds read.

use crate::error::Error;
use crate::error::Result;

/// First byte of a length-encoded integer that marks SQL `NULL` in a row.
pub(crate) const LENENC_NULL: u8 = 0xFB;

/// Forward-only cursor over a payload.
#[derive(Debug, Clone)]
pub(crate) struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub(crate) const fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    /// Offset of the cursor from the start of the payload.
    pub(crate) const fn pos(&self) -> usize {
        self.pos
    }

    pub(crate) const fn is_empty(&self) -> bool {
        self.pos >= self.buf.len()
    }

    pub(crate) fn peek_u8(&self) -> Option<u8> {
        self.buf.get(self.pos).copied()
    }

    pub(crate) fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.pos.checked_add(n).ok_or_else(truncated)?;
        let out = self.buf.get(self.pos..end).ok_or_else(truncated)?;
        self.pos = end;
        Ok(out)
    }

    pub(crate) fn skip(&mut self, n: usize) -> Result<()> {
        self.take(n).map(|_| ())
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N]> {
        self.take(N)?.try_into().map_err(|_| truncated())
    }

    pub(crate) fn u8(&mut self) -> Result<u8> {
        let [b] = self.array()?;
        Ok(b)
    }

    pub(crate) fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.array()?))
    }

    pub(crate) fn u24(&mut self) -> Result<u32> {
        let [a, b, c] = self.array()?;
        Ok(u32::from_le_bytes([a, b, c, 0]))
    }

    pub(crate) fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.array()?))
    }

    pub(crate) fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.array()?))
    }

    /// Length-encoded integer. `0xFB` (NULL) and `0xFF` are rejected; row
    /// decoding checks for NULL with [`Reader::peek_u8`] before calling this.
    pub(crate) fn lenenc_int(&mut self) -> Result<u64> {
        match self.u8()? {
            b @ 0..=0xFA => Ok(u64::from(b)),
            0xFC => self.u16().map(u64::from),
            0xFD => self.u24().map(u64::from),
            0xFE => self.u64(),
            b => Err(Error::protocol(format!(
                "invalid length-encoded integer prefix {b:#04x}"
            ))),
        }
    }

    /// Length-encoded integer that must fit in memory, used as a length.
    pub(crate) fn lenenc_len(&mut self) -> Result<usize> {
        let n = self.lenenc_int()?;
        usize::try_from(n).map_err(|_| Error::protocol(format!("length {n} too large")))
    }

    pub(crate) fn lenenc_bytes(&mut self) -> Result<&'a [u8]> {
        let n = self.lenenc_len()?;
        self.take(n)
    }

    /// NUL-terminated string; the terminator is consumed but not returned.
    pub(crate) fn nul_bytes(&mut self) -> Result<&'a [u8]> {
        let rest = self.buf.get(self.pos..).unwrap_or_default();
        let len = rest
            .iter()
            .position(|&b| b == 0)
            .ok_or_else(|| Error::protocol("missing NUL terminator"))?;
        let out = self.take(len)?;
        self.skip(1)?;
        Ok(out)
    }

    /// Everything left in the payload.
    pub(crate) fn rest(&mut self) -> &'a [u8] {
        let out = self.buf.get(self.pos..).unwrap_or_default();
        self.pos = self.buf.len();
        out
    }
}

fn truncated() -> Error {
    Error::protocol("unexpected end of packet")
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "every arm checks that the value fits the encoded width"
)]
pub(crate) fn put_lenenc_int(out: &mut Vec<u8>, v: u64) {
    match v {
        0..0xFB => out.push(v as u8),
        0xFB..0x1_0000 => {
            out.push(0xFC);
            out.extend_from_slice(&(v as u16).to_le_bytes());
        }
        0x1_0000..0x100_0000 => {
            out.push(0xFD);
            out.extend_from_slice(&(v as u32).to_le_bytes()[..3]);
        }
        _ => {
            out.push(0xFE);
            out.extend_from_slice(&v.to_le_bytes());
        }
    }
}

pub(crate) fn put_lenenc_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    put_lenenc_int(out, bytes.len() as u64);
    out.extend_from_slice(bytes);
}

pub(crate) fn put_nul_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(bytes);
    out.push(0);
}

#[cfg(test)]
mod tests {
    use super::Reader;
    use super::put_lenenc_int;

    #[test]
    fn lenenc_int_roundtrip_at_every_width_boundary() {
        for v in [
            0,
            0xFA,
            0xFB,
            0xFFFF,
            0x1_0000,
            0xFF_FFFF,
            0x100_0000,
            u64::MAX,
        ] {
            let mut buf = Vec::new();
            put_lenenc_int(&mut buf, v);
            let mut r = Reader::new(&buf);
            assert_eq!(r.lenenc_int().unwrap(), v);
            assert!(r.is_empty());
        }
    }

    #[test]
    fn lenenc_int_rejects_null_and_err_prefixes() {
        assert!(Reader::new(&[0xFB]).lenenc_int().is_err());
        assert!(Reader::new(&[0xFF]).lenenc_int().is_err());
    }

    #[test]
    fn truncated_input_is_an_error_not_a_panic() {
        assert!(Reader::new(&[]).u8().is_err());
        assert!(Reader::new(&[1, 2, 3]).u32().is_err());
        assert!(Reader::new(&[0xFC, 1]).lenenc_int().is_err());
        assert!(Reader::new(&[5, b'a']).lenenc_bytes().is_err());
        assert!(Reader::new(b"no terminator").nul_bytes().is_err());
    }

    #[test]
    fn take_with_huge_length_does_not_overflow() {
        let mut r = Reader::new(&[1, 2, 3]);
        r.skip(1).unwrap();
        assert!(r.take(usize::MAX).is_err());
    }

    #[test]
    fn nul_bytes_consumes_terminator() {
        let mut r = Reader::new(b"abc\0de");
        assert_eq!(r.nul_bytes().unwrap(), b"abc");
        assert_eq!(r.rest(), b"de");
    }
}
