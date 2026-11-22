"""The asyncio connection's core (``rustmy._aio``, first cut): the protocol
runs on the event loop's thread, driven by the loop's callbacks."""

from __future__ import annotations

import asyncio
import datetime as dt

from decimal import Decimal
from typing import TYPE_CHECKING

import pytest

import rustmy

from rustmy import _aio


if TYPE_CHECKING:
    from tests.conftest import Server


async def connect(server: Server, **overrides: object) -> _aio.Connection:
    kwargs: dict[str, object] = {
        "host": server.host,
        "port": server.port,
        "user": "rustmy",
        "password": "rustmy",
        "database": "rustmy_test",
    }
    kwargs.update(overrides)
    return await _aio.connect(**kwargs)  # type: ignore[arg-type]


async def test_a_query_with_parameters(server: Server) -> None:
    async with await connect(server) as conn, conn.cursor() as cur:
        assert await cur.execute("SELECT %s + 1, %s, %s", (41, "héllo ✓", None)) == 1
        assert cur.fetchall() == ((42, "héllo ✓", None),)
        assert cur.description[0][0] == "%s + 1" or cur.description[0][0] == "41 + 1"


async def test_rows_of_every_kind_and_many_of_them(server: Server) -> None:
    async with await connect(server) as conn, conn.cursor() as cur:
        await cur.execute(
            "SELECT CAST(1.25 AS DECIMAL(5,2)), "
            "CAST('2026-11-22 10:00:00.5' AS DATETIME(1)), "
            "CAST('2026-11-22' AS DATE), X'00ff', 1.5e0"
        )
        assert cur.fetchone() == (
            Decimal("1.25"),
            dt.datetime(2026, 11, 22, 10, 0, 0, 500000),
            dt.date(2026, 11, 22),
            b"\x00\xff",
            1.5,
        )
        # Rows arrive over many reads and are converted in batches.
        limit = (
            "cte_max_recursion_depth"
            if server.name == "mysql"
            else "max_recursive_iterations"
        )
        await cur.execute(f"SET SESSION {limit} = 100000")
        count = await cur.execute(
            "WITH RECURSIVE n (i) AS "
            "(SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 50000) "
            "SELECT i, REPEAT('x', i % 100) FROM n"
        )
        rows = cur.fetchall()
        assert count == len(rows) == 50000
        assert rows[-1] == (50000, "")
        assert rows[99] == (100, "")
        assert rows[98] == (99, "x" * 99)


async def test_dict_rows(server: Server) -> None:
    async with await connect(server) as conn, conn.cursor(as_dict=True) as cur:
        await cur.execute("SELECT 1 AS a, 'x' AS b")
        assert cur.fetchall() == ({"a": 1, "b": "x"},)


async def test_statements_without_rows(server: Server) -> None:
    async with await connect(server) as conn, conn.cursor() as cur:
        await cur.execute(
            "CREATE TEMPORARY TABLE aio_t (id INT AUTO_INCREMENT PRIMARY KEY, v INT)"
        )
        assert await cur.execute("INSERT INTO aio_t (v) VALUES (1), (2), (3)") == 3
        assert cur.lastrowid == 1
        assert cur.description is None
        assert cur.fetchall() == ()


async def test_a_server_error_leaves_the_connection_usable(server: Server) -> None:
    async with await connect(server) as conn, conn.cursor() as cur:
        with pytest.raises(rustmy.ProgrammingError) as error:
            await cur.execute("SELECT * FROM no_such_table")
        assert error.value.args[0] == 1146
        await cur.execute("SELECT 'still usable'")
        assert cur.fetchone() == ("still usable",)


async def test_one_command_at_a_time(server: Server) -> None:
    async with await connect(server) as conn:
        first, second = conn.cursor(), conn.cursor()
        running = asyncio.ensure_future(first.execute("SELECT SLEEP(0.1), 1"))
        await asyncio.sleep(0)
        with pytest.raises(RuntimeError, match="another command is running"):
            await second.execute("SELECT 2")
        # The first one still runs to its end, and the connection goes on.
        await running
        assert first.fetchone() == (0, 1)
        await second.execute("SELECT 3")
        assert second.fetchone() == (3,)


async def test_a_wrong_password_fails_the_connection(server: Server) -> None:
    with pytest.raises(rustmy.OperationalError) as error:
        await connect(server, password="wrong")
    assert error.value.args[0] == 1045


async def test_a_killed_connection_fails_and_stays_closed(server: Server) -> None:
    async with await connect(server) as conn, conn.cursor() as cur:
        await cur.execute("SELECT CONNECTION_ID()")
        (connection_id,) = cur.fetchone()
        with server.connect() as admin:
            admin.cursor().execute(f"KILL {int(connection_id)}")
        with pytest.raises(rustmy.OperationalError):
            await cur.execute("SELECT 1")
        with pytest.raises(rustmy.OperationalError):
            await cur.execute("SELECT 1")


async def test_a_cancelled_command_closes_the_connection(server: Server) -> None:
    async with await connect(server) as conn, conn.cursor() as cur:
        task = asyncio.ensure_future(cur.execute("SELECT SLEEP(0.2)"))
        await asyncio.sleep(0.05)
        task.cancel()
        with pytest.raises(asyncio.CancelledError):
            await task
        # Its reply would arrive in the middle of the next command's.
        with pytest.raises(rustmy.OperationalError):
            await cur.execute("SELECT 4")
