"""PEP 249 connections."""

from __future__ import annotations

from typing import TYPE_CHECKING
from typing import Any
from typing import TypeAlias
from typing import TypeVar
from typing import overload

from rustmy import _connect_args
from rustmy import _core
from rustmy import err
from rustmy.converters import encoders as default_encoders
from rustmy.converters import escape_string
from rustmy.converters import split_conv
from rustmy.converters import to_literal
from rustmy.cursors import Cursor
from rustmy.cursors import prepared_variant


if TYPE_CHECKING:
    from collections.abc import Iterable
    from collections.abc import Mapping
    from os import PathLike
    from types import TracebackType

    from typing_extensions import Self

    from rustmy._core import QueryResult
    from rustmy._core import StreamedResult
    from rustmy.converters import Encoder
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
        host: str | None = None,
        user: str | None = None,
        password: str | None = None,
        database: str | None = None,
        port: int | None = None,
        unix_socket: PathArg | None = None,
        *,
        passwd: str | None = None,
        db: str | None = None,
        charset: str | None = None,
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
        get_server_public_key: bool | None = None,
        binary_prefix: bool = True,
        statement_cache_size: int = 32,
        compress: bool | str = False,
        zstd_level: int | None = None,
        local_infile: bool = False,
        local_infile_dir: PathArg | None = None,
        read_default_file: PathArg | None = None,
        read_default_group: str | None = None,
    ) -> None:
        """Open a connection.

        ``host`` defaults to ``localhost``, ``port`` to 3306 and ``user`` and
        ``password`` to empty. ``passwd`` and ``db`` are accepted as aliases
        of ``password`` and ``database`` for compatibility with other MySQL
        drivers. ``autocommit=None`` keeps the server's default.

        ``read_default_file`` reads connection options from a MySQL option
        file (``my.cnf``): its ``[client]`` group and, if given,
        ``read_default_group``. With only ``read_default_group``, the
        default option files are read (``/etc/my.cnf``, ``~/.my.cnf``…).
        Arguments passed here win over the file. Used: ``host``, ``port``,
        ``user``, ``password``, ``database``, ``socket``,
        ``default-character-set``, ``init-command``, ``connect-timeout``,
        ``ssl-mode``, ``ssl-ca``, ``ssl-cert``, ``ssl-key``,
        ``server-public-key-path``, ``get-server-public-key``,
        ``multi-statements``,
        ``return-found-rows``, ``compress``, ``compression-algorithms``,
        ``zstd-compression-level``, ``local-infile`` and
        ``load-data-local-dir``; other options are ignored.

        ``connect_timeout`` (seconds, default 15) bounds opening the
        connection and every read and write until authentication ends, so a
        server that accepts the connection but never answers fails too; ``0``
        waits forever. ``read_timeout`` and ``write_timeout`` (no default)
        apply from then on. TCP connections send keepalive probes after 60
        seconds idle, so a connection a firewall or load balancer dropped
        while it sat in a pool fails within minutes instead of hanging.

        ``charset`` is the session's character set (default ``utf8mb4``;
        ``utf8`` means ``utf8mb4``): queries are encoded and text results
        decoded in it. Any MySQL character set with a Python codec works.
        Without ``collation`` the session uses the server's default collation
        for it (what its tables use, so comparisons with user variables
        work), set in the handshake at no extra cost; with ``collation``,
        ``SET NAMES … COLLATE …`` runs after connecting, in the same
        statement as ``sql_mode`` and ``autocommit``.

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

        ``bytes`` parameters go as ``_binary'…'``, so the server reads them
        as binary data whatever the session's character set;
        ``binary_prefix=False`` writes a plain ``'…'`` instead, for SQL that
        writes ``_binary %s`` itself (Django's MySQL backend; the
        ``install_as_MySQLdb()`` alias defaults to it, as mysqlclient does).
        Multi-byte legacy character sets (gbk, big5, sjis…) always get a
        hexadecimal ``X'…'``, which is binary on its own.

        ``compress=True`` (or ``"zlib"``) uses the compressed protocol with
        zlib, which pays off on slow or metered networks and costs CPU on
        fast ones; ``compress="zstd"`` uses zstd (MySQL 8.0.18+) at
        ``zstd_level`` (1 to 22, default 3), or zlib if the server offers no
        zstd (MariaDB). A server that offers no compression at all gets an
        uncompressed connection: :attr:`compression` says what was agreed.

        ``LOAD DATA LOCAL INFILE`` is off by default: the server names the
        file the client sends, so a malicious one could ask for any file.
        ``local_infile_dir`` turns it on for the files inside that directory
        (resolved when connecting, symbolic links included); a request is
        answered only if the statement being run names that very file and
        the file, resolved too, is inside the directory. Anything else is
        refused with :class:`~rustmy.err.OperationalError` 2068 and an empty
        file, without reading the disk. Relative names start at the working
        directory, as in other clients. ``local_infile=True`` is accepted for
        compatibility but needs ``local_infile_dir``: rustmy never sends
        files from anywhere.

        Prepared statements (:class:`~rustmy.cursors.PreparedCursor`) are
        prepared once per connection: the last ``statement_cache_size`` stay
        prepared on the server, which allows 16,382 across all its
        connections by default (``max_prepared_stmt_count``); when it is
        full (error 1461), the least recently used one is closed and the
        statement prepared again, once.

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
        ``get_server_public_key=False`` never asks for it, as libmysqlclient
        does by default: those accounts then need TLS, a Unix socket or a
        pinned key, and fail with :class:`~rustmy.err.OperationalError` 2061
        otherwise.
        """
        args = _connect_args.resolve(
            host=host,
            user=user,
            password=password,
            database=database,
            port=port,
            unix_socket=unix_socket,
            passwd=passwd,
            db=db,
            charset=charset,
            collation=collation,
            connect_timeout=connect_timeout,
            read_timeout=read_timeout,
            write_timeout=write_timeout,
            autocommit=autocommit,
            init_command=init_command,
            client_flag=client_flag,
            multi_statements=multi_statements,
            sql_mode=sql_mode,
            ssl_mode=ssl_mode,
            ssl_ca=ssl_ca,
            ssl_cert=ssl_cert,
            ssl_key=ssl_key,
            ssl=ssl,
            server_public_key=server_public_key,
            get_server_public_key=get_server_public_key,
            binary_prefix=binary_prefix,
            statement_cache_size=statement_cache_size,
            compress=compress,
            zstd_level=zstd_level,
            local_infile=local_infile,
            local_infile_dir=local_infile_dir,
            read_default_file=read_default_file,
            read_default_group=read_default_group,
        )
        # Set by close(), unlike `_conn.closed`, which a broken connection sets too.
        self._closed = False
        self._conn = _core.connect(**args.core)
        self.cursorclass = cursorclass
        #: Python types to SQL literal encoders (MySQLdb's attribute), from
        #: ``conv``; :meth:`escape` takes it as its mapping.
        self.encoders: dict[type, Encoder] = dict(default_encoders)
        if conv is not None:
            decoders, custom_encoders = split_conv(conv)
            self._conn.set_converters(decoders, custom_encoders, conv)
            self.encoders.update(
                (key, value) for key, value in conv.items() if isinstance(key, type)
            )
        # The handshake asks for the character set (with the server's
        # default collation for it). SET NAMES follows for an explicit
        # collation, after set_character_set(), and for a character set some
        # supported server does not know: it silently ignores the collation
        # id and keeps its default, so only SET NAMES turns that into an
        # error.
        self._collation = args.collation
        self._set_names = args.collation is not None
        self._sql_mode = args.sql_mode
        self._init_command = args.init_command
        self._autocommit_setting = args.autocommit
        try:
            self._init_session(connecting=True)
        except BaseException:
            self._conn.close()
            raise

    def _init_session(self, *, connecting: bool = False) -> None:
        """Apply the per-session settings chosen at connect time (see
        :func:`rustmy._connect_args.session_plan`)."""
        conn = self._conn
        plan = _connect_args.session_plan(
            charset=conn.charset,
            server_charset=conn.server_charset,
            connecting=connecting,
            set_names=self._set_names,
            collation=self._collation,
            sql_mode=self._sql_mode,
            init_command=self._init_command,
            autocommit=self._autocommit_setting,
            server_autocommit=self.get_autocommit(),
        )
        if plan.settings is not None:
            conn.execute(*plan.settings)
        if plan.init_command is not None:
            self._query(plan.init_command)
        if plan.autocommit is not None:
            self.autocommit(plan.autocommit)

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

    def _executemany_pipelined(
        self,
        query: str,
        seq_of_args: Iterable[Params],
        max_pipeline: int,
        *,
        as_dict: bool,
        batch_bytes: int,
    ) -> tuple[QueryResult, int, bytes] | None:
        return self._conn.executemany_pipelined(
            query, seq_of_args, max_pipeline, as_dict, batch_bytes
        )

    def _executemany_pipelined_streamed(
        self,
        query: str,
        seq_of_args: Iterable[Params],
        max_pipeline: int,
        *,
        as_dict: bool,
        batch_bytes: int,
    ) -> tuple[StreamedResult, int, bytes] | None:
        return self._conn.executemany_pipelined_streamed(
            query, seq_of_args, max_pipeline, as_dict, batch_bytes
        )

    def _execute_prepared(
        self, query: str, args: Params, *, as_dict: bool, batch_bytes: int
    ) -> QueryResult:
        return self._conn.execute_prepared(query, args, as_dict, batch_bytes)

    def _executemany_prepared_insert(
        self, query: str, seq_of_args: Iterable[Params], max_stmt_length: int
    ) -> tuple[int, int | None, bytes | None] | None:
        return self._conn.executemany_prepared_insert(
            query, seq_of_args, max_stmt_length
        )

    def _executemany_prepared(
        self,
        query: str,
        seq_of_args: Iterable[Params],
        max_pipeline: int,
        *,
        as_dict: bool,
        batch_bytes: int,
    ) -> tuple[QueryResult, int, bytes] | None:
        return self._conn.executemany_prepared(
            query, seq_of_args, max_pipeline, as_dict, batch_bytes
        )

    def _next_result(
        self, *, as_dict: bool = False, batch_bytes: int = 4096
    ) -> QueryResult | None:
        return self._conn.next_result(as_dict, batch_bytes)

    def _next_result_streamed(
        self, *, as_dict: bool, batch_bytes: int
    ) -> StreamedResult | None:
        return self._conn.next_result_streamed(as_dict, batch_bytes)

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
        ``connect()``. ``cursor=`` is accepted as an alias (PyMySQL's name).
        ``prepared=True`` gives that class's prepared-statement version
        (:class:`~rustmy.cursors.PreparedCursor` for ``Cursor``,
        :class:`~rustmy.cursors.PreparedDictCursor` for ``DictCursor``).
        """
        if cursor is not None:
            if cursorclass is not None:
                msg = "pass cursorclass or cursor, not both"
                raise TypeError(msg)
            cursorclass = cursor
        chosen = cursorclass or self.cursorclass
        if prepared:
            chosen = prepared_variant(chosen)
        return chosen(self)

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

    def escape(self, value: object, mapping: Mapping[Any, Any] | None = None) -> str:
        """``value`` as an SQL literal (MySQLdb's ``escape``, which Django's
        migrations call with :attr:`encoders`).

        ``mapping`` is a ``conv``-style ``{type: encoder}`` dict: its encoder
        for the value's type is used when it is not the default one;
        otherwise the result is :meth:`literal`'s.
        """
        if mapping is not None and mapping is not self.encoders:
            encoder = mapping.get(type(value))
            if encoder is not None and encoder is not to_literal:
                quoted = encoder(value, mapping)
                if isinstance(quoted, bytes):
                    return quoted.decode(self.encoding)
                return str(quoted)
        return self.literal(value)

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
        self._init_session()

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
        """Close the connection.

        Closing it again raises :exc:`InterfaceError`, as PEP 249 asks of
        any use of a closed connection (leaving a ``with`` block does not).
        A connection that broke (server gone, timeout) can still be closed.
        """
        if self._closed:
            raise err.InterfaceError(0, "connection already closed")
        self._closed = True
        self._conn.close()

    @property
    def tls(self) -> bool:
        """Whether the connection is encrypted with TLS."""
        return self._conn.tls

    @property
    def compression(self) -> str | None:
        """Compression in use: ``"zlib"``, ``"zstd"`` or ``None``."""
        return self._conn.compression

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
        if not self._closed:
            self.close()
