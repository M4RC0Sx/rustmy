"""The PEP 249 compliance suite (``tests/dbapi20.py``) against each server.

``dbapi20.py`` is Stuart Bishop's DB-API 2.0 compliance suite, public domain,
kept byte for byte as published (header included) so it can be compared with
its source:

- source: https://github.com/baztian/dbapi-compliance (the maintained copy of
  the original at https://launchpad.net/dbapi-compliance)
- commit: ea7cb1b48894a2858e79cc6709105f9f1b3948d1 (2022-10-24): release
  1.15.0 plus the fix for connections left open (not in the v1.15.0 tag or
  the PyPI package)
- sha256: 9d58f464d96d93e9b2b1442034711160203b0d0ca51459c2804f0cee48ca8ce9

The suite leaves a few tests for the driver to write, overridden below; any
other failure is a driver bug, never a reason to override a test.
"""

from __future__ import annotations

import hashlib

from pathlib import Path
from typing import Any
from typing import ClassVar

import pytest

import rustmy

from tests import dbapi20
from tests.conftest import resolve_server


SHA256 = "9d58f464d96d93e9b2b1442034711160203b0d0ca51459c2804f0cee48ca8ce9"

LOWER = "dbapi20test_lower"
TWO_RESULTS = "dbapi20test_two_results"


def test_suite_is_the_published_file() -> None:
    data = Path(dbapi20.__file__).read_bytes()
    assert hashlib.sha256(data).hexdigest() == SHA256


class _Compliance(dbapi20.DatabaseAPI20Test):  # type: ignore[misc]
    __test__ = False  # only the per-server subclasses run
    server_name: ClassVar[str]
    #: Every cursor the suite creates is of this class (``connect()``'s
    #: ``cursorclass``): the suite runs again with prepared statements.
    cursorclass: ClassVar[type[rustmy.Cursor]] = rustmy.Cursor
    driver = rustmy
    connect_kw_args: ClassVar[dict[str, Any]] = {}
    # `test_callproc` calls a procedure that lowercases its argument; LOWER
    # itself is a function, which CALL cannot run.
    lower_func = LOWER

    @classmethod
    def setUpClass(cls) -> None:
        server = resolve_server(cls.server_name)
        cls.connect_kw_args = {
            "host": server.host,
            "port": server.port,
            "user": "rustmy",
            "password": "rustmy",
            "database": "rustmy_test",
            "cursorclass": cls.cursorclass,
        }
        with rustmy.connect(**cls.connect_kw_args) as conn:
            cursor = conn.cursor()
            cursor.execute(f"DROP PROCEDURE IF EXISTS {LOWER}")
            cursor.execute(
                f"CREATE PROCEDURE {LOWER}(IN s VARCHAR(20)) SELECT LOWER(s)"
            )

    @classmethod
    def tearDownClass(cls) -> None:
        with rustmy.connect(**cls.connect_kw_args) as conn:
            conn.cursor().execute(f"DROP PROCEDURE IF EXISTS {LOWER}")

    def help_nextset_setUp(self, cur: rustmy.Cursor) -> None:  # noqa: N802
        cur.execute(f"DROP PROCEDURE IF EXISTS {TWO_RESULTS}")
        cur.execute(
            f"CREATE PROCEDURE {TWO_RESULTS}() BEGIN "
            f"SELECT COUNT(*) FROM {self.table_prefix}booze; "
            f"SELECT name FROM {self.table_prefix}booze; "
            "END"
        )

    def help_nextset_tearDown(self, cur: rustmy.Cursor) -> None:  # noqa: N802
        cur.execute(f"DROP PROCEDURE IF EXISTS {TWO_RESULTS}")

    def test_nextset(self) -> None:
        # The suite's own version is replaced by a NotImplementedError stub
        # for drivers to fill in. This follows its intended body, with one
        # addition: in MySQL every CALL ends with an empty result of its
        # own (the CALL's status), so there are three result sets, not two.
        with rustmy.connect(**self.connect_kw_args) as con:
            cur = con.cursor()
            self.executeDDL1(cur)
            for sql in self._populate():
                cur.execute(sql)
            self.help_nextset_setUp(cur)
            try:
                cur.callproc(TWO_RESULTS)
                assert cur.fetchone() == (len(self.samples),)
                assert cur.nextset()
                assert len(cur.fetchall()) == len(self.samples)
                assert cur.nextset()  # the CALL's own status
                assert cur.description is None
                assert cur.nextset() is None
            finally:
                self.help_nextset_tearDown(cur)

    def test_setoutputsize(self) -> None:
        # Also left to drivers. setoutputsize is a no-op here (PEP 249
        # allows it): rows are always read whole.
        with rustmy.connect(**self.connect_kw_args) as con:
            cur = con.cursor()
            cur.setoutputsize(1000)
            cur.setoutputsize(2000, 0)
            self.executeDDL1(cur)
            cur.execute(
                f"INSERT INTO {self.table_prefix}booze VALUES (%s)", ("x" * 20,)
            )
            cur.execute(f"SELECT name FROM {self.table_prefix}booze")
            assert cur.fetchall() == (("x" * 20,),)


@pytest.mark.integration
class TestMySQL(_Compliance):
    __test__ = True
    server_name = "mysql"


@pytest.mark.integration
class TestMariaDB(_Compliance):
    __test__ = True
    server_name = "mariadb"


@pytest.mark.integration
class TestMySQLPrepared(_Compliance):
    __test__ = True
    server_name = "mysql"
    cursorclass = rustmy.cursors.PreparedCursor


@pytest.mark.integration
class TestMariaDBPrepared(_Compliance):
    __test__ = True
    server_name = "mariadb"
    cursorclass = rustmy.cursors.PreparedCursor
