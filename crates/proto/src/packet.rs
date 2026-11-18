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
use crate::compress::Compressed;
use crate::compress::Compression;
use crate::error::Error;
use crate::error::Result;

const HEADER_LEN: usize = 4;
const READ_BUFFER_CAPACITY: usize = 64 * 1024;

/// Where a pipelined reply starts: the sequence id of its first packet and,
/// on a compressed connection, of its first compressed packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ReplyStart {
    pub(crate) seq: u8,
    pub(crate) compressed: u8,
}

/// Buffered packet reader/writer over a byte stream.
#[derive(Debug)]
pub(crate) struct PacketStream<S: Read + Write> {
    inner: BufReader<S>,
    seq: u8,
    write_buf: Vec<u8>,
    /// Compressed layer, once negotiated and authenticated.
    compressed: Option<Box<Compressed>>,
}

impl<S: Read + Write> PacketStream<S> {
    pub(crate) fn new(stream: S) -> Self {
        Self {
            inner: BufReader::with_capacity(READ_BUFFER_CAPACITY, stream),
            seq: 0,
            write_buf: Vec::new(),
            compressed: None,
        }
    }

    /// Hands the raw stream back (to wrap it in TLS) with the sequence id to
    /// continue from. Fails if the server already sent bytes we buffered.
    pub(crate) fn into_parts(self) -> Result<(S, u8)> {
        if !self.inner.buffer().is_empty() {
            return Err(Error::protocol("unexpected data before the TLS handshake"));
        }
        Ok((self.inner.into_inner(), self.seq))
    }

    pub(crate) fn from_parts(stream: S, seq: u8) -> Self {
        let mut packets = Self::new(stream);
        packets.seq = seq;
        packets
    }

    pub(crate) fn get_ref(&self) -> &S {
        self.inner.get_ref()
    }

    pub(crate) fn get_mut(&mut self) -> &mut S {
        self.inner.get_mut()
    }

    /// From now on, every byte travels in compressed packets (once
    /// authentication succeeds: its last packet is not compressed).
    pub(crate) fn compress(&mut self, algorithm: Compression) -> Result<()> {
        self.compressed = Some(Box::new(Compressed::new(algorithm)?));
        Ok(())
    }

    /// Every client command starts a new sequence, compressed one included.
    pub(crate) fn reset_seq(&mut self) {
        self.seq = 0;
        if let Some(compressed) = &mut self.compressed {
            compressed.set_seq(0);
        }
    }

    /// Reads one logical payload (joining split packets) and appends it to
    /// `out`, returning where it landed. Appending lets result-set rows
    /// accumulate in a single contiguous buffer.
    pub(crate) fn read_into(&mut self, out: &mut Vec<u8>) -> Result<Range<usize>> {
        let start = out.len();
        loop {
            let mut header = [0u8; HEADER_LEN];
            match &mut self.compressed {
                None => self.inner.read_exact(&mut header)?,
                Some(compressed) => compressed.read_exact(&mut self.inner, &mut header)?,
            }
            let [l0, l1, l2, seq] = header;
            // Compressed, only the compressed sequence is checked: servers
            // move the ordinary one to it whenever they flush (see
            // `compress`), and the compressed layer keeps us in sync.
            if seq != self.seq && self.compressed.is_none() {
                return Err(Error::protocol(format!(
                    "packet out of order: expected sequence {}, got {seq}",
                    self.seq
                )));
            }
            self.seq = seq.wrapping_add(1);

            let len = u32::from_le_bytes([l0, l1, l2, 0]);
            let len_usize = usize::try_from(len)
                .map_err(|_| Error::protocol("packet length exceeds address space"))?;
            if let Some(compressed) = &mut self.compressed {
                compressed.append(&mut self.inner, out, len_usize)?;
            } else {
                out.reserve(len_usize);
                let read = (&mut self.inner).take(u64::from(len)).read_to_end(out)?;
                if read != len_usize {
                    return Err(io::Error::from(io::ErrorKind::UnexpectedEof).into());
                }
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

    /// Sends one command per item of `args` in a single write, without
    /// waiting for any reply, and records in `replies` where each reply
    /// starts. Before reading reply `i`, call [`PacketStream::expect_reply`]
    /// with `replies[i]`.
    pub(crate) fn write_commands<'a>(
        &mut self,
        cmd: u8,
        args: impl IntoIterator<Item = &'a [u8]>,
        replies: &mut Vec<ReplyStart>,
    ) -> Result<()> {
        let mut buf = std::mem::take(&mut self.write_buf);
        buf.clear();
        // Compressed, each command goes in compressed packets of its own
        // (see `compress`), so it is framed apart first.
        let mut command = Vec::new();
        let mut encoded = Ok(());
        for arg in args {
            self.reset_seq();
            let compressed = if self.compressed.is_some() {
                command.clear();
                encoded = self.encode_parts(&mut command, &[cmd], arg);
                self.compressed.as_mut().map_or(0, |layer| {
                    layer.encode_into(&command, &mut buf);
                    layer.seq()
                })
            } else {
                encoded = self.encode_parts(&mut buf, &[cmd], arg);
                0
            };
            if encoded.is_err() {
                break;
            }
            replies.push(ReplyStart {
                seq: self.seq,
                compressed,
            });
        }
        let result = encoded.and_then(|()| self.send_framed(&buf));
        self.write_buf = buf;
        result
    }

    /// Sets the sequence ids the next packets read must carry (the start of
    /// a pipelined reply, from [`PacketStream::write_commands`]).
    pub(crate) fn expect_reply(&mut self, start: ReplyStart) {
        self.seq = start.seq;
        if let Some(compressed) = &mut self.compressed {
            compressed.set_seq(start.compressed);
        }
    }

    /// Sends `head ++ body` as one logical payload without concatenating
    /// them first in the common single-packet case.
    fn write_parts(&mut self, head: &[u8], body: &[u8]) -> Result<()> {
        let mut buf = std::mem::take(&mut self.write_buf);
        buf.clear();
        let result = self
            .encode_parts(&mut buf, head, body)
            .and_then(|()| self.send(&buf));
        self.write_buf = buf;
        result
    }

    /// Appends `head ++ body` to `buf` as packets of the current sequence.
    fn encode_parts(&mut self, buf: &mut Vec<u8>, head: &[u8], body: &[u8]) -> Result<()> {
        let total = head.len() + body.len();
        if total < MAX_PACKET_PAYLOAD {
            self.push_header(buf, total)?;
            buf.extend_from_slice(head);
            buf.extend_from_slice(body);
            return Ok(());
        }
        let payload = [head, body].concat();
        buf.reserve(total + HEADER_LEN * (total / MAX_PACKET_PAYLOAD + 1));
        let mut chunks = payload.chunks(MAX_PACKET_PAYLOAD);
        loop {
            let chunk = chunks.next().unwrap_or_default();
            self.push_header(buf, chunk.len())?;
            buf.extend_from_slice(chunk);
            // A full chunk must be followed by another packet, possibly
            // empty, so the server knows the payload continues.
            if chunk.len() < MAX_PACKET_PAYLOAD {
                return Ok(());
            }
        }
    }

    /// Sends packets, wrapping them in compressed ones if need be.
    fn send(&mut self, buf: &[u8]) -> Result<()> {
        match &mut self.compressed {
            None => self.send_framed(buf),
            Some(compressed) => {
                let stream = self.inner.get_mut();
                let bytes = compressed.encode(buf);
                Ok(stream.write_all(bytes).and_then(|()| stream.flush())?)
            }
        }
    }

    /// Sends bytes already framed for the wire.
    fn send_framed(&mut self, bytes: &[u8]) -> Result<()> {
        let stream = self.inner.get_mut();
        Ok(stream.write_all(bytes).and_then(|()| stream.flush())?)
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

    #[test]
    fn pipelined_commands_each_start_a_sequence() {
        let mut ps = reader(Vec::new());
        let big = vec![b'x'; MAX_PACKET_PAYLOAD];
        let mut replies = Vec::new();
        ps.write_commands(0x03, [&b"A"[..], &big, b"B"], &mut replies)
            .unwrap();
        // The second command takes two packets, so its reply starts at 2.
        let seqs: Vec<u8> = replies.iter().map(|r| r.seq).collect();
        assert_eq!(seqs, [1, 2, 1]);
        let out = ps.inner.into_inner().output;
        assert_eq!(out[..6], [2, 0, 0, 0, 0x03, b'A']);
        let second = 6 + 4 + MAX_PACKET_PAYLOAD;
        assert_eq!(out[6..10], [0xFF, 0xFF, 0xFF, 0]);
        assert_eq!(out[second..second + 4], [1, 0, 0, 1]);
        assert_eq!(out[second + 5..], [2, 0, 0, 0, 0x03, b'B']);
    }

    #[test]
    fn expect_reply_sets_the_next_read() {
        let mut ps = reader(vec![1, 0, 0, 4, b'a']);
        ps.expect_reply(super::ReplyStart {
            seq: 4,
            compressed: 0,
        });
        let mut out = Vec::new();
        ps.read_into(&mut out).unwrap();
        assert_eq!(out, b"a");
    }
}
