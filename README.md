# rustmy

**A fast MySQL and MariaDB driver for Python, written in Rust.**
DB-API 2.0, a drop-in feel for code written for mysqlclient or PyMySQL,
SQLAlchemy dialects included, Django through mysqlclient's module name, and
wheels with no system libraries to install.

[![PyPI](https://img.shields.io/pypi/v/rustmy)](https://pypi.org/project/rustmy/)
[![Python versions](https://img.shields.io/pypi/pyversions/rustmy)](https://pypi.org/project/rustmy/)
[![Free-threaded](https://img.shields.io/badge/free--threaded-3.14t%20%7C%203.15t-blue)](https://docs.python.org/3/howto/free-threading-python.html)
[![CI](https://github.com/M4RC0Sx/rustmy/actions/workflows/ci.yml/badge.svg?branch=develop)](https://github.com/M4RC0Sx/rustmy/actions/workflows/ci.yml)
[![CodSpeed](https://img.shields.io/endpoint?url=https://codspeed.io/badge.json)](https://codspeed.io/M4RC0Sx/rustmy)
[![Downloads](https://static.pepy.tech/badge/rustmy/month)](https://pepy.tech/project/rustmy)
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue)](#license)

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="https://raw.githubusercontent.com/M4RC0Sx/rustmy/master/benches/results/hero-dark.svg">
  <img alt="rustmy against mysqlclient and PyMySQL: time to read 100,000 rows, insert 10,000 rows and read 100,000 rows through SQLAlchemy (lower is better; the figures are in the Performance table)" src="https://raw.githubusercontent.com/M4RC0Sx/rustmy/master/benches/results/hero-light.svg">
</picture>

```python
import rustmy

with rustmy.connect(
    host="localhost", user="app", password="secret", database="shop"
) as conn:
    with conn.cursor() as cursor:
        cursor.execute("SELECT id, name, price FROM products WHERE price < %s", (10,))
        for product_id, name, price in cursor:
            print(product_id, name, price)
```

## Why rustmy

- **Fast where it counts.** Reading rows is up to **5.3x faster than
  mysqlclient** and **13x faster than PyMySQL**; bulk inserts are 1.6x faster
  than both. Rows are read in small batches with the GIL released and
  converted in Rust while the server sends the next batch. `executemany` of
  `UPDATE`/`DELETE` pipelines its statements: one network round trip per
  256 of them instead of one each.
- **Nothing to install but the wheel.** No libmysqlclient, no compiler, no
  `pkg-config`: TLS is [rustls](https://github.com/rustls/rustls), and the
  supported authentication methods are implemented in Rust.
- **Familiar API.** PEP 249 with `%s` / `%(name)s` placeholders, `Cursor`,
  `DictCursor`, `SSCursor`, `conv=`, `rustmy.constants`: code written for
  MySQLdb or PyMySQL mostly just works.
- **SQLAlchemy ready.** `mysql+rustmy://` and `mariadb+rustmy://` pass
  SQLAlchemy's dialect compliance suite on every supported server, and its
  whole test suite (~25,000 tests) as well as PyMySQL does: the only extra
  failures are tests SQLAlchemy runs for unknown drivers and skips by name for
  the drivers it knows.
- **Runs Django.** `rustmy.install_as_MySQLdb()` puts rustmy behind Django's
  MySQL backend: Django's whole test suite (~19,700 tests) passes as it does
  with mysqlclient, on MySQL and MariaDB, but for one test that pins the
  types mysqlclient's `escape()` returns.
- **Built for free-threaded Python.** Wheels for 3.14t and 3.15t; all network
  I/O runs without the GIL.
- **Correct with every character set.** The driver follows the server's
  definition of each character set, not Python's, and refuses text the server
  would store as something else, including characters that Python's codecs
  turn into a backslash byte (an injection path in `sjis`/`ujis`).
- **Tested hard.** The official PEP 249 compliance suite, property-based tests
  (hypothesis) of escaping and injection against real servers, differential
  tests against PyMySQL, fuzzing of the protocol code, and the integration
  tests on nine MySQL and MariaDB versions in CI.

## Performance

CPython 3.15, MySQL 8.4 on the same machine, best of 5 to 10 runs, every
driver over plain TCP. Higher is better: how many times faster rustmy is.

| Workload | vs mysqlclient | vs PyMySQL |
|---|--:|--:|
| Fetch 100k rows: `DATETIME` + `DATE` | **5.30x** | **13.2x** |
| Fetch 100k rows: double, datetime, date, decimal | **2.78x** | 6.2x |
| Fetch 100k rows: integers and strings | 1.56x | 6.2x |
| Fetch 100k mixed rows as dicts (`DictCursor`) | 2.80x | 6.7x |
| Stream 1M rows (`SSCursor`) | 2.00x | 9.4x |
| Fetch 100k rows in `latin1` / `cp1251` | 1.9–2.0x | 3.3–3.6x |
| `executemany` 10k rows | 1.65x | 1.57x |
| SQLAlchemy Core: fetch 100k mixed rows | 2.25x | 4.7x |
| SQLAlchemy ORM: `add_all` + commit 10k objects | 1.32x | 1.21x |
| 10k tiny queries (`SELECT 1`) | 1.03x | 1.20x |

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="https://raw.githubusercontent.com/M4RC0Sx/rustmy/master/benches/results/current-spike-dark.svg">
  <img alt="Throughput relative to mysqlclient" src="https://raw.githubusercontent.com/M4RC0Sx/rustmy/master/benches/results/current-spike-light.svg">
</picture>

What it means for your application: the more rows you read or write, the
bigger the difference. When the time goes into the network round trip of many
tiny queries, or into building ORM objects one by one, every driver performs
about the same. Loading a million rows at once also takes 30% less memory
than with mysqlclient (214 MiB against 304 MiB). Every scenario, the scripts
and the history of each change are in
[benches/README.md](https://github.com/M4RC0Sx/rustmy/blob/master/benches/README.md).

## Installation

```sh
pip install rustmy
pip install "rustmy[sqlalchemy]"   # with SQLAlchemy 2.0+
```

Or with uv: `uv add rustmy`. Wheels cover Linux (glibc and musl, x86_64 and
aarch64), macOS (Intel and Apple Silicon) and Windows (x64 and arm64), for
CPython 3.10 to 3.15 and the free-threaded 3.14t and 3.15t (3.11 and later on
Windows arm64, where there is no official CPython 3.10). Every wheel is
installed and tested before release.

## Usage

### Connecting

```python
import rustmy

conn = rustmy.connect(
    host="localhost",
    port=3306,
    user="app",
    password="secret",
    database="shop",
    charset="utf8mb4",  # the default
    connect_timeout=5,
    read_timeout=30,
)
print(conn.get_server_info())
conn.close()
```

`unix_socket="/run/mysqld/mysqld.sock"` connects through a socket instead.
`autocommit` is off by default, as PEP 249 asks: call `conn.commit()` to keep
your changes, or pass `autocommit=True`.

Settings can also come from a MySQL option file, as with the `mysql` client:

```python
conn = rustmy.connect(read_default_file="~/.my.cnf", read_default_group="shop")
```

The `[client]` group and the one you name are read (with `!include` and
`!includedir`), and arguments passed to `connect()` win over the file.
`read_default_group` alone reads the default files (`/etc/my.cnf`,
`~/.my.cnf`…). Login paths (`.mylogin.cnf`) are not read.

### Queries and parameters

Parameters are escaped and interpolated in Rust, never by hand:

```python
with conn.cursor() as cursor:
    cursor.execute("SELECT * FROM products WHERE name = %s AND price < %s", ("tea", 5))
    cursor.execute("SELECT * FROM products WHERE name = %(name)s", {"name": "tea"})
    # A sequence becomes a list, for IN.
    cursor.execute("SELECT * FROM products WHERE id IN %s", ((1, 2, 3),))
    rows = cursor.fetchall()
```

Python types map to MySQL ones both ways: `int`, `float`, `Decimal`, `str`,
`bytes`, `bool`, `None`, `date`, `datetime`, `time`, `timedelta`, `uuid.UUID`
(as text) and JSON (as `str`).

### Many rows at once

`executemany` turns an `INSERT … VALUES` into multi-row statements, so 10,000
rows take a few round trips instead of 10,000:

```python
rows = [("tea", 3.5), ("coffee", 4.2), ("cocoa", 5.0)]
with conn.cursor() as cursor:
    cursor.executemany("INSERT INTO products (name, price) VALUES (%s, %s)", rows)
    print(cursor.rowcount, cursor.lastrowid)
conn.commit()
```

Any other statement (`UPDATE`, `DELETE`…) still runs once per item, but
pipelined: rustmy sends up to `cursor.max_pipeline` statements (256 by
default, 16 KiB at most) before reading their replies, so a batch pays one
network round trip per window instead of one per statement. With a 3 ms
round trip, 1,000 `UPDATE`s take under 0.1 s instead of 3.4 s:

```python
with conn.cursor() as cursor:
    cursor.executemany(
        "UPDATE products SET price = %(price)s WHERE name = %(name)s",
        [{"name": "tea", "price": 3.9}, {"name": "cocoa", "price": 5.5}],
    )
    print(cursor.rowcount)  # rows affected by all of them
conn.commit()
```

The server runs every statement it receives, so when one fails, the rest of
its window has already run by the time the error is raised (no further
window is sent). Inside a transaction (the default: autocommit is off),
`conn.rollback()` undoes them all. With autocommit on, set
`cursor.max_pipeline = 1` to stop right at the failing statement, as a loop
of `execute()` calls would.

### Rows as dictionaries

```python
from rustmy.cursors import DictCursor

with conn.cursor(DictCursor) as cursor:
    cursor.execute("SELECT id, name FROM products ORDER BY id LIMIT 2")
    print(cursor.fetchall())  # ({'id': 1, 'name': 'tea'}, {'id': 2, 'name': 'coffee'})
```

`rustmy.connect(..., cursorclass=DictCursor)` makes it the default.

### Large results without loading them

A streaming cursor reads rows as you iterate, in small batches, with flat
memory however large the result is:

```python
from rustmy.cursors import SSCursor

with conn.cursor(SSCursor) as cursor:
    cursor.execute("SELECT id, payload FROM events")
    for event_id, payload in cursor:
        handle(event_id, payload)
```

If another query runs on the connection before the stream is finished, the
rest is discarded with a warning, and the stream raises instead of silently
ending early. Ctrl+C interrupts a long read in any cursor.

### Errors

The PEP 249 exceptions are classed by the server's SQLSTATE, and carry the
MySQL error code:

```python
from rustmy.constants import ER

try:
    with conn.cursor() as cursor:
        cursor.execute("INSERT INTO products (id, name) VALUES (1, 'tea')")
except rustmy.IntegrityError as error:
    code, message = error.args
    if code == ER.DUP_ENTRY:
        print("already there:", message)
```

### TLS

TLS is on whenever the server offers it (`ssl_mode="preferred"`, the
default). To require it and check the server's certificate:

```python
conn = rustmy.connect(
    host="db.example.com",
    user="app",
    password="secret",
    ssl_mode="verify_identity",  # disabled, preferred, required, verify_ca
    ssl_ca="/etc/ssl/db-ca.pem",  # omit to trust the system's CAs
    ssl_cert="/etc/ssl/client.pem",  # client certificate, if required
    ssl_key="/etc/ssl/client-key.pem",
)
```

MySQLdb-style `ssl={"ca": ..., "cert": ..., "key": ...}` works too.
Authentication covers `caching_sha2_password`, `sha256_password` and
`mysql_native_password` (MySQL), and `client_ed25519` and `parsec` (MariaDB),
with or without TLS.

### Character sets

`utf8mb4` is the default and the right choice for new code. Legacy character
sets (`latin1`, `cp1251`, `gbk`, `big5`, `sjis`, `euckr`…) are supported for
existing databases, and decode in Rust:

```python
conn = rustmy.connect(..., charset="latin1", collation="latin1_spanish_ci")
conn.set_character_set("utf8mb4")  # switch later
```

### Custom conversions

`conv=` takes MySQLdb's format. Only the types you change leave the fast path:

```python
from rustmy.constants import FIELD_TYPE
from rustmy.converters import conversions

conv = {**conversions, FIELD_TYPE.NEWDECIMAL: float}  # DECIMAL as float
conn = rustmy.connect(..., conv=conv)
```

### SQLAlchemy

Install `rustmy[sqlalchemy]` and use the `mysql+rustmy` (or `mariadb+rustmy`)
URL; nothing needs importing:

```python
from sqlalchemy import create_engine, select
from sqlalchemy.orm import DeclarativeBase, Mapped, Session, mapped_column


class Base(DeclarativeBase):
    pass


class Product(Base):
    __tablename__ = "products"

    id: Mapped[int] = mapped_column(primary_key=True)
    name: Mapped[str]
    price: Mapped[float]


engine = create_engine(
    "mysql+rustmy://app:secret@localhost/shop?ssl_mode=verify_identity",
    pool_pre_ping=True,
)

with Session(engine) as session:
    cheap = session.scalars(select(Product).where(Product.price < 10)).all()
```

URL query arguments use rustmy's names (`charset`, `collation`, `ssl_mode`,
`ssl_ca`, `connect_timeout`…). The dialect does not send `SET NAMES` on every
new connection (the handshake already sets the character set), and
`stream_results=True` / `yield_per` use the streaming cursor.

### Django

Django's MySQL backend imports mysqlclient's module, `MySQLdb`. Make it load
rustmy at the top of `settings.py` (or anywhere that runs before Django
connects), and configure the database as usual:

```python
import rustmy

rustmy.install_as_MySQLdb()

DATABASES = {
    "default": {
        "ENGINE": "django.db.backends.mysql",
        "NAME": "shop",
        "USER": "app",
        "PASSWORD": "secret",
        "HOST": "localhost",
        "OPTIONS": {"charset": "utf8mb4"},
    }
}
```

`OPTIONS` are passed to `rustmy.connect()`, so `ssl_mode`, `ssl_ca`,
`read_default_file` and the rest work there. The alias reports itself as
mysqlclient 2.2.1, the oldest version Django 6.x accepts, and its
`connect()` leaves `bytes` without the `_binary` prefix, as mysqlclient does,
since Django writes `_binary %s` for binary fields itself. CI runs Django's
test suite with rustmy and with mysqlclient on the same servers and fails on
any test that fails with rustmy only.

### Threads

A connection belongs to one thread at a time (`threadsafety = 1`): give each
thread its own connection, or use a pool such as SQLAlchemy's. Using a busy
connection from another thread raises `ProgrammingError` instead of corrupting
it. On free-threaded Python nothing in rustmy serializes threads that use
their own connections.

## Compatibility

| | Supported and tested in CI |
|---|---|
| Python | CPython 3.10, 3.11, 3.12, 3.13, 3.14, 3.15, and free-threaded 3.14t, 3.15t |
| MySQL | 8.0, 8.4 LTS, 9.7, 26.x |
| MariaDB | 10.11, 11.4, 11.8, 12.3, 13.x |
| SQLAlchemy | 2.0 and 2.1 (CI tests the latest release each Python gets) |
| Django | 5.2 LTS and 6.1 (Django's test suite, weekly in CI) |
| Platforms | Linux x86_64 and aarch64 (glibc, musl), macOS x86_64 and arm64, Windows x64 and arm64 |

### Coming from mysqlclient or PyMySQL

Most code runs unchanged. The differences are deliberate, and each one is
pinned by a test:

- Invalid dates such as `0000-00-00` come back as `None` (as mysqlclient;
  PyMySQL returns the text).
- `rowcount` is `-1` while a streamed result is unread (as mysqlclient).
- `lastrowid` is `None` after a statement that returns rows (PEP 249).
- Closing a connection twice raises `InterfaceError` (PEP 249).
- Text a connection's character set cannot represent raises `DataError`
  before anything is sent, instead of `UnicodeEncodeError` or a corrupted
  value.
- Server errors are classed by SQLSTATE, so for example "unknown column"
  (1054) is a `ProgrammingError`, as the SQL standard defines it. One
  exception keeps code written for mysqlclient working: a numeric value out
  of range (1690) raises `rustmy.err.OutOfRangeError`, both a `DataError`
  (its SQLSTATE) and an `OperationalError` (as mysqlclient raises it).
- `bytes` parameters go as `_binary'…'`, so the server reads them as binary
  whatever the connection's character set; `binary_prefix=False` writes the
  plain `'…'` of mysqlclient and PyMySQL.
- `multi_statements` is off unless you ask for it, which keeps an injection
  from adding statements of its own.
- `executemany` of anything but `INSERT … VALUES` sends its statements in
  windows of up to 256 before reading the replies, so after an error the rest
  of the failing statement's window has already run (see
  [Many rows at once](#many-rows-at-once); `cursor.max_pipeline = 1` sends
  them one by one, as both drivers do).

## Limitations

- **New.** rustmy is tested extensively but has no long production history
  yet. Please report anything that behaves differently from what you expect.
- **Synchronous only.** No asyncio API yet.
- **Text protocol.** Server-side prepared statements, compression and
  `LOAD DATA LOCAL INFILE` are not supported yet.
- **Latency-bound workloads** (many tiny queries, one after another) gain
  little: the network dominates them with any driver. `executemany` is the
  exception: it pipelines its statements.
- **CPython 3.10 installed by uv on Alpine/musl** cannot load the `cp310`
  musllinux wheel (nor any other project's): CPython 3.10 names musl extensions
  `*-linux-gnu.so`, which uv's build of it does not accept. Alpine's own Python
  3.10, and every later version from any source, work.

## How it is built and tested

rustmy is a Rust workspace with a pure-Rust protocol core (`rustmy-proto`:
packets, handshake, authentication, TLS, row decoding; no `unsafe`) and thin
[PyO3](https://pyo3.rs) bindings that build Python objects straight from the
wire. Every pull request runs:

- the PEP 249 compliance suite (`dbapi20.py`), SQLAlchemy's dialect suite and
  part of Django's test suite (all of it weekly), the latter compared test by
  test with mysqlclient;
- property-based tests with [hypothesis](https://hypothesis.readthedocs.io)
  against real servers: arbitrary text and bytes must round-trip byte for
  byte, and generated injection attempts must stay values, with and without
  `NO_BACKSLASH_ESCAPES`;
- every character set of every supported server, character by character;
- differential tests against [PyMySQL](https://github.com/PyMySQL/PyMySQL);
- integration tests on all nine supported server versions, and every
  supported Python against MySQL and MariaDB;
- performance tracking with [CodSpeed](https://codspeed.io/M4RC0Sx/rustmy);
- `cargo clippy` with pedantic lints and no panics, `cargo deny` for
  licenses and advisories; the protocol parsers are also fuzzed.

## Built with

- [PyO3](https://github.com/PyO3/pyo3) and [maturin](https://github.com/PyO3/maturin): Rust bindings and packaging for Python.
- [rustls](https://github.com/rustls/rustls), [ring](https://github.com/briansmith/ring) and [rustls-platform-verifier](https://github.com/rustls/rustls-platform-verifier): TLS without OpenSSL.
- [RustCrypto](https://github.com/RustCrypto) (`rsa`, `sha1`, `sha2`, `pbkdf2`) and [ed25519-dalek](https://github.com/dalek-cryptography/curve25519-dalek): the authentication methods.
- [SQLAlchemy](https://www.sqlalchemy.org): the dialects build on its MySQL dialect.
- [uv](https://github.com/astral-sh/uv), [Ruff](https://github.com/astral-sh/ruff), [mypy](https://mypy-lang.org), [pytest](https://pytest.org), [hypothesis](https://hypothesis.readthedocs.io), [cargo-deny](https://github.com/EmbarkStudios/cargo-deny) and [cargo-fuzz](https://github.com/rust-fuzz/cargo-fuzz): development and testing.
- Stuart Bishop's [DB-API 2.0 compliance suite](https://github.com/baztian/dbapi-compliance) (public domain).

The licenses of the Rust crates compiled into each wheel ship with it in
`THIRD-PARTY-LICENSES`.

## Contributing

Issues and pull requests are welcome. Development needs Rust (via
[rustup](https://rustup.rs)), [uv](https://docs.astral.sh/uv/) and Docker for
the test servers:

```sh
uv sync                         # builds the extension
docker compose up -d --wait     # MySQL and MariaDB test servers
uv run pytest                   # Python tests
cargo nextest run --workspace --exclude rustmy-pydriver
```

Pull request titles follow [Conventional Commits](https://www.conventionalcommits.org);
they become the changelog.

## License

Licensed under either of [Apache License, Version 2.0](https://github.com/M4RC0Sx/rustmy/blob/master/LICENSE-APACHE)
or [MIT license](https://github.com/M4RC0Sx/rustmy/blob/master/LICENSE-MIT), at your option.

"MySQL" is a trademark of Oracle Corporation and "MariaDB" is a trademark of
MariaDB plc; rustmy is not affiliated with or endorsed by either.
