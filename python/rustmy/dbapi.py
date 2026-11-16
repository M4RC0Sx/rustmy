"""PEP 249 type objects and constructors."""

import datetime as dt
import time


class FieldType:
    """Column type codes found in ``cursor.description[i][1]``."""

    DECIMAL = 0
    TINY = 1
    SHORT = 2
    LONG = 3
    FLOAT = 4
    DOUBLE = 5
    NULL = 6
    TIMESTAMP = 7
    LONGLONG = 8
    INT24 = 9
    DATE = 10
    TIME = 11
    DATETIME = 12
    YEAR = 13
    NEWDATE = 14
    VARCHAR = 15
    BIT = 16
    VECTOR = 242
    JSON = 245
    NEWDECIMAL = 246
    ENUM = 247
    SET = 248
    TINY_BLOB = 249
    MEDIUM_BLOB = 250
    LONG_BLOB = 251
    BLOB = 252
    VAR_STRING = 253
    STRING = 254
    GEOMETRY = 255


class DBAPISet(frozenset[int]):
    """Type object that compares equal to each of its type codes."""

    def __eq__(self, other: object) -> bool:
        if isinstance(other, int):
            return other in self
        return super().__eq__(other)

    def __ne__(self, other: object) -> bool:
        return not self == other

    __hash__ = frozenset.__hash__


STRING = DBAPISet(
    {FieldType.ENUM, FieldType.STRING, FieldType.VAR_STRING, FieldType.VARCHAR}
)
BINARY = DBAPISet(
    {
        FieldType.BLOB,
        FieldType.TINY_BLOB,
        FieldType.MEDIUM_BLOB,
        FieldType.LONG_BLOB,
        FieldType.GEOMETRY,
        FieldType.BIT,
    }
)
NUMBER = DBAPISet(
    {
        FieldType.DECIMAL,
        FieldType.NEWDECIMAL,
        FieldType.TINY,
        FieldType.SHORT,
        FieldType.LONG,
        FieldType.INT24,
        FieldType.LONGLONG,
        FieldType.FLOAT,
        FieldType.DOUBLE,
        FieldType.YEAR,
    }
)
DATETIME = DBAPISet(
    {
        FieldType.DATE,
        FieldType.NEWDATE,
        FieldType.TIME,
        FieldType.DATETIME,
        FieldType.TIMESTAMP,
    }
)
ROWID = DBAPISet()

Date = dt.date
Time = dt.time
Timestamp = dt.datetime
Binary = bytes


# PEP 249 defines the *FromTicks constructors in local time.
def DateFromTicks(ticks: float) -> dt.date:  # noqa: N802
    return dt.date(*time.localtime(ticks)[:3])


def TimeFromTicks(ticks: float) -> dt.time:  # noqa: N802
    return dt.time(*time.localtime(ticks)[3:6])


def TimestampFromTicks(ticks: float) -> dt.datetime:  # noqa: N802
    return dt.datetime(*time.localtime(ticks)[:6])  # noqa: DTZ001
