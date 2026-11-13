"""Render benchmark results (``spike.py --json``) as charts and README tables.

    uv run --no-sync python benches/report.py \\
        benches/results/phase0-py310.json benches/results/phase0-py314.json

Writes ``<output>-light.svg`` and ``<output>-dark.svg`` (one panel per input
file) and rewrites the block between the ``results:start`` / ``results:end``
markers of ``benches/README.md`` with the charts and one table per input.
"""

from __future__ import annotations

import argparse
import json
import re

from dataclasses import dataclass
from pathlib import Path
from typing import Any

import matplotlib as mpl


mpl.use("svg")

import matplotlib.pyplot as plt  # backend must be set first

from matplotlib.patches import Patch


HERE = Path(__file__).parent
README = HERE / "README.md"
MARKER = re.compile(
    r"(<!-- results:start -->\n).*?(<!-- results:end -->)", flags=re.DOTALL
)

DRIVERS = ("rustmy", "mysqlclient", "pymysql")
LABELS = {"rustmy": "rustmy", "mysqlclient": "mysqlclient", "pymysql": "PyMySQL"}


@dataclass(frozen=True)
class Theme:
    """Reference categorical palette, first three slots, validated for CVD."""

    name: str
    surface: str
    text: str
    text_secondary: str
    grid: str
    series: dict[str, str]


LIGHT = Theme(
    "light",
    surface="#fcfcfb",
    text="#0b0b0b",
    text_secondary="#52514e",
    grid="#e4e3df",
    series={"rustmy": "#2a78d6", "mysqlclient": "#eb6834", "pymysql": "#1baf7a"},
)
DARK = Theme(
    "dark",
    surface="#1a1a19",
    text="#f0efec",
    text_secondary="#c3c2b7",
    grid="#3a3a37",
    series={"rustmy": "#3987e5", "mysqlclient": "#d95926", "pymysql": "#199e70"},
)


def load(path: Path) -> dict[str, Any]:
    data: dict[str, Any] = json.loads(path.read_text())
    return data


def speedups(run: dict[str, Any]) -> list[tuple[str, dict[str, float]]]:
    """Throughput relative to mysqlclient per comparable scenario."""
    rows = []
    for scenario in run["scenarios"]:
        if scenario["informative"]:
            continue
        seconds = scenario["seconds"]
        base = seconds["mysqlclient"]
        rows.append((scenario["name"], {d: base / seconds[d] for d in DRIVERS}))
    return rows


def plot(runs: list[dict[str, Any]], theme: Theme, path: Path) -> None:
    panels = [speedups(run) for run in runs]
    longest = max(value for rows in panels for _, v in rows for value in v.values())
    bar = 0.26
    height = 1.0 + 0.55 * len(panels[0])
    fig, axes = plt.subplots(
        1,
        len(runs),
        figsize=(5.2 * len(runs) + 1.6, height + 1.3),
        sharey=True,
        squeeze=False,
    )
    fig.patch.set_facecolor(theme.surface)

    for ax, run, rows in zip(axes[0], runs, panels, strict=True):
        ax.set_facecolor(theme.surface)
        for i, (_, values) in enumerate(rows):
            for j, driver in enumerate(DRIVERS):
                y = i + (j - 1) * bar
                value = values[driver]
                ax.barh(
                    y,
                    value,
                    height=bar,
                    color=theme.series[driver],
                    # Surface-coloured edge = the 2px gap between adjacent bars.
                    edgecolor=theme.surface,
                    linewidth=1.5,
                )
                if driver != "mysqlclient":
                    ax.text(
                        value + longest * 0.012,
                        y,
                        f"{value:.2f}x",
                        va="center",
                        fontsize=8,
                        color=theme.text_secondary,
                        # Surface backing keeps labels readable over the 1.0 line.
                        bbox={
                            "boxstyle": "square,pad=0.15",
                            "facecolor": theme.surface,
                            "edgecolor": "none",
                        },
                        zorder=3,
                    )
        ax.axvline(
            1,
            color=theme.text_secondary,
            linewidth=1,
            linestyle=(0, (3, 3)),
            zorder=2,
        )
        ax.set_xlim(0, longest * 1.15)
        ax.set_title(run["python"], color=theme.text, fontsize=11, loc="left")
        ax.set_xlabel(
            "throughput relative to mysqlclient (higher is faster)",
            color=theme.text_secondary,
            fontsize=9,
        )
        ax.xaxis.grid(visible=True, color=theme.grid, linewidth=0.8)
        ax.set_axisbelow(True)
        ax.tick_params(colors=theme.text_secondary, labelsize=9, length=0)
        for side in ("top", "right", "left"):
            ax.spines[side].set_visible(False)
        ax.spines["bottom"].set_color(theme.grid)

    # Wrap the parenthesised detail onto its own line to keep labels narrow.
    names = [name.replace(" (", "\n(") for name, _ in panels[0]]
    first = axes[0][0]
    first.set_yticks(range(len(names)), names, color=theme.text, fontsize=9)
    first.invert_yaxis()

    handles = [Patch(color=theme.series[d], label=LABELS[d]) for d in DRIVERS]
    fig.suptitle(
        "rustmy vs mysqlclient vs PyMySQL: throughput relative to mysqlclient",
        x=0.01,
        y=0.995,
        ha="left",
        color=theme.text,
        fontsize=12,
    )
    fig.legend(
        handles=handles,
        loc="upper left",
        ncols=len(DRIVERS),
        frameon=False,
        labelcolor=theme.text,
        fontsize=9,
        bbox_to_anchor=(0.005, 0.955),
    )
    fig.tight_layout(rect=(0, 0, 1, 0.9))
    fig.savefig(path, facecolor=theme.surface, metadata={"Date": None})
    plt.close(fig)


def table(run: dict[str, Any]) -> str:
    header = (
        "| Scenario | rustmy | mysqlclient | PyMySQL "
        "| rustmy vs mysqlclient | rustmy vs PyMySQL |\n"
        "|---|--:|--:|--:|--:|--:|\n"
    )
    lines = []
    for scenario in run["scenarios"]:
        seconds = scenario["seconds"]
        fastest = min(seconds.values())
        cells = [
            f"**{seconds[d] * 1000:.1f} ms**"
            if seconds[d] == fastest
            else f"{seconds[d] * 1000:.1f} ms"
            for d in DRIVERS
        ]
        name = scenario["name"] + (" ¹" if scenario["informative"] else "")
        vs_mysqlclient = seconds["mysqlclient"] / seconds["rustmy"]
        vs_pymysql = seconds["pymysql"] / seconds["rustmy"]
        lines.append(
            f"| {name} | {' | '.join(cells)} "
            f"| {vs_mysqlclient:.2f}x | {vs_pymysql:.2f}x |"
        )
    drivers = run["drivers"]
    setup = (
        f"**{run['python']}** · {run['server']} · {run['machine']} · "
        f"rustmy {drivers['rustmy']}, mysqlclient {drivers['mysqlclient']}, "
        f"PyMySQL {drivers['pymysql']} · best of {run['repeat']} · {run['date']}"
    )
    return f"{setup}\n\n{header}" + "\n".join(lines) + "\n"


def readme_block(runs: list[dict[str, Any]], output: Path) -> str:
    rel = output.relative_to(HERE).as_posix()
    picture = (
        "<picture>\n"
        f'  <source media="(prefers-color-scheme: dark)" srcset="{rel}-dark.svg">\n'
        f'  <img alt="Throughput relative to mysqlclient per scenario" '
        f'src="{rel}-light.svg">\n'
        "</picture>\n"
    )
    tables = "\n".join(table(run) for run in runs)
    note = (
        "\nTimes per driver; the fastest is in bold. Ratios above 1 mean rustmy "
        "is faster.\n¹ Not comparable yet: rustmy runs one INSERT per row, "
        "mysqlclient and PyMySQL batch them into multi-row INSERTs (Phase 2). "
        "Left out of the chart.\n"
    )
    return f"{picture}\n{tables}{note}"


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("results", nargs="+", type=Path)
    parser.add_argument(
        "--output",
        type=Path,
        default=HERE / "results" / "phase0",
        help="path prefix of the SVG files",
    )
    args = parser.parse_args()
    output: Path = args.output.resolve()
    runs = [load(path) for path in args.results]

    plt.rcParams["svg.fonttype"] = "none"  # keep text as text
    plt.rcParams["svg.hashsalt"] = "rustmy"  # deterministic ids, stable diffs
    for theme in (LIGHT, DARK):
        plot(runs, theme, output.with_name(f"{output.name}-{theme.name}.svg"))

    readme = README.read_text()
    block = readme_block(runs, output)
    updated, count = MARKER.subn(lambda m: m.group(1) + block + m.group(2), readme)
    if count != 1:
        msg = f"expected one results block in {README}, found {count}"
        raise SystemExit(msg)
    README.write_text(updated)
    print(f"wrote {output.name}-light.svg, {output.name}-dark.svg and {README.name}")


if __name__ == "__main__":
    main()
