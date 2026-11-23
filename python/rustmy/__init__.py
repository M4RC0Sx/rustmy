"""rustmy: a fast, dependency-free DB-API 2.0 driver for MySQL/MariaDB."""

from rustmy._core import __version__
from rustmy.connections import Connection
from rustmy.cursors import Cursor
from rustmy.dbapi import BINARY
from rustmy.dbapi import DATETIME
from rustmy.dbapi import NUMBER
from rustmy.dbapi import ROWID
from rustmy.dbapi import STRING
from rustmy.dbapi import Binary
from rustmy.dbapi import Date
from rustmy.dbapi import DateFromTicks
from rustmy.dbapi import FieldType
from rustmy.dbapi import Time
from rustmy.dbapi import TimeFromTicks
from rustmy.dbapi import Timestamp
from rustmy.dbapi import TimestampFromTicks
from rustmy.err import DatabaseError
from rustmy.err import DataError
from rustmy.err import Error
from rustmy.err import IntegrityError
from rustmy.err import InterfaceError
from rustmy.err import InternalError
from rustmy.err import NotSupportedError
from rustmy.err import OperationalError
from rustmy.err import PoolTimeout
from rustmy.err import ProgrammingError
from rustmy.err import Warning  # noqa: A004  # PEP 249 mandates the name
from rustmy.pool import Pool


# PEP 249 module globals.
apilevel = "2.0"
# Threads may share the module but not connections.
threadsafety = 1
# `%s` and `%(name)s` placeholders, as expected by SQLAlchemy's MySQL dialects.
paramstyle = "format"

#: Open a connection; see :class:`Connection` for the arguments.
connect = Connection


def install_as_MySQLdb() -> None:  # noqa: N802  # the name other drivers use
    """Make ``import MySQLdb`` load rustmy (for Django and other code written
    against mysqlclient); see :mod:`rustmy._mysqldb`.
    """
    from rustmy._mysqldb import install_as_MySQLdb as install  # noqa: PLC0415

    install()


__all__ = [
    "BINARY",
    "DATETIME",
    "NUMBER",
    "ROWID",
    "STRING",
    "Binary",
    "Connection",
    "Cursor",
    "DataError",
    "DatabaseError",
    "Date",
    "DateFromTicks",
    "Error",
    "FieldType",
    "IntegrityError",
    "InterfaceError",
    "InternalError",
    "NotSupportedError",
    "OperationalError",
    "Pool",
    "PoolTimeout",
    "ProgrammingError",
    "Time",
    "TimeFromTicks",
    "Timestamp",
    "TimestampFromTicks",
    "Warning",
    "__version__",
    "apilevel",
    "connect",
    "install_as_MySQLdb",
    "paramstyle",
    "threadsafety",
]
