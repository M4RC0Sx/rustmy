"""Generate ``python/rustmy/constants/ER.py`` from the servers' own catalogues.

    uv run --no-sync python scripts/generate_er.py [--dump PATH]

Starts every supported server version with ``docker compose`` (one at a
time, as the CI's integration jobs do) and reads its error catalogue:
MySQL's ``performance_schema.events_errors_summary_global_by_error`` (codes
below 10000; the rest are server log messages that never reach clients) and
MariaDB's ``perror``. Names from every version are kept, so renamed errors
keep their old names too. ``--dump`` also writes the raw catalogues as JSON;
``--from-dump`` regenerates from such a file without starting servers.

Within a vendor a name never changes number across versions (checked). A
name both vendors define with different numbers gets MySQL's (what other
drivers use) and ``MARIADB_<name>`` gets MariaDB's.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import textwrap
import time

from pathlib import Path

import rustmy


ROOT = Path(__file__).resolve().parent.parent
OUTPUT = ROOT / "python" / "rustmy" / "constants" / "ER.py"
MYSQL = ["8.0", "8.4", "9.7", "26.10"]
MARIADB = ["10.11", "11.4", "11.8", "12.3", "13.0"]
#: MariaDB's server error ranges.
MARIADB_CODES = [*range(1000, 2000), *range(3000, 5000)]
PERROR_LINE = re.compile(r"MariaDB error code (\d+) \(([A-Z0-9_]+)\)")


def compose(*args: str, env: dict[str, str] | None = None, check: bool = True) -> str:
    return subprocess.run(  # noqa: S603  # fixed docker compose invocation
        ["docker", "compose", *args],  # noqa: S607
        cwd=ROOT,
        env=env,
        check=check,
        capture_output=True,
        text=True,
    ).stdout


def start(service: str, version: str) -> None:
    env = {**os.environ, "MYSQL_VERSION": version, "MARIADB_VERSION": version}
    compose("down", "-v", env=env)
    compose("up", "-d", "--wait", service, env=env)


def mysql_catalogue() -> dict[str, int]:
    with rustmy.connect(
        host="127.0.0.1",
        port=13306,
        user="root",
        password="rustmy",  # docker test server
        ssl_mode="disabled",
    ) as conn:
        cursor = conn.cursor()
        for _ in range(30):  # performance_schema fills in shortly after start
            cursor.execute(
                "SELECT ERROR_NAME, ERROR_NUMBER FROM performance_schema."
                "events_errors_summary_global_by_error "
                "WHERE ERROR_NAME IS NOT NULL AND ERROR_NUMBER < 10000"
            )
            rows = cursor.fetchall()
            if rows:
                return dict(rows)
            time.sleep(1)
    msg = "empty MySQL error catalogue"
    raise SystemExit(msg)


def mariadb_catalogue() -> dict[str, int]:
    # perror exits with an error when some codes are unknown, but still
    # prints every known one.
    out = compose(
        "exec",
        "-T",
        "mariadb",
        "perror",
        *(str(code) for code in MARIADB_CODES),
        check=False,
    )
    return {name: int(code) for code, name in PERROR_LINE.findall(out)}


def collect() -> dict[str, dict[str, int]]:
    catalogues: dict[str, dict[str, int]] = {}
    for vendor, versions in (("mysql", MYSQL), ("mariadb", MARIADB)):
        for version in versions:
            start(vendor, version)
            catalogue = mysql_catalogue() if vendor == "mysql" else mariadb_catalogue()
            catalogues[f"{vendor} {version}"] = catalogue
            print(f"{vendor} {version}: {len(catalogue)} errors")
    compose("down", "-v")
    return catalogues


def by_vendor(catalogues: dict[str, dict[str, int]]) -> dict[str, dict[str, int]]:
    """Name (without ``ER_``) to number per vendor, across its versions."""
    merged: dict[str, dict[str, int]] = {"mysql": {}, "mariadb": {}}
    for key, catalogue in catalogues.items():
        names = merged[key.split()[0]]
        for raw, code in catalogue.items():
            name = raw.removeprefix("ER_")
            if not name.isidentifier():
                msg = f"{key}: {raw!r} is not a Python identifier"
                raise SystemExit(msg)
            if names.setdefault(name, code) != code:
                msg = f"{key}: {name} is {code} here and {names[name]} elsewhere"
                raise SystemExit(msg)
    return merged


def render(catalogues: dict[str, dict[str, int]]) -> str:
    vendors = by_vendor(catalogues)
    mysql, mariadb = vendors["mysql"], vendors["mariadb"]
    shared = {n: c for n, c in mysql.items() if mariadb.get(n) == c}
    conflicts = {
        n: (c, mariadb[n]) for n, c in mysql.items() if n in mariadb and n not in shared
    }
    sections = [
        ("Same number in MySQL and MariaDB.", shared),
        ("MySQL only.", {n: c for n, c in mysql.items() if n not in mariadb}),
        ("MariaDB only.", {n: c for n, c in mariadb.items() if n not in mysql}),
    ]

    def listed(vendor: str) -> str:
        found = [key.split()[1] for key in catalogues if key.split()[0] == vendor]
        return ", ".join(sorted(found, key=lambda v: tuple(map(int, v.split(".")))))

    source = (
        "Generated by ``scripts/generate_er.py`` from the error catalogues of "
        f"MySQL {listed('mysql')} and MariaDB {listed('mariadb')}. "
        "Do not edit by hand."
    )
    lines = [
        '"""Server error codes (``args[0]`` of errors the server reports).',
        "",
        *textwrap.wrap(source, width=79),
        "",
        "Names from every version are kept (renamed errors keep their old names).",
        "Above about 1900 the vendors diverge: a number one vendor defines may mean",
        "something else in the other. A name both define with different numbers",
        "has MySQL's here and MariaDB's as ``MARIADB_<name>``.",
        '"""',
        "",
        "# Start of the server's error range.",
        "ERROR_FIRST = 1000",
    ]
    for title, names in sections:
        lines += ["", f"# {title}"]
        lines += [
            f"{n} = {c}" for n, c in sorted(names.items(), key=lambda i: (i[1], i[0]))
        ]
    lines += ["", "# Same name, different numbers: MySQL's first, then MariaDB's."]
    for name, (mysql_code, mariadb_code) in sorted(conflicts.items()):
        lines += [f"{name} = {mysql_code}", f"MARIADB_{name} = {mariadb_code}"]
    return "\n".join(lines) + "\n"


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--dump", type=Path, help="also write the raw catalogues")
    parser.add_argument("--from-dump", type=Path, help="use catalogues saved by --dump")
    args = parser.parse_args()
    catalogues = json.loads(args.from_dump.read_text()) if args.from_dump else collect()
    if args.dump:
        args.dump.write_text(json.dumps(catalogues, indent=1, sort_keys=True))
    OUTPUT.write_text(render(catalogues))
    print(f"wrote {OUTPUT.relative_to(ROOT)}")


if __name__ == "__main__":
    main()
