"""Python values to SQL literals for client-side parameter interpolation.

Parameters are interpolated into the query text (``paramstyle = "format"``),
which is what SQLAlchemy's MySQL dialects expect. The work happens in the
native module; these are thin wrappers for direct use and tests.
"""

from __future__ import annotations

from collections.abc import Mapping
from collections.abc import Sequence
from typing import TypeAlias

from rustmy import _core


Params: TypeAlias = Sequence[object] | Mapping[str, object] | object | None
"""Arguments accepted by ``execute``: a sequence for ``%s``, a mapping for
``%(name)s`` or a single value."""


def escape_string(value: str, *, no_backslash_escapes: bool = False) -> str:
    """Escape ``value`` for use inside a single-quoted SQL string."""
    return _core.escape_string(value, no_backslash_escapes)


def literal(value: object, *, no_backslash_escapes: bool = False) -> str:
    """Render ``value`` as an SQL literal (UTF-8 connection)."""
    return _core.literal(value, no_backslash_escapes)


def interpolate(query: str, args: Params, *, no_backslash_escapes: bool) -> str:
    """Substitute ``%s`` / ``%(name)s`` placeholders with SQL literals."""
    return _core.interpolate(query, args, no_backslash_escapes)
