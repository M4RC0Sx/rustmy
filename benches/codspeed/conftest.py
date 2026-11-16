"""Fixtures for the CodSpeed benchmarks: a server and the tables they read.

The server comes from ``RUSTMY_BENCH_HOST`` / ``RUSTMY_BENCH_PORT`` (default
the compose MySQL, 127.0.0.1:13306); the tables are created once per session.
"""

from __future__ import annotations

import os

from typing import TYPE_CHECKING

import pytest

import rustmy


if TYPE_CHECKING:
    from collections.abc import Iterator


ROWS = 10_000

_DIGITS = " UNION ALL ".join(f"SELECT {d} AS d" for d in range(10))
_NUMBERS = "SELECT t0.d + t1.d * 10 + t2.d * 100 + t3.d * 1000 AS n FROM " + ", ".join(
    f"({_DIGITS}) AS t{i}" for i in range(4)
)

SETUP = [
    "DROP TABLE IF EXISTS codspeed_intvarchar, codspeed_mixed, codspeed_insert",
    """CREATE TABLE codspeed_intvarchar (
        id INT PRIMARY KEY, a INT NOT NULL, b BIGINT NOT NULL,
        c VARCHAR(32) NOT NULL, d VARCHAR(64) NULL)""",
    f"""INSERT INTO codspeed_intvarchar
        SELECT n, (n * 7) % 10000, n * 1000003, CONCAT('name-', n),
               IF(n % 10 = 0, NULL, REPEAT(CHAR(97 + n % 26), 20 + n % 40))
        FROM ({_NUMBERS}) AS numbers""",
    """CREATE TABLE codspeed_mixed (
        id INT PRIMARY KEY, f DOUBLE NOT NULL, ts DATETIME(6) NOT NULL,
        d DATE NOT NULL, amount DECIMAL(12, 2) NOT NULL,
        s VARCHAR(64) CHARACTER SET utf8mb4 NOT NULL)""",
    f"""INSERT INTO codspeed_mixed
        SELECT n, n / 7, TIMESTAMPADD(MICROSECOND, n * 1234567, '2020-01-01'),
               DATE_ADD('2000-01-01', INTERVAL n % 9000 DAY), n / 100,
               CONCAT('Señor café ', n)
        FROM ({_NUMBERS}) AS numbers""",
    "CREATE TABLE codspeed_insert (id INT, s VARCHAR(64), f DOUBLE)",
]


def connect(**overrides: object) -> rustmy.Connection:
    kwargs: dict[str, object] = {
        "host": os.environ.get("RUSTMY_BENCH_HOST", "127.0.0.1"),
        "port": int(os.environ.get("RUSTMY_BENCH_PORT", "13306")),
        "user": "rustmy",
        "password": "rustmy",
        "database": "rustmy_test",
        "autocommit": True,
        # Plain TCP: TLS would add work that is not rustmy's.
        "ssl_mode": "disabled",
    }
    kwargs.update(overrides)
    return rustmy.connect(**kwargs)  # type: ignore[arg-type]


@pytest.fixture(scope="session")
def tables() -> None:
    with connect() as conn:
        cursor = conn.cursor()
        for statement in SETUP:
            cursor.execute(statement)


@pytest.fixture
def conn(tables: None) -> Iterator[rustmy.Connection]:  # noqa: ARG001
    connection = connect()
    yield connection
    connection.close()
