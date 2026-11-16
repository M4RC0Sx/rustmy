"""Connection character sets: reading and writing non-ASCII text in latin1,
cp1251 and gbk with rustmy, mysqlclient and PyMySQL.

    uv run --no-sync python benches/charsets.py [--repeat N] [--port 13306] \\
        [--json PATH]

Same setup as ``spike.py`` (see benches/README.md): release build, MySQL from
compose.yaml, plain TCP for every driver, best of N; the JSON has the format
of ``spike.py``. Text is stored in utf8mb4 columns and the server converts it
to each connection's character set, so every driver decodes the same bytes.
"""

from __future__ import annotations

import argparse
import datetime as dt
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
import pymysql

import rustmy
import spike


if TYPE_CHECKING:
    from collections.abc import Callable


ROWS = spike.ROWS
INSERTS = 10_000
#: Pause after creating the tables, as in ``cursors.py``: the server keeps
#: flushing the setup INSERTs in the background for a while.
SETTLE_SECONDS = 15

#: Mostly-ASCII text with a few accented letters, Cyrillic, and Chinese.
SPANISH = "Señor, ¿quiere un café con leche y una tostada? Año ñandú"
RUSSIAN = "Съешь же ещё этих мягких французских булок да выпей чаю"
CHINESE = "我们的产品质量很好价格也很便宜欢迎您来参观选购谢谢"

SETUP = [
    "DROP TABLE IF EXISTS bench_text, bench_text_insert",
    """CREATE TABLE bench_text (
        id INT PRIMARY KEY, es VARCHAR(100) NOT NULL, ru VARCHAR(100) NOT NULL,
        zh VARCHAR(100) NOT NULL) CHARACTER SET utf8mb4""",
    f"""INSERT INTO bench_text
        SELECT n, CONCAT('{SPANISH} ', n), CONCAT('{RUSSIAN} ', n),
               CONCAT('{CHINESE} ', n)
        FROM ({spike._NUMBERS}) AS numbers""",  # noqa: SLF001
    """CREATE TABLE bench_text_insert (id INT, s VARCHAR(100))
        CHARACTER SET utf8mb4""",
]


@dataclass(frozen=True)
class Scenario:
    name: str
    charset: str
    run: Callable[[Any], None]
    #: SQL run before each attempt, outside the timed section.
    reset: str | None = None


def fetch_all(sql: str) -> Callable[[Any], None]:
    def run(conn: Any) -> None:
        cursor = conn.cursor()
        cursor.execute(sql)
        assert len(cursor.fetchall()) == ROWS

    return run


def executemany(text: str) -> Callable[[Any], None]:
    rows = [(i, f"{text[:60]} {i}") for i in range(INSERTS)]

    def run(conn: Any) -> None:
        conn.cursor().executemany("INSERT INTO bench_text_insert VALUES (%s, %s)", rows)

    return run


SCENARIOS = [
    Scenario(
        f"fetchall {ROWS // 1000}k utf8mb4 (es + ru + zh)",
        "utf8mb4",
        fetch_all("SELECT es, ru, zh FROM bench_text"),
    ),
    Scenario(
        f"fetchall {ROWS // 1000}k latin1 (Spanish)",
        "latin1",
        fetch_all("SELECT es FROM bench_text"),
    ),
    Scenario(
        f"fetchall {ROWS // 1000}k cp1251 (Russian)",
        "cp1251",
        fetch_all("SELECT ru FROM bench_text"),
    ),
    Scenario(
        f"fetchall {ROWS // 1000}k gbk (Chinese)",
        "gbk",
        fetch_all("SELECT zh FROM bench_text"),
    ),
    Scenario(
        f"fetchall {ROWS // 1000}k latin1 (ASCII int/varchar)",
        "latin1",
        fetch_all("SELECT * FROM bench_intvarchar"),
    ),
    Scenario(
        f"executemany {INSERTS // 1000}k latin1 (Spanish)",
        "latin1",
        executemany(SPANISH),
        reset="TRUNCATE bench_text_insert",
    ),
    Scenario(
        f"executemany {INSERTS // 1000}k cp1251 (Russian)",
        "cp1251",
        executemany(RUSSIAN),
        reset="TRUNCATE bench_text_insert",
    ),
    Scenario(
        f"executemany {INSERTS // 1000}k gbk (Chinese)",
        "gbk",
        executemany(CHINESE),
        reset="TRUNCATE bench_text_insert",
    ),
]


def connect(driver: str, target: spike.Target, charset: str) -> Any:
    common: dict[str, Any] = {
        "host": target.host,
        "port": target.port,
        "user": target.user,
        "password": target.password,
        "database": target.database,
        "autocommit": True,
        "charset": charset,
    }
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

    with connect("rustmy", target, "utf8mb4") as conn:
        server = conn.get_server_info()
        cursor = conn.cursor()
        for statement in [*spike.SETUP, *SETUP]:
            cursor.execute(statement)
    time.sleep(SETTLE_SECONDS)
    python = f"{platform.python_implementation()} {sys.version.split()[0]}"
    print(f"server {server}; {python}; best of {args.repeat}\n")
    header = "".join(f"{d:>13}" for d in drivers)
    print(f"{'scenario':<44}{header}   vs mysqlclient")

    results = []
    for scenario in SCENARIOS:
        times = {}
        for driver in drivers:
            conn = connect(driver, target, scenario.charset)
            times[driver] = best_time(scenario, conn, args.repeat)
            conn.close()
        results.append({"name": scenario.name, "informative": False, "seconds": times})
        cells = "".join(f"{times[d] * 1000:>11.1f}ms" for d in drivers)
        ratio = times["mysqlclient"] / times["rustmy"]
        print(f"{scenario.name:<44}{cells}{ratio:>14.2f}x")

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
