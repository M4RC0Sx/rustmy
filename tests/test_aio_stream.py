"""``rustmy.aio``'s unbuffered cursors: the rows stay on the server until
they are fetched, read off the event loop one batch at a time; the same
rules as the blocking :class:`rustmy.cursors.SSCursor`."""

from __future__ import annotations

import warnings

from typing import TYPE_CHECKING
from typing import Any

import pytest

import rustmy

from rustmy import aio
from rustmy.constants import FIELD_TYPE
from rustmy.converters import conversions
from tests.test_cursors import MANY
from tests.test_cursors import SEQ


if TYPE_CHECKING:
    from collections.abc import AsyncIterator

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


@pytest.fixture
def ss(conn: aio.Connection) -> aio.SSCursor:
    return conn.cursor(aio.SSCursor)


async def test_streams_large_results(ss: aio.SSCursor) -> None:
    assert await ss.execute(MANY) == -1
    assert ss.rowcount == -1
    assert [d[0] for d in ss.description] == ["n", "s"]
    assert ss.rownumber == 0
    # Nothing is read before it is asked for.
    stream = ss._stream  # noqa: SLF001
    assert stream is not None
    assert stream.take(1) is None
    count = 0
    async for n, s in ss:
        assert (n, s) == (count, f"row-{count}")
        count += 1
    assert count == 10_000
    assert ss.rownumber == 10_000
    assert await ss.fetchone() is None
    assert await ss.fetchall() == ()


async def test_fetch_methods(ss: aio.SSCursor) -> None:
    await ss.execute(SEQ)
    assert await ss.fetchone() == (1,)
    assert await ss.fetchmany() == ((2,),)
    ss.arraysize = 5
    assert await ss.fetchmany() == ((3,), (4,))
    assert await ss.fetchmany() == ()
    await ss.execute(SEQ)
    assert await ss.fetchmany(3) == ((1,), (2,), (3,))
    assert await ss.fetchall() == ((4,),)
    await ss.execute(SEQ)
    assert [row async for row in ss.fetchall_unbuffered()] == [(1,), (2,), (3,), (4,)]


async def test_statement_without_result_set(ss: aio.SSCursor) -> None:
    await ss.execute("CREATE TEMPORARY TABLE t (id INT AUTO_INCREMENT PRIMARY KEY)")
    assert await ss.execute("INSERT INTO t VALUES (), ()") == 2
    assert ss.lastrowid == 1
    assert ss.description is None
    assert ss.rownumber is None
    with pytest.raises(rustmy.ProgrammingError, match="no result set"):
        await ss.fetchone()


async def test_scroll_forward(ss: aio.SSCursor) -> None:
    await ss.execute(MANY)
    assert await ss.fetchone() == (0, "row-0")
    await ss.scroll(10)
    assert await ss.fetchone() == (11, "row-11")
    await ss.scroll(5000, mode="absolute")
    assert ss.rownumber == 5000
    assert await ss.fetchone() == (5000, "row-5000")
    with pytest.raises(rustmy.NotSupportedError, match="backwards"):
        await ss.scroll(-1)
    with pytest.raises(IndexError):
        await ss.scroll(10_000)
    assert ss.rownumber == 10_000
    assert await ss.fetchone() is None


async def test_nextset_and_dicts(server: Server) -> None:
    connection = await connect(server, multi_statements=True)
    async with connection, connection.cursor(aio.SSDictCursor) as ss:
        await ss.execute(f"{SEQ}; SELECT 1 AS x, 2 AS x; SELECT 3")
        await ss.scroll(2)
        assert await ss.fetchone() == {"n": 3}
        # The rest of the first result is skipped without a warning.
        with warnings.catch_warnings():
            warnings.simplefilter("error")
            assert await ss.nextset() is True
        assert await ss.fetchall() == ({"x": 1, ".x": 2},)
        assert await ss.nextset() is True
        assert await ss.nextset() is None
        await ss.execute("SELECT 4 AS y")
        assert await ss.fetchall() == ({"y": 4},)


async def test_rows_before_a_server_error_are_delivered(ss: aio.SSCursor) -> None:
    await ss.execute(
        "SELECT n, IF(n < 5000, 1, (SELECT 1 UNION ALL SELECT 2)) AS x "
        f"FROM ({MANY} LIMIT 10000) AS numbers"
    )
    rows: list[tuple[Any, ...]] = []
    with pytest.raises(rustmy.OperationalError) as excinfo:
        while (batch := await ss.fetchmany(1000)) != ():
            rows.extend(batch)
    assert excinfo.value.args[0] == 1242
    assert [n for n, _ in rows] == list(range(5000))
    assert await ss.fetchone() is None
    await ss.execute("SELECT 1")
    assert await ss.fetchall() == ((1,),)


async def test_another_command_discards_unread_rows(
    conn: aio.Connection, ss: aio.SSCursor
) -> None:
    await ss.execute(MANY)
    assert await ss.fetchone() == (0, "row-0")
    other = conn.cursor()
    with pytest.warns(UserWarning, match="left incomplete") as record:
        await other.execute("SELECT 'other'")
    # The warning points at the caller, not at rustmy internals.
    assert record[0].filename == __file__
    assert await other.fetchall() == (("other",),)
    with pytest.raises(rustmy.ProgrammingError, match="discarded"):
        await ss.fetchone()
    with pytest.raises(rustmy.ProgrammingError, match="discarded"):
        await ss.scroll(1)
    await ss.execute(SEQ)
    assert await ss.fetchall() == ((1,), (2,), (3,), (4,))


async def test_finished_or_closed_streams_discard_nothing(conn: aio.Connection) -> None:
    other = conn.cursor()
    with warnings.catch_warnings():
        warnings.simplefilter("error")
        ss = conn.cursor(aio.SSCursor)
        await ss.execute(SEQ)
        assert await ss.fetchmany(4) == ((1,), (2,), (3,), (4,))
        await other.execute("SELECT 1")
        # Closing drops the rest quietly.
        ss = conn.cursor(aio.SSCursor)
        await ss.execute(MANY)
        await ss.fetchone()
        await ss.close()
        await other.execute("SELECT 2")
        async with conn.cursor(aio.SSCursor) as ss2:
            await ss2.execute(MANY)
        # Running another query on the same cursor too.
        ss = conn.cursor(aio.SSCursor)
        await ss.execute(MANY)
        await ss.execute(SEQ)
        assert await ss.fetchall() == ((1,), (2,), (3,), (4,))
    assert await other.execute("SELECT 3") == 1


async def test_warning_as_error_leaves_the_stream_intact(
    conn: aio.Connection, ss: aio.SSCursor
) -> None:
    await ss.execute(MANY)
    assert await ss.fetchone() == (0, "row-0")
    with warnings.catch_warnings():
        warnings.simplefilter("error")
        with pytest.raises(UserWarning):
            await conn.cursor().execute("SELECT 1")
    assert await ss.fetchone() == (1, "row-1")


async def test_a_conversion_error_aborts_the_stream(server: Server) -> None:
    def broken(_value: bytes) -> object:
        msg = "cannot convert"
        raise ValueError(msg)

    conv = {**conversions, FIELD_TYPE.VAR_STRING: broken}
    async with (
        await connect(server, conv=conv) as connection,
        connection.cursor(aio.SSCursor) as ss,
    ):
        await ss.execute(MANY)
        with pytest.raises(ValueError, match="cannot convert"):
            await ss.fetchone()
        with pytest.raises(rustmy.InterfaceError, match="conversion"):
            await ss.fetchone()
        # The connection is in sync again with the next command.
        async with connection.cursor() as cur:
            await cur.execute("SELECT 1")
            assert await cur.fetchone() == (1,)
