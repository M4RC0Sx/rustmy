---
description: "Moving to rustmy from mysqlclient (MySQLdb) or PyMySQL, and the deliberate differences."
---

# Coming from mysqlclient or PyMySQL

Most code runs unchanged: `connect()` takes the same arguments in the same
positional order, with `passwd` and `db` as aliases, and `cursorclass`,
`DictCursor`, `SSCursor`, `conv=` and `rustmy.constants` are where code
written for MySQLdb or PyMySQL expects them.

=== "mysqlclient"

    ```python
    import rustmy as MySQLdb  # or rustmy.install_as_MySQLdb()

    conn = MySQLdb.connect(host="localhost", user="app", passwd="secret", db="shop")
    ```

=== "PyMySQL"

    ```python
    import rustmy

    conn = rustmy.connect(
        host="localhost",
        user="app",
        password="secret",
        database="shop",
        cursorclass=rustmy.cursors.DictCursor,
    )
    ```

## What differs, on purpose

Each difference is pinned by a test.

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
  [Many rows at once](../guide/queries.md#many-rows-at-once);
  `cursor.max_pipeline = 1` sends them one by one, as both drivers do).
- `LOAD DATA LOCAL INFILE` needs `local_infile_dir`: `local_infile=True`
  alone, which lets mysqlclient and PyMySQL send any file the server asks for,
  raises `ValueError` (see [Loading local files](../guide/local-files.md)).

## What you gain

- Wheels with nothing else to install, where mysqlclient needs libmysqlclient,
  its headers, `pkg-config` and a compiler.
- [Prepared statements](../guide/prepared.md), [asyncio](../guide/asyncio.md)
  and [connection pools](../guide/pools.md), which neither has.
- Pipelined `executemany` for `UPDATE` and `DELETE`.
- See [Performance](../performance.md) for the speed.
