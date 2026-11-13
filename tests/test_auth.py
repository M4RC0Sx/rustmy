"""Authentication plugins.

MySQL: ``caching_sha2_password`` and ``sha256_password`` with the RSA
password exchange over plain TCP. Each case uses a freshly created account,
which cannot be in the server's authentication cache, so the
full-authentication path always runs.

MariaDB: ``client_ed25519`` and ``parsec`` (11.6+), with the accounts from
``tests/docker/mariadb-init.sh``.
"""

from __future__ import annotations

import itertools
import os

from typing import TYPE_CHECKING

import pytest

import rustmy


if TYPE_CHECKING:
    from collections.abc import Callable
    from collections.abc import Iterator
    from pathlib import Path

    from tests.conftest import Server


PLUGINS = ["caching_sha2_password", "sha256_password"]
_names = itertools.count()


@pytest.fixture
def mysql(server: Server) -> Server:
    if server.name != "mysql":
        pytest.skip("caching_sha2_password and sha256_password are MySQL-only")
    return server


@pytest.fixture
def create_user(mysql: Server) -> Iterator[Callable[[str], str]]:
    root = mysql.connect(user="root", database=None, autocommit=True)
    created: list[str] = []

    def create(plugin: str) -> str:
        name = f"fresh_{os.getpid()}_{next(_names)}"
        try:
            root.cursor().execute(
                f"CREATE USER '{name}'@'%' IDENTIFIED WITH {plugin} BY 'fresh-pw'"
            )
        except rustmy.OperationalError as exc:
            if exc.args[0] == 1524:  # plugin not loaded
                pytest.skip(f"{plugin} is not available on this server")
            raise
        created.append(name)
        return name

    yield create
    for name in created:
        root.cursor().execute(f"DROP USER '{name}'@'%'")
    root.close()


@pytest.fixture
def server_public_key(mysql: Server, tmp_path: Path) -> Path:
    with mysql.connect(user="root", database=None) as root:
        cursor = root.cursor()
        cursor.execute("SHOW STATUS LIKE 'Caching_sha2_password_rsa_public_key'")
        (_, pem) = cursor.fetchone() or ("", "")
    path = tmp_path / "server-key.pem"
    path.write_text(pem)
    return path


def _plain_login(
    server: Server, user: str, password: str, **kwargs: object
) -> rustmy.Connection:
    return server.connect(
        user=user, password=password, database=None, ssl_mode="disabled", **kwargs
    )


@pytest.mark.parametrize("plugin", PLUGINS)
def test_password_is_rsa_encrypted_with_requested_key(
    mysql: Server, create_user: Callable[[str], str], plugin: str
) -> None:
    with _plain_login(mysql, create_user(plugin), "fresh-pw") as conn:
        assert not conn.tls


@pytest.mark.parametrize("plugin", PLUGINS)
def test_pinned_server_public_key(
    mysql: Server,
    create_user: Callable[[str], str],
    server_public_key: Path,
    plugin: str,
) -> None:
    user = create_user(plugin)
    with _plain_login(mysql, user, "fresh-pw", server_public_key=server_public_key):
        pass


@pytest.mark.parametrize("plugin", PLUGINS)
def test_wrong_password_over_plain_tcp(
    mysql: Server, create_user: Callable[[str], str], plugin: str
) -> None:
    with pytest.raises(rustmy.OperationalError) as info:
        _plain_login(mysql, create_user(plugin), "nope")
    assert info.value.args[0] == 1045


def test_unreadable_public_key_file_is_interface_error(
    mysql: Server, create_user: Callable[[str], str], tmp_path: Path
) -> None:
    user = create_user("caching_sha2_password")
    with pytest.raises(rustmy.InterfaceError, match="server_public_key"):
        _plain_login(mysql, user, "fresh-pw", server_public_key=tmp_path / "x.pem")


@pytest.fixture
def mariadb(server: Server) -> Server:
    if server.name != "mariadb":
        pytest.skip("client_ed25519 and parsec are MariaDB-only")
    return server


def _mariadb_version(server: Server) -> tuple[int, int]:
    with server.connect() as conn:
        major, minor = conn.get_server_info().split("-")[0].split(".")[:2]
    return int(major), int(minor)


@pytest.mark.parametrize("ssl_mode", ["disabled", "required"])
@pytest.mark.parametrize("user", ["rustmy_ed25519", "rustmy_parsec"])
def test_mariadb_plugins(mariadb: Server, user: str, ssl_mode: str) -> None:
    if user == "rustmy_parsec" and _mariadb_version(mariadb) < (11, 6):
        pytest.skip("parsec needs MariaDB 11.6+")
    with mariadb.connect(user=user, ssl_mode=ssl_mode) as conn:
        cursor = conn.cursor()
        cursor.execute("SELECT CURRENT_USER()")
        (who,) = cursor.fetchone() or ("",)
        assert who.startswith(user)
    with pytest.raises(rustmy.OperationalError) as info:
        mariadb.connect(user=user, password="wrong", ssl_mode=ssl_mode)
    assert info.value.args[0] == 1045
