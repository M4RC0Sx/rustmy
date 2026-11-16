"""SQLAlchemy Core and ORM through rustmy, mysqlclient and PyMySQL.

    uv run --no-sync python benches/orm.py [--repeat N] [--port 13306] \\
        [--json PATH] [--memory]

Same setup as ``spike.py`` (see benches/README.md): release build, MySQL from
compose.yaml, plain TCP for every driver, best of N; the JSON has the format
of ``spike.py`` plus the SQLAlchemy version. Each driver gets its own Engine
(``mysql+rustmy``, ``mysql+mysqldb``, ``mysql+pymysql``), connected before
timing. These numbers include SQLAlchemy's own work, which is the same for
every driver: the ratios are what an application sees, not the driver alone.

With ``--memory``, the peak memory growth of iterating 1M rows as ORM objects
with ``yield_per`` (a server-side cursor), each driver in a fresh process.
"""

from __future__ import annotations

import argparse
import datetime as dt
import gc
import json
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

import MySQLdb
import pymysql
import sqlalchemy as sa

from sqlalchemy import orm
from sqlalchemy.dialects import mysql

import rustmy
import spike


if TYPE_CHECKING:
    from collections.abc import Callable


ROWS = spike.ROWS
OBJECTS = 10_000
GETS = 1_000
INSERTS = 10_000
STREAM_ROWS = 1_000_000
#: Pause after creating the tables, as in ``cursors.py``: the server keeps
#: flushing the setup INSERTs in the background for a while.
SETTLE_SECONDS = 15

URLS = {
    # Plain TCP for everyone: rustmy and libmysqlclient default to TLS.
    "rustmy": "mysql+rustmy://{t.user}:{t.password}@{t.host}:{t.port}/{t.database}"
    "?ssl_mode=disabled",
    "mysqlclient": "mysql+mysqldb://{t.user}:{t.password}@{t.host}:{t.port}/"
    "{t.database}?ssl_mode=DISABLED",
    "pymysql": "mysql+pymysql://{t.user}:{t.password}@{t.host}:{t.port}/{t.database}",
}
DRIVERS = tuple(URLS)


class Base(orm.DeclarativeBase):
    pass


class IntVarchar(Base):
    __tablename__ = "bench_intvarchar"

    id: orm.Mapped[int] = orm.mapped_column(primary_key=True)
    a: orm.Mapped[int]
    b: orm.Mapped[int] = orm.mapped_column(sa.BigInteger)
    c: orm.Mapped[str] = orm.mapped_column(sa.String(32))
    d: orm.Mapped[str | None] = orm.mapped_column(sa.String(64))


class Mixed(Base):
    __tablename__ = "bench_mixed"

    id: orm.Mapped[int] = orm.mapped_column(primary_key=True)
    f: orm.Mapped[float] = orm.mapped_column(sa.Double)
    ts: orm.Mapped[dt.datetime] = orm.mapped_column(mysql.DATETIME(fsp=6))
    d: orm.Mapped[dt.date]
    amount: orm.Mapped[Decimal] = orm.mapped_column(sa.Numeric(12, 2))
    s: orm.Mapped[str] = orm.mapped_column(sa.String(32))


class Inserted(Base):
    __tablename__ = "bench_orm_insert"

    id: orm.Mapped[int] = orm.mapped_column(primary_key=True, autoincrement=False)
    s: orm.Mapped[str] = orm.mapped_column(sa.String(32))
    f: orm.Mapped[float] = orm.mapped_column(sa.Double)


class Streamed(Base):
    __tablename__ = "bench_orm_stream"

    id: orm.Mapped[int] = orm.mapped_column(primary_key=True)
    s: orm.Mapped[str] = orm.mapped_column(sa.String(32))


SETUP = [
    "DROP TABLE IF EXISTS bench_orm_insert",
    "CREATE TABLE bench_orm_insert (id INT PRIMARY KEY, s VARCHAR(32), f DOUBLE)",
]
STREAM_SETUP = [
    "DROP TABLE IF EXISTS bench_orm_stream",
    "CREATE TABLE bench_orm_stream (id INT PRIMARY KEY, s VARCHAR(32) NOT NULL)",
    # 1M rows: the 100k numbers ten times over.
    f"""INSERT INTO bench_orm_stream
        SELECT n + 100000 * k, CONCAT('row-', n)
        FROM ({spike._NUMBERS}) AS numbers,
             (SELECT 0 AS k UNION ALL SELECT 1 UNION ALL SELECT 2 UNION ALL
              SELECT 3 UNION ALL SELECT 4 UNION ALL SELECT 5 UNION ALL
              SELECT 6 UNION ALL SELECT 7 UNION ALL SELECT 8
              UNION ALL SELECT 9) AS k""",  # noqa: SLF001
]


@dataclass(frozen=True)
class Scenario:
    name: str
    run: Callable[[sa.Engine], None]
    #: SQL run before each attempt, outside the timed section.
    reset: str | None = None


def core_fetchall(table: type[Base]) -> Callable[[sa.Engine], None]:
    statement = sa.select(Base.metadata.tables[table.__tablename__])

    def run(engine: sa.Engine) -> None:
        with engine.connect() as conn:
            assert len(conn.execute(statement).all()) == ROWS

    return run


def orm_load(engine: sa.Engine) -> None:
    with orm.Session(engine) as session:
        objects = session.scalars(sa.select(Mixed).limit(OBJECTS)).all()
        assert len(objects) == OBJECTS


def orm_get(engine: sa.Engine) -> None:
    with orm.Session(engine) as session:
        for i in range(GETS):
            assert session.get(IntVarchar, i) is not None


def orm_add_all(engine: sa.Engine) -> None:
    # Explicit ids let the ORM batch the INSERTs (MySQL has no RETURNING).
    with orm.Session(engine) as session:
        session.add_all(Inserted(id=i, s=f"name-{i}", f=i / 7) for i in range(INSERTS))
        session.commit()


_ROWS = [{"id": i, "s": f"name-{i}", "f": i / 7} for i in range(INSERTS)]


def core_insert(engine: sa.Engine) -> None:
    with engine.begin() as conn:
        conn.execute(sa.insert(Base.metadata.tables["bench_orm_insert"]), _ROWS)


def orm_yield_per(engine: sa.Engine) -> None:
    with orm.Session(engine) as session:
        statement = sa.select(IntVarchar).execution_options(yield_per=1000)
        assert sum(1 for _ in session.scalars(statement)) == ROWS


SCENARIOS = [
    Scenario(f"Core fetchall {ROWS // 1000}k int/varchar", core_fetchall(IntVarchar)),
    Scenario(f"Core fetchall {ROWS // 1000}k mixed", core_fetchall(Mixed)),
    Scenario(f"ORM load {OBJECTS // 1000}k objects (mixed)", orm_load),
    Scenario(f"ORM {GETS:,} x session.get", orm_get),
    Scenario(
        f"ORM add_all + commit {INSERTS // 1000}k",
        orm_add_all,
        reset="TRUNCATE bench_orm_insert",
    ),
    Scenario(
        f"Core insert {INSERTS // 1000}k (executemany)",
        core_insert,
        reset="TRUNCATE bench_orm_insert",
    ),
    Scenario(f"ORM yield_per {ROWS // 1000}k objects", orm_yield_per),
]


def engine_for(driver: str, target: spike.Target) -> sa.Engine:
    engine = sa.create_engine(URLS[driver].format(t=target), pool_size=1)
    with engine.connect() as conn:
        conn.exec_driver_sql("SELECT 1")
    return engine


def best_time(scenario: Scenario, engine: sa.Engine, repeat: int) -> float:
    times = []
    for _ in range(repeat):
        if scenario.reset:
            with engine.begin() as conn:
                conn.exec_driver_sql(scenario.reset)
        gc.collect()
        start = time.perf_counter()
        scenario.run(engine)
        times.append(time.perf_counter() - start)
    return min(times)


# Memory --------------------------------------------------------------------


def _maxrss_bytes() -> int:
    peak = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
    # Bytes on macOS, KiB on Linux.
    return peak if sys.platform == "darwin" else peak * 1024


def memory_probe(driver: str, port: int) -> None:
    """Child process: iterate the 1M-row table as ORM objects with
    ``yield_per``, print the growth of the peak RSS in MiB.
    """
    engine = engine_for(driver, spike.Target("127.0.0.1", port))
    gc.collect()
    before = _maxrss_bytes()
    with orm.Session(engine) as session:
        statement = sa.select(Streamed).execution_options(yield_per=1000)
        assert sum(1 for _ in session.scalars(statement)) == STREAM_ROWS
    print(round((_maxrss_bytes() - before) / 2**20, 1))


def memory(port: int) -> list[dict[str, Any]]:
    peak = {}
    for driver in DRIVERS:
        out = subprocess.run(  # noqa: S603  # our own script
            [sys.executable, __file__, "--memory-probe", driver, "--port", str(port)],
            check=True,
            capture_output=True,
            text=True,
        ).stdout
        peak[driver] = float(out.strip())
    cells = "".join(f"{peak[d]:>12.1f} MiB" for d in DRIVERS)
    print(f"ORM yield_per{STREAM_ROWS:>10,} rows{cells}")
    return [{"rows": STREAM_ROWS, "cursor": "ORM yield_per", "peak_mib": peak}]


# Main ------------------------------------------------------------------------


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", type=int, default=13306)
    parser.add_argument("--repeat", type=int, default=5)
    parser.add_argument("--json", type=Path, metavar="PATH")
    parser.add_argument("--memory", action="store_true")
    parser.add_argument("--memory-probe", metavar="DRIVER", help=argparse.SUPPRESS)
    args = parser.parse_args()
    if args.memory_probe:
        memory_probe(args.memory_probe, args.port)
        return
    target = spike.Target(args.host, args.port)

    # A plain connection: the setup SQL has literal % signs, which
    # exec_driver_sql() would take as placeholders (format paramstyle).
    with spike.drivers(target)["rustmy"]() as conn:
        server = conn.get_server_info()
        cursor = conn.cursor()
        stream = STREAM_SETUP if args.memory else []
        for statement in [*spike.SETUP, *SETUP, *stream]:
            cursor.execute(statement)
    time.sleep(SETTLE_SECONDS)
    python = f"{platform.python_implementation()} {sys.version.split()[0]}"
    versions = f"{python}; SQLAlchemy {sa.__version__}"
    print(f"server {server}; {versions}; best of {args.repeat}\n")
    header = "".join(f"{d:>13}" for d in DRIVERS)
    print(f"{'scenario':<40}{header}   vs mysqlclient")

    results = []
    for scenario in SCENARIOS:
        times = {}
        for driver in DRIVERS:
            engine = engine_for(driver, target)
            times[driver] = best_time(scenario, engine, args.repeat)
            engine.dispose()
        results.append({"name": scenario.name, "informative": False, "seconds": times})
        cells = "".join(f"{times[d] * 1000:>11.1f}ms" for d in DRIVERS)
        ratio = times["mysqlclient"] / times["rustmy"]
        print(f"{scenario.name:<40}{cells}{ratio:>14.2f}x")

    report: dict[str, Any] = {
        "date": dt.datetime.now(dt.timezone.utc).isoformat(timespec="seconds"),
        "machine": f"{platform.system()} {platform.machine()}",
        "python": python,
        "sqlalchemy": sa.__version__,
        "server": server,
        "drivers": {
            "rustmy": rustmy.__version__,
            "mysqlclient": ".".join(map(str, MySQLdb.version_info[:3])),
            "pymysql": pymysql.__version__,
        },
        "repeat": args.repeat,
        "scenarios": results,
    }
    if args.memory:
        print()
        report["memory"] = memory(args.port)
    if args.json:
        args.json.parent.mkdir(parents=True, exist_ok=True)
        args.json.write_text(json.dumps(report, indent=2) + "\n")
        print(f"\nresults written to {args.json}")


if __name__ == "__main__":
    main()
