"""A pool of connections for asyncio: :func:`create_pool`, with the names
of aiomysql's and asyncmy's pools.

The accounts are kept in Rust (``rustmy._core.PoolCore``, the same ones as
:class:`rustmy.Pool`); this module holds the connections, opens and closes
them, and serves whoever waits through futures, all on the loop's thread.
"""

from __future__ import annotations

import asyncio
import contextlib

from typing import TYPE_CHECKING
from typing import Any

from rustmy import _core
from rustmy.aio.connections import connect
from rustmy.err import Error
from rustmy.err import InterfaceError
from rustmy.err import PoolTimeout


if TYPE_CHECKING:
    from collections.abc import Coroutine
    from collections.abc import Generator
    from types import TracebackType

    from typing_extensions import Self

    from rustmy.aio.connections import Connection

    _Effects = tuple[list[int], list[tuple[int, int | None]], int, list[int]]

# SERVER_STATUS_IN_TRANS: a transaction is open.
_STATUS_IN_TRANS = 1


def _close_quietly(conn: Connection) -> None:
    with contextlib.suppress(Error):
        conn.close()


_LEND = _core.PoolCore.LEND
_OPEN = _core.PoolCore.OPEN
_WAIT = _core.PoolCore.WAIT


class Pool:
    """Connections opened with :func:`rustmy.aio.connect`'s arguments,
    lent to one task at a time; :func:`create_pool` makes one. As
    :class:`rustmy.Pool` (the same options, and the same health checks:
    rollback when given back with a transaction open, ``reset_on_release``,
    ``pre_ping``, ``pool_recycle``, ``idle_timeout``).

    A connection is given back with ``async with pool.acquire() as conn``,
    or with :meth:`release` (whose task may be awaited). A task cancelled
    in the middle of a command closes its connection, which is then not
    lent again.
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
        # Every open connection by its id; the lent ones by object.
        self._conns: dict[int, Connection] = {}
        self._lent: dict[int, int] = {}
        # Whoever waits, by ticket: served a connection's id, or None for
        # room to open one.
        self._waiters: dict[int, asyncio.Future[int | None]] = {}
        self._tasks: set[asyncio.Task[Any]] = set()
        self._drained: asyncio.Event | None = None

    async def _fill(self) -> None:
        """Opens the minimum, so that ``connect()``'s errors surface."""
        _, _, opens, _ = self._core.start()
        results = await asyncio.gather(
            *(connect(**self._connect_args) for _ in range(opens)),
            return_exceptions=True,
        )
        failure = next((r for r in results if isinstance(r, BaseException)), None)
        if failure is not None:
            self.close()
        for result in results:
            if isinstance(result, BaseException):
                self._apply(self._core.open_failed())
            else:
                conn_id, effects = self._core.idle_opened()
                self._conns[conn_id] = result
                self._apply(effects)
        if failure is not None:
            raise failure

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

    def acquire(self, timeout: float | None = None) -> _Acquire:
        """Lend a connection: ``async with pool.acquire() as conn``, or
        ``conn = await pool.acquire()`` and :meth:`release` later.

        Waits up to ``timeout`` seconds (forever with ``None``) when every
        connection is in use, then raises :class:`~rustmy.err.PoolTimeout`;
        raises :class:`~rustmy.err.InterfaceError` once the pool is closed,
        and what :func:`rustmy.aio.connect` raises when a connection cannot
        be opened.
        """
        return _Acquire(self, timeout)

    async def _get(self, timeout: float | None) -> Connection:
        while True:
            kind, value, ping, effects = self._core.acquire()
            self._apply(effects)
            if kind == _WAIT:
                granted = await self._wait(value, timeout)
                kind, value = (_OPEN, 0) if granted is None else (_LEND, granted)
            if kind == _OPEN:
                return await self._open_lent()
            conn = self._conns.get(value) if kind == _LEND else None
            if conn is None:
                raise InterfaceError(0, "pool closed")
            if ping:
                try:
                    await conn.ping()
                except Error:
                    self._apply(self._core.release(value, False))  # noqa: FBT003
                    continue
                except BaseException:
                    # Cancelled: the connection is closed.
                    self._apply(self._core.release(value, False))  # noqa: FBT003
                    raise
            self._lent[id(conn)] = value
            return conn

    async def _wait(self, ticket: int, timeout: float | None) -> int | None:
        loop = asyncio.get_running_loop()
        waiter: asyncio.Future[int | None] = loop.create_future()
        self._waiters[ticket] = waiter
        timer = None
        if timeout is not None:
            timer = loop.call_later(timeout, self._time_out, ticket)
        try:
            return await waiter
        except asyncio.CancelledError:
            self._waiters.pop(ticket, None)
            if (
                not self._core.cancel(ticket)
                and waiter.done()
                and not waiter.cancelled()
            ):
                # Served just before the task was cancelled: what it got
                # goes back (once cancelled, `_apply` does it).
                self._give_up(waiter.result())
            raise
        finally:
            if timer is not None:
                timer.cancel()

    def _give_up(self, granted: int | None) -> None:
        """What a waiter that is gone was served goes back."""
        if granted is None:
            self._apply(self._core.open_failed())
        else:
            self._apply(self._core.release(granted, True))  # noqa: FBT003

    def _time_out(self, ticket: int) -> None:
        waiter = self._waiters.get(ticket)
        if waiter is not None and not waiter.done() and self._core.cancel(ticket):
            del self._waiters[ticket]
            msg = "no connection was given back in time"
            waiter.set_exception(PoolTimeout(0, msg))

    async def _open_lent(self) -> Connection:
        """Opens the connection the pool made room for, lent to the caller."""
        try:
            conn = await connect(**self._connect_args)
        except BaseException:
            self._apply(self._core.open_failed())
            raise
        conn_id = self._core.opened()
        self._conns[conn_id] = conn
        self._lent[id(conn)] = conn_id
        return conn

    def release(self, conn: Connection) -> asyncio.Future[None]:
        """Give ``conn`` back: rolled back first if a transaction is open.
        The returned future may be awaited (``async with`` awaits it); it is
        already done unless the connection needs a command first."""
        conn_id = self._lent.pop(id(conn), None)
        if conn_id is None:
            raise InterfaceError(0, "connection not lent by this pool")
        try:
            reuse = not conn.closed
            needs_command = reuse and (
                self._reset or bool(conn._core.server_status & _STATUS_IN_TRANS)  # noqa: SLF001
            )
        except Error:
            # A command still running on it (not awaited).
            reuse = needs_command = False
        if needs_command:
            return self._spawn(self._give_back(conn_id, conn))
        # Nothing to send: given back right here, without a turn of the
        # loop.
        self._apply(self._core.release(conn_id, reuse))
        done = asyncio.get_running_loop().create_future()
        done.set_result(None)
        return done

    async def _give_back(self, conn_id: int, conn: Connection) -> None:
        reuse = not conn.closed
        try:
            if reuse and conn._core.server_status & _STATUS_IN_TRANS:  # noqa: SLF001
                await conn.rollback()
            if reuse and self._reset:
                await conn.reset_connection()
        except Error:
            reuse = False
        except BaseException:
            self._apply(self._core.release(conn_id, False))  # noqa: FBT003
            raise
        self._apply(self._core.release(conn_id, reuse))

    # Closing.

    async def clear(self) -> None:
        """Close the idle connections (others are opened for the minimum)."""
        self._apply(self._core.clear())

    def close(self) -> None:
        """Close the pool: idle connections now, those in use as they come
        back; waiting and later :meth:`acquire` calls raise."""
        self._apply(self._core.close())

    def terminate(self) -> None:
        """Close the pool and every connection, those in use included."""
        self.close()
        for conn_id in list(self._lent.values()):
            conn = self._conns.get(conn_id)
            if conn is not None:
                _close_quietly(conn)

    async def wait_closed(self) -> None:
        """Wait for the connections in use to come back once the pool is
        closed."""
        if not self._core.closed:
            raise InterfaceError(0, "close() the pool first")
        if not self._core.drained:
            if self._drained is None:
                self._drained = asyncio.Event()
            await self._drained.wait()
        if self._tasks:
            await asyncio.gather(*self._tasks, return_exceptions=True)

    async def __aenter__(self) -> Self:
        return self

    async def __aexit__(
        self,
        exc_type: type[BaseException] | None,
        exc: BaseException | None,
        tb: TracebackType | None,
    ) -> None:
        self.close()
        await self.wait_closed()

    # What the accounts ask for.

    def _apply(self, effects: _Effects) -> None:
        close, grants, opens, failed = effects
        for conn_id in close:
            _close_quietly(self._conns.pop(conn_id))
        for ticket, granted in grants:
            waiter = self._waiters.pop(ticket, None)
            if waiter is not None and not waiter.done():
                waiter.set_result(granted)
            else:
                # Its waiter is gone (cancelled).
                self._give_up(granted)
        for ticket in failed:
            waiter = self._waiters.pop(ticket, None)
            if waiter is not None and not waiter.done():
                waiter.set_exception(InterfaceError(0, "pool closed"))
        for _ in range(opens):
            self._spawn(self._open_idle())
        if self._drained is not None and self._core.drained:
            self._drained.set()

    def _spawn(self, coroutine: Coroutine[Any, Any, None]) -> asyncio.Task[None]:
        task = asyncio.get_running_loop().create_task(coroutine)
        self._tasks.add(task)
        task.add_done_callback(self._tasks.discard)
        return task

    async def _open_idle(self) -> None:
        """Opens a connection for the minimum: idle, or to whoever waits."""
        try:
            conn = await connect(**self._connect_args)
        except Exception:  # noqa: BLE001  # its room goes back; the next try reports
            self._apply(self._core.open_failed())
            return
        conn_id, effects = self._core.idle_opened()
        self._conns[conn_id] = conn
        self._apply(effects)


class _Acquire:
    """What :meth:`Pool.acquire` returns: awaited, the connection;
    ``async with``, the connection, given back at the end of the block."""

    __slots__ = ("_conn", "_pool", "_timeout")

    def __init__(self, pool: Pool, timeout: float | None) -> None:
        self._pool = pool
        self._timeout = timeout
        self._conn: Connection | None = None

    def __await__(self) -> Generator[Any, None, Connection]:
        return self._pool._get(self._timeout).__await__()  # noqa: SLF001

    async def __aenter__(self) -> Connection:
        self._conn = await self._pool._get(self._timeout)  # noqa: SLF001
        return self._conn

    async def __aexit__(
        self,
        exc_type: type[BaseException] | None,
        exc: BaseException | None,
        tb: TracebackType | None,
    ) -> None:
        conn, self._conn = self._conn, None
        if conn is not None:
            await self._pool.release(conn)


class _CreatePool:
    """What :func:`create_pool` returns: awaited, the pool once its
    minimum is open; ``async with``, the pool, closed (and waited for) at
    the end of the block."""

    __slots__ = ("_pool",)

    def __init__(self, pool: Pool) -> None:
        self._pool = pool

    def __await__(self) -> Generator[Any, None, Pool]:
        return self._ready().__await__()

    async def _ready(self) -> Pool:
        await self._pool._fill()  # noqa: SLF001
        return self._pool

    async def __aenter__(self) -> Pool:
        return await self._ready()

    async def __aexit__(
        self,
        exc_type: type[BaseException] | None,
        exc: BaseException | None,
        tb: TracebackType | None,
    ) -> None:
        self._pool.close()
        await self._pool.wait_closed()


def create_pool(
    minsize: int = 1,
    maxsize: int = 10,
    *,
    pool_recycle: float = -1,
    idle_timeout: float | None = None,
    pre_ping: float | None = 30.0,
    reset_on_release: bool = False,
    **connect_args: Any,
) -> _CreatePool:
    """A pool of connections (:class:`Pool`): ``pool = await
    create_pool(...)``, or ``async with create_pool(...) as pool``.

    ``minsize`` connections are opened first (their errors raise here) and
    kept open; more are opened as needed up to ``maxsize``.
    ``connect_args`` are :func:`rustmy.aio.connect`'s.
    """
    return _CreatePool(
        Pool(
            minsize,
            maxsize,
            pool_recycle=pool_recycle,
            idle_timeout=idle_timeout,
            pre_ping=pre_ping,
            reset_on_release=reset_on_release,
            **connect_args,
        )
    )
