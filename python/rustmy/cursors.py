"""PEP 249 cursors."""

from __future__ import annotations

from typing import TYPE_CHECKING
from typing import Any
from typing import TypeAlias

from rustmy.converters import interpolate
from rustmy.err import ProgrammingError


if TYPE_CHECKING:
    from collections.abc import Iterable
    from collections.abc import Iterator
    from types import TracebackType

    from typing_extensions import Self

    from rustmy._core import Description
    from rustmy._core import QueryResult
    from rustmy.connections import Connection
    from rustmy.converters import Params


Row: TypeAlias = tuple[Any, ...]


class Cursor:
    """Buffered cursor: the whole result set is read when ``execute`` runs.

    Rows are fetched from the server and converted to Python objects in one
    go (with the GIL released while waiting on the network); the ``fetch*``
    methods then just hand them out.
    """

    #: Default number of rows returned by :meth:`fetchmany`.
    arraysize: int = 1

    def __init__(self, connection: Connection) -> None:
        self._connection: Connection | None = connection
        self.description: Description | None = None
        self.rowcount: int = -1
        self.lastrowid: int | None = None
        self._rows: list[Row] = []
        self._pos = 0
        self._executed: str | None = None

    @property
    def connection(self) -> Connection:
        """The connection this cursor was created from."""
        return self._get_connection()

    @property
    def rownumber(self) -> int | None:
        """Index of the next row to fetch, or ``None`` without a result set."""
        return None if self.description is None else self._pos

    def close(self) -> None:
        self._connection = None
        self._rows = []

    def __enter__(self) -> Self:
        return self

    def __exit__(
        self,
        exc_type: type[BaseException] | None,
        exc: BaseException | None,
        tb: TracebackType | None,
    ) -> None:
        self.close()

    def _get_connection(self) -> Connection:
        if self._connection is None:
            raise ProgrammingError(0, "cursor closed")
        return self._connection

    def mogrify(self, query: str, args: Params = None) -> str:
        """Return the query exactly as :meth:`execute` would send it."""
        if args is None:
            return query
        connection = self._get_connection()
        return interpolate(
            query, args, no_backslash_escapes=connection.no_backslash_escapes
        )

    def execute(self, query: str, args: Params = None) -> int:
        """Run ``query``, interpolating ``args`` if given; return the rowcount.

        Without ``args`` the query is sent as is, so ``%`` needs no escaping.
        """
        connection = self._get_connection()
        sql = self.mogrify(query, args)
        self._executed = sql
        self._set_result(connection._query(sql.encode()))  # noqa: SLF001
        return self.rowcount

    def executemany(self, query: str, seq_of_args: Iterable[Params]) -> int:
        """Run ``query`` once per item of ``seq_of_args``.

        Returns the total number of affected rows.
        """
        total = 0
        for args in seq_of_args:
            total += self.execute(query, args)
        self.rowcount = total
        return total

    def nextset(self) -> bool | None:
        """Advance to the next result set of a multi-result query."""
        result = self._get_connection()._next_result()  # noqa: SLF001
        if result is None:
            return None
        self._set_result(result)
        return True

    def _set_result(self, result: QueryResult) -> None:
        description, rows, rowcount, lastrowid, _warnings = result
        self.description = description
        self._rows = [] if rows is None else rows
        self._pos = 0
        self.rowcount = rowcount
        self.lastrowid = lastrowid

    def _check_result(self) -> None:
        if self.description is None:
            raise ProgrammingError(0, "no result set: execute a query first")

    def fetchone(self) -> Row | None:
        self._check_result()
        if self._pos >= len(self._rows):
            return None
        row = self._rows[self._pos]
        self._pos += 1
        return row

    def fetchmany(self, size: int | None = None) -> list[Row]:
        self._check_result()
        end = self._pos + (self.arraysize if size is None else size)
        rows = self._rows[self._pos : end]
        self._pos += len(rows)
        return rows

    def fetchall(self) -> list[Row]:
        self._check_result()
        if self._pos == 0:
            # Hand the list over instead of copying it.
            rows, self._rows = self._rows, []
            return rows
        rows = self._rows[self._pos :]
        self._pos = len(self._rows)
        return rows

    def __iter__(self) -> Iterator[Row]:
        return iter(self.fetchone, None)

    def setinputsizes(self, sizes: object) -> None:
        """No-op, required by PEP 249."""

    def setoutputsize(self, size: object, column: object = None) -> None:
        """No-op, required by PEP 249."""
