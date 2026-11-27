---
description: "rustmy's cursors: tuples or dicts, buffered or streamed, and the PEP 249 methods and attributes."
---

# Cursors

`conn.cursor()` returns a cursor of the class given to `connect()`
(`cursorclass=`, `Cursor` by default), or of the class you pass:

```python
from rustmy.cursors import DictCursor, SSCursor

cursor = conn.cursor()  # tuples, the whole result read by execute()
cursor = conn.cursor(DictCursor)  # dicts
cursor = conn.cursor(SSCursor)  # tuples, streamed as you fetch
cursor = conn.cursor(prepared=True)  # the prepared-statement version
```

| Cursor | Rows | Result |
|---|---|---|
| `Cursor` | tuples | buffered: read whole by `execute()` |
| `DictCursor` | dicts | buffered |
| `SSCursor` | tuples | streamed: read as you fetch |
| `SSDictCursor` | dicts | streamed |
| `PreparedCursor` | tuples | buffered, [prepared statements](prepared.md) |
| `PreparedDictCursor` | dicts | buffered, prepared statements |

All of them are context managers (`with conn.cursor() as cursor:` closes the
cursor at the end) and iterate over their rows.

## Buffered cursors

The default. `execute()` reads the whole result: rows arrive in batches with
the GIL released, and each batch is converted in Rust while the server sends
the next one. `fetchone()`, `fetchmany()` and `fetchall()` then just hand the
rows out, and `scroll()` moves anywhere in them. Ctrl+C interrupts a long
read between batches.

## Rows as dictionaries

```python
with conn.cursor(DictCursor) as cursor:
    cursor.execute("SELECT id, name FROM products ORDER BY id LIMIT 2")
    print(cursor.fetchall())  # ({'id': 1, 'name': 'tea'}, {'id': 2, 'name': 'coffee'})
```

`rustmy.connect(..., cursorclass=DictCursor)` makes it the default. A column
name that repeats an earlier one becomes `table.column` (the table alias,
empty for computed columns), as in other MySQL drivers.

## Large results without loading them

A streaming cursor reads rows as you iterate, in small batches, with flat
memory however large the result is:

```python
from rustmy.cursors import SSCursor

with conn.cursor(SSCursor) as cursor:
    cursor.execute("SELECT id, payload FROM events")
    for event_id, payload in cursor:
        handle(event_id, payload)
```

The MySQL protocol sets the rules of a streamed result, whatever the driver:

- **The connection is busy until every row is read.** If another query runs
  on it first (from any cursor), the rest of the rows is discarded with a
  `UserWarning`, and fetching from the stream then raises `ProgrammingError`
  instead of silently ending early. Closing the cursor, or leaving its
  `with` block, discards them quietly.
- **`rowcount` is `-1`** until the end: the size is unknown before then.
- **`scroll()` only moves forward.**
- **Slow readers keep the server waiting.** Past its `net_write_timeout` (60
  seconds by default) it drops the connection.

## What a cursor tells you

`description`
:   One 7-item sequence per column (name, type code, display size, internal
    size, precision, scale, nullable), or `None` after a statement that
    returned no rows.

`rowcount`
:   Rows returned or affected by the last statement; `-1` before any, and
    while a streamed result is unread.

`lastrowid`
:   The `AUTO_INCREMENT` value of the last insert; `None` after a statement
    that returns rows.

`rownumber`
:   Index of the next row to fetch.

`arraysize`
:   Rows `fetchmany()` returns by default (1).

Fetching after a statement that returned no rows (an `INSERT`, DDL) raises
`ProgrammingError`, as PEP 249 asks. When exhausted, `fetchone()` returns
`None` and `fetchmany()`/`fetchall()` an empty tuple.

## Threads

A connection belongs to one thread at a time (`threadsafety = 1`): give each
thread its own connection, or use a [pool](pools.md). Using a busy
connection from another thread raises `ProgrammingError` instead of
corrupting it. On free-threaded Python nothing in rustmy serializes threads
that use their own connections.
