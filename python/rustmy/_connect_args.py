"""``connect()``'s arguments, resolved once for both connections: the
blocking one (:class:`rustmy.connections.Connection`) and the asyncio one.

:func:`resolve` turns the arguments into what the core takes (aliases,
option files, TLS, compression, defaults) and the session settings applied
after connecting; :func:`session_plan` decides the statements that apply
them, which each connection then runs its own way.
"""

from __future__ import annotations

import os

from dataclasses import dataclass
from typing import TYPE_CHECKING
from typing import Any
from typing import TypeAlias

from rustmy import _core
from rustmy import _option_files
from rustmy import _tls
from rustmy import err
from rustmy.constants import CLIENT


if TYPE_CHECKING:
    from collections.abc import Mapping
    from os import PathLike


PathArg: TypeAlias = "str | PathLike[str]"

#: Seconds ``connect()`` waits by default for the server to accept and
#: authenticate the connection.
DEFAULT_CONNECT_TIMEOUT = 15.0

#: Character sets whose handshake collation id some supported server does
#: not know (MariaDB has no gb18030): it would keep its own default without
#: an error. Every other character set the handshake sets exactly as
#: ``SET NAMES`` would, on all supported servers
#: (``test_charsets.py::test_the_handshake_sets_the_character_set``).
HANDSHAKE_MAY_IGNORE = frozenset({"gb18030"})


def _with_option_file(
    path: PathArg | None, group: str | None, given: dict[str, Any]
) -> dict[str, Any]:
    """``given`` (the ``connect()`` arguments) with the ones left as ``None``
    taken from the option file; flags in the file add to the arguments.
    """
    try:
        from_file = _option_files.connect_args(_option_files.read(path, group))
    except ValueError as exc:
        raise err.ProgrammingError(0, str(exc)) from None
    merged = dict(given)
    for name, value in from_file.items():
        if name in {"multi_statements", "local_infile"}:
            merged[name] = bool(given[name]) or bool(value)
        elif name == "compress":
            merged[name] = given[name] or value
        elif name == "found_rows":
            if value:
                merged["client_flag"] = given["client_flag"] | CLIENT.FOUND_ROWS
        elif name in given and given[name] is None:
            merged[name] = value
    return merged


def timeout(seconds: float | None) -> float | None:
    """``connect(connect_timeout=…)`` as the core takes it: the default
    when not given, and ``None`` (no limit) for ``0``.
    """
    if seconds is None:
        return DEFAULT_CONNECT_TIMEOUT
    return None if seconds == 0 else seconds


def compression(compress: object) -> str | None:
    """``connect(compress=…)`` as the algorithm to ask for."""
    if isinstance(compress, str):
        if compress in {"zlib", "zstd"}:
            return compress
        msg = f"invalid compress {compress!r}; expected True, 'zlib' or 'zstd'"
        raise ValueError(msg)
    # True, or a number as mysqlclient takes it.
    return "zlib" if compress else None


@dataclass(frozen=True)
class ConnectArgs:
    """``connect()``'s arguments, resolved."""

    #: Keyword arguments of the core's connection (``_core.connect`` and
    #: ``_core.AioConnection`` take the same).
    core: dict[str, Any]
    collation: str | None
    sql_mode: str | None
    init_command: str | None
    autocommit: bool | None


def resolve(
    *,
    host: str | None = None,
    user: str | None = None,
    password: str | None = None,
    database: str | None = None,
    port: int | None = None,
    unix_socket: PathArg | None = None,
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
) -> ConnectArgs:
    """``connect()``'s arguments (documented on
    :class:`rustmy.connections.Connection`), resolved."""
    if passwd is not None:
        password = passwd
    if db is not None:
        database = db
    if read_default_file is not None or read_default_group is not None:
        args = _with_option_file(
            read_default_file,
            read_default_group,
            {
                "host": host,
                "user": user,
                "password": password,
                "database": database,
                "port": port,
                "unix_socket": unix_socket,
                "charset": charset,
                "connect_timeout": connect_timeout,
                "init_command": init_command,
                "server_public_key": server_public_key,
                "get_server_public_key": get_server_public_key,
                "multi_statements": multi_statements,
                "client_flag": client_flag,
                "compress": compress,
                "zstd_level": zstd_level,
                "local_infile": local_infile,
                "local_infile_dir": local_infile_dir,
                # An ``ssl`` dict says all there is to say about TLS.
                **(
                    {}
                    if ssl is not None
                    else {
                        "ssl_mode": ssl_mode,
                        "ssl_ca": ssl_ca,
                        "ssl_cert": ssl_cert,
                        "ssl_key": ssl_key,
                    }
                ),
            },
        )
        host, user, password = args["host"], args["user"], args["password"]
        database, port = args["database"], args["port"]
        unix_socket, charset = args["unix_socket"], args["charset"]
        connect_timeout = args["connect_timeout"]
        init_command = args["init_command"]
        server_public_key = args["server_public_key"]
        get_server_public_key = args["get_server_public_key"]
        multi_statements = args["multi_statements"]
        client_flag = args["client_flag"]
        compress, zstd_level = args["compress"], args["zstd_level"]
        local_infile = args["local_infile"]
        local_infile_dir = args["local_infile_dir"]
        if ssl is None:
            ssl_mode, ssl_ca = args["ssl_mode"], args["ssl_ca"]
            ssl_cert, ssl_key = args["ssl_cert"], args["ssl_key"]
    tls = _tls.resolve(
        ssl_mode=ssl_mode,
        ssl_ca=ssl_ca,
        ssl_cert=ssl_cert,
        ssl_key=ssl_key,
        ssl=ssl,
    )
    core = {
        "host": "localhost" if host is None else host,
        "port": 3306 if port is None else port,
        "user": "" if user is None else user,
        "password": "" if password is None else password,
        "database": database,
        "unix_socket": unix_socket,
        "connect_timeout": timeout(connect_timeout),
        "read_timeout": read_timeout,
        "write_timeout": write_timeout,
        "client_flag": client_flag
        | (CLIENT.MULTI_STATEMENTS if multi_statements else 0),
        "charset": "utf8mb4" if charset is None else charset,
        "ssl_mode": tls.mode,
        "ssl_ca": tls.ca,
        "ssl_cert": tls.cert,
        "ssl_key": tls.key,
        "server_public_key": (
            None if server_public_key is None else os.fspath(server_public_key)
        ),
        "get_server_public_key": (
            get_server_public_key is None or bool(get_server_public_key)
        ),
        "binary_prefix": binary_prefix,
        "statement_cache_size": statement_cache_size,
        "compress": compression(compress),
        "zstd_level": 3 if zstd_level is None else zstd_level,
        "local_infile": bool(local_infile),
        "local_infile_dir": local_infile_dir,
    }
    return ConnectArgs(
        core=core,
        collation=collation,
        sql_mode=sql_mode,
        init_command=init_command,
        autocommit=autocommit,
    )


@dataclass(frozen=True)
class SessionPlan:
    """The statements that apply a session's settings, in order."""

    #: One ``SET`` for the character set, ``sql_mode`` and ``autocommit``,
    #: with its parameters; ``None`` when there is nothing to set.
    settings: tuple[str, tuple[object, ...] | None] | None
    #: ``init_command``, run after the ``SET``.
    init_command: str | None
    #: ``autocommit`` to switch to last (after ``init_command``, which may
    #: change it), or ``None``.
    autocommit: bool | None


def session_plan(
    *,
    charset: str,
    server_charset: str | None,
    connecting: bool,
    set_names: bool,
    collation: str | None,
    sql_mode: str | None,
    init_command: str | None,
    autocommit: bool | None,
    server_autocommit: bool,
) -> SessionPlan:
    """The statements that apply the settings chosen at connect time, in as
    few round trips as possible.

    After a reset, a character set other than the server's own is set
    again: MySQL's reset returns the session to the server's default
    character set rather than the one negotiated at connect time (MariaDB
    keeps it).
    """
    settings: list[str] = []
    params: list[object] = []
    if set_names or (
        charset != server_charset
        and (not connecting or charset in HANDSHAKE_MAY_IGNORE)
    ):
        settings.append(_core.names_clause(charset, collation))
    if sql_mode is not None:
        settings.append("sql_mode = %s")
        params.append(sql_mode)
    if (
        init_command is None
        and autocommit is not None
        and server_autocommit != autocommit
    ):
        settings.append("autocommit = 1" if autocommit else "autocommit = 0")
        autocommit = None
    return SessionPlan(
        settings=("SET " + ", ".join(settings), tuple(params) or None)
        if settings
        else None,
        init_command=init_command,
        autocommit=autocommit,
    )
