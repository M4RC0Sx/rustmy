"""rustmy's asyncio SQLAlchemy dialects (:mod:`rustmy.aio.dialect`), beyond
what SQLAlchemy's own suite checks (``tests/sqla_suite`` with the
``*_async`` databases)."""

from __future__ import annotations

import asyncio
import datetime as dt
import sys

from decimal import Decimal
from typing import TYPE_CHECKING

import pytest
import sqlalchemy as sa

from sqlalchemy import orm
from sqlalchemy.dialects import registry

import rustmy

from rustmy import aio
from rustmy.aio.dialect import RustmyAsyncMariaDBDialect
from rustmy.aio.dialect import RustmyAsyncMySQLDialect
from rustmy.constants import CLIENT


if TYPE_CHECKING:
    from collections.abc import AsyncIterator

    from sqlalchemy.ext.asyncio import AsyncEngine

    from tests.conftest import Server


# greenlet (3.5) crashes the interpreter on free-threaded builds at its first
# switch, before any rustmy code runs: nothing asyncio in SQLAlchemy works
# there yet.
pytestmark = pytest.mark.skipif(
    not getattr(sys, "_is_gil_enabled", lambda: True)(),
    reason="greenlet crashes on free-threaded builds",
)

asyncio_ext = pytest.importorskip("sqlalchemy.ext.asyncio")


def _url(server: Server, query: str = "", driver: str = "rustmy") -> str:
    scheme = "mariadb" if server.name == "mariadb" else "mysql"
    url = f"{scheme}+{driver}://rustmy:rustmy@{server.host}:{server.port}/rustmy_test"
    return f"{url}?{query}" if query else url


@pytest.fixture
async def engine(server: Server) -> AsyncIterator[AsyncEngine]:
    engine = asyncio_ext.create_async_engine(_url(server))
    yield engine
    await engine.dispose()


# Registration and URL arguments (no server needed) ---------------------------


def test_the_blocking_url_picks_the_asyncio_dialect() -> None:
    assert registry.load("mysql.rustmy_async") is RustmyAsyncMySQLDialect
    assert registry.load("mariadb.rustmy_async") is RustmyAsyncMariaDBDialect
    for url, dialect in (
        ("mysql+rustmy://", RustmyAsyncMySQLDialect),
        ("mariadb+rustmy://", RustmyAsyncMariaDBDialect),
        ("mysql+rustmy_async://", RustmyAsyncMySQLDialect),
    ):
        engine = asyncio_ext.create_async_engine(url)
        assert type(engine.dialect) is dialect
        assert engine.dialect.is_async
        assert isinstance(engine.pool, sa.pool.AsyncAdaptedQueuePool)
    # The blocking engine keeps the blocking dialect.
    assert type(sa.create_engine("mysql+rustmy://").dialect).__name__ == (
        "RustmyMySQLDialect"
    )


def test_url_arguments() -> None:
    dialect = asyncio_ext.create_async_engine("mysql+rustmy://").dialect
    url = sa.make_url(
        "mysql+rustmy://u:p@h:3307/db?cursorclass=PreparedCursor"
        "&read_timeout=2.5&compress=zstd&ssl_mode=required"
    )
    _, kwargs = dialect.create_connect_args(url)
    assert kwargs["cursorclass"] is aio.PreparedCursor
    assert kwargs["read_timeout"] == 2.5
    assert kwargs["compress"] == "zstd"
    assert kwargs["ssl_mode"] == "required"
    assert (kwargs["user"], kwargs["database"], kwargs["port"]) == ("u", "db", 3307)
    assert kwargs["client_flag"] & CLIENT.FOUND_ROWS
    with pytest.raises(ValueError, match=r"rustmy\.aio\.cursors"):
        dialect.create_connect_args(sa.make_url("mysql+rustmy://h/db?cursorclass=os"))


# Against the servers -----------------------------------------------------------


async def test_core_round_trip_and_found_rows(engine: AsyncEngine) -> None:
    async with engine.begin() as conn:
        await conn.exec_driver_sql(
            "CREATE TEMPORARY TABLE t (id INT PRIMARY KEY, v INT)"
        )
        await conn.execute(
            sa.text("INSERT INTO t VALUES (:id, :v)"),
            [{"id": i, "v": i * 10} for i in range(1, 101)],
        )
        total = await conn.scalar(sa.text("SELECT SUM(v) FROM t"))
        assert total == Decimal(50500)
        # Matched but unchanged counts (FOUND_ROWS).
        result = await conn.exec_driver_sql("UPDATE t SET v = 10 WHERE id = 1")
        assert result.rowcount == 1
        driver = conn.sync_connection.connection.driver_connection  # type: ignore[union-attr]
        assert isinstance(driver, aio.Connection)


async def test_isolation_levels_and_autocommit(engine: AsyncEngine) -> None:
    for level in ("READ COMMITTED", "SERIALIZABLE", "REPEATABLE READ"):
        async with engine.connect() as conn:
            leveled = await conn.execution_options(isolation_level=level)
            assert await leveled.get_isolation_level() == level
    async with engine.connect() as conn:
        autocommit = await conn.execution_options(isolation_level="AUTOCOMMIT")
        raw = await autocommit.get_raw_connection()
        assert raw.driver_connection.get_autocommit()  # type: ignore[union-attr]


async def test_stream_results_use_a_server_side_cursor(engine: AsyncEngine) -> None:
    query = (
        "SELECT seq FROM seq_1_to_1000"
        if engine.dialect.name == "mariadb"
        else "WITH RECURSIVE s (seq) AS (SELECT 1 UNION ALL "
        "SELECT seq + 1 FROM s WHERE seq < 1000) SELECT seq FROM s"
    )
    async with engine.connect() as conn:
        result = await conn.stream(sa.text(query))
        assert result._real_result.cursor.server_side  # type: ignore[attr-defined]  # noqa: SLF001
        assert sum([row.seq async for row in result]) == 500500


async def test_prepared_statements_from_the_url(server: Server) -> None:
    engine = asyncio_ext.create_async_engine(_url(server, "cursorclass=PreparedCursor"))
    try:
        async with engine.connect() as conn:
            before = await conn.scalar(
                sa.text("SHOW SESSION STATUS LIKE 'Com_stmt_prepare'").columns(
                    sa.column("Variable_name"), sa.column("Value")
                )
            )
            assert before is not None
            value = await conn.scalar(sa.text("SELECT :x + 1"), {"x": 41})
            assert value == 42
            status = await conn.exec_driver_sql(
                "SHOW SESSION STATUS LIKE 'Com_stmt_prepare'"
            )
            assert int(status.one()[1]) >= 1
    finally:
        await engine.dispose()


async def test_pre_ping_replaces_a_killed_connection(server: Server) -> None:
    engine = asyncio_ext.create_async_engine(
        _url(server), pool_size=1, pool_pre_ping=True
    )
    try:
        async with engine.connect() as conn:
            victim = await conn.scalar(sa.text("SELECT CONNECTION_ID()"))
        with server.connect() as admin:
            admin.cursor().execute(f"KILL {victim}")
        async with engine.connect() as conn:
            assert await conn.scalar(sa.text("SELECT CONNECTION_ID()")) != victim
    finally:
        await engine.dispose()


async def test_a_closed_connection_counts_as_a_disconnect(engine: AsyncEngine) -> None:
    async with engine.connect() as conn:
        raw = await conn.get_raw_connection()
        raw.driver_connection.close()  # type: ignore[union-attr]
        with pytest.raises(sa.exc.DBAPIError) as excinfo:
            await conn.exec_driver_sql("SELECT 1")
        assert excinfo.value.connection_invalidated
    async with engine.connect() as conn:
        assert await conn.scalar(sa.text("SELECT 1")) == 1


async def test_a_cancelled_statement_does_not_return_its_connection(
    server: Server,
) -> None:
    """Cancelled in the middle of a statement, the connection is closed
    (rustmy.aio) and the pool replaces it."""
    engine = asyncio_ext.create_async_engine(_url(server), pool_size=1, max_overflow=0)
    try:

        async def sleeper() -> None:
            async with engine.connect() as conn:
                await conn.exec_driver_sql("SELECT SLEEP(5)")

        task = asyncio.create_task(sleeper())
        await asyncio.sleep(0.2)
        task.cancel()
        with pytest.raises(asyncio.CancelledError):
            await task

        async def select_one() -> object:
            async with engine.connect() as conn:
                return await conn.scalar(sa.text("SELECT 1"))

        assert await asyncio.wait_for(select_one(), 2) == 1
    finally:
        await engine.dispose()


class Base(orm.DeclarativeBase):
    pass


class Item(Base):
    __tablename__ = "aio_dialect_items"

    id: orm.Mapped[int] = orm.mapped_column(primary_key=True)
    name: orm.Mapped[str] = orm.mapped_column(sa.String(50))
    price: orm.Mapped[Decimal] = orm.mapped_column(sa.Numeric(10, 2))
    created: orm.Mapped[dt.datetime] = orm.mapped_column(sa.DateTime)
    version: orm.Mapped[int] = orm.mapped_column(nullable=False)

    __mapper_args__ = {"version_id_col": version}  # noqa: RUF012


async def test_orm_round_trip(engine: AsyncEngine) -> None:
    async with engine.begin() as conn:
        await conn.run_sync(Base.metadata.drop_all)
        await conn.run_sync(Base.metadata.create_all)
    try:
        created = dt.datetime(2026, 11, 24, 9, 30, 15)
        async with asyncio_ext.AsyncSession(engine, expire_on_commit=False) as session:
            session.add_all(
                Item(id=i, name=f"item {i}", price=Decimal("9.99"), created=created)
                for i in range(1, 51)
            )
            await session.commit()
            item = await session.get(Item, 7)
            assert item is not None
            assert (item.name, item.price, item.created) == (
                "item 7",
                Decimal("9.99"),
                created,
            )
            item.name = "renamed"
            await session.commit()
            assert item.version == 2
            count = await session.scalar(sa.select(sa.func.count()).select_from(Item))
            assert count == 50
            await session.execute(sa.delete(Item).where(Item.id > 40))
            await session.commit()
            ids = (await session.scalars(sa.select(Item.id).order_by(Item.id))).all()
            assert ids == list(range(1, 41))
    finally:
        async with engine.begin() as conn:
            await conn.run_sync(Base.metadata.drop_all)


async def test_errors_are_rustmy_s(engine: AsyncEngine) -> None:
    async with engine.connect() as conn:
        with pytest.raises(sa.exc.ProgrammingError) as excinfo:
            await conn.exec_driver_sql("SELECT * FROM no_such_table")
        assert isinstance(excinfo.value.orig, rustmy.ProgrammingError)
        assert excinfo.value.orig.args[0] == 1146
