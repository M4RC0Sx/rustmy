"""select_db, reset_connection, change_user and timeouts."""

from __future__ import annotations

import socket
import time

from typing import TYPE_CHECKING
from typing import Any

import pytest

import rustmy


if TYPE_CHECKING:
    from collections.abc import Iterator

    from tests.conftest import Server


def _one(conn: rustmy.Connection, sql: str) -> Any:
    cursor = conn.cursor()
    cursor.execute(sql)
    (value,) = cursor.fetchone() or (None,)
    return value


def test_select_db(conn: rustmy.Connection) -> None:
    conn.select_db("information_schema")
    assert _one(conn, "SELECT DATABASE()") == "information_schema"
    with pytest.raises(rustmy.DatabaseError):
        conn.select_db("no_such_db")
    # A failed select_db is an ordinary server error.
    assert _one(conn, "SELECT 1") == 1


def test_reset_connection_restores_the_connect_time_session(server: Server) -> None:
    with server.connect(init_command="SET @init = 'yes'") as conn:
        thread_id = conn.thread_id()
        cursor = conn.cursor()
        cursor.execute("SET @x = 1")
        cursor.execute("CREATE TEMPORARY TABLE tmp (a INT)")
        cursor.execute("BEGIN")
        cursor.execute("INSERT INTO tmp VALUES (1)")

        conn.reset_connection()

        assert conn.thread_id() == thread_id  # same server session
        assert _one(conn, "SELECT @x") is None
        assert _one(conn, "SELECT @init") == "yes"  # init_command re-applied
        assert not conn.get_autocommit()  # connect() default re-applied
        with pytest.raises(rustmy.ProgrammingError):
            cursor.execute("SELECT * FROM tmp")


def test_change_user(server: Server) -> None:
    with server.connect(init_command="SET @init = 'yes'", autocommit=True) as conn:
        conn.change_user("root", "rustmy", "rustmy_test")
        assert str(_one(conn, "SELECT CURRENT_USER()")).startswith("root@")
        assert _one(conn, "SELECT DATABASE()") == "rustmy_test"
        assert _one(conn, "SELECT @init") == "yes"
        assert conn.get_autocommit()


def test_failed_change_user_leaves_connection_unusable(server: Server) -> None:
    # MySQL closes the connection, MariaDB keeps the old user; rustmy treats
    # both the same way so callers can rely on it.
    conn = server.connect()
    with pytest.raises(rustmy.OperationalError) as info:
        conn.change_user("root", "wrong")
    assert info.value.args[0] == 1045
    with pytest.raises(rustmy.OperationalError) as info:
        conn.cursor().execute("SELECT 1")
    assert info.value.args[0] == 2006
    assert not conn.open
    conn.close()


def test_read_timeout(server: Server) -> None:
    conn = server.connect(read_timeout=0.3)
    start = time.perf_counter()
    with pytest.raises(rustmy.OperationalError) as info:
        conn.cursor().execute("SELECT SLEEP(3)")
    assert info.value.args[0] == 2013
    assert time.perf_counter() - start < 2
    # The stream is in an unknown state afterwards: the connection is gone.
    with pytest.raises(rustmy.OperationalError) as info:
        conn.cursor().execute("SELECT 1")
    assert info.value.args[0] == 2006
    conn.close()


def test_connect_timeout_to_unreachable_host() -> None:
    start = time.perf_counter()
    with pytest.raises(rustmy.OperationalError) as info:
        # Non-routable address: the SYN is never answered (or is rejected).
        rustmy.connect(host="10.255.255.1", port=3306, connect_timeout=0.5)
    assert info.value.args[0] == 2003
    assert time.perf_counter() - start < 3


@pytest.fixture
def silent_port() -> Iterator[int]:
    """A port whose listener accepts connections (the kernel completes the
    TCP handshake) and never says a word."""
    with socket.create_server(("127.0.0.1", 0)) as listener:
        yield listener.getsockname()[1]


def test_connect_timeout_covers_the_greeting(silent_port: int) -> None:
    start = time.perf_counter()
    with pytest.raises(rustmy.OperationalError) as info:
        rustmy.connect(host="127.0.0.1", port=silent_port, connect_timeout=0.3)
    assert info.value.args[0] == 2003
    assert time.perf_counter() - start < 3


def test_connect_timeout_has_a_default(
    silent_port: int, monkeypatch: pytest.MonkeyPatch
) -> None:
    assert rustmy.connections._DEFAULT_CONNECT_TIMEOUT == 15  # noqa: SLF001
    monkeypatch.setattr(rustmy.connections, "_DEFAULT_CONNECT_TIMEOUT", 0.3)
    start = time.perf_counter()
    with pytest.raises(rustmy.OperationalError) as info:
        rustmy.connect(host="127.0.0.1", port=silent_port)
    assert info.value.args[0] == 2003
    assert time.perf_counter() - start < 3


def test_connect_timeout_ends_once_connected(server: Server) -> None:
    with server.connect(connect_timeout=0.3) as conn:
        assert _one(conn, "SELECT SLEEP(0.8)") == 0


def test_ping_after_unread_multi_results(server: Server) -> None:
    with server.connect(client_flag=1 << 16) as conn:
        conn.cursor().execute("SELECT 1; SELECT 2")
        conn.ping()
        assert _one(conn, "SELECT 3") == 3
