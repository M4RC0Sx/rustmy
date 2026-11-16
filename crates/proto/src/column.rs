//! Column definitions (`Protocol::ColumnDefinition41`).

use crate::buf::Reader;
use crate::constants::BINARY_COLLATION;
use crate::constants::ColumnType;
use crate::constants::column_flag;
use crate::error::Result;

/// Metadata for one column of a result set. Names are raw bytes in the
/// session's character set (`character_set_results`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Column {
    pub schema: Vec<u8>,
    pub table: Vec<u8>,
    pub org_table: Vec<u8>,
    pub name: Vec<u8>,
    pub org_name: Vec<u8>,
    /// Collation id of the column's values (63 means binary).
    pub collation: u16,
    /// Maximum display length.
    pub length: u32,
    pub column_type: ColumnType,
    pub flags: u16,
    pub decimals: u8,
}

impl Column {
    pub(crate) fn parse(payload: &[u8]) -> Result<Self> {
        let mut r = Reader::new(payload);
        let _catalog = r.lenenc_bytes()?;
        let schema = r.lenenc_bytes()?.to_vec();
        let table = r.lenenc_bytes()?.to_vec();
        let org_table = r.lenenc_bytes()?.to_vec();
        let name = r.lenenc_bytes()?.to_vec();
        let org_name = r.lenenc_bytes()?.to_vec();
        let _fixed_fields_len = r.lenenc_int()?;
        Ok(Self {
            schema,
            table,
            org_table,
            name,
            org_name,
            collation: r.u16()?,
            length: r.u32()?,
            column_type: ColumnType(r.u8()?),
            flags: r.u16()?,
            decimals: r.u8()?,
        })
    }

    #[must_use]
    pub const fn is_unsigned(&self) -> bool {
        self.flags & column_flag::UNSIGNED != 0
    }

    /// True when values are raw bytes rather than text in the connection
    /// character set.
    #[must_use]
    pub const fn is_binary(&self) -> bool {
        self.collation == BINARY_COLLATION
    }
}

#[cfg(test)]
mod tests {
    use super::Column;
    use crate::buf::put_lenenc_bytes;
    use crate::constants::ColumnType;

    #[test]
    fn parses_column_definition() {
        let mut p = Vec::new();
        for s in ["def", "shop", "o", "orders", "total", "total"] {
            put_lenenc_bytes(&mut p, s.as_bytes());
        }
        p.push(0x0C);
        p.extend_from_slice(&63u16.to_le_bytes());
        p.extend_from_slice(&10u32.to_le_bytes());
        p.push(ColumnType::NEWDECIMAL.0);
        p.extend_from_slice(&(1u16 | 32).to_le_bytes());
        p.push(2);
        p.extend_from_slice(&[0, 0]);

        let col = Column::parse(&p).unwrap();
        assert_eq!(col.name, b"total");
        assert_eq!(col.table, b"o");
        assert_eq!(col.org_table, b"orders");
        assert_eq!(col.column_type, ColumnType::NEWDECIMAL);
        assert_eq!(col.decimals, 2);
        assert!(col.is_unsigned());
        assert!(col.is_binary());

        for len in 0..p.len() - 2 {
            assert!(Column::parse(&p[..len]).is_err(), "len {len}");
        }
    }
}
