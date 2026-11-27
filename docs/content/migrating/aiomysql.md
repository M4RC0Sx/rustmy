---
description: "Moving to rustmy.aio from aiomysql or asyncmy, and the deliberate differences."
---

# Coming from aiomysql or asyncmy

`rustmy.aio` keeps their API: `connect()` and `create_pool()` awaited or
used with `async with`, `await conn.cursor()` (or plain `conn.cursor()`), the
same coroutines on connections and cursors, a plain `close()` and an awaited
`ensure_closed()`, autocommit off.

```python
from rustmy import aio  # instead of: import aiomysql / import asyncmy

pool = await aio.create_pool(host="localhost", user="app", password="secret", db="shop")
async with pool.acquire() as conn:
    async with conn.cursor(aio.DictCursor) as cursor:
        await cursor.execute("SELECT id, name FROM products WHERE id = %s", (42,))
        row = await cursor.fetchone()
```

## What differs, on purpose

- Fetching after a statement that returned no rows (an `INSERT`, DDL) raises
  `ProgrammingError`, as PEP 249 asks; both return `[]` from `fetchall()` and
  `None` from `fetchone()`.
- Server errors are classed by SQLSTATE, as in the blocking API: "unknown
  column" (1054) is a `ProgrammingError`, where both raise
  `OperationalError`.
- A task cancelled in the middle of a command closes its connection, as in
  both; the next command on it raises `OperationalError` (2013) instead of
  `InterfaceError`.
- Pools serve waiting tasks in order of arrival (see
  [Connection pools](../guide/pools.md)), `acquire()` takes a `timeout`, and
  a connection idle for 30 seconds is pinged before it is lent (`pre_ping`).
  `pool_recycle` is off by default, as in aiomysql (asyncmy recycles after an
  hour), and there is no `echo`.
- The rest follows rustmy's blocking API, so the differences in
  [Coming from mysqlclient or PyMySQL](mysqlclient.md) apply too.

## With SQLAlchemy

`create_async_engine("mysql+rustmy://...")` replaces
`mysql+aiomysql://` and `mysql+asyncmy://`: see
[SQLAlchemy](../integrations/sqlalchemy.md#asyncio).
