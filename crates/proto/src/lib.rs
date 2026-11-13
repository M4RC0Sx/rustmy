//! MySQL/MariaDB client/server wire protocol: packet framing, handshake,
//! authentication and result-set decoding.
//!
//! This crate is deliberately free of `PyO3` so it can be unit-tested,
//! benchmarked and fuzzed without a Python interpreter.

// The protocol core parses untrusted server bytes: it stays 100% safe Rust.
#![forbid(unsafe_code)]

/// Maximum payload carried by a single protocol packet (2^24 - 1 bytes).
///
/// Larger payloads are split across consecutive packets.
pub const MAX_PACKET_PAYLOAD: usize = 0x00FF_FFFF;

#[cfg(test)]
mod tests {
    use super::MAX_PACKET_PAYLOAD;

    #[test]
    fn max_payload_fits_in_three_byte_length() {
        assert_eq!(MAX_PACKET_PAYLOAD, (1 << 24) - 1);
    }
}
