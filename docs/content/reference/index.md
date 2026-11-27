---
description: "The rustmy API reference, generated from the sources."
---

# API reference

Generated from rustmy's sources and type stubs. The [guide](../guide/index.md)
explains how the pieces fit together.

| Module | What it holds |
|---|---|
| [`rustmy`](module.md) | `connect()`, the PEP 249 globals and type objects, `install_as_MySQLdb()` |
| [`rustmy.connections`](connection.md) | `Connection`, every argument of `connect()` |
| [`rustmy.cursors`](cursors.md) | `Cursor`, `DictCursor`, `SSCursor`, `SSDictCursor`, `PreparedCursor`, `PreparedDictCursor` |
| [`rustmy.pool`](pool.md) | `Pool`, connections for threads |
| [`rustmy.aio`](aio.md) | the asyncio API: `connect()`, `create_pool()`, its connection, cursors and pool |
| [`rustmy.err`](exceptions.md) | the PEP 249 exceptions, `OutOfRangeError` and `PoolTimeout` |
| [`rustmy.converters`](converters.md) | escaping, `conversions` and the `conv=` format |
