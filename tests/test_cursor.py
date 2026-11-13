"""Cursor behaviour against a real server."""

from __future__ import annotations

from typing import TYPE_CHECKING

import pytest

import rustmy


if TYPE_CHECKING:
    from tests.conftest import Server


SEQ = "SELECT 1 AS n UNION ALL SELECT 2 UNION ALL SELECT 3 UNION ALL SELECT 4"


def test_fetch_methods_walk_the_result(cursor: rustmy.Cursor) -> None:
    assert cursor.execute(SEQ) == 4
    assert cursor.rowcount == 4
    assert cursor.rownumber == 0
    assert cursor.fetchone() == (1,)
    assert cursor.fetchmany(2) == [(2,), (3,)]
    assert cursor.rownumber == 3
    assert cursor.fetchall() == [(4,)]
    assert cursor.fetchone() is None
    assert cursor.fetchall() == []


def test_fetchall_after_execute_returns_every_row(cursor: rustmy.Cursor) -> None:
    cursor.execute(SEQ)
    assert cursor.fetchall() == [(1,), (2,), (3,), (4,)]
    assert cursor.fetchmany() == []


def test_iteration_and_arraysize(cursor: rustmy.Cursor) -> None:
    cursor.execute(SEQ)
    cursor.arraysize = 3
    assert len(cursor.fetchmany()) == 3
    cursor.execute(SEQ)
    assert [row[0] for row in cursor] == [1, 2, 3, 4]


def test_description(cursor: rustmy.Cursor) -> None:
    cursor.execute("SELECT 1 AS one, 'x' AS txt, NULL AS nothing")
    assert cursor.description is not None
    names = [column[0] for column in cursor.description]
    assert names == ["one", "txt", "nothing"]
    assert all(len(column) == 7 for column in cursor.description)
    assert cursor.description[0][1] == rustmy.NUMBER
    assert cursor.description[1][1] == rustmy.STRING


def test_dml_reports_rowcount_and_lastrowid(cursor: rustmy.Cursor) -> None:
    cursor.execute(
        "CREATE TEMPORARY TABLE t (id INT AUTO_INCREMENT PRIMARY KEY, v INT)"
    )
    assert cursor.description is None
    assert cursor.execute("INSERT INTO t (v) VALUES (%s), (%s)", (10, 20)) == 2
    assert cursor.lastrowid == 1
    assert cursor.execute("UPDATE t SET v = v + 1") == 2
    with pytest.raises(rustmy.ProgrammingError, match="no result set"):
        cursor.fetchone()


def test_executemany(cursor: rustmy.Cursor) -> None:
    cursor.execute("CREATE TEMPORARY TABLE t (a INT, b VARCHAR(10))")
    total = cursor.executemany(
        "INSERT INTO t VALUES (%s, %s)", [(1, "a"), (2, "b"), (3, None)]
    )
    assert total == 3
    cursor.execute("SELECT a, b FROM t ORDER BY a")
    assert cursor.fetchall() == [(1, "a"), (2, "b"), (3, None)]


def test_named_parameters(cursor: rustmy.Cursor) -> None:
    cursor.execute("SELECT %(a)s, %(b)s, %(a)s", {"a": 1, "b": "two"})
    assert cursor.fetchone() == (1, "two", 1)


def test_percent_is_literal_without_args(cursor: rustmy.Cursor) -> None:
    cursor.execute("SELECT '100%'")
    assert cursor.fetchone() == ("100%",)


def test_mogrify_shows_the_final_query(cursor: rustmy.Cursor) -> None:
    assert cursor.mogrify("SELECT %s", ("a'b",)) == "SELECT 'a\\'b'"


def test_multiple_result_sets(server: Server) -> None:
    multi_statements = 1 << 16
    with server.connect(client_flag=multi_statements) as conn:
        cursor = conn.cursor()
        cursor.execute("SELECT 1; SELECT 'two'; SELECT * FROM no_such_table")
        assert cursor.fetchall() == [(1,)]
        assert cursor.nextset() is True
        assert cursor.fetchall() == [("two",)]
        # The third statement fails: the error surfaces on nextset().
        with pytest.raises(rustmy.Error):
            cursor.nextset()
        assert cursor.nextset() is None
        cursor.execute("SELECT 3")
        assert cursor.fetchall() == [(3,)]


def test_closed_cursor_rejects_use(cursor: rustmy.Cursor) -> None:
    cursor.close()
    with pytest.raises(rustmy.ProgrammingError, match="cursor closed"):
        cursor.execute("SELECT 1")


def test_large_result(cursor: rustmy.Cursor) -> None:
    # Cross join of digit tables: recursive CTEs stop at 1000 rows by default.
    digits = " UNION ALL ".join(f"SELECT {d} AS d" for d in range(10))
    tables = ", ".join(f"({digits}) AS t{i}" for i in range(5))
    number = "t0.d + t1.d * 10 + t2.d * 100 + t3.d * 1000 + t4.d * 10000"
    cursor.execute(
        f"SELECT {number} AS n, CONCAT('row-', {number}) FROM {tables} "
        "ORDER BY n LIMIT 50000"
    )
    rows = cursor.fetchall()
    assert len(rows) == 50000
    assert rows[-1] == (49999, "row-49999")
