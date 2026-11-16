"""PEP 249 exceptions.

They are defined in the native module (so Rust can raise them without
importing Python code) and re-exported here. Every exception carries
``(code, message)`` as its ``args``.
"""

from rustmy._core import DatabaseError
from rustmy._core import DataError
from rustmy._core import Error
from rustmy._core import IntegrityError
from rustmy._core import InterfaceError
from rustmy._core import InternalError
from rustmy._core import NotSupportedError
from rustmy._core import OperationalError
from rustmy._core import ProgrammingError
from rustmy._core import Warning  # noqa: A004  # PEP 249 mandates the name


__all__ = [
    "DataError",
    "DatabaseError",
    "Error",
    "IntegrityError",
    "InterfaceError",
    "InternalError",
    "NotSupportedError",
    "OperationalError",
    "ProgrammingError",
    "Warning",
]
