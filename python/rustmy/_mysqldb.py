"""``rustmy.install_as_MySQLdb()``: make ``import MySQLdb`` load rustmy.

For code written against mysqlclient's module name, Django's MySQL backend
first of all, which imports ``MySQLdb`` directly. The alias is a module of
its own: rustmy's public names plus the submodules that code imports
(``MySQLdb.constants`` and each constant module, ``MySQLdb.converters``,
``MySQLdb.cursors``, ``MySQLdb.connections``), registered as the very same
module objects, so ``MySQLdb.converters.conversions`` is
``rustmy.converters.conversions`` and keeps the native fast path.

The alias reports a mysqlclient compatibility version (``version_info``,
``__version__``): Django refuses mysqlclient older than 2.2.1. rustmy's own
version stays in ``rustmy.__version__``. Its ``connect()`` defaults to
``binary_prefix=False``, as mysqlclient's does: Django writes ``_binary %s``
for binary values itself.
"""

from __future__ import annotations

import sys
import types

from typing import Any

import rustmy

from rustmy import connections
from rustmy import constants
from rustmy import converters
from rustmy import cursors
from rustmy import err
from rustmy.constants import CLIENT
from rustmy.constants import CR
from rustmy.constants import ER
from rustmy.constants import FIELD_TYPE
from rustmy.constants import FLAG
from rustmy.constants import SERVER_STATUS


#: The mysqlclient version the alias reports.
MYSQLCLIENT_VERSION_INFO = (2, 2, 1, "final", 0)


def connect(*args: Any, **kwargs: Any) -> rustmy.Connection:
    """:func:`rustmy.connect` with mysqlclient's ``binary_prefix=False``
    default."""
    kwargs.setdefault("binary_prefix", False)
    return rustmy.connect(*args, **kwargs)


def install_as_MySQLdb() -> None:  # noqa: N802  # the name other drivers use
    """Register rustmy as ``MySQLdb`` (and its submodules) in
    ``sys.modules``, replacing any module already there under those names.

    Call it before the code that imports ``MySQLdb`` runs, e.g. at the top of
    Django's ``settings.py``.
    """
    alias = types.ModuleType(
        "MySQLdb", "rustmy, installed as MySQLdb by rustmy.install_as_MySQLdb()."
    )
    for name in rustmy.__all__:
        setattr(alias, name, getattr(rustmy, name))
    major, minor, micro, _, _ = MYSQLCLIENT_VERSION_INFO
    vars(alias).update(
        version_info=MYSQLCLIENT_VERSION_INFO,
        __version__=f"{major}.{minor}.{micro}",
        connect=connect,
        Connect=connect,
    )
    submodules: dict[str, types.ModuleType] = {
        "constants": constants,
        "constants.CLIENT": CLIENT,
        "constants.CR": CR,
        "constants.ER": ER,
        "constants.FIELD_TYPE": FIELD_TYPE,
        "constants.FLAG": FLAG,
        "constants.SERVER_STATUS": SERVER_STATUS,
        "converters": converters,
        "cursors": cursors,
        "connections": connections,
        "_exceptions": err,
    }
    for name, module in submodules.items():
        if "." not in name:
            setattr(alias, name, module)
        sys.modules[f"MySQLdb.{name}"] = module
    sys.modules["MySQLdb"] = alias
