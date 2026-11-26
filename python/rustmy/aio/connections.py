"""asyncio connections: :func:`rustmy.aio.connect`.

The protocol runs in Rust on the event loop's thread (``_core.AioConnection``),
driven by an :class:`asyncio.BufferedProtocol` whose callbacks are the core's
own methods: no Python frame runs per read, and no thread is involved.
Arguments, defaults and errors are those of :func:`rustmy.connect`.
"""

from __future__ import annotations

import asyncio
import contextlib
import warnings

from collections.abc import Coroutine
from typing import TYPE_CHECKING
from typing import Any
from typing import TypeVar
from typing import overload

from rustmy import _connect_args
from rustmy import _core
from rustmy import err
from rustmy.aio.cursors import Cursor
from rustmy.aio.cursors import prepared_variant
from rustmy.converters import encoders as default_encoders
from rustmy.converters import escape_string
from rustmy.converters import split_conv
from rustmy.converters import to_literal


if TYPE_CHECKING:
    from collections.abc import Awaitable
    from collections.abc import Callable
    from collections.abc import Generator
    from collections.abc import Mapping
    from types import TracebackType

    from typing_extensions import Self

    from rustmy._connect_args import PathArg
    from rustmy._core import QueryResult
    from rustmy.converters import Encoder


# Server status flags (see the protocol's SERVER_STATUS_* values).
_STATUS_AUTOCOMMIT = 1 << 1
_STATUS_NO_BACKSLASH_ESCAPES = 1 << 9

_CursorT = TypeVar("_CursorT", bound=Cursor)


class _Protocol(asyncio.BufferedProtocol):
    """Hands the loop's callbacks straight to the Rust core: bound as
    instance attributes, so no Python frame runs per read."""

    def __init__(self, core: _core.AioConnection) -> None:
        self.connection_made = core.connection_made  # type: ignore[method-assign,assignment]
        self.get_buffer = core.get_buffer  # type: ignore[method-assign,assignment]
        self.buffer_updated = core.buffer_updated  # type: ignore[method-assign,assignment]
        self.eof_received = core.eof_received  # type: ignore[method-assign]
        self.connection_lost = core.connection_lost  # type: ignore[method-assign,assignment]


class Connection:
    """An asyncio connection to a MySQL or MariaDB server
    (:func:`rustmy.aio.connect` opens one).

    One command at a time: starting another before the first is awaited
    raises :class:`~rustmy.err.ProgrammingError`. A command whose task is
    cancelled (or times out) closes the connection: the server goes on with
    the statement, and its reply would arrive in the middle of the next one.
    """

    # PEP 249 optional extension: exceptions reachable from the connection.
    Warning = err.Warning
    Error = err.Error
    InterfaceError = err.InterfaceError
    DatabaseError = err.DatabaseError
    DataError = err.DataError
    OperationalError = err.OperationalError
    IntegrityError = err.IntegrityError
    InternalError = err.InternalError
    ProgrammingError = err.ProgrammingError
    NotSupportedError = err.NotSupportedError

    def __init__(
        self,
        core: _core.AioConnection,
        args: _connect_args.ConnectArgs,
        *,
        cursorclass: type[Cursor],
        conv: Mapping[Any, Any] | None,
    ) -> None:
        self._core = core
        self._closed = False
        self.cursorclass = cursorclass
        #: Python types to SQL literal encoders (MySQLdb's attribute), from
        #: ``conv``; :meth:`escape` takes it as its mapping.
        self.encoders: dict[type, Encoder] = dict(default_encoders)
        if conv is not None:
            decoders, custom_encoders = split_conv(conv)
            core.set_converters(decoders, custom_encoders, conv)
            self.encoders.update(
                (key, value) for key, value in conv.items() if isinstance(key, type)
            )
        self._collation = args.collation
        self._set_names = args.collation is not None
        self._sql_mode = args.sql_mode
        self._init_command = args.init_command
        self._autocommit_setting = args.autocommit

    async def _run(self, command: Awaitable[Any]) -> Any:
        """Waits for ``command``. Cancelled, the connection is closed (see
        the class)."""
        try:
            return await command
        except asyncio.CancelledError:
            self._core.close()
            raise

    async def _init_session(self, *, connecting: bool = False) -> None:
        """Apply the per-session settings chosen at connect time (see
        :func:`rustmy._connect_args.session_plan`)."""
        core = self._core
        plan = _connect_args.session_plan(
            charset=core.charset,
            server_charset=core.server_charset,
            connecting=connecting,
            set_names=self._set_names,
            collation=self._collation,
            sql_mode=self._sql_mode,
            init_command=self._init_command,
            autocommit=self._autocommit_setting,
            server_autocommit=self.get_autocommit(),
        )
        if plan.settings is not None:
            await self._run(core.execute(*plan.settings))
        if plan.init_command is not None:
            await self._run(core.query(plan.init_command))
        if plan.autocommit is not None:
            await self.autocommit(plan.autocommit)

    async def _query(self, sql: str) -> QueryResult:
        result: QueryResult = await self._run(self._core.query(sql))
        return result

    @overload
    def cursor(
        self, cursorclass: None = None, *, cursor: None = None, prepared: bool = False
    ) -> Cursor: ...

    @overload
    def cursor(
        self,
        cursorclass: type[_CursorT],
        *,
        cursor: None = None,
        prepared: bool = False,
    ) -> _CursorT: ...

    @overload
    def cursor(
        self,
        cursorclass: None = None,
        *,
        cursor: type[_CursorT],
        prepared: bool = False,
    ) -> _CursorT: ...

    def cursor(
        self,
        cursorclass: type[Cursor] | None = None,
        *,
        cursor: type[Cursor] | None = None,
        prepared: bool = False,
    ) -> Cursor:
        """Create a cursor of ``cursorclass``, by default the one given to
        ``connect()``; as :meth:`rustmy.connections.Connection.cursor`."""
        if cursor is not None:
            if cursorclass is not None:
                msg = "pass cursorclass or cursor, not both"
                raise TypeError(msg)
            cursorclass = cursor
        chosen = cursorclass or self.cursorclass
        if prepared:
            chosen = prepared_variant(chosen)
        return chosen(self)

    async def begin(self) -> None:
        await self._query("BEGIN")

    async def commit(self) -> None:
        await self._query("COMMIT")

    async def rollback(self) -> None:
        await self._query("ROLLBACK")

    async def autocommit(self, on: bool) -> None:  # noqa: FBT001  # aiomysql-compatible
        """Enable or disable autocommit mode."""
        if self.get_autocommit() != on:
            await self._query("SET autocommit=1" if on else "SET autocommit=0")

    def get_autocommit(self) -> bool:
        return bool(self._core.server_status & _STATUS_AUTOCOMMIT)

    @property
    def no_backslash_escapes(self) -> bool:
        """Whether the session has ``NO_BACKSLASH_ESCAPES`` in its SQL mode."""
        return bool(self._core.server_status & _STATUS_NO_BACKSLASH_ESCAPES)

    def escape_string(self, value: str) -> str:
        return escape_string(value, no_backslash_escapes=self.no_backslash_escapes)

    def literal(self, value: object) -> str:
        """``value`` as an SQL literal for this connection."""
        return self._core.literal(value)

    def escape(self, value: object, mapping: Mapping[Any, Any] | None = None) -> str:
        """``value`` as an SQL literal; as
        :meth:`rustmy.connections.Connection.escape`."""
        if mapping is not None and mapping is not self.encoders:
            encoder = mapping.get(type(value))
            if encoder is not None and encoder is not to_literal:
                quoted = encoder(value, mapping)
                if isinstance(quoted, bytes):
                    return quoted.decode(self.encoding)
                return str(quoted)
        return self.literal(value)

    async def ping(self, reconnect: bool = False) -> None:  # noqa: FBT001, FBT002
        """Check that the server is reachable; raises if it is not.

        ``reconnect`` (aiomysql's) is accepted for compatibility but not
        supported: a connection that broke is not reopened.
        """
        if reconnect:
            msg = "ping(reconnect=True) is not supported: open a new connection"
            raise err.NotSupportedError(0, msg)
        await self._run(self._core.ping())

    async def select_db(self, database: str) -> None:
        """Make ``database`` the default database of the session."""
        await self._run(self._core.select_db(database))

    async def reset_connection(self) -> None:
        """Return the session to its just-connected state; as
        :meth:`rustmy.connections.Connection.reset_connection`."""
        await self._run(self._core.reset_connection())
        await self._init_session()

    async def change_user(
        self, user: str, password: str = "", database: str | None = None
    ) -> None:
        """Re-authenticate as another account on this connection; as
        :meth:`rustmy.connections.Connection.change_user`."""
        await self._run(self._core.change_user(user, password, database))
        await self._init_session()

    async def set_character_set(
        self, charset: str, collation: str | None = None
    ) -> None:
        """Switch the session to ``charset`` (and ``collation``, if given);
        as :meth:`rustmy.connections.Connection.set_character_set`."""
        await self._run(self._core.set_character_set(charset, collation))
        self._collation = collation
        self._set_names = True

    def close(self) -> None:
        """Close the connection, sending ``COM_QUIT`` first when no command
        is running. Closing it again raises :exc:`InterfaceError`, as PEP 249
        asks (leaving an ``async with`` block does not)."""
        if self._closed:
            raise err.InterfaceError(0, "connection already closed")
        self._closed = True
        self._core.close()

    def __del__(self) -> None:
        # The event loop keeps the protocol (the core) alive, so nothing else
        # would close a connection dropped while open.
        core = getattr(self, "_core", None)
        if core is None or self._closed or core.closed:
            return
        warnings.warn(f"Unclosed connection {self!r}", ResourceWarning, stacklevel=2)
        self._closed = True
        # With the loop closed, its transport closes the socket when freed.
        with contextlib.suppress(RuntimeError):
            core.close()

    async def ensure_closed(self) -> None:
        """Close the connection (aiomysql's and asyncmy's name)."""
        if not self._closed:
            self.close()

    @property
    def closed(self) -> bool:
        return self._closed or self._core.closed

    @property
    def tls(self) -> bool:
        """Whether the connection is encrypted with TLS."""
        return self._core.tls

    @property
    def compression(self) -> str | None:
        """Compression in use: ``"zlib"``, ``"zstd"`` or ``None``."""
        return self._core.compression

    @property
    def open(self) -> bool:
        return not self._core.closed

    def thread_id(self) -> int:
        """Server-side connection id."""
        return self._core.connection_id

    def get_server_info(self) -> str:
        return self._core.server_version

    def character_set_name(self) -> str:
        """The session's character set, as MySQL names it."""
        return self._core.charset

    @property
    def encoding(self) -> str:
        """Python codec of the session's character set."""
        return self._core.encoding

    def _decode_statement(self, statement: bytes) -> str:
        return self._core.decode_statement(statement)

    async def __aenter__(self) -> Self:
        return self

    async def __aexit__(
        self,
        exc_type: type[BaseException] | None,
        exc: BaseException | None,
        tb: TracebackType | None,
    ) -> None:
        if not self._closed:
            self.close()


class _Connect(Coroutine[Any, Any, "Connection"]):
    """What :func:`connect` returns: a coroutine (awaited, or given to
    ``asyncio.create_task``, it opens the connection) that also works with
    ``async with``, closing the connection at the end of the block, as
    aiomysql's and asyncmy's ``connect()`` do. Nothing starts until it is
    awaited or entered."""

    __slots__ = ("_conn", "_coro", "_open")

    def __init__(self, open_: Callable[[], Coroutine[Any, Any, Connection]]) -> None:
        self._open = open_
        self._coro: Coroutine[Any, Any, Connection] | None = None
        self._conn: Connection | None = None

    def _started(self) -> Coroutine[Any, Any, Connection]:
        if self._coro is None:
            self._coro = self._open()
        return self._coro

    def send(self, value: Any) -> Any:
        return self._started().send(value)

    def throw(
        self, typ: Any, val: Any = None, tb: TracebackType | None = None, /
    ) -> Any:
        if val is None and tb is None:
            return self._started().throw(typ)
        return self._started().throw(typ, val, tb)

    def close(self) -> None:
        if self._coro is not None:
            self._coro.close()

    def __await__(self) -> Generator[Any, None, Connection]:
        return self._started().__await__()

    async def __aenter__(self) -> Connection:
        self._conn = await self._started()
        return self._conn

    async def __aexit__(
        self,
        exc_type: type[BaseException] | None,
        exc: BaseException | None,
        tb: TracebackType | None,
    ) -> None:
        conn, self._conn = self._conn, None
        if conn is not None:
            await conn.__aexit__(exc_type, exc, tb)


def connect(  # noqa: PLR0917  # MySQLdb's positional order
    host: str | None = None,
    user: str | None = None,
    password: str | None = None,
    database: str | None = None,
    port: int | None = None,
    unix_socket: PathArg | None = None,
    *,
    cursorclass: type[Cursor] = Cursor,
    conv: Mapping[Any, Any] | None = None,
    **options: Any,
) -> _Connect:
    """Open a connection on the running event loop: ``conn = await
    connect(...)``, or ``async with connect(...) as conn``, which closes it
    at the end of the block.

    Takes the arguments of :func:`rustmy.connect`, with the same defaults and
    meaning (``passwd``, ``db``, ``ssl=``, option files, ``init_command``,
    ``autocommit``...), in the same positional order. ``connect_timeout``
    (default 15 seconds, ``0`` for none) bounds opening the connection,
    TLS and authentication included. ``cursorclass`` is one of
    :mod:`rustmy.aio.cursors`.
    """
    return _Connect(
        lambda: _open(
            host,
            user,
            password,
            database,
            port,
            unix_socket,
            cursorclass=cursorclass,
            conv=conv,
            **options,
        )
    )


async def _open(  # noqa: PLR0917
    host: str | None,
    user: str | None,
    password: str | None,
    database: str | None,
    port: int | None,
    unix_socket: PathArg | None,
    *,
    cursorclass: type[Cursor],
    conv: Mapping[Any, Any] | None,
    **options: Any,
) -> Connection:
    args = _connect_args.resolve(
        host=host,
        user=user,
        password=password,
        database=database,
        port=port,
        unix_socket=unix_socket,
        **options,
    )
    core_args = args.core
    loop = asyncio.get_running_loop()
    core = _core.AioConnection(loop, **core_args)
    connected = core.connected()

    async def handshake() -> None:
        socket = core_args["unix_socket"]
        if socket is not None:
            await loop.create_unix_connection(lambda: _Protocol(core), socket)
        else:
            await loop.create_connection(
                lambda: _Protocol(core), core_args["host"], core_args["port"]
            )
        if connected is not None:
            await connected

    timeout = core_args["connect_timeout"]
    try:
        await asyncio.wait_for(handshake(), timeout)
    except asyncio.TimeoutError:
        core.close()
        target = core_args["unix_socket"] or f"{core_args['host']}:{core_args['port']}"
        raise err.OperationalError(
            2003, f"Can't connect to MySQL server on '{target}' (timed out)"
        ) from None
    except OSError as exc:
        core.close()
        target = core_args["unix_socket"] or f"{core_args['host']}:{core_args['port']}"
        raise err.OperationalError(
            2003, f"Can't connect to MySQL server on '{target}' ({exc})"
        ) from None
    except BaseException:
        core.close()
        raise
    connection = Connection(core, args, cursorclass=cursorclass, conv=conv)
    try:
        await connection._init_session(connecting=True)  # noqa: SLF001
    except BaseException:
        connection.close()
        raise
    return connection
