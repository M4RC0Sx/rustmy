from importlib.metadata import version

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
