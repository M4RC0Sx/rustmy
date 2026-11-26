"""executemany: batched multi-row INSERTs, and pipelined statements for the
rest."""

from __future__ import annotations

import datetime as dt

from decimal import Decimal
from typing import TYPE_CHECKING

import pytest

import rustmy

from rustmy import aio
from rustmy import cursors


if TYPE_CHECKING:
    from collections.abc import Iterator

    from tests.conftest import Server


@pytest.fixture(params=["text", "prepared"])
def cursor(conn: rustmy.Connection, request: pytest.FixtureRequest) -> rustmy.Cursor:
    """Every test runs with a Cursor and with a PreparedCursor, which batch
    and pipeline the same way (as text, or with typed parameters)."""
    return conn.cursor(prepared=request.param == "prepared")


@pytest.fixture
def table(cursor: rustmy.Cursor) -> str:
    cursor.execute(
        "CREATE TEMPORARY TABLE t ("
        "id INT AUTO_INCREMENT PRIMARY KEY, n INT UNIQUE, s VARCHAR(64), "
        "b VARBINARY(16), d DECIMAL(10, 2), ts DATETIME(6))"
    )
    return "t"


def _insert_statements(cursor: rustmy.Cursor) -> int:
    """Server-side count of INSERT statements run by this session.

    Uses a separate cursor so the one under test keeps its rowcount.
    """
    status = cursor.connection.cursor()
    status.execute("SHOW SESSION STATUS LIKE 'Com_insert'")
    (_, value) = status.fetchone() or ("", "0")
    return int(value)


def _rows(n: int) -> list[tuple[int, str]]:
    return [(i, f"row {i}") for i in range(n)]


def test_inserts_are_batched_into_one_statement(
    cursor: rustmy.Cursor, table: str
) -> None:
    before = _insert_statements(cursor)
    count = cursor.executemany(
        f"INSERT INTO {table} (n, s) VALUES (%s, %s)", _rows(1000)
    )
    after = _insert_statements(cursor)
    assert count == cursor.rowcount == 1000
    assert after - before == 1
    cursor.execute(f"SELECT COUNT(*), MIN(n), MAX(n) FROM {table}")
    assert cursor.fetchone() == (1000, 0, 999)


def test_batches_respect_max_stmt_length(cursor: rustmy.Cursor, table: str) -> None:
    cursor.max_stmt_length = 200
    before = _insert_statements(cursor)
    cursor.executemany(f"INSERT INTO {table} (n, s) VALUES (%s, %s)", _rows(100))
    # lastrowid is the first id of the last statement, as with one INSERT.
    lastrowid = cursor.lastrowid
    statements = _insert_statements(cursor) - before
    assert 1 < statements < 100
    assert lastrowid is not None
    assert 1 < lastrowid <= 100
    cursor.execute(f"SELECT COUNT(*) FROM {table}")
    assert cursor.fetchone() == (100,)


def test_default_statement_sizes() -> None:
    """Text statements as large as PyMySQL's, which still fit the 1 MiB
    ``max_allowed_packet`` of old servers with the command byte; prepared
    ones smaller, since the server prepares each one (larger ones measured
    slower)."""
    for text in (cursors.Cursor, cursors.DictCursor, cursors.SSCursor, aio.Cursor):
        assert text.max_stmt_length == 1_024_000
        assert text.max_stmt_length + 1 <= 1 << 20
    for prepared in (
        cursors.PreparedCursor,
        cursors.PreparedDictCursor,
        aio.PreparedCursor,
        aio.PreparedDictCursor,
    ):
        assert prepared.max_stmt_length == 256 * 1024


def test_row_larger_than_limit_goes_alone(cursor: rustmy.Cursor, table: str) -> None:
    cursor.max_stmt_length = 10
    before = _insert_statements(cursor)
    cursor.executemany(f"INSERT INTO {table} (n, s) VALUES (%s, %s)", _rows(5))
    assert _insert_statements(cursor) - before == 5


def test_values_roundtrip_through_batches(cursor: rustmy.Cursor, table: str) -> None:
    rows = [
        (
            1,
            "quote ' backslash \\ 🦀",
            b"\x00\xff'",
            Decimal("-1.25"),
            dt.datetime(2026, 11, 13, 21, 5, 9, 1),
        ),
        (2, None, None, None, None),
    ]
    cursor.executemany(
        f"INSERT INTO {table} (n, s, b, d, ts) VALUES (%s, %s, %s, %s, %s)", rows
    )
    cursor.execute(f"SELECT n, s, b, d, ts FROM {table} ORDER BY n")
    assert cursor.fetchall() == tuple(rows)


def test_named_parameters_and_generators(cursor: rustmy.Cursor, table: str) -> None:
    def gen() -> Iterator[dict[str, object]]:
        for i in range(3):
            yield {"n": i, "s": f"v{i}"}

    assert (
        cursor.executemany(f"INSERT INTO {table} (n, s) VALUES (%(n)s, %(s)s)", gen())
        == 3
    )
    cursor.execute(f"SELECT s FROM {table} ORDER BY n")
    assert cursor.fetchall() == (("v0",), ("v1",), ("v2",))


def test_on_duplicate_key_update(cursor: rustmy.Cursor, table: str) -> None:
    cursor.executemany(f"INSERT INTO {table} (n, s) VALUES (%s, %s)", _rows(3))
    before = _insert_statements(cursor)
    count = cursor.executemany(
        f"INSERT INTO {table} (n, s) VALUES (%s, %s) "
        "ON DUPLICATE KEY UPDATE s = CONCAT(s, '!')",
        [(1, "x"), (2, "x"), (3, "new")],
    )
    assert _insert_statements(cursor) - before == 1
    assert count == 2 + 2 + 1  # MySQL counts an updated row twice
    cursor.execute(f"SELECT n, s FROM {table} ORDER BY n")
    assert cursor.fetchall() == ((0, "row 0"), (1, "row 1!"), (2, "row 2!"), (3, "new"))


def test_row_alias_on_mysql(server: Server, cursor: rustmy.Cursor, table: str) -> None:
    if server.name != "mysql":
        pytest.skip("INSERT … AS alias is MySQL 8.0.19+ syntax")
    cursor.executemany(f"INSERT INTO {table} (n, s) VALUES (%s, %s)", _rows(2))
    cursor.executemany(
        f"INSERT INTO {table} (n, s) VALUES (%s, %s) AS new "
        "ON DUPLICATE KEY UPDATE s = new.s",
        [(0, "a"), (1, "b")],
    )
    cursor.execute(f"SELECT s FROM {table} ORDER BY n")
    assert cursor.fetchall() == (("a",), ("b",))


def test_percent_signs_in_batched_inserts(cursor: rustmy.Cursor, table: str) -> None:
    cursor.executemany(
        f"INSERT INTO {table} (n, s) VALUES (%s, CONCAT('100%%', %s))",
        [(1, "a"), (2, "b")],
    )
    cursor.execute(f"SELECT s FROM {table} ORDER BY n")
    assert cursor.fetchall() == (("100%a",), ("100%b",))


def test_other_statements_run_once_per_item(cursor: rustmy.Cursor, table: str) -> None:
    cursor.executemany(f"INSERT INTO {table} (n, s) VALUES (%s, %s)", _rows(4))
    count = cursor.executemany(
        f"UPDATE {table} SET s = %s WHERE n = %s", [("a", 0), ("b", 1), ("c", 99)]
    )
    assert count == 2
    cursor.execute(f"SELECT s FROM {table} WHERE n < 2 ORDER BY n")
    assert cursor.fetchall() == (("a",), ("b",))


def test_empty_sequence_runs_nothing(cursor: rustmy.Cursor, table: str) -> None:
    before = _insert_statements(cursor)
    assert cursor.executemany(f"INSERT INTO {table} (n) VALUES (%s)", []) == 0
    assert _insert_statements(cursor) == before


def test_executed_shows_last_query(cursor: rustmy.Cursor) -> None:
    assert cursor._executed is None  # noqa: SLF001
    cursor.execute("SELECT %s", ("x",))
    assert cursor._executed == "SELECT 'x'"  # noqa: SLF001


def test_executed_shows_the_last_batch(cursor: rustmy.Cursor, table: str) -> None:
    cursor.max_stmt_length = 40
    query = f"INSERT INTO {table} (n, s) VALUES (%s, %s)"
    cursor.executemany(query, [(1, "a"), (2, "b'c"), (3, "d")])
    assert cursor._executed == f"INSERT INTO {table} (n, s) VALUES (3, 'd')"  # noqa: SLF001
    # Nothing to run leaves the previous statement in place.
    cursor.executemany(query, [])
    assert cursor._executed == f"INSERT INTO {table} (n, s) VALUES (3, 'd')"  # noqa: SLF001


# Pipelined executemany: anything but INSERT … VALUES.


def _filled(cursor: rustmy.Cursor, table: str, n: int) -> None:
    cursor.executemany(f"INSERT INTO {table} (n, s) VALUES (%s, %s)", _rows(n))


def _updated(cursor: rustmy.Cursor, table: str) -> list[int]:
    check = cursor.connection.cursor()
    check.execute(f"SELECT n FROM {table} WHERE d IS NOT NULL ORDER BY n")
    return [n for (n,) in check.fetchall()]


def test_pipelined_updates_add_up(cursor: rustmy.Cursor, table: str) -> None:
    _filled(cursor, table, 1000)
    count = cursor.executemany(
        f"UPDATE {table} SET s = %(s)s WHERE n = %(n)s",
        [{"n": i, "s": f"new {i}"} for i in range(1000)] + [{"n": -1, "s": "x"}],
    )
    assert count == cursor.rowcount == 1000
    assert cursor.description is None
    assert cursor.lastrowid == 0
    cursor.execute(f"SELECT COUNT(*) FROM {table} WHERE s = CONCAT('new ', n)")
    assert cursor.fetchone() == (1000,)


@pytest.mark.parametrize(("max_pipeline", "updated"), [(4, [0, 2, 3]), (1, [0])])
def test_error_stops_at_the_end_of_its_window(
    cursor: rustmy.Cursor, table: str, max_pipeline: int, updated: list[int]
) -> None:
    """The statements sent with the failing one have run; no other window
    is sent."""
    _filled(cursor, table, 10)
    cursor.max_pipeline = max_pipeline
    values = [(Decimal(1), i) for i in range(10)]
    values[1] = (Decimal(10) ** 12, 1)  # out of range for DECIMAL(10, 2)
    with pytest.raises(rustmy.DataError):
        cursor.executemany(f"UPDATE {table} SET d = %s WHERE n = %s", values)
    assert _updated(cursor, table) == updated
    cursor.connection.rollback()
    assert _updated(cursor, table) == []


def test_error_in_the_last_window(cursor: rustmy.Cursor, table: str) -> None:
    _filled(cursor, table, 3)
    values = [(Decimal(1), 0), (Decimal(1), 1), (Decimal(10) ** 12, 2)]
    with pytest.raises(rustmy.DataError):
        cursor.executemany(f"UPDATE {table} SET d = %s WHERE n = %s", values)
    assert cursor.description is None
    assert _updated(cursor, table) == [0, 1]
    # In sync, with the error's statement as the last one sent.
    cursor.execute("SELECT 1")
    assert cursor.fetchall() == ((1,),)


def test_iteration_error_runs_the_items_before(
    cursor: rustmy.Cursor, table: str
) -> None:
    _filled(cursor, table, 10)

    def values() -> Iterator[tuple[Decimal, int]]:
        yield (Decimal(1), 0)
        yield (Decimal(1), 1)
        msg = "no more"
        raise LookupError(msg)

    with pytest.raises(LookupError, match="no more"):
        cursor.executemany(f"UPDATE {table} SET d = %s WHERE n = %s", values())
    assert _updated(cursor, table) == [0, 1]


def test_interpolation_error_runs_the_items_before(
    cursor: rustmy.Cursor, table: str
) -> None:
    _filled(cursor, table, 10)
    values: list[tuple[object, ...]] = [(Decimal(1), 0), (Decimal(1),), (1, 2)]
    with pytest.raises(rustmy.ProgrammingError):
        cursor.executemany(f"UPDATE {table} SET d = %s WHERE n = %s", values)
    assert _updated(cursor, table) == [0]


@pytest.mark.parametrize(
    "cursorclass",
    [rustmy.cursors.Cursor, rustmy.cursors.DictCursor, rustmy.cursors.SSCursor],
)
def test_cursor_keeps_the_last_result(
    conn: rustmy.Connection, cursorclass: type[rustmy.cursors.Cursor]
) -> None:
    cursor = conn.cursor(cursorclass)
    cursor.max_pipeline = 2
    cursor.executemany("SELECT %s AS v", [(i,) for i in range(5)])
    expected: object = {"v": 4} if cursorclass is rustmy.cursors.DictCursor else (4,)
    assert cursor.fetchall() == (expected,)
    assert cursor.description is not None
    # Each result set counts its rows, as execute() does; a streamed one
    # does not know its size.
    assert cursor.rowcount == (-1 if cursorclass is rustmy.cursors.SSCursor else 5)
    assert cursor._executed == "SELECT 4 AS v"  # noqa: SLF001
    cursor.execute("SELECT 'next' AS v")
    assert cursor.fetchone() == (
        {"v": "next"} if cursorclass is rustmy.cursors.DictCursor else ("next",)
    )


def test_streamed_cursor_adds_up_affected_rows(
    conn: rustmy.Connection, table: str
) -> None:
    cursor = conn.cursor(rustmy.cursors.SSCursor)
    _filled(cursor, table, 10)
    count = cursor.executemany(
        f"UPDATE {table} SET s = %s WHERE n = %s", [("x", i) for i in range(10)]
    )
    assert count == cursor.rowcount == 10


def test_multi_statement_template(server: Server) -> None:
    """Every reply of every statement is read; the last one's queued
    results stay for nextset(), as after execute()."""
    with server.connect(multi_statements=True) as multi:
        cursor = multi.cursor()
        cursor.max_pipeline = 2
        cursor.execute("CREATE TEMPORARY TABLE m (n INT)")
        count = cursor.executemany(
            "INSERT INTO m SELECT %s; SELECT COUNT(*) FROM m", [(1,), (2,), (3,)]
        )
        assert count == 3
        assert cursor.nextset()
        assert cursor.fetchall() == ((3,),)
        assert not cursor.nextset()
        cursor.execute("SELECT SUM(n) FROM m")
        assert cursor.fetchall() == ((Decimal(6),),)


def test_returning_on_mariadb(
    server: Server, cursor: rustmy.Cursor, table: str
) -> None:
    if server.name != "mariadb":
        pytest.skip("DELETE … RETURNING is MariaDB syntax")
    _filled(cursor, table, 5)
    cursor.max_pipeline = 2
    count = cursor.executemany(
        f"DELETE FROM {table} WHERE n = %s RETURNING n, s", [(i,) for i in range(5)]
    )
    assert count == 5
    assert cursor.fetchall() == ((4, "row 4"),)
    cursor.execute(f"SELECT COUNT(*) FROM {table}")
    assert cursor.fetchone() == (0,)


def test_statements_beyond_the_byte_budget(cursor: rustmy.Cursor) -> None:
    """Statements of 10 KiB: two never share a 16 KiB window, and one larger
    than the budget still goes (alone)."""
    values = [("x" * 10_000,), ("y" * 40_000,), ("z" * 10,)]
    cursor.executemany("SELECT LENGTH(%s)", values)
    assert cursor.fetchall() == ((10,),)
    assert cursor.rowcount == 3


def test_executed_shows_the_last_pipelined_statement(
    cursor: rustmy.Cursor, table: str
) -> None:
    _filled(cursor, table, 3)
    cursor.executemany(f"DELETE FROM {table} WHERE n = %s", [(0,), (2,)])
    assert cursor._executed == f"DELETE FROM {table} WHERE n = 2"  # noqa: SLF001


def test_parameters_may_query_the_same_connection(
    conn: rustmy.Connection, table: str
) -> None:
    """A generator of parameters can use the connection, as with a loop of
    execute() calls (each window is complete before the next is built)."""
    cursor = conn.cursor()
    _filled(cursor, table, 5)
    lookup = conn.cursor()

    def values() -> Iterator[tuple[str, int]]:
        for n in range(5):
            lookup.execute(f"SELECT s FROM {table} WHERE n = %s", (n,))
            (s,) = lookup.fetchone() or ("",)
            yield (f"{s}!", n)

    cursor.max_pipeline = 2
    assert cursor.executemany(f"UPDATE {table} SET s = %s WHERE n = %s", values()) == 5
    cursor.execute(f"SELECT s FROM {table} ORDER BY n")
    assert cursor.fetchall() == tuple((f"row {n}!",) for n in range(5))
