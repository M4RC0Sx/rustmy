"""Connection character sets and collations against real servers."""

from __future__ import annotations

import uuid

from typing import TYPE_CHECKING
from typing import Any

import pytest

import rustmy

from rustmy.cursors import DictCursor
from rustmy.cursors import SSCursor


if TYPE_CHECKING:
    from collections.abc import Iterator

    from tests.conftest import Server


#: Character set, Python codec reported as ``encoding``, sample text.
SAMPLES = [
    ("latin1", "cp1252", "Señor, ¿un café? Ça coûte 5 €"),
    ("latin2", "iso8859_2", "Zażółć gęślą jaźń"),
    ("cp1250", "cp1250", "Příliš žluťoučký kůň"),
    ("cp1251", "cp1251", "Съешь же ещё этих мягких булок"),
    ("koi8r", "koi8_r", "Привет, мир"),
    ("greek", "iso8859_7", "Καλημέρα κόσμε"),
    ("hebrew", "iso8859_8", "שלום עולם"),
    ("tis620", "tis_620", "สวัสดีชาวโลก"),
    ("gbk", "gbk", "你好，世界"),  # noqa: RUF001  # full-width comma on purpose
    ("gb18030", "gb18030", "你好，世界"),  # noqa: RUF001
    ("big5", "big5", "運動會與許功蓋"),
    ("cp932", "cp932", "日本語のテキスト①"),
    ("sjis", "shift_jis", "日本語のテキスト"),
    ("ujis", "euc_jp", "日本語のテキスト"),
    ("euckr", "euc_kr", "안녕하세요 세계"),
]

#: Multi-byte character sets where a character's last byte can be 0x5C
#: (backslash), the shape behind CVE-2006-2753.
BACKSLASH_TRAIL = ["gbk", "big5", "cp932", "sjis"]


def _supports(conn: rustmy.Connection, charset: str) -> bool:
    cursor = conn.cursor()
    cursor.execute("SHOW CHARACTER SET LIKE %s", (charset,))
    return bool(cursor.fetchall())


@pytest.fixture
def table(conn: rustmy.Connection) -> Iterator[str]:
    name = f"cs_{uuid.uuid4().hex[:8]}"
    cursor = conn.cursor()
    cursor.execute(
        f"CREATE TABLE {name} (id INT PRIMARY KEY, s VARCHAR(100)) "
        "CHARACTER SET utf8mb4"
    )
    yield name
    cursor.execute(f"DROP TABLE {name}")


# Default collation ---------------------------------------------------------


def test_session_uses_the_servers_default_collation(conn: rustmy.Connection) -> None:
    # Comparing a column with a user variable fails (1267, "Illegal mix of
    # collations") when the session's collation differs from the table's.
    cursor = conn.cursor()
    cursor.execute("CREATE TEMPORARY TABLE t (s VARCHAR(10))")
    cursor.execute(
        "SELECT TABLE_COLLATION FROM information_schema.TABLES "
        "WHERE TABLE_SCHEMA = DATABASE() AND TABLE_NAME = 't'"
    )
    cursor.execute("SELECT @@collation_connection, @@collation_database")
    session, database = cursor.fetchone() or (None, None)
    assert session == database
    cursor.execute("SET @v = 'a'")
    cursor.execute("INSERT INTO t VALUES ('a')")
    assert cursor.execute("SELECT * FROM t WHERE s = @v") == 1
    assert conn.character_set_name() == "utf8mb4"
    assert conn.encoding == "utf8"


def test_utf8_means_utf8mb4(server: Server) -> None:
    with server.connect(charset="utf8") as conn:
        assert conn.character_set_name() == "utf8mb4"
        cursor = conn.cursor()
        cursor.execute("SELECT %s", ("emoji 😀",))
        assert cursor.fetchone() == ("emoji 😀",)


def test_utf8mb3_rejects_4_byte_characters(server: Server) -> None:
    with server.connect(charset="utf8mb3") as conn:
        cursor = conn.cursor()
        cursor.execute("SELECT %s", ("BMP: ñ € 日本",))
        assert cursor.fetchone() == ("BMP: ñ € 日本",)
        with pytest.raises(rustmy.DataError, match="U\\+1F600"):
            cursor.execute("SELECT %s", ("emoji 😀",))
        assert cursor.execute("SELECT 1") == 1


# Round trips -----------------------------------------------------------------


def _ids(sample: tuple[str, str, str]) -> str:
    return sample[0]


@pytest.mark.parametrize("sample", SAMPLES, ids=_ids)
def test_text_and_bytes_round_trip(
    server: Server, conn: rustmy.Connection, table: str, sample: tuple[str, str, str]
) -> None:
    charset, encoding, text = sample
    if not _supports(conn, charset):
        pytest.skip(f"{server.name} has no {charset}")
    blob = bytes(range(256))
    with server.connect(charset=charset) as other:
        assert other.character_set_name() == charset
        assert other.encoding == encoding
        cursor = other.cursor()
        cursor.execute("SELECT @@character_set_client, @@character_set_results")
        assert cursor.fetchone() == (charset, charset)
        # Parameters, the query text itself and binary data.
        cursor.execute(f"SELECT %s, '{text}', %s", (text, blob))
        assert cursor.fetchone() == (text, text, blob)
        # Through a utf8mb4 column: the server converts in both directions.
        cursor.execute(f"INSERT INTO {table} VALUES (%s, %s)", (1, text))
        cursor.execute(f"SELECT s FROM {table} WHERE s = %s", (text,))
        assert cursor.fetchall() == [(text,)]
        other.commit()
    # Stored correctly, as seen from a utf8mb4 session (in a new transaction:
    # REPEATABLE READ would hide the other session's commit).
    conn.rollback()
    cursor = conn.cursor()
    cursor.execute(f"SELECT s FROM {table}")
    assert cursor.fetchall() == [(text,)]


@pytest.mark.parametrize("sample", SAMPLES, ids=_ids)
def test_streaming_dict_and_executemany(
    server: Server, conn: rustmy.Connection, table: str, sample: tuple[str, str, str]
) -> None:
    charset, _, text = sample
    if not _supports(conn, charset):
        pytest.skip(f"{server.name} has no {charset}")
    with server.connect(charset=charset) as other:
        cursor = other.cursor()
        rows = [(i, f"{text} {i}") for i in range(50)]
        cursor.max_stmt_length = 512  # several batches
        assert cursor.executemany(f"INSERT INTO {table} VALUES (%s, %s)", rows) == 50
        assert text in (cursor._executed or "")  # noqa: SLF001
        streamed = other.cursor(SSCursor)
        streamed.execute(f"SELECT id, s FROM {table} ORDER BY id")
        assert list(streamed) == rows
        dicts = other.cursor(DictCursor)
        dicts.execute(f"SELECT s AS `{text[:3]}` FROM {table} WHERE id = 7")
        assert dicts.fetchall() == [{text[:3]: f"{text} 7"}]


# Encoding safety -------------------------------------------------------------


@pytest.mark.parametrize("charset", BACKSLASH_TRAIL)
def test_multibyte_backslash_trail_cannot_break_out(
    server: Server, charset: str
) -> None:
    codec = "shift_jis" if charset == "sjis" else charset
    candidates = "運許功蓋縗表能十予ソ"
    trail = [c for c in candidates if c.encode(codec, "ignore")[-1:] == b"\\"]
    assert trail, f"no {charset} character ending in a backslash byte"
    with server.connect(charset=charset) as other:
        cursor = other.cursor()
        for c in trail:
            payload = f"{c}' OR 1=1 -- {c}\\{c}"
            cursor.execute("SELECT %s, %s = %s", (payload, payload, payload))
            assert cursor.fetchone() == (payload, 1)
        # Raw bytes shaped like the classic attack (lead byte + quote) go
        # as X'…' hex in multi-byte character sets.
        attack = b"\xbf' OR 1=1 -- \xbf\\'"
        assert "X'" in other.cursor().mogrify("SELECT %s", (attack,))
        cursor.execute("SELECT %s", (attack,))
        assert cursor.fetchone() == (attack,)


def test_unencodable_character_raises_before_sending(server: Server) -> None:
    with server.connect(charset="latin1") as conn:
        cursor = conn.cursor()
        with pytest.raises(rustmy.DataError, match="cannot be encoded"):
            cursor.execute("SELECT %s", ("Привет",))
        with pytest.raises(rustmy.DataError, match="cannot be encoded"):
            cursor.execute("SELECT 'Привет'")
        assert cursor.execute("SELECT 1") == 1


def test_latin1_follows_mysqls_definition(server: Server) -> None:
    # cp1252 leaves five bytes undefined; MySQL's latin1 maps them to the
    # C1 controls of the same value, in both directions.
    c1 = "\x81\x8d\x8f\x90\x9d"
    with server.connect(charset="latin1") as conn:
        cursor = conn.cursor()
        cursor.execute("SELECT CONVERT(0x818D8F909D USING latin1), %s", (c1,))
        assert cursor.fetchone() == (c1, c1)
        cursor.execute("SELECT HEX(CONVERT(%s USING utf8mb4))", (c1,))
        assert cursor.fetchone() == ("C281C28DC28FC290C29D",)


# Metadata, messages, JSON ----------------------------------------------------


def test_names_messages_and_json_use_the_session_charset(server: Server) -> None:
    with server.connect(charset="latin1") as conn:
        cursor = conn.cursor()
        cursor.execute("SELECT 'x' AS `café`")
        assert cursor.description is not None
        assert cursor.description[0][0] == "café"
        with pytest.raises(rustmy.ProgrammingError, match="tabla_niño"):
            cursor.execute("SELECT * FROM `tabla_niño`")
        cursor.execute("CREATE TEMPORARY TABLE j (doc JSON)")
        cursor.execute("INSERT INTO j VALUES (%s)", ('{"a": "é"}',))
        cursor.execute("SELECT doc FROM j")
        assert cursor.fetchone() == ('{"a": "é"}',)
        if server.name == "mysql":
            # Reported as binary (63), yet sent in the session's charset.
            cursor.execute("""SELECT CAST('{"b": "ñ"}' AS JSON)""")
            assert cursor.fetchone() == ('{"b": "ñ"}',)


def test_mogrify_and_literal_in_the_session_charset(server: Server) -> None:
    with server.connect(charset="cp1251") as conn:
        cursor = conn.cursor()
        assert cursor.mogrify("SELECT %s", ("Привет",)) == "SELECT 'Привет'"
        assert conn.literal("Привет") == "'Привет'"
    with server.connect(charset="gbk") as conn:
        assert conn.literal(b"\x00") == "X'00'"


# Collation, reset, change_user, set_character_set ----------------------------


def _session(conn: rustmy.Connection) -> tuple[Any, ...]:
    cursor = conn.cursor()
    cursor.execute("SELECT @@character_set_client, @@collation_connection")
    return cursor.fetchone() or ()


def test_explicit_collation_survives_reset_and_change_user(server: Server) -> None:
    with server.connect(collation="utf8mb4_bin") as conn:
        assert _session(conn) == ("utf8mb4", "utf8mb4_bin")
        conn.reset_connection()
        assert _session(conn) == ("utf8mb4", "utf8mb4_bin")
        conn.change_user("rustmy", "rustmy", "rustmy_test")
        assert _session(conn) == ("utf8mb4", "utf8mb4_bin")


def test_non_default_charset_survives_reset_and_change_user(server: Server) -> None:
    # MySQL's reset returns to the server's default character set.
    with server.connect(charset="latin1") as conn:
        assert _session(conn) == ("latin1", "latin1_swedish_ci")
        conn.reset_connection()
        assert _session(conn) == ("latin1", "latin1_swedish_ci")
        conn.change_user("rustmy", "rustmy", "rustmy_test")
        assert _session(conn) == ("latin1", "latin1_swedish_ci")
        cursor = conn.cursor()
        cursor.execute("SELECT %s", ("café",))
        assert cursor.fetchone() == ("café",)


def test_invalid_collations(server: Server) -> None:
    with pytest.raises(rustmy.ProgrammingError, match="invalid collation"):
        server.connect(collation="utf8mb4_bin; DROP TABLE x")
    with pytest.raises(rustmy.Error) as excinfo:
        server.connect(charset="latin1", collation="utf8mb4_bin")
    assert excinfo.value.args[0] == 1253  # COLLATION not valid for CHARACTER SET


def test_set_character_set(conn: rustmy.Connection) -> None:
    conn.set_character_set("cp1251", "cp1251_bin")
    assert conn.character_set_name() == "cp1251"
    assert conn.encoding == "cp1251"
    assert _session(conn) == ("cp1251", "cp1251_bin")
    cursor = conn.cursor()
    cursor.execute("SELECT %s", ("Привет",))
    assert cursor.fetchone() == ("Привет",)
    conn.reset_connection()
    assert _session(conn) == ("cp1251", "cp1251_bin")
    # An unsupported character set leaves the session as it was.
    with pytest.raises(rustmy.NotSupportedError):
        conn.set_character_set("binary")
    assert conn.character_set_name() == "cp1251"
    conn.set_character_set("utf8mb4")
    cursor.execute("SELECT %s", ("😀",))
    assert cursor.fetchone() == ("😀",)


def test_init_command_is_encoded_in_the_session_charset(server: Server) -> None:
    with server.connect(charset="latin1", init_command="SET @greeting = 'olé'") as conn:
        cursor = conn.cursor()
        cursor.execute("SELECT @greeting")
        assert cursor.fetchone() == ("olé",)
