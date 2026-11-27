"""A griffe extension for the API reference (zensical.toml).

rustmy's docstrings use Sphinx's conventions, which mkdocstrings does not
read: ``:class:`~rustmy.err.Error``` roles, and ``#:`` comments documenting
attributes. This turns the roles into autorefs cross-references
(``[`Error`][rustmy.err.Error]``) and the comments into docstrings.
"""

from __future__ import annotations

import re

from typing import Any

import griffe


ROLE = re.compile(r":(?:py:)?(class|func|meth|attr|mod|data|exc|obj):`([^`]+)`")
PEP = re.compile(r":pep:`(\d+)`")

#: Names the reference documents under another path.
RENAMED = {"rustmy.connect": "rustmy.connections.Connection"}


def _resolve(target: str, obj: griffe.Object) -> str | None:
    """The path ``target`` names from ``obj``'s docstring, or ``None`` for
    a private object, which the reference does not document."""
    target = RENAMED.get(target, target)
    if "." not in target and target in _exceptions(obj):
        # Connections carry the exceptions too, but undocumented there.
        return f"rustmy.err.{target}"
    if "." not in target:
        # A member of the class the docstring belongs to, inherited or not.
        cls = obj if obj.is_class else obj.parent
        if cls is not None and cls.is_class and target in cls.all_members:
            return f"{cls.path}.{target}"
    if any(part.startswith("_") for part in target.split(".")):
        return None
    return target


def _exceptions(obj: griffe.Object) -> set[str]:
    """The names ``rustmy.err`` exports."""
    err = obj.package.members.get("err")
    exports = getattr(err, "exports", None)
    return set(exports or ())


def convert(text: str, obj: griffe.Object) -> str:
    """The docstring with its Sphinx roles as Markdown cross-references."""

    def crossref(match: re.Match[str]) -> str:
        kind, target = match.group(1), re.sub(r"\s+", "", match.group(2))
        short = target.startswith("~")
        target = target.lstrip("~.")
        text = target.rsplit(".", 1)[-1] if short else target
        if kind in {"func", "meth"}:
            text += "()"
        path = _resolve(target, obj)
        return f"`{text}`" if path is None else f"[`{text}`][{path}]"

    def pep(match: re.Match[str]) -> str:
        number = int(match.group(1))
        return f"[PEP {number}](https://peps.python.org/pep-{number:04d}/)"

    return PEP.sub(pep, ROLE.sub(crossref, text))


class SphinxRoles(griffe.Extension):
    """Sphinx roles and ``#:`` attribute comments, read as mkdocstrings
    expects them."""

    def on_package(self, *, pkg: griffe.Module, **kwargs: Any) -> None:  # noqa: ARG002
        # Once the whole package is loaded, so roles can name any member.
        for obj in _walk(pkg):
            if obj.is_attribute and obj.docstring is None:
                _comment_docstring(obj)
            if obj.docstring is not None:
                obj.docstring.value = convert(obj.docstring.value, obj)


def _walk(obj: griffe.Object) -> list[griffe.Object]:
    found = [obj]
    for member in obj.members.values():
        if not member.is_alias:
            found.extend(_walk(member))  # type: ignore[arg-type]
    return found


def _comment_docstring(attr: griffe.Object) -> None:
    """The ``#:`` comment lines right above ``attr``, as its docstring."""
    if attr.lineno is None or isinstance(attr.filepath, list):
        return
    if attr.filepath not in attr.lines_collection:
        return
    lines = attr.lines_collection[attr.filepath]
    comment: list[str] = []
    index = attr.lineno - 2
    while index >= 0 and lines[index].lstrip().startswith("#:"):
        comment.insert(0, lines[index].lstrip()[2:].strip())
        index -= 1
    if comment:
        attr.docstring = griffe.Docstring(
            " ".join(comment), lineno=index + 2, parent=attr
        )
