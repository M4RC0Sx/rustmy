"""Run SQLAlchemy's whole test suite with rustmy and PyMySQL, and compare.

The dialect suite in ``tests/sqla_suite`` runs on every CI job; this is the
full suite of the installed SQLAlchemy version (~24k tests, ~10 minutes per
driver and server), taken from its sdist. It runs before closing a block:

    uv run python scripts/sqlalchemy_full_suite.py --server mysql
    uv run python scripts/sqlalchemy_full_suite.py --server mariadb

Before each driver's run the test databases are recreated (as root, on the
throwaway docker servers), so one run's leftovers cannot fail the next. The
script fails if a test fails with rustmy and not with PyMySQL, except those
that SQLAlchemy's own requirements exclude by driver name (``KNOWN``).
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
import tarfile
import urllib.request

from pathlib import Path

import sqlalchemy

import rustmy


ROOT = Path(__file__).resolve().parent.parent
SERVERS = {
    "mysql": ("mysql", 13306),
    "mariadb": ("mariadb", 13307),
}
DATABASES = ("sqla_test", "test_schema", "test_schema_2")

#: Tests that fail with rustmy only because SQLAlchemy's test suite picks
#: them by driver name: server-side cursors are recognised by name, and the
#: implicit-bound date/time tests are excluded for PyMySQL and mysqlclient,
#: which send these parameters exactly as rustmy does.
KNOWN = re.compile(
    r"ServerSideCursorsTest::"
    r"|(Date|DateTime|DateTimeCoercedToDateTime|DateHistoric|DateTimeHistoric"
    r"|Time|TimeMicroseconds)Test::test_select_direct"
)


def sdist(workdir: Path) -> Path:
    """The installed SQLAlchemy's sdist, downloaded and unpacked once."""
    version = sqlalchemy.__version__
    target = workdir / f"sqlalchemy-{version}"
    if (target / "test").is_dir():
        return target
    url = f"https://pypi.org/pypi/SQLAlchemy/{version}/json"
    with urllib.request.urlopen(url, timeout=30) as response:
        files = json.load(response)["urls"]
    source = next(f["url"] for f in files if f["packagetype"] == "sdist")
    archive = workdir / f"sqlalchemy-{version}.tar.gz"
    workdir.mkdir(parents=True, exist_ok=True)
    urllib.request.urlretrieve(source, archive)  # noqa: S310  # PyPI's https URL
    with tarfile.open(archive) as tar:
        tar.extractall(workdir, filter="data")
    return next(
        p for p in workdir.iterdir() if p.is_dir() and p.name.lower() == target.name
    )


def reset_databases(port: int) -> None:
    with rustmy.connect(
        host="127.0.0.1", port=port, user="root", password="rustmy"
    ) as conn:
        cursor = conn.cursor()
        for name in DATABASES:
            cursor.execute(f"DROP DATABASE IF EXISTS {name}")
            cursor.execute(f"CREATE DATABASE {name} CHARACTER SET utf8mb4")


def failures(source: Path, backend: str, driver: str, port: int, log: Path) -> set[str]:
    reset_databases(port)
    url = f"{backend}+{driver}://rustmy:rustmy@127.0.0.1:{port}/sqla_test"
    command = [
        sys.executable, "-m", "pytest", "test",
        "-p", "no:randomly", "-p", "no:cacheprovider",
        "-q", "--tb=line", "--maxfail=100000", "--dburi", url,
    ]  # fmt: skip
    with log.open("w") as out:
        subprocess.run(  # noqa: S603  # our own interpreter and fixed arguments
            command, cwd=source, stdout=out, stderr=subprocess.STDOUT, check=False
        )
    names = set()
    for line in log.read_text().splitlines():
        if line.startswith(("FAILED ", "ERROR ")):
            test = line.split(" - ")[0].split(" ", 1)[1]
            names.add(re.sub(r"_(mysql|mariadb)\+\w+?_[\d_]+::", "::", test))
    print(f"{driver}: {log.read_text().splitlines()[-1].strip('= ')}")
    return names


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--server", choices=sorted(SERVERS), default="mysql")
    parser.add_argument("--workdir", type=Path, default=ROOT / ".cache" / "sqlalchemy")
    args = parser.parse_args()
    backend, port = SERVERS[args.server]
    source = sdist(args.workdir)
    print(f"SQLAlchemy {sqlalchemy.__version__}, {args.server}, sdist in {source}")
    ours = failures(
        source, backend, "rustmy", port, args.workdir / f"{args.server}-rustmy.log"
    )
    theirs = failures(
        source, backend, "pymysql", port, args.workdir / f"{args.server}-pymysql.log"
    )
    only_ours = sorted(t for t in ours - theirs if not KNOWN.search(t))
    known = sorted(t for t in ours - theirs if KNOWN.search(t))
    only_theirs = sorted(theirs - ours)
    print(f"failing with both: {len(ours & theirs)}, known: {len(known)}")
    print(f"failing with PyMySQL only: {len(only_theirs)}")
    for test in only_theirs:
        print(f"  {test}")
    print(f"failing with rustmy only: {len(only_ours)}")
    for test in only_ours:
        print(f"  {test}")
    return 1 if only_ours else 0


if __name__ == "__main__":
    sys.exit(main())
