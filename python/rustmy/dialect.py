"""SQLAlchemy dialects: ``mysql+rustmy://`` and ``mariadb+rustmy://``.

Registered through the ``sqlalchemy.dialects`` entry points, so nothing needs
importing: ``create_engine("mysql+rustmy://user:password@host/db")``. Needs
SQLAlchemy 2.0 or later (``pip install rustmy[sqlalchemy]``).

Built on SQLAlchemy's mysqlclient dialect, whose DB-API rustmy follows, with
these differences:

- URL query arguments use rustmy's names (``user``, ``password``,
  ``database``, ``ssl_mode``, ``collation``…); timeouts may be fractional.
  ``cursorclass`` names a class of :mod:`rustmy.cursors`, e.g.
  ``?cursorclass=PreparedCursor`` for prepared statements.
- No ``SET NAMES`` on every new connection: the handshake already sets the
  character set, with the server's default collation for it, and the
  statement would override an explicit ``collation``.
- A connection closed under SQLAlchemy (``InterfaceError``, "connection
  already closed") counts as a disconnect, so the pool replaces it.
"""

from __future__ import annotations

from typing import TYPE_CHECKING
from typing import Any

from sqlalchemy import util
from sqlalchemy.dialects.mysql.mariadb import MariaDBDialect
from sqlalchemy.dialects.mysql.mysqldb import MySQLDialect_mysqldb

import rustmy
import rustmy.cursors

from rustmy.cursors import SSCursor


if TYPE_CHECKING:
    from collections.abc import Callable
    from types import ModuleType

    from sqlalchemy.engine.interfaces import ConnectArgsType
    from sqlalchemy.engine.interfaces import DBAPIConnection
    from sqlalchemy.engine.interfaces import DBAPICursor
    from sqlalchemy.engine.interfaces import DBAPIModule
    from sqlalchemy.engine.interfaces import PoolProxiedConnection
    from sqlalchemy.engine.url import URL


#: Query arguments rustmy takes as floats (mysqlclient's dialect makes them
#: integers) and as booleans (the URL gives strings).
_FLOAT_ARGS = ("connect_timeout", "read_timeout", "write_timeout")
_BOOL_ARGS = ("autocommit", "multi_statements")


def _cursor_class(name: str) -> type[rustmy.Cursor]:
    """The class of :mod:`rustmy.cursors` called ``name``."""
    found = getattr(rustmy.cursors, name, None)
    if isinstance(found, type) and issubclass(found, rustmy.Cursor):
        return found
    msg = f"cursorclass={name!r} is not a cursor class of rustmy.cursors"
    raise ValueError(msg)


class RustmyMySQLDialect(MySQLDialect_mysqldb):
    driver = "rustmy"
    supports_statement_cache = True
    supports_server_side_cursors = True
    _sscursor = SSCursor

    @classmethod
    def import_dbapi(cls) -> ModuleType:
        return rustmy

    @classmethod
    def load_provisioning(cls) -> None:
        # SQLAlchemy's test plugin imports `<dialect package>.provision`;
        # this dialect lives outside sqlalchemy.dialects.mysql.
        __import__("sqlalchemy.dialects.mysql.provision")

    def create_connect_args(
        self, url: URL, _translate_args: dict[str, Any] | None = None
    ) -> ConnectArgsType:
        if _translate_args is None:
            _translate_args = {"username": "user"}
        timeouts = {
            name: float(value)
            for name, value in url.query.items()
            if name in _FLOAT_ARGS and isinstance(value, str)
        }
        cursorclass = url.query.get("cursorclass")
        # mysqlclient's dialect would make ``compress`` a bool: keep "zstd".
        compress = url.query.get("compress")
        zstd_level = url.query.get("zstd_level")
        url = url.difference_update_query(
            [*timeouts, "cursorclass", "compress", "zstd_level"]
        )
        args, kwargs = super().create_connect_args(url, _translate_args)
        kwargs.update(timeouts)
        if isinstance(cursorclass, str):
            kwargs["cursorclass"] = _cursor_class(cursorclass)
        if isinstance(compress, str):
            kwargs["compress"] = (
                compress if compress in {"zlib", "zstd"} else util.asbool(compress)
            )
        if isinstance(zstd_level, str):
            kwargs["zstd_level"] = int(zstd_level)
        for name in _BOOL_ARGS:
            if name in kwargs:
                kwargs[name] = util.asbool(kwargs[name])
        return args, kwargs

    def on_connect(self) -> Callable[[DBAPIConnection], None]:
        # Skips mysqlclient's dialect, which sends SET NAMES on every new
        # connection: the handshake has already set the character set.
        base = super(MySQLDialect_mysqldb, self).on_connect()

        def on_connect(conn: DBAPIConnection) -> None:
            if base is not None:
                base(conn)

        return on_connect

    def is_disconnect(
        self,
        e: DBAPIModule.Error,
        connection: PoolProxiedConnection | DBAPIConnection | None,
        cursor: DBAPICursor | None,
    ) -> bool:
        if super().is_disconnect(e, connection, cursor):
            return True
        return isinstance(e, rustmy.InterfaceError) and "already closed" in str(e)


class RustmyMariaDBDialect(MariaDBDialect, RustmyMySQLDialect):
    supports_statement_cache = True
