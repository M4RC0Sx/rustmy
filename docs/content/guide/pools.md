---
description: "Connection pools for threads (rustmy.Pool) and asyncio tasks (rustmy.aio.create_pool), serving waiters in order of arrival."
---

# Connection pools

`rustmy.Pool` lends connections to threads, and `rustmy.aio.create_pool` to
tasks. Both take `connect()`'s arguments, keep `minsize` connections open,
open more as needed up to `maxsize`, and then make callers wait for one to
come back, **in order of arrival**. Leaving the pool's block closes it, after
waiting for the connections in use.

=== "Threads"

    ```python
    import rustmy

    with rustmy.Pool(
        minsize=2, maxsize=10, host="localhost", user="app", password="secret"
    ) as pool:
        with pool.acquire(timeout=5) as conn, conn.cursor() as cursor:
            cursor.execute("SELECT COUNT(*) FROM orders")
            (count,) = cursor.fetchone()
    ```

=== "asyncio"

    ```python
    from rustmy import aio

    async with aio.create_pool(
        minsize=2, maxsize=10, host="localhost", user="app", password="secret"
    ) as pool:
        async with pool.acquire(timeout=5) as conn, conn.cursor() as cursor:
            await cursor.execute("SELECT COUNT(*) FROM orders")
            (count,) = await cursor.fetchone()
    ```

## Options

| Argument | Default | |
|---|---|---|
| `minsize` | 1 | connections opened at once (their errors raise there) and kept open |
| `maxsize` | 10 | connections open at most, lent or idle |
| `pre_ping` | 30 | seconds idle after which a connection is pinged before it is lent (`None`: never) |
| `pool_recycle` | -1 | seconds after which a connection is closed instead of lent again (-1: never) |
| `idle_timeout` | `None` | seconds unused after which idle connections beyond `minsize` are closed |
| `reset_on_release` | `False` | reset the session (variables, temporary tables) of each connection given back: a round trip per use |
| `acquire(timeout=…)` | `None` | seconds to wait for a connection before raising `rustmy.PoolTimeout` (`None`: forever) |

## What a pool does for you

- A connection given back with a transaction open is rolled back, and one
  that is closed or failed is not lent again. Idle ones are lent most
  recently used first.
- `rustmy.Pool` opens the connections for the minimum on threads of their
  own, so giving one back never waits for a new one to open.

## Without a `with` block

=== "Threads"

    ```python
    lease = pool.acquire()
    conn = lease.connection
    try:
        ...
    finally:
        pool.release(conn)
    ```

=== "asyncio"

    ```python
    conn = await pool.acquire()
    try:
        ...
    finally:
        pool.release(conn)  # awaiting it is optional (1)
    ```

    1.  It is already done unless a rollback is needed.

`pool.size` and `pool.freesize` count the open and the idle connections;
`clear()` closes the idle ones, `close()` the pool (those in use as they come
back) and `wait_closed()` waits for them; `terminate()` closes every
connection at once.

## Fair waiting

Waiters are served first come, first served: under contention a task never
waits much longer than the others. aiomysql's and asyncmy's pools let the
task that gives a connection back take it again at once, a little faster
with more tasks than connections (a few percent; one more turn of the event
loop per handover here), at the cost of tasks that wait far longer than the
rest: with 32 tasks over 8 connections, the longest wait was 2.6 ms here
against 102 ms with asyncmy.

The accounts of both pools are kept in Rust, in the same code, without I/O.

## SQLAlchemy's pool

With SQLAlchemy, use its own pool (`pool_pre_ping=True`, and `pool_recycle`
below the server's `wait_timeout`): see
[SQLAlchemy](../integrations/sqlalchemy.md).
