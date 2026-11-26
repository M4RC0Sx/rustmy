"""The README's performance section: absolute times of everyday workloads,
100,000 and 1,000,000 rows, rustmy against mysqlclient, PyMySQL and
mysql-connector-python (with its C extension), and rustmy.aio against
asyncmy, aiomysql and mysql.connector.aio.

    docker compose up -d --wait mysql
    uv run --no-sync python benches/showcase.py [--repeat N] [--json PATH]
    uv run --no-sync python benches/showcase.py --render PATH...   # charts + tables

Areas: plain DB-API (tuples, dicts, streaming, inserts), prepared statements,
SQLAlchemy 2 (Core and ORM) and Django (QuerySets and ``bulk_create``), the
peak memory of reading a million rows, and on asyncio the DB-API workloads
plus a pool's (``asyncio``) and SQLAlchemy's (``sqlalchemy-asyncio``). Every
driver runs each area in a fresh process of its own (Django can only load
one ``MySQLdb``), over plain TCP with ``autocommit``, against the same
tables; best of ``--repeat`` (``--repeat-large`` for the 1M-row workloads).
``--loops asyncio,uvloop`` runs the asyncio areas on both loops;
``--latency MS`` runs through the ``toxiproxy`` compose service with that
much added to every reply (``SHOWCASE_SIZES`` picks the sizes).

The table is an order list (integers, a name, a decimal price, a
microsecond datetime, a nullable date and a nullable note): the mix most
applications read. mysql-connector-python is GPL software used here only to
measure; it is not a dependency and never ships with rustmy.
"""

from __future__ import annotations

import argparse
import asyncio
import datetime as dt
import gc
import inspect
import json
import os
import platform
import resource
import subprocess
import sys
import time

from dataclasses import dataclass
from decimal import Decimal
from pathlib import Path
from typing import TYPE_CHECKING
from typing import Any


if TYPE_CHECKING:
    from collections.abc import Awaitable
    from collections.abc import Callable
    from collections.abc import Coroutine
    from collections.abc import Iterable
    from collections.abc import Iterator


HERE = Path(__file__).resolve().parent
RESULTS = HERE / "results"
HOST = "127.0.0.1"
PORT = int(os.environ.get("RUSTMY_BENCH_PORT", "13306"))
USER = "rustmy"
PASSWORD = "rustmy"  # docker test server
DATABASE = "rustmy_test"

DRIVERS = ("rustmy", "mysqlclient", "pymysql", "mysqlconnector")
#: The asyncio areas' drivers; mysql-connector-python's asyncio API is
#: ``mysql.connector.aio`` (pure Python), and SQLAlchemy has no asyncio
#: dialect for it.
ASYNC_DRIVERS = ("rustmy", "asyncmy", "aiomysql", "mysqlconnector")
AREA_DRIVERS = {
    "asyncio": ASYNC_DRIVERS,
    "sqlalchemy-asyncio": ("rustmy", "asyncmy", "aiomysql"),
}
LABELS = {
    "rustmy": "rustmy",
    "rustmy-prepared": "rustmy, PreparedCursor",
    "mysqlclient": "mysqlclient",
    "pymysql": "PyMySQL",
    "mysqlconnector": "mysql-connector",
    "mysqlconnector-prepared": "mysql-connector, prepared",
}
#: Row counts measured; SHOWCASE_SIZES="1000,5000" for a quick trial run.
SIZES = tuple(
    int(n) for n in os.environ.get("SHOWCASE_SIZES", "100000,1000000").split(",")
)
TABLE = "showcase_orders"
INSERT_TABLE = "showcase_orders_insert"
COLUMNS = "id, customer_id, product, quantity, price, created_at, shipped_on, note"
SCHEMA = """(
    id INT PRIMARY KEY,
    customer_id INT NOT NULL,
    product VARCHAR(64) NOT NULL,
    quantity SMALLINT NOT NULL,
    price DECIMAL(10, 2) NOT NULL,
    created_at DATETIME(6) NOT NULL,
    shipped_on DATE NULL,
    note VARCHAR(255) NULL
)"""
_DIGITS = " UNION ALL ".join(f"SELECT {d} AS d" for d in range(10))
_NUMBERS = (
    "SELECT "
    + " + ".join(f"t{i}.d * {10**i}" for i in range(6))
    + " + 1 AS n FROM "
    + ", ".join(f"({_DIGITS}) AS t{i}" for i in range(6))
)
SETUP = [
    f"DROP TABLE IF EXISTS {TABLE}, {INSERT_TABLE}",
    f"CREATE TABLE {TABLE} {SCHEMA}",
    f"""INSERT INTO {TABLE}
        SELECT n, 1 + n % 50000, CONCAT('Product ', n % 1000, ' ñ'), 1 + n % 9,
               (n % 100000) / 100,
               TIMESTAMPADD(MICROSECOND, n * 31536, '2025-01-01'),
               IF(n % 4 = 0, NULL, DATE_ADD('2025-01-01', INTERVAL n % 365 DAY)),
               IF(n % 3 = 0, NULL, CONCAT('Leave at the door, order ', n))
        FROM ({_NUMBERS}) AS numbers""",
    f"CREATE TABLE {INSERT_TABLE} {SCHEMA}",
]


@dataclass(frozen=True)
class Scenario:
    area: str
    name: str
    rows: int
    #: What ``rows`` counts (the pool workload counts queries).
    unit: str = "rows"


def rows_to_insert(count: int) -> list[tuple[Any, ...]]:
    base = dt.datetime(2026, 1, 1)  # noqa: DTZ001  # DATETIME is naive
    return [
        (
            i,
            1 + i % 50000,
            f"Product {i % 1000} ñ",
            1 + i % 9,
            Decimal(i % 100000) / 100,
            base + dt.timedelta(seconds=i),
            None if i % 4 == 0 else dt.date(2026, 1, 1) + dt.timedelta(days=i % 365),
            None if i % 3 == 0 else f"Leave at the door, order {i}",
        )
        for i in range(1, count + 1)
    ]


def best(
    run: Callable[[], object], repeat: int, reset: Callable[[], object] | None = None
) -> float:
    times = []
    for _ in range(repeat):
        if reset is not None:
            reset()
        gc.collect()
        start = time.perf_counter()
        run()
        times.append(time.perf_counter() - start)
    return min(times)


# --- DB-API ------------------------------------------------------------------


def dbapi_connect(driver: str) -> Any:
    common: dict[str, Any] = {
        "host": HOST,
        "port": PORT,
        "user": USER,
        "password": PASSWORD,
        "database": DATABASE,
        "autocommit": True,
    }
    # Plain TCP for everyone: rustmy, libmysqlclient and mysql-connector
    # would otherwise negotiate TLS.
    if driver == "rustmy":
        import rustmy  # noqa: PLC0415

        return rustmy.connect(**common, ssl_mode="disabled")
    if driver == "mysqlclient":
        import MySQLdb  # noqa: PLC0415

        return MySQLdb.connect(**common, ssl_mode="DISABLED")
    if driver == "pymysql":
        import pymysql  # noqa: PLC0415

        return pymysql.connect(**common)
    import mysql.connector  # noqa: PLC0415

    conn = mysql.connector.connect(**common, ssl_disabled=True, use_pure=False)
    if type(conn).__name__ != "CMySQLConnection":
        msg = "mysql-connector-python runs without its C extension"
        raise RuntimeError(msg)
    return conn


def dict_cursor(driver: str, conn: Any) -> Any:
    if driver == "rustmy":
        from rustmy.cursors import DictCursor  # noqa: PLC0415

        return conn.cursor(DictCursor)
    if driver == "mysqlclient":
        from MySQLdb.cursors import DictCursor as MySQLdbDict  # noqa: PLC0415

        return conn.cursor(MySQLdbDict)
    if driver == "pymysql":
        from pymysql.cursors import DictCursor as PyMySQLDict  # noqa: PLC0415

        return conn.cursor(PyMySQLDict)
    return conn.cursor(dictionary=True)


def stream_cursor(driver: str, conn: Any) -> Any:
    if driver == "rustmy":
        from rustmy.cursors import SSCursor  # noqa: PLC0415

        return conn.cursor(SSCursor)
    if driver == "mysqlclient":
        from MySQLdb.cursors import SSCursor as MySQLdbSS  # noqa: PLC0415

        return conn.cursor(MySQLdbSS)
    if driver == "pymysql":
        from pymysql.cursors import SSCursor as PyMySQLSS  # noqa: PLC0415

        return conn.cursor(PyMySQLSS)
    # mysql-connector's default cursor is unbuffered: rows come as iterated.
    return conn.cursor()


def dbapi(
    driver: str, repeat: int, repeat_large: int
) -> Iterator[tuple[Scenario, float]]:
    conn = dbapi_connect(driver)
    placeholders = ", ".join(["%s"] * len(COLUMNS.split(",")))
    insert = f"INSERT INTO {INSERT_TABLE} ({COLUMNS}) VALUES ({placeholders})"
    for rows in SIZES:
        times = repeat if rows < max(SIZES) else repeat_large
        select = f"SELECT {COLUMNS} FROM {TABLE} WHERE id <= {rows}"

        def fetchall(
            cursor_for: Callable[[], Any], sql: str = select, n: int = rows
        ) -> None:
            cursor = cursor_for()
            cursor.execute(sql)
            assert len(cursor.fetchall()) == n
            cursor.close()

        def stream(sql: str = select, n: int = rows) -> None:
            cursor = stream_cursor(driver, conn)
            cursor.execute(sql)
            assert sum(1 for _ in cursor) == n
            cursor.close()

        yield (
            Scenario("dbapi", "Read rows (tuples)", rows),
            best(lambda: fetchall(conn.cursor), times),
        )
        yield (
            Scenario("dbapi", "Read rows (dicts)", rows),
            best(lambda: fetchall(lambda: dict_cursor(driver, conn)), times),
        )
        yield Scenario("dbapi", "Stream rows", rows), best(stream, times)

        data = rows_to_insert(rows)

        def executemany(data: list[tuple[Any, ...]] = data) -> None:
            cursor = conn.cursor()
            # In chunks of 10,000 for every driver: some build the whole
            # batch as one statement, beyond max_allowed_packet at 1M rows.
            for start in range(0, len(data), 10_000):
                cursor.executemany(insert, data[start : start + 10_000])
            cursor.close()

        def truncate() -> None:
            cursor = conn.cursor()
            cursor.execute(f"TRUNCATE {INSERT_TABLE}")
            cursor.close()

        yield (
            Scenario("dbapi", "Insert rows (executemany)", rows),
            best(executemany, times, truncate),
        )
    conn.close()


def prepared(
    driver: str, repeat: int, repeat_large: int
) -> Iterator[tuple[Scenario, float]]:
    """The same read through each driver's prepared statements (the binary
    protocol); mysqlclient and PyMySQL have none, so they read as text."""
    conn = dbapi_connect(driver)
    for rows in SIZES:
        times = repeat if rows < max(SIZES) else repeat_large
        sql = f"SELECT {COLUMNS} FROM {TABLE} WHERE id <= %s"
        if driver == "rustmy":
            from rustmy.cursors import PreparedCursor  # noqa: PLC0415

            def make() -> Any:
                return conn.cursor(PreparedCursor)

            config = "rustmy-prepared"
        elif driver == "mysqlconnector":

            def make() -> Any:
                return conn.cursor(prepared=True)

            sql = sql.replace("%s", "?")
            config = "mysqlconnector-prepared"
        else:
            make = conn.cursor
            config = driver

        def run(n: int = rows, make: Callable[[], Any] = make, sql: str = sql) -> None:
            cursor = make()
            cursor.execute(sql, (n,))
            assert len(cursor.fetchall()) == n
            cursor.close()

        yield Scenario("prepared", f"Read rows ({config})", rows), best(run, times)
    conn.close()


# --- SQLAlchemy --------------------------------------------------------------


def sqlalchemy_models() -> tuple[Any, Any, list[str]]:
    """The ORM classes of the orders table and of the insert table, and the
    column names."""
    import sqlalchemy as sa  # noqa: PLC0415

    from sqlalchemy import orm  # noqa: PLC0415
    from sqlalchemy.dialects import mysql  # noqa: PLC0415

    class Base(orm.DeclarativeBase):
        pass

    def columns() -> dict[str, Any]:
        return {
            "id": orm.mapped_column(sa.Integer, primary_key=True, autoincrement=False),
            "customer_id": orm.mapped_column(sa.Integer),
            "product": orm.mapped_column(sa.String(64)),
            "quantity": orm.mapped_column(sa.SmallInteger),
            "price": orm.mapped_column(sa.Numeric(10, 2)),
            "created_at": orm.mapped_column(mysql.DATETIME(fsp=6)),
            "shipped_on": orm.mapped_column(sa.Date, nullable=True),
            "note": orm.mapped_column(sa.String(255), nullable=True),
        }

    order = type("Order", (Base,), {"__tablename__": TABLE, **columns()})
    inserted = type("Inserted", (Base,), {"__tablename__": INSERT_TABLE, **columns()})
    return order, inserted, [c.strip() for c in COLUMNS.split(",")]


def sqlalchemy(
    driver: str, repeat: int, repeat_large: int
) -> Iterator[tuple[Scenario, float]]:
    import sqlalchemy as sa  # noqa: PLC0415

    from sqlalchemy import orm  # noqa: PLC0415

    order, inserted, names = sqlalchemy_models()
    urls = {
        "rustmy": "mysql+rustmy://{u}:{p}@{h}:{port}/{d}?ssl_mode=disabled",
        "mysqlclient": "mysql+mysqldb://{u}:{p}@{h}:{port}/{d}?ssl_mode=DISABLED",
        "pymysql": "mysql+pymysql://{u}:{p}@{h}:{port}/{d}",
        "mysqlconnector": "mysql+mysqlconnector://{u}:{p}@{h}:{port}/{d}",
    }
    connect_args = (
        {"ssl_disabled": True, "use_pure": False} if driver == "mysqlconnector" else {}
    )
    engine = sa.create_engine(
        urls[driver].format(u=USER, p=PASSWORD, h=HOST, port=PORT, d=DATABASE),
        connect_args=connect_args,
        pool_size=1,
    )
    table = order.__table__
    # render_nulls: without it the ORM leaves out the NULL columns of each
    # row and splits the batch wherever the NULLs change (thousands of
    # statements here, with any driver).
    statement = sa.insert(inserted).execution_options(render_nulls=True)
    for rows in SIZES:
        times = repeat if rows < max(SIZES) else repeat_large

        def core(n: int = rows) -> None:
            with engine.connect() as conn:
                result = conn.execute(sa.select(table).where(table.c.id <= n)).all()
            assert len(result) == n

        def orm_load(n: int = rows) -> None:
            with orm.Session(engine) as session:
                query: Any = sa.select(order).where(order.id <= n)
                objects: list[Any] = list(session.scalars(query))
            assert len(objects) == n

        def orm_stream(n: int = rows) -> None:
            with orm.Session(engine) as session:
                query: Any = (
                    sa.select(order)
                    .where(order.id <= n)
                    .execution_options(yield_per=2000)
                )
                assert sum(1 for _ in session.scalars(query)) == n

        data = [dict(zip(names, row, strict=True)) for row in rows_to_insert(rows)]

        def orm_insert(data: list[dict[str, Any]] = data) -> None:
            with orm.Session(engine) as session, session.begin():
                for start in range(0, len(data), 10_000):
                    session.execute(statement, data[start : start + 10_000])

        def truncate() -> None:
            with engine.begin() as conn:
                conn.exec_driver_sql(f"TRUNCATE {INSERT_TABLE}")

        yield Scenario("sqlalchemy", "Core: read rows", rows), best(core, times)
        yield Scenario("sqlalchemy", "ORM: load objects", rows), best(orm_load, times)
        yield (
            Scenario("sqlalchemy", "ORM: stream objects (yield_per)", rows),
            best(orm_stream, times),
        )
        yield (
            Scenario("sqlalchemy", "ORM: bulk insert", rows),
            best(orm_insert, times, truncate),
        )
    engine.dispose()


# --- Django ------------------------------------------------------------------


def django_backend(driver: str) -> tuple[str, dict[str, Any]]:
    """The ENGINE and OPTIONS for ``driver`` (plain TCP for all)."""
    if driver == "rustmy":
        import rustmy  # noqa: PLC0415

        rustmy.install_as_MySQLdb()
        return "django.db.backends.mysql", {"ssl_mode": "disabled"}
    if driver == "mysqlclient":
        return "django.db.backends.mysql", {"ssl_mode": "DISABLED"}
    if driver == "pymysql":
        import pymysql  # noqa: PLC0415

        # What PyMySQL's users do: Django refuses its own version number.
        pymysql.version_info = (2, 2, 1, "final", 0)
        pymysql.install_as_MySQLdb()
        return "django.db.backends.mysql", {}
    return "mysql.connector.django", {"ssl_disabled": True, "use_pure": False}


def django(
    driver: str, repeat: int, repeat_large: int
) -> Iterator[tuple[Scenario, float]]:
    engine, options = django_backend(driver)
    import django as django_module  # noqa: PLC0415

    from django.conf import settings  # noqa: PLC0415

    settings.configure(
        DATABASES={
            "default": {
                "ENGINE": engine,
                "HOST": HOST,
                "PORT": PORT,
                "USER": USER,
                "PASSWORD": PASSWORD,
                "NAME": DATABASE,
                "OPTIONS": options,
            }
        },
        INSTALLED_APPS=[],
        USE_TZ=False,
        DEFAULT_AUTO_FIELD="django.db.models.AutoField",
    )
    django_module.setup()

    from django.db import connection  # noqa: PLC0415
    from django.db import models  # noqa: PLC0415

    def fields() -> dict[str, Any]:
        return {
            "id": models.IntegerField(primary_key=True),
            "customer_id": models.IntegerField(),
            "product": models.CharField(max_length=64),
            "quantity": models.SmallIntegerField(),
            "price": models.DecimalField(max_digits=10, decimal_places=2),
            "created_at": models.DateTimeField(),
            "shipped_on": models.DateField(null=True),
            "note": models.CharField(max_length=255, null=True),
            "__module__": __name__,
        }

    meta = type("Meta", (), {"app_label": "showcase", "db_table": TABLE})
    order: Any = type("Order", (models.Model,), {**fields(), "Meta": meta})
    meta_insert = type("Meta", (), {"app_label": "showcase", "db_table": INSERT_TABLE})
    inserted: Any = type("Inserted", (models.Model,), {**fields(), "Meta": meta_insert})
    names = [c.strip() for c in COLUMNS.split(",")]
    for rows in SIZES:
        times = repeat if rows < max(SIZES) else repeat_large

        def queryset(n: int = rows) -> None:
            assert len(list(order.objects.filter(id__lte=n))) == n

        def values_list(n: int = rows) -> None:
            assert len(list(order.objects.filter(id__lte=n).values_list(*names))) == n

        def iterator(n: int = rows) -> None:
            objects = order.objects.filter(id__lte=n).iterator(chunk_size=2000)
            assert sum(1 for _ in objects) == n

        objects = [
            inserted(**dict(zip(names, row, strict=True)))
            for row in rows_to_insert(rows)
        ]

        def bulk_create(objects: list[Any] = objects) -> None:
            inserted.objects.bulk_create(objects, batch_size=10_000)

        def truncate() -> None:
            with connection.cursor() as cursor:
                cursor.execute(f"TRUNCATE {INSERT_TABLE}")

        yield Scenario("django", "QuerySet: load objects", rows), best(queryset, times)
        yield (
            Scenario("django", "QuerySet: values_list()", rows),
            best(values_list, times),
        )
        yield Scenario("django", "QuerySet: iterator()", rows), best(iterator, times)
        yield (
            Scenario("django", "bulk_create()", rows),
            best(bulk_create, times, truncate),
        )


# --- asyncio -----------------------------------------------------------------

#: The event loop the asyncio areas run on (``--loop``).
LOOP = "asyncio"
#: The pool workload: tasks sharing a pool of connections, each running its
#: share of point lookups.
POOL_TASKS = 32
POOL_SIZE = 8
POOL_QUERIES = 6400


def run_loop(
    main: Callable[[], Coroutine[Any, Any, list[tuple[Scenario, float]]]],
) -> list[tuple[Scenario, float]]:
    if LOOP == "uvloop":
        import uvloop  # noqa: PLC0415

        results: list[tuple[Scenario, float]] = uvloop.run(main())
        return results
    return asyncio.run(main())


async def abest(
    run: Callable[[], Awaitable[object]],
    repeat: int,
    reset: Callable[[], Awaitable[object]] | None = None,
) -> float:
    times = []
    for _ in range(repeat):
        if reset is not None:
            await reset()
        gc.collect()
        start = time.perf_counter()
        await run()
        times.append(time.perf_counter() - start)
    return min(times)


def aio_kwargs(driver: str) -> dict[str, Any]:
    """Connection arguments: plain TCP, ``autocommit``."""
    common: dict[str, Any] = {
        "host": HOST,
        "port": PORT,
        "user": USER,
        "password": PASSWORD,
        "autocommit": True,
    }
    if driver == "rustmy":
        return {**common, "database": DATABASE, "ssl_mode": "disabled"}
    if driver == "mysqlconnector":
        return {**common, "database": DATABASE, "ssl_disabled": True}
    return {**common, "db": DATABASE}


async def aio_connect(driver: str) -> Any:
    if driver == "rustmy":
        from rustmy import aio  # noqa: PLC0415

        return await aio.connect(**aio_kwargs(driver))
    if driver == "asyncmy":
        import asyncmy  # noqa: PLC0415

        return await asyncmy.connect(**aio_kwargs(driver))
    if driver == "aiomysql":
        import aiomysql  # noqa: PLC0415

        return await aiomysql.connect(**aio_kwargs(driver))
    import mysql.connector.aio  # noqa: PLC0415

    return await mysql.connector.aio.connect(**aio_kwargs(driver))


async def aio_close(driver: str, conn: Any) -> None:
    if driver == "mysqlconnector":
        await conn.close()
    else:
        await conn.ensure_closed()


async def aio_cursor(driver: str, conn: Any, kind: str = "tuple") -> Any:
    """A cursor of ``kind`` (``tuple``, ``dict`` or ``stream``)."""
    if driver == "mysqlconnector":
        # Its default cursor is unbuffered: rows come as iterated.
        return await conn.cursor(dictionary=kind == "dict")
    if driver == "rustmy":
        from rustmy import aio  # noqa: PLC0415

        classes: dict[str, Any] = {"dict": aio.DictCursor, "stream": aio.SSCursor}
    elif driver == "asyncmy":
        from asyncmy import cursors  # noqa: PLC0415

        classes = {"dict": cursors.DictCursor, "stream": cursors.SSCursor}
    else:
        import aiomysql  # noqa: PLC0415

        classes = {"dict": aiomysql.DictCursor, "stream": aiomysql.SSCursor}
    cursor = conn.cursor(classes[kind]) if kind in classes else conn.cursor()
    # aiomysql's cursor() is a coroutine.
    return await cursor if inspect.isawaitable(cursor) else cursor


async def aio_count(driver: str, cursor: Any) -> int:
    """Iterates over the rows of an unbuffered cursor."""
    count = 0
    if driver == "mysqlconnector":
        # Its cursors' __aiter__ is a coroutine, which async for refuses.
        while batch := await cursor.fetchmany(1000):
            count += len(batch)
    else:
        async for _ in cursor:
            count += 1
    return count


async def aio_pool(driver: str) -> tuple[Any, Callable[[], Any]]:
    """A pool of ``POOL_SIZE`` connections and how to close it."""
    kwargs = aio_kwargs(driver)
    if driver == "rustmy":
        from rustmy import aio  # noqa: PLC0415

        pool: Any = await aio.create_pool(POOL_SIZE, POOL_SIZE, **kwargs)
    elif driver == "asyncmy":
        import asyncmy  # noqa: PLC0415

        pool = await asyncmy.create_pool(minsize=POOL_SIZE, maxsize=POOL_SIZE, **kwargs)
    else:
        import aiomysql  # noqa: PLC0415

        pool = await aiomysql.create_pool(
            minsize=POOL_SIZE, maxsize=POOL_SIZE, **kwargs
        )

    async def close() -> None:
        pool.close()
        await pool.wait_closed()

    return pool, close


def asyncio_dbapi(
    driver: str, repeat: int, repeat_large: int
) -> list[tuple[Scenario, float]]:
    async def main() -> list[tuple[Scenario, float]]:
        out: list[tuple[Scenario, float]] = []
        conn = await aio_connect(driver)
        placeholders = ", ".join(["%s"] * len(COLUMNS.split(",")))
        insert = f"INSERT INTO {INSERT_TABLE} ({COLUMNS}) VALUES ({placeholders})"
        for rows in SIZES:
            times = repeat if rows < max(SIZES) else repeat_large
            select = f"SELECT {COLUMNS} FROM {TABLE} WHERE id <= {rows}"

            async def fetchall(kind: str, sql: str = select, n: int = rows) -> None:
                cursor = await aio_cursor(driver, conn, kind)
                await cursor.execute(sql)
                assert len(await cursor.fetchall()) == n
                await cursor.close()

            async def stream(sql: str = select, n: int = rows) -> None:
                cursor = await aio_cursor(driver, conn, "stream")
                await cursor.execute(sql)
                assert await aio_count(driver, cursor) == n
                await cursor.close()

            out.append(
                (
                    Scenario("asyncio", "Read rows (tuples)", rows),
                    await abest(lambda: fetchall("tuple"), times),
                )
            )
            out.append(
                (
                    Scenario("asyncio", "Read rows (dicts)", rows),
                    await abest(lambda: fetchall("dict"), times),
                )
            )
            out.append(
                (
                    Scenario("asyncio", "Stream rows", rows),
                    await abest(stream, times),
                )
            )
            data = rows_to_insert(rows)

            async def executemany(data: list[tuple[Any, ...]] = data) -> None:
                cursor = await aio_cursor(driver, conn)
                # In chunks of 10,000, as in the blocking area.
                for start in range(0, len(data), 10_000):
                    await cursor.executemany(insert, data[start : start + 10_000])
                await cursor.close()

            async def truncate() -> None:
                cursor = await aio_cursor(driver, conn)
                await cursor.execute(f"TRUNCATE {INSERT_TABLE}")
                await cursor.close()

            out.append(
                (
                    Scenario("asyncio", "Insert rows (executemany)", rows),
                    await abest(executemany, times, truncate),
                )
            )
        await aio_close(driver, conn)
        # mysql.connector.aio's pool fails when it has no idle connection
        # instead of waiting for one: it cannot serve more tasks than
        # connections.
        if driver != "mysqlconnector":
            out.append(
                (
                    Scenario(
                        "asyncio",
                        f"Pool: {POOL_TASKS} tasks, {POOL_SIZE} connections",
                        POOL_QUERIES,
                        "queries",
                    ),
                    await pool_queries(driver, repeat),
                )
            )
        return out

    return run_loop(main)


async def pool_queries(driver: str, repeat: int) -> float:
    """``POOL_TASKS`` tasks, each acquiring a connection of the pool for every
    point lookup of its share."""
    pool, close = await aio_pool(driver)
    sql = f"SELECT {COLUMNS} FROM {TABLE} WHERE id = %s"
    share = POOL_QUERIES // POOL_TASKS

    async def task(first: int) -> None:
        for i in range(first, first + share):
            async with pool.acquire() as conn:
                cursor = await aio_cursor(driver, conn)
                await cursor.execute(sql, (i,))
                assert len(await cursor.fetchall()) == 1
                await cursor.close()

    async def run() -> None:
        await asyncio.gather(*(task(1 + t * share) for t in range(POOL_TASKS)))

    elapsed = await abest(run, repeat)
    await close()
    return elapsed


def sqlalchemy_asyncio(
    driver: str, repeat: int, repeat_large: int
) -> list[tuple[Scenario, float]]:
    import sqlalchemy as sa  # noqa: PLC0415

    from sqlalchemy.ext import asyncio as sa_asyncio  # noqa: PLC0415

    order, inserted, names = sqlalchemy_models()
    urls = {
        # create_async_engine picks rustmy's asyncio dialect.
        "rustmy": "mysql+rustmy://{u}:{p}@{h}:{port}/{d}?ssl_mode=disabled",
        "asyncmy": "mysql+asyncmy://{u}:{p}@{h}:{port}/{d}",
        "aiomysql": "mysql+aiomysql://{u}:{p}@{h}:{port}/{d}",
    }
    table = order.__table__
    statement = sa.insert(inserted).execution_options(render_nulls=True)

    async def main() -> list[tuple[Scenario, float]]:
        out: list[tuple[Scenario, float]] = []
        engine = sa_asyncio.create_async_engine(
            urls[driver].format(u=USER, p=PASSWORD, h=HOST, port=PORT, d=DATABASE),
            pool_size=1,
        )
        for rows in SIZES:
            times = repeat if rows < max(SIZES) else repeat_large

            async def core(n: int = rows) -> None:
                async with engine.connect() as conn:
                    result = await conn.execute(sa.select(table).where(table.c.id <= n))
                    assert len(result.all()) == n

            async def orm_load(n: int = rows) -> None:
                async with sa_asyncio.AsyncSession(engine) as session:
                    query: Any = sa.select(order).where(order.id <= n)
                    objects = list(await session.scalars(query))
                assert len(objects) == n

            async def orm_stream(n: int = rows) -> None:
                async with sa_asyncio.AsyncSession(engine) as session:
                    query: Any = (
                        sa.select(order)
                        .where(order.id <= n)
                        .execution_options(yield_per=2000)
                    )
                    count = 0
                    async for _ in await session.stream_scalars(query):
                        count += 1
                assert count == n

            data = [dict(zip(names, row, strict=True)) for row in rows_to_insert(rows)]

            async def orm_insert(data: list[dict[str, Any]] = data) -> None:
                async with sa_asyncio.AsyncSession(engine) as session, session.begin():
                    for start in range(0, len(data), 10_000):
                        await session.execute(statement, data[start : start + 10_000])

            async def truncate() -> None:
                async with engine.begin() as conn:
                    await conn.exec_driver_sql(f"TRUNCATE {INSERT_TABLE}")

            area = "sqlalchemy-asyncio"
            out.append(
                (
                    Scenario(area, "Core: read rows", rows),
                    await abest(core, times),
                )
            )
            out.append(
                (
                    Scenario(area, "ORM: load objects", rows),
                    await abest(orm_load, times),
                )
            )
            out.append(
                (
                    Scenario(area, "ORM: stream objects (yield_per)", rows),
                    await abest(orm_stream, times),
                )
            )
            out.append(
                (
                    Scenario(area, "ORM: bulk insert", rows),
                    await abest(orm_insert, times, truncate),
                )
            )
        await engine.dispose()
        return out

    return run_loop(main)


# --- Memory ------------------------------------------------------------------


def _maxrss_bytes() -> int:
    peak = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
    # Bytes on macOS, KiB on Linux.
    return peak if sys.platform == "darwin" else peak * 1024


def memory(
    driver: str,
    _repeat: int,
    _repeat_large: int,
) -> Iterator[tuple[Scenario, float]]:
    """Peak memory growth of ``fetchall()`` of 1M rows, in MiB (a fresh
    process per driver: the peak only goes up)."""
    conn = dbapi_connect(driver)
    cursor = conn.cursor()
    gc.collect()
    before = _maxrss_bytes()
    rows = max(SIZES)
    cursor.execute(f"SELECT {COLUMNS} FROM {TABLE} WHERE id <= {rows}")
    assert len(cursor.fetchall()) == rows
    yield (
        Scenario("memory", "Peak memory of fetchall()", rows),
        ((_maxrss_bytes() - before) / 2**20),
    )
    conn.close()


AREAS: dict[str, Callable[[str, int, int], Iterable[tuple[Scenario, float]]]] = {
    "dbapi": dbapi,
    "prepared": prepared,
    "sqlalchemy": sqlalchemy,
    "django": django,
    "memory": memory,
    "asyncio": asyncio_dbapi,
    "sqlalchemy-asyncio": sqlalchemy_asyncio,
}


def worker(area: str, driver: str, repeat: int, repeat_large: int) -> None:
    loop = {"loop": LOOP} if area in AREA_DRIVERS else {}
    for scenario, value in AREAS[area](driver, repeat, repeat_large):
        print(
            json.dumps({**scenario.__dict__, "driver": driver, **loop, "value": value}),
            flush=True,
        )


def versions() -> dict[str, str]:
    out: dict[str, str] = {}
    for name, module in (
        ("rustmy", "rustmy"),
        ("mysqlclient", "MySQLdb"),
        ("pymysql", "pymysql"),
        ("mysqlconnector", "mysql.connector"),
        ("asyncmy", "asyncmy"),
        ("aiomysql", "aiomysql"),
        ("uvloop", "uvloop"),
        ("sqlalchemy", "sqlalchemy"),
        ("django", "django"),
    ):
        # asyncmy has no __version__: its distribution's metadata then.
        code = (
            f"import importlib.metadata, {module} as m; "
            "v = getattr(m, '__version__', None) "
            "or ('.'.join(map(str, m.version_info[:3])) "
            "if hasattr(m, 'version_info') "
            f"else importlib.metadata.version({name!r})); print(v)"
        )
        out[name] = subprocess.run(  # noqa: S603  # our own interpreter
            [sys.executable, "-c", code], check=True, capture_output=True, text=True
        ).stdout.strip()
    return out


def run_all(args: argparse.Namespace) -> None:
    import rustmy  # noqa: PLC0415

    conn = rustmy.connect(
        host=HOST,
        port=PORT,
        user=USER,
        password=PASSWORD,
        database=DATABASE,
        autocommit=True,
        ssl_mode="disabled",
    )
    cursor = conn.cursor()
    if not args.keep_tables:
        print("creating 1,000,000 orders...", flush=True)
        for statement in SETUP:
            cursor.execute(statement)
        # The server keeps flushing a large INSERT for a while: let it settle
        # before measuring, or whichever driver runs first looks slower.
        time.sleep(20)
    server = conn.get_server_info()
    conn.close()
    # Every insert goes to the binary log (some 36 million rows in all): left
    # there, it fills the server's tmpfs, which then waits for space without
    # an error. Purged after each driver's run.
    root = rustmy.connect(
        host=HOST, port=PORT, user="root", password=PASSWORD, ssl_mode="disabled"
    )
    root_cursor = root.cursor()
    root_cursor.execute("SELECT @@log_bin")
    binary_log = root_cursor.fetchone() == (1,)
    results: list[dict[str, Any]] = []
    default = "dbapi,asyncio" if args.latency is not None else ",".join(AREAS)
    areas = (args.areas or default).split(",")
    runs = [
        (area, driver, loop)
        for area in areas
        for loop in (args.loops if area in AREA_DRIVERS else ["-"])
        for driver in AREA_DRIVERS.get(area, DRIVERS)
    ]
    env = dict(os.environ)
    latency: dict[str, Any] = {}
    if args.latency is not None:
        # Every worker connects through toxiproxy; the tables were set up
        # directly.
        from pipeline import PROXIES  # noqa: PLC0415
        from pipeline import round_trip  # noqa: PLC0415
        from pipeline import set_latency  # noqa: PLC0415

        listen, proxy = PROXIES[PORT]
        env["RUSTMY_BENCH_PORT"] = str(listen)
        set_latency(proxy, args.latency)
        through = rustmy.connect(
            host=HOST,
            port=listen,
            user=USER,
            password=PASSWORD,
            autocommit=True,
            ssl_mode="disabled",
        )
        latency = {
            "latency_ms": args.latency,
            "round_trip_ms": round(round_trip(through) * 1e3, 3),
        }
        through.close()
        print(f"+{args.latency} ms per reply: {latency['round_trip_ms']} ms round trip")
    try:
        for area, driver, loop in runs:
            results.extend(measure(args, area, driver, loop, env))
            if binary_log:
                root_cursor.execute("PURGE BINARY LOGS BEFORE NOW()")
    finally:
        if latency:
            set_latency(proxy, 0)
    report = {
        "date": dt.datetime.now(dt.timezone.utc).isoformat(timespec="seconds"),
        "machine": f"{platform.system()} {platform.machine()}",
        "python": f"{platform.python_implementation()} {sys.version.split()[0]}",
        "server": server,
        "versions": versions(),
        "repeat": args.repeat,
        "repeat_large": args.repeat_large,
        **latency,
        "results": results,
    }
    args.json.parent.mkdir(parents=True, exist_ok=True)
    args.json.write_text(json.dumps(report, indent=2) + "\n")
    print(f"results written to {args.json}")


def measure(
    args: argparse.Namespace, area: str, driver: str, loop: str, env: dict[str, str]
) -> list[dict[str, Any]]:
    """Runs one area with one driver in a process of its own."""
    print(f"{area} / {driver}" + ("" if loop == "-" else f" / {loop}"), flush=True)
    out = subprocess.run(  # noqa: S603  # our own script
        [
            sys.executable,
            __file__,
            "--worker",
            area,
            driver,
            "--repeat",
            str(args.repeat),
            "--repeat-large",
            str(args.repeat_large),
            *(["--loop", loop] if loop != "-" else []),
        ],
        check=True,
        capture_output=True,
        text=True,
        env=env,
    ).stdout
    records = [json.loads(line) for line in out.splitlines()]
    for record in records:
        unit = "MiB" if area == "memory" else "ms"
        value = record["value"] if area == "memory" else record["value"] * 1000
        print(f"  {record['name']:<44}{record['rows']:>10,}{value:>12.1f} {unit}")
    return records


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--repeat", type=int, default=5)
    parser.add_argument("--repeat-large", type=int, default=3)
    parser.add_argument("--areas", help="comma-separated subset of " + ", ".join(AREAS))
    parser.add_argument("--keep-tables", action="store_true", help="reuse the tables")
    parser.add_argument(
        "--loops",
        type=lambda value: value.split(","),
        default=["asyncio"],
        help="event loops for the asyncio areas: asyncio (default), uvloop or both",
    )
    parser.add_argument("--loop", default="asyncio", help=argparse.SUPPRESS)
    parser.add_argument(
        "--latency",
        type=int,
        metavar="MS",
        help="through toxiproxy, with MS added to every reply (areas dbapi and "
        "asyncio unless --areas)",
    )
    parser.add_argument(
        "--json", type=Path, default=RESULTS / "showcase.json", metavar="PATH"
    )
    parser.add_argument(
        "--worker", nargs=2, metavar=("AREA", "DRIVER"), help=argparse.SUPPRESS
    )
    parser.add_argument(
        "--render",
        type=Path,
        nargs="+",
        metavar="PATH",
        help="draw charts and tables from a run's JSON, and the latency tables "
        "from --latency runs' JSON",
    )
    args = parser.parse_args()
    if args.worker:
        global LOOP  # noqa: PLW0603  # one worker process, one loop
        LOOP = args.loop
        area, driver = args.worker
        worker(area, driver, args.repeat, args.repeat_large)
    elif args.render:
        from showcase_render import render  # noqa: PLC0415

        render(args.render)
    else:
        run_all(args)


if __name__ == "__main__":
    main()
