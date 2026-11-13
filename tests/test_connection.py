"""Connection lifecycle, errors, transactions and threads."""

from __future__ import annotations

import threading
import time
import uuid

from typing import TYPE_CHECKING

import pytest

import rustmy


if TYPE_CHECKING:
    from collections.abc import Iterator

    from tests.conftest import Server


def test_server_info(conn: rustmy.Connection, server: Server) -> None:
    assert conn.open
    assert conn.thread_id() > 0
    assert int(conn.get_server_info().split(".")[0]) == server.major_version
    assert conn.character_set_name() == "utf8mb4"
    conn.ping()


@pytest.mark.parametrize(
    ("sql", "exc", "code"),
    [
        ("SELEC 1", rustmy.ProgrammingError, 1064),
        ("SELECT * FROM no_such_table", rustmy.ProgrammingError, 1146),
        ("SELECT no_such_column", rustmy.ProgrammingError, 1054),
    ],
)
def test_server_errors_map_to_pep249_classes(
    cursor: rustmy.Cursor, sql: str, exc: type[Exception], code: int
) -> None:
    with pytest.raises(exc) as info:
        cursor.execute(sql)
    assert info.value.args[0] == code
    # The connection stays usable after a server error.
    cursor.execute("SELECT 1")
    assert cursor.fetchone() == (1,)


def test_duplicate_key_is_integrity_error(cursor: rustmy.Cursor) -> None:
    cursor.execute("CREATE TEMPORARY TABLE t (id INT PRIMARY KEY)")
    cursor.execute("INSERT INTO t VALUES (1)")
    with pytest.raises(rustmy.IntegrityError) as info:
        cursor.execute("INSERT INTO t VALUES (1)")
    assert info.value.args[0] == 1062


def test_wrong_password_is_operational_error(server: Server) -> None:
    # Over TLS a failed caching_sha2_password login gets a real answer.
    with pytest.raises(rustmy.OperationalError) as info:
        server.connect(password="wrong", ssl_mode="required")
    assert info.value.args[0] == 1045


def test_closed_connection_raises_interface_error(server: Server) -> None:
    conn = server.connect()
    conn.close()
    conn.close()  # idempotent
    assert not conn.open
    with pytest.raises(rustmy.InterfaceError):
        conn.cursor().execute("SELECT 1")


def test_killed_connection_is_reported_as_gone(server: Server) -> None:
    victim = server.connect()
    with server.connect() as admin:
        admin.cursor().execute(f"KILL {victim.thread_id()}")
    with pytest.raises(rustmy.OperationalError):
        victim.cursor().execute("SELECT 1")
    with pytest.raises(rustmy.OperationalError):
        victim.cursor().execute("SELECT 1")
    victim.close()


@pytest.fixture
def table(server: Server) -> Iterator[str]:
    """A real (non-temporary) table, visible to other connections."""
    name = f"t_{uuid.uuid4().hex[:12]}"
    with server.connect(autocommit=True) as conn:
        conn.cursor().execute(f"CREATE TABLE {name} (v INT)")
        yield name
        conn.cursor().execute(f"DROP TABLE {name}")


def _count(server: Server, table: str) -> int:
    with server.connect() as other:
        cursor = other.cursor()
        cursor.execute(f"SELECT COUNT(*) FROM {table}")
        (count,) = cursor.fetchone() or (None,)
        assert isinstance(count, int)
        return count


def test_autocommit_is_off_by_default_and_commit_publishes(
    conn: rustmy.Connection, server: Server, table: str
) -> None:
    assert not conn.get_autocommit()
    conn.cursor().execute(f"INSERT INTO {table} VALUES (1)")
    assert _count(server, table) == 0
    conn.commit()
    assert _count(server, table) == 1


def test_rollback_discards(conn: rustmy.Connection, server: Server, table: str) -> None:
    conn.cursor().execute(f"INSERT INTO {table} VALUES (1)")
    conn.rollback()
    assert _count(server, table) == 0


def test_autocommit_mode(conn: rustmy.Connection, server: Server, table: str) -> None:
    conn.autocommit(True)  # noqa: FBT003
    assert conn.get_autocommit()
    conn.cursor().execute(f"INSERT INTO {table} VALUES (1)")
    assert _count(server, table) == 1


def test_init_command(server: Server) -> None:
    with server.connect(init_command="SET @rustmy = 42") as conn:
        cursor = conn.cursor()
        cursor.execute("SELECT @rustmy")
        assert cursor.fetchone() == (42,)


def test_queries_on_separate_connections_run_in_parallel(server: Server) -> None:
    """The GIL is released while waiting on the server."""
    threads_count, sleep = 4, 0.5
    connections = [server.connect() for _ in range(threads_count)]
    barrier = threading.Barrier(threads_count)

    def work(conn: rustmy.Connection) -> None:
        barrier.wait()
        conn.cursor().execute(f"SELECT SLEEP({sleep})")

    threads = [threading.Thread(target=work, args=(c,)) for c in connections]
    start = time.perf_counter()
    for thread in threads:
        thread.start()
    for thread in threads:
        thread.join()
    elapsed = time.perf_counter() - start
    for conn in connections:
        conn.close()
    # Serialised execution would take threads_count * sleep = 2 s.
    assert elapsed < sleep * 2, f"{elapsed:.2f}s"


def test_concurrent_use_of_one_connection_is_rejected(
    conn: rustmy.Connection,
) -> None:
    started = threading.Event()
    errors: list[BaseException] = []

    def slow_query() -> None:
        started.set()
        try:
            conn.cursor().execute("SELECT SLEEP(1)")
        except BaseException as exc:  # noqa: BLE001
            errors.append(exc)

    thread = threading.Thread(target=slow_query)
    thread.start()
    started.wait()
    time.sleep(0.3)
    with pytest.raises(rustmy.ProgrammingError, match="another thread"):
        conn.cursor().execute("SELECT 1")
    thread.join()
    assert errors == []
    # Usable again once the other thread is done.
    conn.cursor().execute("SELECT 1")
