"""Python values to SQL literals for client-side parameter interpolation.

Parameters are interpolated into the query text (``paramstyle = "format"``),
which is what SQLAlchemy's MySQL dialects expect. String escaping follows
the server's ``NO_BACKSLASH_ESCAPES`` mode, reported in its status flags.
"""

from __future__ import annotations

import datetime as dt
import decimal
import math

from collections.abc import Callable
from collections.abc import Mapping
from collections.abc import Sequence
from typing import Any
from typing import TypeAlias

from rustmy.err import ProgrammingError


Params: TypeAlias = Sequence[object] | Mapping[str, object] | object | None
"""Arguments accepted by ``execute``: a sequence for ``%s``, a mapping for
``%(name)s`` or a single value."""

_BACKSLASH_ESCAPES = str.maketrans(
    {
        "\0": "\\0",
        "\n": "\\n",
        "\r": "\\r",
        "\x1a": "\\Z",
        "\\": "\\\\",
        "'": "\\'",
        '"': '\\"',
    }
)


def escape_string(value: str, *, no_backslash_escapes: bool = False) -> str:
    """Escape ``value`` for use inside a single-quoted SQL string."""
    if no_backslash_escapes:
        return value.replace("'", "''")
    return value.translate(_BACKSLASH_ESCAPES)


Escape: TypeAlias = Callable[[str], str]


def _quoted(value: str, escape: Escape) -> str:
    return f"'{escape(value)}'"


def _encode_bool(value: bool, _escape: Escape) -> str:  # noqa: FBT001
    return "1" if value else "0"


def _encode_int(value: int, _escape: Escape) -> str:
    # int() turns IntEnum and other subclasses into a plain number.
    return str(int(value))


def _encode_float(value: float, _escape: Escape) -> str:
    if not math.isfinite(value):
        raise ProgrammingError(0, f"{value!r} cannot be stored in MySQL")
    return repr(value)


def _encode_decimal(value: decimal.Decimal, _escape: Escape) -> str:
    if not value.is_finite():
        raise ProgrammingError(0, f"{value!r} cannot be stored in MySQL")
    # Fixed-point notation keeps the value exact (``1E+2`` would be a float).
    return format(value, "f")


def _encode_str(value: str, escape: Escape) -> str:
    return _quoted(value, escape)


def _encode_bytes(value: bytes | bytearray | memoryview, _escape: Escape) -> str:
    # Hex literals are binary-safe and need no escaping at all.
    return f"X'{bytes(value).hex()}'"


def _encode_datetime(value: dt.datetime, escape: Escape) -> str:
    return _quoted(value.isoformat(" "), escape)


def _encode_iso(value: dt.date | dt.time, escape: Escape) -> str:
    return _quoted(value.isoformat(), escape)


def _encode_timedelta(value: dt.timedelta, escape: Escape) -> str:
    total = (value.days * 86_400 + value.seconds) * 1_000_000 + value.microseconds
    sign = "-" if total < 0 else ""
    seconds, micros = divmod(abs(total), 1_000_000)
    minutes, seconds = divmod(seconds, 60)
    hours, minutes = divmod(minutes, 60)
    return _quoted(
        f"{sign}{hours:02d}:{minutes:02d}:{seconds:02d}.{micros:06d}", escape
    )


def _encode_sequence(value: Sequence[object], escape: Escape) -> str:
    return "(" + ",".join(_literal(item, escape) for item in value) + ")"


# Checked in order: bool before int, datetime before date (subclasses first).
_ENCODERS: tuple[tuple[type | tuple[type, ...], Callable[[Any, Escape], str]], ...] = (
    (bool, _encode_bool),
    (int, _encode_int),
    (float, _encode_float),
    (decimal.Decimal, _encode_decimal),
    (str, _encode_str),
    ((bytes, bytearray, memoryview), _encode_bytes),
    (dt.datetime, _encode_datetime),
    ((dt.date, dt.time), _encode_iso),
    (dt.timedelta, _encode_timedelta),
    ((list, tuple, set, frozenset), _encode_sequence),
)


def _literal(value: object, escape: Escape) -> str:
    if value is None:
        return "NULL"
    for types, encode in _ENCODERS:
        if isinstance(value, types):
            return encode(value, escape)
    msg = f"unsupported parameter type: {type(value).__name__}"
    raise ProgrammingError(0, msg)


def literal(value: object, *, no_backslash_escapes: bool = False) -> str:
    """Render ``value`` as an SQL literal."""
    return _literal(value, _escaper(no_backslash_escapes=no_backslash_escapes))


def _escaper(*, no_backslash_escapes: bool) -> Escape:
    def escape(value: str) -> str:
        return escape_string(value, no_backslash_escapes=no_backslash_escapes)

    return escape


def interpolate(query: str, args: Params, *, no_backslash_escapes: bool) -> str:
    """Substitute ``%s`` / ``%(name)s`` placeholders with SQL literals."""
    escape = _escaper(no_backslash_escapes=no_backslash_escapes)
    values: tuple[str, ...] | dict[str, str]
    if isinstance(args, Mapping):
        values = {key: _literal(value, escape) for key, value in args.items()}
    elif isinstance(args, (list, tuple)):
        values = tuple(_literal(value, escape) for value in args)
    else:
        values = (_literal(args, escape),)
    try:
        return query % values
    except (TypeError, ValueError, KeyError) as exc:
        msg = f"cannot interpolate parameters into query: {exc}"
        raise ProgrammingError(0, msg) from exc
