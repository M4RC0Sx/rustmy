//! SQL text helpers for client-side parameter interpolation: string escaping,
//! recognising `INSERT … VALUES (…)` statements that `executemany` can
//! batch into multi-row inserts, and the file names of `LOAD DATA LOCAL
//! INFILE` statements (the only files the client sends).
//!
//! Everything works on UTF-8 / ASCII-compatible bytes and never panics.

use std::ops::Range;

/// Appends `value` escaped for use inside a single-quoted SQL literal.
///
/// With `no_backslash_escapes` (the server's `NO_BACKSLASH_ESCAPES` SQL mode)
/// only quotes are doubled; otherwise the characters MySQL treats specially
/// after a backslash are escaped. `value` must be in the connection's
/// character set and, for multi-byte legacy charsets, must be escaped before
/// encoding (see the callers).
pub fn escape_into(out: &mut Vec<u8>, value: &[u8], no_backslash_escapes: bool) {
    out.reserve(value.len() + value.len() / 8 + 2);
    let mut run_start = 0;
    for (i, &byte) in value.iter().enumerate() {
        if let Some(replacement) = escape_sequence(byte, no_backslash_escapes) {
            out.extend_from_slice(value.get(run_start..i).unwrap_or_default());
            out.extend_from_slice(replacement);
            run_start = i + 1;
        }
    }
    out.extend_from_slice(value.get(run_start..).unwrap_or_default());
}

/// What replaces `byte` inside a quoted literal, if it needs escaping.
const fn escape_sequence(byte: u8, no_backslash_escapes: bool) -> Option<&'static [u8]> {
    if no_backslash_escapes {
        return if byte == b'\'' { Some(b"''") } else { None };
    }
    match byte {
        0 => Some(b"\\0"),
        b'\n' => Some(b"\\n"),
        b'\r' => Some(b"\\r"),
        0x1a => Some(b"\\Z"),
        b'\\' => Some(b"\\\\"),
        b'\'' => Some(b"\\'"),
        b'"' => Some(b"\\\""),
        _ => None,
    }
}

/// Parts of an `INSERT`/`REPLACE … VALUES (…)` statement, as byte ranges of
/// the query: everything before the row (`INSERT INTO t (a, b) VALUES `),
/// the parenthesised row template (`(%s, %s)`) and what follows it (empty,
/// `ON DUPLICATE KEY UPDATE …` or a MySQL 8 row alias `AS new …`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InsertValues {
    pub prefix: Range<usize>,
    pub row: Range<usize>,
    pub suffix: Range<usize>,
}

/// Recognises a single-row `INSERT`/`REPLACE … VALUES (…)` whose row can be
/// repeated to insert many rows in one statement. Returns `None` for
/// anything else (`INSERT … SELECT`, `INSERT … SET`, several rows already,
/// `VALUES ROW(…)`, other statements), which callers execute row by row.
#[must_use]
pub fn insert_values(query: &[u8]) -> Option<InsertValues> {
    let start = skip_space(query, 0);
    let first = word_at(query, start)?;
    if !first.eq_ignore_ascii_case(b"INSERT") && !first.eq_ignore_ascii_case(b"REPLACE") {
        return None;
    }

    // Find `VALUES`/`VALUE` at nesting depth 0, outside quotes and comments.
    let mut scanner = Scanner::new(query, start + first.len());
    let values_end = loop {
        let token = scanner.next_token()?;
        if let Token::Word(range) = token {
            let word = query.get(range.clone())?;
            if scanner.depth == 0
                && (word.eq_ignore_ascii_case(b"VALUES") || word.eq_ignore_ascii_case(b"VALUE"))
            {
                break range.end;
            }
        }
    };

    let row_start = skip_space(query, values_end);
    if query.get(row_start) != Some(&b'(') {
        return None; // e.g. `VALUES ROW(...)`
    }
    let mut scanner = Scanner::new(query, row_start);
    let row_end = loop {
        match scanner.next_token()? {
            Token::Close if scanner.depth == 0 => break scanner.pos,
            _ => {}
        }
    };

    // What follows the row must not repeat per row: nothing (optionally a
    // final `;`), an `ON DUPLICATE KEY UPDATE` clause or a row alias.
    let after = skip_space(query, row_end);
    let rest = query.get(after..)?;
    let rest_trimmed = trim_end_statement(rest);
    let suffix_ok = rest_trimmed.is_empty()
        || starts_with_words(rest_trimmed, &[b"ON", b"DUPLICATE", b"KEY", b"UPDATE"])
        || starts_with_words(rest_trimmed, &[b"AS"]);
    if !suffix_ok {
        return None;
    }
    Some(InsertValues {
        prefix: 0..row_start,
        row: row_start..row_end,
        suffix: row_end..row_end + (after - row_end) + rest_trimmed.len(),
    })
}

/// File names that the `LOAD DATA LOCAL INFILE` (and `LOAD XML LOCAL
/// INFILE`) statements of `query` make the server ask for: the string
/// literal after `INFILE`, unescaped as the server reads it (backslash
/// escapes unless `no_backslash_escapes`, doubled quotes, adjacent literals
/// joined).
///
/// The scan is byte-wise, so a name with a multi-byte character whose
/// second byte is a backslash (sjis, gbk, big5) decodes differently from
/// the server's: such a request is then refused, never granted.
#[must_use]
pub fn local_infile_names(query: &[u8], no_backslash_escapes: bool) -> Vec<Vec<u8>> {
    let mut names = Vec::new();
    scan_local_infile(query, |scanner| {
        if let Some(name) = scanner.string_literal(no_backslash_escapes) {
            names.push(name);
        }
    });
    names
}

/// Whether `query` has a `LOAD DATA LOCAL INFILE` (or `LOAD XML LOCAL
/// INFILE`) statement, with its file name written or still a placeholder.
#[must_use]
pub fn mentions_local_infile(query: &[u8]) -> bool {
    let mut found = false;
    scan_local_infile(query, |_| found = true);
    found
}

/// Calls `found` with the scanner right after each `LOAD DATA|XML
/// [LOW_PRIORITY|CONCURRENT] LOCAL INFILE` of `query` (and its spaces).
fn scan_local_infile(query: &[u8], mut found: impl FnMut(&mut Scanner<'_>)) {
    let mut scanner = Scanner::new(query, 0);
    // How many words of the statement's head have matched so far.
    let mut matched = 0;
    while let Some(token) = scanner.next_token() {
        let word = match token {
            Token::Word(range) => query.get(range).unwrap_or_default(),
            Token::Space => continue,
            Token::Close | Token::Other => {
                matched = 0;
                continue;
            }
        };
        let is = |expected: &[u8]| word.eq_ignore_ascii_case(expected);
        matched = match matched {
            1 if is(b"DATA") || is(b"XML") => 2,
            2 if is(b"LOW_PRIORITY") || is(b"CONCURRENT") => 2,
            2 if is(b"LOCAL") => 3,
            3 if is(b"INFILE") => {
                scanner.skip_space();
                found(&mut scanner);
                0
            }
            _ if is(b"LOAD") => 1,
            _ => 0,
        };
    }
}

/// Drops trailing whitespace and one optional `;`.
fn trim_end_statement(bytes: &[u8]) -> &[u8] {
    let trimmed = bytes.trim_ascii_end();
    trimmed
        .strip_suffix(b";")
        .map_or(trimmed, <[u8]>::trim_ascii_end)
}

fn skip_space(query: &[u8], mut pos: usize) -> usize {
    while query.get(pos).is_some_and(u8::is_ascii_whitespace) {
        pos += 1;
    }
    pos
}

const fn is_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'$' || byte >= 0x80
}

fn word_at(query: &[u8], start: usize) -> Option<&[u8]> {
    let rest = query.get(start..)?;
    let len = rest.iter().take_while(|&&b| is_word_byte(b)).count();
    (len > 0).then(|| rest.get(..len)).flatten()
}

fn starts_with_words(text: &[u8], words: &[&[u8]]) -> bool {
    let mut pos = 0;
    for expected in words {
        pos = skip_space(text, pos);
        match word_at(text, pos) {
            Some(word) if word.eq_ignore_ascii_case(expected) => pos += word.len(),
            _ => return false,
        }
    }
    true
}

#[derive(Debug)]
enum Token {
    Word(Range<usize>),
    Close,
    /// Whitespace or a comment.
    Space,
    Other,
}

/// Minimal lexer: skips quoted strings, quoted identifiers and comments,
/// tracks parenthesis depth and yields bare words.
struct Scanner<'a> {
    query: &'a [u8],
    pos: usize,
    depth: usize,
}

impl<'a> Scanner<'a> {
    const fn new(query: &'a [u8], pos: usize) -> Self {
        Self {
            query,
            pos,
            depth: 0,
        }
    }

    fn peek(&self, offset: usize) -> Option<u8> {
        self.query.get(self.pos + offset).copied()
    }

    /// Next token, or `None` at the end of the query or of an unterminated
    /// quote or comment.
    fn next_token(&mut self) -> Option<Token> {
        let byte = self.peek(0)?;
        match byte {
            b'\'' | b'"' | b'`' => {
                self.skip_quoted(byte)?;
                Some(Token::Other)
            }
            b'#' => {
                self.skip_line();
                Some(Token::Space)
            }
            b'-' if self.peek(1) == Some(b'-')
                && self.peek(2).is_none_or(|b| b.is_ascii_whitespace()) =>
            {
                self.skip_line();
                Some(Token::Space)
            }
            // `/*! … */` runs on the server but counts as a comment here, so
            // a `LOCAL` hidden in one is never seen: the request is refused.
            b'/' if self.peek(1) == Some(b'*') => {
                let body = self.query.get(self.pos + 2..)?;
                let end = body.windows(2).position(|w| w == b"*/")?;
                self.pos += 2 + end + 2;
                Some(Token::Space)
            }
            b if b.is_ascii_whitespace() => {
                self.skip_space();
                Some(Token::Space)
            }
            b'(' => {
                self.depth += 1;
                self.pos += 1;
                Some(Token::Other)
            }
            b')' => {
                self.depth = self.depth.checked_sub(1)?;
                self.pos += 1;
                Some(Token::Close)
            }
            b if is_word_byte(b) => {
                let start = self.pos;
                while self.peek(0).is_some_and(is_word_byte) {
                    self.pos += 1;
                }
                Some(Token::Word(start..self.pos))
            }
            _ => {
                self.pos += 1;
                Some(Token::Other)
            }
        }
    }

    /// Skips a quoted string or identifier, honouring doubled quotes and,
    /// for strings, backslash escapes.
    fn skip_quoted(&mut self, quote: u8) -> Option<()> {
        self.pos += 1;
        loop {
            let byte = self.peek(0)?;
            self.pos += 1;
            if byte == b'\\' && quote != b'`' {
                self.pos += 1;
            } else if byte == quote {
                if self.peek(0) == Some(quote) {
                    self.pos += 1;
                } else {
                    return Some(());
                }
            }
        }
    }

    fn skip_space(&mut self) {
        self.pos = skip_space(self.query, self.pos);
    }

    /// Reads the string literal at the current position as the server
    /// reads it, joining adjacent literals (`'a' "b"` is `ab`). `None` if
    /// there is none or it is unterminated.
    fn string_literal(&mut self, no_backslash_escapes: bool) -> Option<Vec<u8>> {
        let mut value = Vec::new();
        let mut pieces = 0;
        while let Some(quote) = self.peek(0).filter(|&b| b == b'\'' || b == b'"') {
            self.pos += 1;
            loop {
                let byte = self.peek(0)?;
                self.pos += 1;
                if byte == b'\\' && !no_backslash_escapes {
                    let escaped = self.peek(0)?;
                    self.pos += 1;
                    match escaped {
                        b'0' => value.push(0),
                        b'b' => value.push(0x08),
                        b'n' => value.push(b'\n'),
                        b'r' => value.push(b'\r'),
                        b't' => value.push(b'\t'),
                        b'Z' => value.push(0x1a),
                        // Kept as is outside pattern matching.
                        b'%' | b'_' => value.extend_from_slice(&[b'\\', escaped]),
                        other => value.push(other),
                    }
                } else if byte == quote {
                    if self.peek(0) == Some(quote) {
                        self.pos += 1;
                        value.push(quote);
                    } else {
                        break;
                    }
                } else {
                    value.push(byte);
                }
            }
            pieces += 1;
            self.skip_space();
        }
        (pieces > 0).then_some(value)
    }

    fn skip_line(&mut self) {
        while let Some(byte) = self.peek(0) {
            self.pos += 1;
            if byte == b'\n' {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::escape_into;
    use super::insert_values;
    use super::local_infile_names;
    use super::mentions_local_infile;

    fn escaped(value: &[u8], no_backslash_escapes: bool) -> Vec<u8> {
        let mut out = Vec::new();
        escape_into(&mut out, value, no_backslash_escapes);
        out
    }

    #[test]
    fn backslash_escaping() {
        assert_eq!(
            escaped(b"a'b\"c\\d\0e\nf\rg\x1ah", false),
            b"a\\'b\\\"c\\\\d\\0e\\nf\\rg\\Zh"
        );
        assert_eq!(escaped(b"plain", false), b"plain");
        assert_eq!(escaped(b"", false), b"");
    }

    #[test]
    fn no_backslash_escapes_only_doubles_quotes() {
        assert_eq!(escaped(b"a'b\\c\0", true), b"a''b\\c\0");
    }

    #[test]
    fn utf8_is_untouched() {
        assert_eq!(escaped("ñ'🦀".as_bytes(), false), "ñ\\'🦀".as_bytes());
    }

    fn parts(query: &str) -> Option<(String, String, String)> {
        let q = query.as_bytes();
        insert_values(q).map(|p| {
            let s = |r: std::ops::Range<usize>| String::from_utf8(q[r].to_vec()).unwrap();
            (s(p.prefix), s(p.row), s(p.suffix))
        })
    }

    #[test]
    fn recognises_batchable_inserts() {
        assert_eq!(
            parts("INSERT INTO t (a, b) VALUES (%s, %s)"),
            Some((
                "INSERT INTO t (a, b) VALUES ".into(),
                "(%s, %s)".into(),
                String::new()
            ))
        );
        assert_eq!(
            parts("  replace into t value (%(a)s, NOW(), 'x)y');\n"),
            Some((
                "  replace into t value ".into(),
                "(%(a)s, NOW(), 'x)y')".into(),
                String::new()
            ))
        );
        assert_eq!(
            parts("INSERT INTO t VALUES (%s) ON DUPLICATE KEY UPDATE a = VALUES(a)"),
            Some((
                "INSERT INTO t VALUES ".into(),
                "(%s)".into(),
                " ON DUPLICATE KEY UPDATE a = VALUES(a)".into()
            ))
        );
        assert_eq!(
            parts("INSERT INTO t VALUES (%s) AS new ON DUPLICATE KEY UPDATE a = new.a"),
            Some((
                "INSERT INTO t VALUES ".into(),
                "(%s)".into(),
                " AS new ON DUPLICATE KEY UPDATE a = new.a".into()
            ))
        );
    }

    #[test]
    fn ignores_values_inside_strings_comments_and_subqueries() {
        assert_eq!(
            parts("INSERT INTO `values` /* VALUES (x) */ VALUES (%s)").map(|p| p.1),
            Some("(%s)".into())
        );
        assert_eq!(
            parts("INSERT INTO t -- VALUES (x)\nVALUES (%s)").map(|p| p.1),
            Some("(%s)".into())
        );
        assert_eq!(
            parts("INSERT INTO t (c) VALUES ('it''s \\' VALUES (')").map(|p| p.1),
            Some("('it''s \\' VALUES (')".into())
        );
    }

    #[test]
    fn leaves_everything_else_alone() {
        for query in [
            "UPDATE t SET a = %s",
            "SELECT * FROM t WHERE a IN (SELECT 1) VALUES",
            "INSERT INTO t SELECT * FROM u",
            "INSERT INTO t SET a = %s",
            "INSERT INTO t VALUES (%s), (%s)",
            "INSERT INTO t VALUES ROW(%s)",
            "INSERT INTO t VALUES (%s",
            "INSERT INTO t VALUES ('unterminated)",
            "INSERT INTO t (a VALUES (%s)",
            "INSERTX INTO t VALUES (%s)",
            "",
        ] {
            assert_eq!(parts(query), None, "{query}");
        }
    }

    fn names(query: &str, no_backslash_escapes: bool) -> Vec<String> {
        local_infile_names(query.as_bytes(), no_backslash_escapes)
            .into_iter()
            .map(|name| String::from_utf8(name).unwrap())
            .collect()
    }

    #[test]
    fn local_infile_names_as_the_server_reads_them() {
        let cases = [
            ("LOAD DATA LOCAL INFILE 'a.csv' INTO TABLE t", vec!["a.csv"]),
            (
                "load data local infile \"b.csv\" into table t",
                vec!["b.csv"],
            ),
            (
                "LOAD DATA LOW_PRIORITY LOCAL INFILE '/tmp/c.csv' REPLACE INTO TABLE t",
                vec!["/tmp/c.csv"],
            ),
            (
                "LOAD DATA CONCURRENT LOCAL INFILE 'd' INTO TABLE t",
                vec!["d"],
            ),
            ("LOAD XML LOCAL INFILE 'e.xml' INTO TABLE t", vec!["e.xml"]),
            (
                "LOAD /* c */ DATA\n-- c\nLOCAL # c\n INFILE   'f' INTO TABLE t",
                vec!["f"],
            ),
            (
                r"LOAD DATA LOCAL INFILE 'it''s \'q\' \\ \n' INTO TABLE t",
                vec!["it's 'q' \\ \n"],
            ),
            (
                r"LOAD DATA LOCAL INFILE 'a\%b\_c\x' INTO TABLE t",
                vec![r"a\%b\_cx"],
            ),
            (
                r#"LOAD DATA LOCAL INFILE 'part' "s" 'end' INTO TABLE t"#,
                vec!["partsend"],
            ),
            (
                "LOAD DATA LOCAL INFILE 'ñ 🦀.csv' INTO TABLE t",
                vec!["ñ 🦀.csv"],
            ),
            (
                "SELECT 1; LOAD DATA LOCAL INFILE 'x' INTO TABLE t; \
                 LOAD DATA LOCAL INFILE 'y' INTO TABLE u",
                vec!["x", "y"],
            ),
        ];
        for (query, expected) in cases {
            assert_eq!(names(query, false), expected, "{query}");
            assert!(mentions_local_infile(query.as_bytes()), "{query}");
        }
        assert_eq!(
            names(r"LOAD DATA LOCAL INFILE 'C:\data\x.csv' INTO TABLE t", true),
            [r"C:\data\x.csv"]
        );
        assert_eq!(
            names("LOAD DATA LOCAL INFILE 'it''s' INTO TABLE t", true),
            ["it's"]
        );
    }

    #[test]
    fn local_infile_needs_the_whole_statement_head() {
        for query in [
            "LOAD DATA INFILE 'server-side.csv' INTO TABLE t",
            "SELECT 'LOAD DATA LOCAL INFILE ''x'''",
            "SELECT 1 -- LOAD DATA LOCAL INFILE 'x'",
            "SELECT `LOAD DATA LOCAL INFILE 'x'`",
            "LOAD DATA /*!LOCAL*/ INFILE 'x' INTO TABLE t",
            "DOWNLOAD DATA LOCAL INFILE 'x'",
            "LOAD DATA x LOCAL INFILE 'x'",
            "LOAD, DATA LOCAL INFILE 'x'",
            "",
        ] {
            assert_eq!(names(query, false), Vec::<String>::new(), "{query}");
            assert!(!mentions_local_infile(query.as_bytes()), "{query}");
        }
        // A placeholder: the statement is there, its name not yet.
        let template = "LOAD DATA LOCAL INFILE %s INTO TABLE t";
        assert!(mentions_local_infile(template.as_bytes()));
        assert_eq!(names(template, false), Vec::<String>::new());
        // Unterminated literals name nothing.
        assert_eq!(
            names("LOAD DATA LOCAL INFILE 'x", false),
            Vec::<String>::new()
        );
        assert_eq!(
            names(r"LOAD DATA LOCAL INFILE 'x\", false),
            Vec::<String>::new()
        );
    }

    #[test]
    fn arbitrary_bytes_never_panic() {
        let samples: [&[u8]; 5] = [
            b"INSERT\xff VALUES (",
            b"INSERT INTO t VALUES ('\\",
            b"INSERT /*",
            b"INSERT INTO t VALUES ()",
            b")))INSERT VALUES ((((",
        ];
        for sample in samples {
            let _ = insert_values(sample);
            let _ = local_infile_names(sample, false);
            let _ = escaped(sample, false);
            let _ = escaped(sample, true);
        }
    }
}
