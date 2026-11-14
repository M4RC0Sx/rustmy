"""executemany batch-size sweep: rustmy at several ``max_stmt_length`` values
against mysqlclient and PyMySQL with their defaults.

    uv run --no-sync python benches/executemany.py [--repeat N] [--port 13306]

Same setup as ``spike.py`` (see benches/README.md): release build, MySQL from
compose.yaml, plain TCP for every driver, best of N.
"""

from __future__ import annotations

import argparse
import datetime as dt
import gc
import json
import platform
import sys
import time

from pathlib import Path
from typing import Any

import MySQLdb
import pymysql

import rustmy


BATCH_SIZES = {
    "16 KiB": 16 * 1024,
    "64 KiB": 64 * 1024,
    "256 KiB": 256 * 1024,
    "1 MiB": 1024 * 1024,
}
ROW_COUNTS = (10_000, 100_000)
#: Label of rustmy's default ``Cursor.max_stmt_length``.
DEFAULT_BATCH = "256 KiB"
INSERT = "INSERT INTO bench_many (n, s, f, ts) VALUES (%s, %s, %s, %s)"


def rows(count: int) -> list[tuple[Any, ...]]:
    base = dt.datetime(2026, 1, 1)  # noqa: DTZ001
    return [
        (i, f"name-{i}-{'x' * (i % 20)}", i / 7, base + dt.timedelta(seconds=i))
        for i in range(count)
    ]


def best_time(
    conn: Any, data: list[tuple[Any, ...]], repeat: int, size: int | None
) -> float:
    times = []
    for _ in range(repeat):
        conn.cursor().execute("TRUNCATE bench_many")
        cursor = conn.cursor()
        if size is not None:
            cursor.max_stmt_length = size
        gc.collect()
        start = time.perf_counter()
        cursor.executemany(INSERT, data)
        times.append(time.perf_counter() - start)
    return min(times)


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
    connections: dict[str, Any] = {
        "rustmy": rustmy.connect(**common, ssl_mode="disabled"),
        "mysqlclient": MySQLdb.connect(**common, ssl_mode="DISABLED"),
        "pymysql": pymysql.connect(**common),
    }
    setup = connections["rustmy"].cursor()
    setup.execute("DROP TABLE IF EXISTS bench_many")
    setup.execute(
        "CREATE TABLE bench_many (n INT, s VARCHAR(64), f DOUBLE, ts DATETIME(6))"
    )

    print(f"best of {args.repeat}; rows: (int, varchar, double, datetime)\n")
    sweep = []
    for count in ROW_COUNTS:
        data = rows(count)
        seconds = {
            "mysqlclient": best_time(
                connections["mysqlclient"], data, args.repeat, None
            ),
            "pymysql": best_time(connections["pymysql"], data, args.repeat, None),
        }
        for label, size in BATCH_SIZES.items():
            seconds[f"rustmy {label}"] = best_time(
                connections["rustmy"], data, args.repeat, size
            )
        sweep.append({"rows": count, "seconds": seconds})
        print(f"{count:,} rows")
        for config, value in seconds.items():
            ratio = seconds["mysqlclient"] / value
            print(f"  {config:<24}{value * 1000:>9.1f} ms   {ratio:>5.2f}x")
        print()
    if args.json:
        mysqlclient_version = ".".join(map(str, MySQLdb.version_info[:3]))
        report = {
            "date": dt.datetime.now(dt.timezone.utc).isoformat(timespec="seconds"),
            "machine": f"{platform.system()} {platform.machine()}",
            "python": f"{platform.python_implementation()} {sys.version.split()[0]}",
            "server": connections["rustmy"].get_server_info(),
            "drivers": {
                "rustmy": rustmy.__version__,
                "mysqlclient": mysqlclient_version,
                "pymysql": pymysql.__version__,
            },
            "repeat": args.repeat,
            "default": f"rustmy {DEFAULT_BATCH}",
            "sweep": sweep,
        }
        args.json.parent.mkdir(parents=True, exist_ok=True)
        args.json.write_text(json.dumps(report, indent=2) + "\n")
        print(f"results written to {args.json}")
    setup.execute("DROP TABLE bench_many")
    for conn in connections.values():
        conn.close()


if __name__ == "__main__":
    main()
