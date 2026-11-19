"""scripts/locked_version.py: the maturin version the wheels are built with."""

from __future__ import annotations

import importlib.util
import re

from pathlib import Path
from typing import Any

import pytest


ROOT = Path(__file__).resolve().parent.parent


def _load() -> Any:
    """The script as a module (``scripts/`` is not a package)."""
    spec = importlib.util.spec_from_file_location(
        "locked_version", ROOT / "scripts" / "locked_version.py"
    )
    assert spec is not None
    assert spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


_script = _load()
locked_version = _script.locked_version


LOCK_TEXT = """\
version = 1

[[package]]
name = "maturin-extra"
version = "9.9.9"

[[package]]
name = "maturin"
version = "1.16.0"
source = { registry = "https://pypi.org/simple" }
dependencies = [
    { name = "maturin-extra" },
]
"""


def test_the_package_named_exactly() -> None:
    assert locked_version(LOCK_TEXT, "maturin") == "1.16.0"
    assert locked_version(LOCK_TEXT, "maturin-extra") == "9.9.9"


def test_a_missing_package_stops_the_build() -> None:
    with pytest.raises(SystemExit, match=r"extra is not in uv\.lock"):
        locked_version(LOCK_TEXT, "extra")


def test_the_lock_meets_the_build_requirement() -> None:
    # The wheels use the locked maturin: it must satisfy what pyproject.toml
    # asks of a build backend.
    version = tuple(
        int(n) for n in locked_version(_script.LOCK.read_text(), "maturin").split(".")
    )
    pyproject = (ROOT / "pyproject.toml").read_text()
    found = re.search(r'requires = \["maturin>=(\d+)\.(\d+),<(\d+)\.0"\]', pyproject)
    assert found is not None
    low = (int(found.group(1)), int(found.group(2)))
    assert low <= version[:2] < (int(found.group(3)), 0)
