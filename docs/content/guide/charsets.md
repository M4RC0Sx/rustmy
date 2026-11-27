---
description: "Character sets and collations in rustmy, how it follows the server's definitions, and custom conversions with conv=."
---

# Character sets and conversions

## Character sets

`utf8mb4` is the default and the right choice for new code. Legacy character
sets (`latin1`, `cp1251`, `gbk`, `big5`, `sjis`, `euckr`…) are supported for
existing databases, and decode in Rust:

```python
conn = rustmy.connect(..., charset="latin1", collation="latin1_spanish_ci")
conn.set_character_set("utf8mb4")  # switch later
print(conn.character_set_name(), conn.encoding)  # utf8mb4 utf8
```

- `utf8` means `utf8mb4`. Any MySQL character set with a Python codec works.
- The handshake already sets the character set, so no `SET NAMES` follows
  unless a collation is given (or the server lacks the character set).
- `set_character_set()` survives `reset_connection()` and `change_user()`.
- User and database names and connection attributes go in the connection's
  character set; the password always as UTF-8.

## The server's definition wins

Python's codecs and MySQL do not always agree on what a byte means. rustmy
follows the **server's** definition of each character set: single-byte tables
are patched byte by byte, `euckr` uses Python's `cp949` (the server's is
UHC), and characters the server cannot represent are refused with
`DataError` before anything is sent, instead of being stored as something
else.

That closes an injection path: Python encodes `¥` as the byte `0x5C` in
`sjis`/`ujis`, which the server reads as a backslash. Text that would not
decode back to itself after encoding is refused. Every character of every
supported server is checked by the test suite.

## Custom conversions

`conv=` takes MySQLdb's format: a dict from MySQL field types to decoders,
and from Python types to encoders. Only the types you change leave the fast
path:

```python
from rustmy.constants import FIELD_TYPE
from rustmy.converters import conversions

conv = {**conversions, FIELD_TYPE.NEWDECIMAL: float}  # DECIMAL as float
conn = rustmy.connect(..., conv=conv)
```

- A custom decoder gets `str` for `DECIMAL`, date and time columns, and
  `bytes` otherwise.
- A custom encoder is tried first for its type, and returns SQL text that
  goes into the statement as it is: it must return a complete, escaped
  literal.
- `PreparedCursor` cannot use custom encoders (`NotSupportedError`): its
  parameters are typed values, not SQL.
