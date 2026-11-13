"""Every column type comes back as the right Python type and value."""

from __future__ import annotations

import datetime as dt

from decimal import Decimal
from typing import TYPE_CHECKING

import pytest


if TYPE_CHECKING:
    import rustmy

    from tests.conftest import Server


CASES: list[tuple[str, str, object]] = [
    ("TINYINT", "-128", -128),
    ("TINYINT UNSIGNED", "255", 255),
    ("SMALLINT", "-32768", -32768),
    ("MEDIUMINT", "8388607", 8388607),
    ("INT", "-2147483648", -2147483648),
    ("INT UNSIGNED", "4294967295", 4294967295),
    ("BIGINT", "-9223372036854775808", -(2**63)),
    ("BIGINT UNSIGNED", "18446744073709551615", 2**64 - 1),
    ("YEAR", "2026", 2026),
    ("FLOAT", "1.5", 1.5),
    ("DOUBLE", "-1.25e-300", -1.25e-300),
    ("DECIMAL(20,6)", "-12345678901234.123456", Decimal("-12345678901234.123456")),
    ("DECIMAL(5,2)", "0", Decimal("0.00")),
    ("DATE", "'2026-11-13'", dt.date(2026, 11, 13)),
    ("DATETIME", "'2026-11-13 21:05:09'", dt.datetime(2026, 11, 13, 21, 5, 9)),
    (
        "DATETIME(6)",
        "'2026-11-13 21:05:09.000123'",
        dt.datetime(2026, 11, 13, 21, 5, 9, 123),
    ),
    (
        "TIMESTAMP(3)",
        "'2026-11-13 21:05:09.5'",
        dt.datetime(2026, 11, 13, 21, 5, 9, 500000),
    ),
    ("TIME", "'25:00:01'", dt.timedelta(hours=25, seconds=1)),
    (
        "TIME(6)",
        "'-838:59:59.000000'",
        -dt.timedelta(hours=838, minutes=59, seconds=59),
    ),
    ("CHAR(3)", "'abc'", "abc"),
    ("VARCHAR(20)", "'héllo 🦀'", "héllo 🦀"),
    ("TEXT", "'line\\nbreak'", "line\nbreak"),
    ("ENUM('a','b')", "'b'", "b"),
    ("SET('a','b')", "'a,b'", "a,b"),
    ("BINARY(2)", "X'0102'", b"\x01\x02"),
    ("VARBINARY(10)", "X'00FF10'", b"\x00\xff\x10"),
    ("BLOB", "X'DEADBEEF'", b"\xde\xad\xbe\xef"),
    ("BIT(8)", "b'10100101'", b"\xa5"),
    ("JSON", "'{\"k\": [1, 2]}'", '{"k": [1, 2]}'),
    ("INT", "NULL", None),
    ("VARCHAR(5)", "NULL", None),
]


@pytest.mark.parametrize(("column_type", "sql_literal", "expected"), CASES)
def test_column_type_roundtrip(
    cursor: rustmy.Cursor, column_type: str, sql_literal: str, expected: object
) -> None:
    cursor.execute(f"CREATE TEMPORARY TABLE t (v {column_type})")
    cursor.execute(f"INSERT INTO t VALUES ({sql_literal})")
    cursor.execute("SELECT v FROM t")
    (value,) = cursor.fetchone() or ()
    assert value == expected
    assert type(value) is type(expected)


def test_zero_date_is_none(cursor: rustmy.Cursor) -> None:
    cursor.execute("SET SESSION sql_mode = ''")
    cursor.execute("CREATE TEMPORARY TABLE t (d DATE, ts DATETIME)")
    cursor.execute("INSERT INTO t VALUES ('0000-00-00', '0000-00-00 00:00:00')")
    cursor.execute("SELECT d, ts FROM t")
    assert cursor.fetchall() == [(None, None)]


# `SELECT %s` keeps these types: the literal alone tells the server the type.
SCALARS: list[object] = [
    None,
    0,
    -(2**63),
    2**64 - 1,
    Decimal("-1234567890.0123456789"),
    "",
    "quote ' double \" backslash \\ nul \0 newline \n ctrl-z \x1a",
    "unicode 🦀 ñ 中文",
    bytes(range(256)),
    b"",
]


@pytest.mark.parametrize("value", SCALARS, ids=repr)
@pytest.mark.parametrize("no_backslash_escapes", [False, True])
def test_parameter_roundtrip(
    cursor: rustmy.Cursor, value: object, *, no_backslash_escapes: bool
) -> None:
    if no_backslash_escapes:
        cursor.execute("SET SESSION sql_mode = 'NO_BACKSLASH_ESCAPES'")
        assert cursor.connection.no_backslash_escapes
    cursor.execute("SELECT %s", (value,))
    assert cursor.fetchone() == (value,)


def test_typed_parameter_roundtrip(cursor: rustmy.Cursor) -> None:
    """Temporal and float parameters need a typed column to come back typed."""
    values = (
        dt.date(1, 1, 1),
        dt.datetime(9999, 12, 31, 23, 59, 59, 999999),
        dt.timedelta(hours=-838, minutes=-59, seconds=-59),
        0.1,
        True,
    )
    cursor.execute(
        "CREATE TEMPORARY TABLE t "
        "(d DATE, ts DATETIME(6), t TIME(6), f DOUBLE, b BOOLEAN)"
    )
    cursor.execute("INSERT INTO t VALUES (%s, %s, %s, %s, %s)", values)
    cursor.execute("SELECT d, ts, t, f, b FROM t")
    # BOOLEAN is TINYINT(1): it comes back as the integer 1.
    assert cursor.fetchone() == (*values[:4], 1)


def test_server_marks_text_columns_as_unicode(
    cursor: rustmy.Cursor, server: Server
) -> None:
    cursor.execute("SELECT CONVERT('x' USING utf8mb4), CAST('x' AS BINARY)")
    assert cursor.fetchone() == ("x", b"x"), server.name
