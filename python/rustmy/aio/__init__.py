"""rustmy for asyncio: :func:`connect` opens a connection on the running
event loop, with the arguments of :func:`rustmy.connect`.

The API has the shape of aiomysql's and asyncmy's: ``await cur.execute(...)``,
``await cur.fetchall()``, ``async with conn.cursor() as cur``,
``await conn.commit()``. The protocol runs in Rust on the loop's thread.

One difference from those drivers: fetching after a statement that returned
no result set (an ``INSERT``, DDL) raises :class:`ProgrammingError`, as PEP 249
asks and as rustmy's blocking cursors do, instead of returning ``()``.
"""

from __future__ import annotations

from rustmy.aio.connections import Connection
from rustmy.aio.connections import connect
from rustmy.aio.cursors import Cursor
from rustmy.aio.cursors import DictCursor
from rustmy.aio.cursors import PreparedCursor
from rustmy.aio.cursors import PreparedDictCursor
from rustmy.aio.cursors import SSCursor
from rustmy.aio.cursors import SSDictCursor
from rustmy.err import DatabaseError
from rustmy.err import DataError
from rustmy.err import Error
from rustmy.err import IntegrityError
from rustmy.err import InterfaceError
from rustmy.err import InternalError
from rustmy.err import NotSupportedError
from rustmy.err import OperationalError
from rustmy.err import ProgrammingError
from rustmy.err import Warning  # noqa: A004


__all__ = [
    "Connection",
    "Cursor",
    "DataError",
    "DatabaseError",
    "DictCursor",
    "Error",
    "IntegrityError",
    "InterfaceError",
    "InternalError",
    "NotSupportedError",
    "OperationalError",
    "PreparedCursor",
    "PreparedDictCursor",
    "ProgrammingError",
    "SSCursor",
    "SSDictCursor",
    "Warning",
    "connect",
]
