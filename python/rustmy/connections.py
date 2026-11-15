"""PEP 249 connections."""

from __future__ import annotations

import os

from typing import TYPE_CHECKING
from typing import Any
from typing import TypeAlias
from typing import TypeVar
from typing import overload

from rustmy import _core
from rustmy import _tls
from rustmy import err
from rustmy.constants import CLIENT
from rustmy.converters import escape_string
from rustmy.converters import split_conv
from rustmy.cursors import Cursor


if TYPE_CHECKING:
    from collections.abc import Iterable
    from collections.abc import Mapping
    from os import PathLike
    from types import TracebackType

    from typing_extensions import Self

    from rustmy._core import QueryResult
    from rustmy._core import StreamedResult
    from rustmy.converters import Params


# Server status flags (see the protocol's SERVER_STATUS_* values).
_STATUS_AUTOCOMMIT = 1 << 1
_STATUS_NO_BACKSLASH_ESCAPES = 1 << 9

PathArg: TypeAlias = "str | PathLike[str]"

_CursorT = TypeVar("_CursorT", bound=Cursor)


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
        unix_socket: PathArg | None = None,
        *,
        passwd: str | None = None,
        db: str | None = None,
        charset: str = "utf8mb4",
        collation: str | None = None,
        connect_timeout: float | None = None,
        read_timeout: float | None = None,
        write_timeout: float | None = None,
        autocommit: bool | None = False,
        init_command: str | None = None,
        client_flag: int = 0,
        multi_statements: bool = False,
        sql_mode: str | None = None,
        conv: Mapping[Any, Any] | None = None,
        cursorclass: type[Cursor] = Cursor,
        ssl_mode: str | None = None,
        ssl_ca: PathArg | None = None,
        ssl_cert: PathArg | None = None,
        ssl_key: PathArg | None = None,
        ssl: Mapping[str, object] | bool | None = None,
        server_public_key: PathArg | None = None,
    ) -> None:
        """Open a connection.

        ``passwd`` and ``db`` are accepted as aliases of ``password`` and
        ``database`` for compatibility with other MySQL drivers.
        ``autocommit=None`` keeps the server's default.

        ``charset`` is the session's character set (default ``utf8mb4``;
        ``utf8`` means ``utf8mb4``): queries are encoded and text results
        decoded in it. Any MySQL character set with a Python codec works.
        Without ``collation`` the session uses the server's default collation
        for it (what its tables use, so comparisons with user variables
        work), set in the handshake at no extra cost; with ``collation``,
        ``SET NAMES … COLLATE …`` runs after connecting.

        ``multi_statements=True`` allows several statements separated by
        ``;`` in one ``execute`` (off by default: it lets an injected string
        append whole statements). ``sql_mode`` sets the session's SQL mode;
        ``init_command`` runs after it. Both are applied again after
        :meth:`reset_connection` and :meth:`change_user`.

        ``conv`` maps column types (``FIELD_TYPE``) to decoders and Python
        types to encoders, in MySQLdb's format; start from
        :data:`rustmy.converters.conversions`. Only entries that differ from
        it are called from Python, so the other columns keep the native fast
        path. A decoder gets ``str`` for ``DECIMAL`` and date/time columns and
        ``bytes`` for the rest (``None`` is passed through); a column type may
        map to ``[(FLAG mask, decoder), …]`` to choose by the column's flags.
        An encoder is called as ``encoder(value, conv)`` and returns the SQL
        literal.

        TLS: ``ssl_mode`` is one of ``disabled``, ``preferred`` (default),
        ``required`` (encrypted, server not verified), ``verify_ca`` and
        ``verify_identity``. Passing ``ssl_ca`` alone means
        ``verify_identity`` against that CA only; the verify modes without
        ``ssl_ca`` trust the operating system's certificate store.
        ``ssl_cert``/``ssl_key`` present a client certificate. ``ssl`` accepts
        the ``{"ca", "cert", "key", "check_hostname"}`` dict of other MySQL
        drivers, or ``True``/``False``.

        Without TLS, accounts using ``caching_sha2_password`` (MySQL's
        default) or ``sha256_password`` send their password RSA-encrypted
        with the server's public key, which is requested from the server as
        PyMySQL does. ``server_public_key`` (a PEM file) pins the key
        instead, so a man in the middle cannot substitute its own.
        """
        tls = _tls.resolve(
            ssl_mode=ssl_mode,
            ssl_ca=ssl_ca,
            ssl_cert=ssl_cert,
            ssl_key=ssl_key,
            ssl=ssl,
        )
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
            client_flag=client_flag
            | (CLIENT.MULTI_STATEMENTS if multi_statements else 0),
            charset=charset,
            ssl_mode=tls.mode,
            ssl_ca=tls.ca,
            ssl_cert=tls.cert,
            ssl_key=tls.key,
            server_public_key=(
                None if server_public_key is None else os.fspath(server_public_key)
            ),
        )
        self.cursorclass = cursorclass
        if conv is not None:
            decoders, encoders = split_conv(conv)
            self._conn.set_converters(decoders, encoders, conv)
        # The session gets the character set (with the server's default
        # collation for it) in the handshake; SET NAMES is only needed for
        # an explicit collation or after set_character_set().
        self._collation = collation
        self._set_names = collation is not None
        self._sql_mode = sql_mode
        self._init_command = init_command
        self._autocommit_setting = autocommit
        try:
            self._init_session()
        except BaseException:
            self._conn.close()
            raise

    def _init_session(self, *, after_reset: bool = False) -> None:
        """Apply the per-session settings chosen at connect time.

        MySQL's reset returns the session to the server's default character
        set rather than the one negotiated at connect time (MariaDB keeps
        it), so a different one is set again.
        """
        conn = self._conn
        if self._set_names or (after_reset and conn.charset != conn.server_charset):
            conn.set_character_set(conn.charset, self._collation)
        if self._sql_mode is not None:
            self._conn.execute("SET sql_mode = %s", (self._sql_mode,))
        if self._init_command is not None:
            self._query(self._init_command)
        if self._autocommit_setting is not None:
            self.autocommit(self._autocommit_setting)

    def _query(self, sql: str) -> QueryResult:
        return self._conn.query(sql)

    def _execute(
        self,
        query: str,
        args: Params,
        *,
        as_dict: bool = False,
        batch_bytes: int = 4096,
    ) -> QueryResult:
        return self._conn.execute(query, args, as_dict, batch_bytes)

    def _execute_streamed(
        self, query: str, args: Params, *, as_dict: bool, batch_bytes: int
    ) -> StreamedResult:
        return self._conn.execute_streamed(query, args, as_dict, batch_bytes)

    def _mogrify(self, query: str, args: Params) -> str:
        return self._conn.mogrify(query, args)

    def _executemany(
        self, query: str, seq_of_args: Iterable[Params], max_stmt_length: int
    ) -> tuple[int, int | None, bytes | None] | None:
        return self._conn.executemany(query, seq_of_args, max_stmt_length)

    def _next_result(
        self, *, as_dict: bool = False, batch_bytes: int = 4096
    ) -> QueryResult | None:
        return self._conn.next_result(as_dict, batch_bytes)

    def _next_result_streamed(
        self, *, as_dict: bool, batch_bytes: int
    ) -> StreamedResult | None:
        return self._conn.next_result_streamed(as_dict, batch_bytes)

    @overload
    def cursor(self, cursorclass: None = None, *, cursor: None = None) -> Cursor: ...
    @overload
    def cursor(
        self, cursorclass: type[_CursorT], *, cursor: None = None
    ) -> _CursorT: ...
    @overload
    def cursor(
        self, cursorclass: None = None, *, cursor: type[_CursorT]
    ) -> _CursorT: ...
    def cursor(
        self,
        cursorclass: type[Cursor] | None = None,
        *,
        cursor: type[Cursor] | None = None,
    ) -> Cursor:
        """Create a cursor of ``cursorclass``, by default the one given to
        ``connect()``. ``cursor=`` is accepted as an alias (PyMySQL's name).
        """
        if cursor is not None:
            if cursorclass is not None:
                msg = "pass cursorclass or cursor, not both"
                raise TypeError(msg)
            cursorclass = cursor
        return (cursorclass or self.cursorclass)(self)

    def commit(self) -> None:
        self._query("COMMIT")

    def rollback(self) -> None:
        self._query("ROLLBACK")

    def autocommit(self, on: bool) -> None:  # noqa: FBT001  # MySQLdb-compatible
        """Enable or disable autocommit mode."""
        if self.get_autocommit() != on:
            self._query("SET autocommit=1" if on else "SET autocommit=0")

    def get_autocommit(self) -> bool:
        return bool(self._conn.server_status & _STATUS_AUTOCOMMIT)

    @property
    def no_backslash_escapes(self) -> bool:
        """Whether the session has ``NO_BACKSLASH_ESCAPES`` in its SQL mode."""
        return bool(self._conn.server_status & _STATUS_NO_BACKSLASH_ESCAPES)

    def escape_string(self, value: str) -> str:
        return escape_string(value, no_backslash_escapes=self.no_backslash_escapes)

    def literal(self, value: object) -> str:
        """``value`` as an SQL literal for this connection."""
        return self._conn.literal(value)

    def ping(self) -> None:
        """Check that the server is reachable; raises if it is not."""
        self._conn.ping()

    def select_db(self, database: str) -> None:
        """Make ``database`` the default database of the session."""
        self._conn.select_db(database)

    def reset_connection(self) -> None:
        """Return the session to its just-connected state, cheaply.

        Rolls back any open transaction, drops temporary tables and clears
        user and session variables without re-authenticating, then applies
        ``init_command`` and the ``autocommit`` mode given to ``connect()``
        again. Useful for connection pools.
        """
        self._conn.reset_connection()
        self._init_session(after_reset=True)

    def change_user(
        self, user: str, password: str = "", database: str | None = None
    ) -> None:
        """Re-authenticate as another account on this connection.

        The session is reset as with :meth:`reset_connection`, then
        ``init_command`` and the ``autocommit`` mode given to ``connect()``
        are applied again.
        """
        self._conn.change_user(user, password, database)
        self._init_session()

    def close(self) -> None:
        """Close the connection. Calling it again is a no-op."""
        self._conn.close()

    @property
    def tls(self) -> bool:
        """Whether the connection is encrypted with TLS."""
        return self._conn.tls

    @property
    def open(self) -> bool:
        return not self._conn.closed

    def thread_id(self) -> int:
        """Server-side connection id."""
        return self._conn.connection_id

    def get_server_info(self) -> str:
        return self._conn.server_version

    def character_set_name(self) -> str:
        """The session's character set, as MySQL names it."""
        return self._conn.charset

    @property
    def encoding(self) -> str:
        """Python codec of the session's character set."""
        return self._conn.encoding

    def set_character_set(self, charset: str, collation: str | None = None) -> None:
        """Switch the session to ``charset`` (and ``collation``, if given).

        Queries are encoded and results decoded in it from now on, and it
        survives :meth:`reset_connection` and :meth:`change_user`.
        """
        self._conn.set_character_set(charset, collation)
        self._collation = collation
        self._set_names = True

    def _decode_statement(self, statement: bytes) -> str:
        return self._conn.decode_statement(statement)

    def __enter__(self) -> Self:
        return self

    def __exit__(
        self,
        exc_type: type[BaseException] | None,
        exc: BaseException | None,
        tb: TracebackType | None,
    ) -> None:
        self.close()
