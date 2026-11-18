//! Protocol constants: capability flags, server status, commands and column
//! metadata.

/// Client/server capability flags exchanged during the handshake.
pub mod capability {
    pub const LONG_PASSWORD: u32 = 1;
    pub const FOUND_ROWS: u32 = 1 << 1;
    pub const LONG_FLAG: u32 = 1 << 2;
    pub const CONNECT_WITH_DB: u32 = 1 << 3;
    pub const COMPRESS: u32 = 1 << 5;
    pub const LOCAL_FILES: u32 = 1 << 7;
    pub const PROTOCOL_41: u32 = 1 << 9;
    pub const SSL: u32 = 1 << 11;
    pub const TRANSACTIONS: u32 = 1 << 13;
    pub const SECURE_CONNECTION: u32 = 1 << 15;
    pub const MULTI_STATEMENTS: u32 = 1 << 16;
    pub const MULTI_RESULTS: u32 = 1 << 17;
    pub const PS_MULTI_RESULTS: u32 = 1 << 18;
    pub const PLUGIN_AUTH: u32 = 1 << 19;
    pub const CONNECT_ATTRS: u32 = 1 << 20;
    pub const PLUGIN_AUTH_LENENC_CLIENT_DATA: u32 = 1 << 21;
    pub const SESSION_TRACK: u32 = 1 << 23;
    pub const DEPRECATE_EOF: u32 = 1 << 24;
    /// MySQL 8.0.18+: zstd for the compressed protocol.
    pub const ZSTD_COMPRESSION_ALGORITHM: u32 = 1 << 26;
}

/// Server status flags reported in OK and EOF packets.
pub mod status {
    pub const IN_TRANS: u16 = 1;
    pub const AUTOCOMMIT: u16 = 1 << 1;
    pub const MORE_RESULTS_EXISTS: u16 = 1 << 3;
    pub const NO_BACKSLASH_ESCAPES: u16 = 1 << 9;
}

/// Command bytes (first byte of a client command packet).
pub(crate) mod command {
    pub(crate) const QUIT: u8 = 0x01;
    pub(crate) const INIT_DB: u8 = 0x02;
    pub(crate) const QUERY: u8 = 0x03;
    pub(crate) const STMT_PREPARE: u8 = 0x16;
    pub(crate) const STMT_EXECUTE: u8 = 0x17;
    pub(crate) const STMT_CLOSE: u8 = 0x19;
    pub(crate) const PING: u8 = 0x0E;
    pub(crate) const CHANGE_USER: u8 = 0x11;
    pub(crate) const RESET_CONNECTION: u8 = 0x1F;
}

/// Column definition flags.
pub mod column_flag {
    pub const NOT_NULL: u16 = 1;
    pub const PRI_KEY: u16 = 1 << 1;
    pub const UNIQUE_KEY: u16 = 1 << 2;
    pub const MULTIPLE_KEY: u16 = 1 << 3;
    pub const BLOB: u16 = 1 << 4;
    pub const UNSIGNED: u16 = 1 << 5;
    pub const ZEROFILL: u16 = 1 << 6;
    pub const BINARY: u16 = 1 << 7;
    pub const ENUM: u16 = 1 << 8;
    pub const AUTO_INCREMENT: u16 = 1 << 9;
    pub const TIMESTAMP: u16 = 1 << 10;
    pub const SET: u16 = 1 << 11;
    pub const NUM: u16 = 1 << 15;
}

/// Column type as sent in a column definition.
///
/// Kept as an open newtype rather than an enum so that types added by future
/// server versions decode instead of failing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ColumnType(pub u8);

impl ColumnType {
    pub const DECIMAL: Self = Self(0);
    pub const TINY: Self = Self(1);
    pub const SHORT: Self = Self(2);
    pub const LONG: Self = Self(3);
    pub const FLOAT: Self = Self(4);
    pub const DOUBLE: Self = Self(5);
    pub const NULL: Self = Self(6);
    pub const TIMESTAMP: Self = Self(7);
    pub const LONGLONG: Self = Self(8);
    pub const INT24: Self = Self(9);
    pub const DATE: Self = Self(10);
    pub const TIME: Self = Self(11);
    pub const DATETIME: Self = Self(12);
    pub const YEAR: Self = Self(13);
    pub const NEWDATE: Self = Self(14);
    pub const VARCHAR: Self = Self(15);
    pub const BIT: Self = Self(16);
    pub const TIMESTAMP2: Self = Self(17);
    pub const DATETIME2: Self = Self(18);
    pub const TIME2: Self = Self(19);
    pub const VECTOR: Self = Self(242);
    pub const JSON: Self = Self(245);
    pub const NEWDECIMAL: Self = Self(246);
    pub const ENUM: Self = Self(247);
    pub const SET: Self = Self(248);
    pub const TINY_BLOB: Self = Self(249);
    pub const MEDIUM_BLOB: Self = Self(250);
    pub const LONG_BLOB: Self = Self(251);
    pub const BLOB: Self = Self(252);
    pub const VAR_STRING: Self = Self(253);
    pub const STRING: Self = Self(254);
    pub const GEOMETRY: Self = Self(255);
}

/// Collation id the server uses for binary strings.
pub const BINARY_COLLATION: u16 = 63;
