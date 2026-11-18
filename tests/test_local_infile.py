"""``LOAD DATA LOCAL INFILE``: ``connect(local_infile_dir=…)``.

The refusals of a malicious server (a file after a ``SELECT``, another file
than the statement's) are in ``crates/proto/tests/local_infile.rs``, against
a fake server; these run the real thing on both servers.
"""

from __future__ import annotations

import os

from typing import TYPE_CHECKING

import pytest

import rustmy

from rustmy.cursors import PreparedCursor
from rustmy.cursors import SSCursor


if TYPE_CHECKING:
    from collections.abc import Iterator
    from pathlib import Path

    from tests.conftest import Server


TABLE = "CREATE TEMPORARY TABLE loaded (id INT PRIMARY KEY, s VARCHAR(64))"
LOAD = "LOAD DATA LOCAL INFILE %s INTO TABLE loaded FIELDS TERMINATED BY ','"


@pytest.fixture
def allowed(tmp_path: Path) -> Path:
    """The directory connections allow (resolved: macOS's temporary
    directory is behind a symbolic link)."""
    path = tmp_path.resolve() / "allowed"
    path.mkdir()
    return path


@pytest.fixture
def loader(server: Server, allowed: Path) -> Iterator[rustmy.Connection]:
    with server.connect(local_infile_dir=allowed, autocommit=True) as conn:
        conn.cursor().execute(TABLE)
        yield conn


def write_rows(path: Path, start: int, count: int) -> Path:
    path.write_text("".join(f"{i},row {i}\n" for i in range(start, start + count)))
    return path


def loaded(conn: rustmy.Connection) -> tuple[tuple[object, ...], ...]:
    cursor = conn.cursor()
    cursor.execute("SELECT id, s FROM loaded ORDER BY id")
    return cursor.fetchall()


def assert_rejected(conn: rustmy.Connection, query: str, reason: str) -> None:
    cursor = conn.cursor()
    with pytest.raises(rustmy.OperationalError) as info:
        cursor.execute(query)
    code, message = info.value.args
    assert code == 2068
    assert reason in message
    # An empty file went instead: nothing loaded, the connection in sync.
    assert loaded(conn) == ()
    cursor.execute("SELECT 1")
    assert cursor.fetchone() == (1,)


@pytest.mark.parametrize(
    "name",
    ["data.csv", 'it\'s "quoted" \\ and %_.csv', "ñandú 🦀 spaced.csv"],
)
def test_loads_files_from_the_directory(
    loader: rustmy.Connection, allowed: Path, name: str
) -> None:
    path = write_rows(allowed / name, 1, 3)
    cursor = loader.cursor()
    assert cursor.execute(LOAD, (str(path),)) == 3
    assert loaded(loader) == ((1, "row 1"), (2, "row 2"), (3, "row 3"))


def test_without_backslash_escapes(loader: rustmy.Connection, allowed: Path) -> None:
    path = write_rows(allowed / "back\\slash's.csv", 1, 2)
    cursor = loader.cursor()
    cursor.execute("SET SESSION sql_mode = 'NO_BACKSLASH_ESCAPES'")
    # Interpolation follows the mode, and so does the name the client expects.
    assert cursor.execute(LOAD, (str(path),)) == 2
    literal = str(path).replace("'", "''")
    cursor.execute("DELETE FROM loaded")
    assert cursor.execute(LOAD.replace("%s", f"'{literal}'")) == 2


def test_relative_names_start_at_the_working_directory(
    loader: rustmy.Connection, allowed: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    write_rows(allowed / "relative.csv", 1, 1)
    monkeypatch.chdir(allowed)
    assert loader.cursor().execute(LOAD, ("relative.csv",)) == 1
    # From elsewhere, the same name is another file: outside the directory.
    monkeypatch.chdir(allowed.parent)
    write_rows(allowed.parent / "relative.csv", 5, 1)
    with pytest.raises(rustmy.OperationalError, match="outside local_infile_dir"):
        loader.cursor().execute(LOAD, ("relative.csv",))


def test_refuses_files_outside_the_directory(
    loader: rustmy.Connection, allowed: Path
) -> None:
    outside = write_rows(allowed.parent / "outside.csv", 1, 1)
    query = LOAD % f"'{outside}'"
    assert_rejected(loader, query, "outside local_infile_dir")
    assert_rejected(loader, LOAD % f"'{allowed}/../outside.csv'", "outside")


@pytest.mark.skipif(os.name == "nt", reason="symbolic links need privileges")
def test_refuses_symbolic_links_out_of_the_directory(
    loader: rustmy.Connection, allowed: Path
) -> None:
    outside = write_rows(allowed.parent / "secret.csv", 1, 1)
    (allowed / "link.csv").symlink_to(outside)
    assert_rejected(loader, LOAD % f"'{allowed / 'link.csv'}'", "outside")
    # A link that stays inside is fine.
    write_rows(allowed / "inside.csv", 1, 1)
    (allowed / "inner-link.csv").symlink_to(allowed / "inside.csv")
    assert loader.cursor().execute(LOAD % f"'{allowed / 'inner-link.csv'}'") == 1


def test_refuses_what_is_not_a_readable_file(
    loader: rustmy.Connection, allowed: Path
) -> None:
    assert_rejected(loader, LOAD % f"'{allowed / 'missing.csv'}'", "missing.csv")
    (allowed / "subdir").mkdir()
    assert_rejected(loader, LOAD % f"'{allowed / 'subdir'}'", "not a regular file")


def test_off_by_default(server: Server, allowed: Path) -> None:
    path = write_rows(allowed / "data.csv", 1, 1)
    with server.connect() as conn:
        cursor = conn.cursor()
        cursor.execute(TABLE)
        # The client does not offer it, so the server refuses on its own.
        with pytest.raises(rustmy.DatabaseError) as info:
            cursor.execute(LOAD, (str(path),))
        assert info.value.args[0] == (3948 if server.name == "mysql" else 4166)


def test_server_may_refuse(server: Server, allowed: Path) -> None:
    path = write_rows(allowed / "data.csv", 1, 1)
    with server.connect(user="root", database=None, autocommit=True) as root:
        root.cursor().execute("SET GLOBAL local_infile = 0")
        try:
            with server.connect(local_infile_dir=allowed) as conn:
                cursor = conn.cursor()
                cursor.execute(TABLE)
                with pytest.raises(rustmy.DatabaseError) as info:
                    cursor.execute(LOAD, (str(path),))
                assert info.value.args[0] == (3948 if server.name == "mysql" else 4166)
        finally:
            root.cursor().execute("SET GLOBAL local_infile = 1")


@pytest.mark.parametrize("compress", [False, "zlib", "zstd"])
def test_large_files(server: Server, allowed: Path, *, compress: bool | str) -> None:
    """Over 16 MiB: many packets, compressed ones too."""
    path = allowed / "large.csv"
    count = 400_000
    with path.open("w") as out:
        for i in range(count):
            out.write(f"{i},{'x' * 40}\n")
    assert path.stat().st_size > 16 * 1024 * 1024
    with server.connect(
        local_infile_dir=allowed, compress=compress, autocommit=True
    ) as conn:
        cursor = conn.cursor()
        cursor.execute(TABLE)
        assert cursor.execute(LOAD, (str(path),)) == count
        cursor.execute("SELECT COUNT(*), SUM(id), MIN(LENGTH(s)) FROM loaded")
        assert cursor.fetchone() == (count, count * (count - 1) // 2, 40)


def test_executemany_loads_one_at_a_time(
    loader: rustmy.Connection, allowed: Path
) -> None:
    files = [(str(write_rows(allowed / f"part{n}.csv", n * 10, 3)),) for n in range(5)]
    cursor = loader.cursor()
    assert cursor.executemany(LOAD, files) == 15
    assert len(loaded(loader)) == 15


def test_several_statements(server: Server, allowed: Path) -> None:
    first = write_rows(allowed / "first.csv", 1, 2)
    second = write_rows(allowed / "second.csv", 3, 1)
    with server.connect(
        local_infile_dir=allowed, multi_statements=True, autocommit=True
    ) as conn:
        cursor = conn.cursor()
        cursor.execute(TABLE)
        load_first, load_second = (LOAD % f"'{path}'" for path in (first, second))
        cursor.execute(f"{load_first}; SELECT COUNT(*) FROM loaded; {load_second}")
        assert cursor.rowcount == 2
        assert cursor.nextset()
        assert cursor.fetchall() == ((2,),)
        assert cursor.nextset()
        assert cursor.rowcount == 1
        assert not cursor.nextset()
        assert len(loaded(conn)) == 3


def test_other_cursors(loader: rustmy.Connection, allowed: Path) -> None:
    path = write_rows(allowed / "data.csv", 1, 2)
    assert loader.cursor(SSCursor).execute(LOAD, (str(path),)) == 2
    # The server cannot prepare it with a placeholder for the name; without
    # placeholders it goes as text.
    prepared = loader.cursor(PreparedCursor)
    with pytest.raises(rustmy.ProgrammingError):
        prepared.execute(LOAD, (str(path),))
    loader.cursor().execute("DELETE FROM loaded")
    assert prepared.execute(LOAD % f"'{path}'") == 2


def test_load_xml(loader: rustmy.Connection, allowed: Path) -> None:
    path = allowed / "rows.xml"
    path.write_text(
        '<rows><row id="1" s="one"/><row><field name="id">2</field>'
        '<field name="s">two</field></row></rows>'
    )
    cursor = loader.cursor()
    query = "LOAD XML LOCAL INFILE %s INTO TABLE loaded"
    assert cursor.execute(query, (str(path),)) == 2
    assert loaded(loader) == ((1, "one"), (2, "two"))


def test_connect_arguments(allowed: Path) -> None:
    with pytest.raises(ValueError, match="local_infile=True needs local_infile_dir"):
        rustmy.connect(host="127.0.0.1", port=1, local_infile=True)
    for path in (allowed / "missing", write_rows(allowed / "file.csv", 1, 1)):
        with pytest.raises(rustmy.InterfaceError, match="local_infile_dir"):
            rustmy.connect(host="127.0.0.1", port=1, local_infile_dir=path)
