"""Write a release version into ``Cargo.toml`` and ``Cargo.lock``.

``[workspace.package]`` of ``Cargo.toml`` is the version's single source:
maturin copies it into the wheel's metadata and ``rustmy.__version__`` comes
from it. ``Cargo.lock`` records it for each workspace crate and is updated
here too, so builds can stay ``--locked`` and no ``cargo update`` is needed.
Called by the wheel builds (to stamp the version about to be released, without
committing it) and by semantic-release (which then commits both files):

    python scripts/set_version.py 1.2.3
"""

from __future__ import annotations

import re
import sys

from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
#: Semantic Versioning 2.0.0, as semantic-release produces it.
SEMVER = re.compile(
    r"(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)"
    r"(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?"
)
SECTION = re.compile(
    r"^\[workspace\.package\]\n(.*?)(?=^\[|\Z)", re.MULTILINE | re.DOTALL
)
VERSION = re.compile(r'^version = "[^"]*"$', re.MULTILINE)
NAME = re.compile(r'^name = "([^"]+)"$', re.MULTILINE)


def set_manifest_version(text: str, version: str) -> str:
    """``text`` (the root Cargo.toml) with the workspace version replaced."""
    section = SECTION.search(text)
    if section is None:
        msg = "Cargo.toml has no [workspace.package] section"
        raise ValueError(msg)
    body, count = VERSION.subn(f'version = "{version}"', section.group(1), count=1)
    if count != 1:
        msg = "[workspace.package] has no version"
        raise ValueError(msg)
    return text[: section.start(1)] + body + text[section.end(1) :]


def set_lock_version(text: str, crates: list[str], version: str) -> str:
    """``text`` (Cargo.lock) with each workspace crate's version replaced."""
    for crate in crates:
        entry = re.compile(
            rf'^(\[\[package\]\]\nname = "{re.escape(crate)}"\nversion = )"[^"]*"$',
            re.MULTILINE,
        )
        text, count = entry.subn(rf'\g<1>"{version}"', text)
        if count != 1:
            msg = f"Cargo.lock has no entry for {crate}"
            raise ValueError(msg)
    return text


def workspace_crates() -> list[str]:
    names = []
    for manifest in sorted((ROOT / "crates").glob("*/Cargo.toml")):
        match = NAME.search(manifest.read_text())
        if match is None:
            msg = f"{manifest} has no package name"
            raise ValueError(msg)
        names.append(match.group(1))
    return names


def main() -> int:
    if len(sys.argv) != 2 or not SEMVER.fullmatch(sys.argv[1]):
        print(__doc__, file=sys.stderr)
        return 2
    version = sys.argv[1]
    manifest = ROOT / "Cargo.toml"
    lock = ROOT / "Cargo.lock"
    manifest.write_text(set_manifest_version(manifest.read_text(), version))
    lock.write_text(set_lock_version(lock.read_text(), workspace_crates(), version))
    print(f"Cargo.toml and Cargo.lock: version {version}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
