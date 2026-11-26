"""``rustmy.aio``'s public API: the same arguments, session settings,
cursors and errors as the blocking connection, awaited."""

from __future__ import annotations

import asyncio
import socket
import time
import uuid
import warnings

from decimal import Decimal
from typing import TYPE_CHECKING
from typing import Any

import pytest

import rustmy

from rustmy import aio
from rustmy.constants import ER
from rustmy.constants import FIELD_TYPE
from rustmy.converters import conversions


if TYPE_CHECKING:
    from collections.abc import AsyncIterator
    from collections.abc import Iterator
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


async def one(conn: aio.Connection, sql: str) -> Any:
    async with conn.cursor() as cur:
        await cur.execute(sql)
        row = await cur.fetchone()
    assert row is not None
    return row[0]


@pytest.fixture
async def conn(server: Server) -> AsyncIterator[aio.Connection]:
    connection = await connect(server)
    yield connection
    await connection.ensure_closed()


async def test_positional_arguments_and_aliases(server: Server) -> None:
    connection = await aio.connect(
        server.host,
        "rustmy",
        None,
        None,
        server.port,
        passwd="rustmy",
        db="rustmy_test",
    )
    async with connection:
        assert await one(connection, "SELECT DATABASE()") == "rustmy_test"
        assert connection.open
    assert connection.closed


async def test_option_file(server: Server, tmp_path: Path) -> None:
    cnf = tmp_path / "my.cnf"
    cnf.write_text(
        f"[client]\nhost={server.host}\nport={server.port}\nuser=rustmy\n"
        "password=rustmy\ndatabase=rustmy_test\n"
    )
    cnf.chmod(0o600)
    async with await aio.connect(read_default_file=cnf) as connection:
        assert await one(connection, "SELECT DATABASE()") == "rustmy_test"


async def test_session_settings(server: Server) -> None:
    async with await connect(
        server,
        charset="latin1",
        collation="latin1_bin",
        sql_mode="ANSI_QUOTES",
        autocommit=True,
        init_command="SET @init = 'yes'",
    ) as connection:
        async with connection.cursor() as cur:
            await cur.execute("SELECT @@collation_connection, @@sql_mode, @@autocommit")
            assert await cur.fetchone() == ("latin1_bin", "ANSI_QUOTES", 1)
        assert await one(connection, "SELECT @init") == "yes"
        assert connection.get_autocommit()
        assert connection.character_set_name() == "latin1"
        assert connection.encoding == "cp1252"
        # Text in latin1 both ways.
        async with connection.cursor() as cur:
            await cur.execute("SELECT %s", ("café",))
            assert await cur.fetchone() == ("café",)
        await connection.reset_connection()
        assert await one(connection, "SELECT @init") == "yes"
        async with connection.cursor() as cur:
            await cur.execute("SELECT @@collation_connection, @@sql_mode, @@autocommit")
            assert await cur.fetchone() == ("latin1_bin", "ANSI_QUOTES", 1)


async def test_sql_mode_drives_escaping(server: Server) -> None:
    async with await connect(server, sql_mode="NO_BACKSLASH_ESCAPES") as connection:
        assert connection.no_backslash_escapes
        async with connection.cursor() as cur:
            await cur.execute("SELECT %s", ("a\\b'c",))
            assert await cur.fetchone() == ("a\\b'c",)
        assert connection.escape_string("a\\b'c") == "a\\b''c"
        assert connection.literal("a'b") == "'a''b'"


async def test_transactions(conn: aio.Connection) -> None:
    table = f"aio_tx_{uuid.uuid4().hex[:8]}"
    async with conn.cursor() as cur:
        await cur.execute(f"CREATE TABLE {table} (a INT)")
        try:
            assert not conn.get_autocommit()
            await cur.execute(f"INSERT INTO {table} VALUES (1)")
            await conn.rollback()
            await cur.execute(f"SELECT COUNT(*) FROM {table}")
            assert await cur.fetchone() == (0,)
            await conn.begin()
            await cur.execute(f"INSERT INTO {table} VALUES (2)")
            await conn.commit()
            await conn.autocommit(on=True)
            assert conn.get_autocommit()
            await cur.execute(f"SELECT a FROM {table}")
            assert await cur.fetchall() == ((2,),)
        finally:
            await cur.execute(f"DROP TABLE {table}")


async def test_conv(server: Server) -> None:
    def decimal_as_float(value: str) -> float:
        return float(value)

    conv = {**conversions, FIELD_TYPE.NEWDECIMAL: decimal_as_float}
    async with await connect(server, conv=conv) as connection:
        async with connection.cursor() as cur:
            await cur.execute("SELECT CAST(1.25 AS DECIMAL(5,2)), 7")
            assert await cur.fetchone() == (1.25, 7)
        assert connection.encoders[Decimal] is conv[Decimal]


@pytest.mark.parametrize(
    "cursor_class", [aio.Cursor, aio.PreparedCursor], ids=lambda c: c.__name__
)
async def test_cursors(conn: aio.Connection, cursor_class: type[aio.Cursor]) -> None:
    async with conn.cursor(cursor_class) as cur:
        assert await cur.execute("SELECT %s, %s, %s", (1, "x", Decimal("2.50"))) == 1
        assert await cur.fetchall() == ((1, "x", Decimal("2.50")),)
        await cur.execute(
            "SELECT 1 UNION ALL SELECT 2 UNION ALL SELECT 3 UNION ALL SELECT 4"
        )
        assert cur.rownumber == 0
        assert await cur.fetchone() == (1,)
        assert await cur.fetchmany(2) == ((2,), (3,))
        cur.scroll(-1)
        assert [row async for row in cur] == [(3,), (4,)]
        cur.scroll(0, mode="absolute")
        assert await cur.fetchall() == ((1,), (2,), (3,), (4,))
        with pytest.raises(IndexError):
            cur.scroll(10)


async def test_dict_and_prepared_variants(conn: aio.Connection) -> None:
    assert isinstance(conn.cursor(prepared=True), aio.PreparedCursor)
    assert isinstance(
        conn.cursor(aio.DictCursor, prepared=True), aio.PreparedDictCursor
    )
    async with conn.cursor(aio.PreparedDictCursor) as cur:
        await cur.execute("SELECT %s AS a, %s AS b", (1, "x"))
        assert await cur.fetchall() == ({"a": 1, "b": "x"},)


async def test_default_cursor_class(server: Server) -> None:
    async with await connect(server, cursorclass=aio.DictCursor) as connection:
        assert type(connection.cursor()) is aio.DictCursor


async def test_mogrify_and_executed(conn: aio.Connection) -> None:
    async with conn.cursor() as cur:
        assert cur.mogrify("SELECT %s, %s", (1, "it's")) == "SELECT 1, 'it\\'s'"
        await cur.execute("SELECT %s", ("a",))
        assert cur._executed == "SELECT 'a'"  # noqa: SLF001


async def test_multiple_results_and_callproc(server: Server) -> None:
    procedure = f"aio_proc_{uuid.uuid4().hex[:8]}"
    connection = await connect(server, multi_statements=True)
    async with connection, connection.cursor() as cur:
        await cur.execute("SELECT 1; SELECT 2, 3")
        assert await cur.fetchall() == ((1,),)
        assert await cur.nextset() is True
        assert await cur.fetchall() == ((2, 3),)
        assert await cur.nextset() is None
        await cur.execute(
            f"CREATE PROCEDURE {procedure}"
            "(IN a INT, OUT b INT, INOUT c VARCHAR(10)) "
            "BEGIN SELECT a; SET b = a * 2; SELECT CONCAT(c, '!'); "
            "SET c = UPPER(c); END"
        )
        try:
            args = (21, None, "hi")
            assert await cur.callproc(procedure, args) is args
            assert await cur.fetchall() == ((21,),)
            assert await cur.nextset() is True
            assert await cur.fetchall() == (("hi!",),)
            assert await cur.nextset() is True
            assert await cur.nextset() is None
            await cur.execute(f"SELECT @`_{procedure}_1`, @`_{procedure}_2`")
            assert await cur.fetchall() == ((42, "HI"),)
        finally:
            await cur.execute(f"DROP PROCEDURE {procedure}")


async def test_simple_commands(conn: aio.Connection) -> None:
    await conn.ping()
    with pytest.raises(rustmy.NotSupportedError):
        await conn.ping(reconnect=True)
    await conn.select_db("information_schema")
    assert await one(conn, "SELECT DATABASE()") == "information_schema"
    assert isinstance(conn.thread_id(), int)
    assert conn.get_server_info()
    await conn.set_character_set("latin1")
    assert conn.character_set_name() == "latin1"
    assert await one(conn, "SELECT @@character_set_client") == "latin1"


async def test_change_user(server: Server) -> None:
    async with await connect(
        server, init_command="SET @init = 'yes'", autocommit=True
    ) as connection:
        await connection.change_user("root", "rustmy", "rustmy_test")
        assert str(await one(connection, "SELECT CURRENT_USER()")).startswith("root@")
        assert await one(connection, "SELECT @init") == "yes"
        assert connection.get_autocommit()


async def test_a_failed_change_user_leaves_the_connection_unusable(
    server: Server,
) -> None:
    connection = await connect(server)
    with pytest.raises(rustmy.OperationalError) as info:
        await connection.change_user("root", "wrong")
    assert info.value.args[0] == ER.ACCESS_DENIED_ERROR
    with pytest.raises(rustmy.OperationalError):
        await one(connection, "SELECT 1")
    assert not connection.open
    connection.close()


async def test_closing(conn: aio.Connection) -> None:
    conn.close()
    with pytest.raises(rustmy.InterfaceError):
        conn.close()
    assert conn.closed
    with pytest.raises(rustmy.OperationalError):
        await one(conn, "SELECT 1")


async def test_a_dropped_connection_is_closed(
    conn: aio.Connection, server: Server
) -> None:
    """The event loop keeps the protocol alive, so a connection dropped
    without closing it would stay open (and keep its server session) for as
    long as the loop runs."""
    dropped = await connect(server)
    thread = dropped.thread_id()
    with pytest.warns(ResourceWarning, match="Unclosed connection"):
        del dropped
    sql = f"SELECT COUNT(*) FROM information_schema.PROCESSLIST WHERE ID = {thread}"
    deadline = time.monotonic() + 5
    while await one(conn, sql):
        assert time.monotonic() < deadline, "the server session is still open"
        await asyncio.sleep(0.01)


async def test_a_closed_connection_dropped_does_not_warn(server: Server) -> None:
    closed = await connect(server)
    closed.close()
    with warnings.catch_warnings():
        warnings.simplefilter("error", ResourceWarning)
        del closed


async def test_server_errors(conn: aio.Connection) -> None:
    async with conn.cursor() as cur:
        with pytest.raises(rustmy.ProgrammingError) as info:
            await cur.execute("SELEC 1")
        assert info.value.args[0] == ER.PARSE_ERROR
        await cur.execute("CREATE TEMPORARY TABLE aio_pk (id INT PRIMARY KEY)")
        await cur.execute("INSERT INTO aio_pk VALUES (1)")
        with pytest.raises(rustmy.IntegrityError) as duplicate:
            await cur.execute("INSERT INTO aio_pk VALUES (1)")
        assert duplicate.value.args[0] == ER.DUP_ENTRY
        # Still usable after both.
        await cur.execute("SELECT COUNT(*) FROM aio_pk")
        assert await cur.fetchone() == (1,)


@pytest.fixture
def silent_port() -> Iterator[int]:
    """A port whose listener accepts connections and never says a word."""
    with socket.create_server(("127.0.0.1", 0)) as listener:
        yield listener.getsockname()[1]


async def test_connect_timeout(silent_port: int) -> None:
    start = time.perf_counter()
    with pytest.raises(rustmy.OperationalError) as info:
        await aio.connect(host="127.0.0.1", port=silent_port, connect_timeout=0.3)
    assert info.value.args[0] == 2003
    assert time.perf_counter() - start < 3


async def test_connection_refused() -> None:
    with socket.socket() as probe:
        probe.bind(("127.0.0.1", 0))
        port = probe.getsockname()[1]
    with pytest.raises(rustmy.OperationalError) as info:
        await aio.connect(host="127.0.0.1", port=port)
    assert info.value.args[0] == 2003


async def test_many_connections_at_once(server: Server) -> None:
    connections = await asyncio.gather(*(connect(server) for _ in range(20)))
    try:
        ids = await asyncio.gather(
            *(one(c, "SELECT CONNECTION_ID()") for c in connections)
        )
        assert len(set(ids)) == 20
    finally:
        for connection in connections:
            connection.close()
