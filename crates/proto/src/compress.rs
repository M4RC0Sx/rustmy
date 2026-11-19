//! Compressed protocol (`CLIENT_COMPRESS`, `CLIENT_ZSTD_COMPRESSION_ALGORITHM`).
//!
//! Once authentication succeeds, every byte of the ordinary packet stream
//! travels inside compressed packets: a 7-byte header (payload length,
//! compressed sequence id, uncompressed length) and a zlib or zstd payload.
//! An uncompressed length of 0 means the payload is stored as is. One
//! compressed packet may carry several ordinary packets or part of one, so
//! this layer is a byte stream under [`crate::packet::PacketStream`].
//!
//! The compressed sequence restarts with every command, like the ordinary
//! one, and a reply continues the command's. Only the compressed sequence
//! is checked: a server copies it into the ordinary one whenever it flushes
//! (`net_flush` in MySQL's `net_serv.cc`, which reads inner packets by
//! length only), so the second result of a multi-statement query starts at
//! whatever sequence the compressed one reached. Pipelined commands each go
//! in compressed packets of their own, so every reply starts at compressed
//! sequence 1, as verified on all nine supported servers.

use std::io;
use std::io::Read;

use flate2::Compress;
use flate2::Decompress;
use flate2::FlushCompress;
use flate2::FlushDecompress;
use flate2::Status;

use crate::error::Error;
use crate::error::Result;
use crate::packet::release;

const HEADER_LEN: usize = 7;
/// Largest payload, compressed or not, one compressed packet can declare.
const MAX_PAYLOAD: usize = 0xFF_FFFF;
/// Payloads shorter than this go uncompressed, as MySQL's own client does.
const MIN_COMPRESS_LENGTH: usize = 50;
/// zlib level of MySQL's client (zlib's default).
const ZLIB_LEVEL: u32 = 6;

/// Compression algorithm of a connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compression {
    Zlib,
    /// MySQL 8.0.18+ only, with its level (1 to 22; MySQL's default is 3).
    Zstd {
        level: u8,
    },
}

enum Codec {
    Zlib {
        compress: Box<Compress>,
        decompress: Box<Decompress>,
    },
    Zstd {
        compress: Box<zstd::bulk::Compressor<'static>>,
        decompress: Box<zstd::bulk::Decompressor<'static>>,
    },
}

impl std::fmt::Debug for Codec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Zlib { .. } => "Zlib",
            Self::Zstd { .. } => "Zstd",
        })
    }
}

/// State of the compressed layer of one connection.
#[derive(Debug)]
pub(crate) struct Compressed {
    codec: Codec,
    /// Compressed sequence id of the next packet, read or written.
    seq: u8,
    /// Decompressed bytes not handed out yet start at `pos`.
    input: Vec<u8>,
    pos: usize,
    /// Payload of the compressed packet being read.
    wire: Vec<u8>,
    /// Compressed packets being written ([`Compressed::encode`]).
    out: Vec<u8>,
    /// One compressed payload.
    scratch: Vec<u8>,
}

impl Compressed {
    pub(crate) fn new(algorithm: Compression) -> Result<Self> {
        let codec = match algorithm {
            Compression::Zlib => Codec::Zlib {
                compress: Box::new(Compress::new(flate2::Compression::new(ZLIB_LEVEL), true)),
                decompress: Box::new(Decompress::new(true)),
            },
            Compression::Zstd { level } => Codec::Zstd {
                compress: Box::new(zstd::bulk::Compressor::new(i32::from(level))?),
                decompress: Box::new(zstd::bulk::Decompressor::new()?),
            },
        };
        Ok(Self {
            codec,
            seq: 0,
            input: Vec::new(),
            pos: 0,
            wire: Vec::new(),
            out: Vec::new(),
            scratch: Vec::new(),
        })
    }

    /// Compressed sequence id of the next packet.
    pub(crate) const fn seq(&self) -> u8 {
        self.seq
    }

    /// Sets the compressed sequence id of the next packet: 0 for a new
    /// command, or where a pipelined reply starts.
    pub(crate) const fn set_seq(&mut self, seq: u8) {
        self.seq = seq;
    }

    /// Fills `buf` with decompressed bytes.
    pub(crate) fn read_exact(&mut self, source: &mut impl Read, buf: &mut [u8]) -> Result<()> {
        let mut filled = 0;
        while let Some(rest) = buf.get_mut(filled..).filter(|rest| !rest.is_empty()) {
            let available = self.available(source)?;
            let n = available.len().min(rest.len());
            if let (Some(dst), Some(src)) = (rest.get_mut(..n), available.get(..n)) {
                dst.copy_from_slice(src);
            }
            self.pos += n;
            filled += n;
        }
        self.release_consumed();
        Ok(())
    }

    /// Appends `len` decompressed bytes to `out`.
    pub(crate) fn append(
        &mut self,
        source: &mut impl Read,
        out: &mut Vec<u8>,
        len: usize,
    ) -> Result<()> {
        out.reserve(len);
        let mut left = len;
        while left > 0 {
            let available = self.available(source)?;
            let n = available.len().min(left);
            out.extend_from_slice(available.get(..n).unwrap_or_default());
            self.pos += n;
            left -= n;
        }
        self.release_consumed();
        Ok(())
    }

    /// Releases a large decompressed packet once all of it was handed out,
    /// rather than when the next one arrives (which, after the last reply,
    /// may be long).
    fn release_consumed(&mut self) {
        if self.pos >= self.input.len() {
            release(&mut self.input);
        }
    }

    /// Decompressed bytes not handed out yet, reading the next compressed
    /// packet when there are none.
    fn available(&mut self, source: &mut impl Read) -> Result<&[u8]> {
        while self.pos >= self.input.len() {
            self.fill(source)?;
        }
        Ok(self.input.get(self.pos..).unwrap_or_default())
    }

    /// Reads the next compressed packet and decompresses it into `input`.
    fn fill(&mut self, source: &mut impl Read) -> Result<()> {
        // Whatever happens below, nothing old is left to hand out.
        release(&mut self.input);
        self.input.clear();
        self.pos = 0;
        let mut header = [0u8; HEADER_LEN];
        source.read_exact(&mut header)?;
        let [c0, c1, c2, seq, u0, u1, u2] = header;
        if seq != self.seq {
            return Err(Error::protocol(format!(
                "compressed packet out of order: expected sequence {}, got {seq}",
                self.seq
            )));
        }
        self.seq = seq.wrapping_add(1);
        let wire_len = u32::from_le_bytes([c0, c1, c2, 0]);
        let plain_len = usize::try_from(u32::from_le_bytes([u0, u1, u2, 0]))
            .map_err(|_| Error::protocol("packet length exceeds address space"))?;
        release(&mut self.wire);
        self.wire.clear();
        let read = source
            .take(u64::from(wire_len))
            .read_to_end(&mut self.wire)?;
        if u64::try_from(read).ok() != Some(u64::from(wire_len)) {
            return Err(io::Error::from(io::ErrorKind::UnexpectedEof).into());
        }
        if plain_len == 0 {
            std::mem::swap(&mut self.input, &mut self.wire);
            return Ok(());
        }
        let decompressed = self.decompress(wire_len, plain_len);
        release(&mut self.wire);
        decompressed
    }

    /// Decompresses `wire` into `input`, which must end up `plain_len`
    /// bytes long.
    fn decompress(&mut self, wire_len: u32, plain_len: usize) -> Result<()> {
        // Room for what the header declares only: a payload that inflates
        // to more fails instead of growing the buffer (decompression bomb).
        self.input.reserve_exact(plain_len);
        let complete = match &mut self.codec {
            Codec::Zlib { decompress, .. } => {
                decompress.reset(true);
                let status =
                    decompress.decompress_vec(&self.wire, &mut self.input, FlushDecompress::Finish);
                matches!(status, Ok(Status::StreamEnd))
                    && decompress.total_in() == u64::from(wire_len)
            }
            Codec::Zstd { decompress, .. } => decompress
                .decompress_to_buffer(&self.wire, &mut self.input)
                .is_ok(),
        };
        if !complete || self.input.len() != plain_len {
            self.input.clear();
            return Err(Error::protocol("corrupt compressed packet"));
        }
        Ok(())
    }

    /// Wraps `data` in compressed packets of the current sequence and
    /// returns them.
    pub(crate) fn encode(&mut self, data: &[u8]) -> &[u8] {
        let mut out = std::mem::take(&mut self.out);
        out.clear();
        self.encode_into(data, &mut out);
        self.out = out;
        &self.out
    }

    /// Releases what [`Compressed::encode`] returned once it was sent, if a
    /// large payload grew it.
    pub(crate) fn release_output(&mut self) {
        release(&mut self.out);
    }

    /// Appends `data` to `out` as compressed packets of the current
    /// sequence.
    pub(crate) fn encode_into(&mut self, data: &[u8], out: &mut Vec<u8>) {
        // An empty payload still needs one (empty) packet.
        let mut chunks = data.chunks(MAX_PAYLOAD);
        let first = chunks.next().unwrap_or_default();
        for chunk in std::iter::once(first).chain(chunks) {
            let packed = chunk.len() >= MIN_COMPRESS_LENGTH && self.compress(chunk);
            let (payload, plain_len) = if packed {
                (self.scratch.as_slice(), chunk.len())
            } else {
                (chunk, 0)
            };
            let [c0, c1, c2, _] = u32::try_from(payload.len())
                .unwrap_or_default()
                .to_le_bytes();
            let [u0, u1, u2, _] = u32::try_from(plain_len).unwrap_or_default().to_le_bytes();
            out.extend_from_slice(&[c0, c1, c2, self.seq, u0, u1, u2]);
            out.extend_from_slice(payload);
            self.seq = self.seq.wrapping_add(1);
        }
        release(&mut self.scratch);
    }

    /// Compresses `chunk` into `scratch`; false when that would not make it
    /// smaller (it then goes as is).
    fn compress(&mut self, chunk: &[u8]) -> bool {
        self.scratch.clear();
        // Only worth it if smaller: give the codec no more room than that.
        self.scratch.reserve_exact(chunk.len());
        match &mut self.codec {
            Codec::Zlib { compress, .. } => {
                compress.reset();
                let status = compress.compress_vec(chunk, &mut self.scratch, FlushCompress::Finish);
                matches!(status, Ok(Status::StreamEnd)) && self.scratch.len() < chunk.len()
            }
            Codec::Zstd { compress, .. } => compress
                .compress_to_buffer(chunk, &mut self.scratch)
                .is_ok_and(|n| n < chunk.len()),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::Compressed;
    use super::Compression;
    use super::MAX_PAYLOAD;
    use crate::packet::KEEP_CAPACITY;

    const ALGORITHMS: [Compression; 2] = [Compression::Zlib, Compression::Zstd { level: 3 }];

    /// Bytes no codec can shrink.
    fn noise(len: u32) -> Vec<u8> {
        let mut state = 0x9E37_79B9_7F4A_7C15_u64;
        (0..len)
            .map(|_| {
                // xorshift64*
                state ^= state >> 12;
                state ^= state << 25;
                state ^= state >> 27;
                state.wrapping_mul(0x2545_F491_4F6C_DD1D).to_le_bytes()[7]
            })
            .collect()
    }

    /// What `writer` sends, read back by a fresh layer of the same kind.
    fn round_trip(algorithm: Compression, data: &[u8]) -> (Vec<u8>, Vec<u8>) {
        let mut writer = Compressed::new(algorithm).unwrap();
        let wire = writer.encode(data).to_vec();
        let mut reader = Compressed::new(algorithm).unwrap();
        let mut source = Cursor::new(wire.clone());
        let mut out = Vec::new();
        reader.append(&mut source, &mut out, data.len()).unwrap();
        assert_eq!(source.position(), wire.len() as u64, "all packets read");
        (wire, out)
    }

    fn header(wire: &[u8]) -> (usize, u8, usize) {
        let len = |b: &[u8]| usize::from(b[0]) | usize::from(b[1]) << 8 | usize::from(b[2]) << 16;
        (len(&wire[..3]), wire[3], len(&wire[4..7]))
    }

    #[test]
    fn compressible_data_shrinks_and_comes_back() {
        let data = b"SELECT id, name FROM customers WHERE name LIKE 'a%' ".repeat(40);
        for algorithm in ALGORITHMS {
            let (wire, out) = round_trip(algorithm, &data);
            assert_eq!(out, data);
            let (wire_len, seq, plain_len) = header(&wire);
            assert_eq!((seq, plain_len), (0, data.len()));
            assert_eq!(wire_len, wire.len() - 7);
            assert!(wire.len() < data.len() / 4, "{algorithm:?}: {}", wire.len());
        }
    }

    #[test]
    fn short_or_incompressible_payloads_go_as_is() {
        let random = noise(4000);
        for algorithm in ALGORITHMS {
            for data in [&b"\x03SELECT 1"[..], &random] {
                let (wire, out) = round_trip(algorithm, data);
                assert_eq!(out, data);
                assert_eq!(header(&wire), (data.len(), 0, 0));
            }
            let mut writer = Compressed::new(algorithm).unwrap();
            assert_eq!(
                writer.encode(b""),
                [0; 7],
                "an empty payload is one empty packet"
            );
        }
    }

    #[test]
    fn payloads_over_16_mib_span_several_packets() {
        let data = vec![b'x'; MAX_PAYLOAD + 10];
        for algorithm in ALGORITHMS {
            let (wire, out) = round_trip(algorithm, &data);
            assert_eq!(out, data);
            let (first_len, first_seq, first_plain) = header(&wire);
            assert_eq!((first_seq, first_plain), (0, MAX_PAYLOAD));
            let second = &wire[7 + first_len..];
            assert_eq!(header(second), (10, 1, 0), "the 10-byte tail goes as is");
        }
    }

    #[test]
    fn reads_across_packet_boundaries() {
        let mut writer = Compressed::new(Compression::Zlib).unwrap();
        let mut wire = Vec::new();
        writer.encode_into(&[b'a'; 100], &mut wire);
        writer.encode_into(b"bc", &mut wire);
        let mut reader = Compressed::new(Compression::Zlib).unwrap();
        let mut source = Cursor::new(wire);
        let mut head = [0u8; 99];
        reader.read_exact(&mut source, &mut head).unwrap();
        let mut across = [0u8; 2];
        reader.read_exact(&mut source, &mut across).unwrap();
        assert_eq!(&across, b"ab");
        assert_eq!(reader.seq(), 2);
    }

    #[test]
    fn rejects_out_of_order_packets() {
        let mut writer = Compressed::new(Compression::Zlib).unwrap();
        writer.set_seq(3);
        let wire = writer.encode(b"hello").to_vec();
        let mut reader = Compressed::new(Compression::Zlib).unwrap();
        let err = reader
            .read_exact(&mut Cursor::new(wire), &mut [0u8; 5])
            .unwrap_err();
        assert!(
            err.to_string().contains("expected sequence 0, got 3"),
            "{err}"
        );
    }

    /// A payload that inflates beyond its declared length (or short of it,
    /// or with trailing bytes) is refused, never buffered whole.
    #[test]
    fn rejects_payloads_that_do_not_match_their_header() {
        let data = vec![b'z'; 1 << 20];
        for algorithm in ALGORITHMS {
            let mut writer = Compressed::new(algorithm).unwrap();
            let wire = writer.encode(&data).to_vec();
            let (wire_len, _, _) = header(&wire);
            for declared in [16, data.len() - 1, data.len() + 1] {
                let mut forged = wire.clone();
                forged[4..7].copy_from_slice(&u32::try_from(declared).unwrap().to_le_bytes()[..3]);
                let mut reader = Compressed::new(algorithm).unwrap();
                let err = reader
                    .read_exact(&mut Cursor::new(forged), &mut [0u8; 1])
                    .unwrap_err();
                assert!(
                    err.to_string().contains("corrupt"),
                    "{algorithm:?} {declared}: {err}"
                );
                assert!(reader.input.capacity() < (1 << 20) + 4096);
                // Nothing of it is handed out later.
                assert_eq!(reader.input, b"");
            }
            if algorithm == Compression::Zlib {
                let mut trailing = wire.clone();
                trailing.push(0);
                trailing[..3]
                    .copy_from_slice(&u32::try_from(wire_len + 1).unwrap().to_le_bytes()[..3]);
                let mut reader = Compressed::new(algorithm).unwrap();
                assert!(
                    reader
                        .read_exact(&mut Cursor::new(trailing), &mut [0u8; 1])
                        .is_err()
                );
            }
        }
    }

    #[test]
    fn truncated_packets_are_an_error() {
        let mut writer = Compressed::new(Compression::Zlib).unwrap();
        let wire = writer.encode(&[b'q'; 500]).to_vec();
        for cut in [3, 7, wire.len() - 1] {
            let mut reader = Compressed::new(Compression::Zlib).unwrap();
            let truncated = wire[..cut].to_vec();
            assert!(
                reader
                    .read_exact(&mut Cursor::new(truncated), &mut [0u8; 1])
                    .is_err()
            );
        }
    }

    /// Found by the `session` fuzz target: a packet that failed to arrive
    /// right after a large one (whose buffer was released) left the read
    /// position past the end, and the next read spun forever on an empty
    /// slice instead of failing.
    #[test]
    fn a_failed_read_after_a_large_packet_is_not_a_hang() {
        let large = noise(2 << 20);
        let mut writer = Compressed::new(Compression::Zlib).unwrap();
        let mut wire = writer.encode(&large).to_vec();
        // The next packet's header, and then the stream ends.
        wire.extend_from_slice(&[10, 0, 0, writer.seq(), 0, 0, 0]);
        let mut reader = Compressed::new(Compression::Zlib).unwrap();
        let mut source = Cursor::new(wire);
        let mut out = Vec::new();
        reader.append(&mut source, &mut out, large.len()).unwrap();
        assert_eq!(out, large);
        for _ in 0..2 {
            assert!(reader.read_exact(&mut source, &mut [0u8; 4]).is_err());
        }
    }

    #[test]
    fn large_buffers_are_released_once_used() {
        for algorithm in ALGORITHMS {
            // Incompressible, so the packets written are as large as the
            // data; and compressible, so reading inflates them.
            for data in [noise(4 << 20), vec![b'z'; 4 << 20]] {
                let mut writer = Compressed::new(algorithm).unwrap();
                let wire = writer.encode(&data).to_vec();
                assert!(writer.scratch.capacity() <= KEEP_CAPACITY);
                writer.release_output();
                assert!(writer.out.capacity() <= KEEP_CAPACITY);

                let mut reader = Compressed::new(algorithm).unwrap();
                let mut out = Vec::new();
                reader
                    .append(&mut Cursor::new(wire), &mut out, data.len())
                    .unwrap();
                assert_eq!(out, data);
                // Released as soon as everything was handed out, without
                // waiting for another packet.
                assert!(reader.input.capacity() <= KEEP_CAPACITY);
                assert!(reader.wire.capacity() <= KEEP_CAPACITY);
            }
        }
    }
}
