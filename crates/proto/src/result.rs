//! Command responses: OK, ERR and result sets (text and binary protocol).

use std::ops::Range;

use crate::buf::LENENC_NULL;
use crate::buf::Reader;
use crate::column::Column;
use crate::constants::ColumnType;
use crate::error::Error;
use crate::error::Result;

/// Fields of an OK packet (or of the OK/EOF that ends a result set).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct OkPacket {
    pub(crate) affected_rows: u64,
    pub(crate) last_insert_id: u64,
    pub(crate) status: u16,
    pub(crate) warnings: u16,
}

/// Parses an OK packet; its header byte is `0x00`, or `0xFE` when it ends a
/// result set under `CLIENT_DEPRECATE_EOF`.
pub(crate) fn parse_ok(payload: &[u8]) -> Result<OkPacket> {
    let mut r = Reader::new(payload);
    r.skip(1)?;
    Ok(OkPacket {
        affected_rows: r.lenenc_int()?,
        last_insert_id: r.lenenc_int()?,
        status: r.u16()?,
        warnings: r.u16()?,
    })
}

/// Parses a legacy EOF packet (`0xFE`, warnings, status).
pub(crate) fn parse_eof(payload: &[u8]) -> Result<OkPacket> {
    let mut r = Reader::new(payload);
    r.skip(1)?;
    let warnings = r.u16()?;
    let status = r.u16()?;
    Ok(OkPacket {
        status,
        warnings,
        ..OkPacket::default()
    })
}

/// Converts an ERR packet into [`Error::Server`]. Never fails: a malformed
/// ERR packet still becomes an error, with whatever could be decoded.
pub(crate) fn parse_err(payload: &[u8]) -> Error {
    let mut r = Reader::new(payload);
    let _header = r.u8();
    let code = r.u16().unwrap_or(0);
    let mut sql_state = String::new();
    if r.peek_u8() == Some(b'#') {
        let _marker = r.u8();
        if let Ok(state) = r.take(5) {
            sql_state = String::from_utf8_lossy(state).into_owned();
        }
    }
    Error::Server {
        code,
        sql_state,
        message: r.rest().to_vec(),
    }
}

/// Location of one cell inside [`RowSet`]'s data buffer.
#[derive(Debug, Clone, Copy)]
struct Cell {
    start: usize,
    end: usize,
}

impl Cell {
    const NULL: Self = Self {
        start: usize::MAX,
        end: usize::MAX,
    };
}

/// How a binary-protocol row stores one column's value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Encoding {
    /// Little-endian, this many bytes (integers, `FLOAT`, `DOUBLE`).
    Fixed(usize),
    /// A length byte, then that many bytes of date/time fields.
    Temporal,
    /// A length-encoded string, as in the text protocol.
    LengthEncoded,
}

fn binary_encoding(column_type: ColumnType) -> Encoding {
    match column_type {
        ColumnType::NULL => Encoding::Fixed(0),
        ColumnType::TINY => Encoding::Fixed(1),
        ColumnType::SHORT | ColumnType::YEAR => Encoding::Fixed(2),
        ColumnType::LONG | ColumnType::INT24 | ColumnType::FLOAT => Encoding::Fixed(4),
        ColumnType::LONGLONG | ColumnType::DOUBLE => Encoding::Fixed(8),
        ColumnType::DATE
        | ColumnType::NEWDATE
        | ColumnType::DATETIME
        | ColumnType::TIMESTAMP
        | ColumnType::TIME => Encoding::Temporal,
        _ => Encoding::LengthEncoded,
    }
}

/// All rows of a result set, stored as the raw row payloads in one
/// contiguous buffer plus an index of where each cell starts and ends.
///
/// Rows are read from the socket in one go (with the GIL released, in the
/// Python binding) and converted to Python objects afterwards. A cell of a
/// text-protocol row is the value as text; a cell of a binary-protocol row
/// (prepared statements, [`RowSet::is_binary`]) is the value's bytes:
/// little-endian integers and floats, date/time fields (empty for a zero
/// value), or the same bytes as in text for every other type.
#[derive(Debug, Default)]
pub struct RowSet {
    data: Vec<u8>,
    cells: Vec<Cell>,
    columns: usize,
    /// Each column's encoding in binary-protocol rows; empty for text rows.
    binary: Vec<Encoding>,
}

impl RowSet {
    fn new(columns: usize) -> Self {
        Self {
            data: Vec::new(),
            cells: Vec::new(),
            columns,
            binary: Vec::new(),
        }
    }

    fn new_binary(columns: &[Column]) -> Self {
        Self {
            binary: columns
                .iter()
                .map(|c| binary_encoding(c.column_type))
                .collect(),
            ..Self::new(columns.len())
        }
    }

    /// Whether the rows come from the binary protocol (prepared statements).
    #[must_use]
    pub fn is_binary(&self) -> bool {
        !self.binary.is_empty()
    }

    /// Number of rows.
    #[must_use]
    pub fn len(&self) -> usize {
        self.cells.len().checked_div(self.columns).unwrap_or(0)
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.cells.is_empty()
    }

    /// Drops every row, keeping the column count and the allocations, so
    /// the next batch of a streamed result set reuses them.
    pub fn clear(&mut self) {
        self.data.clear();
        self.cells.clear();
    }

    /// Bytes of row data held.
    pub(crate) const fn data_len(&self) -> usize {
        self.data.len()
    }

    /// Number of cells per row.
    #[must_use]
    pub const fn columns(&self) -> usize {
        self.columns
    }

    /// Iterates rows; each row yields its cells, `None` for SQL `NULL`.
    pub fn rows(&self) -> impl ExactSizeIterator<Item = RowRef<'_>> {
        self.cells.chunks(self.columns.max(1)).map(|cells| RowRef {
            data: &self.data,
            cells,
        })
    }

    #[cfg(feature = "fuzzing")]
    pub(crate) fn new_for_fuzzing(columns: usize) -> Self {
        Self::new(columns)
    }

    /// A binary-protocol row set whose columns have `types`.
    #[cfg(feature = "fuzzing")]
    pub(crate) fn new_binary_for_fuzzing(types: &[ColumnType]) -> Self {
        Self {
            binary: types.iter().map(|&t| binary_encoding(t)).collect(),
            ..Self::new(types.len())
        }
    }

    /// Buffer the connection appends raw row packets to.
    pub(crate) fn data_mut(&mut self) -> &mut Vec<u8> {
        &mut self.data
    }

    /// Indexes one row stored at `range` of `data`.
    fn push_row(&mut self, range: Range<usize>) -> Result<()> {
        if self.is_binary() {
            return self.push_binary_row(range);
        }
        let payload = self.data.get(range.clone()).unwrap_or_default();
        let mut r = Reader::new(payload);
        for _ in 0..self.columns {
            if r.peek_u8() == Some(LENENC_NULL) {
                r.skip(1)?;
                self.cells.push(Cell::NULL);
            } else {
                let len = r.lenenc_len()?;
                let start = range.start + r.pos();
                r.skip(len)?;
                self.cells.push(Cell {
                    start,
                    end: start + len,
                });
            }
        }
        if !r.is_empty() {
            return Err(Error::protocol("row has more cells than columns"));
        }
        Ok(())
    }

    /// Indexes one binary-protocol row: a `0x00` header, a NULL bitmap
    /// (offset by two bits) and the values of the columns that are not
    /// NULL.
    fn push_binary_row(&mut self, range: Range<usize>) -> Result<()> {
        let payload = self.data.get(range.clone()).unwrap_or_default();
        let mut r = Reader::new(payload);
        if r.u8()? != 0x00 {
            return Err(Error::protocol("binary row without its 0x00 header"));
        }
        let bitmap = r.take((self.columns + 7 + 2) / 8)?;
        for (index, &encoding) in self.binary.iter().enumerate() {
            let bit = index + 2;
            let null = bitmap
                .get(bit / 8)
                .is_some_and(|byte| byte & (1 << (bit % 8)) != 0);
            if null {
                self.cells.push(Cell::NULL);
                continue;
            }
            let len = match encoding {
                Encoding::Fixed(len) => len,
                Encoding::Temporal => usize::from(r.u8()?),
                Encoding::LengthEncoded => r.lenenc_len()?,
            };
            let start = range.start + r.pos();
            r.skip(len)?;
            self.cells.push(Cell {
                start,
                end: start + len,
            });
        }
        if !r.is_empty() {
            return Err(Error::protocol("row has more cells than columns"));
        }
        Ok(())
    }
}

/// One row of a [`RowSet`].
#[derive(Debug, Clone, Copy)]
pub struct RowRef<'a> {
    data: &'a [u8],
    cells: &'a [Cell],
}

impl<'a> RowRef<'a> {
    /// Cells as raw text-protocol bytes, `None` for SQL `NULL`.
    pub fn cells(&self) -> impl ExactSizeIterator<Item = Option<&'a [u8]>> + use<'a> {
        let data = self.data;
        self.cells.iter().map(move |c| {
            if c.start == usize::MAX {
                None
            } else {
                // In range by construction in `push_row`.
                Some(data.get(c.start..c.end).unwrap_or_default())
            }
        })
    }
}

/// Outcome of one statement: a result set (possibly empty) or an OK.
#[derive(Debug, Default)]
pub struct QueryResult {
    /// Empty for statements that return no result set (INSERT, UPDATE...).
    pub columns: Vec<Column>,
    pub rows: RowSet,
    pub affected_rows: u64,
    pub last_insert_id: u64,
    pub warnings: u16,
}

impl QueryResult {
    pub(crate) fn from_ok(ok: OkPacket) -> Self {
        Self {
            affected_rows: ok.affected_rows,
            last_insert_id: ok.last_insert_id,
            warnings: ok.warnings,
            ..Self::default()
        }
    }

    pub(crate) fn rowset(columns: Vec<Column>, binary: bool) -> Self {
        let rows = if binary {
            RowSet::new_binary(&columns)
        } else {
            RowSet::new(columns.len())
        };
        Self {
            columns,
            rows,
            ..Self::default()
        }
    }
}

/// What a packet read while streaming rows turned out to be.
#[derive(Debug)]
pub(crate) enum RowPacket {
    Row,
    End(OkPacket),
}

/// Tells a row packet from the end of the result set: `None` for a row, the
/// terminator's fields at the end, or the server error that aborted it.
pub(crate) fn row_terminator(payload: &[u8], deprecate_eof: bool) -> Result<Option<OkPacket>> {
    // A row can only start with 0xFE when its first cell is at least 16 MiB
    // long, so a short 0xFE packet is always the terminator.
    match payload.first() {
        Some(0xFE) if payload.len() < crate::MAX_PACKET_PAYLOAD => if deprecate_eof {
            parse_ok(payload)
        } else {
            parse_eof(payload)
        }
        .map(Some),
        Some(0xFF) => Err(parse_err(payload)),
        _ => Ok(None),
    }
}

/// Classifies the packet just appended to `rows` at `range`: indexes it if it
/// is a row, removes it from the buffer if it terminates the result set.
pub(crate) fn accept_row_packet(
    rows: &mut RowSet,
    range: Range<usize>,
    deprecate_eof: bool,
) -> Result<RowPacket> {
    let payload = rows.data.get(range.clone()).unwrap_or_default();
    match row_terminator(payload, deprecate_eof) {
        Ok(None) => rows.push_row(range).map(|()| RowPacket::Row),
        Ok(Some(end)) => {
            rows.data.truncate(range.start);
            Ok(RowPacket::End(end))
        }
        Err(err) => {
            rows.data.truncate(range.start);
            Err(err)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::RowPacket;
    use super::RowSet;
    use super::accept_row_packet;
    use super::parse_err;
    use crate::Error;
    use crate::buf::put_lenenc_bytes;

    fn append(rows: &mut RowSet, payload: &[u8]) -> std::ops::Range<usize> {
        let start = rows.data.len();
        rows.data.extend_from_slice(payload);
        start..rows.data.len()
    }

    #[test]
    fn indexes_rows_with_nulls_and_drops_terminator() {
        let mut rows = RowSet::new(2);
        let mut row = Vec::new();
        put_lenenc_bytes(&mut row, b"42");
        row.push(0xFB);
        let r = append(&mut rows, &row);
        assert!(matches!(
            accept_row_packet(&mut rows, r, true).unwrap(),
            RowPacket::Row
        ));

        // OK terminator: affected=0, insert_id=0, status=0x0002, warnings=1.
        let r = append(&mut rows, &[0xFE, 0, 0, 2, 0, 1, 0]);
        let RowPacket::End(ok) = accept_row_packet(&mut rows, r, true).unwrap() else {
            panic!("expected terminator");
        };
        assert_eq!((ok.status, ok.warnings), (2, 1));

        assert_eq!(rows.len(), 1);
        let cells: Vec<_> = rows.rows().next().unwrap().cells().collect();
        assert_eq!(cells, [Some(&b"42"[..]), None]);
    }

    #[test]
    fn legacy_eof_terminator() {
        let mut rows = RowSet::new(1);
        let r = append(&mut rows, &[0xFE, 3, 0, 2, 0]);
        let RowPacket::End(eof) = accept_row_packet(&mut rows, r, false).unwrap() else {
            panic!("expected terminator");
        };
        assert_eq!((eof.warnings, eof.status), (3, 2));
        assert!(rows.is_empty());
    }

    #[test]
    fn rejects_rows_with_wrong_cell_count() {
        let mut rows = RowSet::new(1);
        let r = append(&mut rows, &[1, b'a', 1, b'b']);
        assert!(accept_row_packet(&mut rows, r, true).is_err());

        let mut rows = RowSet::new(2);
        let r = append(&mut rows, &[1, b'a']);
        assert!(accept_row_packet(&mut rows, r, true).is_err());
    }

    #[test]
    fn err_packet_mid_result_set() {
        let mut rows = RowSet::new(1);
        let mut p = vec![0xFF];
        p.extend_from_slice(&1317u16.to_le_bytes());
        p.extend_from_slice(b"#70100Query execution was interrupted");
        let r = append(&mut rows, &p);
        let err = accept_row_packet(&mut rows, r, true).unwrap_err();
        let Error::Server {
            code,
            sql_state,
            message,
        } = err
        else {
            panic!("expected server error");
        };
        assert_eq!(code, 1317);
        assert_eq!(sql_state, "70100");
        assert_eq!(message, b"Query execution was interrupted");
    }

    #[test]
    fn malformed_err_packet_still_becomes_an_error() {
        assert!(matches!(parse_err(&[0xFF]), Error::Server { code: 0, .. }));
    }
}
