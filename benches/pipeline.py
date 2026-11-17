"""executemany of statements that are not INSERT … VALUES (UPDATE, DELETE),
against mysqlclient and PyMySQL, with and without added network latency.

    docker compose --profile bench up -d --wait mysql toxiproxy
    uv run --no-sync python benches/pipeline.py [--repeat N] [--latency MS]

Same rules as ``spike.py`` (see benches/README.md): release build, plain TCP
for every driver, ``autocommit=True``, best of N. Latency comes from the
``toxiproxy`` compose service (profile ``bench``): the script sets a latency
toxic on the server's proxy through its API and connects through the proxy,
also with 0 ms, so both runs pay for the same extra hop. ``--sweep`` adds
rustmy at several ``Cursor.max_pipeline`` values.
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

from pathlib import Path
from typing import Any

import MySQLdb
import pymysql

import rustmy


#: toxiproxy's proxies: (listen port, proxy name) per server port.
PROXIES = {13306: (23306, "mysql"), 13307: (23307, "mariadb")}
TOXIPROXY_API = "http://127.0.0.1:8474"
SWEEP = (1, 16, 64, 256, 1024)

CREATE = "CREATE TABLE bench_pipe (id INT PRIMARY KEY, v INT, s VARCHAR(64))"
FILL = "INSERT INTO bench_pipe (id, v, s) VALUES (%s, %s, %s)"
#: name -> (statement, parameters for row ``i``); every scenario runs on a
#: freshly filled table of the same size.
SCENARIOS: dict[str, tuple[str, Any]] = {
    "UPDATE by primary key": (
        "UPDATE bench_pipe SET v = v + 1, s = %(s)s WHERE id = %(id)s",
        lambda i: {"id": i, "s": f"updated-{i}"},
    ),
    "UPDATE matching no row": (
        "UPDATE bench_pipe SET v = v + 1 WHERE id = %(id)s",
        lambda i: {"id": -1 - i},
    ),
    "DELETE by primary key": (
        "DELETE FROM bench_pipe WHERE id = %s",
        lambda i: (i,),
    ),
}


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


def set_latency(proxy: str, ms: int) -> None:
    toxiproxy("DELETE", f"/proxies/{proxy}/toxics/latency")
    if ms:
        toxiproxy(
            "POST",
            f"/proxies/{proxy}/toxics",
            {
                "name": "latency",
                "type": "latency",
                "stream": "downstream",
                "attributes": {"latency": ms},
            },
        )


def round_trip(conn: Any, repeat: int = 50) -> float:
    cursor = conn.cursor()
    start = time.perf_counter()
    for _ in range(repeat):
        cursor.execute("SELECT 1")
        cursor.fetchall()
    return (time.perf_counter() - start) / repeat


def best_time(
    conn: Any,
    statement: str,
    params: list[Any],
    *,
    rows: int,
    repeat: int,
    window: int | None,
) -> float:
    times = []
    for _ in range(repeat):
        setup = conn.cursor()
        setup.execute("TRUNCATE bench_pipe")
        setup.executemany(FILL, [(i, 0, f"row-{i}") for i in range(rows)])
        cursor = conn.cursor()
        if window is not None:
            cursor.max_pipeline = window
        gc.collect()
        start = time.perf_counter()
        cursor.executemany(statement, params)
        times.append(time.perf_counter() - start)
    return min(times)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", type=int, default=13306, choices=sorted(PROXIES))
    parser.add_argument("--repeat", type=int, default=3)
    parser.add_argument(
        "--latency",
        type=int,
        default=0,
        metavar="MS",
        help="added latency per server reply, in ms (default: 0)",
    )
    parser.add_argument(
        "--rows",
        type=int,
        nargs="+",
        default=[1000],
        help="rows per scenario (default: 1000)",
    )
    parser.add_argument(
        "--sweep", action="store_true", help="rustmy at several max_pipeline values"
    )
    parser.add_argument(
        "--json",
        type=Path,
        metavar="PATH",
        help="also write the results as JSON (input of benches/report.py)",
    )
    args = parser.parse_args()
    proxy_port, proxy = PROXIES[args.port]
    common: dict[str, Any] = {
        "host": args.host,
        "port": proxy_port,
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
    setup.execute("DROP TABLE IF EXISTS bench_pipe")
    setup.execute(CREATE)

    scenarios = []
    windows = []
    try:
        set_latency(proxy, args.latency)
        rtt = round_trip(connections["rustmy"])
        title = f"+{args.latency} ms per reply ({rtt * 1e3:.1f} ms round trip)"
        print(f"best of {args.repeat}, through toxiproxy ({proxy}), {title}\n")
        for rows in args.rows:
            for name, (statement, make) in SCENARIOS.items():
                params = [make(i) for i in range(rows)]
                configs: dict[str, tuple[Any, int | None]] = {
                    driver: (connections[driver], None) for driver in connections
                }
                if args.sweep:
                    for window in SWEEP:
                        configs[f"rustmy w{window}"] = (connections["rustmy"], window)
                seconds = {
                    config: best_time(
                        conn,
                        statement,
                        params,
                        rows=rows,
                        repeat=args.repeat,
                        window=window,
                    )
                    for config, (conn, window) in configs.items()
                }
                label = f"{name} ({rows:,} rows)"
                scenarios.append(
                    {
                        "name": label,
                        "informative": False,
                        "seconds": {d: seconds[d] for d in connections},
                    }
                )
                if args.sweep:
                    windows.append(
                        {
                            "name": label,
                            "seconds": {
                                c: v
                                for c, v in seconds.items()
                                if c.startswith("rustmy w")
                            },
                        }
                    )
                print(f"  {label}")
                for config, value in seconds.items():
                    ratio = seconds["mysqlclient"] / value
                    print(f"    {config:<16}{value * 1000:>10.1f} ms   {ratio:>6.2f}x")
    finally:
        set_latency(proxy, 0)
    if args.json:
        mysqlclient_version = ".".join(map(str, MySQLdb.version_info[:3]))
        report: dict[str, Any] = {
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
            "title": title,
            "latency_ms": args.latency,
            "round_trip_ms": round(rtt * 1e3, 3),
            "scenarios": scenarios,
        }
        if windows:
            report["windows"] = windows
        args.json.parent.mkdir(parents=True, exist_ok=True)
        args.json.write_text(json.dumps(report, indent=2) + "\n")
        print(f"results written to {args.json}")
    setup.execute("DROP TABLE bench_pipe")
    for conn in connections.values():
        conn.close()


if __name__ == "__main__":
    main()
