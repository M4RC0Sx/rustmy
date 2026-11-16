"""rustmy's hot paths, tracked by CodSpeed on every pull request.

CodSpeed counts CPU instructions instead of timing, so small regressions show
on noisy CI runners; network waits are not counted. Unlike the scripts in
benches/, these compare rustmy with itself, not with other drivers:

    uv run pytest benches/codspeed --codspeed    # needs the compose MySQL
"""

from __future__ import annotations

import datetime as dt
import uuid

from decimal import Decimal
from typing import TYPE_CHECKING

from conftest import ROWS
from conftest import connect
from rustmy.converters import literal
from rustmy.cursors import Cursor
from rustmy.cursors import DictCursor
from rustmy.cursors import SSCursor


if TYPE_CHECKING:
    from pytest_codspeed import BenchmarkFixture

    import rustmy


PARAMETERS = [
    1,
    -(2**63),
    1.5,
    Decimal("12345.678"),
    "plain text",
    'it\'s "quoted" \\ with ñ and 😀',
    b"\x00\x01binary'\\",
    dt.date(2026, 1, 2),
    dt.datetime(2026, 1, 2, 3, 4, 5, 678901),  # noqa: DTZ001
    dt.timedelta(hours=-1, microseconds=5),
    uuid.UUID("12345678-1234-5678-1234-567812345678"),
    None,
    True,
] * 100


# Escaping and interpolation (no server) ----------------------------------------


def test_literal(benchmark: BenchmarkFixture) -> None:
    benchmark(lambda: [literal(value) for value in PARAMETERS])


def test_mogrify(benchmark: BenchmarkFixture, conn: rustmy.Connection) -> None:
    cursor = conn.cursor()
    template = "INSERT INTO t VALUES (" + ", ".join(["%s"] * len(PARAMETERS)) + ")"
    benchmark(cursor.mogrify, template, PARAMETERS)


# Reading rows ------------------------------------------------------------------


def _fetchall(
    conn: rustmy.Connection, sql: str, cursor_class: type[Cursor] = Cursor
) -> None:
    cursor = conn.cursor(cursor_class)
    cursor.execute(sql)
    assert len(cursor.fetchall()) == ROWS


def test_fetchall_int_varchar(
    benchmark: BenchmarkFixture, conn: rustmy.Connection
) -> None:
    benchmark(_fetchall, conn, "SELECT * FROM codspeed_intvarchar")


def test_fetchall_mixed(benchmark: BenchmarkFixture, conn: rustmy.Connection) -> None:
    benchmark(_fetchall, conn, "SELECT * FROM codspeed_mixed")


def test_dict_fetchall_mixed(
    benchmark: BenchmarkFixture, conn: rustmy.Connection
) -> None:
    benchmark(_fetchall, conn, "SELECT * FROM codspeed_mixed", DictCursor)


def test_stream_int_varchar(
    benchmark: BenchmarkFixture, conn: rustmy.Connection
) -> None:
    def stream() -> None:
        cursor = conn.cursor(SSCursor)
        cursor.execute("SELECT * FROM codspeed_intvarchar")
        assert sum(1 for _ in cursor) == ROWS

    benchmark(stream)


def test_fetchall_latin1(benchmark: BenchmarkFixture, tables: None) -> None:  # noqa: ARG001
    # Single-byte character set: decoded through rustmy's 256-entry table.
    latin1 = connect(charset="latin1")
    try:
        benchmark(_fetchall, latin1, "SELECT s FROM codspeed_mixed")
    finally:
        latin1.close()


# Writing rows ------------------------------------------------------------------


def test_executemany(benchmark: BenchmarkFixture, conn: rustmy.Connection) -> None:
    rows = [(i, f"name-{i} ñ", i / 7) for i in range(ROWS)]
    cursor = conn.cursor()

    def empty() -> None:
        cursor.execute("TRUNCATE codspeed_insert")

    def insert() -> None:
        cursor.executemany("INSERT INTO codspeed_insert VALUES (%s, %s, %s)", rows)

    benchmark.pedantic(insert, setup=empty, rounds=3)
