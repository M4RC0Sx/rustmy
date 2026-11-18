"""The compressed protocol: ``connect(compress=…)``.

``RUSTMY_TEST_COMPRESS=zlib`` (or ``zstd``) runs the whole suite compressed;
these tests check what each server agrees to and the paths whose framing
differs (large payloads, pipelined batches, several results, streaming).
"""

from __future__ import annotations

import datetime as dt

from decimal import Decimal
from typing import TYPE_CHECKING
from typing import Any

import pytest

import rustmy

from rustmy.cursors import PreparedCursor
from rustmy.cursors import SSCursor


if TYPE_CHECKING:
    from tests.conftest import Server


ALGORITHMS = [True, "zlib", "zstd"]


def expected(server: Server, *, compress: bool | str) -> str:
    # MariaDB has no zstd: it falls back to zlib.
    if compress == "zstd" and server.name == "mysql":
        return "zstd"
    return "zlib"


@pytest.mark.parametrize("compress", ALGORITHMS)
@pytest.mark.parametrize("ssl_mode", ["disabled", "required"])
def test_negotiated_algorithm(
    server: Server, *, compress: bool | str, ssl_mode: str
) -> None:
    with server.connect(compress=compress, ssl_mode=ssl_mode) as conn:
        assert conn.compression == expected(server, compress=compress)
        assert conn.tls is (ssl_mode == "required")
        cursor = conn.cursor()
        if server.name == "mysql":
            cursor.execute("SHOW SESSION STATUS LIKE 'Compression_algorithm'")
            assert cursor.fetchone() == ("Compression_algorithm", conn.compression)
        else:
            cursor.execute("SHOW SESSION STATUS LIKE 'Compression'")
            assert cursor.fetchone() == ("Compression", "ON")


def test_off_by_default(server: Server) -> None:
    with server.connect(compress=False) as conn:
        assert conn.compression is None
        cursor = conn.cursor()
        cursor.execute("SHOW SESSION STATUS LIKE 'Compression'")
        assert cursor.fetchone() == ("Compression", "OFF")


@pytest.mark.parametrize("compress", ["zlib", "zstd"])
def test_round_trip(server: Server, compress: str) -> None:
    row = (
        7,
        "ñandú " * 1000,
        b"\x00\xff" * 5000,
        Decimal("12345.6789"),
        dt.datetime(2026, 11, 18, 1, 2, 3, 456789),
    )
    with server.connect(compress=compress, multi_statements=True) as conn:
        cursor = conn.cursor()
        cursor.execute(
            "CREATE TEMPORARY TABLE packed (id INT PRIMARY KEY, t MEDIUMTEXT, "
            "b MEDIUMBLOB, d DECIMAL(12, 4), ts DATETIME(6))"
        )
        cursor.execute("INSERT INTO packed VALUES (%s, %s, %s, %s, %s)", row)
        cursor.execute("SELECT * FROM packed")
        assert cursor.fetchall() == (row,)
        # Over 16 MiB each way: several compressed packets.
        big = "x" * 17_000_000
        cursor.execute("SELECT LENGTH(%s), REPEAT('y', 17000000)", (big,))
        lengths = cursor.fetchone()
        assert lengths is not None
        assert (lengths[0], len(lengths[1])) == (17_000_000, 17_000_000)
        # Each result of a multi-statement query is flushed apart.
        cursor.execute("SELECT 1; SELECT 2; SELECT 3")
        results = [cursor.fetchall()]
        while cursor.nextset():
            results.append(cursor.fetchall())
        assert results == [((1,),), ((2,),), ((3,),)]
        conn.reset_connection()
        conn.ping()


@pytest.mark.parametrize("compress", ["zlib", "zstd"])
@pytest.mark.parametrize("prepared", [False, True])
def test_batches(server: Server, compress: str, *, prepared: bool) -> None:
    """Pipelined statements each travel in compressed packets of their own."""
    with server.connect(compress=compress, autocommit=True) as conn:
        cursor = conn.cursor(prepared=prepared)
        cursor.execute(
            "CREATE TEMPORARY TABLE batch (id INT PRIMARY KEY, v VARCHAR(200))"
        )
        rows = [(i, f"value {i} " * (i % 15)) for i in range(2000)]
        assert cursor.executemany("INSERT INTO batch VALUES (%s, %s)", rows) == 2000
        updated = cursor.executemany(
            "UPDATE batch SET v = %s WHERE id = %s",
            [(f"updated {i}", i) for i in range(0, 2000, 3)],
        )
        assert updated == 667
        with pytest.raises(rustmy.IntegrityError):
            cursor.executemany(
                "UPDATE batch SET id = %s WHERE id = %s", [(1, 2), (5000, 3)]
            )
        cursor.execute("SELECT COUNT(*), SUM(v LIKE 'updated%') FROM batch")
        assert cursor.fetchone() == (2000, Decimal(667))


@pytest.mark.parametrize("compress", ["zlib", "zstd"])
def test_streaming_and_prepared_reads(server: Server, compress: str) -> None:
    with server.connect(compress=compress) as conn:
        query = (
            "WITH RECURSIVE seq (n) AS "
            "(SELECT 0 UNION ALL SELECT n + 1 FROM seq WHERE n < 99) "
            "SELECT n, REPEAT('row', MOD(n, 50)) FROM seq WHERE n >= %s ORDER BY n"
        )
        plain = conn.cursor()
        plain.execute(query, (0,))
        expected_rows = plain.fetchall()
        assert len(expected_rows) == 100
        streamed = conn.cursor(SSCursor)
        streamed.execute(query, (0,))
        assert tuple(streamed) == expected_rows
        prepared = conn.cursor(PreparedCursor)
        prepared.execute(query, (0,))
        assert prepared.fetchall() == expected_rows


def test_server_without_compression(server: Server) -> None:
    """A server that offers none gives an uncompressed connection."""
    if server.name != "mysql":
        pytest.skip("only MySQL can turn the compressed protocol off")
    with server.connect(user="root", database=None, autocommit=True) as root:
        cursor = root.cursor()
        cursor.execute("SELECT @@GLOBAL.protocol_compression_algorithms")
        row = cursor.fetchone()
        assert row is not None
        before = row[0]
        cursor.execute("SET GLOBAL protocol_compression_algorithms = 'uncompressed'")
        try:
            for compress in ALGORITHMS:
                with server.connect(compress=compress) as conn:
                    assert conn.compression is None
            cursor.execute("SET GLOBAL protocol_compression_algorithms = 'zlib'")
            with server.connect(compress="zstd") as conn:
                assert conn.compression == "zlib"
        finally:
            cursor.execute("SET GLOBAL protocol_compression_algorithms = %s", (before,))


@pytest.mark.parametrize(
    ("kwargs", "message"),
    [
        ({"compress": "lz4"}, "invalid compress 'lz4'"),
        ({"compress": "zstd", "zstd_level": 0}, "zstd_level must be between 1 and 22"),
        ({"compress": "zstd", "zstd_level": 23}, "zstd_level must be between 1 and 22"),
    ],
)
def test_invalid_arguments(kwargs: dict[str, Any], message: str) -> None:
    with pytest.raises(ValueError, match=message):
        rustmy.connect(host="127.0.0.1", port=1, **kwargs)
