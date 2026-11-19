"""Cursor behaviour against a real server."""

from __future__ import annotations

from collections import OrderedDict
from collections.abc import Mapping
from types import MappingProxyType
from typing import TYPE_CHECKING
from typing import NamedTuple

import pytest

import rustmy


if TYPE_CHECKING:
    from collections.abc import Iterator
    from collections.abc import Sequence

    from tests.conftest import Server


SEQ = "SELECT 1 AS n UNION ALL SELECT 2 UNION ALL SELECT 3 UNION ALL SELECT 4"


def test_fetch_methods_walk_the_result(cursor: rustmy.Cursor) -> None:
    assert cursor.execute(SEQ) == 4
    assert cursor.rowcount == 4
    assert cursor.rownumber == 0
    assert cursor.fetchone() == (1,)
    assert cursor.fetchmany(2) == ((2,), (3,))
    assert cursor.rownumber == 3
    assert cursor.fetchall() == ((4,),)
    assert cursor.fetchone() is None
    assert cursor.fetchall() == ()


def test_fetchall_after_execute_returns_every_row(cursor: rustmy.Cursor) -> None:
    cursor.execute(SEQ)
    assert cursor.fetchall() == ((1,), (2,), (3,), (4,))
    assert cursor.fetchmany() == ()


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
    # PEP 249: no row id for a statement that returns rows (as PyMySQL).
    cursor.execute("SELECT v FROM t")
    assert cursor.lastrowid is None


def test_executemany(cursor: rustmy.Cursor) -> None:
    cursor.execute("CREATE TEMPORARY TABLE t (a INT, b VARCHAR(10))")
    total = cursor.executemany(
        "INSERT INTO t VALUES (%s, %s)", [(1, "a"), (2, "b"), (3, None)]
    )
    assert total == 3
    cursor.execute("SELECT a, b FROM t ORDER BY a")
    assert cursor.fetchall() == ((1, "a"), (2, "b"), (3, None))


def test_named_parameters(cursor: rustmy.Cursor) -> None:
    cursor.execute("SELECT %(a)s, %(b)s, %(a)s", {"a": 1, "b": "two"})
    assert cursor.fetchone() == (1, "two", 1)


class _Row(NamedTuple):
    a: int
    b: str


class _Tuple(tuple):  # type: ignore[type-arg]
    __slots__ = ()


class _List(list):  # type: ignore[type-arg]
    pass


class _Dict(dict):  # type: ignore[type-arg]
    pass


class _Mapping(Mapping[str, object]):
    def __init__(self, items: dict[str, object]) -> None:
        self._items = items

    def __getitem__(self, key: str) -> object:
        return self._items[key]

    def __iter__(self) -> Iterator[str]:
        return iter(self._items)

    def __len__(self) -> int:
        return len(self._items)


@pytest.mark.parametrize(
    "args",
    [
        (1, "x"),
        [1, "x"],
        _Row(1, "x"),
        _Tuple((1, "x")),
        _List([1, "x"]),
    ],
    ids=["tuple", "list", "namedtuple", "tuple-subclass", "list-subclass"],
)
def test_sequences_fill_positional_placeholders(
    cursor: rustmy.Cursor, args: Sequence[object]
) -> None:
    assert cursor.mogrify("SELECT %s, %s", args) == "SELECT 1, 'x'"


@pytest.mark.parametrize(
    "args",
    [
        {"a": 1, "b": "x"},
        _Dict(a=1, b="x"),
        OrderedDict(a=1, b="x"),
        MappingProxyType({"a": 1, "b": "x"}),
        _Mapping({"a": 1, "b": "x"}),
    ],
    ids=["dict", "dict-subclass", "ordereddict", "mappingproxy", "mapping"],
)
def test_mappings_fill_named_placeholders(
    cursor: rustmy.Cursor, args: Mapping[str, object]
) -> None:
    assert cursor.mogrify("SELECT %(a)s, %(b)s", args) == "SELECT 1, 'x'"


@pytest.mark.parametrize("value", [1, "x", b"x"], ids=repr)
def test_a_single_value_is_one_parameter(cursor: rustmy.Cursor, value: object) -> None:
    # Not a sequence: a str or bytes is one value, not one per character.
    expected = {1: "1", "x": "'x'", b"x": "_binary'x'"}
    assert cursor.mogrify("SELECT %s", value) == f"SELECT {expected[value]}"


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
        assert cursor.fetchall() == ((1,),)
        assert cursor.nextset() is True
        assert cursor.fetchall() == (("two",),)
        # The third statement fails: the error surfaces on nextset().
        with pytest.raises(rustmy.Error):
            cursor.nextset()
        assert cursor.nextset() is None
        cursor.execute("SELECT 3")
        assert cursor.fetchall() == ((3,),)


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
