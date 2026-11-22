"""Rows whose values cannot form reference cycles stay out of the cyclic
garbage collector, so reading a large result does not make every collection
traverse it; rows holding a container stay tracked and collectable."""

from __future__ import annotations

import gc
import weakref

from decimal import Decimal
from typing import TYPE_CHECKING
from typing import Any

import pytest

from rustmy.constants import FIELD_TYPE
from rustmy.converters import conversions
from rustmy.cursors import Cursor
from rustmy.cursors import DictCursor
from rustmy.cursors import PreparedCursor
from rustmy.cursors import PreparedDictCursor
from rustmy.cursors import SSCursor
from rustmy.cursors import SSDictCursor


if TYPE_CHECKING:
    import rustmy

    from tests.conftest import Server


# Every kind of value rustmy builds, a Decimal included (Python 3.13+ tracks
# Decimals), with a placeholder so that prepared cursors prepare it.
ATOMIC = (
    "SELECT 1 AS i, 1.5e0 AS f, CAST(1.25 AS DECIMAL(5,2)) AS d, 'text' AS s, "
    "CAST('2026-11-22 10:00:00.5' AS DATETIME(1)) AS dt, "
    "CAST('2026-11-22' AS DATE) AS da, CAST('10:00:00' AS TIME) AS t, "
    "X'00' AS b, NULL AS n, %s AS p "
    "UNION ALL SELECT 2, 2.5e0, 2.50, 'more', NULL, NULL, NULL, NULL, NULL, %s"
)

TUPLE_CURSORS = [Cursor, SSCursor, PreparedCursor]
DICT_CURSORS = [DictCursor, SSDictCursor, PreparedDictCursor]


@pytest.mark.parametrize("cursor_class", TUPLE_CURSORS, ids=lambda c: c.__name__)
def test_rows_of_atomic_values_are_untracked(
    conn: rustmy.Connection, cursor_class: type[Cursor]
) -> None:
    cursor = conn.cursor(cursor_class)
    cursor.execute(ATOMIC, (7, 8))
    rows: list[Any] = list(cursor.fetchall())
    assert len(rows) == 2
    assert [gc.is_tracked(row) for row in rows] == [False, False]
    assert isinstance(rows[0][2], Decimal)


def test_rows_holding_a_container_stay_tracked_and_collectable(server: Server) -> None:
    class Holder:
        row: Any = None

    holders: list[weakref.ref[Holder]] = []

    def decode(_value: object) -> Holder:
        holder = Holder()
        holders.append(weakref.ref(holder))
        return holder

    conv = {**conversions, FIELD_TYPE.LONG: decode, FIELD_TYPE.LONGLONG: decode}
    with server.connect(conv=conv) as conn:
        cursor = conn.cursor()
        cursor.execute("SELECT 1 AS i, 'text' AS s")
        rows = cursor.fetchall()
    assert gc.is_tracked(rows[0])
    # A cycle through the row: collected once nothing else refers to it.
    rows[0][0].row = rows[0]
    del rows, cursor
    gc.collect()
    assert [ref() for ref in holders] == [None]


@pytest.mark.parametrize("cursor_class", DICT_CURSORS, ids=lambda c: c.__name__)
def test_a_cycle_through_a_dict_row_is_collected(
    conn: rustmy.Connection, cursor_class: type[Cursor]
) -> None:
    # rustmy never untracks dict rows: since Python 3.14 an untracked dict
    # does not track itself again when a container is stored in it, and the
    # cycle below would never be collected.
    class Holder:
        row: Any = None

    cursor = conn.cursor(cursor_class)
    cursor.execute(ATOMIC, (7, 8))
    row: Any = cursor.fetchall()[0]
    holder = Holder()
    holder.row = row
    row["holder"] = holder
    ref = weakref.ref(holder)
    del row, holder, cursor
    gc.collect()
    assert ref() is None
