"""SQLAlchemy's asyncio dialects over :mod:`rustmy.aio`.

``create_async_engine("mysql+rustmy://user:password@host/db")`` picks them
from the blocking dialects' URL (as psycopg's does), and
``mysql+rustmy_async://`` and ``mariadb+rustmy_async://`` name them
explicitly. They need SQLAlchemy's asyncio extra
(``pip install rustmy[sqlalchemy-asyncio]``, which brings greenlet).

They are the blocking dialects (:mod:`rustmy.dialect`: the same URL
arguments, no ``SET NAMES``, the same disconnects) with SQLAlchemy's asyncio
adaptation in between: each DB-API call awaits the asyncio connection's
coroutine through a greenlet, as SQLAlchemy's asyncmy and aiomysql dialects
do. ``?cursorclass=PreparedCursor`` names a class of
:mod:`rustmy.aio.cursors`; server-side cursors (``stream_results``) are
:class:`rustmy.aio.SSCursor`.

Works with SQLAlchemy 2.0 and 2.1, whose asyncio layers differ in how the
connection is created and awaited, and in who picks the pool.
"""

from __future__ import annotations

from typing import TYPE_CHECKING
from typing import Any

from sqlalchemy import pool
from sqlalchemy import util
from sqlalchemy.connectors import asyncio as sa_asyncio
from sqlalchemy.util import concurrency

import rustmy
import rustmy.aio.cursors

from rustmy import aio
from rustmy.constants import CLIENT
from rustmy.dialect import RustmyMariaDBDialect
from rustmy.dialect import RustmyMySQLDialect


if TYPE_CHECKING:
    from collections.abc import Awaitable
    from collections.abc import Callable

    from sqlalchemy.engine.interfaces import ConnectArgsType
    from sqlalchemy.engine.interfaces import DBAPIConnection
    from sqlalchemy.engine.url import URL


#: SQLAlchemy 2.1 creates the adapted connection with ``create()`` and awaits
#: with ``await_``; 2.0 builds it directly and awaits with ``await_only`` (or
#: ``await_fallback`` with ``?async_fallback=true``).
_SQLA_21 = hasattr(sa_asyncio.AsyncAdapt_dbapi_connection, "create")
_await: Callable[[Awaitable[Any]], Any] = (
    getattr(concurrency, "await_", None) or concurrency.await_only
)
_await_fallback: Callable[[Awaitable[Any]], Any] | None = getattr(
    concurrency, "await_fallback", None
)


class AsyncAdaptRustmyCursor(sa_asyncio.AsyncAdapt_dbapi_cursor):
    __slots__ = ()


class AsyncAdaptRustmySSCursor(
    sa_asyncio.AsyncAdapt_dbapi_ss_cursor, AsyncAdaptRustmyCursor
):
    __slots__ = ()

    def _make_new_cursor(self, connection: Any) -> Any:
        return connection.cursor(aio.SSCursor)


class AsyncAdaptRustmyConnection(
    sa_asyncio.AsyncAdapt_terminate, sa_asyncio.AsyncAdapt_dbapi_connection
):
    __slots__ = ()
    _cursor_cls = AsyncAdaptRustmyCursor
    _ss_cursor_cls = AsyncAdaptRustmySSCursor

    def _wait(self, awaitable: Awaitable[Any]) -> Any:
        # SQLAlchemy 2.0 keeps the awaiting function on the connection (the
        # fallback one with ``async_fallback``).
        wait: Callable[[Awaitable[Any]], Any] = getattr(self, "await_", _await)
        return wait(awaitable)

    def ping(self, reconnect: bool = False) -> None:  # noqa: FBT001, FBT002
        if reconnect:
            msg = "reconnect is not supported"
            raise NotImplementedError(msg)
        self._wait(self._do_ping())

    async def _do_ping(self) -> None:
        try:
            async with self._execute_mutex:
                await self._connection.ping(reconnect=False)
        except Exception as error:  # noqa: BLE001  # SQLAlchemy's handler raises it
            self._handle_exception(error)

    def character_set_name(self) -> str:
        name: str = self._connection.character_set_name()
        return name

    def autocommit(self, value: bool) -> None:  # noqa: FBT001  # MySQLdb's signature
        self._wait(self._connection.autocommit(value))

    def get_autocommit(self) -> bool:
        on: bool = self._connection.get_autocommit()
        return on

    def close(self) -> None:
        self._wait(self._connection.ensure_closed())

    async def _terminate_graceful_close(self) -> None:
        await self._connection.ensure_closed()

    def _terminate_force_close(self) -> None:
        self._connection.close()


class AsyncAdaptRustmyFallbackConnection(AsyncAdaptRustmyConnection):
    """SQLAlchemy 2.0's ``?async_fallback=true``."""

    __slots__ = ()
    await_ = staticmethod(_await_fallback or _await)  # type: ignore[assignment,unused-ignore]


class AsyncAdaptRustmyDBAPI(sa_asyncio.AsyncAdapt_dbapi_module):
    """The DB-API module SQLAlchemy sees: :mod:`rustmy`'s names, with
    ``connect`` awaiting :func:`rustmy.aio.connect`."""

    def __init__(self) -> None:
        if _SQLA_21:
            super().__init__(aio, dbapi_module=rustmy)  # type: ignore[call-arg,unused-ignore]
        self.paramstyle = "format"
        for name in (
            "Warning",
            "Error",
            "InterfaceError",
            "DataError",
            "DatabaseError",
            "OperationalError",
            "IntegrityError",
            "ProgrammingError",
            "InternalError",
            "NotSupportedError",
        ):
            setattr(self, name, getattr(rustmy, name))

    STRING = rustmy.STRING
    NUMBER = rustmy.NUMBER
    BINARY = rustmy.BINARY
    DATETIME = rustmy.DATETIME
    TIMESTAMP = rustmy.DATETIME
    Binary = staticmethod(bytes)

    def connect(self, *args: Any, **kwargs: Any) -> AsyncAdaptRustmyConnection:
        fallback = util.asbool(kwargs.pop("async_fallback", False))
        creator = kwargs.pop("async_creator_fn", aio.connect)
        if _SQLA_21:
            connection: AsyncAdaptRustmyConnection = _await(
                AsyncAdaptRustmyConnection.create(self, creator(*args, **kwargs))  # type: ignore[attr-defined,unused-ignore]
            )
            return connection
        if fallback and _await_fallback is not None:
            return AsyncAdaptRustmyFallbackConnection(
                self,  # type: ignore[arg-type,unused-ignore]
                _await_fallback(creator(*args, **kwargs)),
            )
        return AsyncAdaptRustmyConnection(
            self,  # type: ignore[arg-type,unused-ignore]
            _await(creator(*args, **kwargs)),
        )


def _cursor_class(name: str) -> type[aio.Cursor]:
    """The class of :mod:`rustmy.aio.cursors` called ``name``."""
    found = getattr(rustmy.aio.cursors, name, None)
    if isinstance(found, type) and issubclass(found, aio.Cursor):
        return found
    msg = f"cursorclass={name!r} is not a cursor class of rustmy.aio.cursors"
    raise ValueError(msg)


class RustmyAsyncMySQLDialect(RustmyMySQLDialect):
    """``mysql+rustmy_async://``, or ``mysql+rustmy://`` under
    ``create_async_engine``."""

    is_async = True
    has_terminate = True
    supports_statement_cache = True
    supports_server_side_cursors = True
    _sscursor = AsyncAdaptRustmySSCursor  # type: ignore[assignment]

    @classmethod
    def import_dbapi(cls) -> AsyncAdaptRustmyDBAPI:  # type: ignore[override]
        return AsyncAdaptRustmyDBAPI()

    @classmethod
    def get_pool_class(cls, url: URL) -> type[pool.Pool]:
        # SQLAlchemy 2.1 picks it from ``is_async``; 2.0 asks the dialect.
        if util.asbool(url.query.get("async_fallback", False)):
            fallback: type[pool.Pool] | None = getattr(
                pool, "FallbackAsyncAdaptedQueuePool", None
            )
            if fallback is not None:
                return fallback
        return pool.AsyncAdaptedQueuePool

    def create_connect_args(
        self, url: URL, _translate_args: dict[str, Any] | None = None
    ) -> ConnectArgsType:
        cursorclass = url.query.get("cursorclass")
        url = url.difference_update_query(["cursorclass"])
        args, kwargs = super().create_connect_args(url, _translate_args)
        if isinstance(cursorclass, str):
            kwargs["cursorclass"] = _cursor_class(cursorclass)
        return args, kwargs

    def _found_rows_client_flag(self) -> int:
        return CLIENT.FOUND_ROWS

    def do_terminate(self, dbapi_connection: DBAPIConnection) -> None:
        dbapi_connection.terminate()

    def get_driver_connection(self, connection: DBAPIConnection) -> Any:
        return connection._connection  # noqa: SLF001


class RustmyAsyncMariaDBDialect(RustmyMariaDBDialect, RustmyAsyncMySQLDialect):
    """``mariadb+rustmy_async://``, or ``mariadb+rustmy://`` under
    ``create_async_engine``."""

    supports_statement_cache = True
