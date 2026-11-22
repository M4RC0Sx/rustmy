//! MySQL/MariaDB client/server wire protocol: packet framing, handshake,
//! authentication and result-set decoding.
//!
//! This crate is deliberately free of `PyO3` so it can be unit-tested,
//! benchmarked and fuzzed without a Python interpreter.

// The protocol core parses untrusted server bytes: it stays 100% safe Rust.
#![forbid(unsafe_code)]

mod auth;
pub mod binary;
mod buf;
pub mod charset;
mod column;
mod compress;
mod conn;
pub mod constants;
mod error;
#[cfg(feature = "fuzzing")]
#[doc(hidden)]
pub mod fuzzing;
mod handshake;
mod login;
mod packet;
mod result;
pub mod sql;
pub mod text;
mod tls;

pub use binary::Execute;
pub use column::Column;
pub use compress::Compression;
pub use conn::Conn;
pub use conn::DEFAULT_STATEMENT_CACHE;
pub use conn::Opts;
pub use conn::PipelineTotals;
pub use conn::ReadOutcome;
pub use conn::Statement;
pub use conn::StreamId;
pub use error::Error;
pub use error::Result;
pub use result::QueryResult;
pub use result::RowRef;
pub use result::RowSet;
pub use tls::SslMode;
pub use tls::TlsOpts;

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
