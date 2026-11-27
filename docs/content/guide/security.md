---
description: "Security notes for rustmy: parameters and escaping, building SQL by hand, multi-statements, local files and what verified TLS protects."
---

# Security notes

## Parameters, not formatting

Pass values as parameters (`%s`, `%(name)s`), never by formatting SQL
yourself: rustmy escapes them for the connection's character set and SQL
mode, and refuses text that the server would read differently.

```python
# Never:
cursor.execute(f"SELECT * FROM users WHERE name = '{name}'")
# Always:
cursor.execute("SELECT * FROM users WHERE name = %s", (name,))
```

## Building SQL by hand

To build SQL by hand, use the connection's `conn.escape_string()` and
`conn.literal()`, which follow its character set and `NO_BACKSLASH_ESCAPES`.
The module-level `rustmy.converters.escape_string()` and `literal()` assume
a UTF-8 connection in the default SQL mode.

An encoder given in `conv=` returns SQL text that goes into the statement as
it is: it must return a complete, escaped literal.

## Off unless you ask

- `multi_statements` is off, which keeps an injection from adding statements
  of its own.
- `LOAD DATA LOCAL INFILE` only sends files from the directory you allow,
  and only the file the statement names: see
  [Loading local files](local-files.md).

## The network

Without verified TLS (`ssl_mode="verify_identity"` or `verify_ca`), whoever
can intercept the connection can impersonate the server, and with
`caching_sha2_password` or `sha256_password` obtain the password during a
full authentication, as with any MySQL client. An account that uses
`mysql_clear_password` gives its password away over unverified TLS too.

Pinning the server's RSA key with `server_public_key="server-key.pem"` closes
that path over plain TCP: the password is then only ever encrypted with that
key. `get_server_public_key=False` never asks the server for its key: see
[RSA keys over plain TCP](tls.md#rsa-keys-over-plain-tcp).

`mysql_clear_password` is only ever used over TLS or a Unix socket.

## Reporting a vulnerability

Please report vulnerabilities privately, as the
[security policy](https://github.com/M4RC0Sx/rustmy/blob/master/SECURITY.md)
explains.
