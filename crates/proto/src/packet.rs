//! Packet framing: 3-byte little-endian length + 1-byte sequence id.
//!
//! Payloads of `MAX_PACKET_PAYLOAD` bytes or more are split across several
//! packets; a payload whose length is an exact multiple of the maximum is
//! followed by an empty packet.

use std::io;
use std::io::BufReader;
use std::io::Read;
use std::io::Write;
use std::ops::Range;

use crate::MAX_PACKET_PAYLOAD;
use crate::error::Error;
use crate::error::Result;

const HEADER_LEN: usize = 4;
const READ_BUFFER_CAPACITY: usize = 64 * 1024;

/// Buffered packet reader/writer over a byte stream.
#[derive(Debug)]
pub(crate) struct PacketStream<S: Read + Write> {
    inner: BufReader<S>,
    seq: u8,
    write_buf: Vec<u8>,
}

impl<S: Read + Write> PacketStream<S> {
    pub(crate) fn new(stream: S) -> Self {
        Self {
            inner: BufReader::with_capacity(READ_BUFFER_CAPACITY, stream),
            seq: 0,
            write_buf: Vec::new(),
        }
    }

    pub(crate) fn get_ref(&self) -> &S {
        self.inner.get_ref()
    }

    /// Every client command starts a new sequence.
    pub(crate) const fn reset_seq(&mut self) {
        self.seq = 0;
    }

    /// Reads one logical payload (joining split packets) and appends it to
    /// `out`, returning where it landed. Appending lets result-set rows
    /// accumulate in a single contiguous buffer.
    pub(crate) fn read_into(&mut self, out: &mut Vec<u8>) -> Result<Range<usize>> {
        let start = out.len();
        loop {
            let mut header = [0u8; HEADER_LEN];
            self.inner.read_exact(&mut header)?;
            let [l0, l1, l2, seq] = header;
            if seq != self.seq {
                return Err(Error::protocol(format!(
                    "packet out of order: expected sequence {}, got {seq}",
                    self.seq
                )));
            }
            self.seq = self.seq.wrapping_add(1);

            let len = u32::from_le_bytes([l0, l1, l2, 0]);
            let len_usize = usize::try_from(len)
                .map_err(|_| Error::protocol("packet length exceeds address space"))?;
            out.reserve(len_usize);
            let read = (&mut self.inner).take(u64::from(len)).read_to_end(out)?;
            if read != len_usize {
                return Err(io::Error::from(io::ErrorKind::UnexpectedEof).into());
            }
            if len_usize < MAX_PACKET_PAYLOAD {
                return Ok(start..out.len());
            }
        }
    }

    /// Starts a new command: resets the sequence and sends `[cmd] ++ arg`.
    pub(crate) fn write_command(&mut self, cmd: u8, arg: &[u8]) -> Result<()> {
        self.reset_seq();
        self.write_parts(&[cmd], arg)
    }

    /// Sends one logical payload, continuing the current sequence.
    pub(crate) fn write_packet(&mut self, payload: &[u8]) -> Result<()> {
        self.write_parts(&[], payload)
    }

    /// Sends `head ++ body` as one logical payload without concatenating
    /// them first in the common single-packet case.
    fn write_parts(&mut self, head: &[u8], body: &[u8]) -> Result<()> {
        let total = head.len() + body.len();
        let mut buf = std::mem::take(&mut self.write_buf);
        buf.clear();
        if total < MAX_PACKET_PAYLOAD {
            self.push_header(&mut buf, total)?;
            buf.extend_from_slice(head);
            buf.extend_from_slice(body);
        } else {
            let payload = [head, body].concat();
            buf.reserve(total + HEADER_LEN * (total / MAX_PACKET_PAYLOAD + 1));
            let mut chunks = payload.chunks(MAX_PACKET_PAYLOAD);
            loop {
                let chunk = chunks.next().unwrap_or_default();
                self.push_header(&mut buf, chunk.len())?;
                buf.extend_from_slice(chunk);
                // A full chunk must be followed by another packet, possibly
                // empty, so the server knows the payload continues.
                if chunk.len() < MAX_PACKET_PAYLOAD {
                    break;
                }
            }
        }
        let stream = self.inner.get_mut();
        let result = stream.write_all(&buf).and_then(|()| stream.flush());
        self.write_buf = buf;
        Ok(result?)
    }

    fn push_header(&mut self, buf: &mut Vec<u8>, len: usize) -> Result<()> {
        let len = u32::try_from(len)
            .ok()
            .filter(|&l| l as usize <= MAX_PACKET_PAYLOAD)
            .ok_or_else(|| Error::protocol("packet chunk too large"))?;
        let [l0, l1, l2, _] = len.to_le_bytes();
        buf.extend_from_slice(&[l0, l1, l2, self.seq]);
        self.seq = self.seq.wrapping_add(1);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::io::Read;
    use std::io::Write;

    use super::PacketStream;
    use crate::MAX_PACKET_PAYLOAD;

    /// In-memory duplex stream: reads from `input`, records writes.
    #[derive(Debug, Default)]
    struct Duplex {
        input: Cursor<Vec<u8>>,
        output: Vec<u8>,
    }

    impl Read for Duplex {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            self.input.read(buf)
        }
    }

    impl Write for Duplex {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.output.write(buf)
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn reader(bytes: Vec<u8>) -> PacketStream<Duplex> {
        PacketStream::new(Duplex {
            input: Cursor::new(bytes),
            output: Vec::new(),
        })
    }

    fn written(payload: &[u8]) -> Vec<u8> {
        let mut ps = reader(Vec::new());
        ps.write_packet(payload).unwrap();
        ps.inner.into_inner().output
    }

    #[test]
    fn reads_a_single_packet() {
        let mut ps = reader(vec![3, 0, 0, 0, b'a', b'b', b'c']);
        let mut out = vec![b'x'];
        let range = ps.read_into(&mut out).unwrap();
        assert_eq!(range, 1..4);
        assert_eq!(out, b"xabc");
    }

    #[test]
    fn rejects_out_of_order_sequence() {
        let mut ps = reader(vec![1, 0, 0, 5, b'a']);
        assert!(ps.read_into(&mut Vec::new()).is_err());
    }

    #[test]
    fn truncated_payload_is_an_error() {
        let mut ps = reader(vec![10, 0, 0, 0, b'a']);
        assert!(ps.read_into(&mut Vec::new()).is_err());
    }

    #[test]
    fn joins_split_payload() {
        let payload = vec![7u8; MAX_PACKET_PAYLOAD + 10];
        let wire = written(&payload);
        let mut ps = reader(wire);
        let mut out = Vec::new();
        ps.read_into(&mut out).unwrap();
        assert_eq!(out, payload);
    }

    #[test]
    fn exact_multiple_of_max_is_followed_by_empty_packet() {
        let wire = written(&vec![1u8; MAX_PACKET_PAYLOAD]);
        assert_eq!(wire.len(), MAX_PACKET_PAYLOAD + 8);
        assert_eq!(wire[MAX_PACKET_PAYLOAD + 4..], [0, 0, 0, 1]);
    }

    #[test]
    fn empty_payload_is_a_single_empty_packet() {
        assert_eq!(written(&[]), [0, 0, 0, 0]);
    }

    #[test]
    fn command_resets_sequence() {
        let mut ps = reader(Vec::new());
        ps.write_packet(b"a").unwrap();
        ps.write_command(0x03, b"SELECT 1").unwrap();
        let out = ps.inner.into_inner().output;
        assert_eq!(out[5..9], [9, 0, 0, 0]);
        assert_eq!(&out[9..], b"\x03SELECT 1");
    }
}
