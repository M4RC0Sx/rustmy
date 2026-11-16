"""Resolution of the TLS arguments of ``connect()`` into one ``ssl_mode``.

Rules, in order:

1. An explicit ``ssl_mode`` always wins.
2. ``ssl`` (a dict, as accepted by MySQLdb/PyMySQL, or a bool) implies
   ``verify_identity`` when it names a CA (``verify_ca`` if
   ``check_hostname`` is false), ``required`` otherwise; ``ssl=False``
   means ``disabled``.
3. ``ssl_ca`` alone implies ``verify_identity``: giving a CA means wanting
   verification. A client certificate alone implies ``required``.
4. Nothing at all means ``preferred``.

Contradictions raise instead of being silently ignored.
"""

from __future__ import annotations

import os

from dataclasses import dataclass
from typing import TYPE_CHECKING

from rustmy.err import ProgrammingError


if TYPE_CHECKING:
    from collections.abc import Mapping

    from rustmy.connections import PathArg


MODES = ("disabled", "preferred", "required", "verify_ca", "verify_identity")
_DICT_KEYS = frozenset({"ca", "cert", "key", "check_hostname"})


@dataclass(frozen=True)
class TlsSettings:
    mode: str
    ca: str | None = None
    cert: str | None = None
    key: str | None = None


def _path(value: PathArg | None) -> str | None:
    return None if value is None else os.fspath(value)


def _from_dict(
    ssl: Mapping[str, object],
) -> tuple[str | None, str | None, str | None, bool]:
    unknown = set(ssl) - _DICT_KEYS
    if unknown:
        msg = (
            f"unsupported ssl option(s) {sorted(unknown)}; supported keys are "
            f"{sorted(_DICT_KEYS)}. To enable TLS without certificate checks "
            "use ssl_mode='required'."
        )
        raise ProgrammingError(0, msg)
    paths: list[str | None] = []
    for name in ("ca", "cert", "key"):
        value = ssl.get(name)
        if value is not None and not isinstance(value, (str, os.PathLike)):
            msg = f"ssl[{name!r}] must be a path, got {type(value).__name__}"
            raise ProgrammingError(0, msg)
        paths.append(None if value is None else os.fspath(value))
    ca, cert, key = paths
    return ca, cert, key, bool(ssl.get("check_hostname", True))


def _merge(name: str, explicit: str | None, from_dict: str | None) -> str | None:
    if explicit is not None and from_dict is not None and explicit != from_dict:
        msg = f"ssl_{name} and ssl[{name!r}] disagree; pass only one of them"
        raise ProgrammingError(0, msg)
    return explicit if explicit is not None else from_dict


def resolve(  # noqa: C901
    *,
    ssl_mode: str | None,
    ssl_ca: PathArg | None,
    ssl_cert: PathArg | None,
    ssl_key: PathArg | None,
    ssl: Mapping[str, object] | bool | None,
) -> TlsSettings:
    ca, cert, key = _path(ssl_ca), _path(ssl_cert), _path(ssl_key)
    implied: str | None = None

    if isinstance(ssl, bool):
        implied = "required" if ssl else "disabled"
    elif ssl is not None:
        dict_ca, dict_cert, dict_key, check_hostname = _from_dict(ssl)
        ca = _merge("ca", ca, dict_ca)
        cert = _merge("cert", cert, dict_cert)
        key = _merge("key", key, dict_key)
        if ca is None:
            implied = "required"
        else:
            implied = "verify_identity" if check_hostname else "verify_ca"
    elif ca is not None:
        implied = "verify_identity"
    elif cert is not None or key is not None:
        implied = "required"

    if ssl_mode is None:
        mode = implied or "preferred"
    else:
        mode = ssl_mode.lower()
        if mode not in MODES:
            msg = f"invalid ssl_mode {ssl_mode!r}; expected one of {', '.join(MODES)}"
            raise ProgrammingError(0, msg)

    if (cert is None) != (key is None):
        msg = "a client certificate needs both ssl_cert and ssl_key"
        raise ProgrammingError(0, msg)
    if mode == "disabled" and (ca or cert):
        msg = "ssl_mode='disabled' contradicts the certificate options given"
        raise ProgrammingError(0, msg)
    if ca is not None and mode in {"preferred", "required"}:
        msg = (
            f"ssl_ca is only used to verify the server; with ssl_mode={mode!r} "
            "nothing is verified. Use 'verify_identity' (or 'verify_ca'), or "
            "drop ssl_ca."
        )
        raise ProgrammingError(0, msg)
    return TlsSettings(mode=mode, ca=ca, cert=cert, key=key)
