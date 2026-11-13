"""rustmy: a fast, dependency-free DB-API 2.0 driver for MySQL/MariaDB."""

from rustmy._core import __version__


# PEP 249 module globals.
apilevel = "2.0"
# Threads may share the module but not connections.
threadsafety = 1
# `%s` and `%(name)s` placeholders, as expected by SQLAlchemy's MySQL dialects.
paramstyle = "format"

__all__ = [
    "__version__",
    "apilevel",
    "paramstyle",
    "threadsafety",
]
