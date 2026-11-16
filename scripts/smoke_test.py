"""Smoke test of an installed rustmy wheel (run by .github/workflows/wheels.yml).

Checks that the wheel imports from its install location (not a source tree),
carries the expected version and its third-party licenses, and keeps the GIL
off on free-threaded Python. With ``RUSTMY_SMOKE_HOST`` set, it also runs real
queries as root (the CI servers' account): TLS negotiation, parameters, the
main types, a database of its own, ``executemany`` and streaming. Runs on
every supported Python, so no syntax newer than 3.10.

    RUSTMY_SMOKE_HOST=127.0.0.1 RUSTMY_SMOKE_PASSWORD=rustmy \\
        python scripts/smoke_test.py
"""

from __future__ import annotations

import datetime as dt
import os
import sys
import sysconfig

from decimal import Decimal
from importlib import metadata
from pathlib import Path

import rustmy

from rustmy.cursors import SSCursor


class SmokeError(Exception):
    pass


def expect(condition: object, message: str) -> None:
    # Not `assert`: python -O would skip it.
    if not condition:
        raise SmokeError(message)


def gil_enabled() -> bool:
    # sys._is_gil_enabled() exists from Python 3.13 on.
    probe = getattr(sys, "_is_gil_enabled", None)
    return True if probe is None else bool(probe())


def check_install() -> str:
    # Installed, not the source tree (python/rustmy): uv's layered
    # environments put it in a site-packages other than sysconfig's.
    location = Path(rustmy.__file__).resolve()
    expect(
        location.parent.parent.name == "site-packages",
        f"rustmy imported from {location}, not an installed wheel",
    )
    version = metadata.version("rustmy")
    expect(
        rustmy.__version__ == version,
        f"rustmy.__version__ {rustmy.__version__}, metadata {version}",
    )
    expected = os.environ.get("RUSTMY_SMOKE_VERSION")
    if expected:
        expect(version == expected, f"version {version}, expected {expected}")
    licenses = {Path(str(f)).name for f in metadata.files("rustmy") or ()}
    expect("THIRD-PARTY-LICENSES" in licenses, "THIRD-PARTY-LICENSES missing")
    free_threaded = bool(sysconfig.get_config_var("Py_GIL_DISABLED"))
    if free_threaded:
        expect(not gil_enabled(), "importing rustmy re-enabled the GIL")
    return f"{version}{' free-threaded' if free_threaded else ''}"


def check_server(host: str) -> str:
    conn = rustmy.connect(
        host=host,
        port=int(os.environ.get("RUSTMY_SMOKE_PORT", "3306")),
        user=os.environ.get("RUSTMY_SMOKE_USER", "root"),
        password=os.environ.get("RUSTMY_SMOKE_PASSWORD", ""),
    )
    try:
        cursor = conn.cursor()
        values = (
            "ñ 😀 ' \\",
            b"\x00\xff",
            Decimal("12.34"),
            dt.datetime(2026, 1, 2, 3, 4, 5, 678901),  # noqa: DTZ001
            dt.date(2026, 1, 2),
            None,
        )
        cursor.execute(
            "SELECT %s, %s, CAST(%s AS DECIMAL(5, 2)), CAST(%s AS DATETIME(6)), "
            "CAST(%s AS DATE), %s",
            values,
        )
        expect(cursor.fetchone() == values, "parameters did not round-trip")
        cursor.execute("CREATE DATABASE IF NOT EXISTS rustmy_smoke")
        conn.select_db("rustmy_smoke")
        cursor.execute("CREATE TEMPORARY TABLE smoke (id INT PRIMARY KEY, s TEXT)")
        rows = [(i, f"row {i}") for i in range(1000)]
        inserted = cursor.executemany("INSERT INTO smoke VALUES (%s, %s)", rows)
        expect(inserted == len(rows), f"executemany inserted {inserted} rows")
        streamed = conn.cursor(SSCursor)
        streamed.execute("SELECT id, s FROM smoke ORDER BY id")
        expect(list(streamed) == rows, "streamed rows differ")
        return f"server {conn.get_server_info()}, TLS {conn.tls}"
    finally:
        conn.close()


def main() -> None:
    summary = [f"rustmy {check_install()}", f"Python {sys.version.split()[0]}"]
    host = os.environ.get("RUSTMY_SMOKE_HOST")
    summary.append(check_server(host) if host else "no server (import only)")
    print("smoke test passed: " + "; ".join(summary))


if __name__ == "__main__":
    main()
