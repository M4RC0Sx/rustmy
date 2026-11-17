"""``install_as_MySQLdb()`` and the MySQLdb connection API Django uses."""

from __future__ import annotations

import datetime as dt
import importlib
import sys

from typing import TYPE_CHECKING
from typing import Any

import pytest

import rustmy
import rustmy.constants
import rustmy.converters
import rustmy.cursors

from rustmy.constants import CLIENT
from rustmy.constants import FIELD_TYPE


if TYPE_CHECKING:
    from collections.abc import Iterator

    from tests.conftest import Server


@pytest.fixture
def mysqldb() -> Iterator[Any]:
    """``MySQLdb`` installed for the test; ``sys.modules`` restored after."""
    saved = {
        name: module
        for name, module in sys.modules.items()
        if name.split(".")[0] == "MySQLdb"
    }
    for name in saved:
        del sys.modules[name]
    try:
        rustmy.install_as_MySQLdb()
        yield importlib.import_module("MySQLdb")
    finally:
        installed = [n for n in sys.modules if n.split(".")[0] == "MySQLdb"]
        for name in installed:
            del sys.modules[name]
        sys.modules.update(saved)


def test_alias_modules_are_rustmy_s(mysqldb: Any) -> None:
    import MySQLdb  # noqa: PLC0415
    import MySQLdb.constants.FIELD_TYPE  # noqa: PLC0415

    from MySQLdb.constants import CLIENT as ALIAS_CLIENT  # noqa: PLC0415
    from MySQLdb.converters import conversions  # noqa: PLC0415
    from MySQLdb.cursors import DictCursor  # noqa: PLC0415

    assert MySQLdb is mysqldb
    assert ALIAS_CLIENT is CLIENT
    assert MySQLdb.constants.FIELD_TYPE is FIELD_TYPE
    # The same objects, not copies: an unchanged conv keeps the fast path.
    assert conversions is rustmy.converters.conversions
    assert DictCursor is rustmy.cursors.DictCursor
    assert MySQLdb.Connection is rustmy.Connection
    assert MySQLdb.OperationalError is rustmy.OperationalError
    with pytest.raises(ModuleNotFoundError):
        importlib.import_module("MySQLdb._mysql")


@pytest.mark.usefixtures("mysqldb")
def test_alias_version() -> None:
    import MySQLdb  # noqa: PLC0415

    # Django refuses mysqlclient older than 2.2.1.
    assert MySQLdb.version_info >= (2, 2, 1)
    assert MySQLdb.__version__ == "2.2.1"
    # Only the alias carries the compatibility version.
    assert not hasattr(rustmy, "version_info")


def test_install_twice(mysqldb: Any) -> None:
    del mysqldb
    rustmy.install_as_MySQLdb()
    import MySQLdb  # noqa: PLC0415
    import MySQLdb.converters  # noqa: PLC0415

    assert MySQLdb.converters is rustmy.converters
    assert sys.modules["MySQLdb.converters"] is rustmy.converters


def _typecast_time(value: str) -> dt.time:
    """Django's ``typecast_time`` (TIME as ``datetime.time``)."""
    hour, minute, second = value.split(":")
    seconds, _, micros = second.partition(".")
    return dt.time(int(hour), int(minute), int(seconds), int(micros.ljust(6, "0")))


@pytest.mark.usefixtures("mysqldb")
def test_django_style_connection(server: Server) -> None:
    import MySQLdb  # noqa: PLC0415

    from MySQLdb.converters import conversions  # noqa: PLC0415

    conn = MySQLdb.connect(
        host=server.host,
        port=server.port,
        user="rustmy",
        password="rustmy",
        database="rustmy_test",
        charset="utf8mb4",
        conv={**conversions, FIELD_TYPE.TIME: _typecast_time},
        client_flag=CLIENT.FOUND_ROWS,
    )
    try:
        conn.autocommit(True)  # noqa: FBT003  # MySQLdb's signature
        conn.ping()
        cursor = conn.cursor()
        cursor.execute("SELECT CAST('12:34:56.5' AS TIME(6)), %s", ("x",))
        assert cursor.fetchone() == (dt.time(12, 34, 56, 500000), "x")
        assert cursor._executed == "SELECT CAST('12:34:56.5' AS TIME(6)), 'x'"  # noqa: SLF001
    finally:
        conn.close()


@pytest.mark.parametrize(
    ("value", "expected"),
    [
        ("it's", "'it\\'s'"),
        (3, "3"),
        (None, "NULL"),
        (True, "1"),
        (b"\x00a", "_binary'\\0a'"),
        (dt.date(2026, 1, 2), "'2026-01-02'"),
    ],
)
def test_escape_with_the_connection_encoders(
    conn: rustmy.Connection, value: object, expected: str
) -> None:
    assert conn.escape(value, conn.encoders) == expected
    assert conn.escape(value) == conn.literal(value) == expected


def test_escape_with_a_custom_mapping(server: Server) -> None:
    mapping = {**rustmy.converters.conversions, str: lambda _value, _conv: b"'CUSTOM'"}
    with server.connect(conv=mapping) as conn:
        assert conn.encoders[str] is mapping[str]
        # bytes from an encoder come back as str, as Django expects.
        assert conn.escape("x", conn.encoders) == "'CUSTOM'"
        assert conn.escape(1, conn.encoders) == "1"
        # Where the mapping has the default encoder, literal() decides, and
        # it applies the connection's conv.
        assert conn.escape("x", rustmy.converters.encoders) == "'CUSTOM'"


def test_alias_connect_has_no_binary_prefix(mysqldb: Any, server: Server) -> None:
    kwargs = {
        "host": server.host,
        "port": server.port,
        "user": "rustmy",
        "password": "rustmy",
        "database": "rustmy_test",
    }
    assert mysqldb.Connect is mysqldb.connect
    with mysqldb.connect(**kwargs) as conn:
        assert isinstance(conn, rustmy.Connection)
        # As mysqlclient: Django writes `_binary %s` for binary values.
        assert conn.literal(b"a'") == "'a\\''"
        cursor = conn.cursor()
        cursor.execute("CREATE TEMPORARY TABLE t (b LONGBLOB, n INT UNSIGNED NOT NULL)")
        data = bytes(range(256))
        cursor.execute("INSERT INTO t VALUES (_binary %s, 0)", (data,))
        cursor.execute("SELECT b FROM t WHERE b = _binary %s", (data,))
        assert cursor.fetchall() == ((data,),)
        # Django's MySQL backend turns these OperationalErrors into
        # IntegrityError: an unsigned field going below zero.
        with pytest.raises(mysqldb.OperationalError) as raised:
            cursor.execute("UPDATE t SET n = n - 1")
        assert raised.value.args[0] == 1690
    with mysqldb.connect(binary_prefix=True, **kwargs) as conn:
        assert conn.literal(b"a") == "_binary'a'"
