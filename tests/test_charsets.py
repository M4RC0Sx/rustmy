"""Connection character sets and collations against real servers.

Every character set in any supported server's ``SHOW CHARACTER SET`` is in
``CATALOGUE``, either supported (with a sample of its alphabet) or not
(with the reason the connection refuses it): a server version that adds
one fails ``test_catalogue_covers_the_server``.
"""

from __future__ import annotations

import uuid

from dataclasses import dataclass
from typing import TYPE_CHECKING
from typing import Any

import pytest

import rustmy

from rustmy.cursors import DictCursor
from rustmy.cursors import SSCursor


if TYPE_CHECKING:
    from collections.abc import Iterator

    from tests.conftest import Server


@dataclass(frozen=True)
class Supported:
    #: Python codec, as reported by ``Connection.encoding``.
    encoding: str
    #: Text in the character set's alphabet.
    sample: str
    single_byte: bool = False


@dataclass(frozen=True)
class Unsupported:
    #: Part of the error message.
    reason: str


def _single(encoding: str, sample: str) -> Supported:
    return Supported(encoding, sample, single_byte=True)


CATALOGUE: dict[str, Supported | Unsupported] = {
    "utf8mb4": Supported("utf8", "emoji 😀, ñ, €, 日本"),
    "utf8mb3": Supported("utf8", "BMP only: ñ, €, 日本"),
    "ascii": _single("ascii", "plain ASCII text"),
    "latin1": _single("cp1252", "Señor, ¿un café? Ça coûte 5 €"),
    "latin2": _single("iso8859_2", "Zażółć gęślą jaźń"),
    "latin5": _single("iso8859_9", "Pijamalı hasta yağız şoföre çabucak güvendi"),  # noqa: RUF001
    "latin7": _single("iso8859_13", "Ąžuolas ir šešėlis, Rīga"),
    "cp1250": _single("cp1250", "Příliš žluťoučký kůň"),
    "cp1251": _single("cp1251", "Съешь же ещё этих мягких булок"),
    "cp1256": _single("cp1256", "مرحبا بالعالم"),
    "cp1257": _single("cp1257", "Ąžuolas ir šešėlis, Rīga"),
    "cp850": _single("cp850", "Señor, ¿un café? Ça coûte"),
    "cp852": _single("cp852", "Zażółć gęślą jaźń"),
    "cp866": _single("cp866", "Съешь же ещё этих булок"),
    "koi8r": _single("koi8_r", "Привет, мир"),
    "koi8u": _single("koi8_u", "Привіт, світе! Ґанок"),
    "greek": _single("iso8859_7", "Καλημέρα κόσμε"),
    "hebrew": _single("iso8859_8", "שלום עולם"),
    "macroman": _single("mac_roman", "Café, naïve, Œuvre ©"),
    "macce": _single("mac_latin2", "Příliš žluťoučký kůň"),
    "tis620": _single("tis_620", "สวัสดีชาวโลก"),
    "big5": Supported("big5", "運動會與許功蓋"),
    "cp932": Supported("cp932", "日本語のテキスト①"),
    # Hangul syllables outside EUC-KR's 2350 (갂, 똠) need the server's
    # Unified Hangul Code.
    "euckr": Supported("cp949", "안녕하세요 세계 갂똠"),
    "gb2312": Supported("gb2312", "你好，世界"),  # noqa: RUF001  # full-width comma on purpose
    "gb18030": Supported("gb18030", "你好，世界 😀 ḿ"),  # noqa: RUF001
    "gbk": Supported("gbk", "你好，世界"),  # noqa: RUF001
    "sjis": Supported("shift_jis", "日本語のテキスト"),
    "ujis": Supported("euc_jp", "日本語のテキスト"),
    "binary": Unsupported("needs a text character set"),
    "eucjpms": Unsupported("no exact codec"),
    "ucs2": Unsupported("does not accept it as a connection character set"),
    "utf16": Unsupported("does not accept it as a connection character set"),
    "utf16le": Unsupported("does not accept it as a connection character set"),
    "utf32": Unsupported("does not accept it as a connection character set"),
    "armscii8": Unsupported("no codec"),
    "dec8": Unsupported("no codec"),
    "hp8": Unsupported("no codec"),
    "swe7": Unsupported("no codec"),
    "keybcs2": Unsupported("no codec"),
    "geostd8": Unsupported("no codec"),
}

SUPPORTED = [name for name, cs in CATALOGUE.items() if isinstance(cs, Supported)]
UNSUPPORTED = [name for name, cs in CATALOGUE.items() if isinstance(cs, Unsupported)]
SINGLE_BYTE = [
    name
    for name, cs in CATALOGUE.items()
    if isinstance(cs, Supported) and cs.single_byte
]
MULTI_BYTE = [
    name
    for name, cs in CATALOGUE.items()
    if isinstance(cs, Supported) and not cs.single_byte and cs.encoding != "utf8"
]

#: Multi-byte character sets where a character's last byte can be 0x5C
#: (backslash), the shape behind CVE-2006-2753.
BACKSLASH_TRAIL = ["gbk", "big5", "cp932", "sjis"]


def _supported(name: str) -> Supported:
    cs = CATALOGUE[name]
    assert isinstance(cs, Supported)
    return cs


def _server_charsets(conn: rustmy.Connection) -> set[str]:
    cursor = conn.cursor()
    cursor.execute("SHOW CHARACTER SET")
    return {row[0] for row in cursor.fetchall()}


def _supports(conn: rustmy.Connection, charset: str) -> bool:
    return charset in _server_charsets(conn)


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


# Catalogue -----------------------------------------------------------------


def test_catalogue_covers_the_server(conn: rustmy.Connection) -> None:
    assert _server_charsets(conn) <= set(CATALOGUE)


@pytest.mark.parametrize("charset", UNSUPPORTED)
def test_unsupported_charsets_are_refused(
    server: Server, conn: rustmy.Connection, charset: str
) -> None:
    if not _supports(conn, charset):
        pytest.skip(f"{server.name} has no {charset}")
    reason = _supported_reason(charset)
    with pytest.raises(rustmy.NotSupportedError, match=reason):
        server.connect(charset=charset)
    with pytest.raises(rustmy.NotSupportedError, match=reason):
        conn.set_character_set(charset)
    assert _session(conn) == ("utf8mb4", _session(conn)[1])
    assert conn.character_set_name() == "utf8mb4"


def _supported_reason(charset: str) -> str:
    cs = CATALOGUE[charset]
    assert isinstance(cs, Unsupported)
    return cs.reason


def test_charset_the_server_lacks_fails_to_connect(
    server: Server, conn: rustmy.Connection
) -> None:
    # A server keeps its default character set when the handshake asks for
    # a collation id it does not know; SET NAMES turns that into an error
    # instead of a session that silently decodes the wrong way.
    if _supports(conn, "gb18030"):
        pytest.skip(f"{server.name} has gb18030")
    with pytest.raises(rustmy.ProgrammingError) as excinfo:
        server.connect(charset="gb18030")
    assert excinfo.value.args[0] == 1115  # ER_UNKNOWN_CHARACTER_SET


# Default collation ---------------------------------------------------------


def test_session_uses_the_servers_default_collation(conn: rustmy.Connection) -> None:
    # Comparing a column with a user variable fails (1267, "Illegal mix of
    # collations") when the session's collation differs from the table's.
    cursor = conn.cursor()
    cursor.execute("CREATE TEMPORARY TABLE t (s VARCHAR(10))")
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


def _connect_or_skip(server: Server, conn: rustmy.Connection, charset: str) -> None:
    if not _supports(conn, charset):
        pytest.skip(f"{server.name} has no {charset}")


@pytest.mark.parametrize("charset", SUPPORTED)
def test_text_and_bytes_round_trip(
    server: Server, conn: rustmy.Connection, table: str, charset: str
) -> None:
    _connect_or_skip(server, conn, charset)
    cs = _supported(charset)
    text = cs.sample
    blob = bytes(range(256))
    with server.connect(charset=charset) as other:
        assert other.character_set_name() == charset
        assert other.encoding == cs.encoding
        cursor = other.cursor()
        cursor.execute("SELECT @@character_set_client, @@character_set_results")
        assert cursor.fetchone() == (charset, charset)
        # Parameters, the query text itself and binary data.
        cursor.execute(f"SELECT %s, '{text}', %s", (text, blob))
        assert cursor.fetchone() == (text, text, blob)
        # What the server understood, independently of our decoder.
        cursor.execute("SELECT HEX(CONVERT(%s USING utf8mb4))", (text,))
        assert cursor.fetchone() == (text.encode().hex().upper(),)
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


@pytest.mark.parametrize("charset", SUPPORTED)
def test_user_variables_compare_with_columns(
    server: Server, conn: rustmy.Connection, charset: str
) -> None:
    # The handshake picks the character set's default collation: a column
    # in that character set (with its default collation) and a utf8mb4 one
    # both compare with a user variable without "Illegal mix of collations".
    _connect_or_skip(server, conn, charset)
    text = _supported(charset).sample
    with server.connect(charset=charset) as other:
        cursor = other.cursor()
        cursor.execute(
            f"CREATE TEMPORARY TABLE t (own VARCHAR(100) CHARACTER SET {charset}, "
            "utf VARCHAR(100) CHARACTER SET utf8mb4)"
        )
        cursor.execute("INSERT INTO t VALUES (%s, %s)", (text, text))
        cursor.execute("SET @v = %s", (text,))
        assert cursor.execute("SELECT 1 FROM t WHERE own = @v") == 1
        assert cursor.execute("SELECT 1 FROM t WHERE utf = @v") == 1
        cursor.execute("SELECT own, utf FROM t")
        assert cursor.fetchone() == (text, text)


@pytest.mark.parametrize("charset", SUPPORTED)
def test_streaming_dict_and_executemany(
    server: Server, conn: rustmy.Connection, table: str, charset: str
) -> None:
    _connect_or_skip(server, conn, charset)
    text = _supported(charset).sample
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


# The server's definition of each character set --------------------------------


def _server_meaning(conn: rustmy.Connection, charset: str, data: bytes) -> str | None:
    """What the server reads ``data`` in ``charset`` as (``None``: nothing)."""
    cursor = conn.cursor()
    cursor.execute(
        f"SELECT HEX(CONVERT(CONVERT(UNHEX(%s) USING {charset}) USING utf8mb4))",
        (data.hex(),),
    )
    row = cursor.fetchone()
    hexed = None if row is None else row[0]
    if hexed is None:  # MySQL: invalid input
        return None
    text = bytes.fromhex(hexed).decode()
    # MariaDB: invalid input becomes `?` or U+FFFD.
    if text in {"?", "�"} and data != b"?":
        return None
    return text


@pytest.mark.parametrize("charset", SINGLE_BYTE)
def test_single_byte_charsets_follow_the_server(
    server: Server, conn: rustmy.Connection, charset: str
) -> None:
    # Every byte, as the server defines it: decoded to the server's
    # character, encoded back from it, and refused where the server has
    # no character (even if Python's codec has one).
    _connect_or_skip(server, conn, charset)
    codec = _supported(charset).encoding
    meanings = {b: _server_meaning(conn, charset, bytes([b])) for b in range(256)}
    defined = bytes(b for b, c in meanings.items() if c is not None)
    expected = "".join(c for c in meanings.values() if c is not None)
    with server.connect(charset=charset) as other:
        cursor = other.cursor()
        cursor.execute(f"SELECT CONVERT(UNHEX(%s) USING {charset})", (defined.hex(),))
        assert cursor.fetchone() == (expected,)
        cursor.execute("SELECT HEX(%s)", (expected,))
        assert cursor.fetchone() == (defined.hex().upper(),)
        for byte, meaning in meanings.items():
            if meaning is not None:
                continue
            with pytest.raises(rustmy.DataError, match="not a character"):
                cursor.execute(f"SELECT CONVERT(UNHEX('{byte:02X}') USING {charset})")
            try:
                python = bytes([byte]).decode(codec)
            except UnicodeDecodeError:
                continue
            if python in expected:
                continue  # the server has it elsewhere
            with pytest.raises(rustmy.DataError, match="cannot be encoded"):
                cursor.mogrify("SELECT %s", (python,))


def _codec_alphabet(codec: str, *, astral_step: int = 0) -> list[str]:
    """Every non-ASCII character Python's ``codec`` encodes, plus every
    ``astral_step``-th one above the BMP."""
    candidates = [*range(0x80, 0xD800), *range(0xE000, 0x10000)]
    if astral_step:
        candidates += range(0x10000, 0x110000, astral_step)
    alphabet = []
    for code in candidates:
        c = chr(code)
        try:
            c.encode(codec)
        except UnicodeEncodeError:
            continue
        alphabet.append(c)
    return alphabet


def _chunks(items: list[str], size: int = 2000) -> Iterator[list[str]]:
    for start in range(0, len(items), size):
        yield items[start : start + size]


@pytest.mark.slow
@pytest.mark.parametrize("charset", MULTI_BYTE)
def test_multi_byte_charsets_follow_the_server(
    server: Server, conn: rustmy.Connection, charset: str
) -> None:
    # Every character Python's codec knows: those the server can represent
    # go through a session in that character set unchanged both ways;
    # the rest are refused before sending. Characters go in chunks joined
    # by newlines, a byte that never occurs inside a multi-byte character.
    _connect_or_skip(server, conn, charset)
    codec = _supported(charset).encoding
    alphabet = _codec_alphabet(codec, astral_step=97 if charset == "gb18030" else 0)
    cursor = conn.cursor()
    representable: list[str] = []
    for chunk in _chunks(alphabet):
        cursor.execute(f"SELECT HEX(CONVERT(%s USING {charset}))", ("\n".join(chunk),))
        row = cursor.fetchone()
        assert row is not None
        encoded = bytes.fromhex(row[0]).split(b"\n")
        assert len(encoded) == len(chunk)
        representable += [c for c, e in zip(chunk, encoded, strict=True) if e != b"?"]
    missing = sorted(set(alphabet) - set(representable))
    with server.connect(charset=charset) as other:
        session = other.cursor()
        for chunk in _chunks(representable):
            text = "\n".join(chunk)
            session.execute("SELECT %s, HEX(CONVERT(%s USING utf8mb4))", (text, text))
            assert session.fetchone() == (text, text.encode().hex().upper())
        for c in missing:
            with pytest.raises(rustmy.DataError, match="cannot be encoded"):
                session.mogrify("SELECT %s", (c,))


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


@pytest.mark.parametrize("charset", ["sjis", "ujis"])
@pytest.mark.parametrize("sql_mode", [None, "NO_BACKSLASH_ESCAPES"])
def test_characters_python_encodes_as_ascii_are_refused(
    server: Server, charset: str, sql_mode: str | None
) -> None:
    # Python's shift_jis and euc_jp encode `¥` as 0x5C and `‾` as 0x7E, which
    # the server reads as a backslash and a tilde. Escaping happens on the
    # text, so `¥'` became `\\'` on the wire: an escaped backslash and a
    # quote that ends the string (SQL injection, found by this block).
    with server.connect(charset=charset, sql_mode=sql_mode) as conn:
        cursor = conn.cursor()
        for payload in ("¥' OR 1=1 -- ", "‾"):
            with pytest.raises(rustmy.DataError, match="cannot be encoded"):
                cursor.execute("SELECT %s", (payload,))
            with pytest.raises(rustmy.DataError, match="cannot be encoded"):
                cursor.mogrify("SELECT %s", (payload,))
        assert cursor.execute("SELECT %s", ("\\' OR 1=1 -- ",)) == 1
        assert cursor.fetchone() == ("\\' OR 1=1 -- ",)


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


# Non-ASCII user and database names -------------------------------------------


@pytest.fixture
def account(server: Server) -> Iterator[tuple[str, str, str]]:
    suffix = uuid.uuid4().hex[:6]
    user, password, database = f"josé_{suffix}", "contraseña", f"año_{suffix}"
    with server.connect(user="root", database=None) as root:
        cursor = root.cursor()
        cursor.execute(f"CREATE USER '{user}'@'%' IDENTIFIED BY '{password}'")
        cursor.execute(f"CREATE DATABASE `{database}`")
        cursor.execute(f"GRANT ALL ON `{database}`.* TO '{user}'@'%'")
        cursor.execute(f"GRANT ALL ON rustmy_test.* TO '{user}'@'%'")
        try:
            yield user, password, database
        finally:
            cursor.execute(f"DROP USER '{user}'@'%'")
            cursor.execute(f"DROP DATABASE `{database}`")


def _whoami(conn: rustmy.Connection) -> tuple[Any, ...]:
    cursor = conn.cursor()
    cursor.execute("SELECT SUBSTRING_INDEX(CURRENT_USER(), '@', 1), DATABASE()")
    return cursor.fetchone() or ()


@pytest.mark.parametrize("charset", ["utf8mb4", "latin1"])
def test_non_ascii_user_and_database(
    server: Server, account: tuple[str, str, str], charset: str
) -> None:
    # The server reads the user and database names in the session's
    # character set (PyMySQL does the same); the password goes as UTF-8
    # whatever the character set, since the server compares the bytes it
    # was set with.
    user, password, database = account
    with server.connect(
        user=user, password=password, database=database, charset=charset
    ) as conn:
        assert _whoami(conn) == (user, database)
        conn.select_db("rustmy_test")
        conn.select_db(database)
        assert _whoami(conn) == (user, database)
        conn.change_user(user, password, database)
        assert _whoami(conn) == (user, database)
    # change_user names the handshake's character set, not the session's.
    with server.connect(user=user, password=password, database=database) as conn:
        conn.set_character_set("latin1")
        conn.change_user(user, password, database)
        assert _whoami(conn) == (user, database)
        assert conn.character_set_name() == "latin1"
    # cp1250 has `é` but no `ñ`.
    with server.connect(user=user, password=password, charset="cp1250") as conn:
        assert _whoami(conn) == (user, "rustmy_test")
    with pytest.raises(rustmy.DataError, match="U\\+00F1"):
        server.connect(
            user=user, password=password, database=database, charset="cp1250"
        )
