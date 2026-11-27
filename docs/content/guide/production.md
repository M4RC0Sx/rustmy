---
description: "Running rustmy in production: verified TLS, timeouts, pooling and naming the application."
---

# In production

A checklist for services that talk to MySQL or MariaDB across a network.

## Verify the server

The default `ssl_mode="preferred"` encrypts whenever the server offers TLS,
but it does not check the certificate and falls back to plain text when TLS
is not offered: it stops passive eavesdropping, not someone who can intercept
the connection and pretend to be the server. Across any network you do not
control, use `ssl_mode="verify_identity"` (the system's CAs, or `ssl_ca=`
for a private one). See [TLS and authentication](tls.md).

## Set timeouts

`connect_timeout` is 15 seconds by default; `read_timeout` and
`write_timeout` (seconds, fractions allowed) are off by default, as in
mysqlclient, so a slow query is never cut short. Set `read_timeout` to bound
any single read.

TCP keepalive probes start after 60 seconds idle: a pooled connection that a
firewall or load balancer dropped fails within a few minutes instead of
blocking a read for as long as the operating system retries, and the probes
keep idle connections from being dropped in the first place.

## Pool connections

Each thread or task uses its own connection, which is what pools hand out:

- rustmy's own pools (see [Connection pools](pools.md));
- SQLAlchemy's (`pool_pre_ping=True`, and `pool_recycle` below the server's
  `wait_timeout`);
- Django's `CONN_MAX_AGE` with `CONN_HEALTH_CHECKS`.

## Name the application

Every connection tells the server who opened it (`_client_name=rustmy`,
`_client_version`, `_os`, `_platform`, `_pid`); `program_name` and
`conn_attrs` add your own, so a DBA can see which service holds each
connection:

```python
conn = rustmy.connect(
    ..., program_name="billing-worker", conn_attrs={"team": "payments"}
)
```

```sql
SELECT PROCESSLIST_ID, ATTR_NAME, ATTR_VALUE
FROM performance_schema.session_connect_attrs;
```

MySQL fills that table by default; MariaDB only with
`performance_schema=ON`. In a SQLAlchemy URL, `?program_name=billing-worker`.

## A complete configuration

=== "SQLAlchemy"

    ```python
    from sqlalchemy import create_engine

    engine = create_engine(
        "mysql+rustmy://app:secret@db.internal/shop"
        "?ssl_mode=verify_identity&connect_timeout=5&read_timeout=30"
        "&program_name=shop-api",
        pool_size=10,
        pool_pre_ping=True,
        pool_recycle=3600,
    )
    ```

=== "rustmy.aio"

    ```python
    from rustmy import aio

    pool = await aio.create_pool(
        minsize=2,
        maxsize=20,
        host="db.internal",
        user="app",
        password="secret",
        database="shop",
        ssl_mode="verify_identity",
        connect_timeout=5,
        read_timeout=30,
        program_name="shop-api",
    )
    ```

The [FastAPI](../integrations/fastapi.md) page shows both inside an
application.
