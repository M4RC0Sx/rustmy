"""SQLAlchemy's dialect compliance suite against rustmy.

Two suite classes are replaced (same name, so only these are collected):

- ``ServerSideCursorsTest``: the suite recognises server-side cursors by
  driver name only ("a huge issue [...] for third party dialects", in its
  own words); this tells it what rustmy's are.
- ``HasTableTest``: on MariaDB, an account without global privileges gets
  "access denied" (1142) for a table in a schema that does not exist, which
  SQLAlchemy does not read as "no such table". PyMySQL fails the same way;
  as root the test passes.
"""

from __future__ import annotations

from typing import Any

from sqlalchemy.testing import exclusions
from sqlalchemy.testing.suite import *
from sqlalchemy.testing.suite import HasTableTest as _HasTableTest
from sqlalchemy.testing.suite import ServerSideCursorsTest as _ServerSideCursorsTest

from rustmy.cursors import SSCursor


class ServerSideCursorsTest(_ServerSideCursorsTest):
    def _is_server_side(self, cursor: Any) -> bool:
        # The asyncio dialects' cursors are SQLAlchemy's adapters, which say
        # so (as asyncmy's do).
        return isinstance(cursor, SSCursor) or getattr(cursor, "server_side", False)


class HasTableTest(_HasTableTest):
    @exclusions.skip_if("mariadb", "1142 instead of 'no such table' (see above)")
    def test_has_table_nonexistent_schema(self) -> None:
        super().test_has_table_nonexistent_schema()
