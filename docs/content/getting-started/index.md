---
description: "Install rustmy and run your first queries with the DB-API, asyncio, SQLAlchemy and Django."
---

# Quick start

## Install

=== "pip"

    ```sh
    pip install rustmy
    pip install "rustmy[sqlalchemy]"            # with SQLAlchemy 2.0+
    pip install "rustmy[sqlalchemy-asyncio]"    # with SQLAlchemy's asyncio extension
    ```

=== "uv"

    ```sh
    uv add rustmy
    uv add "rustmy[sqlalchemy]"
    uv add "rustmy[sqlalchemy-asyncio]"
    ```

The wheels need nothing else installed: no libmysqlclient, no compiler. See
[Installation](installation.md) for the platforms and versions covered.

## Connect and query

rustmy follows [PEP 249](https://peps.python.org/pep-0249/), the DB-API, as
mysqlclient and PyMySQL do: `connect()`, cursors, `%s` placeholders, and
explicit commits.

```python
import rustmy

with rustmy.connect(
    host="localhost", user="app", password="secret", database="shop"
) as conn:
    with conn.cursor() as cursor:
        cursor.execute("SELECT id, name, price FROM products WHERE price < %s", (10,))
        for product_id, name, price in cursor:
            print(product_id, name, price)

        cursor.executemany(
            "INSERT INTO products (name, price) VALUES (%s, %s)",
            [("tea", 3.5), ("coffee", 4.2)],
        )
    conn.commit()  # (1)!
```

1.  Autocommit is off by default, as PEP 249 asks: without `commit()` the
    inserts are rolled back when the connection closes. Pass
    `autocommit=True` to `connect()` to commit every statement.

Leaving the `with` block closes the connection. Parameters are always passed
separately from the SQL: rustmy escapes them for the connection's character
set and SQL mode.

## Rows as dictionaries

```python
from rustmy.cursors import DictCursor

with conn.cursor(DictCursor) as cursor:
    cursor.execute("SELECT id, name FROM products ORDER BY id LIMIT 2")
    print(cursor.fetchall())  # ({'id': 1, 'name': 'tea'}, {'id': 2, 'name': 'coffee'})
```

## asyncio

`rustmy.aio` runs the same driver on the event loop, with the API of
aiomysql and asyncmy:

```python
import asyncio

from rustmy import aio


async def main() -> None:
    async with aio.connect(
        host="localhost", user="app", password="secret", database="shop"
    ) as conn:
        async with conn.cursor(aio.DictCursor) as cursor:
            await cursor.execute("SELECT id, name FROM products WHERE id = %s", (42,))
            print(await cursor.fetchone())


asyncio.run(main())
```

## SQLAlchemy and Django

=== "SQLAlchemy"

    The `mysql+rustmy` (or `mariadb+rustmy`) URL, nothing to import:

    ```python
    from sqlalchemy import create_engine, text

    engine = create_engine("mysql+rustmy://app:secret@localhost/shop", pool_pre_ping=True)
    with engine.connect() as conn:
        rows = conn.execute(text("SELECT id, name FROM products")).all()
    ```

=== "Django"

    Two lines at the top of `settings.py`, then the usual MySQL backend:

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

## Next steps

<div class="grid cards" markdown>

-   :lucide-book-open:{ .lg .middle } **[The guide](../guide/index.md)**

    ---

    Connecting, queries, cursors, TLS, asyncio, pools and running in
    production.

-   :lucide-arrow-right-left:{ .lg .middle } **[Migrating](../migrating/mysqlclient.md)**

    ---

    What changes coming from mysqlclient, PyMySQL, aiomysql or asyncmy.

-   :lucide-code:{ .lg .middle } **[API reference](../reference/index.md)**

    ---

    Every class, function and argument.

-   :lucide-gauge:{ .lg .middle } **[Performance](../performance.md)**

    ---

    The benchmarks against the other drivers.

</div>
