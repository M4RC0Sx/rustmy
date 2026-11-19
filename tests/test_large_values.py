"""Statements over one packet: split at 16 MiB as the protocol requires, and
written straight from the statement's buffer from 1 MiB up."""

from __future__ import annotations

import os

from typing import TYPE_CHECKING

import pytest

from rustmy import _core


if TYPE_CHECKING:
    from collections.abc import Iterator

    import rustmy

    from tests.conftest import Server


MAX = _core.MAX_PACKET_PAYLOAD
PREFIX = b"INSERT INTO t (v) VALUES (_binary'"
SUFFIX = b"')"


@pytest.fixture
def large(server: Server) -> Iterator[rustmy.Connection]:
    """A connection whose server accepts 64 MiB statements (MariaDB's
    default is 16 MiB), with an empty table ``t``."""
    with server.connect(user="root", database=None, autocommit=True) as root:
        cursor = root.cursor()
        cursor.execute("SELECT @@GLOBAL.max_allowed_packet")
        row = cursor.fetchone()
        assert row is not None
        before = row[0]
        cursor.execute("SET GLOBAL max_allowed_packet = %s", (64 << 20,))
        try:
            with server.connect(autocommit=True) as conn:
                conn.cursor().execute("CREATE TEMPORARY TABLE t (v LONGBLOB)")
                yield conn
        finally:
            cursor.execute("SET GLOBAL max_allowed_packet = %s", (before,))


@pytest.mark.parametrize(
    "payload",
    [1 << 20, MAX - 1, MAX, MAX + 1, 2 * MAX, 2 * MAX + 7],
    ids=["1MiB", "max-1", "max", "max+1", "2max", "2max+7"],
)
def test_statements_at_the_packet_boundaries(
    large: rustmy.Connection, payload: int
) -> None:
    # The command byte plus the statement make the payload: sizes chosen so
    # it ends exactly at, just before and just after a packet boundary.
    value = b"x" * (payload - 1 - len(PREFIX) - len(SUFFIX))
    cursor = large.cursor()
    cursor.execute((PREFIX + value + SUFFIX).decode())
    cursor.execute("SELECT LENGTH(v), v = REPEAT('x', LENGTH(v)) FROM t")
    assert cursor.fetchone() == (len(value), 1)


@pytest.mark.parametrize("prepared", [False, True], ids=["text", "prepared"])
def test_large_values_round_trip(large: rustmy.Connection, *, prepared: bool) -> None:
    value = os.urandom(MAX + (5 << 20))
    cursor = large.cursor(prepared=prepared)
    cursor.execute("INSERT INTO t (v) VALUES (%s)", (value,))
    cursor.execute("SELECT v FROM t WHERE LENGTH(v) = %s", (len(value),))
    assert cursor.fetchone() == (value,)
    # The connection carries on with ordinary statements.
    cursor.execute("SELECT %s", (1,))
    assert cursor.fetchone() == (1,)
