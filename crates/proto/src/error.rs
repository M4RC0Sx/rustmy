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
    #[error("({code}, {:?})", String::from_utf8_lossy(.message))]
    Server {
        /// MySQL error number, e.g. 1064 for a syntax error.
        code: u16,
        /// Five-character SQLSTATE, empty when the server did not send one.
        sql_state: String,
        /// Human-readable message from the server, in the session's
        /// character set (`character_set_results`) once connected.
        message: Vec<u8>,
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

    /// The server asked for a local file (`LOAD DATA LOCAL INFILE`) that
    /// this connection may not send; an empty one went instead.
    #[error("LOAD DATA LOCAL INFILE file request rejected: {0}")]
    LocalInfile(String),

    /// A prepared statement executed with another number of parameters
    /// than its placeholders.
    #[error("the statement has {expected} placeholders but {given} parameters were given")]
    Parameters { expected: u16, given: usize },
}

impl Error {
    pub(crate) fn protocol(message: impl Into<String>) -> Self {
        Self::Protocol(message.into())
    }

    pub(crate) fn unsupported(message: impl Into<String>) -> Self {
        Self::Unsupported(message.into())
    }
}
