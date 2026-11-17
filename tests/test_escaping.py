"""Property tests for escaping and interpolation against real servers.

Every generated value goes into a query and must come back as it was, with
the server having received exactly its bytes (``HEX``) and the query keeping
its shape (a trailing sentinel column that a value breaking out of its
string, or a comment, would drop or change). Each property runs with the
server's default ``sql_mode`` and with ``NO_BACKSLASH_ESCAPES`` (plus
``ANSI_QUOTES``), which changes how strings must be escaped.

Budget: see the profiles in ``conftest.py`` (100 examples per property;
``--hypothesis-profile thorough`` for 5000).
"""

from __future__ import annotations

from functools import cache
from typing import TYPE_CHECKING

import pytest

from hypothesis import given
from hypothesis import strategies as st

import rustmy


if TYPE_CHECKING:
    from collections.abc import Iterator

    from tests.conftest import Server


MODES = {
    "default": "",
    "no_backslash_escapes": "NO_BACKSLASH_ESCAPES,ANSI_QUOTES",
}

#: Pieces of SQL that matter to a string literal, an interpolation template
#: or the statement around them.
TOKENS = [
    "'",
    '"',
    "`",
    "\\",
    "\\'",
    "''",
    "\\\\",
    "\x00",
    "\n",
    "\r",
    "\x1a",
    "\t",
    "%",
    "%%",
    "%s",
    "%(x)s",
    "--",
    "-- ",
    "#",
    "/*",
    "*/",
    "/*!",
    ";",
    " OR 1=1",
    "' OR '1'='1",
    "_",
    "?",
    "😀",
    "ñ",
]

#: Every Unicode scalar value (lone surrogates cannot be encoded at all).
texts = st.text(st.characters(exclude_categories=["Cs"]))
payloads = st.lists(st.sampled_from(TOKENS) | st.text(max_size=4)).map("".join)


def _hex(data: bytes) -> str:
    return data.hex().upper()


def _connect(server: Server, mode: str, **kwargs: object) -> rustmy.Connection:
    if not mode:
        return server.connect(**kwargs)
    with server.connect() as probe:
        cursor = probe.cursor()
        cursor.execute("SELECT @@SESSION.sql_mode")
        row = cursor.fetchone()
    default = "" if row is None else row[0]
    return server.connect(sql_mode=",".join(filter(None, [default, mode])), **kwargs)


@pytest.fixture(scope="module", params=list(MODES))
def session(server: Server, request: pytest.FixtureRequest) -> Iterator[rustmy.Cursor]:
    with _connect(server, MODES[request.param]) as conn:
        cursor = conn.cursor()
        cursor.execute("SELECT @@SESSION.sql_mode")
        row = cursor.fetchone()
        assert row is not None
        assert ("NO_BACKSLASH_ESCAPES" in row[0]) == bool(MODES[request.param])
        cursor.execute(
            "CREATE TEMPORARY TABLE known (s VARCHAR(20) CHARACTER SET utf8mb4)"
        )
        cursor.execute("INSERT INTO known VALUES ('known-row')")
        cursor.execute(
            "CREATE TEMPORARY TABLE probe (id INT AUTO_INCREMENT PRIMARY KEY, "
            "s LONGTEXT CHARACTER SET utf8mb4, b LONGBLOB)"
        )
        yield cursor


def _check_text(cursor: rustmy.Cursor, value: str, encoding: str = "utf8") -> None:
    cursor.execute("SELECT %s, HEX(CONVERT(%s USING utf8mb4)), 'end'", (value, value))
    assert cursor.fetchall() == ((value, _hex(value.encode()), "end"),)
    assert cursor.description is not None
    assert len(cursor.description) == 3
    if encoding != "utf8":
        # Sent in the session's character set, byte for byte.
        cursor.execute("SELECT HEX(%s)", (value,))
        assert cursor.fetchone() == (_hex(value.encode(encoding)),)


@given(value=texts)
def test_text_round_trips(session: rustmy.Cursor, value: str) -> None:
    _check_text(session, value)


@given(value=st.binary())
def test_bytes_round_trip(session: rustmy.Cursor, value: bytes) -> None:
    session.execute("SELECT %s, HEX(%s), 'end'", (value, value))
    assert session.fetchall() == ((value, _hex(value), "end"),)


@pytest.fixture(scope="module", params=list(MODES))
def unprefixed(
    server: Server, request: pytest.FixtureRequest
) -> Iterator[rustmy.Cursor]:
    """A session with ``binary_prefix=False``, as the MySQLdb alias opens."""
    with _connect(server, MODES[request.param], binary_prefix=False) as conn:
        yield conn.cursor()


@given(value=st.binary())
def test_bytes_round_trip_without_binary_prefix(
    unprefixed: rustmy.Cursor, value: bytes
) -> None:
    # Django's MySQL backend writes the introducer itself.
    unprefixed.execute(
        "SELECT _binary %s, HEX(_binary %s), %s = _binary %s, 'end'",
        (value, value, value, value),
    )
    assert unprefixed.fetchall() == ((value, _hex(value), 1, "end"),)


@given(value=payloads)
def test_injection_attempts_stay_values(session: rustmy.Cursor, value: str) -> None:
    _check_text(session, value)
    # A value that broke out of its string could match the row (OR 1=1).
    session.execute("SELECT COUNT(*) FROM known WHERE HEX(s) = HEX(%s)", (value,))
    assert session.fetchone() == (int(value == "known-row"),)
    # Named parameters go through the same escaping.
    session.execute("SELECT %(v)s, 'end'", {"v": value})
    assert session.fetchone() == (value, "end")


@given(
    rows=st.lists(st.tuples(payloads | texts, st.binary()), min_size=1, max_size=20),
    batch=st.sampled_from([64, 1024, 256 * 1024]),
)
def test_executemany_round_trips(
    session: rustmy.Cursor, rows: list[tuple[str, bytes]], batch: int
) -> None:
    session.execute("DELETE FROM probe")
    session.max_stmt_length = batch
    try:
        inserted = session.executemany("INSERT INTO probe (s, b) VALUES (%s, %s)", rows)
    finally:
        del session.max_stmt_length
    assert inserted == len(rows)
    session.execute("SELECT s, b FROM probe ORDER BY id")
    assert session.fetchall() == tuple(rows)


# Multi-byte character sets ---------------------------------------------------

#: Character sets in which a character can end in 0x5C (backslash): escaping
#: bytes instead of text would let a quote through (CVE-2006-2753). sjis and
#: ujis also have characters Python encodes as a plain backslash (`¥`).
DANGEROUS_CHARSETS = {
    "gbk": "gbk",
    "big5": "big5",
    "cp932": "cp932",
    "sjis": "shift_jis",
    "ujis": "euc_jp",
}


@cache
def _alphabet(codec: str) -> list[str]:
    """Characters ``codec`` encodes with a trailing backslash byte, those
    it encodes one way only (they do not decode back to themselves) and a
    sample of the rest."""
    alphabet: list[str] = []
    for code in range(0x80, 0x10000):
        if 0xD800 <= code < 0xE000:
            continue
        c = chr(code)
        try:
            encoded = c.encode(codec)
        except UnicodeEncodeError:
            continue
        one_way = encoded.decode(codec) != c
        if encoded.endswith(b"\\") or one_way or code % 37 == 0:
            alphabet.append(c)
    return alphabet


def _refuses(conn: rustmy.Connection, c: str) -> bool:
    try:
        conn.literal(c)
    except rustmy.DataError:
        return True
    return False


@pytest.fixture(
    scope="module",
    params=[(cs, mode) for cs in DANGEROUS_CHARSETS for mode in MODES],
    ids=lambda p: f"{p[0]}-{p[1]}",
)
def multibyte(
    server: Server, request: pytest.FixtureRequest
) -> Iterator[tuple[rustmy.Cursor, list[str], set[str], str]]:
    charset, mode = request.param
    alphabet = _alphabet(DANGEROUS_CHARSETS[charset])
    assert any(c.encode(DANGEROUS_CHARSETS[charset]).endswith(b"\\") for c in alphabet)
    with _connect(server, MODES[mode], charset=charset) as conn:
        # What the driver refuses on its own; test_charsets.py checks that
        # this is exactly what the server cannot represent.
        refused = {c for c in alphabet if _refuses(conn, c)}
        yield conn.cursor(), alphabet, refused, conn.encoding


@given(data=st.data())
def test_multibyte_injection_attempts_stay_values(
    multibyte: tuple[rustmy.Cursor, list[str], set[str], str], data: st.DataObject
) -> None:
    cursor, alphabet, refused, encoding = multibyte
    piece = st.sampled_from(alphabet) | st.sampled_from(TOKENS[:16])
    value = data.draw(st.lists(piece, max_size=30).map("".join))
    if refused.intersection(value):
        with pytest.raises(rustmy.DataError, match="cannot be encoded"):
            cursor.execute("SELECT %s, 'end'", (value,))
        return
    _check_text(cursor, value, encoding)


@pytest.mark.parametrize("charset", list(DANGEROUS_CHARSETS))
def test_multibyte_bytes_without_binary_prefix(server: Server, charset: str) -> None:
    # X'…' whatever binary_prefix says: an escaped byte string is unsafe in
    # these character sets, and a hex literal is valid after `_binary`.
    value = bytes(range(256)) + b"\x81\\'\x95\\\\'"
    with server.connect(charset=charset, binary_prefix=False) as conn:
        assert conn.literal(value) == f"X'{value.hex()}'"
        cursor = conn.cursor()
        cursor.execute("SELECT _binary %s, %s, 'end'", (value, value))
        assert cursor.fetchall() == ((value, value, "end"),)


@given(value=st.binary())
def test_multibyte_bytes_round_trip(
    multibyte: tuple[rustmy.Cursor, list[str], set[str], str], value: bytes
) -> None:
    cursor, *_ = multibyte
    cursor.execute("SELECT %s, HEX(%s), 'end'", (value, value))
    assert cursor.fetchall() == ((value, _hex(value), "end"),)
