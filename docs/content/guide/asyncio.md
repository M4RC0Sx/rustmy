---
description: "rustmy.aio, the asyncio API: the same Rust protocol code on the event loop's thread, with the API of aiomysql and asyncmy."
---

# asyncio

`rustmy.aio` runs the same Rust protocol code on the event loop's thread,
with no threads and no runtime of its own: no `run_in_executor` per query. It
takes the arguments of `rustmy.connect()` (TLS, compression, option files,
`local_infile_dir`, `init_command`…), and its API has the shape of
aiomysql's and asyncmy's, so code written for them mostly just works (the
differences are in [Coming from aiomysql or asyncmy](../migrating/aiomysql.md)).

```python
from rustmy import aio

async with aio.connect(
    host="localhost",
    user="app",
    password="secret",
    database="shop",
    read_timeout=30,
) as conn:  # closed at the end of the block
    async with conn.cursor(aio.DictCursor) as cursor:
        await cursor.execute("SELECT id, name FROM products WHERE id = %s", (42,))
        product = await cursor.fetchone()  # {"id": 42, "name": "tea"}, or None
```

`conn = await aio.connect(...)` opens one to keep and close yourself.

## Coroutines

Every call that talks to the server is a coroutine: `execute`,
`executemany`, the `fetch*` methods, `nextset`, `callproc`, `commit`,
`rollback`, `ping`… `conn.close()` is not (it sends the server a goodbye
without waiting for it); `await conn.ensure_closed()` is the same under
aiomysql's and asyncmy's name.

As with the blocking API, autocommit is off, so statements run in a
transaction until `commit()`:

```python
async with conn.cursor() as cursor:
    try:
        await cursor.execute(
            "UPDATE accounts SET balance = balance - %s WHERE id = %s", (10, 1)
        )
        await cursor.execute(
            "UPDATE accounts SET balance = balance + %s WHERE id = %s", (10, 2)
        )
    except aio.Error:
        await conn.rollback()
        raise
await conn.commit()
```

## Streaming

Large results stream with `SSCursor` (or `SSDictCursor`): rows are read from
the server only as fast as you consume them, with flat memory:

```python
async with conn.cursor(aio.SSCursor) as cursor:
    await cursor.execute("SELECT id, total FROM orders")
    async for order_id, total in cursor:
        ...
```

## How it behaves

Cursors
:   `Cursor`, `DictCursor`, `PreparedCursor`, `PreparedDictCursor`,
    `SSCursor` and `SSDictCursor`, as in the blocking API. `executemany`
    groups rows into multi-row `INSERT`s and pipelines other statements, as
    the blocking cursors do.

One command at a time
:   Per connection: starting another before the first is awaited raises
    `ProgrammingError`. Give each task its own connection, or use a
    [pool](pools.md).

Timeouts
:   `read_timeout` and `write_timeout` fail a command that waits that long
    for the server (error 2013, the connection closed); `asyncio.timeout()`
    around a call works too.

Cancellation
:   A task cancelled in the middle of a command closes its connection: the
    server goes on with the statement, and its reply would otherwise arrive
    in the middle of the next command. Pools replace closed connections.

Local files
:   `LOAD DATA LOCAL INFILE` reads the file in the loop's executor, so the
    loop never waits on the disk.

Event loops
:   asyncio's default loop and uvloop.

A connection dropped while open is closed when it is garbage collected, with
a `ResourceWarning`, as asyncio's own transports do.

## With SQLAlchemy

Install `rustmy[sqlalchemy-asyncio]` and use the usual URLs with
`create_async_engine`: see [SQLAlchemy](../integrations/sqlalchemy.md#asyncio).
