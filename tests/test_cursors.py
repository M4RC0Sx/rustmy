"""DictCursor, SSCursor/SSDictCursor, scroll() and callproc()."""

from __future__ import annotations

import _thread
import contextlib
import threading
import time
import uuid
import warnings

from typing import TYPE_CHECKING
from typing import Any

import pytest

import rustmy

from rustmy.cursors import Cursor
from rustmy.cursors import DictCursor
from rustmy.cursors import SSCursor
from rustmy.cursors import SSDictCursor


if TYPE_CHECKING:
    from collections.abc import Iterator

    from tests.conftest import Server


SEQ = "SELECT 1 AS n UNION ALL SELECT 2 UNION ALL SELECT 3 UNION ALL SELECT 4"
_DIGITS = " UNION ALL ".join(f"SELECT {d} AS d" for d in range(10))
_NUMBER = "t0.d + t1.d * 10 + t2.d * 100 + t3.d * 1000"
#: 10,000 rows, `n` from 0 to 9999 in order.
MANY = (
    f"SELECT {_NUMBER} AS n, CONCAT('row-', {_NUMBER}) AS s "
    f"FROM ({_DIGITS}) t0, ({_DIGITS}) t1, ({_DIGITS}) t2, ({_DIGITS}) t3 "
    "ORDER BY n"
)
MULTI_STATEMENTS = 1 << 16


class TinyBatchCursor(SSCursor):
    """Reads one row per batch, so every fetch crosses a batch boundary."""

    _batch_bytes = 1


class TinyBatchDictCursor(SSDictCursor):
    _batch_bytes = 1


@pytest.fixture(params=[SSCursor, TinyBatchCursor])
def ss(request: pytest.FixtureRequest, conn: rustmy.Connection) -> SSCursor:
    cursor_class: type[SSCursor] = request.param
    return conn.cursor(cursor_class)


# DictCursor --------------------------------------------------------------


@pytest.mark.parametrize(
    "cursor_class", [DictCursor, SSDictCursor, TinyBatchDictCursor]
)
def test_dict_rows(conn: rustmy.Connection, cursor_class: type[Cursor]) -> None:
    cursor: Any = conn.cursor(cursor_class)
    cursor.execute("SELECT 1 AS a, 'x' AS b, NULL AS c UNION ALL SELECT 2, 'y', 3")
    assert cursor.description is not None
    assert [d[0] for d in cursor.description] == ["a", "b", "c"]
    assert cursor.fetchone() == {"a": 1, "b": "x", "c": None}
    assert cursor.fetchall() == [{"a": 2, "b": "y", "c": 3}]
    assert cursor.fetchone() is None


def test_dict_cursor_from_connect(server: Server) -> None:
    with server.connect(cursorclass=DictCursor) as conn:
        cursor = conn.cursor()
        assert isinstance(cursor, DictCursor)
        cursor.execute(SEQ)
        assert list(cursor) == [{"n": 1}, {"n": 2}, {"n": 3}, {"n": 4}]
        assert type(conn.cursor(Cursor)) is Cursor


def test_dict_keys_for_repeated_names(cursor: Cursor) -> None:
    dict_cursor = cursor.connection.cursor(DictCursor)
    dict_cursor.execute(
        "SELECT a.id, b.id, a.v FROM (SELECT 1 AS id, 'a' AS v) a, (SELECT 2 AS id) b"
    )
    assert dict_cursor.fetchall() == [{"id": 1, "b.id": 2, "v": "a"}]

    # Without a table the name becomes ".x"; a third copy overwrites the
    # second, as in PyMySQL and mysqlclient.
    dict_cursor.execute("SELECT 1 AS x, 2 AS x, 3 AS x")
    assert dict_cursor.fetchall() == [{"x": 1, ".x": 3}]


def test_dict_cursor_nextset_and_dml(server: Server) -> None:
    with server.connect(client_flag=MULTI_STATEMENTS) as conn:
        cursor = conn.cursor(DictCursor)
        cursor.execute("SELECT 1 AS a; SELECT 2 AS b")
        assert cursor.fetchall() == [{"a": 1}]
        assert cursor.nextset() is True
        assert cursor.fetchall() == [{"b": 2}]
        cursor.execute("CREATE TEMPORARY TABLE t (id INT)")
        assert cursor.execute("INSERT INTO t VALUES (1), (2)") == 2


# Buffered scroll ---------------------------------------------------------


def test_scroll(cursor: Cursor) -> None:
    cursor.execute(SEQ)
    cursor.scroll(2)
    assert cursor.fetchone() == (3,)
    cursor.scroll(-3)
    assert cursor.rownumber == 0
    cursor.scroll(3, mode="absolute")
    assert cursor.fetchone() == (4,)
    assert cursor.fetchall() == []
    # fetchall() keeps the rows, so scrolling back still works.
    cursor.scroll(0, "absolute")
    assert cursor.fetchall() == [(1,), (2,), (3,), (4,)]
    cursor.scroll(0, "absolute")
    assert cursor.fetchmany(2) == [(1,), (2,)]


@pytest.mark.parametrize(
    ("value", "mode"),
    [(4, "absolute"), (-1, "absolute"), (5, "relative"), (-1, "relative")],
)
def test_scroll_out_of_range(cursor: Cursor, value: int, mode: str) -> None:
    cursor.execute(SEQ)
    with pytest.raises(IndexError):
        cursor.scroll(value, mode)
    assert cursor.rownumber == 0


def test_scroll_needs_a_result_and_a_known_mode(cursor: Cursor) -> None:
    with pytest.raises(rustmy.ProgrammingError, match="no result set"):
        cursor.scroll(0)
    cursor.execute(SEQ)
    with pytest.raises(rustmy.ProgrammingError, match="unknown scroll mode"):
        cursor.scroll(0, "sideways")


# callproc ----------------------------------------------------------------


@pytest.fixture
def procedure(conn: rustmy.Connection) -> Iterator[str]:
    """A procedure with IN, OUT and INOUT parameters and two result sets,
    named to need escaping.
    """
    name = f"p`%{uuid.uuid4().hex[:8]}"
    quoted = name.replace("`", "``")
    cursor = conn.cursor()
    cursor.execute(
        f"CREATE PROCEDURE `{quoted}`(IN a INT, OUT doubled INT, INOUT s VARCHAR(20))\n"
        "BEGIN\n"
        "  SELECT a AS a;\n"
        "  SELECT CONCAT(s, '!') AS s;\n"
        "  SET doubled = a * 2, s = UPPER(s);\n"
        "END"
    )
    yield name
    cursor.execute(f"DROP PROCEDURE IF EXISTS `{quoted}`")


@pytest.mark.parametrize("cursor_class", [Cursor, SSCursor])
def test_callproc(
    conn: rustmy.Connection, procedure: str, cursor_class: type[Cursor]
) -> None:
    cursor = conn.cursor(cursor_class)
    args = (21, None, "hi")
    assert cursor.callproc(procedure, args) is args
    quoted = procedure.replace("`", "``")
    assert cursor._executed == (  # noqa: SLF001
        f"CALL `{quoted}`(@`_{quoted}_0`,@`_{quoted}_1`,@`_{quoted}_2`)"
    )
    assert cursor.fetchall() == [(21,)]
    assert cursor.nextset() is True
    assert cursor.fetchall() == [("hi!",)]
    # The empty result of the CALL itself.
    assert cursor.nextset() is True
    assert cursor.description is None
    assert cursor.nextset() is None

    cursor.execute(f"SELECT @`_{quoted}_1`, @`_{quoted}_2`")
    assert cursor.fetchall() == [(42, "HI")]


def test_callproc_without_args(cursor: Cursor) -> None:
    name = f"noargs_{uuid.uuid4().hex[:8]}"
    cursor.execute(f"CREATE PROCEDURE {name}() SELECT 'ok'")
    try:
        assert cursor.callproc(name) == ()
        assert cursor._executed == f"CALL `{name}`()"  # noqa: SLF001
        assert cursor.fetchall() == [("ok",)]
    finally:
        cursor.execute(f"DROP PROCEDURE {name}")


# SSCursor ----------------------------------------------------------------


def test_streams_large_results(ss: SSCursor) -> None:
    assert ss.execute(MANY) == -1
    assert ss.rowcount == -1
    assert ss.description is not None
    assert [d[0] for d in ss.description] == ["n", "s"]
    assert ss.rownumber == 0
    count = 0
    for n, s in ss:
        assert (n, s) == (count, f"row-{count}")
        count += 1
    assert count == 10_000
    assert ss.rownumber == 10_000
    assert ss.fetchone() is None
    assert ss.fetchall() == []


def test_ss_fetch_methods(ss: SSCursor) -> None:
    ss.execute(SEQ)
    assert ss.fetchone() == (1,)
    assert ss.fetchmany() == [(2,)]
    ss.arraysize = 5
    assert ss.fetchmany() == [(3,), (4,)]
    assert ss.fetchmany() == []
    ss.execute(SEQ)
    assert ss.fetchmany(3) == [(1,), (2,), (3,)]
    assert ss.fetchall() == [(4,)]
    ss.execute(SEQ)
    assert list(ss.fetchall_unbuffered()) == [(1,), (2,), (3,), (4,)]


def test_ss_statement_without_result_set(ss: SSCursor) -> None:
    ss.execute("CREATE TEMPORARY TABLE t (id INT AUTO_INCREMENT PRIMARY KEY)")
    assert ss.execute("INSERT INTO t VALUES (), ()") == 2
    assert ss.lastrowid == 1
    assert ss.description is None
    assert ss.rownumber is None
    with pytest.raises(rustmy.ProgrammingError, match="no result set"):
        ss.fetchone()
    assert ss.executemany("INSERT INTO t VALUES (%s)", [(10,), (11,)]) == 2


def test_ss_scroll_forward(ss: SSCursor) -> None:
    ss.execute(MANY)
    assert ss.fetchone() == (0, "row-0")
    ss.scroll(10)
    assert ss.fetchone() == (11, "row-11")
    ss.scroll(5000, mode="absolute")
    assert ss.rownumber == 5000
    assert ss.fetchone() == (5000, "row-5000")
    ss.scroll(0)
    with pytest.raises(rustmy.NotSupportedError, match="backwards"):
        ss.scroll(-1)
    with pytest.raises(rustmy.NotSupportedError, match="backwards"):
        ss.scroll(0, mode="absolute")
    with pytest.raises(rustmy.ProgrammingError, match="unknown scroll mode"):
        ss.scroll(1, "sideways")
    # Past the end: IndexError, and the cursor is left at the end.
    with pytest.raises(IndexError):
        ss.scroll(10_000)
    assert ss.rownumber == 10_000
    assert ss.fetchone() is None


def test_ss_nextset(server: Server) -> None:
    with server.connect(client_flag=MULTI_STATEMENTS) as conn:
        ss = conn.cursor(TinyBatchCursor)
        ss.execute(f"{SEQ}; SELECT 'two'; SELECT 3")
        assert ss.fetchone() == (1,)
        # The rest of the first result is skipped without a warning.
        with warnings.catch_warnings():
            warnings.simplefilter("error")
            assert ss.nextset() is True
        assert ss.fetchall() == [("two",)]
        assert ss.nextset() is True
        assert ss.nextset() is None
        ss.execute("SELECT 4")
        assert ss.fetchall() == [(4,)]


def test_ss_dict_scroll_and_nextset(server: Server) -> None:
    with server.connect(client_flag=MULTI_STATEMENTS) as conn:
        ss = conn.cursor(SSDictCursor)
        ss.execute(f"{SEQ}; SELECT 1 AS x, 2 AS x")
        ss.scroll(2)
        assert ss.fetchone() == {"n": 3}
        assert ss.nextset() is True
        assert ss.fetchall() == [{"x": 1, ".x": 2}]


def test_ss_rows_before_a_server_error_are_delivered(ss: SSCursor) -> None:
    # The scalar subquery fails at run time, at the row with n = 5000, after
    # the rows before it were sent (the ordered derived table is
    # materialised, so they come first).
    ss.execute(
        "SELECT n, IF(n < 5000, 1, (SELECT 1 UNION ALL SELECT 2)) AS x "
        f"FROM ({MANY} LIMIT 10000) AS numbers"
    )
    rows: list[tuple[Any, ...]] = []
    with pytest.raises(rustmy.OperationalError) as excinfo:
        while (batch := ss.fetchmany(1000)) != []:
            rows.extend(batch)
    assert excinfo.value.args[0] == 1242
    assert [n for n, _ in rows] == list(range(5000))
    # The error ended the result set; the connection is fine.
    assert ss.fetchone() is None
    ss.execute("SELECT 1")
    assert ss.fetchall() == [(1,)]


# Streaming and the rest of the connection -------------------------------


def test_another_command_discards_unread_rows(
    conn: rustmy.Connection, ss: SSCursor
) -> None:
    ss.execute(MANY)
    assert ss.fetchone() == (0, "row-0")
    other = conn.cursor()
    with pytest.warns(UserWarning, match="left incomplete") as record:
        other.execute("SELECT 'other'")
    # The warning points at the caller, not at rustmy internals.
    assert record[0].filename == __file__
    assert other.fetchall() == [("other",)]
    for fetch in (ss.fetchone, ss.fetchall, lambda: ss.scroll(1)):
        with pytest.raises(rustmy.ProgrammingError, match="discarded"):
            fetch()
    # The cursor can run a new query.
    ss.execute(SEQ)
    assert ss.fetchall() == [(1,), (2,), (3,), (4,)]


def test_discard_does_not_depend_on_rows_already_read(conn: rustmy.Connection) -> None:
    # The whole result fits in the first batch, so nothing is left on the
    # wire, yet rows the user never fetched are lost just the same.
    ss = conn.cursor(SSCursor)
    ss.execute(SEQ)
    assert ss.fetchone() == (1,)
    with pytest.warns(UserWarning, match="left incomplete"):
        conn.ping()
    with pytest.raises(rustmy.ProgrammingError, match="discarded"):
        ss.fetchone()


@pytest.mark.parametrize(
    "command",
    [
        lambda conn: conn.commit(),
        lambda conn: conn.rollback(),
        lambda conn: conn.select_db("rustmy_test"),
        lambda conn: conn.cursor().executemany("SELECT %s", [(1,)]),
        lambda conn: conn.cursor().callproc("no_such_procedure"),
    ],
)
def test_every_command_discards_with_a_warning(
    conn: rustmy.Connection, command: Any
) -> None:
    ss = conn.cursor(SSCursor)
    ss.execute(MANY)
    with pytest.warns(UserWarning, match="left incomplete") as record:
        _run_ignoring_server_errors(command, conn)
    assert len(record) == 1
    assert record[0].filename == __file__
    with pytest.raises(rustmy.ProgrammingError, match="discarded"):
        ss.fetchone()


def _run_ignoring_server_errors(command: Any, conn: rustmy.Connection) -> None:
    with contextlib.suppress(rustmy.ProgrammingError):  # the missing procedure
        command(conn)


def test_finished_or_closed_streams_discard_nothing(conn: rustmy.Connection) -> None:
    ss = conn.cursor(SSCursor)
    other = conn.cursor()
    with warnings.catch_warnings():
        warnings.simplefilter("error")
        # Every row fetched (even without seeing the end): nothing to lose.
        ss.execute(SEQ)
        assert ss.fetchmany(4) == [(1,), (2,), (3,), (4,)]
        other.execute("SELECT 1")
        assert ss.fetchone() is None
        # Closing drops the rest quietly.
        ss = conn.cursor(SSCursor)
        ss.execute(MANY)
        ss.fetchone()
        ss.close()
        other.execute("SELECT 2")
        with conn.cursor(SSCursor) as ss2:
            ss2.execute(MANY)
        # Running another query on the same cursor too.
        ss = conn.cursor(SSCursor)
        ss.execute(MANY)
        ss.execute(SEQ)
        assert ss.fetchall() == [(1,), (2,), (3,), (4,)]
    assert other.execute("SELECT 3") == 1


def test_warning_as_error_leaves_the_stream_intact(conn: rustmy.Connection) -> None:
    ss = conn.cursor(SSCursor)
    ss.execute(MANY)
    assert ss.fetchone() == (0, "row-0")
    with warnings.catch_warnings():
        warnings.simplefilter("error")
        with pytest.raises(UserWarning):
            conn.cursor().execute("SELECT 1")
    assert ss.fetchone() == (1, "row-1")


def test_dropped_stream_is_discarded_by_the_next_command(
    conn: rustmy.Connection,
) -> None:
    ss = conn.cursor(SSCursor)
    ss.execute(MANY)
    del ss
    with pytest.warns(UserWarning, match="left incomplete"):
        assert conn.cursor().execute("SELECT 1") == 1


def test_closed_connection_mid_stream(server: Server) -> None:
    conn = server.connect()
    ss = conn.cursor(SSCursor)
    ss.execute(MANY)
    conn.close()
    with pytest.raises(rustmy.InterfaceError, match="closed"):
        ss.fetchone()


def test_closed_ss_cursor(ss: SSCursor) -> None:
    ss.execute(MANY)
    ss.close()
    with pytest.raises(rustmy.ProgrammingError, match="cursor closed"):
        ss.execute("SELECT 1")


def test_cursor_alias(conn: rustmy.Connection) -> None:
    assert type(conn.cursor(cursor=SSCursor)) is SSCursor
    with pytest.raises(TypeError, match="not both"):
        conn.cursor(Cursor, cursor=SSCursor)  # type: ignore[call-overload]


def test_concurrent_use_of_one_stream_is_refused_not_corrupted(
    conn: rustmy.Connection,
) -> None:
    # The GIL is released while a batch is read, so threads really overlap
    # (and run fully in parallel on free-threaded builds). Each call either
    # gets rows or is refused; no row is lost or handed out twice.
    ss = conn.cursor(SSCursor)
    ss.execute(MANY)
    seen: list[int] = []
    refused = 0
    lock = threading.Lock()

    def worker() -> None:
        nonlocal refused
        while True:
            try:
                batch = ss.fetchmany(7)
            except rustmy.ProgrammingError as exc:
                assert "in use by another thread" in str(exc)
                with lock:
                    refused += 1
                # Spinning would hold the GIL the reading thread needs back.
                time.sleep(0.0001)
                continue
            if not batch:
                return
            with lock:
                seen.extend(n for n, _ in batch)

    threads = [threading.Thread(target=worker) for _ in range(8)]
    for thread in threads:
        thread.start()
    for thread in threads:
        thread.join()
    assert sorted(seen) == list(range(10_000))
    ss.execute("SELECT 1")
    assert ss.fetchall() == [(1,)]


@pytest.mark.parametrize("cursor_class", [Cursor, DictCursor, SSCursor])
def test_failed_execute_leaves_no_stale_result(
    conn: rustmy.Connection, cursor_class: type[Cursor]
) -> None:
    cursor = conn.cursor(cursor_class)
    cursor.execute(SEQ)
    with pytest.raises(rustmy.ProgrammingError):
        cursor.execute("SELECT * FROM no_such_table")
    assert cursor.description is None
    assert cursor.rowcount == -1
    with pytest.raises(rustmy.ProgrammingError, match="no result set"):
        cursor.fetchall()


# Buffered reads in batches ------------------------------------------------

#: MANY with a pause at row 2000, after earlier rows were sent.
PAUSED = (
    "SELECT n, SLEEP(IF(n = 2000, {pause}, 0)) AS z "
    f"FROM ({MANY} LIMIT 10000) AS numbers"
)


class TinyBufferedCursor(Cursor):
    _batch_bytes = 1


class TinyBufferedDictCursor(DictCursor):
    _batch_bytes = 1


def test_buffered_result_read_in_many_batches(conn: rustmy.Connection) -> None:
    cursor = conn.cursor(TinyBufferedCursor)
    assert cursor.execute(MANY) == 10_000
    rows = cursor.fetchall()
    assert rows[0] == (0, "row-0")
    assert rows[-1] == (9999, "row-9999")
    dict_cursor = conn.cursor(TinyBufferedDictCursor)
    dict_cursor.execute(MANY)
    assert dict_cursor.fetchall()[1234] == {"n": 1234, "s": "row-1234"}


def test_buffered_server_error_mid_result(cursor: Cursor) -> None:
    with pytest.raises(rustmy.OperationalError) as excinfo:
        cursor.execute(
            "SELECT n, IF(n < 5000, 1, (SELECT 1 UNION ALL SELECT 2)) AS x "
            f"FROM ({MANY} LIMIT 10000) AS numbers"
        )
    assert excinfo.value.args[0] == 1242
    assert cursor.description is None
    assert cursor.execute("SELECT 1") == 1


def test_buffered_read_keeps_the_connection(conn: rustmy.Connection) -> None:
    # Another thread cannot run a command between two batches.
    errors: list[Exception] = []

    def intruder() -> None:
        time.sleep(0.1)
        try:
            conn.ping()
        except rustmy.ProgrammingError as exc:
            errors.append(exc)

    thread = threading.Thread(target=intruder)
    thread.start()
    cursor = conn.cursor()
    assert cursor.execute(PAUSED.format(pause=0.5)) == 10_000
    thread.join()
    assert len(errors) == 1
    assert "in use by another thread" in str(errors[0])


def test_ctrl_c_interrupts_a_long_buffered_read(conn: rustmy.Connection) -> None:
    # Pauses at rows 2000 (0.2 s) and 9000 (1 s); Ctrl+C arrives during the
    # first. Checked between batches, it stops the read right after that
    # pause instead of when the whole result has arrived.
    cursor = conn.cursor()
    timer = threading.Timer(0.1, _thread.interrupt_main)
    timer.start()
    start = time.perf_counter()
    with pytest.raises(KeyboardInterrupt):
        cursor.execute(
            "SELECT n, SLEEP(CASE n WHEN 2000 THEN 0.2 WHEN 9000 THEN 1 ELSE 0 END) "
            f"FROM ({MANY} LIMIT 10000) AS numbers"
        )
    elapsed = time.perf_counter() - start
    timer.join()
    assert elapsed < 0.8
    assert cursor.description is None
    # The next command discards the rest quietly.
    with warnings.catch_warnings():
        warnings.simplefilter("error")
        assert cursor.execute("SELECT 1") == 1
    assert cursor.fetchall() == [(1,)]
