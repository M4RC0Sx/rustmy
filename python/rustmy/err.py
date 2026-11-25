"""PEP 249 exceptions.

They are defined in the native module (so Rust can raise them without
importing Python code) and re-exported here. Every exception carries
``(code, message)`` as its ``args``.

:class:`PoolTimeout` is rustmy's own, raised by the pools.

:class:`OutOfRangeError` (server error 1690, a numeric value out of range)
is both a :class:`DataError`, as its SQLSTATE says, and an
:class:`OperationalError`, as mysqlclient raises it: Django turns that one
into an ``IntegrityError``.
"""

from rustmy._core import DatabaseError
from rustmy._core import DataError
from rustmy._core import Error
from rustmy._core import IntegrityError
from rustmy._core import InterfaceError
from rustmy._core import InternalError
from rustmy._core import NotSupportedError
from rustmy._core import OperationalError
from rustmy._core import OutOfRangeError
from rustmy._core import ProgrammingError
from rustmy._core import Warning  # noqa: A004  # PEP 249 mandates the name


class PoolTimeout(OperationalError):  # noqa: N818  # the name pools use (psycopg_pool)
    """No connection of a pool came free in time (``acquire(timeout=…)``)."""


__all__ = [
    "DataError",
    "DatabaseError",
    "Error",
    "IntegrityError",
    "InterfaceError",
    "InternalError",
    "NotSupportedError",
    "OperationalError",
    "OutOfRangeError",
    "PoolTimeout",
    "ProgrammingError",
    "Warning",
]
