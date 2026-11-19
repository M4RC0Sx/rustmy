"""Opening connections: rustmy against mysqlclient and PyMySQL, without TLS,
with TLS and no verification, and verifying the server's certificate.

    docker compose --profile bench up -d --wait mysql toxiproxy
    uv run --no-sync python benches/connect.py [--repeat N] [--latency MS]

Each driver opens and closes ``--connections`` connections one after the
other with its own defaults (``autocommit`` off, utf8mb4), as a pool or a
short-lived script does; best of N. Release build, as in ``spike.py``. Each
closed connection holds a client port in TIME_WAIT for a while (30 s on
macOS, whose ephemeral range is 16k ports), so every attempt starts after a
pause (``--settle``). The modes:

- ``disabled``: plain TCP.
- ``preferred``: TLS without checking the certificate, the default of all
  three.
- ``verify_identity``: TLS checking the chain against the test CA
  (``tests/docker/certs/ca.pem``) and the host name.

The server's general log gives the statements each driver sends while
connecting (switched on briefly through a ``root`` connection).
``--latency`` connects through the ``toxiproxy`` compose service (profile
``bench``) with that many milliseconds added to every reply, where each round
trip shows. ``--before PATH`` adds rustmy's times from an earlier run's JSON
as an extra configuration.
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
from typing import TYPE_CHECKING
from typing import Any

import MySQLdb
import pymysql

import rustmy

from pipeline import PROXIES
from pipeline import set_latency


if TYPE_CHECKING:
    from collections.abc import Callable


CA = Path(__file__).resolve().parent.parent / "tests" / "docker" / "certs" / "ca.pem"
MODES = ("disabled", "preferred", "verify_identity")


def connectors(base: dict[str, Any], mode: str) -> dict[str, Callable[[], Any]]:
    """One function per driver opening a connection in ``mode``."""
    verify = mode == "verify_identity"
    ours: dict[str, Any] = {"ssl_mode": mode, "ssl_ca": CA if verify else None}
    mysqlclient: dict[str, Any] = {"ssl_mode": mode.upper()}
    if verify:
        mysqlclient["ssl"] = {"ca": str(CA)}
    pymysql_tls: dict[str, Any] = {}
    if mode == "disabled":
        pymysql_tls["ssl_disabled"] = True
    elif verify:
        pymysql_tls.update(
            ssl_ca=str(CA), ssl_verify_cert=True, ssl_verify_identity=True
        )
    return {
        "rustmy": lambda: rustmy.connect(**base, **ours),
        "mysqlclient": lambda: MySQLdb.connect(**base, **mysqlclient),
        "pymysql": lambda: pymysql.connect(**base, **pymysql_tls),
    }


def check(name: str, conn: Any, mode: str) -> None:
    """Fails unless the connection is encrypted exactly when ``mode`` asks."""
    cursor = conn.cursor()
    cursor.execute("SHOW SESSION STATUS LIKE 'Ssl_version'")
    row = cursor.fetchone()
    encrypted = bool(row and row[1])
    if encrypted != (mode != "disabled"):
        msg = f"{name} in {mode} mode: encrypted={encrypted}"
        raise SystemExit(msg)


def best_time(
    connect: Callable[[], Any], count: int, repeat: int, settle: float
) -> float:
    times = []
    for _ in range(repeat):
        time.sleep(settle)
        gc.collect()
        start = time.perf_counter()
        for _ in range(count):
            connect().close()
        times.append(time.perf_counter() - start)
    return min(times)


def statements(observer: Any, connect: Callable[[], Any]) -> list[str]:
    """The statements one connection sends while opening, from the
    server's general log (on only for this)."""
    cursor = observer.cursor()
    cursor.execute("SELECT @@log_output, @@general_log")
    output, enabled = cursor.fetchone()
    cursor.execute("SET GLOBAL log_output = 'TABLE', general_log = 1")
    try:
        conn = connect()
        thread = conn.thread_id()
        conn.close()
        cursor.execute(
            "SELECT CONVERT(argument USING utf8mb4) FROM mysql.general_log"
            " WHERE thread_id = %s AND command_type = 'Query'",
            (thread,),
        )
        return [row[0] for row in cursor.fetchall()]
    finally:
        cursor.execute(
            "SET GLOBAL log_output = %s, general_log = %s", (output, enabled)
        )
        cursor.execute("TRUNCATE mysql.general_log")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", type=int, default=13306, choices=sorted(PROXIES))
    parser.add_argument("--repeat", type=int, default=5)
    parser.add_argument("--connections", type=int, default=1000)
    parser.add_argument(
        "--settle",
        type=float,
        default=10,
        metavar="SECONDS",
        help="pause before each attempt, so closed connections leave TIME_WAIT "
        "and the client keeps ephemeral ports (default: 10)",
    )
    parser.add_argument(
        "--latency",
        type=int,
        default=0,
        metavar="MS",
        help="add this many milliseconds to every reply through toxiproxy "
        "(default: connect directly)",
    )
    parser.add_argument(
        "--json",
        type=Path,
        metavar="PATH",
        help="also write the results as JSON (input of benches/report.py)",
    )
    parser.add_argument(
        "--before",
        type=Path,
        metavar="PATH",
        help="JSON of an earlier run: its rustmy times become 'rustmy before'",
    )
    return parser.parse_args()


def main() -> None:
    args = parse_args()
    base: dict[str, Any] = {
        "host": args.host,
        "port": args.port,
        "user": "rustmy",
        "password": "rustmy",  # docker test server
        "database": "rustmy_test",
    }
    listen, proxy = PROXIES[args.port]
    if args.latency:
        base["port"] = listen
    observer = rustmy.connect(
        **{**base, "port": args.port, "user": "root"},
        ssl_mode="disabled",
        autocommit=True,
    )
    title = f"{args.latency} ms added per reply" if args.latency else "local"
    panels = []
    try:
        set_latency(proxy, args.latency)
        print(f"{args.connections:,} connections, best of {args.repeat}, {title}\n")
        for mode in MODES:
            seconds: dict[str, float] = {}
            sent: dict[str, list[str]] = {}
            for name, connect in connectors(base, mode).items():
                conn = connect()
                check(name, conn, mode)
                conn.close()
                sent[name] = statements(observer, connect)
                seconds[name] = best_time(
                    connect, args.connections, args.repeat, args.settle
                )
            panels.append({"label": mode, "seconds": seconds, "statements": sent})
            print(mode)
            for name, value in seconds.items():
                ratio = seconds["mysqlclient"] / value
                each = value / args.connections * 1e6
                print(
                    f"  {name:<14}{each:>9.0f} µs/connection   {ratio:>5.2f}x"
                    f"   {'; '.join(sent[name]) or '-'}"
                )
    finally:
        set_latency(proxy, 0)
    if args.json:
        write_report(args, observer.get_server_info(), title, panels)
    observer.close()


def write_report(
    args: argparse.Namespace, server: str, title: str, panels: list[Any]
) -> None:
    labels = {"mysqlclient": "mysqlclient", "pymysql": "PyMySQL"}
    vendor = server.split("-", maxsplit=1)[0]
    vendor = f"MariaDB {vendor}" if "MariaDB" in server else f"MySQL {vendor}"
    if args.before:
        earlier = json.loads(args.before.read_text())
        for panel, old in zip(panels, earlier["sweep"], strict=True):
            panel["seconds"] = {
                "rustmy": panel["seconds"]["rustmy"],
                "rustmy before": old["seconds"]["rustmy"],
                **{k: v for k, v in panel["seconds"].items() if k != "rustmy"},
            }
            panel["statements"]["rustmy before"] = old["statements"]["rustmy"]
        labels["rustmy before"] = "rustmy before"
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
        "title": (
            f"{args.connections:,} connections to {vendor}, {title}: "
            "throughput relative to mysqlclient"
        ),
        # No configuration is marked "(default)": these are the drivers.
        "default": None,
        "labels": labels,
        "connections": args.connections,
        "latency_ms": args.latency,
        "sweep": panels,
    }
    args.json.parent.mkdir(parents=True, exist_ok=True)
    args.json.write_text(json.dumps(report, indent=2) + "\n")
    print(f"results written to {args.json}")


if __name__ == "__main__":
    main()
