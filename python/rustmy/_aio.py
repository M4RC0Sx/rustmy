"""First cut of the asyncio connection (``rustmy.aio`` to come): the
protocol runs in Rust on the event loop's thread (``_core.AioConnection``),
driven by an :class:`asyncio.BufferedProtocol` whose callbacks are the core's
own methods. Plain TCP and Unix sockets, ``utf8mb4``, buffered cursors."""

from __future__ import annotations

import asyncio

from typing import TYPE_CHECKING
from typing import Any

from rustmy import _core


if TYPE_CHECKING:
    from types import TracebackType

    from typing_extensions import Self


class _Protocol(asyncio.BufferedProtocol):
    """Hands the loop's callbacks straight to the Rust core: bound as
    instance attributes, so no Python frame runs per read."""

    def __init__(self, core: _core.AioConnection) -> None:
        self.connection_made = core.connection_made  # type: ignore[method-assign,assignment]
        self.get_buffer = core.get_buffer  # type: ignore[method-assign,assignment]
        self.buffer_updated = core.buffer_updated  # type: ignore[method-assign,assignment]
        self.eof_received = core.eof_received  # type: ignore[method-assign]
        self.connection_lost = core.connection_lost  # type: ignore[method-assign,assignment]


class Cursor:
    """A buffered cursor: ``execute`` reads the whole result."""

    def __init__(self, conn: Connection, *, as_dict: bool = False) -> None:
        self._conn = conn
        self._as_dict = as_dict
        self._rows: tuple[Any, ...] = ()
        self._next = 0
        self.description: Any = None
        self.rowcount = -1
        self.lastrowid: int | None = None

    async def __aenter__(self) -> Self:
        return self

    async def __aexit__(
        self,
        exc_type: type[BaseException] | None,
        exc: BaseException | None,
        traceback: TracebackType | None,
    ) -> None:
        self.close()

    async def execute(self, query: str, args: object = None) -> int:
        conn = self._conn
        command = conn._core.execute(query, args, self._as_dict)  # noqa: SLF001
        description, rows, rowcount, lastrowid, _ = await conn._run(command)  # noqa: SLF001
        self.description = description
        self._rows = rows if rows is not None else ()
        self._next = 0
        self.rowcount = rowcount
        self.lastrowid = lastrowid
        return rowcount

    def fetchone(self) -> Any:
        if self._next >= len(self._rows):
            return None
        row = self._rows[self._next]
        self._next += 1
        return row

    def fetchall(self) -> tuple[Any, ...]:
        rows = self._rows[self._next :]
        self._next = len(self._rows)
        return rows

    def close(self) -> None:
        self._rows = ()


class Connection:
    def __init__(self, core: _core.AioConnection) -> None:
        self._core = core

    async def _run(
        self, command: asyncio.Future[_core.QueryResult]
    ) -> _core.QueryResult:
        """Waits for `command`. Cancelled, the connection is closed: the
        server goes on with the statement and the reply would arrive in the
        middle of the next one."""
        try:
            return await command
        except asyncio.CancelledError:
            self.close()
            raise

    def cursor(self, *, as_dict: bool = False) -> Cursor:
        return Cursor(self, as_dict=as_dict)

    def close(self) -> None:
        self._core.close()

    async def __aenter__(self) -> Self:
        return self

    async def __aexit__(
        self,
        exc_type: type[BaseException] | None,
        exc: BaseException | None,
        traceback: TracebackType | None,
    ) -> None:
        self.close()


async def connect(
    *,
    host: str = "localhost",
    port: int = 3306,
    user: str = "",
    password: str = "",
    database: str | None = None,
    unix_socket: str | None = None,
    **options: Any,
) -> Connection:
    """Opens a connection on the running event loop. ``options`` are those
    of :func:`rustmy.connect` that the core takes (``ssl_mode``, ``ssl_ca``,
    ``compress``, ``charset``...)."""
    loop = asyncio.get_running_loop()
    core = _core.AioConnection(
        loop,
        host=host,
        port=port,
        user=user,
        password=password,
        database=database,
        unix_socket=unix_socket,
        **options,
    )
    connected = core.connected()
    if unix_socket is not None:
        await loop.create_unix_connection(lambda: _Protocol(core), unix_socket)
    else:
        await loop.create_connection(lambda: _Protocol(core), host, port)
    if connected is not None:
        await connected
    return Connection(core)
