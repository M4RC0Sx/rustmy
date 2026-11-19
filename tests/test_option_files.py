"""MySQL option files: ``read_default_file`` / ``read_default_group``.

The parsing cases' expected values are what ``my_print_defaults`` (MySQL's
client library, 26.10) printed for the same lines; the precedence cases were
checked against mysqlclient on that library.
"""

from __future__ import annotations

import sys

from typing import TYPE_CHECKING

import pytest

import rustmy

from rustmy import _option_files


if TYPE_CHECKING:
    from pathlib import Path

    from tests.conftest import Server


# (line in a group, expected value) — from my_print_defaults.
VALUE_CASES = [
    ("rustmy", "rustmy"),
    ("  spaced value  ", "spaced value"),
    ("'quoted'", "quoted"),
    ('"quoted"', "quoted"),
    ("plain # comment", "plain"),
    ("rust#my", "rust"),
    ("'a#b' # c", "a#b"),
    ('"rustmy" trailing', '"rustmy" trailing'),
    ('"unterminated', '"unterminated'),
    ("rus\\tmy", "rus\tmy"),
    ("rus\\smy", "rus my"),
    ("back\\\\slash", "back\\slash"),
    ("rustm\\y", "rustm\\y"),
    ("'rustm\\y'", "rustm\\y"),
    ("C:\\Program Files\\x", "C:\\Program Files\\x"),
    ('"q\\"uote"', 'q"uote'),
    ("a\\'b", "a'b"),
    ("'it\\'s'", "it's"),
    ("x#y'z", "x"),
    ("\\", "\\"),
]


def write(path: Path, text: str) -> Path:
    path.write_text(text, encoding="utf-8")
    # Whatever the umask: a file anyone may write to would be ignored.
    path.chmod(0o644)
    return path


@pytest.mark.parametrize(("raw", "expected"), VALUE_CASES)
def test_values_match_the_client_library(
    tmp_path: Path, raw: str, expected: str
) -> None:
    cnf = write(tmp_path / "my.cnf", f"[client]\nkey = {raw}\n")
    assert _option_files.read(cnf, None) == {"key": expected}


def test_names_comments_and_flags(tmp_path: Path) -> None:
    cnf = write(
        tmp_path / "my.cnf",
        "# comment\n; also a comment\n[client]\n"
        "Default_Character_Set = latin1\nflag\nport=1\nport=2\n",
    )
    assert _option_files.read(cnf, None) == {
        "default-character-set": "latin1",
        "flag": "",
        "port": "2",
    }


def test_client_and_group_in_file_order(tmp_path: Path) -> None:
    cnf = write(
        tmp_path / "my.cnf",
        "[app]\nuser = app\n[client]\nuser = client\nhost = h\n"
        "[other]\nhost = other\n[APP]\nport = 1\n",
    )
    # The later line wins, whichever group it is in; group names ignore case.
    assert _option_files.read(cnf, "app") == {
        "user": "client",
        "host": "h",
        "port": "1",
    }
    assert _option_files.read(cnf, None) == {"user": "client", "host": "h"}


def test_home_is_expanded(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    # As libmysqlclient does: read_default_file="~/.my.cnf" works.
    monkeypatch.setenv("HOME", str(tmp_path))
    monkeypatch.setenv("USERPROFILE", str(tmp_path))  # Windows
    write(tmp_path / ".my.cnf", "[client]\nuser = home\n")
    assert _option_files.read("~/.my.cnf", None) == {"user": "home"}


def test_missing_file_is_skipped(tmp_path: Path) -> None:
    assert _option_files.read(tmp_path / "nope.cnf", "app") == {}


def test_include_and_includedir(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    (tmp_path / "d").mkdir()
    write(tmp_path / "d" / "b.cnf", "[client]\nport = 2\n")
    write(tmp_path / "d" / "a.cnf", "[client]\nport = 1\nuser = a\n")
    write(tmp_path / "d" / "c.txt", "[client]\nuser = ignored\n")
    write(tmp_path / "inc.cnf", "[client]\nhost = included\n")
    cnf = write(
        tmp_path / "my.cnf",
        f"[client]\n!include {tmp_path / 'inc.cnf'}\n!includedir {tmp_path / 'd'}\n",
    )
    assert _option_files.read(cnf, None) == {
        "host": "included",
        "port": "2",
        "user": "a",
    }
    # A relative include is relative to the current directory.
    write(tmp_path / "rel.cnf", "[client]\n!include inc.cnf\n")
    monkeypatch.chdir(tmp_path)
    assert _option_files.read(tmp_path / "rel.cnf", None) == {"host": "included"}


def test_missing_include_stops_the_file(tmp_path: Path) -> None:
    cnf = write(
        tmp_path / "my.cnf",
        f"[client]\nuser = u\n!include {tmp_path / 'nope.cnf'}\nhost = h\n",
    )
    with pytest.warns(RuntimeWarning, match="cannot read included file"):
        assert _option_files.read(cnf, None) == {"user": "u"}


@pytest.mark.skipif(sys.platform == "win32", reason="POSIX permissions")
def test_files_anyone_can_write_are_ignored(tmp_path: Path) -> None:
    """As my_print_defaults (MySQL 8.4): the file is skipped with a warning,
    and a file including it goes on; writable by the group is fine."""
    open_file = write(tmp_path / "open.cnf", "[client]\nuser = intruder\n")
    open_file.chmod(0o666)
    with pytest.warns(RuntimeWarning, match="anyone can write"):
        assert _option_files.read(open_file, None) == {}
    cnf = write(
        tmp_path / "my.cnf",
        f"[client]\nhost = main\n!include {open_file}\nport = 1\n",
    )
    cnf.chmod(0o644)
    with pytest.warns(RuntimeWarning, match="anyone can write"):
        assert _option_files.read(cnf, None) == {"host": "main", "port": "1"}
    open_file.chmod(0o664)
    assert _option_files.read(open_file, None) == {"user": "intruder"}


def test_include_loop_is_cut(tmp_path: Path) -> None:
    cnf = tmp_path / "my.cnf"
    write(cnf, f"[client]\nuser = u\n!include {cnf}\n")
    with pytest.warns(RuntimeWarning, match="nested too deeply"):
        assert _option_files.read(cnf, None) == {"user": "u"}


def test_group_alone_reads_the_default_files(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr(
        _option_files, "default_files", lambda: [tmp_path / "a.cnf", tmp_path / "b"]
    )
    write(tmp_path / "a.cnf", "[app]\nuser = from-default\n")
    assert _option_files.read(None, "app") == {"user": "from-default"}


def test_connect_args() -> None:
    args = _option_files.connect_args(
        {
            "host": "h",
            "port": "3307",
            "socket": "/s",
            "default-character-set": "latin1",
            "connect-timeout": "5",
            "ssl-mode": "DISABLED",
            "password": "",
            "multi-queries": "",
            "return-found-rows": "0",
            "no-beep": "",
        }
    )
    assert args == {
        "host": "h",
        "port": 3307,
        "unix_socket": "/s",
        "charset": "latin1",
        "connect_timeout": 5,
        "ssl_mode": "DISABLED",
        "multi_statements": True,
        "found_rows": False,
    }
    with pytest.raises(ValueError, match="port must be a number"):
        _option_files.connect_args({"port": "x"})


@pytest.mark.parametrize(
    ("options", "expected"),
    [
        ({"compress": ""}, {"compress": True}),
        ({"compress": "0"}, {"compress": False}),
        ({"compression-algorithms": "zstd,zlib"}, {"compress": "zstd"}),
        ({"compression-algorithms": "uncompressed,ZLIB"}, {"compress": "zlib"}),
        ({"compression-algorithms": "uncompressed"}, {}),
        ({"zstd-compression-level": "7"}, {"zstd_level": 7}),
    ],
)
def test_compression_options(
    options: dict[str, str], expected: dict[str, object]
) -> None:
    assert _option_files.connect_args(options) == expected


@pytest.mark.parametrize(
    ("options", "expected"),
    [
        ({"get-server-public-key": ""}, True),
        ({"get-server-public-key": "1"}, True),
        ({"get-server-public-key": "0"}, False),
        ({"get-server-public-key": "OFF"}, False),
        ({"skip-get-server-public-key": ""}, False),
        ({"disable-get-server-public-key": ""}, False),
        ({"enable-get-server-public-key": ""}, True),
    ],
)
def test_get_server_public_key_option(
    options: dict[str, str], *, expected: bool
) -> None:
    assert _option_files.connect_args(options) == {"get_server_public_key": expected}


def test_unknown_skip_options_are_ignored() -> None:
    assert _option_files.connect_args({"skip-ssl": "", "skip-compress": ""}) == {}


# Against the servers.


def server_file(server: Server, path: Path, extra: str = "") -> Path:
    return write(
        path,
        f"[client]\nhost = {server.host}\nport = {server.port}\n"
        f"user = rustmy\npassword = rustmy\ndatabase = rustmy_test\n{extra}",
    )


def test_connects_from_a_file(server: Server, tmp_path: Path) -> None:
    cnf = server_file(
        server,
        tmp_path / "my.cnf",
        "[app]\ndefault-character-set = latin1\ninit-command = SET @app = 1\n",
    )
    with rustmy.connect(read_default_file=cnf, read_default_group="app") as conn:
        cursor = conn.cursor()
        cursor.execute("SELECT CURRENT_USER(), DATABASE(), @app")
        assert cursor.fetchone() == ("rustmy@%", "rustmy_test", 1)
        assert conn.character_set_name() == "latin1"


def test_arguments_win_over_the_file(server: Server, tmp_path: Path) -> None:
    cnf = write(
        tmp_path / "my.cnf",
        f"[client]\nhost = {server.host}\nport = {server.port}\n"
        "user = nobody\npassword = wrong\ndatabase = information_schema\n",
    )
    with rustmy.connect(
        read_default_file=cnf, user="rustmy", password="rustmy", database=None
    ) as conn:
        cursor = conn.cursor()
        cursor.execute("SELECT CURRENT_USER(), DATABASE()")
        # database=None is "not given": the file's applies.
        assert cursor.fetchone() == ("rustmy@%", "information_schema")
    with pytest.raises(rustmy.OperationalError) as info:
        rustmy.connect(read_default_file=cnf, port=1)
    assert info.value.args[0] == 2003


def test_tls_and_flags_from_the_file(server: Server, tmp_path: Path) -> None:
    cnf = server_file(
        server, tmp_path / "my.cnf", "ssl-mode = DISABLED\nreturn-found-rows\n"
    )
    with rustmy.connect(read_default_file=cnf) as conn:
        assert not conn.tls
        cursor = conn.cursor()
        cursor.execute("CREATE TEMPORARY TABLE found (id INT PRIMARY KEY, v INT)")
        cursor.execute("INSERT INTO found VALUES (1, 1)")
        # FOUND_ROWS: a matched row counts even when nothing changes.
        assert cursor.execute("UPDATE found SET v = 1 WHERE id = 1") == 1
    # An ssl dict covers TLS on its own: the file's ssl-mode is not used.
    with rustmy.connect(read_default_file=cnf, ssl={}) as conn:
        assert conn.tls


def test_compression_from_the_file(server: Server, tmp_path: Path) -> None:
    cnf = server_file(
        server,
        tmp_path / "my.cnf",
        "compression-algorithms = zstd,zlib\nzstd-compression-level = 5\n",
    )
    zstd = "zstd" if server.name == "mysql" else "zlib"
    with rustmy.connect(read_default_file=cnf) as conn:
        assert conn.compression == zstd
    # An argument wins over the file.
    with rustmy.connect(read_default_file=cnf, compress="zlib") as conn:
        assert conn.compression == "zlib"


@pytest.mark.parametrize(
    ("options", "expected"),
    [
        ({"local-infile": ""}, {"local_infile": True}),
        ({"local-infile": "0"}, {"local_infile": False}),
        ({"load-data-local-dir": "/srv/in"}, {"local_infile_dir": "/srv/in"}),
    ],
)
def test_local_infile_options(
    options: dict[str, str], expected: dict[str, object]
) -> None:
    assert _option_files.connect_args(options) == expected


def test_local_infile_from_the_file(server: Server, tmp_path: Path) -> None:
    allowed = tmp_path.resolve()
    (allowed / "one.csv").write_text("1\n")
    load = f"LOAD DATA LOCAL INFILE '{allowed / 'one.csv'}' INTO TABLE loaded"
    cnf = server_file(server, tmp_path / "my.cnf", f"load-data-local-dir = {allowed}\n")
    with rustmy.connect(read_default_file=cnf) as conn:
        cursor = conn.cursor()
        cursor.execute("CREATE TEMPORARY TABLE loaded (id INT)")
        assert cursor.execute(load) == 1
    # `local-infile` alone is not enough: rustmy needs the directory.
    cnf = server_file(server, tmp_path / "other.cnf", "local-infile\n")
    with pytest.raises(ValueError, match="needs local_infile_dir"):
        rustmy.connect(read_default_file=cnf)


def test_bad_option_value(server: Server, tmp_path: Path) -> None:
    cnf = server_file(server, tmp_path / "my.cnf", "connect-timeout = soon\n")
    with pytest.raises(rustmy.ProgrammingError, match="connect-timeout"):
        rustmy.connect(read_default_file=cnf)
