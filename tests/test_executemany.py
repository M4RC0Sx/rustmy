"""executemany: batched multi-row INSERTs and the row-by-row fallback."""

from __future__ import annotations

import datetime as dt

from decimal import Decimal
from typing import TYPE_CHECKING

import pytest


if TYPE_CHECKING:
    from collections.abc import Iterator

    import rustmy

    from tests.conftest import Server


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
    assert cursor.fetchall() == rows


def test_named_parameters_and_generators(cursor: rustmy.Cursor, table: str) -> None:
    def gen() -> Iterator[dict[str, object]]:
        for i in range(3):
            yield {"n": i, "s": f"v{i}"}

    assert (
        cursor.executemany(f"INSERT INTO {table} (n, s) VALUES (%(n)s, %(s)s)", gen())
        == 3
    )
    cursor.execute(f"SELECT s FROM {table} ORDER BY n")
    assert cursor.fetchall() == [("v0",), ("v1",), ("v2",)]


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
    assert cursor.fetchall() == [(0, "row 0"), (1, "row 1!"), (2, "row 2!"), (3, "new")]


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
    assert cursor.fetchall() == [("a",), ("b",)]


def test_percent_signs_in_batched_inserts(cursor: rustmy.Cursor, table: str) -> None:
    cursor.executemany(
        f"INSERT INTO {table} (n, s) VALUES (%s, CONCAT('100%%', %s))",
        [(1, "a"), (2, "b")],
    )
    cursor.execute(f"SELECT s FROM {table} ORDER BY n")
    assert cursor.fetchall() == [("100%a",), ("100%b",)]


def test_other_statements_run_row_by_row(cursor: rustmy.Cursor, table: str) -> None:
    cursor.executemany(f"INSERT INTO {table} (n, s) VALUES (%s, %s)", _rows(4))
    count = cursor.executemany(
        f"UPDATE {table} SET s = %s WHERE n = %s", [("a", 0), ("b", 1), ("c", 99)]
    )
    assert count == 2
    cursor.execute(f"SELECT s FROM {table} WHERE n < 2 ORDER BY n")
    assert cursor.fetchall() == [("a",), ("b",)]


def test_empty_sequence_runs_nothing(cursor: rustmy.Cursor, table: str) -> None:
    before = _insert_statements(cursor)
    assert cursor.executemany(f"INSERT INTO {table} (n) VALUES (%s)", []) == 0
    assert _insert_statements(cursor) == before


def test_executed_shows_last_query(cursor: rustmy.Cursor) -> None:
    assert cursor._executed is None  # noqa: SLF001
    cursor.execute("SELECT %s", ("x",))
    assert cursor._executed == "SELECT 'x'"  # noqa: SLF001
