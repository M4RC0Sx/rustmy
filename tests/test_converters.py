"""Parameter interpolation and escaping (no server needed)."""

import datetime as dt

from decimal import Decimal

import pytest

import rustmy

from rustmy.converters import escape_string
from rustmy.converters import interpolate
from rustmy.converters import literal


@pytest.mark.parametrize(
    ("value", "expected"),
    [
        (None, "NULL"),
        (True, "1"),
        (False, "0"),
        (-42, "-42"),
        (2**64, "18446744073709551616"),
        (1.5, "1.5"),
        (Decimal("1E+2"), "100"),
        (Decimal("-0.000001"), "-0.000001"),
        ("plain", "'plain'"),
        (b"\x00\xff", "X'00ff'"),
        (bytearray(b"ab"), "X'6162'"),
        (b"", "X''"),
        (dt.date(2026, 11, 13), "'2026-11-13'"),
        (dt.datetime(2026, 11, 13, 21, 5, 9, 250), "'2026-11-13 21:05:09.000250'"),
        (dt.time(21, 5), "'21:05:00'"),
        (dt.timedelta(hours=-25, seconds=-1), "'-25:00:01.000000'"),
        (dt.timedelta(days=1, microseconds=5), "'24:00:00.000005'"),
        ((1, "a", None), "(1,'a',NULL)"),
    ],
)
def test_literal(value: object, expected: str) -> None:
    assert literal(value) == expected


def test_backslash_escapes() -> None:
    raw = "a'b\"c\\d\0e\nf\rg\x1ah"
    assert escape_string(raw) == "a\\'b\\\"c\\\\d\\0e\\nf\\rg\\Zh"


def test_no_backslash_escapes_only_doubles_quotes() -> None:
    assert escape_string("a'b\\c", no_backslash_escapes=True) == "a''b\\c"


@pytest.mark.parametrize("value", [float("nan"), float("inf"), Decimal("NaN")])
def test_non_finite_numbers_are_rejected(value: object) -> None:
    with pytest.raises(rustmy.ProgrammingError):
        literal(value)


def test_unsupported_type_is_rejected() -> None:
    with pytest.raises(rustmy.ProgrammingError, match="object"):
        literal(object())


def test_interpolate_positional_mapping_and_scalar() -> None:
    assert (
        interpolate("SELECT %s, %s", (1, "x"), no_backslash_escapes=False)
        == "SELECT 1, 'x'"
    )
    assert (
        interpolate("SELECT %(a)s", {"a": None}, no_backslash_escapes=False)
        == "SELECT NULL"
    )
    assert interpolate("SELECT %s", 5, no_backslash_escapes=False) == "SELECT 5"


@pytest.mark.parametrize(
    ("query", "args"),
    [("SELECT %s, %s", (1,)), ("SELECT %s", (1, 2)), ("SELECT %(a)s", {"b": 1})],
)
def test_interpolate_mismatch_is_programming_error(
    query: str, args: tuple[object, ...] | dict[str, object]
) -> None:
    with pytest.raises(rustmy.ProgrammingError):
        interpolate(query, args, no_backslash_escapes=False)
