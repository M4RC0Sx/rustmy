"""``rustmy.aio``: ``read_timeout``/``write_timeout`` and ``LOAD DATA LOCAL
INFILE`` with the file read off the event loop's thread."""

from __future__ import annotations

import asyncio
import time

from concurrent.futures import ThreadPoolExecutor
from typing import TYPE_CHECKING
from typing import Any

import pytest

import rustmy

from rustmy import aio


if TYPE_CHECKING:
    from collections.abc import Callable
    from pathlib import Path

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


async def test_read_timeout(server: Server) -> None:
    conn = await connect(server, read_timeout=0.3)
    start = time.perf_counter()
    async with conn.cursor() as cur:
        with pytest.raises(rustmy.OperationalError) as info:
            await cur.execute("SELECT SLEEP(3)")
    assert info.value.args[0] == 2013
    assert time.perf_counter() - start < 2
    # The stream is in an unknown state afterwards: the connection is gone.
    assert conn.closed
    with pytest.raises(rustmy.OperationalError):
        await conn.cursor().execute("SELECT 1")
    await conn.ensure_closed()


async def test_read_timeout_counts_only_waiting_for_the_server(server: Server) -> None:
    """Time between commands, or while a streamed result is held, is not
    waiting for the server; many short commands add up past the timeout."""
    conn = await connect(server, read_timeout=0.2)
    async with conn, conn.cursor() as cur:
        await asyncio.sleep(0.4)
        await cur.execute("SELECT 1")
        start = time.perf_counter()
        while time.perf_counter() - start < 0.5:
            await cur.execute("SELECT SLEEP(0.05)")
        async with conn.cursor(aio.SSCursor) as ss:
            await ss.execute("SELECT 1 UNION ALL SELECT 2")
            assert await ss.fetchone() == (1,)
            await asyncio.sleep(0.4)
            assert await ss.fetchall() == ((2,),)
        assert not conn.closed


async def test_write_timeout_alone_does_not_bound_reads(server: Server) -> None:
    conn = await connect(server, write_timeout=0.1)
    async with conn, conn.cursor() as cur:
        await cur.execute("SELECT SLEEP(0.4)")
        assert await cur.fetchone() == (0,)


TABLE = "CREATE TEMPORARY TABLE loaded (id INT PRIMARY KEY, s VARCHAR(64))"
LOAD = "LOAD DATA LOCAL INFILE %s INTO TABLE loaded FIELDS TERMINATED BY ','"


class CountingExecutor(ThreadPoolExecutor):
    """The loop's default executor, counting what runs in it."""

    def __init__(self) -> None:
        super().__init__(max_workers=2)
        self.jobs = 0

    def submit(self, fn: Callable[..., Any], /, *args: Any, **kwargs: Any) -> Any:
        self.jobs += 1
        return super().submit(fn, *args, **kwargs)


ROWS = 40_000  # about 600 KiB: three pieces of 256 KiB


@pytest.fixture
def allowed(tmp_path: Path) -> Path:
    """The directory the connection allows (resolved: macOS's temporary
    directory is behind a symbolic link), with the files the test loads."""
    path = tmp_path.resolve()
    (path / "rows.csv").write_text("".join(f"{i},row {i}\n" for i in range(ROWS)))
    (path / "empty.csv").write_text("")
    return path


async def test_local_infile_is_read_off_the_loop(server: Server, allowed: Path) -> None:
    path = allowed / "rows.csv"
    rows = ROWS
    executor = CountingExecutor()
    asyncio.get_running_loop().set_default_executor(executor)
    conn = await connect(server, local_infile_dir=allowed, autocommit=True)
    async with conn, conn.cursor() as cur:
        await cur.execute(TABLE)
        # The loop keeps running while the file is read and sent.
        ticks = 0

        async def tick() -> None:
            nonlocal ticks
            while True:
                ticks += 1
                await asyncio.sleep(0)

        ticker = asyncio.create_task(tick())
        try:
            assert await cur.execute(LOAD, (str(path),)) == rows
        finally:
            ticker.cancel()
        assert executor.jobs >= 3
        assert ticks > 0
        await cur.execute("SELECT COUNT(*), MAX(id), MAX(s) FROM loaded")
        assert await cur.fetchone() == (rows, rows - 1, "row 9999")
        # An empty file, and one refused, leave the connection in sync.
        empty = allowed / "empty.csv"
        assert await cur.execute(LOAD, (str(empty),)) == 0
        with pytest.raises(rustmy.OperationalError) as info:
            await cur.execute(LOAD, (str(allowed / "missing.csv"),))
        assert info.value.args[0] == 2068
        await cur.execute("SELECT 1")
        assert await cur.fetchone() == (1,)
