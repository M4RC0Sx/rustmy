"""sql_mode, multi_statements, conv= and the protocol constants."""

from __future__ import annotations

import datetime as dt
import enum
import json

from decimal import Decimal
from typing import TYPE_CHECKING
from typing import Any

import pytest

import rustmy

from rustmy.constants import CLIENT
from rustmy.constants import CR
from rustmy.constants import ER
from rustmy.constants import FIELD_TYPE
from rustmy.constants import FLAG
from rustmy.constants import SERVER_STATUS
from rustmy.converters import conversions
from rustmy.converters import split_conv
from rustmy.converters import to_date
from rustmy.converters import to_datetime
from rustmy.converters import to_timedelta
from rustmy.cursors import DictCursor
from rustmy.cursors import SSCursor


if TYPE_CHECKING:
    from tests.conftest import Server


_DIGITS = " UNION ALL ".join(f"SELECT {d} AS d" for d in range(10))
#: Ten million rows: long enough for a statement timeout to hit.
SLOW_COUNT = "SELECT COUNT(*) FROM " + ", ".join(
    f"({_DIGITS}) AS t{i}" for i in range(7)
)


# Constants ---------------------------------------------------------------------


def test_constants_match_the_protocol() -> None:
    assert CLIENT.FOUND_ROWS == 2
    assert CLIENT.MULTI_STATEMENTS == 1 << 16
    assert SERVER_STATUS.AUTOCOMMIT == SERVER_STATUS.SERVER_STATUS_AUTOCOMMIT == 2
    assert FLAG.BINARY == 128
    assert FIELD_TYPE.NEWDECIMAL == rustmy.FieldType.NEWDECIMAL == 246
    assert CR.CONN_HOST_ERROR == 2003
    assert ER.DUP_ENTRY == 1062


def test_error_codes(server: Server, conn: rustmy.Connection) -> None:
    cursor = conn.cursor()
    cursor.execute("CREATE TEMPORARY TABLE t (id INT PRIMARY KEY, n INT CHECK (n > 0))")
    cursor.execute("INSERT INTO t VALUES (1, 1)")
    cases = [
        ("INSERT INTO t VALUES (1, 1)", ER.DUP_ENTRY),
        ("SELECT * FROM no_such_table", ER.NO_SUCH_TABLE),
        ("SELECT nope FROM t", ER.BAD_FIELD_ERROR),
        ("SELEC 1", ER.PARSE_ERROR),
        (
            "INSERT INTO t VALUES (2, -1)",
            ER.CHECK_CONSTRAINT_VIOLATED
            if server.name == "mysql"
            else ER.CONSTRAINT_FAILED,
        ),
        (
            f"SELECT /*+ MAX_EXECUTION_TIME(20) */ * FROM ({SLOW_COUNT}) AS c"
            if server.name == "mysql"
            else f"SET STATEMENT max_statement_time = 0.02 FOR {SLOW_COUNT}",
            ER.QUERY_TIMEOUT if server.name == "mysql" else ER.STATEMENT_TIMEOUT,
        ),
    ]
    for sql, code in cases:
        with pytest.raises(rustmy.DatabaseError) as excinfo:
            cursor.execute(sql)
        assert excinfo.value.args[0] == code, sql


def test_found_rows_counts_matched_rows(server: Server) -> None:
    for flag, expected in ((0, 0), (CLIENT.FOUND_ROWS, 1)):
        with server.connect(client_flag=flag) as conn:
            cursor = conn.cursor()
            cursor.execute("CREATE TEMPORARY TABLE t (n INT)")
            cursor.execute("INSERT INTO t VALUES (1)")
            # Same value: no row changes, one row matches.
            assert cursor.execute("UPDATE t SET n = 1") == expected


# multi_statements and sql_mode --------------------------------------------------


def test_multi_statements_is_off_by_default(
    server: Server, conn: rustmy.Connection
) -> None:
    with pytest.raises(rustmy.ProgrammingError) as excinfo:
        conn.cursor().execute("SELECT 1; SELECT 2")
    assert excinfo.value.args[0] == ER.PARSE_ERROR
    with server.connect(multi_statements=True) as multi:
        cursor = multi.cursor()
        cursor.execute("SELECT 1; SELECT 2")
        assert cursor.fetchall() == [(1,)]
        assert cursor.nextset()
        assert cursor.fetchall() == [(2,)]


def _sql_mode(conn: rustmy.Connection) -> set[str]:
    cursor = conn.cursor()
    cursor.execute("SELECT @@sql_mode")
    row = cursor.fetchone()
    assert row is not None
    return set(row[0].split(","))


def test_sql_mode_survives_reset_and_change_user(server: Server) -> None:
    mode = "ANSI_QUOTES,NO_BACKSLASH_ESCAPES"
    with server.connect(sql_mode=mode) as conn:
        assert _sql_mode(conn) == set(mode.split(","))
        cursor = conn.cursor()
        # ANSI_QUOTES: double quotes delimit identifiers.
        with pytest.raises(rustmy.ProgrammingError) as excinfo:
            cursor.execute('SELECT "no_such_column"')
        assert excinfo.value.args[0] == ER.BAD_FIELD_ERROR
        # Escaping follows NO_BACKSLASH_ESCAPES.
        cursor.execute("SELECT %s", ("a\\b'c",))
        assert cursor.fetchone() == ("a\\b'c",)
        conn.reset_connection()
        assert _sql_mode(conn) == set(mode.split(","))
        conn.change_user("rustmy", "rustmy", "rustmy_test")
        assert _sql_mode(conn) == set(mode.split(","))


# conv: decoders -----------------------------------------------------------------


MIXED = (
    "SELECT CAST(1.25 AS DECIMAL(5, 2)) AS d, 42 AS i, 'text' AS s, "
    "CAST('2026-11-15 10:20:30.5' AS DATETIME(6)) AS ts, "
    "CAST('-25:00:01' AS TIME) AS t, NULL AS n"
)


def test_conv_defaults_change_nothing(server: Server, conn: rustmy.Connection) -> None:
    assert split_conv(conversions) == (None, None)
    cursor = conn.cursor()
    cursor.execute(MIXED)
    expected = cursor.fetchall()
    with server.connect(conv=conversions) as other:
        cursor = other.cursor()
        cursor.execute(MIXED)
        assert cursor.fetchall() == expected


def test_conv_decoders_get_str_or_bytes(server: Server) -> None:
    calls: list[tuple[str, object]] = []

    def record(name: str) -> Any:
        def decode(value: object) -> object:
            calls.append((name, value))
            return (name, value)

        return decode

    conv = {
        **conversions,
        FIELD_TYPE.NEWDECIMAL: float,
        FIELD_TYPE.TIME: record("time"),
        FIELD_TYPE.LONGLONG: record("int"),
        FIELD_TYPE.LONG: record("int"),
        FIELD_TYPE.VAR_STRING: record("text"),
        FIELD_TYPE.NULL: record("null"),
    }
    with server.connect(conv=conv) as conn:
        cursor = conn.cursor()
        cursor.execute(MIXED)
        row = cursor.fetchone()
    assert row == (
        1.25,
        ("int", b"42"),
        ("text", b"text"),
        dt.datetime(2026, 11, 15, 10, 20, 30, 500000),
        ("time", "-25:00:01"),
        None,
    )
    # NULL is passed through without calling the decoder.
    assert [name for name, _ in calls] == ["int", "text", "time"]


def test_conv_json_text_and_flag_lists(server: Server) -> None:
    conv = {
        **conversions,
        FIELD_TYPE.JSON: json.loads,
        # TEXT and BLOB columns share a type; the binary flag tells them apart.
        FIELD_TYPE.BLOB: [(FLAG.BINARY, bytes.hex), (None, bytes.upper)],
    }
    with server.connect(conv=conv) as conn:
        cursor = conn.cursor()
        cursor.execute("CREATE TEMPORARY TABLE t (j JSON, txt TEXT, bin BLOB)")
        cursor.execute(
            "INSERT INTO t VALUES (%s, %s, %s)", ('{"a": [1, 2]}', "abc", b"\x01\xff")
        )
        cursor.execute("SELECT j, txt, bin FROM t")
        row = cursor.fetchone()
    assert row is not None
    # MariaDB reports JSON as LONGTEXT (BLOB type): it gets the BLOB entry.
    if server.name == "mysql":
        assert row[0] == {"a": [1, 2]}
    assert row[1:] == (b"ABC", "01ff")


def test_conv_in_streaming_and_dict_cursors(server: Server) -> None:
    conv = {**conversions, FIELD_TYPE.NEWDECIMAL: float}
    sql = "SELECT CAST(2.5 AS DECIMAL(4, 1)) AS d UNION ALL SELECT 3.5"
    with server.connect(conv=conv) as conn:
        streamed = conn.cursor(SSCursor)
        streamed.execute(sql)
        assert list(streamed) == [(2.5,), (3.5,)]
        dicts = conn.cursor(DictCursor)
        dicts.execute(sql)
        assert dicts.fetchall() == [{"d": 2.5}, {"d": 3.5}]


# conv: encoders -----------------------------------------------------------------


class Color(enum.Enum):
    RED = "red"
    GREEN = "green"


def test_conv_encoders(server: Server) -> None:
    seen: list[object] = []

    def encode_color(value: Color, conv: object) -> str:
        seen.append(conv)
        return f"'{value.value}'"

    conv: dict[Any, Any] = {
        **conversions,
        Color: encode_color,
        bool: lambda value, _: "'yes'" if value else "'no'",
    }
    with server.connect(conv=conv) as conn:
        cursor = conn.cursor()
        cursor.execute("SELECT %s, %s, %s", (Color.GREEN, True, 7))
        assert cursor.fetchone() == ("green", "yes", 7)
        assert seen == [conv]
        assert cursor.mogrify("SELECT %s", ([Color.RED, False],)) == (
            "SELECT ('red','no')"
        )
        # executemany's batched INSERT uses them too.
        cursor.execute("CREATE TEMPORARY TABLE t (c VARCHAR(10))")
        cursor.executemany("INSERT INTO t VALUES (%s)", [(Color.RED,), (Color.GREEN,)])
        cursor.execute("SELECT c FROM t ORDER BY c")
        assert cursor.fetchall() == [("green",), ("red",)]


def test_conv_encoder_must_return_text(server: Server) -> None:
    with server.connect(conv={Color: lambda value, _: value}) as conn:
        with pytest.raises(rustmy.ProgrammingError, match="must return str or bytes"):
            conn.cursor().execute("SELECT %s", (Color.RED,))
        assert conn.cursor().execute("SELECT 1") == 1


def test_unknown_types_still_fail_without_an_encoder(conn: rustmy.Connection) -> None:
    with pytest.raises(rustmy.ProgrammingError, match="unsupported parameter type"):
        conn.cursor().execute("SELECT %s", (Color.RED,))


# Default converter functions ------------------------------------------------------


def test_default_converter_functions() -> None:
    assert to_datetime("2026-11-15 10:20:30.5") == dt.datetime(
        2026, 11, 15, 10, 20, 30, 500000
    )
    assert to_datetime("0000-00-00 00:00:00") is None
    assert to_date("2026-11-15") == dt.date(2026, 11, 15)
    assert to_date("0000-00-00") is None
    assert to_timedelta("-25:00:01") == -dt.timedelta(hours=25, seconds=1)
    assert to_timedelta("838:59:59.5") == dt.timedelta(
        hours=838, minutes=59, seconds=59.5
    )
    assert to_timedelta("garbage") is None
    assert conversions[FIELD_TYPE.NEWDECIMAL] is Decimal


def test_split_conv_keeps_only_custom_entries() -> None:
    def custom(value: object) -> object:
        return value

    decoders, encoders = split_conv(
        {
            **conversions,
            FIELD_TYPE.TIME: custom,
            FIELD_TYPE.VARCHAR: str,  # MySQLdb-style default for text
            FIELD_TYPE.BLOB: [(FLAG.BINARY, bytes), (None, custom)],
            FIELD_TYPE.STRING: [(FLAG.BINARY, bytes), (None, str)],
            Color: lambda value, _: str(value),
        }
    )
    assert decoders == {
        FIELD_TYPE.TIME: custom,
        FIELD_TYPE.BLOB: [(FLAG.BINARY, None), (None, custom)],
    }
    assert encoders is not None
    assert list(encoders) == [Color]


#: Names other drivers define that no supported server has any more.
OBSOLETE_ER = {"ERROR_LAST", "SLAVE_HAS_MORE_GTIDS_THAN_MASTER", "DUMP_NOT_IMPLEMENTED"}


def _codes(module: Any) -> dict[str, int]:
    return {
        name: value
        for name in dir(module)
        if name.isupper() and isinstance(value := getattr(module, name), int)
    }


@pytest.mark.parametrize("driver", ["MySQLdb", "pymysql"])
def test_er_has_every_name_other_drivers_define(driver: str) -> None:
    theirs = _codes(pytest.importorskip(f"{driver}.constants.ER"))
    ours = _codes(ER)
    assert not set(theirs) - set(ours) - OBSOLETE_ER
    if driver == "MySQLdb":
        # PyMySQL has a few numbers wrong (HOSTNAME, USERNAME...); mysqlclient
        # agrees with the servers.
        mismatched = {
            name: (code, ours[name])
            for name, code in theirs.items()
            if name in ours and ours[name] != code
        }
        assert not mismatched


def test_er_matches_the_servers_catalogue(server: Server) -> None:
    if server.name != "mysql":
        pytest.skip(
            "MariaDB has no SQL-readable error catalogue (see scripts/generate_er.py)"
        )
    with server.connect(user="root") as conn:
        cursor = conn.cursor()
        cursor.execute(
            "SELECT ERROR_NAME, ERROR_NUMBER FROM performance_schema."
            "events_errors_summary_global_by_error "
            "WHERE ERROR_NAME IS NOT NULL AND ERROR_NUMBER < 10000"
        )
        catalogue = {name.removeprefix("ER_"): code for name, code in cursor.fetchall()}
    assert catalogue
    ours = _codes(ER)
    differs = {
        name: (code, ours.get(name))
        for name, code in catalogue.items()
        if ours.get(name) != code
    }
    # A new server version: regenerate with scripts/generate_er.py.
    assert not differs
