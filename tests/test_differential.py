"""Differential tests: the same statements through rustmy and PyMySQL.

Each case runs on a fresh connection of each driver against the same server
and records an ``Outcome``: every cell with its exact type, ``description``,
``rowcount``, ``lastrowid`` and every further result set, or the error's
class and code. Both outcomes must be equal, except for the cases listed in
``INTENDED_DIFFERENCES``, where each side must match what was decided (not
``xfail``: if either driver changes, the test fails).

The reference is PyMySQL only, for now (decided with the maintainer when
this block was planned): mysqlclient needs libmysqlclient in every CI job
and would bring a second list of differences. The harness is
driver-agnostic (``DRIVERS``), so it can be added later.
"""

from __future__ import annotations

import datetime as dt

from dataclasses import dataclass
from dataclasses import field
from decimal import Decimal
from typing import TYPE_CHECKING
from typing import Any

import pymysql
import pymysql.cursors
import pytest

import rustmy

from rustmy.cursors import DictCursor
from rustmy.cursors import SSCursor


if TYPE_CHECKING:
    from collections.abc import Callable
    from collections.abc import Iterator
    from collections.abc import Sequence

    from tests.conftest import Server


Cell = tuple[str, Any]


@dataclass(frozen=True)
class Outcome:
    #: Result sets, first to last: (description, rows, rowcount).
    results: tuple[tuple[Any, tuple[Any, ...], int], ...] = ()
    lastrowid: Any = None
    #: (exception class name, first argument) when the case raised.
    error: tuple[str, Any] | None = None


@dataclass(frozen=True)
class Case:
    id: str
    sql: str
    params: Any = None
    #: Rows for executemany instead of a single execute.
    many: Sequence[Any] | None = None
    #: Statements run first, on the same connection.
    setup: tuple[str, ...] = ()
    cursor: str = "buffered"  # buffered, dict, streaming
    connect: dict[str, Any] = field(default_factory=dict)
    #: Only on this server ("mysql" or "mariadb").
    only: str | None = None
    #: Call ``sql`` as a stored procedure with these arguments instead.
    proc_args: tuple[Any, ...] | None = None
    #: A statement run afterwards on the same cursor; its results are added.
    after: str | None = None


def _cell(value: Any) -> Cell:
    if isinstance(value, float):
        return ("float", repr(value))
    return (type(value).__name__, value)


def _row(row: Any) -> tuple[Any, ...]:
    if isinstance(row, dict):
        return tuple((key, _cell(value)) for key, value in row.items())
    return tuple(_cell(value) for value in row)


def _results(cursor: Any) -> Iterator[tuple[Any, tuple[Any, ...], int]]:
    while True:
        description = None
        if cursor.description is not None:
            description = tuple(tuple(column) for column in cursor.description)
        rows = tuple(_row(row) for row in cursor.fetchall()) if description else ()
        yield description, rows, cursor.rowcount
        if not cursor.nextset():
            return


DRIVERS: dict[str, tuple[Callable[..., Any], dict[str, Any]]] = {
    "rustmy": (
        rustmy.connect,
        {"buffered": rustmy.Cursor, "dict": DictCursor, "streaming": SSCursor},
    ),
    "pymysql": (
        pymysql.connect,
        {
            "buffered": pymysql.cursors.Cursor,
            "dict": pymysql.cursors.DictCursor,
            "streaming": pymysql.cursors.SSCursor,
        },
    ),
}


def run(driver: str, server: Server, case: Case) -> Outcome:
    connect, cursors = DRIVERS[driver]
    kwargs: dict[str, Any] = {
        "host": server.host,
        "port": server.port,
        "user": "rustmy",
        "password": "rustmy",
        "database": "rustmy_test",
        **case.connect,
    }
    if driver == "rustmy":
        kwargs["ssl_mode"] = "disabled"  # PyMySQL uses plain TCP by default
    try:
        conn = connect(**kwargs)
    except Exception as exc:  # noqa: BLE001  # the outcome under test
        return Outcome(error=(type(exc).__name__, exc.args[0] if exc.args else None))
    try:
        setup = conn.cursor()
        for statement in case.setup:
            setup.execute(statement)
        cursor = conn.cursor(cursors[case.cursor])
        try:
            if case.proc_args is not None:
                cursor.callproc(case.sql, case.proc_args)
            elif case.many is not None:
                cursor.executemany(case.sql, case.many)
            else:
                cursor.execute(case.sql, case.params)
            results = tuple(_results(cursor))
            if case.after is not None:
                cursor.execute(case.after)
                results += tuple(_results(cursor))
        except Exception as exc:  # noqa: BLE001  # the outcome under test
            return Outcome(
                error=(type(exc).__name__, exc.args[0] if exc.args else None)
            )
        return Outcome(results=results, lastrowid=cursor.lastrowid)
    finally:
        conn.close()


# Cases ---------------------------------------------------------------------

TYPES_TABLE = (
    "CREATE TEMPORARY TABLE types ("
    "ti TINYINT, si SMALLINT UNSIGNED, mi MEDIUMINT, i INT, bi BIGINT, "
    "ubi BIGINT UNSIGNED, f FLOAT, d DOUBLE, dc DECIMAL(20, 6), "
    "da DATE, dtm DATETIME(6), ts TIMESTAMP(3) NULL, tm TIME(6), y YEAR, "
    "c CHAR(5), vc VARCHAR(20), tx TEXT, bn BINARY(4), vb VARBINARY(10), "
    "bl BLOB, e ENUM('a', 'b'), st SET('x', 'y'), bt BIT(10))"
)
TYPES_ROW = (
    "INSERT INTO types VALUES (-128, 65535, -8388608, 2147483647, "
    "-9223372036854775808, 18446744073709551615, 1.5, -2.25e300, "
    "-12345678901234.123456, '2024-02-29', '2024-01-02 03:04:05.000006', "
    "'2038-01-19 03:14:07.999', '-838:59:59.000000', 2155, 'ab', 'café', "
    "'text ✓', X'00FF', X'', X'DEADBEEF', 'b', 'x,y', b'1010101010')"
)
TYPES_NULLS = "INSERT INTO types () VALUES ()"

DML_TABLE = (
    "CREATE TEMPORARY TABLE dml (id INT AUTO_INCREMENT PRIMARY KEY, "
    "v VARCHAR(10) NOT NULL UNIQUE, n INT)"
)
DML_ROWS = "INSERT INTO dml (v, n) VALUES ('a', 1), ('b', 2), ('c', 3)"

ZERO_DATES = (
    "SET SESSION sql_mode = ''",
    "CREATE TEMPORARY TABLE z (d DATE, dtm DATETIME, ts TIMESTAMP NULL)",
    (
        "INSERT INTO z VALUES ('0000-00-00', '0000-00-00 00:00:00', "
        "'0000-00-00 00:00:00'), ('2024-00-10', '2024-01-00 00:00:00', NULL)"
    ),
)

CASES = [
    # Literals and expressions.
    Case("integers", "SELECT 1, -1, 18446744073709551615, -9223372036854775808"),
    Case("decimals", "SELECT 1.50, -0.000001, CAST(12.345 AS DECIMAL(10, 4))"),
    Case("doubles", "SELECT 1.5e0, -2.5e-300, PI()"),
    Case("strings", "SELECT 'abc', '', 'ñ😀', NULL"),
    Case("binary", "SELECT X'00FF', _binary'ab', CAST('x' AS BINARY)"),
    Case(
        "temporal_literals",
        "SELECT DATE '2024-02-29', TIMESTAMP '2024-01-01 12:34:56.789012', "
        "TIME '-838:59:59', TIME '12:00:00.5', TIME '00:00:00'",
    ),
    Case("json_function", "SELECT JSON_OBJECT('a', 1, 'b', 'ñ')"),
    Case("json_cast", """SELECT CAST('{"a": [1, 2]}' AS JSON)""", only="mysql"),
    Case("bit_literal", "SELECT b'101', b'1111111111111111'"),
    Case("empty_result", "SELECT 1 FROM DUAL WHERE FALSE"),
    # Every column type, with values and NULLs.
    Case("column_types", "SELECT * FROM types", setup=(TYPES_TABLE, TYPES_ROW)),
    Case("column_nulls", "SELECT * FROM types", setup=(TYPES_TABLE, TYPES_NULLS)),
    Case("zero_dates", "SELECT * FROM z", setup=ZERO_DATES),
    # Parameters of every type.
    Case("param_none_bool", "SELECT %s, %s, %s", (None, True, False)),
    Case("param_ints", "SELECT %s, %s, %s", (0, -(2**63), 2**64 - 1)),
    Case("param_floats", "SELECT %s, %s, %s", (1.5, -0.1, 1e300)),
    Case("param_decimal", "SELECT %s, %s", (Decimal("1.10"), Decimal("-1e-5"))),
    Case(
        "param_strings",
        "SELECT %s, %s, %s",
        ('it\'s "quoted" \\ back', "\x00\n\r\x1a", "ñ😀"),
    ),
    Case("param_bytes", "SELECT %s, %s", (b"\x00\xff'\\", bytearray(b"ab"))),
    Case(
        "param_temporal",
        "SELECT %s, %s, %s, %s",
        (
            dt.date(2024, 2, 29),
            dt.datetime(2024, 1, 2, 3, 4, 5, 6),
            dt.time(12, 30, 0, 500),
            dt.timedelta(days=-1, seconds=1),
        ),
    ),
    Case("param_sequence", "SELECT 1 IN %s, 4 IN %s", ((1, 2, 3), [1, 2, 3])),
    Case("param_named", "SELECT %(a)s, %(b)s, %(a)s", {"a": 1, "b": "x"}),
    Case("param_percent", "SELECT '100%%', %s", ("%s",)),
    Case("no_params_percent", "SELECT '100%'"),
    # DML: rowcount and lastrowid.
    Case(
        "insert",
        "INSERT INTO dml (v, n) VALUES ('d', 4)",
        setup=(DML_TABLE, DML_ROWS),
    ),
    Case(
        "update_unchanged",
        "UPDATE dml SET n = 1 WHERE v = 'a'",
        setup=(DML_TABLE, DML_ROWS),
    ),
    Case("update_some", "UPDATE dml SET n = n + 1", setup=(DML_TABLE, DML_ROWS)),
    Case("delete", "DELETE FROM dml WHERE n > 1", setup=(DML_TABLE, DML_ROWS)),
    Case(
        "executemany_insert",
        "INSERT INTO dml (v, n) VALUES (%s, %s)",
        many=[("x", 1), ("y", None), ("z", 3)],
        setup=(DML_TABLE,),
    ),
    Case(
        "executemany_upsert",
        "INSERT INTO dml (v, n) VALUES (%s, %s) ON DUPLICATE KEY UPDATE n = 9",
        many=[("a", 1), ("new", 2)],
        setup=(DML_TABLE, DML_ROWS),
    ),
    Case(
        "executemany_update",
        "UPDATE dml SET n = %s WHERE v = %s",
        many=[(10, "a"), (20, "b"), (30, "missing")],
        setup=(DML_TABLE, DML_ROWS),
    ),
    Case(
        "executemany_named",
        "INSERT INTO dml (v, n) VALUES (%(v)s, %(n)s)",
        many=[{"v": "p", "n": 1}, {"v": "q", "n": 2}],
        setup=(DML_TABLE,),
    ),
    # Cursors.
    Case("dict_rows", "SELECT 1 AS a, 'x' AS b", cursor="dict"),
    Case("dict_repeated_names", "SELECT 1 AS x, 2 AS x, 3 AS x", cursor="dict"),
    Case(
        "dict_repeated_table_names",
        "SELECT a.v, b.v FROM dml a JOIN dml b ON a.id = b.id ORDER BY a.id",
        setup=(DML_TABLE, DML_ROWS),
        cursor="dict",
    ),
    Case(
        "streaming",
        "SELECT id, v FROM dml ORDER BY id",
        setup=(DML_TABLE, DML_ROWS),
        cursor="streaming",
    ),
    Case(
        "streaming_dml",
        "UPDATE dml SET n = 0",
        setup=(DML_TABLE, DML_ROWS),
        cursor="streaming",
    ),
    # Errors: class and code.
    Case("syntax_error", "SELEC 1"),
    Case("unknown_table", "SELECT * FROM no_such_table"),
    Case("unknown_column", "SELECT no_such_column"),
    Case(
        "duplicate_key",
        "INSERT INTO dml (v) VALUES ('a')",
        setup=(DML_TABLE, DML_ROWS),
    ),
    Case("not_null", "INSERT INTO dml (v) VALUES (NULL)", setup=(DML_TABLE,)),
    Case(
        "data_too_long",
        "INSERT INTO dml (v) VALUES ('far too long for ten')",
        setup=("SET SESSION sql_mode = 'STRICT_ALL_TABLES'", DML_TABLE),
    ),
    Case(
        "out_of_range",
        "INSERT INTO dml (v, n) VALUES ('a', 99999999999)",
        setup=("SET SESSION sql_mode = 'STRICT_ALL_TABLES'", DML_TABLE),
    ),
    Case("multi_statements_off", "SELECT 1; SELECT 2"),
    Case("wrong_param_count", "SELECT %s, %s", (1,)),
    Case("missing_named_param", "SELECT %(a)s", {"b": 1}),
    Case("unknown_database", "SELECT 1", connect={"database": "no_such_db"}),
    Case("wrong_password", "SELECT 1", connect={"password": "wrong"}),
    # Character sets.
    Case(
        "latin1_text",
        "SELECT %s, 'Señor'",
        ("café €",),
        connect={"charset": "latin1"},
    ),
    Case(
        "latin1_undefined_bytes",
        "SELECT CONVERT(0x818D8F909D USING latin1)",
        connect={"charset": "latin1"},
    ),
    Case(
        "latin1_unencodable",
        "SELECT %s",
        ("Привет",),
        connect={"charset": "latin1"},
    ),
    Case("gbk_text", "SELECT %s", ("你好，世界",), connect={"charset": "gbk"}),  # noqa: RUF001
    Case(
        "sjis_yen_injection",
        "SELECT %s",
        ("¥' OR 1=1 -- ",),
        connect={"charset": "sjis"},
    ),
    Case(
        "euckr_uhc_syllable",
        "SELECT HEX(CONVERT(%s USING utf8mb4))",
        ("갂",),
        connect={"charset": "euckr"},
    ),
    Case("charset_missing_on_server", "SELECT 1", connect={"charset": "gb18030"}),
    # Stored procedures (created by the `procedure` fixture).
    Case(
        "callproc",
        "difftest_proc",
        proc_args=(21, None, "hi"),
        after="SELECT @_difftest_proc_0, @_difftest_proc_1, @_difftest_proc_2",
    ),
    Case("callproc_qualified", "rustmy_test.difftest_proc", proc_args=(1, None, "x")),
]

PROCEDURE = (
    "CREATE PROCEDURE difftest_proc(IN a INT, OUT doubled INT, INOUT s VARCHAR(10)) "
    "BEGIN SELECT a; SELECT CONCAT(s, '!') AS s; SET doubled = a * 2, s = UPPER(s); END"
)


@pytest.fixture(scope="module", autouse=True)
def procedure(server: Server) -> Iterator[None]:
    with server.connect() as conn:
        cursor = conn.cursor()
        cursor.execute("DROP PROCEDURE IF EXISTS difftest_proc")
        cursor.execute(PROCEDURE)
        yield
        cursor.execute("DROP PROCEDURE IF EXISTS difftest_proc")


def _first(outcome: Outcome, **changes: Any) -> Outcome:
    """``outcome`` with fields of its first result set replaced."""
    description, rows, rowcount = outcome.results[0]
    first = (
        changes.get("description", description),
        changes.get("rows", rows),
        changes.get("rowcount", rowcount),
    )
    return Outcome((first, *outcome.results[1:]), outcome.lastrowid, outcome.error)


@dataclass(frozen=True)
class Aspect:
    """The part of an outcome a difference is about: ``get`` reads it,
    ``clear`` blanks it so the rest can still be compared."""

    get: Callable[[Outcome], Any]
    clear: Callable[[Outcome], Outcome]


#: The error, or the rows of every result set when there is none.
RESULT = Aspect(
    lambda o: o.error or tuple(rows for _, rows, _ in o.results),
    lambda _: Outcome(),
)
ROWS = Aspect(lambda o: o.results[0][1], lambda o: _first(o, rows=()))
ROWCOUNT = Aspect(lambda o: o.results[0][2], lambda o: _first(o, rowcount=0))
#: Column names (and with them the rest of the description: a different
#: literal gets a different display size).
COLUMN_NAMES = Aspect(
    lambda o: tuple(column[0] for column in o.results[0][0]),
    lambda o: _first(o, description=None),
)


@dataclass(frozen=True)
class Difference:
    reason: str
    aspect: Aspect
    rustmy: Any
    pymysql: Any
    #: Only differs on this server; elsewhere both drivers agree.
    only: str | None = None


def _str(value: str) -> Cell:
    return ("str", value)


ZERO_DATE_STRINGS = (
    (_str("0000-00-00"), _str("0000-00-00 00:00:00"), _str("0000-00-00 00:00:00")),
    (_str("2024-00-10"), _str("2024-01-00 00:00:00"), ("NoneType", None)),
)

#: Differences decided on purpose, with each driver's side.
INTENDED_DIFFERENCES: dict[str, Difference] = {
    "zero_dates": Difference(
        "Invalid dates are None, as in mysqlclient (PyMySQL returns the text).",
        ROWS,
        rustmy=((("NoneType", None),) * 3,) * 2,
        pymysql=ZERO_DATE_STRINGS,
    ),
    "streaming": Difference(
        "rowcount is -1 while a streamed result is unread, as in mysqlclient "
        "(PyMySQL reports the protocol's 'unknown', 2**64 - 1).",
        ROWCOUNT,
        rustmy=-1,
        pymysql=2**64 - 1,
    ),
    "param_floats": Difference(
        "Floats are sent as the shortest exact digits with an exponent "
        "(-1e-1); PyMySQL writes repr() plus 'e0'. Same DOUBLE value: only "
        "the column name, taken from the literal, differs.",
        COLUMN_NAMES,
        rustmy=("1.5e0", "-1e-1", "1e300"),
        pymysql=("1.5e0", "-0.1e0", "1e+300"),
    ),
    "param_bytes": Difference(
        "bytes go as _binary'...' in UTF-8 sessions (shorter than X'...'); "
        "same value, the column name comes from the literal.",
        COLUMN_NAMES,
        # The server names the column from the text it could decode.
        rustmy=("'\\", "ab"),
        pymysql=("X'00ff275c'", "X'6162'"),
    ),
    "wrong_param_count": Difference(
        "Every rustmy error has (errno, message) arguments; 0 when the error "
        "is the driver's own.",
        RESULT,
        rustmy=("ProgrammingError", 0),
        pymysql=("ProgrammingError", "not enough arguments for format string"),
    ),
    "missing_named_param": Difference(
        "A missing named parameter is a ProgrammingError (PEP 249), not the "
        "KeyError of Python's % operator.",
        RESULT,
        rustmy=("ProgrammingError", 0),
        pymysql=("KeyError", "a"),
    ),
    "unknown_column": Difference(
        "Server errors are classed by SQLSTATE: class 42 (syntax or access "
        "rule violation) is ProgrammingError. PyMySQL uses its own list.",
        RESULT,
        rustmy=("ProgrammingError", 1054),
        pymysql=("OperationalError", 1054),
    ),
    "unknown_database": Difference(
        "SQLSTATE 42000: ProgrammingError (see unknown_column).",
        RESULT,
        rustmy=("ProgrammingError", 1044),
        pymysql=("OperationalError", 1044),
    ),
    "charset_missing_on_server": Difference(
        "SQLSTATE 42000: ProgrammingError (see unknown_column). Only MariaDB "
        "lacks gb18030.",
        RESULT,
        rustmy=("ProgrammingError", 1115),
        pymysql=("OperationalError", 1115),
        only="mariadb",
    ),
    "latin1_undefined_bytes": Difference(
        "latin1 is MySQL's: the five bytes cp1252 leaves undefined are the C1 "
        "controls of the same value (PyMySQL decodes with cp1252 and fails).",
        RESULT,
        rustmy=((((_str("\x81\x8d\x8f\x90\x9d"),),),)),
        pymysql=("UnicodeDecodeError", "charmap"),
    ),
    "latin1_unencodable": Difference(
        "Text the session's character set cannot represent is a DataError "
        "(PEP 249) before anything is sent.",
        RESULT,
        rustmy=("DataError", 0),
        pymysql=("UnicodeEncodeError", "charmap"),
    ),
    "sjis_yen_injection": Difference(
        "Python's shift_jis encodes ¥ as 0x5C, the server's backslash: "
        "escaping the text first turns ¥' into an escaped backslash and a "
        "quote that ends the string. rustmy refuses text that does not "
        "survive its codec both ways; PyMySQL runs the injected OR 1=1.",
        RESULT,
        rustmy=("DataError", 0),
        pymysql=(((("int", 1),),),),
    ),
    "euckr_uhc_syllable": Difference(
        "The server's euckr is the Unified Hangul Code (cp949); Python's "
        "euc_kr spells syllables outside EUC-KR as jamo sequences, which the "
        "server stores as four letters.",
        RESULT,
        rustmy=(((_str("EAB082"),),),),
        pymysql=(((_str("E385A4E384B1E3858FE384B2"),),),),
    ),
    "callproc_qualified": Difference(
        "A database-qualified name is quoted part by part (`db`.`proc`); "
        "PyMySQL quotes it whole and finds no such procedure.",
        RESULT,
        rustmy=(((("int", 1),),), (((_str("x!")),),), ()),
        pymysql=("OperationalError", 1305),
    ),
}


@pytest.mark.parametrize("case", CASES, ids=lambda case: case.id)
def test_same_outcome_as_pymysql(server: Server, case: Case) -> None:
    if case.only is not None and case.only != server.name:
        pytest.skip(f"{case.id} only applies to {case.only}")
    ours = run("rustmy", server, case)
    theirs = run("pymysql", server, case)
    difference = INTENDED_DIFFERENCES.get(case.id)
    if difference is None or difference.only not in {None, server.name}:
        assert ours == theirs
        return
    aspect = difference.aspect
    assert aspect.get(ours) == difference.rustmy
    assert aspect.get(theirs) == difference.pymysql
    assert aspect.clear(ours) == aspect.clear(theirs)


def test_intended_differences_are_cases() -> None:
    assert set(INTENDED_DIFFERENCES) <= {case.id for case in CASES}


def test_case_ids_are_unique() -> None:
    ids = [case.id for case in CASES]
    assert len(ids) == len(set(ids))
