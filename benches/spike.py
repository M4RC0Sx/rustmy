"""Phase 0 spike: rustmy vs mysqlclient vs PyMySQL against the same server.

Usage (MySQL from compose.yaml, extension built in release mode):

    docker compose up -d --wait mysql
    PKG_CONFIG_PATH=/opt/homebrew/opt/mysql-client/lib/pkgconfig \\
        uv sync --group bench
    uv run --no-sync maturin develop --uv --release
    uv run --no-sync python benches/spike.py [--repeat N] [--port 13306]

Every scenario runs ``--repeat`` times per driver and keeps the best time
(the least noisy statistic on a shared machine). The figure that matters is
the speedup against mysqlclient measured in the same run, not absolute times.
"""

from __future__ import annotations

import argparse
import datetime as dt
import gc
import json
import platform
import statistics
import sys
import time

from dataclasses import dataclass
from pathlib import Path
from typing import TYPE_CHECKING
from typing import Any

import MySQLdb
import pymysql

import rustmy


if TYPE_CHECKING:
    from collections.abc import Callable


ROWS = 100_000
SMALL_QUERIES = 10_000
INSERTS = 10_000

_DIGITS = " UNION ALL ".join(f"SELECT {d} AS d" for d in range(10))
_NUMBERS = (
    "SELECT t0.d + t1.d * 10 + t2.d * 100 + t3.d * 1000 + t4.d * 10000 AS n FROM "
    + ", ".join(f"({_DIGITS}) AS t{i}" for i in range(5))
)

SETUP = [
    "DROP TABLE IF EXISTS bench_intvarchar, bench_mixed, bench_insert",
    """CREATE TABLE bench_intvarchar (
        id INT PRIMARY KEY, a INT NOT NULL, b BIGINT NOT NULL,
        c VARCHAR(32) NOT NULL, d VARCHAR(64) NULL)""",
    f"""INSERT INTO bench_intvarchar
        SELECT n, (n * 7) % 100000, n * 1000003, CONCAT('name-', n),
               IF(n % 10 = 0, NULL, REPEAT(CHAR(97 + n % 26), 20 + n % 40))
        FROM ({_NUMBERS}) AS numbers""",
    """CREATE TABLE bench_mixed (
        id INT PRIMARY KEY, f DOUBLE NOT NULL, ts DATETIME(6) NOT NULL,
        d DATE NOT NULL, amount DECIMAL(12, 2) NOT NULL, s VARCHAR(32) NOT NULL)""",
    f"""INSERT INTO bench_mixed
        SELECT n, n / 7, TIMESTAMPADD(MICROSECOND, n * 1234567, '2020-01-01'),
               DATE_ADD('2000-01-01', INTERVAL n % 9000 DAY), n / 100,
               CONCAT('s-', n)
        FROM ({_NUMBERS}) AS numbers""",
    "CREATE TABLE bench_insert (id INT, s VARCHAR(32), f DOUBLE)",
]


@dataclass(frozen=True)
class Target:
    host: str
    port: int
    user: str = "rustmy"
    password: str = "rustmy"  # docker test server
    database: str = "rustmy_test"


def drivers(target: Target) -> dict[str, Callable[[], Any]]:
    common: dict[str, Any] = {
        "host": target.host,
        "port": target.port,
        "user": target.user,
        "password": target.password,
        "database": target.database,
        "autocommit": True,
    }
    return {
        "rustmy": lambda: rustmy.connect(**common, ssl_mode="disabled"),
        # Plain TCP for everyone: rustmy and libmysqlclient default to TLS.
        "mysqlclient": lambda: MySQLdb.connect(**common, ssl_mode="DISABLED"),
        "pymysql": lambda: pymysql.connect(**common),
    }


def fetch_all(sql: str) -> Callable[[Any], None]:
    def run(conn: Any) -> None:
        cursor = conn.cursor()
        cursor.execute(sql)
        rows = cursor.fetchall()
        assert len(rows) == ROWS

    return run


def small_queries(conn: Any) -> None:
    cursor = conn.cursor()
    for _ in range(SMALL_QUERIES):
        cursor.execute("SELECT 1")
        cursor.fetchall()


def single_inserts(conn: Any) -> None:
    cursor = conn.cursor()
    for i in range(INSERTS):
        cursor.execute(
            "INSERT INTO bench_insert VALUES (%s, %s, %s)", (i, f"name-{i}", i / 3)
        )


def executemany_insert(conn: Any) -> None:
    cursor = conn.cursor()
    cursor.executemany(
        "INSERT INTO bench_insert VALUES (%s, %s, %s)",
        [(i, f"name-{i}", i / 3) for i in range(INSERTS)],
    )


@dataclass(frozen=True)
class Scenario:
    name: str
    run: Callable[[Any], None]
    #: SQL run before each attempt, outside the timed section.
    reset: str | None = None
    #: Known not to be representative in the spike (see the report note).
    informative: bool = False


SCENARIOS = [
    Scenario(
        f"fetchall {ROWS // 1000}k int/varchar",
        fetch_all("SELECT * FROM bench_intvarchar"),
    ),
    Scenario(
        f"fetchall {ROWS // 1000}k mixed (double/datetime/date/decimal)",
        fetch_all("SELECT * FROM bench_mixed"),
    ),
    Scenario(
        f"fetchall {ROWS // 1000}k datetime + date",
        fetch_all("SELECT ts, d FROM bench_mixed"),
    ),
    Scenario(
        f"fetchall {ROWS // 1000}k decimal",
        fetch_all("SELECT amount FROM bench_mixed"),
    ),
    Scenario(f"{SMALL_QUERIES // 1000}k x SELECT 1", small_queries),
    Scenario(
        f"{INSERTS // 1000}k x INSERT with params",
        single_inserts,
        reset="TRUNCATE bench_insert",
    ),
    Scenario(
        f"executemany {INSERTS // 1000}k rows",
        executemany_insert,
        reset="TRUNCATE bench_insert",
        informative=True,
    ),
]


def best_time(scenario: Scenario, conn: Any, repeat: int) -> float:
    times = []
    for _ in range(repeat):
        if scenario.reset:
            conn.cursor().execute(scenario.reset)
        gc.collect()
        start = time.perf_counter()
        scenario.run(conn)
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
    target = Target(args.host, args.port)
    factories = drivers(target)

    with rustmy.connect(
        ssl_mode="disabled",
        host=target.host,
        port=target.port,
        user=target.user,
        password=target.password,
        database=target.database,
        autocommit=True,
    ) as conn:
        server = conn.get_server_info()
        cursor = conn.cursor()
        for statement in SETUP:
            cursor.execute(statement)

    print(f"server   {server} at {target.host}:{target.port}")
    print(f"python   {platform.python_implementation()} {sys.version.split()[0]}")
    mysqlclient_version = ".".join(map(str, MySQLdb.version_info[:3]))
    print(f"drivers  rustmy {rustmy.__version__}, mysqlclient {mysqlclient_version},")
    print(f"         pymysql {pymysql.__version__}; best of {args.repeat}\n")

    connections = {name: factory() for name, factory in factories.items()}
    header = f"{'scenario':<48}" + "".join(f"{name:>13}" for name in factories)
    print(header + f"{'rustmy speedup':>17}")
    speedups = []
    results = []
    for scenario in SCENARIOS:
        times = {
            name: best_time(scenario, conn, args.repeat)
            for name, conn in connections.items()
        }
        speedup = times["mysqlclient"] / times["rustmy"]
        if not scenario.informative:
            speedups.append(speedup)
        marker = " *" if scenario.informative else ""
        cells = "".join(f"{times[name] * 1000:>11.1f}ms" for name in factories)
        print(f"{scenario.name + marker:<48}{cells}{speedup:>16.2f}x")
        results.append(
            {
                "name": scenario.name,
                "informative": scenario.informative,
                "seconds": times,
            }
        )
    for conn in connections.values():
        conn.close()

    print(f"\ngeometric mean speedup: {statistics.geometric_mean(speedups):.2f}x")
    print(
        "* informative: rustmy's executemany still runs one INSERT per row in the "
        "spike,\n  mysqlclient rewrites it into multi-row INSERTs (Phase 2)."
    )
    if args.json:
        report = {
            "date": dt.datetime.now(dt.timezone.utc).isoformat(timespec="seconds"),
            "machine": f"{platform.system()} {platform.machine()}",
            "python": f"{platform.python_implementation()} {sys.version.split()[0]}",
            "server": server,
            "drivers": {
                "rustmy": rustmy.__version__,
                "mysqlclient": mysqlclient_version,
                "pymysql": pymysql.__version__,
            },
            "repeat": args.repeat,
            "scenarios": results,
        }
        args.json.parent.mkdir(parents=True, exist_ok=True)
        args.json.write_text(json.dumps(report, indent=2) + "\n")
        print(f"results written to {args.json}")


if __name__ == "__main__":
    main()
