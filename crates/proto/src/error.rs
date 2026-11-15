//! Error type shared by the whole protocol core.

use std::io;

/// Result alias used throughout the crate.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Everything that can go wrong while talking to a server.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// Socket-level failure (connection refused, reset, timeout, ...).
    #[error(transparent)]
    Io(#[from] io::Error),

    /// The server sent bytes that do not follow the protocol.
    #[error("malformed packet: {0}")]
    Protocol(String),

    /// The server answered with an ERR packet.
    #[error("({code}, {message:?})")]
    Server {
        /// MySQL error number, e.g. 1064 for a syntax error.
        code: u16,
        /// Five-character SQLSTATE, empty when the server did not send one.
        sql_state: String,
        /// Human-readable message from the server.
        message: String,
    },

    /// Invalid connection settings, e.g. an unreadable key file.
    #[error("invalid configuration: {0}")]
    Config(String),

    /// TLS setup or handshake failure (bad certificate, unreadable file...).
    #[error("TLS error: {0}")]
    Tls(String),

    /// Rows of a streamed result set were requested after another command
    /// discarded them.
    #[error("result set discarded: another command ran before all its rows were read")]
    ResultDiscarded,

    /// A valid server request that this client does not implement yet.
    #[error("unsupported: {0}")]
    Unsupported(String),
}

impl Error {
    pub(crate) fn protocol(message: impl Into<String>) -> Self {
        Self::Protocol(message.into())
    }

    pub(crate) fn unsupported(message: impl Into<String>) -> Self {
        Self::Unsupported(message.into())
    }
}
