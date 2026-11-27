"""Connection attributes (``performance_schema.session_connect_attrs``):
the driver's own, ``program_name`` and ``conn_attrs``, in the handshake and
after ``change_user``, blocking and asyncio."""

from __future__ import annotations

import os

from typing import TYPE_CHECKING
from typing import Any

import pytest

import rustmy

from rustmy import aio


if TYPE_CHECKING:
    from tests.conftest import Server


QUERY = (
    "SELECT ATTR_NAME, ATTR_VALUE"
    " FROM performance_schema.session_account_connect_attrs"
    " WHERE PROCESSLIST_ID = CONNECTION_ID()"
)


def _attrs(conn: rustmy.Connection) -> dict[str, str] | None:
    """The session's attributes, or ``None`` without the performance schema
    (MariaDB's default)."""
    cursor = conn.cursor()
    cursor.execute("SELECT @@performance_schema")
    if cursor.fetchone() != (1,):
        return None
    cursor.execute(QUERY)
    return dict(cursor.fetchall())


@pytest.fixture
def attrs_of(server: Server) -> Any:
    def connect_and_read(**kwargs: Any) -> dict[str, str] | None:
        with server.connect(**kwargs) as conn:
            return _attrs(conn)

    return connect_and_read


def test_the_driver_names_itself(attrs_of: Any) -> None:
    attrs = attrs_of()
    if attrs is None:
        pytest.skip("the server has no performance schema")
    assert attrs["_client_name"] == "rustmy"
    assert attrs["_client_version"]
    assert attrs["_pid"] == str(os.getpid())
    assert attrs["_os"]
    assert attrs["_platform"] in {"x86_64", "aarch64", "x86", "arm"}
    assert "program_name" not in attrs


def test_program_name_and_conn_attrs(attrs_of: Any) -> None:
    attrs = attrs_of(
        program_name="billing-worker",
        conn_attrs={"team": "payments", "région": "eu-west-1 ñ"},
    )
    if attrs is None:
        pytest.skip("the server has no performance schema")
    assert attrs["program_name"] == "billing-worker"
    assert attrs["team"] == "payments"
    # In the connection's character set, as the server reads them.
    assert attrs["région"] == "eu-west-1 ñ"
    assert attrs["_client_name"] == "rustmy"


def test_in_another_character_set(attrs_of: Any) -> None:
    attrs = attrs_of(charset="latin1", conn_attrs={"café": "crème"})
    if attrs is None:
        pytest.skip("the server has no performance schema")
    assert attrs["café"] == "crème"


def test_change_user_sends_them_again(server: Server) -> None:
    with server.connect(program_name="before") as conn:
        conn.change_user("rustmy", "rustmy", "rustmy_test")
        attrs = _attrs(conn)
    if attrs is None:
        pytest.skip("the server has no performance schema")
    assert attrs["program_name"] == "before"
    assert attrs["_client_name"] == "rustmy"


def test_with_compression(attrs_of: Any) -> None:
    # The zstd level follows the attributes in the handshake response.
    for compress in ("zlib", "zstd"):
        attrs = attrs_of(compress=compress, program_name=compress)
        if attrs is not None:
            assert attrs["program_name"] == compress


async def test_aio(server: Server) -> None:
    args = server.connect_args(program_name="aio-worker", conn_attrs={"k": "v"})
    async with aio.connect(**args) as conn:
        cursor = await conn.cursor()
        await cursor.execute("SELECT @@performance_schema")
        if await cursor.fetchone() != (1,):
            pytest.skip("the server has no performance schema")
        await cursor.execute(QUERY)
        attrs: dict[str, str] = dict(await cursor.fetchall())
    assert attrs["program_name"] == "aio-worker"
    assert attrs["k"] == "v"
    assert attrs["_client_name"] == "rustmy"


@pytest.mark.parametrize(
    ("kwargs", "error"),
    [
        ({"conn_attrs": {"_client_name": "x"}}, ValueError),
        ({"conn_attrs": {"": "x"}}, ValueError),
        ({"conn_attrs": {"k": 1}}, TypeError),
        ({"conn_attrs": {1: "v"}}, TypeError),
        ({"conn_attrs": ["k", "v"]}, TypeError),
        ({"program_name": 1}, TypeError),
        ({"program_name": "a", "conn_attrs": {"program_name": "b"}}, ValueError),
    ],
)
def test_invalid_attributes(server: Server, kwargs: Any, error: type) -> None:
    with pytest.raises(error):
        server.connect(**kwargs)


def test_too_large(server: Server) -> None:
    with pytest.raises(rustmy.InterfaceError, match="65535"):
        server.connect(conn_attrs={f"k{i}": "x" * 1000 for i in range(70)})
