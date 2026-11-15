"""Cursor benchmark: DictCursor and the streaming SSCursor/SSDictCursor of
rustmy against mysqlclient and PyMySQL.

    uv run --no-sync python benches/cursors.py [--repeat N] [--port 13306] \\
        [--json PATH] [--sweep-json PATH] [--memory]

Same setup as ``spike.py`` (see benches/README.md): release build, MySQL from
compose.yaml, plain TCP for every driver, best of N. Three parts:

- timed scenarios (``--json``, the format of ``spike.py``);
- a sweep of rustmy's streaming batch size against the other drivers
  (``--sweep-json``, the format of ``executemany.py``);
- with ``--memory``, the peak memory of reading a result set with a buffered
  and a streaming cursor, each in a fresh subprocess, stored in ``--json``.
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
from pathlib import Path
from typing import TYPE_CHECKING
from typing import Any

import MySQLdb
import MySQLdb.cursors
import pymysql
import pymysql.cursors

import rustmy
import rustmy.cursors
import spike


if TYPE_CHECKING:
    from collections.abc import Callable


ROWS = spike.ROWS
BIG_ROWS = 1_000_000
INTVARCHAR = "SELECT * FROM bench_intvarchar"
MIXED = "SELECT * FROM bench_mixed"
BIG = "SELECT * FROM bench_stream"
WIDE = f"SELECT id, REPEAT('x', 1000) AS pad FROM bench_stream LIMIT {ROWS}"

_DIGITS = " UNION ALL ".join(f"SELECT {d} AS d" for d in range(10))
_MILLION = (
    "SELECT t0.d + t1.d * 10 + t2.d * 100 + t3.d * 1000 + t4.d * 10000"
    " + t5.d * 100000 AS n FROM " + ", ".join(f"({_DIGITS}) AS t{i}" for i in range(6))
)
SETUP = [
    "DROP TABLE IF EXISTS bench_stream",
    """CREATE TABLE bench_stream (
        id INT PRIMARY KEY, a INT NOT NULL, c VARCHAR(32) NOT NULL)""",
    f"""INSERT INTO bench_stream
        SELECT n, (n * 7) % 100000, CONCAT('name-', n) FROM ({_MILLION}) AS numbers""",
]

#: Cursor classes by driver, under common names.
CURSORS: dict[str, Any] = {
    "rustmy": rustmy.cursors,
    "mysqlclient": MySQLdb.cursors,
    "pymysql": pymysql.cursors,
}

#: Pause after creating the tables: the server keeps flushing the 1M-row
#: INSERT in the background for a while, which slowed down whichever driver
#: was measured first (rustmy) by ~25 %.
SETTLE_SECONDS = 15

BATCH_SIZES = {
    "4 KiB": 4 * 1024,
    "8 KiB": 8 * 1024,
    "16 KiB": 16 * 1024,
    "64 KiB": 64 * 1024,
    "256 KiB": 256 * 1024,
    "1 MiB": 1024 * 1024,
}
#: Label of rustmy's default ``SSCursor._batch_bytes``.
DEFAULT_BATCH = "4 KiB"


@dataclass(frozen=True)
class Scenario:
    name: str
    #: Runs the scenario with a driver's cursor module.
    run: Callable[[Any, Any], None]


def fetchall(kind: str, sql: str, rows: int) -> Callable[[Any, Any], None]:
    def run(conn: Any, cursors: Any) -> None:
        cursor = conn.cursor(getattr(cursors, kind))
        cursor.execute(sql)
        assert len(cursor.fetchall()) == rows
        cursor.close()

    return run


def iterate(kind: str, sql: str, rows: int) -> Callable[[Any, Any], None]:
    def run(conn: Any, cursors: Any) -> None:
        cursor = conn.cursor(getattr(cursors, kind))
        cursor.execute(sql)
        count = 0
        for _ in cursor:
            count += 1
        assert count == rows
        cursor.close()

    return run


def fetchmany_loop(sql: str, rows: int) -> Callable[[Any, Any], None]:
    def run(conn: Any, cursors: Any) -> None:
        cursor = conn.cursor(cursors.SSCursor)
        cursor.execute(sql)
        count = 0
        while batch := cursor.fetchmany(1000):
            count += len(batch)
        assert count == rows
        cursor.close()

    return run


def fetchone_loop(sql: str, rows: int) -> Callable[[Any, Any], None]:
    def run(conn: Any, cursors: Any) -> None:
        cursor = conn.cursor(cursors.SSCursor)
        cursor.execute(sql)
        count = 0
        while cursor.fetchone() is not None:
            count += 1
        assert count == rows
        cursor.close()

    return run


SCENARIOS = [
    Scenario(
        f"Cursor fetchall {ROWS // 1000}k int/varchar",
        fetchall("Cursor", INTVARCHAR, ROWS),
    ),
    Scenario(
        f"DictCursor fetchall {ROWS // 1000}k int/varchar",
        fetchall("DictCursor", INTVARCHAR, ROWS),
    ),
    Scenario(
        f"DictCursor fetchall {ROWS // 1000}k mixed",
        fetchall("DictCursor", MIXED, ROWS),
    ),
    Scenario(
        f"SSCursor iterate {ROWS // 1000}k int/varchar",
        iterate("SSCursor", INTVARCHAR, ROWS),
    ),
    Scenario(
        f"SSCursor iterate {ROWS // 1000}k mixed",
        iterate("SSCursor", MIXED, ROWS),
    ),
    Scenario(
        f"SSCursor fetchmany(1000) {ROWS // 1000}k int/varchar",
        fetchmany_loop(INTVARCHAR, ROWS),
    ),
    Scenario(
        f"SSCursor fetchone {ROWS // 1000}k int/varchar",
        fetchone_loop(INTVARCHAR, ROWS),
    ),
    Scenario(
        f"SSCursor fetchall {ROWS // 1000}k int/varchar",
        fetchall("SSCursor", INTVARCHAR, ROWS),
    ),
    Scenario(
        f"SSDictCursor iterate {ROWS // 1000}k int/varchar",
        iterate("SSDictCursor", INTVARCHAR, ROWS),
    ),
    Scenario(
        f"SSCursor iterate {BIG_ROWS // 1_000_000}M int/varchar",
        iterate("SSCursor", BIG, BIG_ROWS),
    ),
]


def first_row_time(conn: Any, cursors: Any, repeat: int) -> float:
    """Time from ``execute`` to the first row of the 1M-row table; the rest
    is drained outside the timed section.
    """
    times = []
    for _ in range(repeat):
        cursor = conn.cursor(cursors.SSCursor)
        gc.collect()
        start = time.perf_counter()
        cursor.execute(BIG)
        assert cursor.fetchone() is not None
        times.append(time.perf_counter() - start)
        cursor.close()
    return min(times)


def best_time(
    run: Callable[[Any, Any], None], conn: Any, cursors: Any, repeat: int
) -> float:
    times = []
    for _ in range(repeat):
        gc.collect()
        start = time.perf_counter()
        run(conn, cursors)
        times.append(time.perf_counter() - start)
    return min(times)


def timed(run: Callable[[Any, Any], None]) -> Callable[[Any, Any, int], float]:
    def measure(conn: Any, cursors: Any, repeat: int) -> float:
        return best_time(run, conn, cursors, repeat)

    return measure


def with_batch(size: int) -> Any:
    """rustmy's cursor module with SSCursor reading batches of ``size``."""

    class Cursors:
        SSCursor = type("SSCursor", (rustmy.cursors.SSCursor,), {"_batch_bytes": size})

    return Cursors


# Memory --------------------------------------------------------------------


def _maxrss_bytes() -> int:
    peak = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
    # Bytes on macOS, KiB on Linux.
    return peak if sys.platform == "darwin" else peak * 1024


def memory_probe(driver: str, kind: str, rows: int, port: int) -> None:
    """Child process: read ``rows`` rows with a ``kind`` cursor, print the
    growth of the peak RSS in MiB.
    """
    target = spike.Target("127.0.0.1", port)
    conn = spike.drivers(target)[driver]()
    cursor = conn.cursor(getattr(CURSORS[driver], kind))
    gc.collect()
    before = _maxrss_bytes()
    cursor.execute(f"SELECT * FROM bench_stream LIMIT {rows}")
    count = 0
    if kind == "Cursor":
        # Buffered: the whole result is in memory at once, as in fetchall().
        for _ in cursor.fetchall():
            count += 1
    else:
        for _ in cursor:
            count += 1
    assert count == rows
    print(round((_maxrss_bytes() - before) / 2**20, 1))


def memory(port: int) -> list[dict[str, Any]]:
    results = []
    for rows in (ROWS, BIG_ROWS):
        for kind in ("Cursor", "SSCursor"):
            peak = {}
            for driver in CURSORS:
                out = subprocess.run(  # noqa: S603  # our own script
                    [
                        sys.executable,
                        __file__,
                        "--memory-probe",
                        driver,
                        kind,
                        str(rows),
                        "--port",
                        str(port),
                    ],
                    check=True,
                    capture_output=True,
                    text=True,
                ).stdout
                peak[driver] = float(out.strip())
            results.append({"rows": rows, "cursor": kind, "peak_mib": peak})
            cells = "".join(f"{peak[d]:>12.1f} MiB" for d in CURSORS)
            print(f"{kind:<10}{rows:>10,} rows{cells}")
    return results


# Main ------------------------------------------------------------------------


def metadata(server: str, repeat: int) -> dict[str, Any]:
    return {
        "date": dt.datetime.now(dt.timezone.utc).isoformat(timespec="seconds"),
        "machine": f"{platform.system()} {platform.machine()}",
        "python": f"{platform.python_implementation()} {sys.version.split()[0]}",
        "server": server,
        "drivers": {
            "rustmy": rustmy.__version__,
            "mysqlclient": ".".join(map(str, MySQLdb.version_info[:3])),
            "pymysql": pymysql.__version__,
        },
        "repeat": repeat,
    }


def report_line(name: str, times: dict[str, float]) -> None:
    cells = "".join(f"{times[d] * 1000:>11.1f}ms" for d in CURSORS)
    ratio = times["mysqlclient"] / times["rustmy"]
    print(f"{name:<44}{cells}{ratio:>14.2f}x")


def run_scenarios(connections: dict[str, Any], repeat: int) -> list[dict[str, Any]]:
    header = "".join(f"{d:>13}" for d in CURSORS)
    print(f"{'scenario':<44}{header}   vs mysqlclient")
    results = []
    for scenario in SCENARIOS:
        times = {
            d: best_time(scenario.run, connections[d], CURSORS[d], repeat)
            for d in CURSORS
        }
        results.append({"name": scenario.name, "informative": False, "seconds": times})
        report_line(scenario.name, times)
    name = f"SSCursor first row of {BIG_ROWS // 1_000_000}M"
    times = {d: first_row_time(connections[d], CURSORS[d], repeat) for d in CURSORS}
    results.append({"name": name, "informative": False, "seconds": times})
    report_line(name, times)
    return results


def run_sweep(connections: dict[str, Any], repeat: int) -> list[dict[str, Any]]:
    """Iteration of narrow, mixed and ~1 KiB rows (where a small batch holds
    few rows), and the time to the first row, per batch size.
    """
    panels: list[tuple[str, Callable[[Any, Any, int], float]]] = [
        (f"iterate {ROWS // 1000}k {label}", timed(iterate("SSCursor", sql, ROWS)))
        for label, sql in (
            ("int/varchar", INTVARCHAR),
            ("mixed", MIXED),
            ("1 KiB", WIDE),
        )
    ]
    panels.append((f"first row of {BIG_ROWS // 1_000_000}M", first_row_time))
    sweep = []
    for title, measure in panels:
        seconds = {
            d: measure(connections[d], CURSORS[d], repeat)
            for d in ("mysqlclient", "pymysql")
        }
        for size_label, size in BATCH_SIZES.items():
            seconds[f"rustmy {size_label}"] = measure(
                connections["rustmy"], with_batch(size), repeat
            )
        sweep.append({"rows": ROWS, "label": title, "seconds": seconds})
        print(f"\nbatch sweep, SSCursor {title}")
        for config, value in seconds.items():
            ratio = seconds["mysqlclient"] / value
            print(f"  {config:<24}{value * 1000:>9.1f} ms   {ratio:>5.2f}x")
    return sweep


def write_json(path: Path, report: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(report, indent=2) + "\n")
    print(f"written to {path}")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", type=int, default=13306)
    parser.add_argument("--repeat", type=int, default=5)
    parser.add_argument("--json", type=Path, metavar="PATH")
    parser.add_argument("--sweep-json", type=Path, metavar="PATH")
    parser.add_argument("--memory", action="store_true")
    parser.add_argument("--memory-probe", nargs=3, help=argparse.SUPPRESS)
    args = parser.parse_args()
    if args.memory_probe:
        driver, kind, rows = args.memory_probe
        memory_probe(driver, kind, int(rows), args.port)
        return

    factories = spike.drivers(spike.Target(args.host, args.port))
    connections = {name: factory() for name, factory in factories.items()}
    setup = connections["rustmy"].cursor()
    for statement in [*spike.SETUP, *SETUP]:
        setup.execute(statement)
    time.sleep(SETTLE_SECONDS)
    meta = metadata(connections["rustmy"].get_server_info(), args.repeat)
    print(f"server {meta['server']}; {meta['python']}; best of {args.repeat}\n")

    results = run_scenarios(connections, args.repeat)
    sweep = run_sweep(connections, args.repeat)
    for conn in connections.values():
        conn.close()
    report: dict[str, Any] = {**meta, "scenarios": results}
    if args.memory:
        print("\npeak memory growth while reading the result (fresh process each)")
        report["memory"] = memory(args.port)

    if args.json:
        write_json(args.json, report)
    if args.sweep_json:
        write_json(
            args.sweep_json,
            {
                **meta,
                "title": "SSCursor batch size: throughput relative to mysqlclient",
                "labels": {
                    "mysqlclient": "mysqlclient (row by row)",
                    "pymysql": "PyMySQL (row by row)",
                },
                "default": f"rustmy {DEFAULT_BATCH}",
                "sweep": sweep,
            },
        )


if __name__ == "__main__":
    main()
