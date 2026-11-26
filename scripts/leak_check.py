"""Look for memory leaks: run each path through the driver many times and
check that memory stops growing once warmed up.

    docker compose up -d --wait mysql
    uv run --no-sync python scripts/leak_check.py [--iterations N] [--rounds R]
        [--only NAME,...]

For each scenario: a warm-up round (caches, interned strings, allocator
pools), then ``--rounds`` rounds of ``--iterations`` calls, measuring after
each round the live Python objects (``gc.get_objects``), the memory Python
allocated (``tracemalloc``) and the process's resident memory (RSS, which
also covers what Rust allocates). A leak grows every round; noise does not.
The verdict compares the last round with the first measured one: growth
over ``--objects`` live objects, or over ``--rss-kib`` KiB of RSS, is a
possible leak.

RSS is the only signal for memory Rust allocates, but also the noisiest: the
allocator keeps freed pages, so scenarios that open thousands of connections
grow it by a few hundred bytes per connection without holding anything. To
tell the two apart, run the scenario in a process that stays alive and ask
the system what is still allocated: on macOS ``heap <pid>`` (bytes in use; a
leak makes them grow with the iterations) and ``leaks <pid>`` (blocks
nothing points to); on Linux, valgrind or heaptrack.

Uses the dev build; run it after ``maturin develop --release`` to check the
code that ships.
"""

from __future__ import annotations

import argparse
import asyncio
import contextlib
import datetime as dt
import gc
import os
import subprocess
import sys
import tempfile
import tracemalloc
import warnings

from dataclasses import dataclass
from decimal import Decimal
from pathlib import Path
from typing import TYPE_CHECKING
from typing import Any
from typing import TypeVar

import rustmy
import rustmy.aio
import rustmy.constants
import rustmy.converters

from rustmy.cursors import DictCursor
from rustmy.cursors import PreparedCursor
from rustmy.cursors import SSCursor


if TYPE_CHECKING:
    from collections.abc import Callable
    from collections.abc import Coroutine

    _T = TypeVar("_T")


CERTS = Path(__file__).resolve().parent.parent / "tests" / "docker" / "certs"
BASE: dict[str, Any] = {
    "host": "127.0.0.1",
    "port": 13306,
    "user": "rustmy",
    "password": "rustmy",  # docker test server
    "database": "rustmy_test",
    "autocommit": True,
    "ssl_mode": "disabled",
}
SETUP = [
    "DROP TABLE IF EXISTS leak_rows, leak_target",
    """CREATE TABLE leak_rows (
        id INT PRIMARY KEY, n BIGINT, f DOUBLE, amount DECIMAL(12, 2),
        ts DATETIME(6), d DATE, t TIME(3), s VARCHAR(64), b VARBINARY(32),
        j JSON)""",
    """INSERT INTO leak_rows
        WITH RECURSIVE seq (n) AS (
            SELECT 1 UNION ALL SELECT n + 1 FROM seq WHERE n < 200)
        SELECT n, n * 1000003, n / 7, n / 100,
               TIMESTAMPADD(MICROSECOND, n * 1234567, '2020-01-01'),
               DATE_ADD('2000-01-01', INTERVAL n DAY), SEC_TO_TIME(n * 37.5),
               CONCAT('fila ñ ', n), UNHEX(MD5(n)), JSON_OBJECT('n', n)
        FROM seq""",
    "CREATE TABLE leak_target (id INT PRIMARY KEY, s VARCHAR(64))",
    "INSERT INTO leak_target SELECT id, s FROM leak_rows",
]


def rss_kib() -> int:
    """Resident memory of this process (Linux and macOS)."""
    out = subprocess.run(  # noqa: S603 (a fixed command and our own pid)
        ["ps", "-o", "rss=", "-p", str(os.getpid())],  # noqa: S607
        check=True,
        capture_output=True,
        text=True,
    ).stdout
    return int(out.strip())


@dataclass
class Sample:
    objects: int
    traced: int
    rss: int


def sample() -> Sample:
    gc.collect()
    return Sample(len(gc.get_objects()), tracemalloc.get_traced_memory()[0], rss_kib())


class Scenarios:
    """One method per path; each call does one unit of work."""

    def __init__(self, workdir: Path) -> None:
        self.conn = rustmy.connect(**BASE)
        self.workdir = workdir
        self.csv = workdir / "rows.csv"
        self.csv.write_text("".join(f"{i},row {i}\n" for i in range(200)))
        self.infile = rustmy.connect(**BASE, local_infile_dir=workdir)
        self.zstd = rustmy.connect(**BASE, compress="zstd")
        self.multi = rustmy.connect(**BASE, multi_statements=True)
        self.small_cache = rustmy.connect(**BASE, statement_cache_size=2)
        self.conv = rustmy.connect(
            **BASE,
            conv={
                **rustmy.converters.conversions,
                rustmy.constants.FIELD_TYPE.LONGLONG: lambda value: int(value) + 1,
            },
        )
        self.counter = 0
        cursor = self.conn.cursor()
        for statement in SETUP:
            cursor.execute(statement)
        self.pool = rustmy.Pool(1, 2, **BASE)
        self.loop = asyncio.new_event_loop()
        self.aconn = self.run(rustmy.aio.connect(**BASE))
        self.ainfile = self.run(rustmy.aio.connect(**BASE, local_infile_dir=workdir))
        self.azstd = self.run(rustmy.aio.connect(**BASE, compress="zstd"))
        self.apool = self.run(self._create_pool())

    @staticmethod
    async def _create_pool() -> rustmy.aio.Pool:
        return await rustmy.aio.create_pool(1, 2, **BASE)

    def run(self, coroutine: Coroutine[Any, Any, _T]) -> _T:
        """Run ``coroutine`` on the scenarios' event loop."""
        return self.loop.run_until_complete(coroutine)

    def connect_close(self) -> None:
        rustmy.connect(**BASE).close()

    def connect_dropped(self) -> None:
        # Never closed: garbage collection must free it all.
        conn = rustmy.connect(**BASE)
        conn.cursor().execute("SELECT 1")

    def connect_tls(self) -> None:
        with rustmy.connect(
            **{**BASE, "ssl_mode": "verify_ca", "ssl_ca": CERTS / "ca.pem"}
        ) as conn:
            conn.cursor().execute("SELECT 1")

    def connect_failure(self) -> None:
        with contextlib.suppress(rustmy.OperationalError):
            rustmy.connect(**{**BASE, "password": "wrong"})

    def fetchall(self) -> None:
        cursor = self.conn.cursor()
        cursor.execute("SELECT * FROM leak_rows")
        cursor.fetchall()

    def fetchall_dict(self) -> None:
        cursor = self.conn.cursor(DictCursor)
        cursor.execute("SELECT * FROM leak_rows")
        cursor.fetchall()

    def fetch_iterate(self) -> None:
        cursor = self.conn.cursor()
        cursor.execute("SELECT * FROM leak_rows WHERE id < %s", (50,))
        for _ in cursor:
            pass
        cursor.close()

    def stream_full(self) -> None:
        cursor = self.conn.cursor(SSCursor)
        cursor.execute("SELECT * FROM leak_rows")
        for _ in cursor:
            pass
        cursor.close()

    def stream_abandoned(self) -> None:
        # A streamed result left half read: the next command discards it.
        cursor = self.conn.cursor(SSCursor)
        cursor.execute("SELECT * FROM leak_rows")
        cursor.fetchone()
        self.conn.cursor().execute("SELECT 1")

    def prepared(self) -> None:
        cursor = self.conn.cursor(PreparedCursor)
        cursor.execute("SELECT * FROM leak_rows WHERE id < %s", (100,))
        cursor.fetchall()

    def prepared_evicted(self) -> None:
        # A cache of two and three statements in turn: every execute
        # prepares one and closes another.
        self.counter += 1
        cursor = self.small_cache.cursor(PreparedCursor)
        cursor.execute(
            f"SELECT id + {self.counter % 3} FROM leak_rows WHERE id < %s",  # noqa: S608
            (5,),
        )
        cursor.fetchall()

    def server_error(self) -> None:
        with contextlib.suppress(rustmy.ProgrammingError):
            self.conn.cursor().execute("SELECT * FROM no_such_table")

    def prepared_error(self) -> None:
        with contextlib.suppress(rustmy.Error):
            self.conn.cursor(PreparedCursor).execute(
                "SELECT * FROM no_such_table WHERE id = %s", (1,)
            )

    def parameter_error(self) -> None:
        with contextlib.suppress(rustmy.ProgrammingError):
            self.conn.cursor().execute("SELECT %s, %s", (1,))

    def interpolate_types(self) -> None:
        self.conn.cursor().mogrify(
            "SELECT %s, %s, %s, %s, %s, %s, %s, %s, %s",
            (
                1,
                1.5,
                Decimal("1.25"),
                "ñ'\\",
                b"\x00'",
                dt.date(2026, 1, 1),
                dt.datetime(2026, 1, 1, 1, 1, 1, 5),  # noqa: DTZ001
                dt.timedelta(seconds=-1),
                None,
            ),
        )

    def executemany_batched(self) -> None:
        cursor = self.conn.cursor()
        cursor.execute("DELETE FROM leak_target WHERE id > 1000")
        cursor.executemany(
            "INSERT INTO leak_target VALUES (%s, %s)",
            [(1000 + i, f"new {i}") for i in range(1, 51)],
        )

    def executemany_pipelined(self) -> None:
        self.conn.cursor().executemany(
            "UPDATE leak_target SET s = %s WHERE id = %s",
            [(f"row {i}", i) for i in range(1, 51)],
        )

    def executemany_prepared(self) -> None:
        self.conn.cursor(PreparedCursor).executemany(
            "UPDATE leak_target SET s = %s WHERE id = %s",
            [(f"row {i}", i) for i in range(1, 51)],
        )

    def multi_results(self) -> None:
        cursor = self.multi.cursor()
        cursor.execute("SELECT 1; SELECT * FROM leak_rows WHERE id < 20; DO 1")
        while cursor.nextset():
            if cursor.description is not None:
                cursor.fetchall()

    def custom_conv(self) -> None:
        cursor = self.conv.cursor()
        cursor.execute("SELECT n, ts, amount FROM leak_rows")
        cursor.fetchall()

    def compressed(self) -> None:
        cursor = self.zstd.cursor()
        cursor.execute("SELECT * FROM leak_rows")
        cursor.fetchall()

    def local_infile(self) -> None:
        cursor = self.infile.cursor()
        cursor.execute(
            "CREATE TEMPORARY TABLE IF NOT EXISTS loaded (id INT, s VARCHAR(64))"
        )
        cursor.execute(
            "LOAD DATA LOCAL INFILE %s INTO TABLE loaded FIELDS TERMINATED BY ','",
            (str(self.csv),),
        )
        cursor.execute("TRUNCATE loaded")

    def local_infile_refused(self) -> None:
        with contextlib.suppress(rustmy.OperationalError):
            self.infile.cursor().execute(
                "LOAD DATA LOCAL INFILE '/etc/hosts' INTO TABLE leak_target"
            )

    def read_timeout(self) -> None:
        # The connection breaks: everything it held must go with it.
        conn = rustmy.connect(**BASE, read_timeout=0.05)
        with contextlib.suppress(rustmy.OperationalError):
            conn.cursor().execute("SELECT SLEEP(0.2)")
        conn.close()

    def literal_and_escape(self) -> None:
        self.conn.literal("it's ñ")
        self.conn.escape_string("it's ñ")
        rustmy.converters.literal(dt.datetime(2026, 1, 1))  # noqa: DTZ001

    def pool_acquire(self) -> None:
        with self.pool.acquire() as conn:
            conn.cursor().execute("SELECT 1")

    def pool_rollback(self) -> None:
        # Given back inside a transaction: the pool rolls it back.
        with self.pool.acquire() as conn:
            cursor = conn.cursor()
            cursor.execute("BEGIN")
            cursor.execute("UPDATE leak_target SET s = 'x' WHERE id = 1")

    def pool_timeout(self) -> None:
        with (
            self.pool.acquire(),
            self.pool.acquire(),
            contextlib.suppress(rustmy.PoolTimeout),
            self.pool.acquire(timeout=0),
        ):
            pass

    def pool_open_close(self) -> None:
        with rustmy.Pool(1, 2, **BASE) as pool, pool.acquire() as conn:
            conn.cursor().execute("SELECT 1")

    def aio_connect_close(self) -> None:
        async def go() -> None:
            conn = await rustmy.aio.connect(**BASE)
            await conn.ensure_closed()

        self.run(go())

    def aio_connect_dropped(self) -> None:
        # Never closed: garbage collection must free it all, the transport
        # and its protocol included.
        async def go() -> None:
            conn = await rustmy.aio.connect(**BASE)
            await conn.cursor().execute("SELECT 1")

        self.run(go())

    def aio_connect_tls(self) -> None:
        async def go() -> None:
            conn = await rustmy.aio.connect(
                **{**BASE, "ssl_mode": "verify_ca", "ssl_ca": CERTS / "ca.pem"}
            )
            await conn.cursor().execute("SELECT 1")
            await conn.ensure_closed()

        self.run(go())

    def aio_connect_failure(self) -> None:
        async def go() -> None:
            with contextlib.suppress(rustmy.OperationalError):
                await rustmy.aio.connect(**{**BASE, "password": "wrong"})

        self.run(go())

    def aio_fetchall(self) -> None:
        async def go() -> None:
            async with self.aconn.cursor() as cursor:
                await cursor.execute("SELECT * FROM leak_rows")
                await cursor.fetchall()

        self.run(go())

    def aio_fetchall_dict(self) -> None:
        async def go() -> None:
            async with self.aconn.cursor(rustmy.aio.DictCursor) as cursor:
                await cursor.execute("SELECT * FROM leak_rows")
                await cursor.fetchall()

        self.run(go())

    def aio_stream_full(self) -> None:
        async def go() -> None:
            async with self.aconn.cursor(rustmy.aio.SSCursor) as cursor:
                await cursor.execute("SELECT * FROM leak_rows")
                async for _ in cursor:
                    pass

        self.run(go())

    def aio_stream_abandoned(self) -> None:
        async def go() -> None:
            cursor = self.aconn.cursor(rustmy.aio.SSCursor)
            await cursor.execute("SELECT * FROM leak_rows")
            await cursor.fetchone()
            await self.aconn.cursor().execute("SELECT 1")

        self.run(go())

    def aio_prepared(self) -> None:
        async def go() -> None:
            cursor = self.aconn.cursor(prepared=True)
            await cursor.execute("SELECT * FROM leak_rows WHERE id < %s", (100,))
            await cursor.fetchall()

        self.run(go())

    def aio_server_error(self) -> None:
        async def go() -> None:
            with contextlib.suppress(rustmy.ProgrammingError):
                await self.aconn.cursor().execute("SELECT * FROM no_such_table")

        self.run(go())

    def aio_executemany(self) -> None:
        async def go() -> None:
            cursor = self.aconn.cursor()
            await cursor.execute("DELETE FROM leak_target WHERE id > 1000")
            await cursor.executemany(
                "INSERT INTO leak_target VALUES (%s, %s)",
                [(1000 + i, f"new {i}") for i in range(1, 51)],
            )
            await cursor.executemany(
                "UPDATE leak_target SET s = %s WHERE id = %s",
                [(f"row {i}", i) for i in range(1, 51)],
            )

        self.run(go())

    def aio_compressed(self) -> None:
        async def go() -> None:
            cursor = self.azstd.cursor()
            await cursor.execute("SELECT * FROM leak_rows")
            await cursor.fetchall()

        self.run(go())

    def aio_local_infile(self) -> None:
        async def go() -> None:
            cursor = self.ainfile.cursor()
            await cursor.execute(
                "CREATE TEMPORARY TABLE IF NOT EXISTS loaded (id INT, s VARCHAR(64))"
            )
            await cursor.execute(
                "LOAD DATA LOCAL INFILE %s INTO TABLE loaded FIELDS TERMINATED BY ','",
                (str(self.csv),),
            )
            await cursor.execute("TRUNCATE loaded")

        self.run(go())

    def aio_read_timeout(self) -> None:
        async def go() -> None:
            conn = await rustmy.aio.connect(**BASE, read_timeout=0.05)
            with contextlib.suppress(rustmy.OperationalError):
                await conn.cursor().execute("SELECT SLEEP(0.2)")
            conn.close()

        self.run(go())

    def aio_cancelled(self) -> None:
        # A command whose task is cancelled closes its connection.
        async def go() -> None:
            conn = await rustmy.aio.connect(**BASE)
            task = asyncio.ensure_future(conn.cursor().execute("SELECT SLEEP(0.2)"))
            await asyncio.sleep(0.01)
            task.cancel()
            with contextlib.suppress(asyncio.CancelledError):
                await task
            conn.close()

        self.run(go())

    def aio_pool_acquire(self) -> None:
        async def go() -> None:
            async with self.apool.acquire() as conn:
                await conn.cursor().execute("SELECT 1")

        self.run(go())

    def aio_pool_contention(self) -> None:
        # More tasks than connections: waiters served through futures.
        async def one() -> None:
            async with self.apool.acquire() as conn:
                await conn.cursor().execute("SELECT 1")

        async def go() -> None:
            await asyncio.gather(*(one() for _ in range(6)))

        self.run(go())

    def aio_pool_timeout(self) -> None:
        async def go() -> None:
            async with self.apool.acquire(), self.apool.acquire():
                with contextlib.suppress(rustmy.PoolTimeout):
                    async with self.apool.acquire(timeout=0):
                        pass

        self.run(go())

    def close(self) -> None:
        cursor = self.conn.cursor()
        cursor.execute("DROP TABLE leak_rows, leak_target")
        for conn in (
            self.conn,
            self.infile,
            self.zstd,
            self.multi,
            self.small_cache,
            self.conv,
        ):
            conn.close()
        self.pool.close()
        self.apool.close()
        self.run(self.apool.wait_closed())
        for aconn in (self.aconn, self.ainfile, self.azstd):
            self.run(aconn.ensure_closed())
        self.loop.close()


# Heavier scenarios get fewer iterations per round.
WEIGHT = {
    "connect_close": 0.2,
    "connect_dropped": 0.2,
    "connect_tls": 0.05,
    "connect_failure": 0.1,
    "read_timeout": 0.01,
    "executemany_batched": 0.2,
    "executemany_pipelined": 0.2,
    "executemany_prepared": 0.2,
    "local_infile": 0.2,
    "pool_open_close": 0.1,
    "pool_timeout": 0.5,
    "aio_connect_close": 0.2,
    "aio_connect_dropped": 0.2,
    "aio_connect_tls": 0.05,
    "aio_connect_failure": 0.1,
    "aio_read_timeout": 0.01,
    "aio_cancelled": 0.01,
    "aio_executemany": 0.2,
    "aio_local_infile": 0.2,
    "aio_pool_contention": 0.5,
}


def check(run: Callable[[], None], iterations: int, rounds: int) -> list[Sample]:
    for _ in range(iterations):
        run()
    # Preallocated, so that keeping the samples adds no live objects.
    samples = [Sample(0, 0, 0)] * (rounds + 1)
    samples[0] = sample()
    for i in range(1, rounds + 1):
        for _ in range(iterations):
            run()
        samples[i] = sample()
    return samples


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--iterations", type=int, default=2000)
    parser.add_argument("--rounds", type=int, default=4)
    parser.add_argument("--rss-kib", type=int, default=4096)
    parser.add_argument("--objects", type=int, default=100)
    parser.add_argument("--only", help="comma-separated scenario names")
    args = parser.parse_args()
    # Discarding a half-read stream warns (stream_abandoned): expected here.
    warnings.simplefilter("ignore", UserWarning)
    tracemalloc.start()
    with tempfile.TemporaryDirectory() as tmp:
        scenarios = Scenarios(Path(tmp).resolve())
        names = [
            name
            for name in vars(Scenarios)
            if not name.startswith("_") and name not in ("close", "run")
        ]
        if args.only:
            names = [name for name in names if name in args.only.split(",")]
        print(
            f"{'scenario':<24}{'iterations':>11}{'objects':>22}"
            f"{'traced KiB':>26}{'RSS KiB':>28}  verdict"
        )
        leaks = []
        for name in names:
            iterations = max(10, int(args.iterations * WEIGHT.get(name, 1.0)))
            samples = check(getattr(scenarios, name), iterations, args.rounds)
            # Minus the samples taken so far: each one is a live object.
            objects = [
                s.objects - samples[0].objects - i for i, s in enumerate(samples[1:], 1)
            ]
            traced = [(s.traced - samples[0].traced) // 1024 for s in samples[1:]]
            rss = [s.rss - samples[0].rss for s in samples[1:]]
            leak = rss[-1] > args.rss_kib or objects[-1] > args.objects
            if leak:
                leaks.append(name)
            print(
                f"{name:<24}{iterations * args.rounds:>11}"
                f"{' '.join(map(str, objects)):>22}{' '.join(map(str, traced)):>26}"
                f"{' '.join(map(str, rss)):>28}  {'LEAK?' if leak else 'ok'}",
                flush=True,
            )
        scenarios.close()
    if leaks:
        print(f"\npossible leaks: {', '.join(leaks)}")
        sys.exit(1)


if __name__ == "__main__":
    main()
