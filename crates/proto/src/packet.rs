//! Packet framing: 3-byte little-endian length + 1-byte sequence id.
//!
//! Payloads of `MAX_PACKET_PAYLOAD` bytes or more are split across several
//! packets; a payload whose length is an exact multiple of the maximum is
//! followed by an empty packet.
//!
//! [`Framer`] does the framing without I/O: bytes from the server go in
//! through an [`Input`], whole payloads come out, in whatever pieces the
//! bytes arrive. [`PacketStream`] drives it over a blocking stream.

use std::io;
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

/// Bytes received from the server and not consumed yet (`pos..end` of
/// `buf`). The buffer is not shrunk as it is consumed, so refilling it
/// zero-fills nothing.
#[derive(Debug, Default)]
pub(crate) struct Input {
    buf: Vec<u8>,
    pos: usize,
    end: usize,
}

impl Input {
    /// Room for at least `min` more bytes; report how many were written at
    /// its start with [`Input::filled`].
    pub(crate) fn spare(&mut self, min: usize) -> &mut [u8] {
        if self.pos > 0 {
            // What is left is at most part of a header: framing consumes
            // whatever body bytes arrived.
            self.buf.copy_within(self.pos..self.end, 0);
            self.end -= self.pos;
            self.pos = 0;
        }
        let wanted = self.end.saturating_add(min.max(1));
        if self.buf.len() < wanted {
            self.buf.resize(wanted, 0);
        }
        self.buf.get_mut(self.end..).unwrap_or_default()
    }

    /// `n` bytes were written at the start of what [`Input::spare`]
    /// returned.
    pub(crate) fn filled(&mut self, n: usize) {
        self.end = self.end.saturating_add(n).min(self.buf.len());
    }

    /// The bytes not consumed yet.
    pub(crate) fn data(&self) -> &[u8] {
        self.buf.get(self.pos..self.end).unwrap_or_default()
    }

    pub(crate) fn consume(&mut self, n: usize) {
        self.pos = self.pos.saturating_add(n).min(self.end);
    }

    pub(crate) const fn is_empty(&self) -> bool {
        self.pos == self.end
    }
}

/// The logical payload being read, while its packets arrive.
#[derive(Debug, Clone, Copy)]
struct Partial {
    /// Where the payload starts in the caller's buffer.
    start: usize,
    /// Bytes of the current packet still to come; `None` between two
    /// packets of a split payload, until the next header.
    left: Option<usize>,
    /// Whether the current packet ends the payload.
    last: bool,
}

/// Ordinary framing state: sequence and the payload being read.
#[derive(Debug)]
struct Frames {
    seq: u8,
    /// Largest logical payload accepted.
    payload_limit: usize,
    partial: Option<Partial>,
}

impl Frames {
    /// Appends to `out` the bytes of `data` that belong to the payload
    /// being read and returns how many it used, with where the payload
    /// landed once it is whole. `check_seq` is off on compressed
    /// connections (see [`crate::compress`]).
    fn parse(
        &mut self,
        check_seq: bool,
        data: &[u8],
        out: &mut Vec<u8>,
    ) -> Result<(usize, Option<Range<usize>>)> {
        let mut used = 0;
        loop {
            let rest = data.get(used..).unwrap_or_default();
            let (mut partial, left) = match self.partial {
                Some(
                    partial @ Partial {
                        left: Some(left), ..
                    },
                ) => (partial, left),
                previous => {
                    let Some(&[l0, l1, l2, seq]) = rest.first_chunk::<HEADER_LEN>() else {
                        return Ok((used, None));
                    };
                    // Compressed, only the compressed sequence is checked:
                    // servers move the ordinary one to it whenever they
                    // flush (see `compress`), and the compressed layer
                    // keeps us in sync.
                    if check_seq && seq != self.seq {
                        return Err(Error::protocol(format!(
                            "packet out of order: expected sequence {}, got {seq}",
                            self.seq
                        )));
                    }
                    self.seq = seq.wrapping_add(1);
                    let len = usize::try_from(u32::from_le_bytes([l0, l1, l2, 0]))
                        .map_err(|_| Error::protocol("packet length exceeds address space"))?;
                    let start = previous.map_or(out.len(), |partial| partial.start);
                    let read = out.len().saturating_sub(start);
                    if read.saturating_add(len) > self.payload_limit {
                        return Err(Error::protocol(format!(
                            "the server sent a payload over the {} byte limit",
                            self.payload_limit
                        )));
                    }
                    used += HEADER_LEN;
                    if rest.len() - HEADER_LEN < len {
                        // It comes in parts: room for it once, as the
                        // header (checked above) announces.
                        out.reserve(len);
                    }
                    let partial = Partial {
                        start,
                        left: Some(len),
                        last: len < MAX_PACKET_PAYLOAD,
                    };
                    (partial, len)
                }
            };
            let rest = data.get(used..).unwrap_or_default();
            let n = left.min(rest.len());
            out.extend_from_slice(rest.get(..n).unwrap_or_default());
            used += n;
            if n < left {
                partial.left = Some(left - n);
                self.partial = Some(partial);
                return Ok((used, None));
            }
            if partial.last {
                self.partial = None;
                return Ok((used, Some(partial.start..out.len())));
            }
            partial.left = None;
            self.partial = Some(partial);
        }
    }
}

/// Packet framing of one connection, without I/O.
#[derive(Debug)]
pub(crate) struct Framer {
    frames: Frames,
    /// Compressed layer, once negotiated and authenticated.
    compressed: Option<Box<Compressed>>,
}

impl Default for Framer {
    fn default() -> Self {
        Self {
            frames: Frames {
                seq: 0,
                payload_limit: HANDSHAKE_PAYLOAD_LIMIT,
                partial: None,
            },
            compressed: None,
        }
    }
}

impl Framer {
    /// Appends the next logical payload of `input` to `out` (joining split
    /// packets) and returns where it landed, or `None` until more bytes
    /// arrive. What `input` held is consumed either way, and the next call
    /// goes on where this one stopped: it must get the same `out`.
    /// Appending lets result-set rows accumulate in a single contiguous
    /// buffer.
    pub(crate) fn read_payload(
        &mut self,
        input: &mut Input,
        out: &mut Vec<u8>,
    ) -> Result<Option<Range<usize>>> {
        let Some(compressed) = &mut self.compressed else {
            // Nearly every payload (a row) is one packet the buffer holds
            // whole: taken in one step (the general path below costs half
            // as much again per row).
            if self.frames.partial.is_none()
                && let Some((&[l0, l1, l2, seq], rest)) =
                    input.data().split_first_chunk::<HEADER_LEN>()
                && seq == self.frames.seq
            {
                let len = usize::from(l0) | usize::from(l1) << 8 | usize::from(l2) << 16;
                if len < MAX_PACKET_PAYLOAD
                    && len <= self.frames.payload_limit
                    && let Some(body) = rest.get(..len)
                {
                    self.frames.seq = seq.wrapping_add(1);
                    let start = out.len();
                    out.extend_from_slice(body);
                    input.consume(HEADER_LEN + len);
                    return Ok(Some(start..out.len()));
                }
            }
            let (used, payload) = self.frames.parse(true, input.data(), out)?;
            input.consume(used);
            return Ok(payload);
        };
        loop {
            let (used, payload) = self.frames.parse(false, compressed.plain(), out)?;
            compressed.consume(used);
            if payload.is_some() {
                return Ok(payload);
            }
            let (used, decoded) = compressed.decode(input.data())?;
            input.consume(used);
            if !decoded {
                return Ok(None);
            }
        }
    }

    /// Bytes of the current packet's body still to come, when they can be
    /// read straight into the caller's buffer (uncompressed connections).
    fn body_left(&self) -> usize {
        match (&self.compressed, self.frames.partial) {
            (
                None,
                Some(Partial {
                    left: Some(left), ..
                }),
            ) => left,
            _ => 0,
        }
    }

    /// `n` bytes of the current packet's body were appended to the
    /// caller's buffer directly (see [`Framer::body_left`]).
    fn body_read(&mut self, n: usize) {
        if let Some(Partial {
            left: Some(left), ..
        }) = &mut self.frames.partial
        {
            *left = left.saturating_sub(n);
        }
    }

    /// The error for a stream that ended before the payload did.
    fn end_of_stream(&self) -> io::Error {
        let in_packet = match &self.compressed {
            None => matches!(self.frames.partial, Some(Partial { left: Some(_), .. })),
            Some(compressed) => compressed.receiving(),
        };
        if in_packet {
            io::Error::from(io::ErrorKind::UnexpectedEof)
        } else {
            io::Error::new(io::ErrorKind::UnexpectedEof, "failed to fill whole buffer")
        }
    }

    /// Whether nothing of a payload is half read.
    fn is_idle(&self) -> bool {
        self.frames.partial.is_none()
            && self
                .compressed
                .as_ref()
                .is_none_or(|compressed| compressed.plain().is_empty() && !compressed.receiving())
    }

    /// Raises the payload limit from [`HANDSHAKE_PAYLOAD_LIMIT`] to
    /// [`PAYLOAD_LIMIT`], once authenticated.
    pub(crate) const fn authenticated(&mut self) {
        self.frames.payload_limit = PAYLOAD_LIMIT;
    }

    /// From now on, every byte travels in compressed packets (once
    /// authentication succeeds: its last packet is not compressed).
    pub(crate) fn compress(&mut self, algorithm: Compression) -> Result<()> {
        self.compressed = Some(Box::new(Compressed::new(algorithm)?));
        Ok(())
    }

    /// Every client command starts a new sequence, compressed one included.
    pub(crate) fn reset_seq(&mut self) {
        self.frames.seq = 0;
        if let Some(compressed) = &mut self.compressed {
            compressed.set_seq(0);
        }
    }

    /// Sets the sequence ids the next packets read must carry (the start of
    /// a pipelined reply).
    pub(crate) fn expect_reply(&mut self, start: ReplyStart) {
        self.frames.seq = start.seq;
        if let Some(compressed) = &mut self.compressed {
            compressed.set_seq(start.compressed);
        }
    }

    /// The header of the next packet, carrying `len` bytes.
    fn next_header(&mut self, len: usize) -> Result<[u8; HEADER_LEN]> {
        let len = u32::try_from(len)
            .ok()
            .filter(|&l| l as usize <= MAX_PACKET_PAYLOAD)
            .ok_or_else(|| Error::protocol("packet chunk too large"))?;
        let [l0, l1, l2, _] = len.to_le_bytes();
        let header = [l0, l1, l2, self.frames.seq];
        self.frames.seq = self.frames.seq.wrapping_add(1);
        Ok(header)
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
}

/// [`Framer`] over a blocking byte stream.
#[derive(Debug)]
pub(crate) struct PacketStream<S: Read + Write> {
    stream: S,
    input: Input,
    framer: Framer,
    write_buf: Vec<u8>,
}

impl<S: Read + Write> PacketStream<S> {
    pub(crate) fn new(stream: S) -> Self {
        Self {
            stream,
            input: Input::default(),
            framer: Framer::default(),
            write_buf: Vec::new(),
        }
    }

    /// Raises the payload limit from [`HANDSHAKE_PAYLOAD_LIMIT`] to
    /// [`PAYLOAD_LIMIT`], once authenticated.
    pub(crate) const fn authenticated(&mut self) {
        self.framer.authenticated();
    }

    /// Hands the raw stream back (to wrap it in TLS) with the sequence id to
    /// continue from. Fails if the server already sent bytes we buffered.
    pub(crate) fn into_parts(self) -> Result<(S, u8)> {
        if !self.input.is_empty() || !self.framer.is_idle() {
            return Err(Error::protocol("unexpected data before the TLS handshake"));
        }
        Ok((self.stream, self.framer.frames.seq))
    }

    pub(crate) fn from_parts(stream: S, seq: u8) -> Self {
        let mut packets = Self::new(stream);
        packets.framer.frames.seq = seq;
        packets
    }

    pub(crate) const fn get_ref(&self) -> &S {
        &self.stream
    }

    pub(crate) const fn get_mut(&mut self) -> &mut S {
        &mut self.stream
    }

    /// From now on, every byte travels in compressed packets (once
    /// authentication succeeds: its last packet is not compressed).
    pub(crate) fn compress(&mut self, algorithm: Compression) -> Result<()> {
        self.framer.compress(algorithm)
    }

    /// Every client command starts a new sequence, compressed one included.
    pub(crate) fn reset_seq(&mut self) {
        self.framer.reset_seq();
    }

    /// Reads one logical payload (joining split packets) and appends it to
    /// `out`, returning where it landed. Appending lets result-set rows
    /// accumulate in a single contiguous buffer.
    pub(crate) fn read_into(&mut self, out: &mut Vec<u8>) -> Result<Range<usize>> {
        loop {
            if let Some(payload) = self.framer.read_payload(&mut self.input, out)? {
                return Ok(payload);
            }
            // The buffer is empty (framing took every body byte it had): a
            // large rest of a body is read straight into `out`, as it was
            // reserved, without passing through the buffer.
            let left = self.framer.body_left();
            if left >= READ_BUFFER_CAPACITY {
                let wanted =
                    u64::try_from(left).map_err(|_| Error::protocol("packet length too large"))?;
                let read = (&mut self.stream).take(wanted).read_to_end(out)?;
                if read < left {
                    return Err(self.framer.end_of_stream().into());
                }
                self.framer.body_read(read);
                continue;
            }
            self.fill()?;
        }
    }

    /// Reads what the stream has (at least a byte) into the input buffer.
    fn fill(&mut self) -> Result<()> {
        loop {
            match self.stream.read(self.input.spare(READ_BUFFER_CAPACITY)) {
                Ok(0) => return Err(self.framer.end_of_stream().into()),
                Ok(n) => {
                    self.input.filled(n);
                    return Ok(());
                }
                Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                Err(err) => return Err(err.into()),
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
        let framer = &mut self.framer;
        for arg in args {
            framer.reset_seq();
            let compressed = if framer.compressed.is_some() {
                command.clear();
                encoded = framer.encode_parts(&mut command, &[cmd], arg);
                framer.compressed.as_mut().map_or(0, |layer| {
                    layer.encode_into(&command, &mut buf);
                    layer.seq()
                })
            } else {
                encoded = framer.encode_parts(&mut buf, &[cmd], arg);
                0
            };
            if encoded.is_err() {
                break;
            }
            replies.push(ReplyStart {
                seq: framer.frames.seq,
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
        self.framer.expect_reply(start);
    }

    /// Sends `head ++ body` as one logical payload without concatenating
    /// them.
    fn write_parts(&mut self, head: &[u8], body: &[u8]) -> Result<()> {
        if self.framer.compressed.is_none() && head.len() + body.len() >= KEEP_CAPACITY {
            return self.write_direct(head, body);
        }
        let mut buf = std::mem::take(&mut self.write_buf);
        buf.clear();
        let result = self
            .framer
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
            let header = self.framer.next_header(in_head.len() + in_body.len())?;
            self.stream.write_all(&header)?;
            self.stream.write_all(in_head)?;
            self.stream.write_all(in_body)?;
        }
        Ok(self.stream.flush()?)
    }

    /// Sends packets, wrapping them in compressed ones if need be.
    fn send(&mut self, buf: &[u8]) -> Result<()> {
        match &mut self.framer.compressed {
            None => self.send_framed(buf),
            Some(compressed) => {
                let stream = &mut self.stream;
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
        let stream = &mut self.stream;
        Ok(stream.write_all(bytes).and_then(|()| stream.flush())?)
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::io::Read;
    use std::io::Write;

    use super::Framer;
    use super::HANDSHAKE_PAYLOAD_LIMIT;
    use super::HEADER_LEN;
    use super::Input;
    use super::KEEP_CAPACITY;
    use super::PAYLOAD_LIMIT;
    use super::PacketStream;
    use crate::MAX_PACKET_PAYLOAD;
    use crate::compress::Compressed;
    use crate::compress::Compression;

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
        ps.stream.output
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
        ps.framer.frames.payload_limit = MAX_PACKET_PAYLOAD + 9;
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
        let out = ps.stream.output;
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
        let out = ps.stream.output;
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
                ps.stream.output == framed(&payload, 0),
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

    /// What the framer makes of `wire` delivered in pieces whose sizes
    /// `piece` gives in turn: every payload, then the error that stopped
    /// it (if any). Payloads are taken out as they complete.
    fn in_pieces(
        wire: &[u8],
        compression: Option<Compression>,
        mut piece: impl FnMut() -> usize,
    ) -> (Vec<Vec<u8>>, Option<String>) {
        let mut framer = Framer::default();
        framer.authenticated();
        if let Some(algorithm) = compression {
            framer.compress(algorithm).unwrap();
        }
        let mut input = Input::default();
        let mut out = Vec::new();
        let mut payloads = Vec::new();
        let mut sent = 0;
        loop {
            match framer.read_payload(&mut input, &mut out) {
                Ok(Some(range)) => {
                    payloads.push(out[range].to_vec());
                    out.clear();
                }
                Ok(None) if sent == wire.len() => return (payloads, None),
                Ok(None) => {
                    let n = piece().clamp(1, wire.len() - sent);
                    input.spare(n)[..n].copy_from_slice(&wire[sent..sent + n]);
                    input.filled(n);
                    sent += n;
                }
                Err(err) => return (payloads, Some(err.to_string())),
            }
        }
    }

    /// `payloads` framed back to back, the sequence going on from one to
    /// the next as in a result set.
    fn stream_of(payloads: &[&[u8]]) -> Vec<u8> {
        let mut wire = Vec::new();
        let mut seq = 0u8;
        for payload in payloads {
            wire.extend_from_slice(&framed(payload, seq));
            // One packet per full maximum, then the one that ends it.
            let packets = u8::try_from(payload.len() / MAX_PACKET_PAYLOAD + 1).unwrap();
            seq = seq.wrapping_add(packets);
        }
        wire
    }

    /// The same bytes give the same payloads and the same error however
    /// they are split: at every single point, and byte by byte.
    #[test]
    fn framing_does_not_depend_on_how_bytes_arrive() {
        let ok = stream_of(&[b"first", b"", &[b'r'; 300], b"last"]);
        let mut bad = ok.clone();
        // The third packet's sequence id, out of order.
        bad[4 + 5 + 4 + 3] = 9;
        let truncated = ok[..ok.len() - 2].to_vec();
        for wire in [ok, bad, truncated] {
            let whole = in_pieces(&wire, None, || usize::MAX);
            assert_ne!(whole.0.len(), 0);
            for cut in 1..wire.len() {
                let mut sizes = [cut, usize::MAX].into_iter();
                let split = in_pieces(&wire, None, || sizes.next().unwrap_or(usize::MAX));
                assert_eq!(split, whole, "cut at {cut}");
            }
            assert_eq!(in_pieces(&wire, None, || 1), whole);
        }
    }

    #[test]
    fn split_payloads_do_not_depend_on_how_bytes_arrive() {
        let big: Vec<u8> = (0..MAX_PACKET_PAYLOAD + 100)
            .map(|i| u8::try_from(i % 251).unwrap())
            .collect();
        let exact = vec![b'e'; MAX_PACKET_PAYLOAD];
        let wire = stream_of(&[b"a", &big, &exact, b"z"]);
        let whole = in_pieces(&wire, None, || usize::MAX);
        assert_eq!(whole.0.len(), 4);
        assert_eq!(whole.0[1], big);
        assert_eq!(whole.0[2], exact);
        assert_eq!(whole.1, None);
        // Pieces of varying sizes, so the cuts fall in headers and bodies
        // alike.
        let mut n = 0;
        let split = in_pieces(&wire, None, || {
            n = (n * 7 + 3) % 70_001;
            n
        });
        assert_eq!(split, whole);
    }

    /// Compressed, ordinary packets straddle compressed ones anywhere
    /// (headers included): the framing is the same at every split of the
    /// ordinary stream and of the compressed one.
    #[test]
    fn compressed_framing_does_not_depend_on_packet_boundaries() {
        let plain = stream_of(&[b"first", &[b'r'; 300], b"", b"last row of the set"]);
        for algorithm in [Compression::Zlib, Compression::Zstd { level: 3 }] {
            let expected = in_pieces(&plain, None, || usize::MAX);
            for cut in (1..plain.len()).step_by(3) {
                let mut writer = Compressed::new(algorithm).unwrap();
                let mut wire = Vec::new();
                writer.encode_into(&plain[..cut], &mut wire);
                writer.encode_into(&plain[cut..], &mut wire);
                assert_eq!(
                    in_pieces(&wire, Some(algorithm), || usize::MAX),
                    expected,
                    "{algorithm:?}, compressed packets split at {cut}"
                );
                assert_eq!(in_pieces(&wire, Some(algorithm), || 1), expected);
            }
        }
    }

    #[test]
    fn a_stream_that_ends_early_reports_where() {
        let mut ps = reader(vec![10, 0]);
        let err = ps.read_into(&mut Vec::new()).unwrap_err();
        assert_eq!(err.to_string(), "failed to fill whole buffer");
        let mut ps = reader(vec![10, 0, 0, 0, b'a']);
        let err = ps.read_into(&mut Vec::new()).unwrap_err();
        assert_eq!(err.to_string(), "unexpected end of file");
    }

    #[test]
    fn large_bodies_are_read_past_the_buffer() {
        // A body much larger than the read buffer, arriving in small reads:
        // the part the buffer does not hold goes straight to `out`.
        let payload: Vec<u8> = (0..300_000)
            .map(|i| u8::try_from(i % 241).unwrap())
            .collect();
        let mut ps = PacketStream::new(Trickle(Cursor::new(framed(&payload, 0))));
        ps.authenticated();
        let mut out = Vec::new();
        assert_eq!(ps.read_into(&mut out).unwrap(), 0..payload.len());
        assert!(out == payload);
        assert!(ps.input.buf.len() <= super::READ_BUFFER_CAPACITY + HEADER_LEN);
    }
}
