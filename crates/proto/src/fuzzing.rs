//! Fuzzing entry points (`fuzzing` feature only, used by `fuzz/`).
//!
//! Every function feeds untrusted bytes to one parser or to the whole client
//! state machine and ignores the result: the property under test is that no
//! input panics, hangs or allocates without bound.

use std::io;
use std::io::Cursor;
use std::io::Read;
use std::io::Write;
use std::path::PathBuf;
use std::sync::OnceLock;

use crate::Compression;
use crate::Conn;
use crate::Opts;
use crate::column::Column;
use crate::conn::Stream;
use crate::constants::ColumnType;
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

/// A fake server that hands its bytes out in pieces of varying sizes,
/// taken from the bytes themselves, as a socket may, and swallows writes.
#[derive(Debug)]
pub struct Pieces(Cursor<Vec<u8>>);

impl Read for Pieces {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let position = usize::try_from(self.0.position()).unwrap_or(usize::MAX);
        let byte = self.0.get_ref().get(position).copied().unwrap_or(0);
        let n = (1 + usize::from(byte) % 23).min(buf.len());
        self.0.read(buf.get_mut(..n).unwrap_or_default())
    }
}

impl Write for Pieces {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Packet framing: reads logical packets until the stream fails, from the
/// bytes whole and in pieces; both must give the same payloads and error.
pub fn packets(data: &[u8]) {
    let whole = read_packets(PacketStream::new(MockStream(Cursor::new(data.to_vec()))));
    let pieces = read_packets(PacketStream::new(Pieces(Cursor::new(data.to_vec()))));
    assert_eq!(whole, pieces, "framing depends on how the bytes arrive");
}

fn read_packets<S: Read + Write>(mut stream: PacketStream<S>) -> (Vec<u8>, String) {
    let mut out = Vec::new();
    loop {
        if let Err(err) = stream.read_into(&mut out) {
            return (out, err.to_string());
        }
    }
}

/// Compressed packet framing: the first byte picks zlib or zstd, the rest
/// is what the server sends once compression is on, read whole and in
/// pieces; both must give the same payloads and error.
pub fn compressed(data: &[u8]) {
    let Some((&selector, rest)) = data.split_first() else {
        return;
    };
    let algorithm = if selector & 1 == 0 {
        Compression::Zlib
    } else {
        Compression::Zstd { level: 3 }
    };
    let whole = read_compressed(
        PacketStream::new(MockStream(Cursor::new(rest.to_vec()))),
        algorithm,
    );
    let pieces = read_compressed(
        PacketStream::new(Pieces(Cursor::new(rest.to_vec()))),
        algorithm,
    );
    assert_eq!(whole, pieces, "framing depends on how the bytes arrive");
}

fn read_compressed<S: Read + Write>(
    mut stream: PacketStream<S>,
    algorithm: Compression,
) -> Option<(Vec<Vec<u8>>, String)> {
    stream.compress(algorithm).ok()?;
    let mut payloads = Vec::new();
    let mut out = Vec::new();
    loop {
        match stream.read_into(&mut out) {
            Ok(_) => {
                // Answers go through the compressed writer too.
                let _ = stream.write_command(0x03, &out);
                payloads.push(std::mem::take(&mut out));
            }
            Err(err) => return Some((payloads, err.to_string())),
        }
    }
}

/// Column definitions, rows and terminators, text and binary. The first
/// byte picks the column count (and, for binary rows, as many next bytes
/// their types); the rest is split into payloads on 0xFF separators.
pub fn result_set(data: &[u8]) {
    let Some((&columns, rest)) = data.split_first() else {
        return;
    };
    let columns = usize::from(columns % 16);
    if let Some(types) = rest.get(..columns) {
        let types: Vec<ColumnType> = types.iter().map(|&t| ColumnType(t)).collect();
        let mut binary = RowSet::new_binary_for_fuzzing(&types);
        for payload in rest
            .get(columns..)
            .unwrap_or_default()
            .split(|&b| b == 0xFF)
        {
            let start = binary.data_mut().len();
            binary.data_mut().extend_from_slice(payload);
            let end = binary.data_mut().len();
            let _ = accept_row_packet(&mut binary, start..end, true);
        }
        for row in binary.rows() {
            for (cell, &column_type) in row.cells().zip(&types) {
                if let Some(bytes) = cell {
                    let _ = crate::binary::to_text(column_type, false, 31, bytes);
                    let _ = crate::binary::to_text(column_type, true, 3, bytes);
                }
            }
        }
    }
    let mut rows = RowSet::new_for_fuzzing(columns);
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

/// SQL helpers used on user-written queries: the `INSERT … VALUES` and
/// `LOAD DATA LOCAL INFILE` recognisers and string escaping in both modes.
pub fn sql(data: &[u8]) {
    let _ = crate::sql::insert_values(data);
    let _ = crate::sql::local_infile_names(data, false);
    let _ = crate::sql::local_infile_names(data, true);
    let _ = crate::sql::mentions_local_infile(data);
    let mut out = Vec::new();
    crate::sql::escape_into(&mut out, data, false);
    crate::sql::escape_into(&mut out, data, true);
}

/// Text-protocol value parsers, and the binary protocol's value decoders
/// (the first byte picks a number of decimals).
pub fn text_values(data: &[u8]) {
    if let Some((&decimals, value)) = data.split_first() {
        let _ = crate::binary::signed(value);
        let _ = crate::binary::unsigned(value);
        let _ = crate::binary::float(value, decimals);
        let _ = crate::binary::date(value);
        let _ = crate::binary::datetime(value);
        let _ = crate::binary::time(value);
    }
    let _ = text::parse_i64(data);
    let _ = text::parse_u64(data);
    let _ = text::parse_f64(data);
    let _ = text::parse_date(data);
    let _ = text::parse_datetime(data);
    let _ = text::parse_time(data);
}

/// The whole client against a server that answers with `data`: handshake,
/// authentication (every plugin, RSA, auth switches), queries, streamed
/// result sets (batches, skips, rows left for the next command to discard),
/// multiple results, pipelined batches, `LOAD DATA LOCAL INFILE` requests
/// (answered, refused, inside a pipeline) and the other commands.
pub fn session(data: &[u8]) {
    // Recorded sessions are uncompressed; asking for compression (zstd,
    // zlib when the greeting offers no zstd) reads the same bytes as
    // compressed packets once authentication is over.
    session_with(data, None);
    session_with(data, Some(Compression::Zstd { level: 3 }));
}

/// A small file in the temporary directory the sessions allow, which their
/// `LOAD DATA LOCAL INFILE` names; `None` if it cannot be written.
fn local_file() -> Option<&'static PathBuf> {
    static FILE: OnceLock<Option<PathBuf>> = OnceLock::new();
    FILE.get_or_init(|| {
        let dir = std::fs::canonicalize(std::env::temp_dir()).ok()?;
        let path = dir.join("rustmy-fuzz-local-infile.csv");
        std::fs::write(&path, b"1,a\n2,b\n").ok()?;
        Some(path)
    })
    .as_ref()
}

fn session_with(data: &[u8], compress: Option<Compression>) {
    let file = local_file();
    let opts = Opts {
        user: b"fuzz".to_vec(),
        password: "secret".to_owned(),
        database: Some(b"db".to_vec()),
        client_flag: capability::MULTI_STATEMENTS,
        tls: TlsOpts {
            mode: SslMode::Disabled,
            ..TlsOpts::default()
        },
        compress,
        local_infile_dir: file.and_then(|path| path.parent()).map(Into::into),
        ..Opts::default()
    };
    let transport = Stream::Mock(MockStream(Cursor::new(data.to_vec())));
    let Ok(mut conn) = Conn::establish(transport, &opts) else {
        return;
    };
    if conn.query(b"SELECT 1").is_ok() {
        while let Ok(Some(_)) = conn.next_result() {}
    }
    if let Ok((mut head, Some(id))) = conn.query_streamed(b"SELECT 2") {
        let _ = conn.read_rows(id, &mut head.rows, 1);
        let _ = conn.skip_rows(id, 1);
        head.rows.clear();
        let _ = conn.read_rows(id, &mut head.rows, 64);
    }
    // Discards whatever the stream left, then streams the next result.
    if let Ok(Some((mut head, Some(id)))) = conn.next_result_streamed() {
        let _ = conn.read_rows(id, &mut head.rows, usize::MAX);
    }
    // Prepared statements: prepare, execute (cached, pipelined), binary rows.
    if let Ok(statement) = conn.prepare(b"SELECT ?") {
        let mut params = crate::Execute::default();
        params.int(1);
        params.text(b"x");
        if let Ok((mut head, Some(id))) = conn.execute_streamed(&params.finish(statement.id)) {
            let _ = conn.read_rows(id, &mut head.rows, 64);
        }
        let _ = conn.close_statement(statement.id);
    }
    let mut one = crate::Execute::default();
    one.null();
    let _ = conn.execute_cached(b"SELECT ?", &one);
    let _ = conn.execute_many_cached(b"DO ?", &[one]);
    // Pipelined batches: every reply read, then one left streaming.
    let _ = conn.pipeline([&b"DO 1"[..], b"DO 2", b"DO 3"]);
    if let Ok((_, mut head, Some(id))) =
        conn.pipeline_streamed([&b"DO 4"[..], b"DO 5"], b"SELECT 6")
    {
        let _ = conn.read_rows(id, &mut head.rows, 1);
    }
    let _ = conn.ping();
    let _ = conn.select_db(b"db");
    let _ = conn.reset_connection();
    let _ = conn.change_user(b"other", "pw", Some(b"db"));
    let _ = conn.query(b"SELECT 2");
    // Last, so the recorded seeds still reach everything above.
    if let Some(path) = file.and_then(|path| path.to_str()) {
        let load = format!("LOAD DATA LOCAL INFILE '{path}' INTO TABLE t");
        let _ = conn.query(load.as_bytes());
        let _ = conn.pipeline([load.as_bytes(), b"DO 7"]);
    }
}
