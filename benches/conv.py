"""Custom converters (``conv=``): rustmy against mysqlclient and PyMySQL, each
starting from its own default ``conversions`` mapping.

    uv run --no-sync python benches/conv.py [--repeat N] [--port 13306] \\
        [--json PATH]

Same setup as ``spike.py`` (see benches/README.md); the JSON has its format.
The Django-style scenario changes a type the table does not have (``TIME``),
so it must cost what no ``conv`` at all costs: only changed entries leave the
native path.
"""

from __future__ import annotations

import argparse
import datetime as dt
import enum
import gc
import json
import platform
import sys
import time

from dataclasses import dataclass
from pathlib import Path
from typing import TYPE_CHECKING
from typing import Any

import MySQLdb
import MySQLdb.converters
import pymysql
import pymysql.converters

import rustmy
import rustmy.converters
import spike

from rustmy.constants import FIELD_TYPE


if TYPE_CHECKING:
    from collections.abc import Callable


ROWS = spike.ROWS
INSERTS = 10_000
#: Pause after creating the tables, as in ``cursors.py``: the server keeps
#: flushing the setup INSERTs in the background for a while.
SETTLE_SECONDS = 15
DEFAULTS: dict[str, dict[Any, Any]] = {
    "rustmy": rustmy.converters.conversions,
    "mysqlclient": MySQLdb.converters.conversions,
    "pymysql": pymysql.converters.conversions,
}


class Color(enum.Enum):
    RED = "red"
    GREEN = "green"


def encode_color(value: Color, conv: object) -> str:
    del conv
    return f"'{value.value}'"


def double(value: bytes | str) -> int:
    return int(value) * 2


@dataclass(frozen=True)
class Scenario:
    name: str
    #: Changes on top of each driver's default mapping (None: no conv=).
    changes: dict[Any, Any] | None
    run: Callable[[Any], None]
    reset: str | None = None


def fetch_all(sql: str) -> Callable[[Any], None]:
    def run(conn: Any) -> None:
        cursor = conn.cursor()
        cursor.execute(sql)
        assert len(cursor.fetchall()) == ROWS

    return run


def insert_colors(conn: Any) -> None:
    rows = [(i, Color.RED if i % 2 else Color.GREEN) for i in range(INSERTS)]
    conn.cursor().executemany("INSERT INTO bench_conv VALUES (%s, %s)", rows)


SCENARIOS = [
    Scenario(
        f"fetchall {ROWS // 1000}k decimal, no conv",
        None,
        fetch_all("SELECT amount FROM bench_mixed"),
    ),
    Scenario(
        f"fetchall {ROWS // 1000}k decimal, conv NEWDECIMAL: float",
        {FIELD_TYPE.NEWDECIMAL: float},
        fetch_all("SELECT amount FROM bench_mixed"),
    ),
    Scenario(
        f"fetchall {ROWS // 1000}k int/varchar, no conv",
        None,
        fetch_all("SELECT * FROM bench_intvarchar"),
    ),
    Scenario(
        f"fetchall {ROWS // 1000}k int/varchar, conv TIME changed (Django)",
        {FIELD_TYPE.TIME: lambda value: value},
        fetch_all("SELECT * FROM bench_intvarchar"),
    ),
    Scenario(
        f"fetchall {ROWS // 1000}k int/varchar, conv LONG: Python function",
        {FIELD_TYPE.LONG: double},
        fetch_all("SELECT * FROM bench_intvarchar"),
    ),
    Scenario(
        f"executemany {INSERTS // 1000}k rows, conv encoder for an enum",
        {Color: encode_color},
        insert_colors,
        reset="TRUNCATE bench_conv",
    ),
]


def connect(driver: str, target: spike.Target, conv: dict[Any, Any] | None) -> Any:
    common: dict[str, Any] = {
        "host": target.host,
        "port": target.port,
        "user": target.user,
        "password": target.password,
        "database": target.database,
        "autocommit": True,
    }
    if conv is not None:
        common["conv"] = conv
    if driver == "rustmy":
        return rustmy.connect(**common, ssl_mode="disabled")
    if driver == "mysqlclient":
        return MySQLdb.connect(**common, ssl_mode="DISABLED")
    return pymysql.connect(**common)


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
    parser.add_argument("--json", type=Path, metavar="PATH")
    args = parser.parse_args()
    target = spike.Target(args.host, args.port)
    drivers = ("rustmy", "mysqlclient", "pymysql")

    with connect("rustmy", target, None) as conn:
        server = conn.get_server_info()
        cursor = conn.cursor()
        for statement in [
            *spike.SETUP,
            "DROP TABLE IF EXISTS bench_conv",
            "CREATE TABLE bench_conv (id INT, color VARCHAR(10))",
        ]:
            cursor.execute(statement)
    time.sleep(SETTLE_SECONDS)
    python = f"{platform.python_implementation()} {sys.version.split()[0]}"
    print(f"server {server}; {python}; best of {args.repeat}\n")
    header = "".join(f"{d:>13}" for d in drivers)
    print(f"{'scenario':<60}{header}   vs mysqlclient")

    results = []
    for scenario in SCENARIOS:
        times = {}
        for driver in drivers:
            conv = (
                None
                if scenario.changes is None
                else {**DEFAULTS[driver], **scenario.changes}
            )
            conn = connect(driver, target, conv)
            times[driver] = best_time(scenario, conn, args.repeat)
            conn.close()
        results.append({"name": scenario.name, "informative": False, "seconds": times})
        cells = "".join(f"{times[d] * 1000:>11.1f}ms" for d in drivers)
        ratio = times["mysqlclient"] / times["rustmy"]
        print(f"{scenario.name:<60}{cells}{ratio:>14.2f}x")

    if args.json:
        report = {
            "date": dt.datetime.now(dt.timezone.utc).isoformat(timespec="seconds"),
            "machine": f"{platform.system()} {platform.machine()}",
            "python": python,
            "server": server,
            "drivers": {
                "rustmy": rustmy.__version__,
                "mysqlclient": ".".join(map(str, MySQLdb.version_info[:3])),
                "pymysql": pymysql.__version__,
            },
            "repeat": args.repeat,
            "scenarios": results,
        }
        args.json.parent.mkdir(parents=True, exist_ok=True)
        args.json.write_text(json.dumps(report, indent=2) + "\n")
        print(f"\nresults written to {args.json}")


if __name__ == "__main__":
    main()
