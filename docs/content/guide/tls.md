---
description: "TLS with rustmy (ssl_mode, CA files, client certificates) and its authentication methods, including PAM, LDAP and cloud IAM tokens."
---

# TLS and authentication

## TLS

TLS is on whenever the server offers it (`ssl_mode="preferred"`, the
default). To require it and check the server's certificate:

```python
conn = rustmy.connect(
    host="db.example.com",
    user="app",
    password="secret",
    ssl_mode="verify_identity",
    ssl_ca="/etc/ssl/db-ca.pem",  # omit to trust the system's CAs
    ssl_cert="/etc/ssl/client.pem",  # client certificate, if required
    ssl_key="/etc/ssl/client-key.pem",
)
print(conn.tls)  # True
```

| `ssl_mode` | Encrypted | Server certificate checked |
|---|---|---|
| `disabled` | never | no |
| `preferred` (default) | when the server offers TLS | no |
| `required` | always, or the connection fails | no |
| `verify_ca` | always | signed by a trusted CA |
| `verify_identity` | always | signed by a trusted CA, for the host connected to |

With `ssl_ca`, verification trusts only that file, as PyMySQL does; without
it, the verify modes use the operating system's trust store. TLS is
[rustls](https://github.com/rustls/rustls): no OpenSSL anywhere. The TLS
configuration is built once per process for each set of options and reused
while its files are unchanged, so new connections skip rereading them and
resume TLS sessions.

MySQLdb-style `ssl={"ca": ..., "cert": ..., "key": ...}` works too (`ssl={}`
means `required`).

!!! tip "Across a network you do not control"

    `preferred` stops passive eavesdropping, but it does not check the
    certificate and falls back to plain text when TLS is not offered: someone
    who can intercept the connection can pretend to be the server. Use
    `ssl_mode="verify_identity"`. See [In production](production.md).

## Authentication methods

All of them are implemented in Rust:

| Server | Methods |
|---|---|
| MySQL | `caching_sha2_password` (the default), `sha256_password`, `mysql_native_password` |
| MariaDB | `mysql_native_password`, `client_ed25519`, `parsec` |
| Both | `mysql_clear_password`, for PAM, LDAP and cloud IAM tokens |

## PAM, LDAP and cloud IAM tokens

Accounts whose server plugin needs the password itself (PAM, LDAP, and the
IAM tokens of AWS RDS and Aurora, Cloud SQL or Azure) log in with
`mysql_clear_password`. rustmy sends it **only over TLS or a Unix socket**:
over plain TCP the connection fails with `OperationalError` 2061 before the
password is sent, so no option has to enable it.

With RDS, for example, the token is the password and AWS's CA bundle verifies
the server:

```python
import boto3

token = boto3.client("rds").generate_db_auth_token(
    DBHostname="db.example.com", Port=3306, DBUsername="app", Region="eu-west-1"
)
conn = rustmy.connect(
    host="db.example.com", user="app", password=token, ssl_ca="global-bundle.pem"
)
```

## RSA keys over plain TCP

`caching_sha2_password` and `sha256_password` need the password itself for a
full authentication (for `caching_sha2_password`, when the server has not
cached the account yet: its first login since the server started, for
example). Over TLS or a Unix socket it goes as it is;
over plain TCP, encrypted with the server's RSA key.

`server_public_key="server-key.pem"`
:   Pins the key: the password is then only ever encrypted with it, and any
    key the server sends is refused.

`get_server_public_key`
:   On by default, as in PyMySQL: rustmy asks the server for its key, so those
    accounts work over plain TCP. `False` never asks for it, as libmysqlclient
    does by default: those accounts then connect only over TLS, a Unix socket
    or with a pinned key, and fail with `OperationalError` 2061 otherwise.

Both also work in option files (`server-public-key-path`,
`get-server-public-key`) and SQLAlchemy URLs
(`?get_server_public_key=false`).

## MariaDB's parsec

`parsec` derives its key with as many PBKDF2 rounds as the server asks for,
bounded as MariaDB Connector/C bounds them: what fits in `connect_timeout`.
