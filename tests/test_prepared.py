"""Prepared statements (binary protocol) against the text protocol.

A :class:`rustmy.cursors.PreparedCursor` must return exactly the objects a
:class:`rustmy.Cursor` returns for every column type, and a value written
through it as a typed parameter must be stored as the same value as through
interpolation. Both are checked with hypothesis on each server.
"""

from __future__ import annotations

import datetime as dt

from decimal import Decimal
from typing import TYPE_CHECKING
from typing import Any

import pytest

from hypothesis import given
from hypothesis import strategies as st

import rustmy

from rustmy.constants import FIELD_TYPE
from rustmy.cursors import DictCursor
from rustmy.cursors import PreparedCursor
from rustmy.cursors import PreparedDictCursor
from rustmy.cursors import SSCursor


if TYPE_CHECKING:
    from collections.abc import Callable
    from collections.abc import Iterator

    from tests.conftest import Server


def _ints(bits: int, *, unsigned: bool = False) -> st.SearchStrategy[int]:
    if unsigned:
        return st.integers(0, 2**bits - 1)
    return st.integers(-(2 ** (bits - 1)), 2 ** (bits - 1) - 1)


_dates = st.dates(dt.date(1000, 1, 1), dt.date(9999, 12, 31))
_datetimes = st.datetimes(dt.datetime(1000, 1, 1), dt.datetime(9999, 12, 31))
_times = st.timedeltas(
    -dt.timedelta(hours=838, minutes=59, seconds=59),
    dt.timedelta(hours=838, minutes=59, seconds=59),
)

#: Column definition -> values to store in it (``None`` included).
COLUMNS: dict[str, st.SearchStrategy[Any]] = {
    "TINYINT": _ints(8),
    "TINYINT UNSIGNED": _ints(8, unsigned=True),
    "SMALLINT": _ints(16),
    "SMALLINT UNSIGNED": _ints(16, unsigned=True),
    "MEDIUMINT": _ints(24),
    "INT": _ints(32),
    "INT UNSIGNED": _ints(32, unsigned=True),
    "BIGINT": _ints(64),
    "BIGINT UNSIGNED": _ints(64, unsigned=True),
    "YEAR": st.integers(1901, 2155),
    "FLOAT": st.floats(width=32, allow_nan=False, allow_infinity=False),
    "FLOAT(9, 3)": st.floats(-999_999, 999_999),
    "DOUBLE": st.floats(allow_nan=False, allow_infinity=False),
    "DOUBLE(16, 4)": st.floats(-1e11, 1e11),
    "DECIMAL(30, 8)": st.decimals(
        min_value=Decimal("-1e21"), max_value=Decimal("1e21"), places=8
    ),
    "DATE": _dates,
    "DATETIME": _datetimes,
    "DATETIME(3)": _datetimes,
    "DATETIME(6)": _datetimes,
    "TIME": _times,
    "TIME(6)": _times,
    "VARCHAR(64)": st.text(st.characters(exclude_categories=["Cs"]), max_size=64),
    "VARBINARY(64)": st.binary(max_size=64),
    "BLOB": st.binary(max_size=300),
    "BIT(12)": st.integers(0, 4095),
    "ENUM('a', 'b', 'ñ')": st.sampled_from(["a", "b", "ñ"]),
    "SET('x', 'y')": st.sampled_from(["", "x", "y", "x,y"]),
    "JSON": st.sampled_from(['{"a": 1}', "[1, 2.5, null]", '"ñ"', "true"]),
}


@pytest.fixture(scope="module")
def tables(server: Server) -> Iterator[tuple[rustmy.Connection, dict[str, str]]]:
    """One connection, autocommit, and a temporary table per column type."""
    with server.connect(autocommit=True) as conn:
        cursor = conn.cursor()
        names = {}
        for i, definition in enumerate(COLUMNS):
            name = f"ps_{i}"
            cursor.execute(
                f"CREATE TEMPORARY TABLE {name} (id INT PRIMARY KEY, v {definition})"
            )
            names[definition] = name
        yield conn, names


def _rows(cursor: rustmy.Cursor, table: str) -> object:
    cursor.execute(f"SELECT v, id FROM {table} ORDER BY id")
    return cursor.fetchall()


@pytest.mark.parametrize("definition", list(COLUMNS))
@given(data=st.data())
def test_rows_match_the_text_protocol(
    tables: tuple[rustmy.Connection, dict[str, str]], definition: str, data: Any
) -> None:
    conn, names = tables
    table = names[definition]
    value = data.draw(COLUMNS[definition] | st.none())
    cursor = conn.cursor()
    cursor.execute(f"DELETE FROM {table}")
    cursor.execute(f"INSERT INTO {table} VALUES (1, %s)", (value,))
    text = _rows(cursor, table)
    assert _rows(conn.cursor(PreparedCursor), table) == text
    as_dicts = conn.cursor(PreparedDictCursor)
    as_dicts.execute(f"SELECT v FROM {table} WHERE id = %s", (1,))
    plain_dicts = conn.cursor(DictCursor)
    plain_dicts.execute(f"SELECT v FROM {table} WHERE id = 1")
    assert as_dicts.fetchall() == plain_dicts.fetchall()


@pytest.mark.parametrize("definition", list(COLUMNS))
@given(data=st.data())
def test_typed_parameters_store_what_interpolation_stores(
    server: Server,
    tables: tuple[rustmy.Connection, dict[str, str]],
    definition: str,
    data: Any,
) -> None:
    _store_both_ways(
        server, tables, definition, data.draw(COLUMNS[definition] | st.none())
    )


def _rejected_by_server(server: Server, value: object) -> bool:
    """MySQL 9+ rejects a binary TIME parameter in hour 838 with a fraction
    (1690, found by hypothesis), although the value is in range; 8.x and
    MariaDB store it. Sending it as a string instead is no way out: MySQL
    misreads a string parameter in ``CAST(? AS TIME)`` (``'10:00:00'`` gives
    00:00:00, also with SQL-level ``PREPARE``), so the error stays."""
    return (
        server.name == "mysql"
        and server.major_version >= 9
        and isinstance(value, dt.timedelta)
        and value.microseconds != 0
        and abs(value) >= dt.timedelta(hours=838)
    )


def _store_both_ways(
    server: Server,
    tables: tuple[rustmy.Connection, dict[str, str]],
    definition: str,
    value: object,
) -> None:
    conn, names = tables
    table = names[definition]
    cursor = conn.cursor()
    cursor.execute(f"DELETE FROM {table}")
    cursor.execute(f"INSERT INTO {table} VALUES (1, %s)", (value,))
    insert = f"INSERT INTO {table} VALUES (2, %s)"
    if _rejected_by_server(server, value):
        with pytest.raises(rustmy.OperationalError) as info:
            conn.cursor(PreparedCursor).execute(insert, (value,))
        assert info.value.args[0] == 1690
        return
    conn.cursor(PreparedCursor).execute(insert, (value,))
    cursor.execute(f"SELECT v FROM {table} ORDER BY id")
    interpolated, typed = cursor.fetchall()
    assert typed == interpolated


@pytest.mark.parametrize("definition", ["TIME", "TIME(6)"])
@pytest.mark.parametrize(
    "value",
    [
        dt.timedelta(days=-35, seconds=3601, microseconds=1),
        dt.timedelta(hours=838, microseconds=1),
        dt.timedelta(hours=838, minutes=59, seconds=58, microseconds=999999),
        -dt.timedelta(hours=838, minutes=30, microseconds=500000),
        dt.timedelta(hours=838, minutes=59, seconds=59),
        dt.timedelta(hours=837, minutes=59, seconds=59, microseconds=999999),
    ],
)
def test_time_parameters_in_hour_838(
    server: Server,
    tables: tuple[rustmy.Connection, dict[str, str]],
    definition: str,
    value: dt.timedelta,
) -> None:
    _store_both_ways(server, tables, definition, value)


def test_parameters_keep_their_type(conn: rustmy.Connection) -> None:
    """Intended difference: a parameter echoed back by ``SELECT %s`` is a
    string literal when interpolated, but a typed value when prepared."""
    value = dt.datetime(2026, 11, 17, 1, 2, 3)
    plain = conn.cursor()
    plain.execute("SELECT %s", (value,))
    assert plain.fetchone() == ("2026-11-17 01:02:03",)
    prepared = conn.cursor(prepared=True)
    prepared.execute("SELECT %s", (value,))
    assert prepared.fetchone() == (value,)


def _status(conn: rustmy.Connection, name: str) -> int:
    cursor = conn.cursor()
    cursor.execute("SHOW SESSION STATUS LIKE %s", (name,))
    row = cursor.fetchone()
    assert row is not None
    return int(row[1])


@pytest.fixture
def statement_room(server: Server) -> Iterator[Callable[[int], None]]:
    """Sets the server's ``max_prepared_stmt_count`` to the statements
    prepared now plus the given room; restored afterwards."""
    with server.connect(user="root", database=None, autocommit=True) as root:
        cursor = root.cursor()
        cursor.execute("SELECT @@GLOBAL.max_prepared_stmt_count")
        (before,) = cursor.fetchone() or (None,)

        def leave(room: int) -> None:
            cursor.execute("SHOW GLOBAL STATUS LIKE 'Prepared_stmt_count'")
            (_, prepared) = cursor.fetchone() or ("", 0)
            cursor.execute(
                "SET GLOBAL max_prepared_stmt_count = %s", (int(prepared) + room,)
            )

        try:
            yield leave
        finally:
            cursor.execute("SET GLOBAL max_prepared_stmt_count = %s", (before,))


def test_a_full_server_frees_a_cached_statement(
    server: Server, statement_room: Callable[[int], None]
) -> None:
    with server.connect() as conn:
        cursor = conn.cursor(prepared=True)
        cursor.execute("SELECT %s", (1,))
        cursor.execute("SELECT %s, %s", (1, 2))
        statement_room(0)
        # Error 1461 on preparing: the least recently used one is closed
        # and the statement prepared again.
        closes = _status(conn, "Com_stmt_close")
        cursor.execute("SELECT %s, %s, %s", (1, 2, 3))
        assert cursor.fetchall() == ((1, 2, 3),)
        assert _status(conn, "Com_stmt_close") - closes == 1
        cursor.execute("SELECT %s, %s", (4, 5))  # still cached
        assert cursor.fetchall() == ((4, 5),)
        assert _status(conn, "Com_stmt_close") - closes == 1


def test_a_full_server_with_nothing_cached_raises(
    server: Server, statement_room: Callable[[int], None]
) -> None:
    with server.connect() as conn:
        statement_room(0)
        with pytest.raises(rustmy.DatabaseError) as info:
            conn.cursor(prepared=True).execute("SELECT %s", (1,))
        assert info.value.args[0] == 1461
        # The connection carries on.
        statement_room(1)
        cursor = conn.cursor(prepared=True)
        cursor.execute("SELECT %s", (1,))
        assert cursor.fetchall() == ((1,),)


def test_statements_are_prepared_once_and_evicted(server: Server) -> None:
    with server.connect(statement_cache_size=2) as conn:
        cursor = conn.cursor(prepared=True)
        start = _status(conn, "Com_stmt_prepare")
        cursor.execute("SELECT %s", (1,))
        # MySQL counts each COM_STMT_PREPARE twice, MariaDB once.
        unit = _status(conn, "Com_stmt_prepare") - start
        assert unit in {1, 2}

        def prepares() -> int:
            return (_status(conn, "Com_stmt_prepare") - start) // unit

        for _ in range(3):
            cursor.execute("SELECT %s", (1,))
            cursor.execute("SELECT %s, %s", (1, 2))
        assert prepares() == 2
        closes = _status(conn, "Com_stmt_close")
        cursor.execute("SELECT %s, %s, %s", (1, 2, 3))  # evicts "SELECT ?"
        assert _status(conn, "Com_stmt_close") - closes == 1
        cursor.execute("SELECT %s, %s", (1, 2))  # still cached
        assert prepares() == 3
        cursor.execute("SELECT %s", (1,))
        assert prepares() == 4
        # A reset closes them on the server (and zeroes the session's
        # counters): they are prepared again.
        conn.reset_connection()
        start = _status(conn, "Com_stmt_prepare")
        cursor.execute("SELECT %s", (1,))
        assert cursor.fetchall() == ((1,),)
        assert prepares() == 1


def test_named_placeholders_and_percent(conn: rustmy.Connection) -> None:
    cursor = conn.cursor(prepared=True)
    cursor.execute("SELECT %(x)s + %(y)s, %(x)s, '100%%'", {"x": 2, "y": 3})
    assert cursor.fetchall() == ((5, 2, "100%"),)
    # Without parameters the query is sent as written.
    cursor.execute("SELECT '100%', 'what?'")
    assert cursor.fetchall() == (("100%", "what?"),)


def test_only_statements_with_placeholders_are_prepared(server: Server) -> None:
    """DDL and introspection go as text: the server cannot prepare some of
    them (CREATE PROCEDURE, error 1295) and answers others differently."""
    with server.connect() as conn:
        cursor = conn.cursor(prepared=True)
        start = _status(conn, "Com_stmt_prepare")
        cursor.execute("DROP PROCEDURE IF EXISTS ps_text_only")
        cursor.execute("CREATE PROCEDURE ps_text_only() SELECT 1")
        cursor.execute("SELECT '100%%'", ())  # interpolated: %% is %
        assert cursor.fetchall() == (("100%",),)
        with pytest.raises(rustmy.ProgrammingError, match="not all parameters"):
            cursor.execute("SELECT 1", (2,))
        assert _status(conn, "Com_stmt_prepare") == start
        # With a placeholder, a statement the server cannot prepare fails
        # (MariaDB can prepare CREATE PROCEDURE; MySQL cannot).
        cursor.execute("DROP PROCEDURE IF EXISTS ps_param")
        if server.name == "mysql":
            with pytest.raises(rustmy.OperationalError) as info:
                cursor.execute("CREATE PROCEDURE ps_param() SELECT %s", (1,))
            assert info.value.args[0] == 1295
        else:
            cursor.execute("CREATE PROCEDURE ps_param() SELECT %s", (1,))
            cursor.execute("DROP PROCEDURE ps_param")
        cursor.execute("DROP PROCEDURE ps_text_only")


def test_errors(conn: rustmy.Connection) -> None:
    cursor = conn.cursor(prepared=True)
    with pytest.raises(rustmy.ProgrammingError, match="not all parameters"):
        cursor.execute("SELECT %s", (1, 2))
    # A `?` of its own is a placeholder to the server.
    with pytest.raises(rustmy.ProgrammingError, match="2 placeholders but 1"):
        cursor.execute("SELECT ?, %s", (1,))
    with pytest.raises(rustmy.ProgrammingError) as info:
        cursor.execute("SELEC %s", (1,))
    assert info.value.args[0] == 1064
    with pytest.raises(rustmy.NotSupportedError, match="sequence"):
        cursor.execute("SELECT %s IN %s", (1, (1, 2)))
    with pytest.raises(rustmy.NotSupportedError, match="prepared-statement version"):
        conn.cursor(SSCursor, prepared=True)
    # The connection is still usable.
    cursor.execute("SELECT %s", ("ok",))
    assert cursor.fetchall() == (("ok",),)


def test_custom_encoders_need_the_text_protocol(server: Server) -> None:
    conv = dict(rustmy.converters.conversions)
    conv[Decimal] = lambda value, _conv: str(value)
    with server.connect(conv=conv) as conn:
        cursor = conn.cursor(prepared=True)
        with pytest.raises(rustmy.NotSupportedError, match="conv= encoder"):
            cursor.execute("SELECT %s", (Decimal(1),))
        cursor.execute("SELECT %s", (1,))
        assert cursor.fetchall() == ((1,),)


def test_custom_decoders_get_the_text_form(server: Server) -> None:
    """conv= decoders get the same str/bytes as with the text protocol."""
    seen: dict[str, list[object]] = {"text": [], "prepared": []}
    sink: list[object] = []

    def record(value: object) -> object:
        sink.append(value)
        return value

    conv = dict(rustmy.converters.conversions)
    for field in (
        FIELD_TYPE.LONG,
        FIELD_TYPE.LONGLONG,
        FIELD_TYPE.DOUBLE,
        FIELD_TYPE.FLOAT,
        FIELD_TYPE.NEWDECIMAL,
        FIELD_TYPE.DATE,
        FIELD_TYPE.DATETIME,
        FIELD_TYPE.TIME,
    ):
        conv[field] = record
    query = (
        "SELECT CAST(-7 AS SIGNED), 12, CAST(2.5 AS DOUBLE), CAST(1.1 AS FLOAT), "
        "CAST(1.5 AS DECIMAL(6, 2)), DATE('2026-11-17'), "
        "CAST('2026-11-17 01:02:03.45' AS DATETIME(2)), CAST('-01:02:03' AS TIME), "
        "CAST('12:00:00.5' AS TIME(1)), %s"
    )
    with server.connect(conv=conv) as conn:
        for kind, cursor in (
            ("text", conn.cursor()),
            ("prepared", conn.cursor(prepared=True)),
        ):
            sink.clear()
            cursor.execute(query, (1,))
            seen[kind] = list(sink)
    assert seen["prepared"] == seen["text"]


def test_multiple_results_and_executemany(conn: rustmy.Connection) -> None:
    cursor = conn.cursor(prepared=True)
    cursor.execute("CREATE TEMPORARY TABLE ps_many (n INT, s VARCHAR(8))")
    count = cursor.executemany(
        "INSERT INTO ps_many VALUES (%s, %s)", [(1, "a"), (2, "b"), (3, None)]
    )
    assert count == 3
    cursor.execute("SELECT n, s FROM ps_many WHERE n > %s ORDER BY n", (1,))
    assert cursor.fetchall() == ((2, "b"), (3, None))
    plain = conn.cursor()
    plain.execute("DROP PROCEDURE IF EXISTS ps_two_results")
    plain.execute(
        "CREATE PROCEDURE ps_two_results(n INT) "
        "BEGIN SELECT n; SELECT n + 1, NOW() > '2000-01-01'; END"
    )
    cursor.execute("CALL ps_two_results(%s)", (41,))
    assert cursor.fetchall() == ((41,),)
    assert cursor.nextset()
    assert cursor.fetchall() == ((42, 1),)
    assert cursor.nextset()
    assert cursor.description is None
    assert not cursor.nextset()
    plain.execute("DROP PROCEDURE ps_two_results")


def test_connect_cursorclass(server: Server) -> None:
    with server.connect(cursorclass=PreparedDictCursor) as conn:
        cursor = conn.cursor()
        assert isinstance(cursor, PreparedDictCursor)
        cursor.execute("SELECT %s AS v", (Decimal("1.50"),))
        assert cursor.fetchall() == ({"v": Decimal("1.50")},)


# executemany: pipelined executions of one prepared statement.


def _filled_table(cursor: rustmy.Cursor, rows: int) -> None:
    cursor.execute(
        "CREATE TEMPORARY TABLE ps_batch (n INT PRIMARY KEY, d DECIMAL(10, 2))"
    )
    cursor.executemany(
        "INSERT INTO ps_batch VALUES (%s, NULL)", [(i,) for i in range(rows)]
    )


def _updated(conn: rustmy.Connection) -> list[int]:
    check = conn.cursor()
    check.execute("SELECT n FROM ps_batch WHERE d IS NOT NULL ORDER BY n")
    return [n for (n,) in check.fetchall()]


def test_executemany_adds_up_and_keeps_the_last_result(
    conn: rustmy.Connection,
) -> None:
    cursor = conn.cursor(prepared=True)
    _filled_table(cursor, 600)
    count = cursor.executemany(
        "UPDATE ps_batch SET d = %s WHERE n = %s",
        [(Decimal(1), i) for i in range(600)] + [(Decimal(1), -1)],
    )
    assert count == cursor.rowcount == 600
    assert _updated(conn) == list(range(600))
    assert cursor._executed == "UPDATE ps_batch SET d = 1 WHERE n = -1"  # noqa: SLF001
    cursor.max_pipeline = 2
    cursor.executemany("SELECT n FROM ps_batch WHERE n = %s", [(i,) for i in range(5)])
    assert cursor.fetchall() == ((4,),)
    assert cursor.rowcount == 5
    assert cursor.executemany("SELECT %s", []) == 0


@pytest.mark.parametrize(("max_pipeline", "updated"), [(4, [0, 2, 3]), (1, [0])])
def test_executemany_error_stops_at_the_end_of_its_window(
    conn: rustmy.Connection, max_pipeline: int, updated: list[int]
) -> None:
    cursor = conn.cursor(prepared=True)
    _filled_table(cursor, 10)
    cursor.max_pipeline = max_pipeline
    values = [(Decimal(1), i) for i in range(10)]
    values[1] = (Decimal(10) ** 12, 1)  # out of range for DECIMAL(10, 2)
    with pytest.raises(rustmy.DataError):
        cursor.executemany("UPDATE ps_batch SET d = %s WHERE n = %s", values)
    assert _updated(conn) == updated
    conn.rollback()
    assert _updated(conn) == []


def test_executemany_survives_eviction_between_windows(server: Server) -> None:
    """A generator preparing other statements between windows evicts the
    batch's statement (cache of one): the next window prepares it again."""
    with server.connect(statement_cache_size=1) as conn:
        cursor = conn.cursor(prepared=True)
        _filled_table(cursor, 6)
        other = conn.cursor(prepared=True)

        def values() -> Iterator[tuple[Decimal, int]]:
            for n in range(6):
                other.execute("SELECT %s", (n,))
                yield (Decimal(n), n)

        cursor.max_pipeline = 2
        update = "UPDATE ps_batch SET d = %s WHERE n = %s"
        assert cursor.executemany(update, values()) == 6
        assert _updated(conn) == list(range(6))


def test_executemany_bind_error_runs_the_items_before(
    conn: rustmy.Connection,
) -> None:
    cursor = conn.cursor(prepared=True)
    _filled_table(cursor, 5)
    values: list[tuple[object, ...]] = [(Decimal(1), 0), (Decimal(1), 1), (object(), 2)]
    with pytest.raises(rustmy.ProgrammingError, match="unsupported parameter type"):
        cursor.executemany("UPDATE ps_batch SET d = %s WHERE n = %s", values)
    assert _updated(conn) == [0, 1]
