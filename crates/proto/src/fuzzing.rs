//! Fuzzing entry points (`fuzzing` feature only, used by `fuzz/`).
//!
//! Every function feeds untrusted bytes to one parser or to the whole client
//! state machine and ignores the result: the property under test is that no
//! input panics, hangs or allocates without bound.

use std::io;
use std::io::Cursor;
use std::io::Read;
use std::io::Write;

use crate::Conn;
use crate::Opts;
use crate::column::Column;
use crate::conn::Stream;
use crate::constants::capability;
use crate::handshake::ServerHandshake;
use crate::packet::PacketStream;
use crate::result::RowSet;
use crate::result::accept_row_packet;
use crate::result::parse_eof;
use crate::result::parse_err;
use crate::result::parse_ok;
use crate::text;
use crate::tls::SslMode;
use crate::tls::TlsOpts;

/// A fake server that replies with fixed bytes and swallows writes.
#[derive(Debug)]
pub struct MockStream(Cursor<Vec<u8>>);

impl Read for MockStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.0.read(buf)
    }
}

impl Write for MockStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// The server greeting parser.
pub fn handshake(data: &[u8]) {
    let _ = ServerHandshake::parse(data);
}

/// Packet framing: reads logical packets until the stream fails.
pub fn packets(data: &[u8]) {
    let mut stream = PacketStream::new(MockStream(Cursor::new(data.to_vec())));
    let mut out = Vec::new();
    while stream.read_into(&mut out).is_ok() {}
}

/// Column definitions, rows and terminators. The first byte picks the
/// column count, the rest is split into payloads on 0xFF 0xFF separators.
pub fn result_set(data: &[u8]) {
    let Some((&columns, rest)) = data.split_first() else {
        return;
    };
    let mut rows = RowSet::new_for_fuzzing(usize::from(columns % 16));
    for payload in rest.split(|&b| b == 0xFF).filter(|p| !p.is_empty()) {
        let _ = Column::parse(payload);
        let _ = parse_ok(payload);
        let _ = parse_eof(payload);
        let _ = parse_err(payload);
        let start = rows.data_mut().len();
        rows.data_mut().extend_from_slice(payload);
        let end = rows.data_mut().len();
        for deprecate_eof in [true, false] {
            let _ = accept_row_packet(&mut rows, start..end, deprecate_eof);
        }
    }
    for row in rows.rows() {
        for cell in row.cells() {
            let _ = cell;
        }
    }
}

/// Text-protocol value parsers.
pub fn text_values(data: &[u8]) {
    let _ = text::parse_i64(data);
    let _ = text::parse_u64(data);
    let _ = text::parse_f64(data);
    let _ = text::parse_date(data);
    let _ = text::parse_datetime(data);
    let _ = text::parse_time(data);
}

/// The whole client against a server that answers with `data`: handshake,
/// authentication (every plugin, RSA, auth switches), queries, multiple
/// results and the other commands.
pub fn session(data: &[u8]) {
    let opts = Opts {
        user: "fuzz".to_owned(),
        password: "secret".to_owned(),
        database: Some("db".to_owned()),
        client_flag: capability::MULTI_STATEMENTS,
        tls: TlsOpts {
            mode: SslMode::Disabled,
            ..TlsOpts::default()
        },
        ..Opts::default()
    };
    let transport = Stream::Mock(MockStream(Cursor::new(data.to_vec())));
    let Ok(mut conn) = Conn::establish(transport, &opts) else {
        return;
    };
    if conn.query(b"SELECT 1").is_ok() {
        while let Ok(Some(_)) = conn.next_result() {}
    }
    let _ = conn.ping();
    let _ = conn.select_db("db");
    let _ = conn.reset_connection();
    let _ = conn.change_user("other", "pw", Some("db"));
    let _ = conn.query(b"SELECT 2");
}
