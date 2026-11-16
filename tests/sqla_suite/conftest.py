"""Loads SQLAlchemy's test plugin; the dialects come from rustmy's entry points."""

import pytest


pytest.register_assert_rewrite("sqlalchemy.testing.assertions")

from sqlalchemy.testing.plugin.pytestplugin import *  # noqa: E402
