import sys
import sysconfig

from importlib.metadata import version

import pytest

import rustmy

from rustmy import _core


def test_version_matches_distribution_metadata() -> None:
    assert rustmy.__version__ == version("rustmy")


def test_pep249_globals() -> None:
    assert rustmy.apilevel == "2.0"
    assert rustmy.threadsafety == 1
    assert rustmy.paramstyle == "format"


def test_native_module_links_proto_crate() -> None:
    assert _core.MAX_PACKET_PAYLOAD == (1 << 24) - 1


@pytest.mark.skipif(
    not sysconfig.get_config_var("Py_GIL_DISABLED"),
    reason="requires a free-threaded CPython build",
)
def test_import_keeps_gil_disabled() -> None:
    # CPython silently re-enables the GIL when importing an extension that
    # does not declare free-threading support (`gil_used = false` in PyO3).
    if sys.version_info >= (3, 13):
        # Documented public API despite the underscore (CPython 3.13+).
        assert not sys._is_gil_enabled()  # noqa: SLF001
