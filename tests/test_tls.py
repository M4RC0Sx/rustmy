"""TLS: argument resolution (no server) and behaviour against the servers."""

from __future__ import annotations

import os

from pathlib import Path
from typing import TYPE_CHECKING
from typing import Any

import pytest

import rustmy

from rustmy._tls import TlsSettings
from rustmy._tls import resolve


if TYPE_CHECKING:
    from tests.conftest import Server


CERTS = Path(__file__).parent / "docker" / "certs"


def _resolve(**kwargs: Any) -> TlsSettings:
    args: dict[str, Any] = {
        "ssl_mode": None,
        "ssl_ca": None,
        "ssl_cert": None,
        "ssl_key": None,
        "ssl": None,
    }
    args.update(kwargs)
    return resolve(**args)


@pytest.mark.parametrize(
    ("kwargs", "expected"),
    [
        ({}, TlsSettings("preferred")),
        ({"ssl_mode": "REQUIRED"}, TlsSettings("required")),
        ({"ssl_ca": "ca.pem"}, TlsSettings("verify_identity", ca="ca.pem")),
        ({"ssl_ca": Path("ca.pem")}, TlsSettings("verify_identity", ca="ca.pem")),
        (
            {"ssl_mode": "verify_ca", "ssl_ca": "ca.pem"},
            TlsSettings("verify_ca", ca="ca.pem"),
        ),
        (
            {"ssl_cert": "c.pem", "ssl_key": "k.pem"},
            TlsSettings("required", cert="c.pem", key="k.pem"),
        ),
        ({"ssl": True}, TlsSettings("required")),
        ({"ssl": False}, TlsSettings("disabled")),
        ({"ssl": {}}, TlsSettings("required")),
        ({"ssl": {"ca": "ca.pem"}}, TlsSettings("verify_identity", ca="ca.pem")),
        (
            {"ssl": {"ca": "ca.pem", "check_hostname": False}},
            TlsSettings("verify_ca", ca="ca.pem"),
        ),
        (
            {"ssl": {"ca": "ca.pem"}, "ssl_ca": "ca.pem"},
            TlsSettings("verify_identity", ca="ca.pem"),
        ),
        ({"ssl": True, "ssl_mode": "verify_identity"}, TlsSettings("verify_identity")),
    ],
)
def test_resolution(kwargs: dict[str, Any], expected: TlsSettings) -> None:
    assert _resolve(**kwargs) == expected


@pytest.mark.parametrize(
    ("kwargs", "message"),
    [
        ({"ssl_mode": "verify"}, "invalid ssl_mode"),
        ({"ssl_mode": "disabled", "ssl_ca": "ca.pem"}, "contradicts"),
        ({"ssl_mode": "required", "ssl_ca": "ca.pem"}, "only used to verify"),
        ({"ssl_mode": "preferred", "ssl_ca": "ca.pem"}, "only used to verify"),
        ({"ssl_cert": "c.pem"}, "both ssl_cert and ssl_key"),
        ({"ssl": {"fake_flag_to_force_tls": True}}, "ssl_mode='required'"),
        ({"ssl": {"capath": "/etc/ssl"}}, "unsupported ssl option"),
        ({"ssl": {"ca": 1}}, "must be a path"),
        ({"ssl": {"ca": "a.pem"}, "ssl_ca": "b.pem"}, "disagree"),
    ],
)
def test_contradictions_raise(kwargs: dict[str, Any], message: str) -> None:
    with pytest.raises(rustmy.ProgrammingError, match=message):
        _resolve(**kwargs)


def _cipher(conn: rustmy.Connection) -> str:
    cursor = conn.cursor()
    cursor.execute("SHOW SESSION STATUS LIKE 'Ssl_cipher'")
    (_, cipher) = cursor.fetchone() or ("", "")
    assert isinstance(cipher, str)
    return cipher


@pytest.mark.parametrize(
    ("kwargs", "encrypted"),
    [
        ({}, True),  # preferred, and the test servers offer TLS
        ({"ssl_mode": "disabled"}, False),
        ({"ssl": True}, True),
        ({"ssl_ca": CERTS / "ca.pem"}, True),
        ({"ssl_ca": CERTS / "ca.pem", "host": "localhost"}, True),
        ({"ssl": {"ca": os.fspath(CERTS / "ca.pem"), "check_hostname": False}}, True),
    ],
)
def test_connection_encryption(
    server: Server, kwargs: dict[str, Any], *, encrypted: bool
) -> None:
    with server.connect(**kwargs) as conn:
        assert conn.tls is encrypted
        assert bool(_cipher(conn)) is encrypted


@pytest.mark.parametrize(
    "kwargs",
    [
        {"ssl_ca": CERTS / "other-ca.pem"},
        {"ssl_mode": "verify_identity"},  # test CA is not in the OS store
        {"ssl_ca": CERTS / "missing.pem"},
    ],
    ids=["wrong-ca", "os-store", "missing-file"],
)
def test_verification_failures_are_tls_errors(
    server: Server, kwargs: dict[str, Any]
) -> None:
    with pytest.raises(rustmy.OperationalError) as info:
        server.connect(**kwargs)
    assert info.value.args[0] == 2026


def test_client_certificate(server: Server) -> None:
    with server.connect(
        user="rustmy_x509",
        ssl_ca=CERTS / "ca.pem",
        ssl_cert=CERTS / "client.pem",
        ssl_key=CERTS / "client-key.pem",
    ) as conn:
        assert _cipher(conn)
    with pytest.raises(rustmy.OperationalError) as info:
        server.connect(user="rustmy_x509", ssl=True)
    assert info.value.args[0] == 1045


def _sessions_reused(conn: rustmy.Connection) -> str:
    cursor = conn.cursor()
    cursor.execute("SHOW SESSION STATUS LIKE 'Ssl_sessions_reused'")
    (_, reused) = cursor.fetchone() or ("", "")
    assert isinstance(reused, str)
    return reused


@pytest.mark.parametrize(
    "kwargs",
    [{}, {"ssl_ca": CERTS / "ca.pem"}],
    ids=["preferred", "verify_identity"],
)
def test_connections_with_the_same_options_resume_the_session(
    server: Server, kwargs: dict[str, Any]
) -> None:
    # The TLS configuration is shared by every connection with the same
    # options, and with it the session tickets the server issued. MySQL
    # issues them; MariaDB does not, and then every handshake is complete.
    server.connect(**kwargs).close()
    with server.connect(**kwargs) as conn:
        assert conn.tls
        expected = "0" if server.name == "mariadb" else "1"
        assert _sessions_reused(conn) == expected
