"""One large value: the time to insert it and read it back, the process's
peak memory, and what the connection keeps once idle, for rustmy and
mysqlclient.

    docker compose up -d --wait mysql
    uv run --no-sync python benches/large_values.py [--mib N] [--repeat N]

Each configuration runs in a fresh process (peak memory is per process):
it connects, inserts a ``--mib`` MiB BLOB ``--repeat`` times (best time
kept) and reads it back. "Held" is the memory still allocated afterwards,
after ``gc.collect()`` with the connection open and idle, as a pooled
connection would sit: the heap in use (``malloc_zone_statistics`` on macOS,
``mallinfo2`` on glibc), not the resident size, which also counts what the
allocator keeps cached for reuse. Plain TCP and ``autocommit``, as in ``spike.py``; the
server's ``max_allowed_packet`` is raised for the run and its binary log
purged after each configuration (``root``).
"""

from __future__ import annotations

import argparse
import ctypes
import gc
import json
import os
import platform
import resource
import subprocess
import sys
import time

from pathlib import Path
from typing import Any


CONFIGS = ("rustmy", "rustmy prepared", "rustmy zlib", "rustmy zstd", "mysqlclient")
BASE: dict[str, Any] = {
    "host": "127.0.0.1",
    "port": 13306,
    "user": "rustmy",
    "password": "rustmy",  # docker test server
    "database": "rustmy_test",
    "autocommit": True,
}


class _ZoneStats(ctypes.Structure):
    _fields_ = (
        ("blocks_in_use", ctypes.c_uint),
        ("size_in_use", ctypes.c_size_t),
        ("max_size_in_use", ctypes.c_size_t),
        ("size_allocated", ctypes.c_size_t),
    )


class _MallInfo2(ctypes.Structure):
    _fields_ = tuple(
        (name, ctypes.c_size_t)
        for name in (
            "arena", "ordblks", "smblks", "hblks", "hblkhd", "usmblks",
            "fsmblks", "uordblks", "fordblks", "keepcost",
        )
    )  # fmt: skip


def heap_mib() -> float:
    """Bytes allocated with malloc and not freed, in MiB."""
    libc = ctypes.CDLL(None)
    if sys.platform == "darwin":
        stats = _ZoneStats()
        libc.malloc_zone_statistics(None, ctypes.byref(stats))
        in_use: int = stats.size_in_use
        return in_use / (1 << 20)
    libc.mallinfo2.restype = _MallInfo2
    info = libc.mallinfo2()
    return (info.uordblks + info.hblkhd) / (1 << 20)


def peak_mib() -> float:
    peak = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
    # Bytes on macOS, KiB elsewhere.
    return peak / (1024 * 1024 if sys.platform == "darwin" else 1024)


def connect(config: str) -> Any:
    if config == "mysqlclient":
        import MySQLdb  # noqa: PLC0415  # only in this configuration's process

        return MySQLdb.connect(**BASE, ssl_mode="DISABLED")
    import rustmy  # noqa: PLC0415

    last = config.rsplit(maxsplit=1)[-1]
    compress = last if last in {"zlib", "zstd"} else False
    return rustmy.connect(**BASE, ssl_mode="disabled", compress=compress)


def measure(config: str, mib: int, repeat: int) -> dict[str, float]:
    conn = connect(config)
    cursor = (
        conn.cursor(prepared=True) if config.endswith("prepared") else conn.cursor()
    )
    value = os.urandom(mib << 20)
    gc.collect()
    before = heap_mib()
    times = []
    for _ in range(repeat):
        cursor.execute("TRUNCATE bench_large")
        start = time.perf_counter()
        cursor.execute("INSERT INTO bench_large VALUES (%s)", (value,))
        times.append(time.perf_counter() - start)
    start = time.perf_counter()
    cursor.execute("SELECT v FROM bench_large")
    (read,) = cursor.fetchone()
    read_time = time.perf_counter() - start
    if read != value:
        msg = f"{config}: the value read back differs"
        raise SystemExit(msg)
    del read
    cursor.close()
    gc.collect()
    held = heap_mib() - before
    conn.close()
    return {
        "insert": min(times),
        "select": read_time,
        "peak_mib": peak_mib(),
        "held_mib": held,
    }


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--mib", type=int, default=64)
    parser.add_argument("--repeat", type=int, default=5)
    parser.add_argument("--json", metavar="PATH", help="also write the results")
    parser.add_argument("--child", choices=CONFIGS, help=argparse.SUPPRESS)
    return parser.parse_args()


def root_cursor() -> Any:
    import rustmy  # noqa: PLC0415

    root = rustmy.connect(**{**BASE, "user": "root"}, ssl_mode="disabled")
    return root.cursor()


def prepare(mib: int) -> str:
    cursor = root_cursor()
    cursor.execute("SET GLOBAL max_allowed_packet = %s", (4 * (mib << 20),))
    cursor.execute("DROP TABLE IF EXISTS bench_large")
    cursor.execute("CREATE TABLE bench_large (v LONGBLOB)")
    server: str = cursor.connection.get_server_info()
    cursor.connection.close()
    return server


def purge_binary_logs() -> None:
    """Every insert goes to the binary log: left there, a few runs fill the
    server's tmpfs, and it then waits for space without an error."""
    cursor = root_cursor()
    cursor.execute("SELECT @@log_bin")
    if cursor.fetchone() == (1,):
        cursor.execute("PURGE BINARY LOGS BEFORE NOW()")
    cursor.connection.close()


def main() -> None:
    args = parse_args()
    if args.child:
        print(json.dumps(measure(args.child, args.mib, args.repeat)))
        return
    server = prepare(args.mib)
    print(f"{args.mib} MiB BLOB, best of {args.repeat} inserts\n")
    print(f"{'':<18}{'insert':>10}{'select':>10}{'peak RSS':>12}{'held':>10}")
    results = {}
    for config in CONFIGS:
        out = subprocess.run(  # noqa: S603  # this script, another configuration
            [
                sys.executable,
                __file__,
                "--child",
                config,
                "--mib",
                str(args.mib),
                "--repeat",
                str(args.repeat),
            ],
            capture_output=True,
            text=True,
            check=True,
        )
        result = json.loads(out.stdout)
        results[config] = result
        purge_binary_logs()
        print(
            f"{config:<18}{result['insert'] * 1000:>8.0f} ms"
            f"{result['select'] * 1000:>7.0f} ms"
            f"{result['peak_mib']:>8.0f} MiB{result['held_mib']:>6.0f} MiB"
        )
    if args.json:
        report = {
            "python": f"{platform.python_implementation()} {sys.version.split()[0]}",
            "machine": f"{platform.system()} {platform.machine()}",
            "server": server,
            "mib": args.mib,
            "repeat": args.repeat,
            "results": results,
        }
        Path(args.json).write_text(json.dumps(report, indent=2) + "\n")


if __name__ == "__main__":
    main()
