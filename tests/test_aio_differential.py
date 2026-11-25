"""``rustmy.aio`` against the blocking connection: the same values, stored
and read through every kind of cursor, give the same rows, descriptions and
counts. The blocking side is checked against the server elsewhere
(``test_prepared.py``, ``test_differential.py``), so equality here carries
those checks over to asyncio."""

from __future__ import annotations

import asyncio
import uuid

from typing import TYPE_CHECKING
from typing import Any

import pytest

from hypothesis import given
from hypothesis import strategies as st

import rustmy

from rustmy import aio
from rustmy import cursors
from tests.test_prepared import COLUMNS


if TYPE_CHECKING:
    from collections.abc import Coroutine
    from collections.abc import Iterator

    from tests.conftest import Server


#: Blocking cursor class -> its asyncio counterpart.
PAIRS: list[tuple[type[rustmy.Cursor], type[aio.Cursor]]] = [
    (cursors.Cursor, aio.Cursor),
    (cursors.DictCursor, aio.DictCursor),
    (cursors.SSCursor, aio.SSCursor),
    (cursors.SSDictCursor, aio.SSDictCursor),
    (cursors.PreparedCursor, aio.PreparedCursor),
    (cursors.PreparedDictCursor, aio.PreparedDictCursor),
]


class Pair:
    """A blocking and an asyncio connection to the same server, with a
    table per column type (``v``) and a twin of each to insert into."""

    def __init__(self, server: Server) -> None:
        self.loop = asyncio.new_event_loop()
        self.conn = server.connect(autocommit=True)
        self.aconn = self.run(aio.connect(**server.connect_args(autocommit=True)))
        prefix = f"aiodiff_{uuid.uuid4().hex[:8]}"
        self.tables: dict[str, str] = {}
        cursor = self.conn.cursor()
        for i, definition in enumerate(COLUMNS):
            name = f"{prefix}_{i}"
            for table in (name, f"{name}_b"):
                cursor.execute(
                    f"CREATE TABLE {table} (id INT PRIMARY KEY, v {definition})"
                )
            self.tables[definition] = name

    def run(self, coroutine: Coroutine[Any, Any, Any]) -> Any:
        return self.loop.run_until_complete(coroutine)

    def close(self) -> None:
        cursor = self.conn.cursor()
        for name in self.tables.values():
            cursor.execute(f"DROP TABLE IF EXISTS {name}, {name}_b")
        self.conn.close()
        self.run(self.aconn.ensure_closed())
        self.loop.close()


@pytest.fixture(scope="module")
def pair(server: Server) -> Iterator[Pair]:
    both = Pair(server)
    yield both
    both.close()


def _values(definition: str) -> st.SearchStrategy[list[Any]]:
    return st.lists(COLUMNS[definition] | st.none(), max_size=12)


def _select(table: str, *, prepared: bool) -> tuple[str, tuple[int, ...] | None]:
    # Prepared cursors prepare only statements with placeholders.
    if prepared:
        return f"SELECT v, id FROM {table} WHERE id > %s ORDER BY id", (0,)
    return f"SELECT v, id FROM {table} ORDER BY id", None


def _read(conn: rustmy.Connection, cls: type[rustmy.Cursor], table: str) -> Any:
    cursor = conn.cursor(cls)
    sql, args = _select(table, prepared=issubclass(cls, cursors.PreparedCursor))
    cursor.execute(sql, args)
    first = cursor.fetchone()
    rows = [first, *cursor.fetchmany(3), *cursor]
    cursor.close()
    return cursor.description, rows


async def _aread(conn: aio.Connection, cls: type[aio.Cursor], table: str) -> Any:
    cursor = conn.cursor(cls)
    sql, args = _select(table, prepared=issubclass(cls, aio.PreparedCursor))
    await cursor.execute(sql, args)
    first = await cursor.fetchone()
    rows = [first, *await cursor.fetchmany(3)]
    rows += [row async for row in cursor]
    await cursor.close()
    return cursor.description, rows


@pytest.mark.parametrize("definition", list(COLUMNS))
@given(data=st.data())
def test_reads_match(pair: Pair, definition: str, data: Any) -> None:
    table = pair.tables[definition]
    values = data.draw(_values(definition))
    cursor = pair.conn.cursor()
    cursor.execute(f"DELETE FROM {table}")
    if values:
        cursor.executemany(
            f"INSERT INTO {table} VALUES (%s, %s)", list(enumerate(values, 1))
        )
    for blocking, asyncio_class in PAIRS:
        expected = _read(pair.conn, blocking, table)
        assert pair.run(_aread(pair.aconn, asyncio_class, table)) == expected, (
            blocking.__name__
        )


@pytest.mark.parametrize("definition", list(COLUMNS))
@pytest.mark.parametrize("prepared", [False, True], ids=["text", "prepared"])
@given(data=st.data())
def test_writes_match(pair: Pair, definition: str, prepared: bool, data: Any) -> None:  # noqa: FBT001
    """``execute`` and ``executemany`` store what the blocking ones store:
    the same rows, row counts and statements."""
    table = pair.tables[definition]
    twin = f"{table}_b"
    values = data.draw(_values(definition).filter(bool))
    rows = list(enumerate(values, 1))
    sql = "INSERT INTO {} VALUES (%s, %s)"
    cursor = pair.conn.cursor(prepared=prepared)
    cursor.execute(f"DELETE FROM {table}")
    cursor.execute(sql.format(table), rows[0])
    one = cursor.rowcount
    many = cursor.executemany(sql.format(table), rows[1:]) if rows[1:] else None

    async def write() -> tuple[int, int | None]:
        acursor = pair.aconn.cursor(prepared=prepared)
        await acursor.execute(f"DELETE FROM {twin}")
        await acursor.execute(sql.format(twin), rows[0])
        aone = acursor.rowcount
        amany = (
            await acursor.executemany(sql.format(twin), rows[1:]) if rows[1:] else None
        )
        return aone, amany

    assert pair.run(write()) == (one, many)
    read = pair.conn.cursor()
    read.execute(f"SELECT id, v FROM {table} ORDER BY id")
    expected = read.fetchall()
    read.execute(f"SELECT id, v FROM {twin} ORDER BY id")
    assert read.fetchall() == expected


@pytest.mark.parametrize("definition", list(COLUMNS))
@given(data=st.data())
def test_mogrify_matches(pair: Pair, definition: str, data: Any) -> None:
    values = data.draw(_values(definition))
    sql = "SELECT " + ", ".join(["%s"] * len(values)) if values else "SELECT 1"
    expected = pair.conn.cursor().mogrify(sql, values or None)
    assert pair.aconn.cursor().mogrify(sql, values or None) == expected
