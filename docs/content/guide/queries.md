---
description: "Running queries with rustmy: placeholders, how Python and MySQL types map, executemany, stored procedures and errors."
---

# Queries and parameters

## Placeholders

Parameters are escaped and interpolated in Rust, never by hand. rustmy's
`paramstyle` is `format`, the one mysqlclient and PyMySQL use: `%s` for
positional parameters and `%(name)s` for named ones.

```python
with conn.cursor() as cursor:
    cursor.execute("SELECT * FROM products WHERE name = %s AND price < %s", ("tea", 5))
    cursor.execute("SELECT * FROM products WHERE name = %(name)s", {"name": "tea"})
    # A sequence becomes a list, for IN.
    cursor.execute("SELECT * FROM products WHERE id IN %s", ((1, 2, 3),))
    rows = cursor.fetchall()
```

`execute()` returns the row count. Without parameters the query is sent as
it is, so a `%` in it needs no escaping; with parameters, write a literal
`%` as `%%`. `cursor.mogrify(query, args)` returns the statement exactly as
`execute()` would send it.

!!! warning "Never format SQL yourself"

    Pass values as parameters, never with f-strings or `%` on the query:
    rustmy escapes them for the connection's character set and SQL mode
    (`NO_BACKSLASH_ESCAPES` included), and refuses text that the server would
    read differently. See [Security notes](security.md).

## Types

Python types map to MySQL ones both ways:

| Python | MySQL |
|---|---|
| `int` | integer types, `YEAR` |
| `float` | `FLOAT`, `DOUBLE` |
| `decimal.Decimal` | `DECIMAL` |
| `str` | `CHAR`, `VARCHAR`, `TEXT`, `ENUM`, `SET`, `JSON` |
| `bytes` | `BINARY`, `VARBINARY`, `BLOB`, `BIT` (sent as `_binary'…'`) |
| `datetime.date` | `DATE` |
| `datetime.datetime` | `DATETIME`, `TIMESTAMP` |
| `datetime.timedelta` | `TIME` (which can exceed a day, or be negative) |
| `None` | `NULL` |

Parameters may also be `bool` (sent as `1` or `0`), `datetime.time` (sent as
a `TIME`) and `uuid.UUID` (sent as its text).

Invalid dates such as `0000-00-00` come back as `None`. `conv=` changes how
any of these are read or written: see
[Custom conversions](charsets.md#custom-conversions).

## Many rows at once

`executemany` turns an `INSERT … VALUES` into multi-row statements, so 10,000
rows take a few round trips instead of 10,000:

```python
rows = [("tea", 3.5), ("coffee", 4.2), ("cocoa", 5.0)]
with conn.cursor() as cursor:
    cursor.executemany("INSERT INTO products (name, price) VALUES (%s, %s)", rows)
    print(cursor.rowcount, cursor.lastrowid)
conn.commit()
```

Each statement is at most `cursor.max_stmt_length` bytes (1,024,000, as in
PyMySQL, under the 1 MiB `max_allowed_packet` of old servers; 256 KiB of
parameters for `PreparedCursor`, where larger statements measured slower).
Raise it on a cursor to send fewer, larger statements to a server that
allows them.

Any other statement (`UPDATE`, `DELETE`…) still runs once per item, but
pipelined: rustmy sends up to `cursor.max_pipeline` statements (256 by
default, 16 KiB at most) before reading their replies, so a batch pays one
network round trip per window instead of one per statement. With a 3 ms
round trip, 1,000 `UPDATE`s take under 0.1 s instead of 3.4 s:

```python
with conn.cursor() as cursor:
    cursor.executemany(
        "UPDATE products SET price = %(price)s WHERE name = %(name)s",
        [{"name": "tea", "price": 3.9}, {"name": "cocoa", "price": 5.5}],
    )
    print(cursor.rowcount)  # rows affected by all of them
conn.commit()
```

!!! note "When one statement of a pipelined batch fails"

    The server runs every statement it receives, so when one fails, the rest
    of its window has already run by the time the error is raised (no
    further window is sent). Inside a transaction (the default: autocommit
    is off), `conn.rollback()` undoes them all. With autocommit on, set
    `cursor.max_pipeline = 1` to stop right at the failing statement, as a
    loop of `execute()` calls would.

## Several result sets

A query that returns more than one result set (several statements with
`multi_statements=True`, or a stored procedure) is read one set at a time:
`cursor.nextset()` moves to the next one and returns `None` when there are
no more.

## Stored procedures

```python
with conn.cursor() as cursor:
    cursor.callproc("monthly_totals", (2026, 11))
    rows = cursor.fetchall()
    while cursor.nextset():
        pass  # the procedure's other result sets, then the CALL's own
```

Arguments go through server variables named `@_procname_n` (`n` counting
from 0), so OUT and INOUT values can be read afterwards with
`SELECT @_monthly_totals_0, …`. `procname` may name the database too
(`"reports.monthly_totals"`).

## Errors

The PEP 249 exceptions are classed by the server's SQLSTATE, and carry the
MySQL error code and message as their `args`:

```python
from rustmy.constants import ER

try:
    with conn.cursor() as cursor:
        cursor.execute("INSERT INTO products (id, name) VALUES (1, 'tea')")
except rustmy.IntegrityError as error:
    code, message = error.args
    if code == ER.DUP_ENTRY:
        print("already there:", message)
```

| SQLSTATE class | Exception | For example |
|---|---|---|
| `23` | `IntegrityError` | duplicate keys, foreign keys, `NOT NULL` |
| `42` | `ProgrammingError` | syntax errors, unknown tables and columns, access denied to a database |
| `22` | `DataError` | values too long or out of range |
| `0A` | `NotSupportedError` | features the server does not support |
| `XX` | `InternalError` | internal server errors |
| any other | `OperationalError` | lost connections, authentication, deadlocks |

Errors of the driver itself are `InterfaceError` (a closed connection, for
example) or `ProgrammingError` (a cursor used wrongly). Text the connection's
character set cannot represent raises `DataError` before anything is sent.

They all derive from `rustmy.Error`, and are also attributes of every
connection (`conn.IntegrityError`), as PEP 249 suggests. A numeric value out
of range (1690) raises `rustmy.err.OutOfRangeError`, both a `DataError` (its
SQLSTATE) and an `OperationalError` (as mysqlclient raises it). The full
hierarchy is in the [API reference](../reference/exceptions.md).
