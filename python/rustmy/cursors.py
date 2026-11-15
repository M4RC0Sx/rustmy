"""PEP 249 cursors.

:class:`Cursor` and :class:`DictCursor` read the whole result set when
``execute`` runs; :class:`SSCursor` and :class:`SSDictCursor` stream it,
reading rows from the server in batches as they are fetched.
"""

from __future__ import annotations

from typing import TYPE_CHECKING
from typing import Any
from typing import ClassVar
from typing import TypeAlias

from rustmy.err import NotSupportedError
from rustmy.err import ProgrammingError


if TYPE_CHECKING:
    from collections.abc import Iterable
    from collections.abc import Iterator
    from collections.abc import Sequence
    from types import TracebackType

    from typing_extensions import Self

    from rustmy._core import Description
    from rustmy._core import QueryResult
    from rustmy._core import ResultStream
    from rustmy._core import StreamedResult
    from rustmy.connections import Connection
    from rustmy.converters import Params


Row: TypeAlias = tuple[Any, ...]
DictRow: TypeAlias = dict[str, Any]


class Cursor:
    """Buffered cursor: the whole result set is read when ``execute`` runs.

    Rows are fetched from the server and converted to Python objects in one
    go (with the GIL released while waiting on the network); the ``fetch*``
    methods then just hand them out.
    """

    #: Default number of rows returned by :meth:`fetchmany`.
    arraysize: int = 1

    #: Largest statement :meth:`executemany` builds when batching inserts.
    max_stmt_length: int = 256 * 1024

    #: Rows come back as dicts keyed by column name instead of tuples.
    _as_dict: ClassVar[bool] = False

    def __init__(self, connection: Connection) -> None:
        self._connection: Connection | None = connection
        self.description: Description | None = None
        self.rowcount: int = -1
        self.lastrowid: int | None = None
        self._rows: list[Any] = []
        self._pos = 0
        # The last statement: a template and its arguments (interpolated on
        # demand by ``_executed``), or the exact bytes sent.
        self._last: tuple[str, Params] | bytes | None = None

    @property
    def connection(self) -> Connection:
        """The connection this cursor was created from."""
        return self._get_connection()

    @property
    def rownumber(self) -> int | None:
        """Index of the next row to fetch, or ``None`` without a result set."""
        return None if self.description is None else self._pos

    def close(self) -> None:
        self._discard()
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

    def _discard(self) -> None:
        """Drop the current result set before another one replaces it."""

    def _clear(self) -> None:
        """Forget the current result, so a statement that fails leaves none
        behind (fetching then raises instead of returning stale rows).
        """
        self._discard()
        self.description = None
        self._rows = []
        self._pos = 0
        self.rowcount = -1

    def mogrify(self, query: str, args: Params = None) -> str:
        """Return the query exactly as :meth:`execute` would send it."""
        return self._get_connection()._mogrify(query, args)  # noqa: SLF001

    @property
    def _executed(self) -> str | None:
        """The last statement sent (built on demand)."""
        last = self._last
        if last is None:
            return None
        if isinstance(last, bytes):
            return last.decode("utf-8", "surrogateescape")
        query, args = last
        return self.mogrify(query, args)

    def execute(self, query: str, args: Params = None) -> int:
        """Run ``query``, interpolating ``args`` if given; return the rowcount.

        Without ``args`` the query is sent as is, so ``%`` needs no escaping.
        """
        connection = self._get_connection()
        self._clear()
        self._last = (query, args)
        self._set_result(
            connection._execute(query, args, as_dict=self._as_dict)  # noqa: SLF001
        )
        return self.rowcount

    def executemany(self, query: str, seq_of_args: Iterable[Params]) -> int:
        """Run ``query`` once per item of ``seq_of_args``.

        A single-row ``INSERT``/``REPLACE … VALUES (…)`` is sent as multi-row
        statements of at most :attr:`max_stmt_length` bytes; anything else
        runs row by row. Returns the total number of affected rows.
        """
        connection = self._get_connection()
        self._clear()
        batched = connection._executemany(  # noqa: SLF001
            query, seq_of_args, self.max_stmt_length
        )
        if batched is not None:
            rowcount, lastrowid, last_statement = batched
            self.description = None
            self._rows = []
            self._pos = 0
            self.rowcount = rowcount
            if lastrowid is not None:
                self.lastrowid = lastrowid
            if last_statement is not None:
                self._last = last_statement
            return rowcount
        total = 0
        for args in seq_of_args:
            total += self.execute(query, args)
        self.rowcount = total
        return total

    def callproc(self, procname: str, args: Sequence[Any] = ()) -> Sequence[Any]:
        """Call the stored procedure ``procname`` with ``args``.

        Arguments go through server variables named ``@_procname_n`` (``n``
        counting from 0), so OUT and INOUT values can be read afterwards with
        ``SELECT @_procname_0, …``. The procedure's own result sets come
        first, followed by the empty one of the ``CALL`` itself: read them
        all with :meth:`nextset`. Returns ``args`` unchanged.
        """
        name = procname.replace("`", "``")
        if args:
            # The name is part of an interpolated template: keep its `%`.
            template = name.replace("%", "%%")
            assignments = ",".join(f"@`_{template}_{i}`=%s" for i in range(len(args)))
            self.execute(f"SET {assignments}", tuple(args))
        variables = ",".join(f"@`_{name}_{i}`" for i in range(len(args)))
        self.execute(f"CALL `{name}`({variables})")
        return args

    def nextset(self) -> bool | None:
        """Advance to the next result set of a multi-result query."""
        result = self._get_connection()._next_result(  # noqa: SLF001
            as_dict=self._as_dict
        )
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
        row: Row = self._rows[self._pos]
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
        # From the start, hand the list itself over instead of copying it.
        rows = self._rows if self._pos == 0 else self._rows[self._pos :]
        self._pos = len(self._rows)
        return rows

    def scroll(self, value: int, mode: str = "relative") -> None:
        """Move to another row: ``value`` rows ahead (negative: back) in
        ``relative`` mode, row number ``value`` in ``absolute`` mode.

        Raises :class:`IndexError` if that row does not exist.
        """
        self._check_result()
        target = _scroll_target(self._pos, value, mode)
        if not 0 <= target < len(self._rows):
            msg = "out of range"
            raise IndexError(msg)
        self._pos = target

    def __iter__(self) -> Iterator[Row]:
        return iter(self.fetchone, None)

    def setinputsizes(self, sizes: object) -> None:
        """No-op, required by PEP 249."""

    def setoutputsize(self, size: object, column: object = None) -> None:
        """No-op, required by PEP 249."""


class DictCursor(Cursor):
    """Buffered cursor returning each row as a dict keyed by column name.

    A column name that repeats an earlier one becomes ``table.column`` (the
    table alias, empty for computed columns), as in other MySQL drivers.
    """

    _as_dict = True

    if TYPE_CHECKING:

        def fetchone(self) -> DictRow | None: ...  # type: ignore[override]
        def fetchmany(self, size: int | None = None) -> list[DictRow]: ...  # type: ignore[override]
        def fetchall(self) -> list[DictRow]: ...  # type: ignore[override]
        def __iter__(self) -> Iterator[DictRow]: ...  # type: ignore[override]


class SSCursor(Cursor):
    """Unbuffered cursor: rows stay on the server until they are fetched.

    For result sets too large to hold in memory, or to start processing
    before the last row arrives. Rows are read in batches of about 4 KiB
    with the GIL released, so memory use does not grow with the result set.

    Rules of the protocol, not of this driver:

    - The connection cannot run another command until every row has been
      read. One that does (from any cursor) first reads and discards the
      remaining rows, with a :class:`UserWarning`; fetching from this cursor
      afterwards raises :class:`~rustmy.err.ProgrammingError`. :meth:`close`
      (or leaving a ``with`` block) discards them silently.
    - :attr:`rowcount` is -1 for result sets: the size is unknown until the
      end.
    - :meth:`scroll` only moves forward.
    - Rows the client is slow to read keep the server waiting; past its
      ``net_write_timeout`` (60 s by default) it drops the connection.
    """

    #: Approximate bytes of rows read from the socket per batch.
    #: The smallest size on par with the best in ``benches/cursors.py``,
    #: and the only one as quick as mysqlclient to hand out the first row.
    _batch_bytes: ClassVar[int] = 4 * 1024

    def __init__(self, connection: Connection) -> None:
        super().__init__(connection)
        self._stream: ResultStream | None = None

    @property
    def rownumber(self) -> int | None:
        """Index of the next row to fetch, or ``None`` without a result set."""
        if self.description is None:
            return None
        return 0 if self._stream is None else self._stream.rownumber

    def _discard(self) -> None:
        stream, self._stream = self._stream, None
        if stream is not None:
            stream.close()

    def execute(self, query: str, args: Params = None) -> int:
        """Run ``query``, interpolating ``args`` if given; return the rowcount
        (-1 for a result set, whose size is unknown until the end).
        """
        connection = self._get_connection()
        self._clear()
        self._last = (query, args)
        self._set_stream(
            connection._execute_streamed(  # noqa: SLF001
                query, args, as_dict=self._as_dict, batch_bytes=self._batch_bytes
            )
        )
        return self.rowcount

    def nextset(self) -> bool | None:
        """Skip what is left of this result set and move to the next one."""
        connection = self._get_connection()
        self._discard()
        result = connection._next_result_streamed(  # noqa: SLF001
            as_dict=self._as_dict, batch_bytes=self._batch_bytes
        )
        if result is None:
            return None
        self._set_stream(result)
        return True

    def _set_stream(self, result: StreamedResult) -> None:
        description, stream, rowcount, lastrowid, _warnings = result
        self.description = description
        self._stream = stream
        self.rowcount = rowcount
        self.lastrowid = lastrowid

    def _active(self) -> ResultStream:
        self._check_result()
        if self._stream is None:
            raise ProgrammingError(0, "result set closed")
        return self._stream

    def fetchone(self) -> Row | None:
        row: Row | None = self._active().fetchone()
        return row

    def fetchmany(self, size: int | None = None) -> list[Row]:
        return self._active().fetchmany(self.arraysize if size is None else size)

    def fetchall(self) -> list[Row]:
        """Fetch every remaining row into a list (iterate the cursor instead
        to keep memory flat).
        """
        return self._active().fetchall()

    def fetchall_unbuffered(self) -> Iterator[Row]:
        """Iterate the remaining rows one by one, as iterating the cursor
        does (PyMySQL compatibility).
        """
        return iter(self)

    def __iter__(self) -> Iterator[Row]:
        return self._active()

    def scroll(self, value: int, mode: str = "relative") -> None:
        """Skip forward to another row (see :meth:`Cursor.scroll`).

        Skipped rows are read but never converted. Moving backwards raises
        :class:`~rustmy.err.NotSupportedError`; :class:`IndexError` means the
        result set ended first, leaving the cursor at its end.
        """
        stream = self._active()
        current = stream.rownumber
        target = _scroll_target(current, value, mode)
        if target < current:
            raise NotSupportedError(0, "unbuffered cursors cannot scroll backwards")
        if stream.skip(target - current) < target - current:
            msg = "out of range"
            raise IndexError(msg)


class SSDictCursor(SSCursor):
    """Unbuffered cursor returning each row as a dict (see :class:`SSCursor`
    and :class:`DictCursor`).
    """

    _as_dict = True

    if TYPE_CHECKING:

        def fetchone(self) -> DictRow | None: ...  # type: ignore[override]
        def fetchmany(self, size: int | None = None) -> list[DictRow]: ...  # type: ignore[override]
        def fetchall(self) -> list[DictRow]: ...  # type: ignore[override]
        def fetchall_unbuffered(self) -> Iterator[DictRow]: ...  # type: ignore[override]
        def __iter__(self) -> Iterator[DictRow]: ...  # type: ignore[override]


def _scroll_target(current: int, value: int, mode: str) -> int:
    if mode == "relative":
        return current + value
    if mode == "absolute":
        return value
    raise ProgrammingError(0, f"unknown scroll mode {mode!r}")
