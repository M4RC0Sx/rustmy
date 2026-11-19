"""PEP 249 module surface (no server needed)."""

from typing import Any

import pytest

import rustmy
import rustmy.err


@pytest.mark.parametrize(
    ("exc", "parent"),
    [
        (rustmy.Warning, Exception),
        (rustmy.Error, Exception),
        (rustmy.InterfaceError, rustmy.Error),
        (rustmy.DatabaseError, rustmy.Error),
        (rustmy.DataError, rustmy.DatabaseError),
        (rustmy.OperationalError, rustmy.DatabaseError),
        (rustmy.IntegrityError, rustmy.DatabaseError),
        (rustmy.InternalError, rustmy.DatabaseError),
        (rustmy.ProgrammingError, rustmy.DatabaseError),
        (rustmy.NotSupportedError, rustmy.DatabaseError),
        (rustmy.err.OutOfRangeError, rustmy.DataError),
        (rustmy.err.OutOfRangeError, rustmy.OperationalError),
    ],
)
def test_exception_hierarchy(exc: type[Exception], parent: type[Exception]) -> None:
    assert issubclass(exc, parent)


def test_exceptions_are_reachable_from_connection_class() -> None:
    assert rustmy.Connection.IntegrityError is rustmy.IntegrityError


def test_type_objects_compare_equal_to_type_codes() -> None:
    assert rustmy.FieldType.VAR_STRING == rustmy.STRING
    assert rustmy.FieldType.LONGLONG == rustmy.NUMBER
    assert rustmy.FieldType.DATETIME == rustmy.DATETIME
    assert rustmy.FieldType.BLOB == rustmy.BINARY
    assert rustmy.FieldType.BLOB != rustmy.STRING


@pytest.mark.parametrize(
    ("charset", "reason"),
    [
        ("binary", "needs a text character set"),
        ("eucjpms", "no exact codec"),
        ("utf16", "does not accept it as a connection character set"),
        ("armscii8", "no codec"),
        ("klingon", "not a MySQL character set"),
    ],
)
def test_connect_with_unsupported_charset_fails_before_connecting(
    charset: str, reason: str
) -> None:
    # Port 1 refuses connections: the error comes before connecting.
    with pytest.raises(rustmy.NotSupportedError, match=reason):
        rustmy.connect(port=1, charset=charset)


def test_connection_refused_is_operational_error_2003() -> None:
    with pytest.raises(rustmy.OperationalError) as info:
        rustmy.connect(host="127.0.0.1", port=1, connect_timeout=2)
    assert info.value.args[0] == 2003


@pytest.mark.parametrize("timeout", [-1.0, float("nan")])
def test_invalid_timeout_is_rejected(timeout: float) -> None:
    with pytest.raises(ValueError, match="positive"):
        rustmy.connect(host="127.0.0.1", port=1, connect_timeout=timeout)


@pytest.mark.parametrize("name", ["read_timeout", "write_timeout"])
def test_zero_read_and_write_timeouts_are_rejected(name: str) -> None:
    timeout: dict[str, Any] = {name: 0}
    with pytest.raises(ValueError, match="positive"):
        rustmy.connect(host="127.0.0.1", port=1, **timeout)


def test_a_zero_connect_timeout_waits_forever() -> None:
    # As in MySQL's clients: 0 turns the limit off, it is not an error.
    with pytest.raises(rustmy.OperationalError) as info:
        rustmy.connect(host="127.0.0.1", port=1, connect_timeout=0)
    assert info.value.args[0] == 2003
