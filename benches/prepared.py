"""Prepared statements: rustmy's PreparedCursor against its Cursor,
mysqlclient and PyMySQL.

    uv run --no-sync python benches/prepared.py [--repeat N] [--port 13306]

Same setup as ``spike.py`` (see benches/README.md): release build, MySQL from
compose.yaml, plain TCP and ``autocommit=True`` for every driver, best of N.
Every scenario passes its values as parameters (``%s``), which is what a
prepared statement needs; the other drivers interpolate them.
"""

from __future__ import annotations

import argparse
import datetime as dt
import gc
import json
import platform
import sys
import time

from decimal import Decimal
from pathlib import Path
from typing import TYPE_CHECKING
from typing import Any

import MySQLdb
import pymysql

import rustmy


if TYPE_CHECKING:
    from collections.abc import Callable


COLUMNS = "id, n, s, d, ts, dt, dec_"
CREATE = (
    "CREATE TABLE {table} (id INT PRIMARY KEY, n BIGINT, s VARCHAR(32), "
    "d DOUBLE, ts DATETIME(6), dt DATE, dec_ DECIMAL(12, 2))"
)
INSERT = "INSERT INTO {table} VALUES (%s, %s, %s, %s, %s, %s, %s)"
ROWS = 100_000
LOOKUPS = 10_000
BATCH = 10_000


def rows(count: int) -> list[tuple[Any, ...]]:
    base = dt.datetime(2026, 1, 1)  # noqa: DTZ001
    return [
        (
            i,
            i * 1_000_003,
            f"name-{i}",
            i / 7,
            base + dt.timedelta(seconds=i),
            (base + dt.timedelta(days=i % 365)).date(),
            Decimal(i) / 3,
        )
        for i in range(count)
    ]


def best_time(run: Callable[[], object], repeat: int) -> float:
    times = []
    for _ in range(repeat):
        gc.collect()
        start = time.perf_counter()
        run()
        times.append(time.perf_counter() - start)
    return min(times)


def scenarios(
    cursor: Any, data: list[tuple[Any, ...]]
) -> dict[str, Callable[[], object]]:
    def fetchall() -> object:
        cursor.execute(f"SELECT {COLUMNS} FROM bench_ps WHERE id >= %s", (0,))
        return cursor.fetchall()

    def lookups() -> None:
        for i in range(LOOKUPS):
            cursor.execute(f"SELECT {COLUMNS} FROM bench_ps WHERE id = %s", (i,))
            cursor.fetchall()

    def insert() -> None:
        cursor.execute("TRUNCATE bench_ps_insert")
        cursor.executemany(INSERT.format(table="bench_ps_insert"), data[:BATCH])

    def update() -> None:
        cursor.executemany(
            "UPDATE bench_ps SET n = n + 1 WHERE id = %s", [(i,) for i in range(BATCH)]
        )

    return {
        f"fetchall {ROWS // 1000}k mixed rows": fetchall,
        f"{LOOKUPS // 1000}k SELECT by primary key": lookups,
        f"executemany INSERT {BATCH // 1000}k rows": insert,
        f"executemany UPDATE {BATCH // 1000}k rows": update,
    }


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", type=int, default=13306)
    parser.add_argument("--repeat", type=int, default=5)
    parser.add_argument(
        "--json",
        type=Path,
        metavar="PATH",
        help="also write the results as JSON (input of benches/report.py)",
    )
    args = parser.parse_args()
    common: dict[str, Any] = {
        "host": args.host,
        "port": args.port,
        "user": "rustmy",
        "password": "rustmy",  # docker test server
        "database": "rustmy_test",
        "autocommit": True,
    }
    rustmy_conn = rustmy.connect(**common, ssl_mode="disabled")
    cursors: dict[str, Any] = {
        "mysqlclient": MySQLdb.connect(**common, ssl_mode="DISABLED").cursor(),
        "pymysql": pymysql.connect(**common).cursor(),
        "rustmy Cursor": rustmy_conn.cursor(),
        "rustmy PreparedCursor": rustmy_conn.cursor(prepared=True),
    }
    setup = rustmy_conn.cursor()
    for table in ("bench_ps", "bench_ps_insert"):
        setup.execute(f"DROP TABLE IF EXISTS {table}")
        setup.execute(CREATE.format(table=table))
    data = rows(ROWS)
    setup.executemany(INSERT.format(table="bench_ps"), data)
    # The server keeps flushing a large INSERT for a while (see README).
    time.sleep(15)

    print(f"best of {args.repeat}\n")
    panels = []
    names = list(scenarios(setup, data))
    for name in names:
        seconds = {
            config: best_time(scenarios(cursor, data)[name], args.repeat)
            for config, cursor in cursors.items()
        }
        panels.append({"label": name, "seconds": seconds})
        print(name)
        for config, value in seconds.items():
            ratio = seconds["mysqlclient"] / value
            print(f"  {config:<24}{value * 1000:>9.1f} ms   {ratio:>5.2f}x")
    if args.json:
        mysqlclient_version = ".".join(map(str, MySQLdb.version_info[:3]))
        report = {
            "date": dt.datetime.now(dt.timezone.utc).isoformat(timespec="seconds"),
            "machine": f"{platform.system()} {platform.machine()}",
            "python": f"{platform.python_implementation()} {sys.version.split()[0]}",
            "server": rustmy_conn.get_server_info(),
            "drivers": {
                "rustmy": rustmy.__version__,
                "mysqlclient": mysqlclient_version,
                "pymysql": pymysql.__version__,
            },
            "repeat": args.repeat,
            "title": "Prepared statements: throughput relative to mysqlclient",
            "default": "rustmy Cursor",
            "labels": {"mysqlclient": "mysqlclient", "pymysql": "PyMySQL"},
            "sweep": panels,
        }
        args.json.parent.mkdir(parents=True, exist_ok=True)
        args.json.write_text(json.dumps(report, indent=2) + "\n")
        print(f"results written to {args.json}")
    for table in ("bench_ps", "bench_ps_insert"):
        setup.execute(f"DROP TABLE {table}")


if __name__ == "__main__":
    main()
