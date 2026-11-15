"""Shared fixtures.

Tests that use the ``server`` fixture (directly or through ``conn`` /
``cursor``) run once per server and are marked ``integration``. Servers come
from ``docker compose up -d --wait``; endpoints can be overridden with
``RUSTMY_TEST_MYSQL`` / ``RUSTMY_TEST_MARIADB`` (``host:port``).

With ``RUSTMY_TEST_SERVERS`` unset, servers that are not running are
skipped. Setting it (``mysql``, ``mariadb`` or both, comma-separated), as CI
does, makes an unreachable server a failure instead.
"""

from __future__ import annotations

import os
import socket

from dataclasses import dataclass
from functools import cached_property
from typing import TYPE_CHECKING
from typing import Any

import pytest

from hypothesis import settings

import rustmy


if TYPE_CHECKING:
    from collections.abc import Iterator


_ENDPOINTS = {
    "mysql": ("RUSTMY_TEST_MYSQL", "127.0.0.1:13306"),
    "mariadb": ("RUSTMY_TEST_MARIADB", "127.0.0.1:13307"),
}


def _selected_servers() -> tuple[list[str], bool]:
    raw = os.environ.get("RUSTMY_TEST_SERVERS")
    if raw is None:
        return list(_ENDPOINTS), False
    return [name.strip() for name in raw.split(",") if name.strip()], True


_SERVERS, _EXPLICIT = _selected_servers()

# Property tests run against real servers: no per-example deadline (network
# latency varies) and, on failure, the blob that replays the exact example
# (`@reproduce_failure`). The seed is random on purpose, CI included: every
# run explores new examples. `--hypothesis-profile thorough` before closing
# a block.
settings.register_profile("default", max_examples=100, deadline=None, print_blob=True)
settings.register_profile("thorough", max_examples=5000, deadline=None, print_blob=True)
settings.load_profile("default")


@dataclass(frozen=True)
class Server:
    name: str
    host: str
    port: int

    def connect(self, **overrides: Any) -> rustmy.Connection:
        kwargs: dict[str, Any] = {
            "host": self.host,
            "port": self.port,
            "user": "rustmy",
            "password": "rustmy",
            "database": "rustmy_test",
        }
        kwargs.update(overrides)
        return rustmy.connect(**kwargs)

    @cached_property
    def major_version(self) -> int:
        with self.connect() as conn:
            return int(conn.get_server_info().split(".")[0])


def _reachable(host: str, port: int) -> bool:
    try:
        with socket.create_connection((host, port), timeout=0.5):
            return True
    except OSError:
        return False


def resolve_server(name: str) -> Server:
    """The server ``name``, skipping when it is not selected or not running."""
    if name not in _SERVERS:
        pytest.skip(f"{name} is not in RUSTMY_TEST_SERVERS")
    env_var, default = _ENDPOINTS[name]
    host, _, port = os.environ.get(env_var, default).rpartition(":")
    if not _EXPLICIT and not _reachable(host, int(port)):
        pytest.skip(f"{name} is not running (docker compose up -d --wait)")
    return Server(name, host, int(port))


@pytest.fixture(scope="session", params=_SERVERS)
def server(request: pytest.FixtureRequest) -> Server:
    return resolve_server(request.param)


@pytest.fixture
def conn(server: Server) -> Iterator[rustmy.Connection]:
    connection = server.connect()
    yield connection
    connection.close()


@pytest.fixture
def cursor(conn: rustmy.Connection) -> rustmy.Cursor:
    return conn.cursor()


def pytest_collection_modifyitems(items: list[pytest.Item]) -> None:
    for item in items:
        if "server" in getattr(item, "fixturenames", ()):
            item.add_marker(pytest.mark.integration)
