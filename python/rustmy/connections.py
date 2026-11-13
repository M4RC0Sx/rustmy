"""PEP 249 connections."""

from __future__ import annotations

from typing import TYPE_CHECKING

from rustmy import _core
from rustmy import err
from rustmy.converters import escape_string
from rustmy.converters import literal
from rustmy.cursors import Cursor


if TYPE_CHECKING:
    from os import PathLike
    from types import TracebackType

    from typing_extensions import Self

    from rustmy._core import QueryResult


# Server status flags (see the protocol's SERVER_STATUS_* values).
_STATUS_AUTOCOMMIT = 1 << 1
_STATUS_NO_BACKSLASH_ESCAPES = 1 << 9

_SUPPORTED_CHARSETS = frozenset({"utf8mb4", "utf8", "utf8mb3"})


class Connection:
    """A connection to a MySQL or MariaDB server.

    Not thread-safe: share the module between threads, not connections.
    Using one connection from two threads at once raises
    :class:`~rustmy.err.ProgrammingError` instead of corrupting the stream.
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

    # Positional order matches MySQLdb: (host, user, password, database, port,
    # unix_socket); existing code relies on it.
    def __init__(  # noqa: PLR0917
        self,
        host: str = "localhost",
        user: str = "",
        password: str = "",
        database: str | None = None,
        port: int = 3306,
        unix_socket: str | PathLike[str] | None = None,
        *,
        passwd: str | None = None,
        db: str | None = None,
        charset: str = "utf8mb4",
        connect_timeout: float | None = None,
        read_timeout: float | None = None,
        write_timeout: float | None = None,
        autocommit: bool | None = False,
        init_command: str | None = None,
        client_flag: int = 0,
        cursorclass: type[Cursor] = Cursor,
    ) -> None:
        """Open a connection.

        ``passwd`` and ``db`` are accepted as aliases of ``password`` and
        ``database`` for compatibility with other MySQL drivers.
        ``autocommit=None`` keeps the server's default.
        """
        if charset.lower() not in _SUPPORTED_CHARSETS:
            msg = f"charset {charset!r} is not supported yet; use utf8mb4"
            raise err.NotSupportedError(0, msg)
        self._conn = _core.connect(
            host=host,
            port=port,
            user=user,
            password=password if passwd is None else passwd,
            database=database if db is None else db,
            unix_socket=unix_socket,
            connect_timeout=connect_timeout,
            read_timeout=read_timeout,
            write_timeout=write_timeout,
            client_flag=client_flag,
        )
        self.cursorclass = cursorclass
        try:
            if init_command is not None:
                self._query(init_command.encode())
            if autocommit is not None:
                self.autocommit(autocommit)
        except BaseException:
            self._conn.close()
            raise

    def _query(self, sql: bytes) -> QueryResult:
        return self._conn.query(sql)

    def _next_result(self) -> QueryResult | None:
        return self._conn.next_result()

    def cursor(self, cursorclass: type[Cursor] | None = None) -> Cursor:
        return (cursorclass or self.cursorclass)(self)

    def commit(self) -> None:
        self._query(b"COMMIT")

    def rollback(self) -> None:
        self._query(b"ROLLBACK")

    def autocommit(self, on: bool) -> None:  # noqa: FBT001  # MySQLdb-compatible
        """Enable or disable autocommit mode."""
        if self.get_autocommit() != on:
            self._query(b"SET autocommit=1" if on else b"SET autocommit=0")

    def get_autocommit(self) -> bool:
        return bool(self._conn.server_status & _STATUS_AUTOCOMMIT)

    @property
    def no_backslash_escapes(self) -> bool:
        """Whether the session has ``NO_BACKSLASH_ESCAPES`` in its SQL mode."""
        return bool(self._conn.server_status & _STATUS_NO_BACKSLASH_ESCAPES)

    def escape_string(self, value: str) -> str:
        return escape_string(value, no_backslash_escapes=self.no_backslash_escapes)

    def literal(self, value: object) -> str:
        return literal(value, no_backslash_escapes=self.no_backslash_escapes)

    def ping(self) -> None:
        """Check that the server is reachable; raises if it is not."""
        self._conn.ping()

    def close(self) -> None:
        """Close the connection. Calling it again is a no-op."""
        self._conn.close()

    @property
    def open(self) -> bool:
        return not self._conn.closed

    def thread_id(self) -> int:
        """Server-side connection id."""
        return self._conn.connection_id

    def get_server_info(self) -> str:
        return self._conn.server_version

    def character_set_name(self) -> str:
        return "utf8mb4"

    def __enter__(self) -> Self:
        return self

    def __exit__(
        self,
        exc_type: type[BaseException] | None,
        exc: BaseException | None,
        tb: TracebackType | None,
    ) -> None:
        self.close()
