---
description: "How rustmy is built and tested: conformance suites, property-based tests, fuzzing, differential tests and nine server versions in CI."
---

# How it is built and tested

rustmy is a Rust workspace with a pure-Rust protocol core (`rustmy-proto`:
packets, handshake, authentication, TLS, row decoding; no `unsafe`) and thin
[PyO3](https://pyo3.rs) bindings that build Python objects straight from the
wire. The asyncio API runs the same protocol code, written once as `async`
functions over a transport.

## On every pull request

- The **PEP 249 compliance suite** (`dbapi20.py`), **SQLAlchemy's dialect
  suite** (blocking and asyncio) and part of **Django's test suite** (all of
  it weekly), the latter compared test by test with mysqlclient.
- **Property-based tests** with [hypothesis](https://hypothesis.readthedocs.io)
  against real servers: arbitrary text and bytes must round-trip byte for
  byte, and generated injection attempts must stay values, with and without
  `NO_BACKSLASH_ESCAPES`.
- **Every character set** of every supported server, character by character.
- **Differential tests** against [PyMySQL](https://github.com/PyMySQL/PyMySQL).
- **Integration tests on all nine supported server versions**, uncompressed
  and compressed, and every supported Python against MySQL and MariaDB.
- **Performance tracking** with [CodSpeed](https://codspeed.io/M4RC0Sx/rustmy),
  and **coverage** of the Python and Rust code (the bindings included,
  measured through the Python tests) on
  [Codecov](https://app.codecov.io/gh/M4RC0Sx/rustmy).
- `cargo clippy` with pedantic lints and no panics, and `cargo deny` for
  licenses and advisories.

## asyncio against the blocking API

Hypothesis stores and reads the same generated values through `rustmy.aio`
and the blocking connection, with every kind of cursor, and requires the same
rows, descriptions and counts.

## Fuzzing

The protocol code is fuzzed with cargo-fuzz (packets, compression, the
handshake, result sets, row values, the SQL scanner and whole sessions), and
a differential fuzzer runs each generated
session through the current code and the 1.3.2 release, failing on any
difference in results or errors.

## Before a release

`scripts/leak_check.py` runs every path through the driver (connections, each
cursor, prepared statements, errors, `executemany`, compression, `LOAD DATA
LOCAL INFILE`…), blocking and asyncio, pools included, thousands of times and
checks that memory stops growing.

Releases are built and published by GitHub Actions, with every action pinned
to a commit. Each wheel and the sdist carry a signed build provenance
attestation, checked before publishing, and PyPI's own:

```sh
gh attestation verify rustmy-*.whl --repo M4RC0Sx/rustmy
```

## Built with

- [PyO3](https://github.com/PyO3/pyo3) and [maturin](https://github.com/PyO3/maturin): Rust bindings and packaging for Python.
- [rustls](https://github.com/rustls/rustls), [ring](https://github.com/briansmith/ring) and [rustls-platform-verifier](https://github.com/rustls/rustls-platform-verifier): TLS without OpenSSL.
- [RustCrypto](https://github.com/RustCrypto) (`rsa`, `sha1`, `sha2`, `pbkdf2`) and [ed25519-dalek](https://github.com/dalek-cryptography/curve25519-dalek): the authentication methods.
- [SQLAlchemy](https://www.sqlalchemy.org): the dialects build on its MySQL dialect.
- [uv](https://github.com/astral-sh/uv), [Ruff](https://github.com/astral-sh/ruff), [mypy](https://mypy-lang.org), [pytest](https://pytest.org), [hypothesis](https://hypothesis.readthedocs.io), [cargo-deny](https://github.com/EmbarkStudios/cargo-deny) and [cargo-fuzz](https://github.com/rust-fuzz/cargo-fuzz): development and testing.
- [Zensical](https://zensical.org) and [mkdocstrings](https://mkdocstrings.github.io): this site.
- Stuart Bishop's [DB-API 2.0 compliance suite](https://github.com/baztian/dbapi-compliance) (public domain).

The licenses of the Rust crates compiled into each wheel ship with it in
`THIRD-PARTY-LICENSES`.
