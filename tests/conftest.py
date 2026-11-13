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

    @property
    def native_user(self) -> str | None:
        """Account whose failed logins surface as errors over plain TCP.

        MySQL's ``rustmy`` uses caching_sha2_password, whose full
        authentication needs TLS or RSA (not implemented yet). MySQL 9+
        has no mysql_native_password account, so such tests are skipped.
        """
        if self.name == "mariadb":
            return "rustmy"
        return "rustmy_native" if self.major_version < 9 else None


def _reachable(host: str, port: int) -> bool:
    try:
        with socket.create_connection((host, port), timeout=0.5):
            return True
    except OSError:
        return False


@pytest.fixture(scope="session", params=_SERVERS)
def server(request: pytest.FixtureRequest) -> Server:
    name: str = request.param
    env_var, default = _ENDPOINTS[name]
    host, _, port = os.environ.get(env_var, default).rpartition(":")
    if not _EXPLICIT and not _reachable(host, int(port)):
        pytest.skip(f"{name} is not running (docker compose up -d --wait)")
    return Server(name, host, int(port))


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
