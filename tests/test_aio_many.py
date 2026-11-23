"""``rustmy.aio``'s ``executemany``: the same groups as the blocking
cursors (batched multi-row INSERTs, pipelined windows for the rest), each
one awaited."""

from __future__ import annotations

import datetime as dt

from decimal import Decimal
from typing import TYPE_CHECKING
from typing import Any

import pytest

import rustmy

from rustmy import aio


if TYPE_CHECKING:
    from collections.abc import AsyncIterator
    from collections.abc import Iterator

    from tests.conftest import Server


async def connect(server: Server, **overrides: Any) -> aio.Connection:
    kwargs: dict[str, Any] = {
        "host": server.host,
        "port": server.port,
        "user": "rustmy",
        "password": "rustmy",
        "database": "rustmy_test",
    }
    kwargs.update(overrides)
    return await aio.connect(**kwargs)


@pytest.fixture
async def conn(server: Server) -> AsyncIterator[aio.Connection]:
    connection = await connect(server)
    yield connection
    await connection.ensure_closed()


@pytest.fixture(params=["text", "prepared"])
async def cursor(conn: aio.Connection, request: pytest.FixtureRequest) -> aio.Cursor:
    """Every test runs with a Cursor and with a PreparedCursor."""
    cur = conn.cursor(aio.PreparedCursor if request.param == "prepared" else aio.Cursor)
    await cur.execute(
        "CREATE TEMPORARY TABLE t ("
        "id INT AUTO_INCREMENT PRIMARY KEY, n INT UNIQUE, s VARCHAR(64), "
        "b VARBINARY(16), d DECIMAL(10, 2), ts DATETIME(6))"
    )
    return cur


INSERT = "INSERT INTO t (n, s) VALUES (%s, %s)"
UPDATE_D = "UPDATE t SET d = %s WHERE n = %s"


def _rows(n: int) -> list[tuple[int, str]]:
    return [(i, f"row {i}") for i in range(n)]


async def _insert_statements(cursor: aio.Cursor) -> int:
    status = cursor.connection.cursor()
    await status.execute("SHOW SESSION STATUS LIKE 'Com_insert'")
    (_, value) = await status.fetchone() or ("", "0")
    return int(value)


async def _updated(cursor: aio.Cursor) -> list[int]:
    check = cursor.connection.cursor()
    await check.execute("SELECT n FROM t WHERE d IS NOT NULL ORDER BY n")
    return [n for (n,) in await check.fetchall()]


async def test_inserts_are_batched_into_one_statement(cursor: aio.Cursor) -> None:
    before = await _insert_statements(cursor)
    count = await cursor.executemany(INSERT, _rows(1000))
    assert count == cursor.rowcount == 1000
    assert await _insert_statements(cursor) - before == 1
    assert cursor.lastrowid == 1
    await cursor.execute("SELECT COUNT(*), MIN(n), MAX(n) FROM t")
    assert await cursor.fetchone() == (1000, 0, 999)


async def test_batches_respect_max_stmt_length(cursor: aio.Cursor) -> None:
    cursor.max_stmt_length = 200
    before = await _insert_statements(cursor)
    await cursor.executemany(INSERT, _rows(100))
    lastrowid = cursor.lastrowid
    statements = await _insert_statements(cursor) - before
    assert 1 < statements < 100
    assert lastrowid is not None
    assert 1 < lastrowid <= 100
    cursor.max_stmt_length = 10
    before = await _insert_statements(cursor)
    await cursor.executemany(INSERT, [(i, "x") for i in range(100, 105)])
    assert await _insert_statements(cursor) - before == 5


async def test_values_roundtrip_through_batches(cursor: aio.Cursor) -> None:
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
    await cursor.executemany(
        "INSERT INTO t (n, s, b, d, ts) VALUES (%s, %s, %s, %s, %s)", rows
    )
    await cursor.execute("SELECT n, s, b, d, ts FROM t ORDER BY n")
    assert await cursor.fetchall() == tuple(rows)


async def test_named_parameters_and_generators(cursor: aio.Cursor) -> None:
    def gen() -> Iterator[dict[str, object]]:
        for i in range(3):
            yield {"n": i, "s": f"v{i}"}

    count = await cursor.executemany(
        "INSERT INTO t (n, s) VALUES (%(n)s, %(s)s)", gen()
    )
    assert count == 3
    await cursor.execute("SELECT s FROM t ORDER BY n")
    assert await cursor.fetchall() == (("v0",), ("v1",), ("v2",))


async def test_empty_sequence_runs_nothing(cursor: aio.Cursor) -> None:
    await cursor.executemany(INSERT, [(1, "a")])
    before = await _insert_statements(cursor)
    assert await cursor.executemany(INSERT, []) == 0
    assert await cursor.executemany("UPDATE t SET s = %s", []) == 0
    assert await _insert_statements(cursor) == before
    # Nothing to run leaves the previous statement in place.
    assert cursor._executed == "INSERT INTO t (n, s) VALUES (1, 'a')"  # noqa: SLF001


async def test_executed_shows_the_last_batch(cursor: aio.Cursor) -> None:
    cursor.max_stmt_length = 40
    await cursor.executemany(INSERT, [(1, "a"), (2, "b'c"), (3, "d")])
    assert cursor._executed == "INSERT INTO t (n, s) VALUES (3, 'd')"  # noqa: SLF001
    await cursor.executemany("DELETE FROM t WHERE n = %s", [(1,), (2,)])
    assert cursor._executed == "DELETE FROM t WHERE n = 2"  # noqa: SLF001


async def test_pipelined_updates_add_up(cursor: aio.Cursor) -> None:
    await cursor.executemany(INSERT, _rows(1000))
    count = await cursor.executemany(
        "UPDATE t SET s = %(s)s WHERE n = %(n)s",
        [{"n": i, "s": f"new {i}"} for i in range(1000)] + [{"n": -1, "s": "x"}],
    )
    assert count == cursor.rowcount == 1000
    assert cursor.description is None
    assert cursor.lastrowid == 0
    await cursor.execute("SELECT COUNT(*) FROM t WHERE s = CONCAT('new ', n)")
    assert await cursor.fetchone() == (1000,)


@pytest.mark.parametrize(("max_pipeline", "updated"), [(4, [0, 2, 3]), (1, [0])])
async def test_error_stops_at_the_end_of_its_window(
    cursor: aio.Cursor, max_pipeline: int, updated: list[int]
) -> None:
    await cursor.executemany(INSERT, _rows(10))
    cursor.max_pipeline = max_pipeline
    values = [(Decimal(1), i) for i in range(10)]
    values[1] = (Decimal(10) ** 12, 1)  # out of range for DECIMAL(10, 2)
    with pytest.raises(rustmy.DataError):
        await cursor.executemany(UPDATE_D, values)
    assert await _updated(cursor) == updated
    await cursor.connection.rollback()
    assert await _updated(cursor) == []


async def test_error_in_the_last_window(cursor: aio.Cursor) -> None:
    await cursor.executemany(INSERT, _rows(3))
    values = [(Decimal(1), 0), (Decimal(1), 1), (Decimal(10) ** 12, 2)]
    with pytest.raises(rustmy.DataError):
        await cursor.executemany(UPDATE_D, values)
    assert cursor.description is None
    assert await _updated(cursor) == [0, 1]
    await cursor.execute("SELECT 1")
    assert await cursor.fetchall() == ((1,),)


async def test_iteration_error_runs_the_items_before(cursor: aio.Cursor) -> None:
    await cursor.executemany(INSERT, _rows(10))

    def values() -> Iterator[tuple[Decimal, int]]:
        yield (Decimal(1), 0)
        yield (Decimal(1), 1)
        msg = "no more"
        raise LookupError(msg)

    with pytest.raises(LookupError, match="no more"):
        await cursor.executemany(UPDATE_D, values())
    assert await _updated(cursor) == [0, 1]


async def test_interpolation_error_runs_the_items_before(cursor: aio.Cursor) -> None:
    await cursor.executemany(INSERT, _rows(10))
    values: list[tuple[object, ...]] = [(Decimal(1), 0), (Decimal(1),), (1, 2)]
    with pytest.raises(rustmy.ProgrammingError):
        await cursor.executemany(UPDATE_D, values)
    assert await _updated(cursor) == [0]


async def test_a_batched_insert_error_runs_no_more(cursor: aio.Cursor) -> None:
    cursor.max_stmt_length = 40

    def rows() -> Iterator[tuple[int, str]]:
        yield from _rows(3)
        msg = "no more"
        raise LookupError(msg)

    with pytest.raises(LookupError, match="no more"):
        await cursor.executemany(INSERT, rows())
    # The statements already full ran; the one being built did not.
    await cursor.execute("SELECT COUNT(*) FROM t")
    (count,) = await cursor.fetchone() or (None,)
    assert count is not None
    assert count < 3


@pytest.mark.parametrize(
    "cursorclass", [aio.Cursor, aio.DictCursor, aio.SSCursor, aio.PreparedCursor]
)
async def test_cursor_keeps_the_last_result(
    conn: aio.Connection, cursorclass: type[aio.Cursor]
) -> None:
    cursor = conn.cursor(cursorclass)
    cursor.max_pipeline = 2
    await cursor.executemany("SELECT %s AS v", [(i,) for i in range(5)])
    expected: object = {"v": 4} if cursorclass is aio.DictCursor else (4,)
    assert await cursor.fetchall() == (expected,)
    assert cursor.description is not None
    assert cursor.rowcount == (-1 if cursorclass is aio.SSCursor else 5)
    assert cursor._executed == "SELECT 4 AS v"  # noqa: SLF001
    await cursor.execute("SELECT 'next' AS v")
    assert await cursor.fetchone() == (
        {"v": "next"} if cursorclass is aio.DictCursor else ("next",)
    )


async def test_streamed_cursor(conn: aio.Connection) -> None:
    cursor = conn.cursor(aio.SSCursor)
    await cursor.execute("CREATE TEMPORARY TABLE t (n INT, s VARCHAR(10))")
    assert await cursor.executemany(INSERT, _rows(10)) == 10
    count = await cursor.executemany(
        "UPDATE t SET s = %s WHERE n = %s", [("x", i) for i in range(10)]
    )
    assert count == cursor.rowcount == 10
    # An open stream is closed first, without a warning.
    await cursor.execute("SELECT n FROM t")
    assert await cursor.fetchone() == (0,)
    await cursor.executemany("SELECT n FROM t WHERE n > %s", [(7,), (8,)])
    assert await cursor.fetchall() == ((9,),)


async def test_multi_statement_template(server: Server) -> None:
    async with (
        await connect(server, multi_statements=True) as multi,
        multi.cursor() as cursor,
    ):
        cursor.max_pipeline = 2
        await cursor.execute("CREATE TEMPORARY TABLE m (n INT)")
        count = await cursor.executemany(
            "INSERT INTO m SELECT %s; SELECT COUNT(*) FROM m", [(1,), (2,), (3,)]
        )
        assert count == 3
        assert await cursor.nextset()
        assert await cursor.fetchall() == ((3,),)
        assert not await cursor.nextset()


async def test_statements_beyond_the_byte_budget(cursor: aio.Cursor) -> None:
    values = [("x" * 10_000,), ("y" * 40_000,), ("z" * 10,)]
    await cursor.executemany("SELECT LENGTH(%s)", values)
    assert await cursor.fetchall() == ((10,),)
    assert cursor.rowcount == 3


async def test_prepared_statement_without_placeholders(conn: aio.Connection) -> None:
    cursor = conn.cursor(aio.PreparedCursor)
    await cursor.executemany("SELECT 1", [(), ()])
    assert await cursor.fetchall() == ((1,),)


async def test_parameters_may_use_the_connection(conn: aio.Connection) -> None:
    """A generator of parameters runs between the groups' commands, so it
    may use the connection's own methods."""
    cursor = conn.cursor()
    cursor.max_pipeline = 2
    await cursor.execute("CREATE TEMPORARY TABLE t (n INT)")

    def values() -> Iterator[tuple[str]]:
        for i in range(5):
            yield (conn.literal(i),)

    await cursor.executemany("INSERT INTO t SELECT %s", values())
    await cursor.execute("SELECT COUNT(*) FROM t")
    assert await cursor.fetchone() == (5,)


async def test_another_command_while_a_batch_runs(conn: aio.Connection) -> None:
    cursor = conn.cursor()
    await cursor.execute("CREATE TEMPORARY TABLE t (n INT)")
    many = conn._core.executemany(  # noqa: SLF001
        "INSERT INTO t SELECT %s", [(1,), (2,)], 1024, 1
    )
    step = many.step()
    assert step is not None
    with pytest.raises(rustmy.ProgrammingError, match="await it first"):
        many.step()
    await step
    while (step := many.step()) is not None:
        await step
    assert many.rowcount == 1
    assert many.statement == b"INSERT INTO t SELECT 2"
