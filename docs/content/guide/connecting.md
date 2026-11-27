---
description: "Opening a connection with rustmy.connect(): arguments, timeouts, Unix sockets, option files, sessions and connection attributes."
---

# Connecting

```python
import rustmy

conn = rustmy.connect(
    host="localhost",
    port=3306,
    user="app",
    password="secret",
    database="shop",
    charset="utf8mb4",  # the default
    connect_timeout=5,
    read_timeout=30,
)
print(conn.get_server_info())
conn.close()
```

A connection is also a context manager: `with rustmy.connect(...) as conn:`
closes it at the end of the block. The arguments keep MySQLdb's positional
order (`host`, `user`, `password`, `database`, `port`, `unix_socket`), and
`passwd` and `db` are accepted as aliases of `password` and `database`.
Every argument is listed in the [API reference](../reference/connection.md).

## Unix sockets

```python
conn = rustmy.connect(unix_socket="/run/mysqld/mysqld.sock", user="app")
```

A Unix socket counts as a secure transport, as TLS does: authentication
methods that need the password itself use it there (see
[TLS and authentication](tls.md)).

## Timeouts

`connect_timeout`
:   15 seconds unless given; `0` waits forever. It covers the whole
    connection, authentication included, so a server that accepts the
    connection but never answers fails too.

`read_timeout`, `write_timeout`
:   Off by default, as in mysqlclient, so a slow query is never cut short.
    In seconds, fractions allowed; they apply once the connection is open.

TCP connections send keepalive probes after 60 seconds idle, so a connection
that a firewall or load balancer dropped while it sat in a pool fails within
minutes instead of hanging.

## Transactions and autocommit

`autocommit` is off by default, as PEP 249 asks: call `conn.commit()` to keep
your changes, `conn.rollback()` to drop them, or pass `autocommit=True`.
`autocommit=None` keeps the server's default, and `conn.autocommit(True)`
switches it later.

## Session settings

`init_command`
:   An SQL statement run right after connecting (and again after
    `reset_connection()` and `change_user()`).

`sql_mode`
:   The session's SQL mode.

`charset`, `collation`
:   The session's character set and collation (see
    [Character sets](charsets.md)).

The session settings of `connect()` (`SET NAMES`, `sql_mode`, `autocommit`)
go in a single statement, before `init_command`.

## Option files

Settings can also come from a MySQL option file, as with the `mysql` client:

```python
conn = rustmy.connect(read_default_file="~/.my.cnf", read_default_group="shop")
```

The `[client]` group and the one you name are read (with `!include` and
`!includedir`), and arguments passed to `connect()` win over the file.
`read_default_group` alone reads the default files (`/etc/my.cnf`,
`~/.my.cnf`…). Login paths (`.mylogin.cnf`) are not read. As in the `mysql`
client, a file anyone can write to is ignored with a warning (outside
Windows).

??? info "Options read from the file"

    `host`, `port`, `user`, `password`, `database`, `socket`,
    `default-character-set`, `init-command`, `connect-timeout`, `ssl-mode`,
    `ssl-ca`, `ssl-cert`, `ssl-key`, `server-public-key-path`,
    `get-server-public-key`, `multi-statements`, `return-found-rows`,
    `compress`, `compression-algorithms`, `zstd-compression-level`,
    `local-infile` and `load-data-local-dir`. Other options are ignored.

## Naming the application

Every connection tells the server who opened it, in the connection
attributes: `_client_name=rustmy`, `_client_version`, `_os`, `_platform` and
`_pid`. Add your own so a DBA can see which service holds each connection:

```python
conn = rustmy.connect(
    host="db.internal",
    user="app",
    program_name="billing-worker",
    conn_attrs={"team": "payments", "region": "eu-west-1"},
)
```

```sql
SELECT PROCESSLIST_ID, ATTR_NAME, ATTR_VALUE
FROM performance_schema.session_connect_attrs;
```

MySQL fills that table by default; MariaDB only with
`performance_schema=ON`. The attributes are sent again by `change_user()`.
Names starting with `_` are reserved for the driver's own (`ValueError`), and
servers accept at most 64 KiB of them.

## Working with a session

```python
conn.ping()  # raises if the server is gone
conn.select_db("analytics")  # change the default database
conn.reset_connection()  # roll back, drop temporary tables and variables
conn.change_user("report", "secret", "analytics")  # log in as another account
print(conn.thread_id(), conn.tls, conn.compression)
```

`reset_connection()` returns the session to its just-connected state without
logging in again, then applies `init_command` and the `autocommit` mode once
more: what a pool needs between borrowers. `change_user()` does the same after
authenticating as another account.

## Closing

`conn.close()` closes the connection. Closing it a second time raises
`InterfaceError`, as PEP 249 asks of any use of a closed connection; leaving a
`with` block after an explicit `close()` does not raise, and a connection that
broke (server gone, timeout) can still be closed.
