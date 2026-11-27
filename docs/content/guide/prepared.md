---
description: "Server-side prepared statements over MySQL's binary protocol with rustmy's PreparedCursor."
---

# Prepared statements

`PreparedCursor` (and `PreparedDictCursor`) runs server-side prepared
statements over MySQL's binary protocol:

- **Parameters travel as typed values**, never as SQL text, so nothing
  depends on escaping.
- **Rows arrive in binary form**, so numbers and dates need no parsing: on a
  table of doubles, dates and decimals, reading takes about half the time of
  `Cursor`; on mostly text, about the same.
- **Each statement is prepared once per connection.**

```python
with conn.cursor(prepared=True) as cursor:
    cursor.execute("SELECT id, name, price FROM products WHERE price > %s", (4,))
    print(cursor.fetchall())  # the same objects a Cursor returns
```

`conn.cursor(prepared=True)` gives the prepared version of the connection's
cursor class; `conn.cursor(PreparedCursor)` and `cursorclass=PreparedCursor`
work too. Rows come back as the very same Python objects a `Cursor` returns.

## How it behaves

- **Placeholders.** Write `%s` / `%(name)s` as usual; they become `?`. A
  query without placeholders has nothing to bind and runs as a `Cursor` runs
  it, so DDL and introspection behave as libraries expect.
- **The statement cache.** The last `statement_cache_size` statements (32 by
  default, a `connect()` argument) stay prepared on the server, which allows
  16,382 across all its connections by default (`max_prepared_stmt_count`):
  32 per connection fits about 500 connections. When that limit is reached
  (error 1461), rustmy closes the connection's least recently used statement
  and prepares again, once; with many more connections, lower
  `statement_cache_size` or raise the server's limit.
- **`executemany`** batches `INSERT … VALUES` into multi-row prepared
  statements and pipelines everything else, as `Cursor` does.

## Limits

- Parameters are values only: a sequence for `IN %s`, or a type with a
  `conv=` encoder, raises `NotSupportedError`.
- A statement the server cannot prepare raises its error; there is no silent
  fallback to text.
- MySQL 9.0 and later refuse a `TIME` parameter between 838:00:00 and
  838:59:59 with fractional seconds (error 1690, although it is in range);
  use `Cursor` for those.

## With SQLAlchemy and Django

SQLAlchemy can use it for every statement:

```python
engine = create_engine(
    "mysql+rustmy://user:password@host/db?cursorclass=PreparedCursor"
)
```

Its dialect test suite passes this way too. Django cannot: its MySQL backend
writes SQL that MySQL refuses to prepare (`->> %s` for `JSONField`,
`_binary %s` for `BinaryField`, parameters in DDL), so keep the default
cursor there.
