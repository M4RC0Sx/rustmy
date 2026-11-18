"""MySQL option files (``my.cnf``) for ``read_default_file`` and
``read_default_group``.

Behaves as the MySQL client library does with those options (checked against
libmysqlclient through mysqlclient):

- With ``read_default_file`` the file is read; without it, but with
  ``read_default_group``, the default option files are (``/etc/my.cnf``,
  ``/etc/mysql/my.cnf``, ``$MYSQL_HOME/my.cnf`` and ``~/.my.cnf``; on
  Windows ``%WINDIR%\\my.ini``/``my.cnf`` and ``C:\\my.ini``/``my.cnf``).
  A missing file is skipped.
- Options come from ``[client]`` and from the requested group (group names
  ignore case). Where both set an option, the one later in the files wins,
  whichever group it is in.
- ``!include FILE`` reads another file (a relative path is relative to the
  current directory, as in the client library); ``!includedir DIR`` reads
  its ``*.cnf`` files (and ``*.ini`` on Windows) in name order. A missing
  include stops reading the file that names it, with a warning.
- ``#`` starts a comment anywhere outside quotes. A value wholly enclosed
  in ``'`` or ``"`` loses them (``"a" b`` is kept as written). The escapes
  ``\\b \\t \\n \\r \\s \\\\ \\' \\"`` apply, quoted or not; any other
  backslash is kept (so ``C:\\Program Files`` works). ``-`` and ``_`` are
  interchangeable in option names.

Options the driver does not use (option files are shared with the ``mysql``
command-line client and others) are ignored. ``.mylogin.cnf`` (encrypted,
MySQL only) is not read.
"""

from __future__ import annotations

import os
import sys
import warnings

from pathlib import Path


_ESCAPES = {
    "b": "\b",
    "t": "\t",
    "n": "\n",
    "r": "\r",
    "s": " ",
    "\\": "\\",
    "'": "'",
    '"': '"',
}
_MAX_INCLUDE_DEPTH = 10


def default_files() -> list[Path]:
    """The option files read when only ``read_default_group`` is given."""
    if sys.platform == "win32":
        windir = os.environ.get("WINDIR", r"C:\Windows")
        return [
            Path(windir, "my.ini"),
            Path(windir, "my.cnf"),
            Path(r"C:\my.ini"),
            Path(r"C:\my.cnf"),
        ]
    files = [Path("/etc/my.cnf"), Path("/etc/mysql/my.cnf")]
    mysql_home = os.environ.get("MYSQL_HOME")
    if mysql_home:
        files.append(Path(mysql_home, "my.cnf"))
    files.append(Path("~/.my.cnf").expanduser())
    return files


def read(path: str | os.PathLike[str] | None, group: str | None) -> dict[str, str]:
    """The options of ``[client]`` and ``group`` in ``path`` (or in the
    default files), as ``{normalized-name: value}``.
    """
    # ``~`` expands, as libmysqlclient does.
    files = [Path(path).expanduser()] if path is not None else default_files()
    groups = {"client"}
    if group:
        groups.add(group.lower())
    options: dict[str, str] = {}
    for file in files:
        _read_file(file, groups, options, depth=0)
    return options


def _read_file(
    path: Path, groups: set[str], options: dict[str, str], depth: int
) -> bool:
    """Read ``path`` into ``options``; ``False`` if it does not exist."""
    try:
        text = path.read_text(encoding="utf-8", errors="surrogateescape")
    except FileNotFoundError:
        return False
    current: str | None = None
    for number, raw in enumerate(text.splitlines(), start=1):
        line = raw.strip()
        if not line or line[0] in "#;":
            continue
        if line.startswith("!"):
            if not _directive(f"{path}:{number}", line, groups, options, depth):
                break
            continue
        if line.startswith("["):
            end = line.find("]")
            current = line[1:end].strip().lower() if end > 0 else None
            continue
        if current not in groups:
            continue
        name, sep, value = line.partition("=")
        options[name.strip().lower().replace("_", "-")] = _value(value) if sep else ""
    return True


def _directive(
    where: str, line: str, groups: set[str], options: dict[str, str], depth: int
) -> bool:
    """Handle ``!include``/``!includedir``; ``False`` stops reading the file."""
    keyword, _, argument = line.partition(" ")
    target = Path(argument.strip()).expanduser()
    if depth >= _MAX_INCLUDE_DEPTH:
        _warn(f"{where}: includes nested too deeply")
        return False
    if keyword == "!include":
        if not _read_file(target, groups, options, depth + 1):
            _warn(f"{where}: cannot read included file {target}")
            return False
    elif keyword == "!includedir":
        suffixes = {".cnf", ".ini"} if sys.platform == "win32" else {".cnf"}
        try:
            entries = sorted(target.iterdir())
        except OSError:
            _warn(f"{where}: cannot read included directory {target}")
            return False
        for entry in entries:
            if entry.suffix in suffixes and entry.is_file():
                _read_file(entry, groups, options, depth + 1)
    return True


def _value(text: str) -> str:
    """An option value: comment removed, enclosing quotes dropped, escapes
    applied.
    """
    text = _strip_comment(text).strip()
    if len(text) >= 2 and text[0] in {"'", '"'} and text[-1] == text[0]:
        text = text[1:-1]
    return _unescape(text)


def _strip_comment(text: str) -> str:
    """``text`` up to the first ``#`` outside quotes."""
    quote = ""
    index = 0
    while index < len(text):
        char = text[index]
        if char == "\\":
            index += 2
            continue
        if quote:
            if char == quote:
                quote = ""
        elif char in {"'", '"'}:
            quote = char
        elif char == "#":
            return text[:index]
        index += 1
    return text


def _unescape(text: str) -> str:
    if "\\" not in text:
        return text
    out: list[str] = []
    index = 0
    while index < len(text):
        char = text[index]
        following = text[index + 1 : index + 2]
        if char == "\\" and following in _ESCAPES:
            out.append(_ESCAPES[following])
            index += 2
        else:
            out.append(char)
            index += 1
    return "".join(out)


def _warn(message: str) -> None:
    warnings.warn(f"option file {message}", RuntimeWarning, stacklevel=4)


#: Option file names (normalized) → ``connect()`` arguments taking a string.
_STRING_OPTIONS = {
    "host": "host",
    "user": "user",
    "password": "password",
    "database": "database",
    "socket": "unix_socket",
    "default-character-set": "charset",
    "init-command": "init_command",
    "ssl-mode": "ssl_mode",
    "ssl-ca": "ssl_ca",
    "ssl-cert": "ssl_cert",
    "ssl-key": "ssl_key",
    "server-public-key-path": "server_public_key",
    "load-data-local-dir": "local_infile_dir",
}
#: Options that take a number.
_NUMBER_OPTIONS = {
    "port": "port",
    "connect-timeout": "connect_timeout",
    "zstd-compression-level": "zstd_level",
}
#: Flags (present, with no value or a true one) → ``connect()`` arguments.
_FLAG_OPTIONS = {
    "multi-statements": "multi_statements",
    "multi-queries": "multi_statements",
    "return-found-rows": "found_rows",
    "compress": "compress",
    "local-infile": "local_infile",
}
_FALSE = {"0", "false", "off", "no"}


def connect_args(options: dict[str, str]) -> dict[str, object]:
    """``options`` (from :func:`read`) as ``connect()`` arguments; options
    with no value and options the driver does not use are left out.
    """
    args: dict[str, object] = {}
    for name, value in options.items():
        if name in _STRING_OPTIONS and value:
            args[_STRING_OPTIONS[name]] = value
        elif name in _NUMBER_OPTIONS and value:
            try:
                args[_NUMBER_OPTIONS[name]] = int(value)
            except ValueError:
                msg = f"option file: {name} must be a number, got {value!r}"
                raise ValueError(msg) from None
        elif name in _FLAG_OPTIONS:
            args[_FLAG_OPTIONS[name]] = value.lower() not in _FALSE
        elif name == "compression-algorithms" and value:
            # A list the server picks from; zstd falls back to zlib here too.
            algorithms = {a.strip().lower() for a in value.split(",")}
            for algorithm in ("zstd", "zlib"):
                if algorithm in algorithms:
                    args["compress"] = algorithm
                    break
    return args
