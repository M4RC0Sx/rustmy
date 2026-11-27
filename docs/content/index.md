---
title: "A fast MySQL and MariaDB driver for Python"
description: "rustmy is a MySQL and MariaDB driver for Python written in Rust, with DB-API 2.0, asyncio, connection pools, SQLAlchemy dialects and Django support, and wheels with no system libraries."
hide:
  - navigation
  - toc
---

<div class="rm-hero" markdown>

# <span>rustmy</span>

<p class="rm-tagline">A fast MySQL and MariaDB driver for Python, written in Rust.
DB-API 2.0 and asyncio, connection pools, SQLAlchemy and Django, and wheels
with nothing else to install.</p>

<div class="rm-actions" markdown>
[Get started :lucide-arrow-right:](getting-started/index.md){ .md-button .md-button--primary }
[Performance](performance.md){ .md-button }
[:fontawesome-brands-github: GitHub <span class="rm-stars" data-repo="M4RC0Sx/rustmy" hidden></span>](https://github.com/M4RC0Sx/rustmy){ .md-button .rm-github }
</div>

<div class="rm-install" data-commands="pip install rustmy|uv add rustmy|poetry add rustmy"><span class="rm-install__prompt">$</span><span class="rm-install__line"><code class="rm-install__cmd"><span class="rm-install__tool">pip</span> install rustmy</code><span class="rm-install__caret" aria-hidden="true"></span></span><button class="rm-install__copy" type="button" title="Copy the command" aria-label="Copy the command"><svg class="rm-clip" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="9" y="9" width="12" height="12" rx="2"/><path d="M5 15V5a2 2 0 0 1 2-2h10"/></svg><svg class="rm-check" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round"><path d="M5 12l5 5 9-10"/></svg></button></div>

<div class="rm-badges" markdown>
[![PyPI](https://img.shields.io/pypi/v/rustmy)](https://pypi.org/project/rustmy/)
[![Python versions](https://img.shields.io/pypi/pyversions/rustmy)](https://pypi.org/project/rustmy/)
[![Free-threaded](https://img.shields.io/badge/free--threaded-3.14t%20%7C%203.15t-blue)](https://docs.python.org/3/howto/free-threading-python.html)
[![Downloads](https://static.pepy.tech/badge/rustmy/month)](https://pepy.tech/project/rustmy)
[![GitHub stars](https://img.shields.io/github/stars/M4RC0Sx/rustmy?style=flat&logo=github&label=stars&color=blue)](https://github.com/M4RC0Sx/rustmy/stargazers)
[![CI](https://github.com/M4RC0Sx/rustmy/actions/workflows/ci.yml/badge.svg?branch=develop)](https://github.com/M4RC0Sx/rustmy/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue)](project/contributing.md#license)
</div>

</div>

<div class="rm-stats" markdown>
<div class="rm-stat"><strong>3.3×</strong><span>faster than mysqlclient reading 1M rows</span></div>
<div class="rm-stat"><strong>3.3×</strong><span>faster inserting 1M rows with <code>executemany</code></span></div>
<div class="rm-stat"><strong>1.5×</strong><span>faster than asyncmy inserting 1M rows on asyncio</span></div>
<div class="rm-stat"><strong>0</strong><span>system libraries to install</span></div>
</div>

<figure class="rm-chart" markdown>
![Time to read and insert 1,000,000 rows, and to load 1,000,000 objects through the SQLAlchemy and Django ORMs, with rustmy, mysqlclient, PyMySQL and mysql-connector-python (lower is better)](https://raw.githubusercontent.com/M4RC0Sx/rustmy/master/benches/results/hero-light.svg#only-light)
![Time to read and insert 1,000,000 rows, and to load 1,000,000 objects through the SQLAlchemy and Django ORMs, with rustmy, mysqlclient, PyMySQL and mysql-connector-python (lower is better)](https://raw.githubusercontent.com/M4RC0Sx/rustmy/master/benches/results/hero-dark.svg#only-dark)
</figure>

## Write it the way you already do

=== "DB-API"

    ```python
    import rustmy

    with rustmy.connect(
        host="localhost", user="app", password="secret", database="shop"
    ) as conn:
        with conn.cursor() as cursor:
            cursor.execute("SELECT id, name, price FROM products WHERE price < %s", (10,))
            for product_id, name, price in cursor:
                print(product_id, name, price)
        conn.commit()
    ```

=== "asyncio"

    ```python
    import asyncio

    from rustmy import aio


    async def main() -> None:
        async with aio.create_pool(
            host="localhost", user="app", password="secret", database="shop"
        ) as pool:
            async with pool.acquire() as conn, conn.cursor() as cursor:
                await cursor.execute(
                    "SELECT id, name FROM products WHERE price < %s", (10,)
                )
                for product_id, name in await cursor.fetchall():
                    print(product_id, name)


    asyncio.run(main())
    ```

=== "SQLAlchemy"

    ```python
    from sqlalchemy import create_engine, text

    engine = create_engine("mysql+rustmy://app:secret@localhost/shop", pool_pre_ping=True)
    with engine.connect() as conn:
        rows = conn.execute(text("SELECT id, name FROM products")).all()
    ```

    On asyncio, `create_async_engine("mysql+rustmy://app:secret@localhost/shop")`.

=== "Django"

    ```python title="settings.py"
    import rustmy

    rustmy.install_as_MySQLdb()

    DATABASES = {
        "default": {
            "ENGINE": "django.db.backends.mysql",
            "NAME": "shop",
            "USER": "app",
            "PASSWORD": "secret",
            "HOST": "localhost",
        }
    }
    ```

## Why rustmy

<div class="grid cards" markdown>

-   :lucide-zap:{ .lg .middle } **Fast where it counts**

    ---

    Rows are read in small batches with the GIL released and converted in
    Rust while the server sends the next batch. `executemany` batches inserts
    and pipelines everything else.

    [:lucide-arrow-right: Performance](performance.md)

-   :lucide-package-check:{ .lg .middle } **Nothing to install but the wheel**

    ---

    No libmysqlclient, no compiler, no `pkg-config`. TLS is rustls, and the supported
    authentication methods are implemented in Rust.

    [:lucide-arrow-right: Installation](getting-started/installation.md)

-   :lucide-refresh-cw:{ .lg .middle } **asyncio on the same core**

    ---

    `rustmy.aio` runs the same Rust protocol code on the event loop's thread,
    with the API of aiomysql and asyncmy. No thread per query.

    [:lucide-arrow-right: asyncio](guide/asyncio.md)

-   :lucide-layers:{ .lg .middle } **Pools for threads and tasks**

    ---

    `rustmy.Pool` and `rustmy.aio.create_pool` serve waiters in order of
    arrival, ping idle connections and roll back what is given back open.

    [:lucide-arrow-right: Connection pools](guide/pools.md)

-   :lucide-puzzle:{ .lg .middle } **SQLAlchemy and Django**

    ---

    `mysql+rustmy://` and `mariadb+rustmy://`, blocking and asyncio, and
    Django's MySQL backend through `install_as_MySQLdb()`.

    [:lucide-arrow-right: Integrations](integrations/sqlalchemy.md)

-   :lucide-shield-check:{ .lg .middle } **Correct and tested hard**

    ---

    The server's definition of every character set, property-based tests of
    escaping and injection, fuzzing, and nine server versions in CI.

    [:lucide-arrow-right: How it is tested](project/testing.md)

</div>

## Supported

| | Supported |
|---|---|
| **Python** | CPython 3.10 to 3.15, free-threaded 3.14t and 3.15t |
| **MySQL** | 8.0, 8.4 LTS, 9.7, 26.x |
| **MariaDB** | 10.11, 11.4, 11.8, 12.3, 13.x |
| **Frameworks** | SQLAlchemy 2.0 and 2.1 (blocking and asyncio), Django 5.2 LTS and 6.x |
| **Platforms** | Linux x86_64 and aarch64 (glibc, musl), macOS x86_64 and arm64, Windows x64 and arm64 |
