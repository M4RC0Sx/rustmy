"""Print the version ``uv.lock`` pins for a package.

The wheel builds take maturin's from here, so releases are built with the
maturin development uses (Dependabot keeps it current) instead of whichever
is latest that day:

    python scripts/locked_version.py maturin

Plain text matching, as ``set_version.py`` does, so it runs on any Python a
runner has (``tomllib`` needs 3.11).
"""

from __future__ import annotations

import re
import sys

from pathlib import Path


LOCK = Path(__file__).resolve().parent.parent / "uv.lock"


def locked_version(lock: str, name: str) -> str:
    """The version of the ``[[package]]`` called ``name`` in ``lock``."""
    found = re.search(
        rf'^\[\[package\]\]\nname = "{re.escape(name)}"\nversion = "([^"]+)"$',
        lock,
        re.MULTILINE,
    )
    if found is None:
        msg = f"{name} is not in uv.lock"
        raise SystemExit(msg)
    return found.group(1)


def main() -> None:
    if len(sys.argv) != 2:  # the script and one argument
        msg = "usage: locked_version.py PACKAGE"
        raise SystemExit(msg)
    print(locked_version(LOCK.read_text(encoding="utf-8"), sys.argv[1]))


if __name__ == "__main__":
    main()
