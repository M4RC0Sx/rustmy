//! Packet framing: 3-byte little-endian length + 1-byte sequence id.
//!
//! Payloads of `MAX_PACKET_PAYLOAD` bytes or more are split across several
//! packets; a payload whose length is an exact multiple of the maximum is
//! followed by an empty packet.

use std::io;
use std::io::BufRead;
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
/// Buffers grown beyond this by a large packet are released once it has
/// been handled, so an idle pooled connection does not keep megabytes.
/// Uncompressed payloads this large are not staged at all: they go from the
/// caller's buffer straight to the stream.
pub(crate) const KEEP_CAPACITY: usize = 1 << 20;

/// Drops a buffer a large packet grew, once it is no longer needed.
pub(crate) fn release(buf: &mut Vec<u8>) {
    if buf.capacity() > KEEP_CAPACITY {
        *buf = Vec::new();
    }
}

/// The packets of the payload `head ++ body`, each as its part of `head`
/// and its part of `body`, without joining them. A payload of exactly `k`
/// full packets is followed by an empty one, so the server knows it ended.
fn packets<'a>(head: &'a [u8], body: &'a [u8]) -> impl Iterator<Item = (&'a [u8], &'a [u8])> {
    let total = head.len() + body.len();
    (0..=total / MAX_PACKET_PAYLOAD).map(move |i| {
        let start = i * MAX_PACKET_PAYLOAD;
        let end = total.min(start + MAX_PACKET_PAYLOAD);
        let in_head = head
            .get(start.min(head.len())..end.min(head.len()))
            .unwrap_or_default();
        let in_body = body
            .get(start.saturating_sub(head.len())..end.saturating_sub(head.len()))
            .unwrap_or_default();
        (in_head, in_body)
    })
}

/// Largest logical payload accepted until authentication succeeds. The
/// greeting, auth switches, RSA keys and OK/ERR packets are all far smaller,
/// so a server (or anyone in between, before TLS) cannot make the client
/// allocate without bound before it even knows who it talks to.
pub(crate) const HANDSHAKE_PAYLOAD_LIMIT: usize = 64 * 1024;

/// Largest logical payload accepted afterwards: 1 GiB, the most any server
/// sends (`max_allowed_packet` cannot go higher) and libmysqlclient's own
/// limit.
pub(crate) const PAYLOAD_LIMIT: usize = 1 << 30;

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
    /// Largest logical payload [`PacketStream::read_into`] accepts.
    payload_limit: usize,
}

/// Appends exactly `len` bytes of `source` to `out`. When the read buffer
/// holds them all (nearly always: a row is one packet) they are copied
/// straight from it; `take(len).read_to_end` would probe for an end it
/// already knows, at a cost on every packet. Otherwise they are read as
/// they come, never zero-filling a length only the peer vouches for.
fn append_exact<R: Read>(source: &mut BufReader<R>, out: &mut Vec<u8>, len: usize) -> Result<()> {
    if let Some(bytes) = source.buffer().get(..len) {
        out.extend_from_slice(bytes);
        source.consume(len);
        return Ok(());
    }
    out.reserve(len);
    let wanted = u64::try_from(len).map_err(|_| Error::protocol("packet length too large"))?;
    let read = source.take(wanted).read_to_end(out)?;
    if read != len {
        return Err(io::Error::from(io::ErrorKind::UnexpectedEof).into());
    }
    Ok(())
}

impl<S: Read + Write> PacketStream<S> {
    pub(crate) fn new(stream: S) -> Self {
        Self {
            inner: BufReader::with_capacity(READ_BUFFER_CAPACITY, stream),
            seq: 0,
            write_buf: Vec::new(),
            compressed: None,
            payload_limit: HANDSHAKE_PAYLOAD_LIMIT,
        }
    }

    /// Raises the payload limit from [`HANDSHAKE_PAYLOAD_LIMIT`] to
    /// [`PAYLOAD_LIMIT`], once authenticated.
    pub(crate) fn authenticated(&mut self) {
        self.payload_limit = PAYLOAD_LIMIT;
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
            let read = out.len().saturating_sub(start);
            if read.saturating_add(len_usize) > self.payload_limit {
                return Err(Error::protocol(format!(
                    "the server sent a payload over the {} byte limit",
                    self.payload_limit
                )));
            }
            if let Some(compressed) = &mut self.compressed {
                compressed.append(&mut self.inner, out, len_usize)?;
            } else {
                append_exact(&mut self.inner, out, len_usize)?;
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
        release(&mut buf);
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
    /// them.
    fn write_parts(&mut self, head: &[u8], body: &[u8]) -> Result<()> {
        if self.compressed.is_none() && head.len() + body.len() >= KEEP_CAPACITY {
            return self.write_direct(head, body);
        }
        let mut buf = std::mem::take(&mut self.write_buf);
        buf.clear();
        let result = self
            .encode_parts(&mut buf, head, body)
            .and_then(|()| self.send(&buf));
        release(&mut buf);
        self.write_buf = buf;
        result
    }

    /// Writes a large payload packet by packet, header and parts, with no
    /// copy (uncompressed connections only).
    fn write_direct(&mut self, head: &[u8], body: &[u8]) -> Result<()> {
        for (in_head, in_body) in packets(head, body) {
            let header = self.next_header(in_head.len() + in_body.len())?;
            let stream = self.inner.get_mut();
            stream.write_all(&header)?;
            stream.write_all(in_head)?;
            stream.write_all(in_body)?;
        }
        Ok(self.inner.get_mut().flush()?)
    }

    /// Appends `head ++ body` to `buf` as packets of the current sequence.
    fn encode_parts(&mut self, buf: &mut Vec<u8>, head: &[u8], body: &[u8]) -> Result<()> {
        let total = head.len() + body.len();
        buf.reserve(total + HEADER_LEN * (total / MAX_PACKET_PAYLOAD + 1));
        for (in_head, in_body) in packets(head, body) {
            let header = self.next_header(in_head.len() + in_body.len())?;
            buf.extend_from_slice(&header);
            buf.extend_from_slice(in_head);
            buf.extend_from_slice(in_body);
        }
        Ok(())
    }

    /// Sends packets, wrapping them in compressed ones if need be.
    fn send(&mut self, buf: &[u8]) -> Result<()> {
        match &mut self.compressed {
            None => self.send_framed(buf),
            Some(compressed) => {
                let stream = self.inner.get_mut();
                let sent = stream
                    .write_all(compressed.encode(buf))
                    .and_then(|()| stream.flush());
                compressed.release_output();
                Ok(sent?)
            }
        }
    }

    /// Sends bytes already framed for the wire.
    fn send_framed(&mut self, bytes: &[u8]) -> Result<()> {
        let stream = self.inner.get_mut();
        Ok(stream.write_all(bytes).and_then(|()| stream.flush())?)
    }

    /// The header of the next packet, carrying `len` bytes.
    fn next_header(&mut self, len: usize) -> Result<[u8; HEADER_LEN]> {
        let len = u32::try_from(len)
            .ok()
            .filter(|&l| l as usize <= MAX_PACKET_PAYLOAD)
            .ok_or_else(|| Error::protocol("packet chunk too large"))?;
        let [l0, l1, l2, _] = len.to_le_bytes();
        let header = [l0, l1, l2, self.seq];
        self.seq = self.seq.wrapping_add(1);
        Ok(header)
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::io::Read;
    use std::io::Write;

    use super::HANDSHAKE_PAYLOAD_LIMIT;
    use super::KEEP_CAPACITY;
    use super::PAYLOAD_LIMIT;
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

    /// A stream past authentication (see [`handshake_reader`]).
    fn reader(bytes: Vec<u8>) -> PacketStream<Duplex> {
        let mut ps = handshake_reader(bytes);
        ps.authenticated();
        ps
    }

    fn handshake_reader(bytes: Vec<u8>) -> PacketStream<Duplex> {
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

    /// Hands out at most three bytes per read, as a slow socket would.
    struct Trickle(Cursor<Vec<u8>>);

    impl Read for Trickle {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let n = buf.len().min(3);
            self.0.read(&mut buf[..n])
        }
    }

    impl Write for Trickle {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn reads_payloads_the_buffer_holds_only_in_part() {
        // Each payload arrives over several reads, so the read buffer never
        // holds one whole: the path that is not a plain copy.
        let mut wire = vec![10, 0, 0, 0];
        wire.extend_from_slice(b"0123456789");
        wire.extend_from_slice(&[2, 0, 0, 1, b'a', b'b']);
        let mut ps = PacketStream::new(Trickle(Cursor::new(wire)));
        let mut out = Vec::new();
        assert_eq!(ps.read_into(&mut out).unwrap(), 0..10);
        assert_eq!(ps.read_into(&mut out).unwrap(), 10..12);
        assert_eq!(out, b"0123456789ab");
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

    /// The limit counts the whole logical payload, split packets included,
    /// and is checked from each header, before reading what it announces.
    #[test]
    fn payloads_over_the_limit_are_refused() {
        // Before authentication: one packet just over 64 KiB, of which only
        // the header arrives.
        let len = u32::try_from(HANDSHAKE_PAYLOAD_LIMIT + 1)
            .unwrap()
            .to_le_bytes();
        let mut ps = handshake_reader(vec![len[0], len[1], len[2], 0]);
        let err = ps.read_into(&mut Vec::new()).unwrap_err();
        assert!(err.to_string().contains("byte limit"), "{err}");
        // At the limit exactly, it is read.
        let payload = vec![1u8; HANDSHAKE_PAYLOAD_LIMIT];
        let mut ps = handshake_reader(written(&payload));
        let mut out = Vec::new();
        ps.read_into(&mut out).unwrap();
        assert_eq!(out.len(), HANDSHAKE_PAYLOAD_LIMIT);
        // Split packets add up: a limit just under two of them stops at the
        // second header.
        let mut ps = reader(written(&vec![7u8; MAX_PACKET_PAYLOAD + 10]));
        ps.payload_limit = MAX_PACKET_PAYLOAD + 9;
        let mut out = Vec::new();
        let err = ps.read_into(&mut out).unwrap_err();
        assert!(err.to_string().contains("byte limit"), "{err}");
        assert_eq!(out.len(), MAX_PACKET_PAYLOAD);
        assert_eq!(PAYLOAD_LIMIT, 1 << 30);
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

    /// The packets of `payload`, framed one by one: the reference both
    /// write paths (staged and direct) must match byte for byte.
    fn framed(payload: &[u8], first_seq: u8) -> Vec<u8> {
        let mut out = Vec::new();
        let mut seq = first_seq;
        let mut chunks = payload.chunks(MAX_PACKET_PAYLOAD).peekable();
        while let Some(chunk) = chunks.next() {
            let len = u32::try_from(chunk.len()).unwrap().to_le_bytes();
            out.extend_from_slice(&[len[0], len[1], len[2], seq]);
            out.extend_from_slice(chunk);
            seq = seq.wrapping_add(1);
            if chunks.peek().is_none() && chunk.len() == MAX_PACKET_PAYLOAD {
                out.extend_from_slice(&[0, 0, 0, seq]);
            }
        }
        if payload.is_empty() {
            out.extend_from_slice(&[0, 0, 0, seq]);
        }
        out
    }

    #[test]
    fn large_commands_are_framed_alike_on_both_paths() {
        // Around the size where writes stop being staged and around the
        // packet boundaries; the command byte shifts every chunk by one.
        for total in [
            KEEP_CAPACITY - 1,
            KEEP_CAPACITY,
            MAX_PACKET_PAYLOAD - 1,
            MAX_PACKET_PAYLOAD,
            MAX_PACKET_PAYLOAD + 1,
            2 * MAX_PACKET_PAYLOAD,
            2 * MAX_PACKET_PAYLOAD + 5,
        ] {
            let body: Vec<u8> = (0..total - 1)
                .map(|i| u8::try_from(i % 251).unwrap())
                .collect();
            let mut ps = reader(Vec::new());
            ps.write_command(0x03, &body).unwrap();
            assert!(ps.write_buf.capacity() <= KEEP_CAPACITY, "{total}");
            let mut payload = vec![0x03];
            payload.extend_from_slice(&body);
            assert!(
                ps.inner.into_inner().output == framed(&payload, 0),
                "{total} bytes framed differently"
            );
        }
    }

    #[test]
    fn staged_writes_do_not_keep_a_large_buffer() {
        let mut ps = reader(Vec::new());
        let big = vec![b'x'; 2 * KEEP_CAPACITY];
        let mut replies = Vec::new();
        ps.write_commands(0x03, [&big[..], b"B"], &mut replies)
            .unwrap();
        assert!(ps.write_buf.capacity() <= KEEP_CAPACITY);
        ps.write_packet(b"small").unwrap();
        assert!(ps.write_buf.capacity() > 0);
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
