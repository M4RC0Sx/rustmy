"""Run Django's test suite with rustmy and with mysqlclient, and compare.

Django's MySQL backend imports ``MySQLdb``; rustmy runs it through
``rustmy.install_as_MySQLdb()``, mysqlclient is the baseline. Each Django
version gets its own environment under ``.cache/django/<version>`` (a
shallow clone of Django, a virtualenv with Django's test requirements that
install from wheels, mysqlclient and rustmy built from this checkout). Runs
before closing a block and weekly in CI:

    uv run python scripts/django_suite.py --django 5.2.19 --python 3.14 --server mysql
    uv run python scripts/django_suite.py --django 6.1.3 --python 3.15 --server mariadb

mysqlclient builds against the MySQL client library: on macOS set
``PKG_CONFIG_PATH`` as in ``benches/README.md``. The servers are the
``compose.yaml`` ones (root, password ``rustmy``). Django clones the test
databases for parallel runs with ``mysqldump``/``mysql``; the script puts
wrappers for both first on ``PATH`` that run the server container's own
client tools, so nothing else needs installing.

The script fails if a test fails (or errors) with rustmy and passes with
mysqlclient, except the differences in ``KNOWN``.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import stat
import subprocess
import sys

from dataclasses import dataclass
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
SERVERS = {"mysql": 13306, "mariadb": 13307}
DRIVERS = ("rustmy", "mysqlclient")
#: Django's test requirements that install from wheels on every supported
#: Python (the rest only enable a few optional tests, skipped for both
#: drivers alike).
REQUIREMENTS = (
    "aiosmtpd",
    "argon2-cffi",
    "bcrypt",
    "docutils",
    "jinja2",
    "numpy",
    "Pillow",
    "pymemcache",
    "PyYAML",
    "sqlparse",
    "tblib",
    "tzdata",
)
#: Tests that fail with rustmy and pass with mysqlclient on purpose, with
#: the reason (``pattern: reason``). Every entry needs a decision recorded in
#: PLAN.md.
KNOWN: dict[str, str] = {
    r"^backends\.mysql\.test_schema\.SchemaEditorTests\.test_quote_value$": (
        "escape() returns str for every value; mysqlclient returns bytes for "
        "bytes, bool, None and dates, which Django's schema editor formats "
        "into SQL as b'...'. The test pins mysqlclient's types."
    ),
}

SETTINGS = """\
{install}
DATABASES = {{
    "default": {{
        "ENGINE": "django.db.backends.mysql",
        "NAME": "django_default",
        "USER": "root",
        "PASSWORD": "rustmy",
        "HOST": "127.0.0.1",
        "PORT": {port},
    }},
    "other": {{
        "ENGINE": "django.db.backends.mysql",
        "NAME": "django_other",
        "USER": "root",
        "PASSWORD": "rustmy",
        "HOST": "127.0.0.1",
        "PORT": {port},
    }},
}}
SECRET_KEY = "django_tests_secret_key"
PASSWORD_HASHERS = ["django.contrib.auth.hashers.MD5PasswordHasher"]
DEFAULT_AUTO_FIELD = "django.db.models.AutoField"
USE_TZ = False
"""

#: ``docker exec`` wrapper for a client tool of the server container: the
#: host and port Django passes are the published ones, inside the container
#: the server is on localhost:3306.
WRAPPER = """\
#!/bin/sh
container={container}
tool={tool}
for arg do
    shift
    case "$arg" in
        --host=*) set -- "$@" --host=127.0.0.1 ;;
        --port=*) set -- "$@" --port=3306 ;;
        *) set -- "$@" "$arg" ;;
    esac
done
exec docker exec -i -e MYSQL_PWD="$MYSQL_PWD" "$container" "$tool" "$@"
"""

TEST_LINE = re.compile(r"^(\w+) \(([\w.]+)\)[^.]*?(?: \.\.\.(?: (.+?))?)?\s*$")
#: The header of each failure's traceback; a test with failing subtests has
#: no status of its own on its ``-v 2`` line.
FAILURE = re.compile(r"^(FAIL|ERROR): \w+ \(([\w.]+)\)")
STATUS = re.compile(
    r" \.\.\. (ok|FAIL|ERROR|skipped.*|expected failure|unexpected success)$"
)
FAILED = {"FAIL", "ERROR", "unexpected success"}


def run(command: list[str]) -> None:
    subprocess.run(command, check=True)  # noqa: S603  # fixed tools and arguments


def checkout(workdir: Path, version: str) -> Path:
    source = workdir / "django"
    if not (source / "tests" / "runtests.py").is_file():
        run([
            "git", "clone", "--quiet", "--depth", "1", "--branch", version,
            "https://github.com/django/django.git", str(source),
        ])  # fmt: skip
    return source


def environment(workdir: Path, source: Path, python: str) -> Path:
    """The virtualenv's interpreter, with rustmy rebuilt from the checkout."""
    venv = workdir / "venv"
    interpreter = venv / ("Scripts/python.exe" if os.name == "nt" else "bin/python")
    uv = shutil.which("uv") or "uv"
    if not interpreter.exists():
        run([uv, "venv", "--quiet", "--python", python, str(venv)])
        run([
            uv, "pip", "install", "--quiet", "--python", str(interpreter),
            "-e", str(source), "mysqlclient", *REQUIREMENTS,
        ])  # fmt: skip
    run([
        uv, "pip", "install", "--quiet", "--python", str(interpreter),
        "--reinstall-package", "rustmy", str(ROOT),
    ])  # fmt: skip
    return interpreter


def wrappers(workdir: Path, server: str) -> Path:
    container = subprocess.run(  # noqa: S603  # fixed arguments
        ["docker", "compose", "ps", "-q", server],  # noqa: S607
        cwd=ROOT,
        capture_output=True,
        text=True,
        check=True,
    ).stdout.strip()
    if not container:
        msg = f"{server} is not running (docker compose up -d --wait {server})"
        raise SystemExit(msg)
    bindir = workdir / f"bin-{server}"
    bindir.mkdir(exist_ok=True)
    tools = {"mysql": "mysql", "mysqldump": "mysqldump"}
    if server == "mariadb":
        tools = {"mysql": "mariadb", "mysqldump": "mariadb-dump"}
    for name, tool in tools.items():
        path = bindir / name
        path.write_text(WRAPPER.format(container=container, tool=tool))
        path.chmod(path.stat().st_mode | stat.S_IXUSR)
    return bindir


def results(log: Path) -> tuple[dict[str, str], str]:
    """``{test id: status}`` from a ``-v 2`` log, and the summary line."""
    statuses: dict[str, str] = {}
    pending: str | None = None
    summary = ""
    for line in log.read_text(errors="replace").splitlines():
        if line.startswith(("OK", "FAILED (")):
            summary = line
        failure = FAILURE.match(line)
        if failure:
            statuses[failure.group(2)] = failure.group(1)
            continue
        match = TEST_LINE.match(line)
        if match:
            test, status = match.group(2), match.group(3)
            if status:
                statuses[test] = (
                    status.split(" ")[0] if status.startswith("skipped") else status
                )
                pending = None
            else:
                pending = test
            continue
        found = STATUS.search(line)
        if pending and found:
            status = found.group(1)
            statuses[pending] = "skipped" if status.startswith("skipped") else status
            pending = None
    return statuses, summary


@dataclass(frozen=True)
class Suite:
    interpreter: Path
    source: Path
    workdir: Path
    bindir: Path
    args: argparse.Namespace


def sql(bindir: Path, statement: str) -> list[str]:
    """Rows of ``statement``, run as root by the server container's client."""
    return subprocess.run(  # noqa: S603  # our wrapper and fixed statements
        [str(bindir / "mysql"), "--user=root", "--batch", "--skip-column-names",
         "--execute", statement],
        env={**os.environ, "MYSQL_PWD": "rustmy"},
        capture_output=True,
        text=True,
        check=True,
    ).stdout.splitlines()  # fmt: skip


def clean_server(bindir: Path) -> None:
    """Drop test databases an interrupted run left behind and the binary
    log: the servers keep their data in tmpfs, and a full run with its
    clones of the test databases nearly fills it."""
    leftovers = sql(
        bindir,
        "SELECT schema_name FROM information_schema.schemata "
        "WHERE schema_name LIKE 'test\\_django\\_%'",
    )
    for name in leftovers:
        sql(bindir, f"DROP DATABASE `{name}`")
    if sql(bindir, "SELECT @@log_bin") == ["1"]:
        # The current log cannot be purged: start a new one first.
        sql(bindir, "FLUSH BINARY LOGS")
        current = sql(bindir, "SHOW BINARY LOGS")[-1].split("\t")[0]
        sql(bindir, f"PURGE BINARY LOGS TO '{current}'")


def run_suite(suite: Suite, driver: str) -> dict[str, str]:
    interpreter, source, workdir = suite.interpreter, suite.source, suite.workdir
    bindir, args = suite.bindir, suite.args
    install = (
        "import rustmy\nrustmy.install_as_MySQLdb()\n" if driver == "rustmy" else ""
    )
    # Not in workdir itself: its django/ checkout would shadow the package.
    settings_dir = workdir / "settings"
    settings_dir.mkdir(exist_ok=True)
    settings = settings_dir / f"settings_{args.server}_{driver}.py"
    settings.write_text(SETTINGS.format(install=install, port=SERVERS[args.server]))
    log = workdir / f"{args.server}-{driver}.log"
    env = {
        **os.environ,
        "PATH": f"{bindir}{os.pathsep}{os.environ['PATH']}",
        "PYTHONPATH": str(settings_dir),
    }
    command = [
        str(interpreter), "runtests.py", "--settings", settings.stem,
        "--noinput", "-v", "2", f"--parallel={args.parallel}", *args.labels,
    ]  # fmt: skip
    clean_server(bindir)
    with log.open("w") as out:
        subprocess.run(  # noqa: S603  # our virtualenv and fixed arguments
            command,
            cwd=source / "tests",
            env=env,
            stdout=out,
            stderr=subprocess.STDOUT,
            check=False,
        )
    statuses, summary = results(log)
    if not summary:
        msg = f"{log}: the run ended without a summary"
        raise SystemExit(msg)
    if summary.startswith("FAILED") and not FAILED.intersection(statuses.values()):
        msg = f"{log}: the run failed, but no failing test was parsed"
        raise SystemExit(msg)
    (workdir / f"{args.server}-{driver}.json").write_text(
        json.dumps(statuses, indent=1)
    )
    print(f"{driver}: {len(statuses)} tests; {summary}")
    return statuses


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--django", required=True, help="a Django tag, e.g. 5.2.19")
    parser.add_argument("--python", required=True, help="e.g. 3.14")
    parser.add_argument("--server", choices=sorted(SERVERS), default="mysql")
    # Each process gets its own copy of both test databases (over 200 MB
    # each with Django 6.1), in the servers' tmpfs.
    parser.add_argument("--parallel", type=int, default=min(os.cpu_count() or 2, 8))
    parser.add_argument("--workdir", type=Path, default=ROOT / ".cache" / "django")
    parser.add_argument("labels", nargs="*", help="test labels (default: all)")
    args = parser.parse_args()
    workdir = (args.workdir / args.django).resolve()
    workdir.mkdir(parents=True, exist_ok=True)
    source = checkout(workdir, args.django)
    interpreter = environment(workdir, source, args.python)
    bindir = wrappers(workdir, args.server)
    print(f"Django {args.django}, Python {args.python}, {args.server}, in {workdir}")
    suite = Suite(interpreter, source, workdir, bindir, args)
    ours, theirs = (run_suite(suite, driver) for driver in DRIVERS)
    failing_ours = {t for t, s in ours.items() if s in FAILED}
    failing_theirs = {t for t, s in theirs.items() if s in FAILED}
    regressions = sorted(
        t
        for t in failing_ours - failing_theirs
        if not any(re.search(pattern, t) for pattern in KNOWN)
    )
    print(f"failing with both: {len(failing_ours & failing_theirs)}")
    print(f"failing with mysqlclient only: {len(failing_theirs - failing_ours)}")
    for test in sorted(failing_theirs - failing_ours):
        print(f"  {test}")
    print(f"failing with rustmy only: {len(regressions)}")
    for test in regressions:
        print(f"  {test}")
    missing = sorted(set(theirs) - set(ours))
    if missing:
        print(f"run with mysqlclient only (not reached with rustmy): {len(missing)}")
    return 1 if regressions else 0


if __name__ == "__main__":
    sys.exit(main())
