"""Conversions between Python values and SQL.

Parameters are interpolated into the query text (``paramstyle = "format"``),
which is what SQLAlchemy's MySQL dialects expect. The work happens in the
native module; ``escape_string``, ``literal`` and ``interpolate`` are thin
wrappers for direct use and tests.

``conversions`` is the default ``conv=`` mapping, in MySQLdb's format: column
types (``FIELD_TYPE``) to decoders and Python types to encoders. Passing it,
or a copy with a few entries changed, keeps every other column on the native
fast path: only entries that differ from it are called from Python.
"""

from __future__ import annotations

import datetime as dt
import uuid

from collections.abc import Callable
from collections.abc import Mapping
from collections.abc import Sequence
from decimal import Decimal
from typing import Any
from typing import TypeAlias

from rustmy import _core
from rustmy.constants import FIELD_TYPE


Params: TypeAlias = Sequence[object] | Mapping[str, object] | object | None
"""Arguments accepted by ``execute``: a sequence for ``%s``, a mapping for
``%(name)s`` or a single value."""

Decoder: TypeAlias = Callable[[Any], object]
#: ``(mask, decoder)`` pairs: the first whose ``FLAG`` mask matches the
#: column's flags (or whose mask is not an int) is used.
FlagDecoders: TypeAlias = Sequence[tuple[Any, Decoder | None]]
Encoder: TypeAlias = Callable[[Any, Any], str | bytes]


def escape_string(value: str, *, no_backslash_escapes: bool = False) -> str:
    """Escape ``value`` for use inside a single-quoted SQL string."""
    return _core.escape_string(value, no_backslash_escapes)


def literal(value: object, *, no_backslash_escapes: bool = False) -> str:
    """Render ``value`` as an SQL literal (UTF-8 connection)."""
    return _core.literal(value, no_backslash_escapes)


def interpolate(query: str, args: Params, *, no_backslash_escapes: bool) -> str:
    """Substitute ``%s`` / ``%(name)s`` placeholders with SQL literals."""
    return _core.interpolate(query, args, no_backslash_escapes)


# Default decoders. The driver converts these types natively; the functions
# exist so that ``conversions`` is complete and usable on its own.


def to_datetime(value: str) -> dt.datetime | None:
    """``DATETIME``/``TIMESTAMP`` text to ``datetime`` (``None`` if invalid,
    e.g. a zero date).
    """
    # Python 3.10's fromisoformat needs 3 or 6 fractional digits.
    head, dot, fraction = value.partition(".")
    if dot:
        value = f"{head}.{fraction.ljust(6, '0')[:6]}"
    try:
        return dt.datetime.fromisoformat(value)
    except ValueError:
        return None


def to_date(value: str) -> dt.date | None:
    """``DATE`` text to ``date`` (``None`` if invalid, e.g. a zero date)."""
    try:
        return dt.date.fromisoformat(value)
    except ValueError:
        return None


def to_timedelta(value: str) -> dt.timedelta | None:
    """``TIME`` text (``[-]H:MM:SS[.ffffff]``, hours may exceed 24) to
    ``timedelta``.
    """
    negative = value.startswith("-")
    try:
        clock, _, fraction = value.lstrip("-").partition(".")
        hours, minutes, seconds = (int(part) for part in clock.split(":"))
        micros = int(fraction.ljust(6, "0")[:6]) if fraction else 0
    except ValueError:
        return None
    delta = dt.timedelta(
        hours=hours, minutes=minutes, seconds=seconds, microseconds=micros
    )
    return -delta if negative else delta


def to_literal(value: object, conv: object = None) -> str:
    """Default encoder: ``value`` as an SQL literal (UTF-8 connection)."""
    del conv
    return _core.literal(value)


#: Types whose values arrive as text or bytes: their default is to decode
#: text (``str``) unless the column's collation is binary (``bytes``).
_TEXT_TYPES = frozenset(
    {
        FIELD_TYPE.VARCHAR,
        FIELD_TYPE.VAR_STRING,
        FIELD_TYPE.STRING,
        FIELD_TYPE.TINY_BLOB,
        FIELD_TYPE.MEDIUM_BLOB,
        FIELD_TYPE.LONG_BLOB,
        FIELD_TYPE.BLOB,
        FIELD_TYPE.JSON,
        FIELD_TYPE.ENUM,
        FIELD_TYPE.SET,
        FIELD_TYPE.BIT,
        FIELD_TYPE.GEOMETRY,
        FIELD_TYPE.VECTOR,
    }
)

decoders: dict[int, Decoder] = {
    FIELD_TYPE.TINY: int,
    FIELD_TYPE.SHORT: int,
    FIELD_TYPE.LONG: int,
    FIELD_TYPE.LONGLONG: int,
    FIELD_TYPE.INT24: int,
    FIELD_TYPE.YEAR: int,
    FIELD_TYPE.FLOAT: float,
    FIELD_TYPE.DOUBLE: float,
    FIELD_TYPE.DECIMAL: Decimal,
    FIELD_TYPE.NEWDECIMAL: Decimal,
    FIELD_TYPE.TIMESTAMP: to_datetime,
    FIELD_TYPE.DATETIME: to_datetime,
    FIELD_TYPE.DATE: to_date,
    FIELD_TYPE.TIME: to_timedelta,
    **dict.fromkeys(_TEXT_TYPES, bytes),
}

encoders: dict[type, Encoder] = dict.fromkeys(
    (
        type(None),
        bool,
        int,
        float,
        str,
        bytes,
        bytearray,
        memoryview,
        Decimal,
        uuid.UUID,
        dt.date,
        dt.datetime,
        dt.time,
        dt.timedelta,
        tuple,
        list,
        set,
        frozenset,
    ),
    to_literal,
)

#: Default ``conv=`` mapping (MySQLdb format).
conversions: dict[Any, Any] = {**encoders, **decoders}


def _is_default(field_type: int, decoder: object) -> bool:
    if decoder is None or decoder is decoders.get(field_type):
        return True
    # MySQLdb's mapping says bytes for text types and decodes text anyway;
    # str or bytes there means the default handling.
    return field_type in _TEXT_TYPES and decoder in (bytes, str)


def _custom_decoder(field_type: int, decoder: Any) -> Any:
    """``decoder`` with default entries replaced by ``None``, or ``None`` if
    it is the default altogether.
    """
    if callable(decoder):
        return None if _is_default(field_type, decoder) else decoder
    pairs = [
        (mask, None if _is_default(field_type, func) else func)
        for mask, func in decoder
    ]
    return None if all(func is None for _, func in pairs) else pairs


def split_conv(
    conv: Mapping[Any, Any],
) -> tuple[dict[int, Any] | None, dict[type, Encoder] | None]:
    """The decoders and encoders of ``conv`` that differ from the defaults,
    ``None`` when there are none.
    """
    custom_decoders: dict[int, Any] = {}
    custom_encoders: dict[type, Encoder] = {}
    for key, value in conv.items():
        if isinstance(key, int) and not isinstance(key, bool):
            custom = _custom_decoder(key, value)
            if custom is not None:
                custom_decoders[key] = custom
        elif isinstance(key, type) and value is not to_literal:
            custom_encoders[key] = value
    return custom_decoders or None, custom_encoders or None
