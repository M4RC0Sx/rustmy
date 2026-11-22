//! Command replies without I/O.
//!
//! [`Session`] holds what the protocol keeps between commands once
//! authenticated (status flags, the result set whose rows are still on the
//! wire) and turns each reply payload into results: [`Head`] reads the start
//! of a statement's reply (an OK, or a result set's column definitions), and
//! row packets go through [`Session::row`] or [`Session::skip`]. Whoever
//! drives it reads and writes the payloads, sends local files and runs the
//! order of commands: `Conn` over a blocking stream.

use std::ops::Range;

use crate::buf::Reader;
use crate::column::Column;
use crate::constants::capability;
use crate::constants::status;
use crate::error::Error;
use crate::error::Result;
use crate::result::QueryResult;
use crate::result::RowPacket;
use crate::result::RowSet;
use crate::result::accept_row_packet;
use crate::result::parse_eof;
use crate::result::parse_err;
use crate::result::parse_ok;
use crate::result::row_terminator;

/// Identifies a result set whose rows are being streamed, so that reading
/// more rows fails once another command has discarded them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamId(u64);

/// Where a streamed result set stands after a read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadOutcome {
    /// More rows (or the terminator) are still on the wire.
    More,
    /// The result set ended; the terminator carried this many warnings.
    Done { warnings: u16 },
}

/// The result set whose rows are still unread on the wire.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Pending {
    id: StreamId,
    deprecate_eof: bool,
}

/// Protocol state of an authenticated connection between commands.
#[derive(Debug)]
pub(crate) struct Session {
    pub(crate) capabilities: u32,
    /// Status flags from the last OK/EOF packet.
    pub(crate) status: u16,
    /// Streamed result set with rows still on the wire.
    pending: Option<Pending>,
    /// Last [`StreamId`] handed out.
    last_stream_id: u64,
    /// Whether the reply being read comes from `COM_STMT_EXECUTE` (its
    /// result sets, queued ones included, have binary rows).
    pub(crate) binary_reply: bool,
}

/// What to do after a payload of a reply's head.
#[derive(Debug)]
pub(crate) enum HeadStep {
    /// Read the next payload.
    Read,
    /// The statement's result: an OK, or a result set whose rows stay on
    /// the wire for the [`StreamId`].
    Done(QueryResult, Option<StreamId>),
    /// The server asks for a local file (`LOAD DATA LOCAL INFILE`) with
    /// this name; [`Session::file_reply`] reads the reply after it.
    File(Vec<u8>),
}

/// Reading the start of a statement's reply.
#[derive(Debug)]
pub(crate) enum Head {
    /// Its first payload: OK, ERR, a local file request or a column count.
    First,
    /// Column definitions, `left` more to come.
    Columns { columns: Vec<Column>, left: usize },
    /// The EOF after the column definitions (without `DEPRECATE_EOF`).
    Eof { columns: Vec<Column> },
}

impl Session {
    pub(crate) const fn new(capabilities: u32, status: u16) -> Self {
        Self {
            capabilities,
            status,
            pending: None,
            last_stream_id: 0,
            binary_reply: false,
        }
    }

    const fn deprecate_eof(&self) -> bool {
        self.capabilities & capability::DEPRECATE_EOF != 0
    }

    /// Whether the server has more results queued for the last query
    /// (multi-statement queries and stored procedures).
    pub(crate) const fn has_more_results(&self) -> bool {
        self.status & status::MORE_RESULTS_EXISTS != 0
    }

    /// Starts reading a statement's reply.
    pub(crate) const fn head(&mut self) -> Head {
        // An error ends the whole response, including queued results.
        self.status &= !status::MORE_RESULTS_EXISTS;
        Head::First
    }

    /// Handles the next payload of the reply's head.
    pub(crate) fn head_step(&mut self, head: &mut Head, payload: &[u8]) -> Result<HeadStep> {
        match head {
            Head::First => match payload.first().copied() {
                Some(0x00) => {
                    let ok = parse_ok(payload)?;
                    self.status = ok.status;
                    Ok(HeadStep::Done(QueryResult::from_ok(ok), None))
                }
                Some(0xFF) => Err(parse_err(payload)),
                Some(0xFB) => Ok(HeadStep::File(
                    payload.get(1..).unwrap_or_default().to_vec(),
                )),
                Some(_) => {
                    let count = Reader::new(payload).lenenc_len()?;
                    // Cap the preallocation: `count` comes from the server.
                    let columns = Vec::with_capacity(count.min(4096));
                    *head = Head::Columns {
                        columns,
                        left: count,
                    };
                    Ok(self.columns_read(head))
                }
                None => Err(Error::protocol("empty response packet")),
            },
            Head::Columns { columns, left } => {
                columns.push(Column::parse(payload)?);
                *left = left.saturating_sub(1);
                Ok(self.columns_read(head))
            }
            Head::Eof { columns } => {
                if payload.first() != Some(&0xFE) {
                    return Err(Error::protocol("missing EOF after column definitions"));
                }
                self.status = parse_eof(payload)?.status;
                let columns = std::mem::take(columns);
                Ok(self.result_set(columns))
            }
        }
    }

    /// After a column definition (or none): what comes next.
    fn columns_read(&mut self, head: &mut Head) -> HeadStep {
        let Head::Columns { columns, left: 0 } = head else {
            return HeadStep::Read;
        };
        let columns = std::mem::take(columns);
        if self.deprecate_eof() {
            return self.result_set(columns);
        }
        *head = Head::Eof { columns };
        HeadStep::Read
    }

    /// A result set whose rows are now pending.
    fn result_set(&mut self, columns: Vec<Column>) -> HeadStep {
        self.last_stream_id += 1;
        let id = StreamId(self.last_stream_id);
        self.pending = Some(Pending {
            id,
            deprecate_eof: self.deprecate_eof(),
        });
        HeadStep::Done(QueryResult::rowset(columns, self.binary_reply), Some(id))
    }

    /// The reply after a local file was sent (or refused with an empty
    /// one): OK or ERR.
    pub(crate) fn file_reply(&mut self, payload: &[u8]) -> Result<QueryResult> {
        match payload.first() {
            Some(0x00) => {
                let ok = parse_ok(payload)?;
                self.status = ok.status;
                Ok(QueryResult::from_ok(ok))
            }
            Some(0xFF) => Err(parse_err(payload)),
            _ => Err(Error::protocol(
                "expected an OK or ERR packet after a local file",
            )),
        }
    }

    /// The reply to a command that answers OK or ERR.
    pub(crate) fn ok_reply(&mut self, payload: &[u8]) -> Result<()> {
        match payload.first() {
            Some(0x00) => {
                self.status = parse_ok(payload)?.status;
                Ok(())
            }
            Some(0xFF) => Err(parse_err(payload)),
            _ => Err(Error::protocol("expected an OK or ERR packet")),
        }
    }

    /// The result set with rows on the wire, if any.
    pub(crate) fn pending(&self) -> Option<StreamId> {
        self.pending.map(|pending| pending.id)
    }

    /// The pending result set, if `id` is it.
    pub(crate) fn pending_rows(&self, id: StreamId) -> Result<Pending> {
        match self.pending {
            Some(pending) if pending.id == id => Ok(pending),
            _ => Err(Error::ResultDiscarded),
        }
    }

    /// Handles a row packet of `pending` just read into `rows` at the range
    /// `packet` gives (or the error reading it): `None` for a row, the
    /// warnings once the result set ended. Any error ends it too.
    pub(crate) fn row(
        &mut self,
        pending: Pending,
        rows: &mut RowSet,
        packet: Result<Range<usize>>,
    ) -> Result<Option<u16>> {
        match packet.and_then(|range| accept_row_packet(rows, range, pending.deprecate_eof)) {
            Ok(RowPacket::Row) => Ok(None),
            Ok(RowPacket::End(end)) => {
                self.pending = None;
                self.status = end.status;
                Ok(Some(end.warnings))
            }
            Err(err) => {
                self.pending = None;
                Err(err)
            }
        }
    }

    /// Like [`Session::row`] for a row being skipped, read on its own (or
    /// the error reading it).
    pub(crate) fn skip(&mut self, pending: Pending, packet: Result<&[u8]>) -> Result<Option<u16>> {
        match packet.and_then(|payload| row_terminator(payload, pending.deprecate_eof)) {
            Ok(None) => Ok(None),
            Ok(Some(end)) => {
                self.pending = None;
                self.status = end.status;
                Ok(Some(end.warnings))
            }
            Err(err) => {
                self.pending = None;
                Err(err)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::HeadStep;
    use super::Session;
    use crate::buf::put_lenenc_bytes;
    use crate::constants::capability;
    use crate::constants::status;
    use crate::error::Error;

    /// A column definition named `name`.
    fn column(name: &[u8]) -> Vec<u8> {
        let mut payload = Vec::new();
        for part in [&b"def"[..], b"db", b"t", b"t", name, name] {
            put_lenenc_bytes(&mut payload, part);
        }
        payload.extend_from_slice(&[0x0C, 0x21, 0x00, 0x0B, 0, 0, 0, 0xFD, 0, 0, 0, 0, 0]);
        payload
    }

    /// Feeds `payloads` to a reply's head; the last must end it.
    fn head(session: &mut Session, payloads: &[&[u8]]) -> crate::Result<HeadStep> {
        let mut head = session.head();
        let (last, rest) = payloads.split_last().unwrap();
        for payload in rest {
            assert!(matches!(
                session.head_step(&mut head, payload)?,
                HeadStep::Read
            ));
        }
        session.head_step(&mut head, last)
    }

    #[test]
    fn a_result_set_head_leaves_its_rows_pending() {
        for deprecate_eof in [true, false] {
            let capabilities = if deprecate_eof {
                capability::DEPRECATE_EOF
            } else {
                0
            };
            let mut session = Session::new(capabilities, 0);
            let (a, b) = (column(b"a"), column(b"b"));
            let eof: &[u8] = &[0xFE, 0, 0, 0x22, 0];
            let mut payloads: Vec<&[u8]> = vec![&[2], &a, &b];
            if !deprecate_eof {
                payloads.push(eof);
            }
            let HeadStep::Done(result, Some(id)) = head(&mut session, &payloads).unwrap() else {
                panic!("a result set");
            };
            assert_eq!(result.columns.len(), 2);
            assert_eq!(session.pending(), Some(id));
            if !deprecate_eof {
                assert_eq!(session.status, 0x22, "from the EOF");
            }
        }
    }

    #[test]
    fn a_reply_without_rows_ends_at_its_ok() {
        let mut session = Session::new(capability::DEPRECATE_EOF, status::MORE_RESULTS_EXISTS);
        // affected 3, insert id 7, status AUTOCOMMIT, 1 warning.
        let HeadStep::Done(result, None) = head(&mut session, &[&[0, 3, 7, 2, 0, 1, 0]]).unwrap()
        else {
            panic!("an OK");
        };
        assert_eq!((result.affected_rows, result.last_insert_id), (3, 7));
        assert_eq!(result.warnings, 1);
        assert_eq!(session.status, 2);
        assert_eq!(session.pending(), None);
    }

    #[test]
    fn errors_and_file_requests_end_the_head() {
        let mut session = Session::new(capability::DEPRECATE_EOF, status::MORE_RESULTS_EXISTS);
        let err = head(&mut session, &[b"\xFF\x7A\x04#42S02no such table"]).unwrap_err();
        assert!(matches!(err, Error::Server { code: 1146, .. }), "{err}");
        assert!(!session.has_more_results(), "an error ends queued results");
        let HeadStep::File(name) = head(&mut session, &[b"\xFBdata.csv"]).unwrap() else {
            panic!("a file request");
        };
        assert_eq!(name, b"data.csv");
        assert!(head(&mut session, &[b""]).is_err());
        let mut legacy = Session::new(0, 0);
        let a = column(b"a");
        let err = head(&mut legacy, &[&[1], &a, &[0x00]]).unwrap_err();
        assert!(err.to_string().contains("missing EOF"), "{err}");
    }
}
