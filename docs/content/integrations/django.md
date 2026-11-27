---
description: "Using rustmy with Django's MySQL backend through install_as_MySQLdb()."
---

# Django

Django's MySQL backend imports mysqlclient's module, `MySQLdb`. Make it load
rustmy at the top of `settings.py` (or anywhere that runs before Django
connects), and configure the database as usual:

```python title="settings.py"
import rustmy

rustmy.install_as_MySQLdb()

DATABASES = {
    "default": {
        "ENGINE": "django.db.backends.mysql",
        "NAME": "shop",
        "USER": "app",
        "PASSWORD": "secret",
        "HOST": "localhost",
        "OPTIONS": {"charset": "utf8mb4"},
    }
}
```

`OPTIONS` are passed to `rustmy.connect()`, so `ssl_mode`, `ssl_ca`,
`read_default_file`, `program_name` and the rest work there.

Django 5.2 LTS and 6.x are supported. Django's whole test suite (about 19,700
tests) passes as it does with mysqlclient, on MySQL and MariaDB, but for one
test that pins the types mysqlclient's `escape()` returns. CI runs it with
rustmy and with mysqlclient on the same servers and fails on any test that
fails with rustmy only.

## How the alias works

- `MySQLdb` and its submodules (`MySQLdb.constants`, `MySQLdb.converters`,
  `MySQLdb.cursors`, `MySQLdb.connections`) are registered as the very same
  rustmy module objects, so the native fast path stays.
- The alias reports itself as mysqlclient 2.2.1, the oldest version Django
  6.x accepts; rustmy's own version stays in `rustmy.__version__`.
- Its `connect()` leaves `bytes` without the `_binary` prefix, as mysqlclient
  does, since Django writes `_binary %s` for binary fields itself.
- A numeric value out of range (error 1690) raises an exception that is both
  a `DataError` and an `OperationalError`, which Django turns into an
  `IntegrityError`, as with mysqlclient.

## In production

Use `CONN_MAX_AGE` with `CONN_HEALTH_CHECKS` to keep connections open across
requests, and `"OPTIONS": {"ssl_mode": "verify_identity"}` across a network
you do not control. See [In production](../guide/production.md).

!!! note "Prepared statements"

    Keep the default cursor: Django's MySQL backend writes SQL that MySQL
    refuses to prepare (`->> %s` for `JSONField`, `_binary %s` for
    `BinaryField`, parameters in DDL).
