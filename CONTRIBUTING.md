# Contributing to rustmy

Thanks for helping. This guide covers the rules every change follows, how to
build and test the project, and the design decisions behind the code, so a
change fits with what is already there. For a bug or a feature, an issue first
is welcome (the templates ask for what is needed); vulnerabilities go privately,
as [SECURITY.md](SECURITY.md) explains.

`rustmy` is a **synchronous** MySQL/MariaDB driver for Python written in Rust
(PyO3 + maturin): DB-API 2.0 with `paramstyle = "format"` (`%s`,
`%(name)s`), its own SQLAlchemy dialect, a Django backend through
`install_as_MySQLdb()`, and wheels with no system dependencies, licensed
`MIT OR Apache-2.0`.

## Ground rules

- **English only**: everything committed is in English: code, comments,
  configuration, commit messages and documentation.
- **License hygiene**: never copy code, tests or documentation from
  mysqlclient or mysql-connector-python (both GPL). They may be read to learn
  how they *behave* and used in benchmarks and differential tests, never
  redistributed, and rustmy must not end up looking like them (structure,
  naming). The one exception is `tests/dbapi20.py` (public domain, header
  kept). Every new Rust dependency must pass `cargo deny check` (allow-list in
  `deny.toml`).
- **Performance**: no performance change without a benchmark before and after
  (`benches/`, ratio against mysqlclient in the same run).
- **GIL**: all network I/O (connect, query, read, ping, commit, close) runs
  with the GIL released. The module declares `gil_used = false`
  (free-threaded), so every exposed type must be safe without the GIL.
- **Tests first for bugs**: a fuzzing crash, a hypothesis failure or a bug
  report becomes a fixed test before the fix.

## Workflow

- Work happens on `develop`. Branches are `feature/<topic>`, cut from
  `develop` and merged back by squash pull request. The pull request title is
  a [Conventional Commit](https://www.conventionalcommits.org) (`feat:`,
  `fix:`, `perf:`, `docs:`, `ci:`, `chore:`...): it becomes the changelog
  entry and decides the next version (`pr-title.yml` checks it).
- `master` is release-only and the default branch: it receives `develop`
  through merge-commit pull requests (never squash) plus the release bot's
  commits, and is never edited directly. CI runs on pushes to `develop` and on
  every pull request.
- Versioning: on `develop` the version stays a placeholder (`0.0.0`).
  semantic-release (`release.config.mjs`, `.github/workflows/release.yml`)
  writes `CHANGELOG.md` and the real version into `[workspace.package]` of
  `Cargo.toml` and into `Cargo.lock` on `master` only; `pyproject.toml`
  declares the version dynamic and maturin copies Cargo's into the wheel.
  `develop` never receives those commits back.
- Releases: merging `develop` into `master` plans the version, builds and
  smoke-tests every wheel (8 platforms, every supported CPython, built with
  the maturin of `uv.lock`, `scripts/locked_version.py`) and the sdist,
  attests them, verifies the attestations, tags, and publishes to PyPI through
  trusted publishing after the maintainer approves the `pypi` environment. A
  pull request into `master` rehearses all of it without publishing.
- Every action in the workflows is pinned to a commit SHA with its version in
  a comment; Dependabot keeps them current. A Dependabot pull request that
  changes Rust crates also needs `THIRD-PARTY-LICENSES` regenerated (command
  in `about.toml`), which CI checks.

## Setting up

Python goes through [uv](https://docs.astral.sh/uv/); `uv sync` builds the
extension with maturin and rebuilds it when `.rs`/`Cargo.toml` files change
(`[tool.uv] cache-keys`). Rust comes from rustup (`rust-toolchain.toml` picks
stable with rustfmt and clippy); the Rust tests use
[cargo-nextest](https://nexte.st).

```sh
uv sync                                   # dev group; add --group bench for the benchmark drivers
uv run maturin develop --uv --release     # release build of the extension, for benchmarks
uv run ruff check && uv run ruff format --check
uv run mypy                               # strict; files/mypy_path set in pyproject
uv run pytest                             # everything
uv run pytest -m "not integration"        # no server needed (integration tests also skip when servers are down)
uv run pytest tests/test_module.py::test_pep249_globals
```

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --workspace --exclude rustmy-pydriver   # pydriver is a cdylib: no native test binary
cargo nextest run -p rustmy-proto <test_name>
cargo deny check
```

The dev environment uses Python 3.10 (`.python-version`, the support floor).
Other versions get their own environment:
`UV_PROJECT_ENVIRONMENT=.venv-3.14t uv run --python 3.14t pytest`. A plain
`uv run --python 3.14t …` recreates `.venv` for that version; `uv sync`
restores it.

maturin's editable installs drop the compiled module into `python/rustmy/` as
`_core.<python-tag>.so`, one file per Python version built. A file for another
version is not rebuilt until that version syncs again, so with `--no-sync` it
can be stale; `rm python/rustmy/_core.*.so && uv sync` resets it.

## Test servers

Integration tests need the servers in `compose.yaml` (MySQL on 13306, MariaDB
on 13307, data in tmpfs). Supported servers are every vendor-supported line,
currently MySQL 8.4, 9.7, 26.x and MariaDB 10.11, 11.4, 11.8, 12.3, 13.x, plus
MySQL 8.0 (upstream end of life, still widely deployed); CI tests each one.
The defaults are MySQL 8.4 and MariaDB 11.4:

```sh
docker compose up -d --wait
cargo nextest run -p rustmy-proto --run-ignored only      # crates/proto/tests/live.rs
uv run pytest -m integration                               # Python tests against the servers
RUSTMY_TEST_COMPRESS=zstd uv run pytest -m integration     # the same over the compressed protocol (also for live.rs)
MYSQL_VERSION=9.7 docker compose up -d --wait mysql        # another version
RUSTMY_TEST_SERVERS=mysql cargo nextest run -p rustmy-proto --run-ignored only
docker compose down
```

For changes that touch the protocol, run every integration test against all
nine servers (CI's `rust-integration` matrix does the same):

```sh
for entry in mysql:8.0 mysql:8.4 mysql:9.7 mysql:26.10 mariadb:10.11 mariadb:11.4 mariadb:11.8 mariadb:12.3 mariadb:13.0; do
  svc=${entry%%:*}; ver=${entry#*:}
  docker compose down -v >/dev/null 2>&1
  export MYSQL_VERSION=$ver MARIADB_VERSION=$ver RUSTMY_TEST_SERVERS=$svc
  docker compose up -d --wait "$svc" >/dev/null 2>&1 || { echo "$entry: container failed"; continue; }
  echo "$entry: rust [$(cargo nextest run -p rustmy-proto --run-ignored only 2>&1 | grep Summary)] python [$(uv run pytest -m integration -q 2>&1 | tail -1)]"
done; docker compose down -v
```

- **TLS**: the `certs` compose service generates throwaway certificates into
  `tests/docker/certs/` (git-ignored; delete the directory to regenerate) and
  both servers use them. Tests refer to `ca.pem`, `other-ca.pem` (an untrusted
  CA) and `client.pem`/`client-key.pem` (for the `REQUIRE X509` account
  `rustmy_x509`). The default `ssl_mode` is `preferred`, so ordinary tests run
  over TLS; plain text is covered by explicit `ssl_mode=disabled` tests.
  Benchmarks force `ssl_mode="disabled"` so every driver runs over plain TCP.
- **Authentication**: tests that must hit the full-authentication path of
  `caching_sha2_password` create a fresh account as `root` (password
  `rustmy`), since a new account cannot be in the server's cache. MariaDB's
  `rustmy_ed25519` (`client_ed25519`) and `rustmy_parsec` (`parsec`, 11.6+)
  come from `tests/docker/mariadb-init.sh`; the `mysql_native_password`
  account `rustmy_native` (`tests/docker/mysql-init.sh`, MySQL 8.x only)
  covers the auth-switch path.
- **`LOAD DATA LOCAL INFILE`**: compose starts both servers with
  `--local-infile=ON` (MySQL 8.0+ defaults to off).

## Other suites

- **SQLAlchemy**: `tests/sqla_suite/` is SQLAlchemy's dialect compliance
  suite as a separate pytest session, in a database of its own (`sqla_test`,
  plus `test_schema`/`test_schema_2`, from `tests/docker/*-init.sh`).
  `requirements.py` states what the servers support by capability, never by
  driver name; `test_suite.py` replaces the two suite classes that would fail
  for reasons outside rustmy. `scripts/sqlalchemy_full_suite.py` runs
  SQLAlchemy's whole suite against PyMySQL's results (about 20 minutes).

  ```sh
  uv run pytest -c tests/sqla_suite/pytest.ini tests/sqla_suite --db mysql --db mariadb
  uv run pytest -c tests/sqla_suite/pytest.ini tests/sqla_suite --db mysql_prepared --db mariadb_prepared
  uv run python scripts/sqlalchemy_full_suite.py --server mysql
  ```

- **Django**: `scripts/django_suite.py` runs Django's test suite through
  rustmy and through mysqlclient on the same server, and fails on any test
  that fails with rustmy only, except the `KNOWN` entries (each with its
  reason). It builds mysqlclient (see `PKG_CONFIG_PATH` in
  `benches/README.md`) into a per-version virtualenv under
  `.cache/django/<tag>/` with a Django clone, and rebuilds rustmy from the
  checkout on every run. CI (`django.yml`) runs a subset with Django 6.1 on
  every pull request, and the whole suite weekly with Django 5.2 (Python 3.14)
  and 6.1 (Python 3.15).

  ```sh
  uv run python scripts/django_suite.py --django 6.1.3 --python 3.15 --server mysql
  uv run python scripts/django_suite.py --django 6.1.3 --python 3.15 --server mysql backends queries
  ```

- **Conformance and properties**: `tests/dbapi20.py` is the PEP 249 suite
  kept byte for byte from `baztian/dbapi-compliance@ea7cb1b4` (sha256 checked
  by `tests/test_dbapi20.py`; excluded from ruff and mypy).
  `tests/test_escaping.py` holds hypothesis properties against the servers,
  with and without `NO_BACKSLASH_ESCAPES` (profiles in `tests/conftest.py`;
  `--hypothesis-profile thorough` runs 5000 examples).
  `tests/test_charsets.py` covers every character set of every server
  (`CATALOGUE`; a new one fails the guard test). `tests/test_differential.py`
  runs the same cases through rustmy and PyMySQL; every decided difference is
  in `INTENDED_DIFFERENCES` with both sides' values, never silenced.
- **Fuzzing** (nightly and cargo-fuzz; details in `fuzz/README.md`):
  `cargo +nightly fuzz run session fuzz/corpus/session fuzz/seeds/session -- -max_total_time=300`.
  Targets call `rustmy_proto::fuzzing::*`, compiled only with the `fuzzing`
  feature (a scripted transport and a low parsec iteration cap); CI lints that
  feature and fuzzes weekly. `differential` runs the same session through
  `proto` and through the 1.3.2 release (a git dependency on its tag, in
  `fuzz/` only) and fails on any difference in results or errors.
- **Coverage**: `coverage.yml` uploads Python (`coverage.xml`) and Rust
  (`cargo llvm-cov` over `proto`'s tests and the instrumented extension under
  pytest) to Codecov with flags `python` and `rust`.
- **Memory leaks**: `uv run --no-sync python scripts/leak_check.py` runs every
  API path in a loop. RSS growth alone is usually the allocator: confirm with
  `heap <pid>`/`leaks <pid>` on macOS or valgrind on Linux.
- **Benchmarks**: see [benches/README.md](benches/README.md) (needs
  `uv sync --group bench`, a release build and the MySQL container).
  `report.py` and `showcase.py --render` turn the JSON results into the
  light/dark SVG charts and rewrite the README tables between their markers:
  never edit those blocks by hand. The server keeps flushing large setup
  INSERTs for a while, so benchmarks that create big tables pause before
  measuring, or whichever driver runs first looks slower. Wheels are per
  Python version, not abi3: abi3 lost the advantage in date conversion.

## Architecture

- `crates/proto` (`rustmy-proto`): pure Rust, **no PyO3**: packets,
  handshake, authentication, TLS, compression, row decoding. Tested, fuzzed
  and benchmarked without an interpreter.
- `crates/pydriver` (`rustmy-pydriver`, lib `_core`, cdylib): the PyO3
  bindings exposed as `rustmy._core`. Holds the hot path that turns rows into
  Python objects.
- `python/rustmy/`: the Python package (maturin `python-source`): the public
  DB-API surface, constants, the SQLAlchemy dialect and the `MySQLdb` alias.
  `connect()`'s arguments are resolved once for both connections, blocking
  and asyncio (`_connect_args.py`: aliases, option files, TLS, defaults, and
  the statements that apply the session settings).
  `_core.pyi` must stay in sync with what `crates/pydriver` exports.
- The version has a single source, `[workspace.package]` in `Cargo.toml`.

### Design notes

**Hot path.** Text protocol with interpolation and escaping in Rust. Results
are read in ~4 KiB batches with the GIL released and each batch is converted
in a tight loop while the server sends the next (network and conversion
overlap; reading everything first was ~1.4x slower), with no intermediate row
or value types. `date`/`datetime`/`timedelta` are built directly; `Decimal`
and JSON go through a cached Python class; dict rows are built in Rust with
the keys created once per result (`convert::Converter`). Tuple rows whose
values cannot form reference cycles (numbers, text, bytes, dates, `None`,
`Decimal`) are taken out of the cyclic garbage collector as they are built
(`convert::untrack`), so collections during a large read do not traverse
every row read so far. Tuples holding a container (a `conv=` decoder's
list) stay tracked, and so do dict rows: they are mutable, and since Python
3.14 an untracked dict does not track itself again when a container is
stored in it.
Parameters are told apart by exact type first (`Args::from_py`): asking
whether any object is a `Mapping` runs Python code. A connection keeps no
buffer a large packet grew past 1 MiB once the operation ends (`release` in
`packet.rs`, compressed buffers included), and uncompressed payloads that
large are written from the caller's buffer, packet by packet, without
staging.

**Buffered cursors** (`Connection::buffered` with a `Session`,
`crates/pydriver/src/connection.rs`) keep the connection locked for the whole
statement and release only the GIL per batch, so concurrent use still fails
fast; signals are checked between batches (Ctrl+C interrupts a long read, and
the next command discards the rest).

**Streaming** (`SSCursor`, `crates/pydriver/src/stream.rs`): the pending
result set is protocol state of the connection (`Session` in `session.rs`,
which reads a reply's head, its rows, a `COM_STMT_PREPARE` reply and a
pipelined statement's whole reply without I/O; `Conn::query_streamed`,
`read_rows` and `skip_rows` drive it), so any command discards unread rows
first. The connection lock is held only
while a batch is read, never while Python runs. Every command bumps the
connection's `generation`; a `ResultStream` compares its own on every fetch,
so a cursor whose rows were discarded raises instead of returning a truncated
result (and the discarding command warns, pointing at the caller).

**Transport.** `connect_timeout` (15 s unless given, set in
`connections.py`) bounds opening the socket and every read and write until
authentication ends; `read_timeout`/`write_timeout` replace it then
(`Conn::connect`; `crates/proto/tests/timeouts.rs` plays a server that never
answers). TCP sockets get `TCP_NODELAY` and keepalive through `socket2`
(probes after 60 s idle, every 10 s where the platform allows setting it).
Packet framing does no I/O of its own (`packet::Framer`): bytes from the
server go into an `Input` buffer in whatever pieces they arrive, compressed
packets included, and whole payloads come out; `packet::Wire` reads and
writes them over a `Transport` (`transport.rs`) and reads a large body
straight into the caller's buffer where the transport can. Every command
from the handshake's end on is an `async fn` of `client::Client` over a
`Transport`, written once; `Conn` runs them over its blocking stream with
`block_on`, a single poll with a no-op waker (a blocking transport never
leaves one pending: no runtime, no thread), and an event loop can run the
same ones with a transport of its own. Rows the input buffer holds are
taken without any I/O (`Wire::try_read_into`); only a refill waits. A row that the buffer holds whole takes a fast path. The `packets`
and `compressed` fuzz targets read every input whole and in pieces, and the
results must match.

**asyncio** (`rustmy.aio`, `python/rustmy/aio/`, `crates/pydriver/src/aio.rs`): the
same `Client` operations run on the event loop's thread, with no runtime and
no other thread. `AioConnection` is the core of an `asyncio.BufferedProtocol`
whose callbacks are bound to its methods (no Python frame per read):
`buffer_updated` hands the bytes to its transport (`LoopIo`, whose refill is
pending until the loop has something) and polls the running operation once
more; what it writes goes to the loop's transport after each poll, and its
result resolves the command's future. Rows are converted every 64 KiB as
they arrive. A command whose task is cancelled closes the connection (the
reply would arrive in the middle of the next command's); an I/O failure or a
protocol violation leaves it closed too. It takes `connect()`'s options
(`connect_opts` in `connection.rs`, shared with the blocking connection).
What the loop's callbacks need lives in `Core`. `read_timeout` and
`write_timeout` are a single `call_later` check, not a timer per read: it
fails a command (2013, the connection closed) that has waited that long for
the server since its last progress (bytes received, a piece of a local
file read), using `write_timeout` while the loop's transport holds unsent
bytes, and otherwise schedules itself again; idle time between commands
does not count. A local file (`LOAD DATA LOCAL INFILE`) is read in the
loop's executor, 256 KiB per trip (`Transport::read_file`), so the loop
never waits on the disk.
TLS runs in memory over any transport (`transport::MaybeTls`, `rustls`'s
`ClientConnection`, with the blocking connection's configuration and
cache): `Client::connect` starts it right after the `SSLRequest`, as the
blocking handshake does. Compression is the same layer as everywhere.
The Python layer mirrors the blocking one with `await` (the shape of aiomysql's
and asyncmy's API: `execute`, the `fetch*` methods, `nextset`, `callproc`,
`commit`... are coroutines): `connect()` resolves its arguments with
`_connect_args`, applies the session settings with the same plan, and the
buffered and prepared cursors keep the blocking cursors' behaviour (PEP 249
errors included). Unbuffered cursors (`AioStream`) apply backpressure: `take`
hands out the rows already converted without I/O (no future is created
while rows are at hand), and `read` reads one more batch off the loop only
when more are asked for; the command generation, the discard warning and
the errors follow the blocking `ResultStream`. `executemany` groups its
items with the blocking connection's builders (`batch.rs`) and runs each
group as a command of its own (`AioMany.step`): one `await` per statement
or window, not per item.

**TLS and authentication.** TLS through `rustls`; `ssl_mode` is disabled /
preferred (default) / required / verify_ca / verify_identity. With `ssl_ca`,
verification uses only that file (as PyMySQL does); verify modes without it
use the operating system's trust store (`rustls-platform-verifier`). `ssl={...}`
dicts are accepted for compatibility (`ssl={}` means `required`). The rustls
configuration is built once per process for each set of TLS options and kept
while its files are unchanged (`client_config` in `tls.rs` compares size,
times and inode), so connections skip rereading them and resume TLS
sessions.
`caching_sha2_password` and `sha256_password` do full authentication in
cleartext over TLS or Unix sockets, and with RSA-OAEP over plain TCP: the key
is requested from the server when needed, as PyMySQL does, or pinned with
`server_public_key`; `get_server_public_key=False` never requests it and
fails with `Error::Auth` (2061) instead, as libmysqlclient does by default. A key is accepted only right after the client asked for
it, never with a pinned one (`requests_key` in `login.rs`;
`crates/proto/tests/auth.rs` plays the man in the middle). MariaDB's `parsec`
derives its key with as many PBKDF2 rounds as the server asks for, bounded
as MariaDB Connector/C bounds them: what fits in `connect_timeout`
(`parsec_max_factor`). A logical payload may be 64 KiB until authentication
succeeds and 1 GiB after, checked from each header before reading
(`packet.rs`). `Opts`'s `Debug` output hides the password. Like framing,
the handshake does no I/O of its own (`login.rs`): `Login` turns the
greeting into the session's parameters and replies, and `Auth` runs the
authentication exchange one payload at a time, for `COM_CHANGE_USER` too;
`Conn` moves the payloads, runs the TLS handshake and reads the pinned key
file (after TLS, before the first response).

**Character sets** (`crates/pydriver/src/charset.rs`, ids in
`crates/proto/src/charset.rs`). The handshake requests the server's own
default collation when the connection's character set is the server's, else
the character set's default, which leaves the session as `SET NAMES` would on
all nine servers (`test_the_handshake_sets_the_character_set`). The
exception is a character set some server lacks: it silently ignores a
collation id it does not know (MariaDB has no gb18030), so `SET NAMES`
follows (`_HANDSHAKE_MAY_IGNORE` in `connections.py`). `collation=` and
`set_character_set()` use `SET NAMES`, reapplied after `reset_connection` and
`change_user`. The session settings of `connect()` (`SET NAMES`, `sql_mode`,
`autocommit`) go in a single `SET`, before `init_command` (which then
precedes `autocommit`). Queries are built in the connection's character set, escaping
text before encoding it; `bytes` go as `X'…'` in multi-byte legacy character
sets. User and database names go in the connection's character set, the
password always as UTF-8. **The server's definition of each character set wins
over Python's codec** (`Fixes` in `charset.rs`, found by probing every
character on all nine servers and checked exhaustively by
`tests/test_charsets.py`): single-byte tables are patched byte by byte,
`euckr` uses Python's `cp949` (the server's is UHC), gb18030 swaps two code
points, and characters the server cannot represent are refused. Multi-byte
text must decode back to itself after encoding or it is refused: Python
encodes `¥` as 0x5C in shift_jis/euc_jp, which the server reads as a
backslash. Multi-byte decoding calls `PyUnicode_Decode`, the crate's only
`unsafe`. Column names and server messages are raw bytes in `proto`, decoded
with the connection's character set in `pydriver`.

**`conv=`** (MySQLdb format): `rustmy.converters.split_conv` keeps only the
entries that differ from `rustmy.converters.conversions`, so unchanged types
stay native. Custom decoders become `Kind::Custom` columns (resolved once per
result) and get `str` for DECIMAL/date/time columns, `bytes` otherwise;
custom encoders are tried first in `Encoder::literal`.

**Constants and errors.** Constants live in `python/rustmy/constants/`
(uppercase module names, as in other drivers; SQLAlchemy imports
`rustmy.constants.CLIENT`). `ER.py` is generated by `scripts/generate_er.py`
from the error catalogues of every supported server: never edit it by hand,
and regenerate it when the supported versions change (an integration test
compares it with each MySQL server's catalogue). `multi_statements` is off by
default.

**Pipelined `executemany`.** How the items are grouped is in `batch.rs`,
without I/O, for both connections. A batchable `INSERT … VALUES` becomes
multi-row statements; anything else is sent in windows of up to `Cursor.max_pipeline`
statements (256) and `Conn::pipeline_budget()` bytes (16 KiB over TCP/TLS,
8 KiB over Unix sockets, so a window always fits in the send buffer: the
server may stop reading while it sends a large reply, a deadlock reproduced
beyond ~1 MiB). `Conn::pipeline` writes every `COM_QUERY` in one write
(`PacketStream::write_commands` records the sequence id each reply starts at)
and reads every reply even after a server error. The connection is locked per
window, so a parameter generator may query the same connection between
windows; after an error, at most the rest of its window has run
(`max_pipeline = 1` restores one by one). `LOAD DATA LOCAL INFILE` statements
stay out of the pipeline.

**Prepared statements** (`PreparedCursor`/`PreparedDictCursor`,
`cursor(prepared=True)`, SQLAlchemy `?cursorclass=PreparedCursor`). `proto`
has `Conn::prepare`/`execute`/`close_statement` and a per-connection LRU
(`connect(statement_cache_size=32)`, cleared by `reset_connection` and
`change_user`; re-prepares once on error 1615, and on 1461, the server's
`max_prepared_stmt_count`, closes its least recently used statement and
prepares once more); `binary.rs` holds the typed
parameters and the binary value decoders. The binary path of `Converter` must
give **the same objects as text** (`tests/test_prepared.py` checks it with
hypothesis per column type; `FLOAT` is rounded to 6 significant digits, as the
server's text does). Only statements **with placeholders** are prepared;
others go as text, so DDL and introspection behave as libraries expect. A
statement the server cannot prepare raises (1295, no silent fallback); a
`conv=` encoder or a sequence parameter raises `NotSupportedError`.
`executemany` turns `INSERT … VALUES` into multi-row prepared statements with
a fixed row count and pipelines the rest. The last batch is a statement of
its own row count: splitting it into powers of two prepares far fewer
statements across calls but costs a round trip per piece, and measured
slower in every case (1.3x locally, up to 2.9x with 1 ms of latency), so it
stays one statement. Not for Django's default cursor,
whose SQL uses `->> %s`, `_binary %s` and parameters in DDL.

**Compression** (`compress=True|"zlib"|"zstd"`, `zstd_level=3`;
`conn.compression` says what was agreed). `proto/src/compress.rs` is a
byte-stream layer under `PacketStream`, switched on after the authentication
OK: zlib through `flate2` with the `zlib-rs` backend, zstd through the `zstd`
crate (vendored libzstd, the only C dependency), payloads under 50 bytes or
that do not shrink stored as is, decompression bounded by the declared length.
**Only the compressed sequence is checked**: servers copy it into the ordinary
one on every flush, so the second result of a multi-statement query restarts
the ordinary sequence. Pipelined commands each go in compressed packets of
their own, and every reply starts at compressed sequence 1 (`ReplyStart` in
`packet.rs`). `RUSTMY_TEST_COMPRESS=zlib|zstd` runs `live.rs` and the Python
suite compressed (CI does it in every server job).

**`LOAD DATA LOCAL INFILE`** (`infile.rs`: the permission, the checks and
the outcome; the connection sends the file): off unless
`connect(local_infile_dir=…)`, and
only then is `CLIENT_LOCAL_FILES` requested. Every command records which file
names it may be asked for: a `COM_QUERY` the names its `LOAD DATA|XML …
LOCAL INFILE '…'` statements give (`sql::local_infile_names`, unescaped as
the server reads them, honouring `NO_BACKSLASH_ESCAPES`), anything else none;
inside a pipelined batch a request closes the connection, since the server
would read the next statements as the file. A request is answered only if the
statement named that exact name and the canonicalized path is a regular file
inside the directory; otherwise an empty file goes and `OperationalError` 2068
is raised, the connection still in sync, with nothing on disk touched unless
the statement named the file. The file goes in 16 KiB packets with the GIL
released (read by `Transport::read_file`, here where the connection runs); a read error halfway closes the connection, so the server aborts
instead of loading part of it. `crates/proto/tests/local_infile.rs` tests it
against a fake, malicious server.

**Django**: `rustmy.install_as_MySQLdb()` (`python/rustmy/_mysqldb.py`)
registers a `MySQLdb` alias whose submodules are the very same rustmy module
objects, reporting mysqlclient 2.2.1 (Django 6 refuses older). The alias's
`connect()` defaults to `binary_prefix=False` like mysqlclient, because Django
writes `_binary %s` for binary fields. `Connection.escape()` and `.encoders`
serve Django's schema editor and always return `str`. Error 1690 raises
`rustmy.err.OutOfRangeError(DataError, OperationalError)`, which Django turns
into `IntegrityError`. Option files (`read_default_file`/`read_default_group`,
`python/rustmy/_option_files.py`) follow `my_print_defaults`: explicit
arguments win, `[client]` and the group in file order, `!include`/
`!includedir`, no `.mylogin.cnf`, and files anyone can write to are ignored
with a warning (outside Windows), as `my_print_defaults` does. Fetch methods
return tuples, `()` when exhausted.

**Pools** (`rustmy.Pool`; `python/rustmy/pool.py`): the accounts are
`rustmy_proto::pool::PoolCore`, without I/O and with the time given by the
caller: it counts connections by id (lent, idle, being opened, so the
maximum holds while they open), lends idle ones most recently used first,
serves waiters in order, and answers each call with what to do (connections
to close, waiters to serve, how many to open for the minimum). Its tests run
random sequences of calls against a model of the caller and check the
accounts after every step. The facade holds the connections, does the
waiting (`threading.Condition`, with the GIL released) and the I/O outside
its lock: connections for the minimum open on threads of their own, so
giving one back never waits for a new one. A connection given back with a
transaction open (the status flag the server already sent) is rolled back;
one that is closed or fails is not lent again.

**SQLAlchemy**: `rustmy/dialect.py` holds `RustmyMySQLDialect` and
`RustmyMariaDBDialect` (`mysql+rustmy`, `mariadb+rustmy`, through entry
points; extra `rustmy[sqlalchemy]`). They subclass SQLAlchemy's mysqlclient
dialect and change only rustmy's URL argument names, skip `SET NAMES` on
connect (the handshake sets the character set), count rustmy's "connection
already closed" error as a disconnect, and provide `load_provisioning` for the
test plugin. `uv.lock` resolves SQLAlchemy 2.0 for Python 3.10 and 2.1
elsewhere; the dialect must type-check against both.

## Style

- **Python**: Ruff with an explicit `extend-select` list (never `ALL`) and
  mypy strict, both configured in `pyproject.toml`; `target-version` py310,
  so no syntax newer than Python 3.10.
- **Rust**: edition 2024, MSRV 1.88 (`rust-version`; CI checks it), rustfmt
  with the default style. The lint policy lives in `[workspace.lints]` of
  `Cargo.toml` (default groups, `pedantic` and a curated list of
  `restriction` lints), and CI runs clippy with `-D warnings`:
  - no panics outside tests: no `unwrap`/`expect`/`panic!`/`buf[i]`;
    propagate `Result` and read with `.get()` (relaxed in tests via
    `clippy.toml`);
  - suppress a lint only with `#[expect(lint, reason = "...")]`; `#[allow]` is
    forbidden;
  - `crates/proto` has `#![forbid(unsafe_code)]`; `unsafe` may only live in
    `pydriver`, each block with a `// SAFETY:` comment.
