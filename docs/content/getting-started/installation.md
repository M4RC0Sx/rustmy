---
description: "Installing rustmy, its extras, and the Python versions, servers, frameworks and platforms it supports."
---

# Installation

=== "pip"

    ```sh
    pip install rustmy
    pip install "rustmy[sqlalchemy]"           # with SQLAlchemy 2.0+
    pip install "rustmy[sqlalchemy-asyncio]"   # and its asyncio extension (greenlet)
    ```

=== "uv"

    ```sh
    uv add rustmy
    uv add "rustmy[sqlalchemy]"
    uv add "rustmy[sqlalchemy-asyncio]"
    ```

Wheels cover Linux (glibc and musl, x86_64 and aarch64), macOS (Intel and
Apple Silicon) and Windows (x64 and arm64), and need nothing else installed.
Every wheel is installed and tested before release.

!!! note "Building from source"

    Elsewhere (FreeBSD, ppc64le, s390x, 32-bit systems, a new Python before
    its wheels), pip builds rustmy from the source distribution, which needs
    a [Rust toolchain](https://rustup.rs) and a C compiler (for the bundled
    zstd library).

## Supported versions

Everything below is tested in CI.

| | Supported |
|---|---|
| **Python** | CPython 3.10 to 3.15, and the free-threaded 3.14t and 3.15t (on Windows arm64, from 3.11: there is no official CPython 3.10 for it). PyPy is not supported. |
| **MySQL** | 8.0, 8.4 LTS, 9.7, 26.x |
| **MariaDB** | 10.11, 11.4, 11.8, 12.3, 13.x |
| **SQLAlchemy** | 2.0 and 2.1 (2.1 needs Python 3.11 or later; CI tests the latest release each Python gets), blocking and asyncio (not on free-threaded builds: greenlet does not run there yet) |
| **Django** | 5.2 LTS (Python 3.10 or later) and 6.x (Python 3.12 or later); Django's whole test suite runs weekly in CI |
| **Platforms** | Linux x86_64 and aarch64 (glibc, musl), macOS x86_64 and arm64, Windows x64 and arm64 |

## Verifying a download

Each wheel and the sdist carry a signed build provenance attestation,
checked before publishing, and PyPI's own. To check a file you downloaded:

```sh
gh attestation verify rustmy-*.whl --repo M4RC0Sx/rustmy
```

## Extras

`rustmy[sqlalchemy]`
:   SQLAlchemy 2.0 or later, for the `mysql+rustmy://` and
    `mariadb+rustmy://` dialects. See [SQLAlchemy](../integrations/sqlalchemy.md).

`rustmy[sqlalchemy-asyncio]`
:   SQLAlchemy with its asyncio extension (greenlet), for
    `create_async_engine`.

Django needs no extra: see [Django](../integrations/django.md).
