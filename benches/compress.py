"""The compressed protocol: rustmy uncompressed, zlib and zstd against
mysqlclient with and without ``compress=True``, on a local link and on
links of limited bandwidth.

    docker compose --profile bench up -d --wait mysql toxiproxy
    uv run --no-sync python benches/compress.py [--repeat N] [--bandwidth KBPS]

Same rules as ``spike.py`` (see benches/README.md): release build, plain TCP
for every driver, ``autocommit=True``, best of N. Without ``--bandwidth`` the
drivers connect to the server directly; with it, through the ``toxiproxy``
compose service (profile ``bench``), whose bandwidth toxic caps both
directions at that many KB/s. PyMySQL has no compressed protocol.
"""

from __future__ import annotations

import argparse
import datetime as dt
import gc
import json
import platform
import sys
import time
import urllib.request

from decimal import Decimal
from pathlib import Path
from typing import TYPE_CHECKING
from typing import Any

import MySQLdb

import rustmy


if TYPE_CHECKING:
    from collections.abc import Callable


#: toxiproxy's proxies: (listen port, proxy name) per server port.
PROXIES = {13306: (23306, "mysql"), 13307: (23307, "mariadb")}
TOXIPROXY_API = "http://127.0.0.1:8474"

COLUMNS = "id, n, s, d, ts, dt, dec_"
CREATE = (
    "CREATE TABLE {table} (id INT PRIMARY KEY, n BIGINT, s VARCHAR(32), "
    "d DOUBLE, ts DATETIME(6), dt DATE, dec_ DECIMAL(12, 2))"
)
INSERT = "INSERT INTO {table} VALUES (%s, %s, %s, %s, %s, %s, %s)"
ROWS = 100_000
LOOKUPS = 1_000
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


def toxiproxy(method: str, path: str, body: dict[str, Any] | None = None) -> None:
    request = urllib.request.Request(  # noqa: S310  # fixed local URL
        TOXIPROXY_API + path,
        data=None if body is None else json.dumps(body).encode(),
        method=method,
    )
    try:
        urllib.request.urlopen(request).close()  # noqa: S310
    except urllib.error.HTTPError as err:
        if not (method == "DELETE" and err.code == 404):
            raise


def set_bandwidth(proxy: str, kbps: int) -> None:
    """Caps each direction of ``proxy`` at ``kbps`` KB/s (0 lifts it)."""
    for stream in ("downstream", "upstream"):
        name = f"bandwidth_{stream}"
        toxiproxy("DELETE", f"/proxies/{proxy}/toxics/{name}")
        if kbps:
            toxiproxy(
                "POST",
                f"/proxies/{proxy}/toxics",
                {
                    "name": name,
                    "type": "bandwidth",
                    "stream": stream,
                    "attributes": {"rate": kbps},
                },
            )


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
        cursor.execute(f"SELECT {COLUMNS} FROM bench_zip")
        return cursor.fetchall()

    def lookups() -> None:
        for i in range(LOOKUPS):
            cursor.execute(f"SELECT {COLUMNS} FROM bench_zip WHERE id = %s", (i,))
            cursor.fetchall()

    def insert() -> None:
        cursor.execute("TRUNCATE bench_zip_insert")
        cursor.executemany(INSERT.format(table="bench_zip_insert"), data[:BATCH])

    return {
        f"fetchall {ROWS // 1000}k mixed rows": fetchall,
        f"{LOOKUPS:,} SELECT by primary key": lookups,
        f"executemany INSERT {BATCH // 1000}k rows": insert,
    }


def main() -> None:
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
        "(default: connect directly, no cap)",
    )
    parser.add_argument(
        "--json",
        type=Path,
        metavar="PATH",
        help="also write the results as JSON (input of benches/report.py)",
    )
    args = parser.parse_args()
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
    for table in ("bench_zip", "bench_zip_insert"):
        setup.execute(f"DROP TABLE IF EXISTS {table}")
        setup.execute(CREATE.format(table=table))
    data = rows(ROWS)
    setup.executemany(INSERT.format(table="bench_zip"), data)
    # The server keeps flushing a large INSERT for a while (see README).
    time.sleep(15)

    connections: dict[str, Any] = {
        "mysqlclient": MySQLdb.connect(**measured, ssl_mode="DISABLED"),
        "mysqlclient zlib": MySQLdb.connect(
            **measured, ssl_mode="DISABLED", compress=1
        ),
        "rustmy": rustmy.connect(**measured, ssl_mode="disabled"),
        "rustmy zlib": rustmy.connect(**measured, ssl_mode="disabled", compress="zlib"),
    }
    zstd = rustmy.connect(**measured, ssl_mode="disabled", compress="zstd")
    if zstd.compression == "zstd":
        connections["rustmy zstd"] = zstd
    else:
        zstd.close()

    if args.bandwidth:
        mbit = args.bandwidth * 8 / 1000
        title = f"{mbit:g} Mbit/s each way"
    else:
        title = "local"
    panels = []
    try:
        set_bandwidth(proxy, args.bandwidth)
        print(f"best of {args.repeat}, {title}\n")
        for name in scenarios(setup, data):
            seconds = {
                config: best_time(scenarios(conn.cursor(), data)[name], args.repeat)
                for config, conn in connections.items()
            }
            panels.append({"label": name, "seconds": seconds})
            print(name)
            for config, value in seconds.items():
                ratio = seconds["mysqlclient"] / value
                print(f"  {config:<20}{value * 1000:>10.1f} ms   {ratio:>6.2f}x")
    finally:
        set_bandwidth(proxy, 0)
    if args.json:
        mysqlclient_version = ".".join(map(str, MySQLdb.version_info[:3]))
        report = {
            "date": dt.datetime.now(dt.timezone.utc).isoformat(timespec="seconds"),
            "machine": f"{platform.system()} {platform.machine()}",
            "python": f"{platform.python_implementation()} {sys.version.split()[0]}",
            "server": setup_conn.get_server_info(),
            "drivers": {
                "rustmy": rustmy.__version__,
                "mysqlclient": mysqlclient_version,
            },
            "repeat": args.repeat,
            "title": f"Compression, {title}: throughput relative to mysqlclient",
            "default": "rustmy",
            "labels": {"mysqlclient": "mysqlclient"},
            "bandwidth_kbps": args.bandwidth,
            "sweep": panels,
        }
        args.json.parent.mkdir(parents=True, exist_ok=True)
        args.json.write_text(json.dumps(report, indent=2) + "\n")
        print(f"results written to {args.json}")
    for table in ("bench_zip", "bench_zip_insert"):
        setup.execute(f"DROP TABLE {table}")
    for conn in connections.values():
        conn.close()


if __name__ == "__main__":
    main()
