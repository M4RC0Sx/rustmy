"""A pool of connections shared by threads: :class:`Pool`.

The accounts (which connection to lend, when to open or close one, who
waits and in what order) are kept in Rust (``rustmy._core.PoolCore``, the
same ones as :func:`rustmy.aio.create_pool`); this module holds the
connections, opens and closes them, and does the waiting.
"""

from __future__ import annotations

import contextlib
import threading
import time

from typing import TYPE_CHECKING
from typing import Any
from typing import Literal

from rustmy import _core
from rustmy.connections import Connection
from rustmy.err import Error
from rustmy.err import InterfaceError
from rustmy.err import PoolTimeout


if TYPE_CHECKING:
    from types import TracebackType

    from typing_extensions import Self

    _Effects = tuple[list[int], list[tuple[int, int | None]], int, list[int]]

# SERVER_STATUS_IN_TRANS: a transaction is open.
_STATUS_IN_TRANS = 1

_LEND = _core.PoolCore.LEND
_OPEN = _core.PoolCore.OPEN
_WAIT = _core.PoolCore.WAIT


def _close_quietly(conn: Connection) -> None:
    with contextlib.suppress(Error):
        conn.close()


class _Lease:
    """A connection lent by :meth:`Pool.acquire`: ``with`` gives the
    connection and returns it to the pool at the end of the block."""

    __slots__ = ("_pool", "connection")

    def __init__(self, pool: Pool, connection: Connection) -> None:
        self._pool = pool
        self.connection = connection

    def __enter__(self) -> Connection:
        return self.connection

    def __exit__(
        self,
        exc_type: type[BaseException] | None,
        exc: BaseException | None,
        tb: TracebackType | None,
    ) -> None:
        self._pool.release(self.connection)


class Pool:
    """Connections opened with :func:`rustmy.connect`'s arguments
    (``connect_args``), lent to one thread at a time.

    ``minsize`` connections are opened at once and kept open; more are
    opened as they are needed, up to ``maxsize``, after which
    :meth:`acquire` waits (in order of arrival) for one to be given back.
    The idle ones are lent most recently used first.

    A connection given back with a transaction open is rolled back; with
    ``reset_on_release``, it is also reset (:meth:`Connection.
    reset_connection`: session variables and temporary tables go, a round
    trip per use). One that is closed, fails, or is older than
    ``pool_recycle`` seconds is closed instead of lent again; one idle for
    ``pre_ping`` seconds is pinged before it is lent; and idle ones beyond
    ``minsize`` are closed after ``idle_timeout`` seconds unused (checked
    as connections are acquired and given back).

    ``with Pool(...) as pool`` closes the pool at the end of the block and
    waits for the connections in use to come back.
    """

    def __init__(
        self,
        minsize: int = 1,
        maxsize: int = 10,
        *,
        pool_recycle: float = -1,
        idle_timeout: float | None = None,
        pre_ping: float | None = 30.0,
        reset_on_release: bool = False,
        **connect_args: Any,
    ) -> None:
        self._core = _core.PoolCore(
            minsize,
            maxsize,
            None if pool_recycle < 0 else pool_recycle,
            idle_timeout,
            pre_ping,
        )
        self._connect_args = connect_args
        self._reset = reset_on_release
        self._cond = threading.Condition()
        # Every open connection by its id; the lent ones by object.
        self._conns: dict[int, Connection] = {}
        self._lent: dict[int, int] = {}
        # Waiters served: a connection's id, or None for room to open one.
        self._grants: dict[int, int | None] = {}
        self._failed: set[int] = set()
        with self._cond:
            _, opens = self._record(self._core.start())
        # The minimum is opened here, so that connect()'s errors surface.
        for opened in range(opens):
            try:
                conn = Connection(**connect_args)
            except BaseException:
                self.close()
                with self._cond:
                    for _ in range(opens - opened):
                        self._record(self._core.open_failed())
                raise
            with self._cond:
                conn_id, effects = self._core.idle_opened()
                self._conns[conn_id] = conn
                after = self._record(effects)
            self._after(*after)

    # The pool's state.

    @property
    def minsize(self) -> int:
        return self._core.minsize

    @property
    def maxsize(self) -> int:
        return self._core.maxsize

    @property
    def size(self) -> int:
        """Connections open, in use or idle, or being opened."""
        return self._core.size

    @property
    def freesize(self) -> int:
        """Idle connections."""
        return self._core.freesize

    @property
    def closed(self) -> bool:
        return self._core.closed

    # Lending.

    def acquire(self, timeout: float | None = None) -> _Lease:
        """Lend a connection: ``with pool.acquire() as conn:``.

        Waits up to ``timeout`` seconds (forever with ``None``) when every
        connection is in use, then raises :class:`~rustmy.err.PoolTimeout`.
        Raises :class:`~rustmy.err.InterfaceError` once the pool is closed,
        and what :func:`rustmy.connect` raises when a connection cannot be
        opened.
        """
        return _Lease(self, self._get(timeout))

    def _get(self, timeout: float | None) -> Connection:
        deadline = None if timeout is None else time.monotonic() + timeout
        while True:
            with self._cond:
                kind, value, ping, effects = self._core.acquire()
                closing, opens = self._record(effects)
                # Before waiting: what is waited for may be one of them.
                self._start_opens(opens)
                if kind == _WAIT:
                    granted = self._wait(value, deadline)
                    kind, value = (_OPEN, 0) if granted is None else (_LEND, granted)
                conn = self._conns.get(value) if kind == _LEND else None
            self._after(closing, 0)
            if kind == _OPEN:
                return self._open_lent()
            if conn is None:
                raise InterfaceError(0, "pool closed")
            if ping and not self._alive(conn):
                self._give_back(value, conn, reuse=False)
                continue
            with self._cond:
                self._lent[id(conn)] = value
            return conn

    def _wait(self, ticket: int, deadline: float | None) -> int | None:
        """Waits (the lock held) for ``ticket``'s grant: a connection's id,
        or ``None`` for room to open one."""
        try:
            outcome = self._await_grant(ticket, deadline)
        except BaseException:
            self._abandon(ticket)
            raise
        if outcome == "failed":
            raise InterfaceError(0, "pool closed")
        if outcome == "timeout":
            msg = "no connection was given back in time"
            raise PoolTimeout(0, msg)
        return self._grants.pop(ticket)

    def _await_grant(
        self, ticket: int, deadline: float | None
    ) -> Literal["granted", "failed", "timeout"]:
        while ticket not in self._grants:
            if ticket in self._failed:
                self._failed.discard(ticket)
                return "failed"
            remaining = None if deadline is None else deadline - time.monotonic()
            if remaining is not None and remaining <= 0 and self._core.cancel(ticket):
                return "timeout"
            self._cond.wait(remaining)
        return "granted"

    def _abandon(self, ticket: int) -> None:
        """Stops waiting (interrupted, Ctrl+C): what it was served
        meanwhile goes back."""
        if self._core.cancel(ticket) or ticket not in self._grants:
            return
        granted = self._grants.pop(ticket)
        effects = (
            self._core.open_failed()
            if granted is None
            else self._core.release(granted, True)  # noqa: FBT003
        )
        self._spawn(*self._record(effects))

    def _alive(self, conn: Connection) -> bool:
        try:
            conn.ping()
        except Error:
            return False
        return True

    def _open_lent(self) -> Connection:
        """Opens the connection the pool made room for, lent to the caller."""
        try:
            conn = Connection(**self._connect_args)
        except BaseException:
            with self._cond:
                after = self._record(self._core.open_failed())
            self._after(*after)
            raise
        with self._cond:
            conn_id = self._core.opened()
            self._conns[conn_id] = conn
            self._lent[id(conn)] = conn_id
        return conn

    def release(self, conn: Connection) -> None:
        """Give ``conn`` back (``acquire``'s ``with`` block does it)."""
        with self._cond:
            conn_id = self._lent.pop(id(conn), None)
        if conn_id is None:
            raise InterfaceError(0, "connection not lent by this pool")
        reuse = conn.open
        if reuse:
            try:
                if conn._conn.server_status & _STATUS_IN_TRANS:  # noqa: SLF001
                    conn.rollback()
                if self._reset:
                    conn.reset_connection()
            except Error:
                reuse = False
        self._give_back(conn_id, conn, reuse=reuse)

    def _give_back(self, conn_id: int, conn: Connection, *, reuse: bool) -> None:
        with self._cond:
            self._lent.pop(id(conn), None)
            after = self._record(self._core.release(conn_id, reuse))
        self._after(*after)

    # Closing.

    def clear(self) -> None:
        """Close the idle connections (others are opened for the minimum)."""
        with self._cond:
            after = self._record(self._core.clear())
        self._after(*after)

    def close(self) -> None:
        """Close the pool: idle connections now, those in use as they come
        back; waiting and later :meth:`acquire` calls raise."""
        with self._cond:
            after = self._record(self._core.close())
        self._after(*after)

    def terminate(self) -> None:
        """Close the pool and every connection, those in use included (their
        threads see them closed)."""
        self.close()
        with self._cond:
            lent = [self._conns[i] for i in self._lent.values() if i in self._conns]
        for conn in lent:
            _close_quietly(conn)

    def wait_closed(self, timeout: float | None = None) -> bool:
        """Wait for the connections in use to come back once the pool is
        closed; false if ``timeout`` seconds pass first."""
        with self._cond:
            if not self._core.closed:
                raise InterfaceError(0, "close() the pool first")
            return self._cond.wait_for(lambda: self._core.drained, timeout)

    def __enter__(self) -> Self:
        return self

    def __exit__(
        self,
        exc_type: type[BaseException] | None,
        exc: BaseException | None,
        tb: TracebackType | None,
    ) -> None:
        self.close()
        self.wait_closed()

    # What the accounts ask for.

    def _record(self, effects: _Effects) -> tuple[list[Connection], int]:
        """Takes in ``effects`` (the lock held): the connections to close
        and how many to open, for the caller to do once it lets go of the
        lock."""
        close, grants, opens, failed = effects
        closing = [self._conns.pop(conn_id) for conn_id in close]
        for ticket, conn_id in grants:
            self._grants[ticket] = conn_id
        self._failed.update(failed)
        if grants or failed or self._core.closed:
            self._cond.notify_all()
        return closing, opens

    def _after(self, closing: list[Connection], opens: int) -> None:
        for conn in closing:
            _close_quietly(conn)
        self._start_opens(opens)

    def _start_opens(self, opens: int) -> None:
        for _ in range(opens):
            # Off the caller's thread: giving a connection back never waits
            # for a new one to open.
            threading.Thread(target=self._open_idle, daemon=True).start()

    def _spawn(self, closing: list[Connection], opens: int) -> None:
        """``_after`` from under the lock: closing waits for it."""
        threading.Thread(target=self._after, args=(closing, opens), daemon=True).start()

    def _open_idle(self) -> None:
        """Opens a connection for the minimum: idle, or to whoever waits."""
        try:
            conn = Connection(**self._connect_args)
        except Exception:  # noqa: BLE001  # its room goes back; the next try reports
            with self._cond:
                after = self._record(self._core.open_failed())
            self._after(*after)
            return
        with self._cond:
            conn_id, effects = self._core.idle_opened()
            self._conns[conn_id] = conn
            after = self._record(effects)
        self._after(*after)
