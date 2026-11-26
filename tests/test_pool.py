"""``rustmy.Pool``: connections shared by threads (the accounts behind it
are tested in ``crates/proto/src/pool.rs``)."""

from __future__ import annotations

import threading
import time

from typing import TYPE_CHECKING
from typing import Any

import pytest

import rustmy


if TYPE_CHECKING:
    from collections.abc import Callable
    from collections.abc import Iterator

    from tests.conftest import Server


@pytest.fixture
def make_pool(server: Server) -> Iterator[Callable[..., rustmy.Pool]]:
    pools: list[rustmy.Pool] = []

    def make(minsize: int = 1, maxsize: int = 2, **options: Any) -> rustmy.Pool:
        pool = rustmy.Pool(minsize, maxsize, **server.connect_args(**options))
        pools.append(pool)
        return pool

    yield make
    for pool in pools:
        pool.terminate()


def connection_id(conn: rustmy.Connection) -> int:
    return conn.thread_id()


def eventually(condition: Callable[[], bool]) -> None:
    """Waits (2 s at most) for what a background thread does."""
    deadline = time.monotonic() + 2
    while not condition():
        assert time.monotonic() < deadline, "timed out"
        time.sleep(0.01)


def test_lends_and_reuses_connections(make_pool: Callable[..., rustmy.Pool]) -> None:
    pool = make_pool(1, 2)
    assert (pool.minsize, pool.maxsize, pool.size, pool.freesize) == (1, 2, 1, 1)
    with pool.acquire() as conn:
        assert pool.freesize == 0
        first = connection_id(conn)
        cursor = conn.cursor()
        cursor.execute("SELECT 1")
        assert cursor.fetchall() == ((1,),)
    assert pool.freesize == 1
    with pool.acquire() as conn:
        assert connection_id(conn) == first
        # A second one is opened on demand.
        with pool.acquire() as other:
            assert connection_id(other) != first
            assert pool.size == 2
    assert pool.freesize == 2


def test_waits_for_a_connection_and_times_out(
    make_pool: Callable[..., rustmy.Pool],
) -> None:
    pool = make_pool(0, 1)
    lease = pool.acquire()
    start = time.monotonic()
    with pytest.raises(rustmy.PoolTimeout):
        pool.acquire(timeout=0.2)
    assert 0.15 < time.monotonic() - start < 1
    # PoolTimeout is an OperationalError.
    assert issubclass(rustmy.PoolTimeout, rustmy.OperationalError)
    got: list[int] = []

    def waiter(tag: int) -> None:
        with pool.acquire(timeout=5):
            got.append(tag)

    threads = [threading.Thread(target=waiter, args=(tag,)) for tag in range(3)]
    for thread in threads:
        thread.start()
        eventually(lambda: pool._core.waiting == len(got) + 1 + threads.index(thread))  # noqa: SLF001, B023
    pool.release(lease.connection)
    for thread in threads:
        thread.join()
    # Served in order of arrival.
    assert got == [0, 1, 2]
    assert pool.size == 1


def test_a_transaction_left_open_is_rolled_back(
    make_pool: Callable[..., rustmy.Pool],
) -> None:
    pool = make_pool(1, 1)
    with pool.acquire() as conn:
        conn.autocommit(False)  # noqa: FBT003  # MySQLdb-compatible API
        cursor = conn.cursor()
        cursor.execute("CREATE TEMPORARY TABLE t (n INT) ENGINE=InnoDB")
        cursor.execute("INSERT INTO t VALUES (1)")
    with pool.acquire() as conn:
        cursor = conn.cursor()
        cursor.execute("SELECT COUNT(*) FROM t")
        assert cursor.fetchone() == (0,)


def test_reset_on_release(make_pool: Callable[..., rustmy.Pool]) -> None:
    pool = make_pool(1, 1, reset_on_release=True)
    with pool.acquire() as conn:
        first = connection_id(conn)
        conn.cursor().execute("SET @kept = 1")
    with pool.acquire() as conn:
        assert connection_id(conn) == first
        cursor = conn.cursor()
        cursor.execute("SELECT @kept")
        assert cursor.fetchone() == (None,)


def test_a_closed_connection_is_replaced(
    make_pool: Callable[..., rustmy.Pool],
) -> None:
    pool = make_pool(1, 2)
    with pool.acquire() as conn:
        first = connection_id(conn)
        conn.close()
    # The minimum is opened again, off this thread.
    eventually(lambda: pool.freesize == 1)
    with pool.acquire() as conn:
        assert connection_id(conn) != first


def test_a_dead_idle_connection_is_not_lent(
    server: Server, make_pool: Callable[..., rustmy.Pool]
) -> None:
    pool = make_pool(1, 1, pre_ping=0)
    with pool.acquire() as conn:
        first = connection_id(conn)
    with server.connect() as admin:
        admin.cursor().execute(f"KILL {first}")
    with pool.acquire() as conn:
        assert connection_id(conn) != first
        conn.cursor().execute("SELECT 1")


def test_pool_recycle(make_pool: Callable[..., rustmy.Pool]) -> None:
    pool = make_pool(0, 1, pool_recycle=0)
    with pool.acquire() as conn:
        first = connection_id(conn)
    assert pool.size == 0
    with pool.acquire() as conn:
        assert connection_id(conn) != first


def test_idle_timeout_keeps_the_minimum(
    make_pool: Callable[..., rustmy.Pool],
) -> None:
    pool = make_pool(1, 3, idle_timeout=0)
    leases = [pool.acquire() for _ in range(3)]
    for lease in leases:
        pool.release(lease.connection)
    assert (pool.size, pool.freesize) == (1, 1)


def test_close_and_wait_closed(make_pool: Callable[..., rustmy.Pool]) -> None:
    pool = make_pool(1, 1)
    lease = pool.acquire()
    failures: list[BaseException] = []

    def waiter() -> None:
        try:
            pool.acquire(timeout=5)
        except rustmy.InterfaceError as exc:
            failures.append(exc)

    thread = threading.Thread(target=waiter)
    thread.start()
    eventually(lambda: pool._core.waiting == 1)  # noqa: SLF001
    with pytest.raises(rustmy.InterfaceError, match="first"):
        pool.wait_closed()
    pool.close()
    thread.join()
    assert len(failures) == 1
    with pytest.raises(rustmy.InterfaceError, match="pool closed"):
        pool.acquire()
    assert not pool.wait_closed(timeout=0.05)
    # The connection still works for whoever has it, and closes on return.
    lease.connection.cursor().execute("SELECT 1")
    pool.release(lease.connection)
    assert not lease.connection.open
    assert pool.wait_closed(timeout=1)
    assert pool.size == 0


def test_with_block_closes_the_pool(server: Server) -> None:
    with rustmy.Pool(1, 2, **server.connect_args()) as pool, pool.acquire() as conn:
        conn.cursor().execute("SELECT 1")
    assert pool.closed
    assert pool.size == 0


def test_terminate_closes_connections_in_use(
    make_pool: Callable[..., rustmy.Pool],
) -> None:
    pool = make_pool(1, 1)
    lease = pool.acquire()
    pool.terminate()
    assert not lease.connection.open
    pool.release(lease.connection)
    assert pool.wait_closed(timeout=1)


def test_connect_errors(server: Server, make_pool: Callable[..., rustmy.Pool]) -> None:
    with pytest.raises(rustmy.OperationalError):
        rustmy.Pool(1, 2, **server.connect_args(password="wrong"))
    pool = make_pool(0, 1, password="wrong")
    with pytest.raises(rustmy.OperationalError):
        pool.acquire()
    # The room is given back.
    assert pool.size == 0
    with pytest.raises(rustmy.OperationalError):
        pool.acquire(timeout=1)


def test_only_its_own_connections_are_given_back(
    server: Server, make_pool: Callable[..., rustmy.Pool]
) -> None:
    pool = make_pool(1, 1)
    with server.connect() as stranger, pytest.raises(rustmy.InterfaceError):
        pool.release(stranger)
    with pool.acquire() as conn:
        pass
    with pytest.raises(rustmy.InterfaceError):
        pool.release(conn)


def test_limits(server: Server) -> None:
    with pytest.raises(ValueError, match="maxsize"):
        rustmy.Pool(0, 0, **server.connect_args())
    with pytest.raises(ValueError, match="minsize"):
        rustmy.Pool(3, 2, **server.connect_args())
    with pytest.raises(ValueError, match="idle_timeout"):
        rustmy.Pool(0, 1, idle_timeout=-1, **server.connect_args())


def test_threads_share_the_connections(make_pool: Callable[..., rustmy.Pool]) -> None:
    pool = make_pool(1, 4)
    in_use: set[int] = set()
    most = 0
    lock = threading.Lock()
    errors: list[BaseException] = []

    def work() -> None:
        nonlocal most
        try:
            for _ in range(40):
                with pool.acquire(timeout=10) as conn:
                    conn_id = connection_id(conn)
                    with lock:
                        assert conn_id not in in_use, "lent twice"
                        in_use.add(conn_id)
                        most = max(most, len(in_use))
                    cursor = conn.cursor()
                    cursor.execute("SELECT CONNECTION_ID()")
                    assert cursor.fetchone() == (conn_id,)
                    with lock:
                        in_use.discard(conn_id)
        except BaseException as exc:  # noqa: BLE001  # reported below
            errors.append(exc)

    threads = [threading.Thread(target=work) for _ in range(16)]
    for thread in threads:
        thread.start()
    for thread in threads:
        thread.join()
    assert errors == []
    assert 1 < most <= 4
    assert pool.size <= 4
    assert pool.freesize == pool.size
