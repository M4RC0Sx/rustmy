"""asyncio cursors, as :mod:`rustmy.cursors` with ``await``.

``execute``, ``callproc``, ``nextset``, ``close`` and the ``fetch*`` methods
are coroutines, as in aiomysql and asyncmy: ``await cursor.fetchall()``
works the same with every cursor. Buffered cursors read the whole result in
``execute``, converting rows in batches as they arrive.
"""

from __future__ import annotations

from typing import TYPE_CHECKING
from typing import Any

from rustmy.cursors import _scroll_target
from rustmy.err import NotSupportedError
from rustmy.err import ProgrammingError


if TYPE_CHECKING:
    from collections.abc import AsyncIterator
    from collections.abc import Iterable
    from collections.abc import Sequence
    from types import TracebackType

    from typing_extensions import Self

    from rustmy._core import QueryResult
    from rustmy.aio.connections import Connection
    from rustmy.converters import Params
    from rustmy.cursors import DictRow
    from rustmy.cursors import Row


class Cursor:
    """Buffered cursor: ``execute`` reads the whole result."""

    _as_dict = False

    #: Rows :meth:`fetchmany` returns by default (PEP 249).
    arraysize = 1

    def __init__(self, connection: Connection) -> None:
        self._connection: Connection | None = connection
        self.description: Any = None
        self.rowcount = -1
        self.lastrowid: int | None = None
        self._rows: tuple[Any, ...] = ()
        self._pos = 0
        self._last: tuple[str, Params] | bytes | None = None

    @property
    def connection(self) -> Connection:
        return self._get_connection()

    @property
    def rownumber(self) -> int | None:
        return None if self.description is None else self._pos

    async def close(self) -> None:
        self._rows = ()
        self._connection = None

    async def __aenter__(self) -> Self:
        return self

    async def __aexit__(
        self,
        exc_type: type[BaseException] | None,
        exc: BaseException | None,
        tb: TracebackType | None,
    ) -> None:
        await self.close()

    def _get_connection(self) -> Connection:
        if self._connection is None:
            raise ProgrammingError(0, "cursor closed")
        return self._connection

    def _clear(self) -> None:
        """Forget the current result, so a statement that fails leaves none
        behind (fetching then raises instead of returning stale rows)."""
        self.description = None
        self._rows = ()
        self._pos = 0
        self.rowcount = -1

    def mogrify(self, query: str, args: Params = None) -> str:
        """Return the query exactly as :meth:`execute` would send it."""
        return self._get_connection()._core.mogrify(query, args)  # noqa: SLF001

    @property
    def _executed(self) -> str | None:
        """The last statement sent (built on demand)."""
        last = self._last
        if last is None:
            return None
        if isinstance(last, bytes):
            return self._get_connection()._decode_statement(last)  # noqa: SLF001
        query, args = last
        return self.mogrify(query, args)

    async def execute(self, query: str, args: Params = None) -> int:
        """Run ``query``, interpolating ``args`` if given; return the rowcount.

        Without ``args`` the query is sent as is, so ``%`` needs no escaping.
        """
        connection = self._get_connection()
        self._clear()
        self._last = (query, args)
        self._set_result(await connection._run(self._start(connection, query, args)))  # noqa: SLF001
        return self.rowcount

    def _start(self, connection: Connection, query: str, args: Params) -> Any:
        return connection._core.execute(query, args, self._as_dict)  # noqa: SLF001

    async def executemany(self, query: str, seq_of_args: Iterable[Params]) -> int:  # noqa: ARG002
        """Run ``query`` once per item of ``seq_of_args`` (not available yet
        on asyncio connections)."""
        msg = "executemany on asyncio connections"
        raise NotSupportedError(0, msg)

    async def callproc(self, procname: str, args: Sequence[Any] = ()) -> Sequence[Any]:
        """Call the stored procedure ``procname`` with ``args``; as
        :meth:`rustmy.cursors.Cursor.callproc`."""
        name = procname.replace("`", "``")
        if args:
            # The name is part of an interpolated template: keep its `%`.
            template = name.replace("%", "%%")
            assignments = ",".join(f"@`_{template}_{i}`=%s" for i in range(len(args)))
            await self.execute(f"SET {assignments}", tuple(args))
        variables = ",".join(f"@`_{name}_{i}`" for i in range(len(args)))
        quoted = "`.`".join(name.split(".", 1))
        await self.execute(f"CALL `{quoted}`({variables})")
        return args

    async def nextset(self) -> bool | None:
        """Advance to the next result set of a multi-result query."""
        connection = self._get_connection()
        result = await connection._run(  # noqa: SLF001
            connection._core.next_result(self._as_dict)  # noqa: SLF001
        )
        if result is None:
            return None
        self._set_result(result)
        return True

    def _set_result(self, result: QueryResult) -> None:
        description, rows, rowcount, lastrowid, _warnings = result
        self.description = description
        self._rows = () if rows is None else rows
        self._pos = 0
        self.rowcount = rowcount
        # PEP 249: None when the statement set no row id (it returned rows).
        self.lastrowid = None if description is not None else lastrowid

    def _check_result(self) -> None:
        if self.description is None:
            raise ProgrammingError(0, "no result set: execute a query first")

    async def fetchone(self) -> Row | None:
        self._check_result()
        if self._pos >= len(self._rows):
            return None
        row: Row = self._rows[self._pos]
        self._pos += 1
        return row

    async def fetchmany(self, size: int | None = None) -> tuple[Row, ...]:
        self._check_result()
        end = self._pos + (self.arraysize if size is None else size)
        rows = self._rows[self._pos : end]
        self._pos += len(rows)
        return rows

    async def fetchall(self) -> tuple[Row, ...]:
        self._check_result()
        # From the start, hand the (immutable) tuple itself over.
        rows = self._rows if self._pos == 0 else self._rows[self._pos :]
        self._pos = len(self._rows)
        return rows

    def scroll(self, value: int, mode: str = "relative") -> None:
        """Move to another row; as :meth:`rustmy.cursors.Cursor.scroll`."""
        self._check_result()
        target = _scroll_target(self._pos, value, mode)
        if not 0 <= target < len(self._rows):
            msg = "out of range"
            raise IndexError(msg)
        self._pos = target

    def __aiter__(self) -> AsyncIterator[Row]:
        return self

    async def __anext__(self) -> Row:
        row = await self.fetchone()
        if row is None:
            raise StopAsyncIteration
        return row

    def setinputsizes(self, sizes: object) -> None:
        """No-op, required by PEP 249."""

    def setoutputsize(self, size: object, column: object = None) -> None:
        """No-op, required by PEP 249."""


class DictCursor(Cursor):
    """Buffered cursor returning each row as a dict keyed by column name;
    as :class:`rustmy.cursors.DictCursor`."""

    _as_dict = True

    if TYPE_CHECKING:

        async def fetchone(self) -> DictRow | None: ...  # type: ignore[override]
        async def fetchmany(self, size: int | None = None) -> tuple[DictRow, ...]: ...  # type: ignore[override]
        async def fetchall(self) -> tuple[DictRow, ...]: ...  # type: ignore[override]


class PreparedCursor(Cursor):
    """Buffered cursor running server-side prepared statements; as
    :class:`rustmy.cursors.PreparedCursor`."""

    def _start(self, connection: Connection, query: str, args: Params) -> Any:
        return connection._core.execute_prepared(query, args, self._as_dict)  # noqa: SLF001


class PreparedDictCursor(PreparedCursor):
    """:class:`PreparedCursor` returning each row as a dict."""

    _as_dict = True

    if TYPE_CHECKING:

        async def fetchone(self) -> DictRow | None: ...  # type: ignore[override]
        async def fetchmany(self, size: int | None = None) -> tuple[DictRow, ...]: ...  # type: ignore[override]
        async def fetchall(self) -> tuple[DictRow, ...]: ...  # type: ignore[override]


def prepared_variant(cursorclass: type[Cursor]) -> type[Cursor]:
    """The prepared-statement version of ``cursorclass``."""
    if issubclass(cursorclass, PreparedCursor):
        return cursorclass
    if issubclass(cursorclass, DictCursor):
        return PreparedDictCursor
    if cursorclass is Cursor:
        return PreparedCursor
    msg = f"no prepared variant of {cursorclass.__name__}"
    raise ProgrammingError(0, msg)
