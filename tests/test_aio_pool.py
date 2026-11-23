"""``rustmy.aio.create_pool``: connections shared by tasks (the accounts
behind it are tested in ``crates/proto/src/pool.rs``)."""

from __future__ import annotations

import asyncio
import time

from typing import TYPE_CHECKING
from typing import Any

import pytest

import rustmy

from rustmy import aio


if TYPE_CHECKING:
    from collections.abc import AsyncIterator
    from collections.abc import Awaitable
    from collections.abc import Callable

    from tests.conftest import Server

    MakePool = Callable[..., Awaitable[aio.Pool]]


@pytest.fixture
async def make_pool(server: Server) -> AsyncIterator[MakePool]:
    pools: list[aio.Pool] = []

    async def make(minsize: int = 1, maxsize: int = 2, **options: Any) -> aio.Pool:
        pool = await aio.create_pool(minsize, maxsize, **server.connect_args(**options))
        pools.append(pool)
        return pool

    yield make
    for pool in pools:
        pool.terminate()
        await pool.wait_closed()


async def eventually(condition: Callable[[], bool]) -> None:
    """Waits (2 s at most) for what a background task does."""
    deadline = time.monotonic() + 2
    while not condition():
        assert time.monotonic() < deadline, "timed out"
        await asyncio.sleep(0.01)


async def test_lends_and_reuses_connections(make_pool: MakePool) -> None:
    pool = await make_pool(1, 2)
    assert (pool.minsize, pool.maxsize, pool.size, pool.freesize) == (1, 2, 1, 1)
    async with pool.acquire() as conn:
        assert pool.freesize == 0
        first = conn.thread_id()
        async with conn.cursor() as cur:
            await cur.execute("SELECT 1")
            assert await cur.fetchall() == ((1,),)
    assert pool.freesize == 1
    # aiomysql's way: await acquire(), then release().
    conn = await pool.acquire()
    assert conn.thread_id() == first
    other = await pool.acquire()
    assert other.thread_id() != first
    assert pool.size == 2
    await pool.release(other)
    pool.release(conn)  # not awaited: it runs anyway
    await eventually(lambda: pool.freesize == 2)


async def test_waits_in_order_and_times_out(make_pool: MakePool) -> None:
    pool = await make_pool(0, 1)
    conn = await pool.acquire()
    start = time.monotonic()
    with pytest.raises(rustmy.PoolTimeout):
        await pool.acquire(timeout=0.2)
    assert 0.15 < time.monotonic() - start < 1
    got: list[int] = []

    async def waiter(tag: int) -> None:
        async with pool.acquire(timeout=5):
            got.append(tag)

    tasks = []
    for tag in range(3):
        tasks.append(asyncio.create_task(waiter(tag)))
        await asyncio.sleep(0.01)
    await pool.release(conn)
    await asyncio.gather(*tasks)
    assert got == [0, 1, 2]
    assert pool.size == 1


async def test_a_transaction_left_open_is_rolled_back(make_pool: MakePool) -> None:
    pool = await make_pool(1, 1)
    async with pool.acquire() as conn, conn.cursor() as cur:
        await conn.autocommit(False)  # noqa: FBT003  # aiomysql-compatible API
        await cur.execute("CREATE TEMPORARY TABLE t (n INT) ENGINE=InnoDB")
        await cur.execute("INSERT INTO t VALUES (1)")
    async with pool.acquire() as conn, conn.cursor() as cur:
        await cur.execute("SELECT COUNT(*) FROM t")
        assert await cur.fetchone() == (0,)


async def test_reset_on_release(make_pool: MakePool) -> None:
    pool = await make_pool(1, 1, reset_on_release=True)
    async with pool.acquire() as conn, conn.cursor() as cur:
        first = conn.thread_id()
        await cur.execute("SET @kept = 1")
    async with pool.acquire() as conn, conn.cursor() as cur:
        assert conn.thread_id() == first
        await cur.execute("SELECT @kept")
        assert await cur.fetchone() == (None,)


async def test_a_closed_connection_is_replaced(make_pool: MakePool) -> None:
    pool = await make_pool(1, 2)
    async with pool.acquire() as conn:
        first = conn.thread_id()
        conn.close()
    await eventually(lambda: pool.freesize == 1)
    async with pool.acquire() as conn:
        assert conn.thread_id() != first


async def test_a_dead_idle_connection_is_not_lent(
    server: Server, make_pool: MakePool
) -> None:
    pool = await make_pool(1, 1, pre_ping=0)
    async with pool.acquire() as conn:
        first = conn.thread_id()
    with server.connect() as admin:
        admin.cursor().execute(f"KILL {first}")
    async with pool.acquire() as conn, conn.cursor() as cur:
        assert conn.thread_id() != first
        await cur.execute("SELECT 1")


async def test_recycle_and_idle_timeout(make_pool: MakePool) -> None:
    pool = await make_pool(0, 1, pool_recycle=0)
    async with pool.acquire() as conn:
        first = conn.thread_id()
    assert pool.size == 0
    async with pool.acquire() as conn:
        assert conn.thread_id() != first
    pool = await make_pool(1, 3, idle_timeout=0)
    conns = [await pool.acquire() for _ in range(3)]
    for conn in conns:
        await pool.release(conn)
    assert (pool.size, pool.freesize) == (1, 1)


async def test_close_and_wait_closed(make_pool: MakePool) -> None:
    pool = await make_pool(1, 1)
    conn = await pool.acquire()
    waiter = asyncio.create_task(pool.acquire(timeout=5).__aenter__())
    await asyncio.sleep(0.01)
    with pytest.raises(rustmy.InterfaceError, match="first"):
        await pool.wait_closed()
    pool.close()
    with pytest.raises(rustmy.InterfaceError, match="pool closed"):
        await waiter
    with pytest.raises(rustmy.InterfaceError, match="pool closed"):
        await pool.acquire()
    closed = asyncio.create_task(pool.wait_closed())
    await asyncio.sleep(0.05)
    assert not closed.done()
    # The connection still works for whoever has it, and closes on return.
    async with conn.cursor() as cur:
        await cur.execute("SELECT 1")
    await pool.release(conn)
    await asyncio.wait_for(closed, 1)
    assert conn.closed
    assert pool.size == 0


async def test_async_with_create_pool(server: Server) -> None:
    async with (
        aio.create_pool(1, 2, **server.connect_args()) as pool,
        pool.acquire() as conn,
        conn.cursor() as cur,
    ):
        await cur.execute("SELECT 1")
    assert pool.closed
    assert pool.size == 0


async def test_terminate_closes_connections_in_use(make_pool: MakePool) -> None:
    pool = await make_pool(1, 1)
    conn = await pool.acquire()
    pool.terminate()
    assert conn.closed
    await pool.release(conn)
    await asyncio.wait_for(pool.wait_closed(), 1)


async def test_connect_errors(server: Server, make_pool: MakePool) -> None:
    with pytest.raises(rustmy.OperationalError):
        await aio.create_pool(2, 2, **server.connect_args(password="wrong"))
    pool = await make_pool(0, 1, password="wrong")
    with pytest.raises(rustmy.OperationalError):
        await pool.acquire()
    # The room is given back.
    assert pool.size == 0


async def test_only_its_own_connections_are_given_back(
    server: Server, make_pool: MakePool
) -> None:
    pool = await make_pool(1, 1)
    stranger = await aio.connect(**server.connect_args())
    with pytest.raises(rustmy.InterfaceError):
        pool.release(stranger)
    await stranger.ensure_closed()


async def test_a_cancelled_waiter_takes_nothing(make_pool: MakePool) -> None:
    pool = await make_pool(0, 1)
    conn = await pool.acquire()
    waiter = asyncio.create_task(pool.acquire().__aenter__())
    await asyncio.sleep(0.01)
    waiter.cancel()
    with pytest.raises(asyncio.CancelledError):
        await waiter
    await pool.release(conn)
    assert (pool.size, pool.freesize) == (1, 1)
    async with pool.acquire(timeout=1) as again:
        assert again is conn


async def test_a_command_cancelled_closes_its_connection(make_pool: MakePool) -> None:
    pool = await make_pool(1, 1)

    async def sleeper() -> None:
        async with pool.acquire() as conn, conn.cursor() as cur:
            await cur.execute("SELECT SLEEP(5)")

    task = asyncio.create_task(sleeper())
    await asyncio.sleep(0.2)
    task.cancel()
    with pytest.raises(asyncio.CancelledError):
        await task
    # Not lent again: a new one is opened for the minimum.
    await eventually(lambda: pool.freesize == 1)
    async with pool.acquire(timeout=1) as conn, conn.cursor() as cur:
        await cur.execute("SELECT 1")
        assert await cur.fetchone() == (1,)


async def test_tasks_share_the_connections(make_pool: MakePool) -> None:
    pool = await make_pool(1, 4)
    in_use: set[int] = set()
    most = 0

    async def work() -> None:
        nonlocal most
        for _ in range(20):
            async with pool.acquire(timeout=10) as conn, conn.cursor() as cur:
                conn_id = conn.thread_id()
                assert conn_id not in in_use, "lent twice"
                in_use.add(conn_id)
                most = max(most, len(in_use))
                await cur.execute("SELECT CONNECTION_ID()")
                assert await cur.fetchone() == (conn_id,)
                in_use.discard(conn_id)

    await asyncio.gather(*(work() for _ in range(64)))
    assert most == 4
    assert pool.size == pool.freesize == 4
