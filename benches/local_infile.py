"""``LOAD DATA LOCAL INFILE``: rustmy against mysqlclient and PyMySQL, on a
local link and, compressed too, on a link of limited bandwidth.

    docker compose --profile bench up -d --wait mysql toxiproxy
    uv run --no-sync python benches/local_infile.py [--repeat N] [--bandwidth KBPS]

Same rules as ``spike.py`` (see benches/README.md): release build, plain TCP
for every driver, ``autocommit=True``, best of N. One CSV file of a million
rows is loaded into a ``BLACKHOLE`` table (the server parses the rows and
drops them: what is left is the client sending the file and the protocol)
and into an InnoDB one, and a small one 100 times. ``--bandwidth`` goes
through toxiproxy as in ``compress.py``, adding the compressed
configurations.
"""

from __future__ import annotations

import argparse
import datetime as dt
import gc
import json
import platform
import sys
import tempfile
import time

from pathlib import Path
from typing import TYPE_CHECKING
from typing import Any

import MySQLdb
import pymysql

import rustmy

from compress import PROXIES
from compress import set_bandwidth


if TYPE_CHECKING:
    from collections.abc import Callable


CREATE = "CREATE TABLE {table} (id INT, n BIGINT, s VARCHAR(32), d DOUBLE, dt DATE)"
LOAD = "LOAD DATA LOCAL INFILE '{path}' INTO TABLE {table} FIELDS TERMINATED BY ','"
LARGE = 1_000_000
SMALL = 1_000
SMALL_LOADS = 100


def write_csv(path: Path, count: int) -> Path:
    base = dt.date(2026, 1, 1)
    with path.open("w") as out:
        for i in range(count):
            day = base + dt.timedelta(days=i % 365)
            out.write(f"{i},{i * 1_000_003},name-{i},{i / 7},{day}\n")
    return path


def best_time(run: Callable[[], object], repeat: int) -> float:
    times = []
    for _ in range(repeat):
        gc.collect()
        start = time.perf_counter()
        run()
        times.append(time.perf_counter() - start)
    return min(times)


def scenarios(cursor: Any, large: Path, small: Path) -> dict[str, Callable[[], object]]:
    def blackhole() -> None:
        cursor.execute(LOAD.format(path=large, table="bench_infile_hole"))

    def innodb() -> None:
        cursor.execute("TRUNCATE bench_infile")
        cursor.execute(LOAD.format(path=large, table="bench_infile"))

    def small_loads() -> None:
        for _ in range(SMALL_LOADS):
            cursor.execute(LOAD.format(path=small, table="bench_infile_hole"))

    return {
        f"{LARGE // 1_000_000}M rows into BLACKHOLE": blackhole,
        f"{LARGE // 1_000_000}M rows into InnoDB": innodb,
        f"{SMALL_LOADS} loads of {SMALL:,} rows": small_loads,
    }


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", type=int, default=13306, choices=sorted(PROXIES))
    parser.add_argument("--repeat", type=int, default=5)
    parser.add_argument(
        "--bandwidth",
        type=int,
        default=0,
        metavar="KBPS",
        help="cap each direction at this many KB/s through toxiproxy "
        "and add the compressed configurations (default: no cap)",
    )
    parser.add_argument(
        "--json",
        type=Path,
        metavar="PATH",
        help="also write the results as JSON (input of benches/report.py)",
    )
    return parser.parse_args()


def open_connections(
    measured: dict[str, Any], workdir: Path, *, compressed: bool
) -> dict[str, Any]:
    mysqlclient = {**measured, "ssl_mode": "DISABLED", "local_infile_dir": str(workdir)}
    ours = {**measured, "ssl_mode": "disabled", "local_infile_dir": workdir}
    connections: dict[str, Any] = {
        "mysqlclient": MySQLdb.connect(**mysqlclient),
        "pymysql": pymysql.connect(**measured, local_infile=True),
        "rustmy": rustmy.connect(**ours),
    }
    if compressed:
        connections["mysqlclient zlib"] = MySQLdb.connect(**mysqlclient, compress=1)
        connections["rustmy zlib"] = rustmy.connect(**ours, compress="zlib")
        zstd = rustmy.connect(**ours, compress="zstd")
        if zstd.compression == "zstd":
            connections["rustmy zstd"] = zstd
        else:
            zstd.close()
    return connections


def binary_log_purger(direct: dict[str, Any]) -> Callable[[], None]:
    """Every load goes to the binary log, BLACKHOLE included (some 50 MB per
    million rows): left there, a few runs fill the server's tmpfs and it then
    waits for space without an error. Purged after each measurement."""
    root = rustmy.connect(
        **{**direct, "user": "root", "database": None}, ssl_mode="disabled"
    ).cursor()
    root.execute("SELECT @@log_bin")
    enabled = root.fetchone() == (1,)

    def purge() -> None:
        if enabled:
            root.execute("PURGE BINARY LOGS BEFORE NOW()")

    return purge


def main() -> None:
    args = parse_args()
    workdir = Path(tempfile.mkdtemp(prefix="rustmy-bench-infile-")).resolve()
    large = write_csv(workdir / "large.csv", LARGE)
    small = write_csv(workdir / "small.csv", SMALL)
    direct: dict[str, Any] = {
        "host": args.host,
        "port": args.port,
        "user": "rustmy",
        "password": "rustmy",  # docker test server
        "database": "rustmy_test",
        "autocommit": True,
    }
    listen, proxy = PROXIES[args.port]
    measured = {**direct, "port": listen} if args.bandwidth else direct
    setup_conn = rustmy.connect(**direct, ssl_mode="disabled")
    setup = setup_conn.cursor()
    purge = binary_log_purger(direct)
    tables = (("bench_infile", "InnoDB"), ("bench_infile_hole", "BLACKHOLE"))
    for table, engine in tables:
        setup.execute(f"DROP TABLE IF EXISTS {table}")
        setup.execute(CREATE.format(table=table) + f" ENGINE={engine}")
    connections = open_connections(measured, workdir, compressed=bool(args.bandwidth))
    if args.bandwidth:
        title = f"{args.bandwidth * 8 / 1000:g} Mbit/s each way"
    else:
        title = "local"
    panels = []
    try:
        set_bandwidth(proxy, args.bandwidth)
        print(f"best of {args.repeat}, {title}\n")
        for name in scenarios(setup, large, small):
            seconds: dict[str, float] = {}
            for config, conn in connections.items():
                run = scenarios(conn.cursor(), large, small)[name]
                seconds[config] = best_time(run, args.repeat)
                purge()
            panels.append({"label": name, "seconds": seconds})
            print(name)
            for config, value in seconds.items():
                ratio = seconds["mysqlclient"] / value
                print(f"  {config:<20}{value * 1000:>10.1f} ms   {ratio:>6.2f}x")
    finally:
        set_bandwidth(proxy, 0)
    if args.json:
        write_report(args, setup_conn.get_server_info(), title, panels)
    for table, _ in tables:
        setup.execute(f"DROP TABLE {table}")
    for conn in (*connections.values(), setup_conn):
        conn.close()
    for path in (large, small):
        path.unlink()
    workdir.rmdir()


def write_report(
    args: argparse.Namespace, server: str, title: str, panels: list[Any]
) -> None:
    report = {
        "date": dt.datetime.now(dt.timezone.utc).isoformat(timespec="seconds"),
        "machine": f"{platform.system()} {platform.machine()}",
        "python": f"{platform.python_implementation()} {sys.version.split()[0]}",
        "server": server,
        "drivers": {
            "rustmy": rustmy.__version__,
            "mysqlclient": ".".join(map(str, MySQLdb.version_info[:3])),
            "pymysql": pymysql.__version__,
        },
        "repeat": args.repeat,
        "title": f"LOAD DATA LOCAL INFILE, {title}: throughput relative to mysqlclient",
        "default": "rustmy",
        "labels": {"mysqlclient": "mysqlclient", "pymysql": "PyMySQL"},
        "bandwidth_kbps": args.bandwidth,
        "sweep": panels,
    }
    args.json.parent.mkdir(parents=True, exist_ok=True)
    args.json.write_text(json.dumps(report, indent=2) + "\n")
    print(f"results written to {args.json}")


if __name__ == "__main__":
    main()
