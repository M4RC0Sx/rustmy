"""Render benchmark results (``spike.py --json``) as charts and README tables.

    uv run --no-sync python benches/report.py \\
        benches/results/phase0-py310.json benches/results/phase0-py314.json

Writes ``<output>-light.svg`` and ``<output>-dark.svg`` (one panel per input
file, or one panel per row count or scenario for an ``executemany.py`` or
``cursors.py --sweep-json`` sweep) and rewrites
the block between the ``results:SECTION:start`` / ``results:SECTION:end``
markers of ``benches/README.md`` (``--section``, default ``phase0``).
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
    # Panels of different network conditions (pipeline.py) differ by orders
    # of magnitude: each gets its own scale. Otherwise one scale compares
    # them.
    own_scale = all("title" in run for run in runs)
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
        if own_scale:
            longest = max(value for _, v in rows for value in v.values())
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
                    bar_label(ax, theme, value, y, longest)
        style_axis(ax, theme, run.get("title", run["python"]), longest)

    # Wrap the parenthesised detail onto its own line to keep labels narrow.
    names = [name.replace(" (", "\n(") for name, _ in panels[0]]
    first = axes[0][0]
    first.set_yticks(range(len(names)), names, color=theme.text, fontsize=9)
    first.invert_yaxis()
    finish(
        fig,
        theme,
        path,
        "rustmy vs mysqlclient vs PyMySQL: throughput relative to mysqlclient",
    )


def bar_label(ax: Any, theme: Theme, value: float, y: float, longest: float) -> None:
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


def style_axis(ax: Any, theme: Theme, title: str, longest: float) -> None:
    """Shared look: reference line at 1.0, recessive grid, no frame."""
    ax.axvline(
        1,
        color=theme.text_secondary,
        linewidth=1,
        linestyle=(0, (3, 3)),
        zorder=2,
    )
    ax.set_xlim(0, longest * 1.15)
    ax.set_title(title, color=theme.text, fontsize=11, loc="left")
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


def finish(
    fig: Any,
    theme: Theme,
    path: Path,
    title: str,
    drivers: tuple[str, ...] = DRIVERS,
) -> None:
    """Title, legend of ``drivers`` and save."""
    handles = [Patch(color=theme.series[d], label=LABELS[d]) for d in drivers]
    fig.suptitle(title, x=0.01, y=0.995, ha="left", color=theme.text, fontsize=12)
    fig.legend(
        handles=handles,
        loc="upper left",
        ncols=len(drivers),
        frameon=False,
        labelcolor=theme.text,
        fontsize=9,
        bbox_to_anchor=(0.005, 0.955),
    )
    fig.tight_layout(rect=(0, 0, 1, 0.9))
    fig.savefig(path, facecolor=theme.surface, metadata={"Date": None})
    plt.close(fig)


#: How the reference drivers appear in the executemany sweep (their default
#: batch size); other sweeps name them in their own ``labels``.
SWEEP_LABELS = {"mysqlclient": "mysqlclient (64 KiB)", "pymysql": "PyMySQL (~1 MB)"}


def sweep_label(config: str, run: dict[str, Any]) -> str:
    if config == run["default"]:
        return f"{config} (default)"
    labels: dict[str, str] = run.get("labels", SWEEP_LABELS)
    return labels.get(config, config)


def panel_title(panel: dict[str, Any]) -> str:
    if "label" in panel:
        label: str = panel["label"]
        return label
    return f"{panel['rows']:,} rows"


def driver_of(config: str) -> str:
    """`rustmy 256 KiB` -> `rustmy`."""
    return config.split(maxsplit=1)[0]


def plot_sweep(run: dict[str, Any], theme: Theme, path: Path) -> None:
    """executemany sweep: one panel per row count, one bar per configuration."""
    panels = run["sweep"]
    configs = list(panels[0]["seconds"])
    longest = max(
        p["seconds"]["mysqlclient"] / s for p in panels for s in p["seconds"].values()
    )
    fig, axes = plt.subplots(
        1,
        len(panels),
        figsize=(4.6 * len(panels) + 1.8, 1.4 + 0.42 * len(configs)),
        sharey=True,
        squeeze=False,
    )
    fig.patch.set_facecolor(theme.surface)
    for ax, panel in zip(axes[0], panels, strict=True):
        ax.set_facecolor(theme.surface)
        base = panel["seconds"]["mysqlclient"]
        for i, config in enumerate(configs):
            value = base / panel["seconds"][config]
            ax.barh(
                i,
                value,
                height=0.7,
                color=theme.series[driver_of(config)],
                edgecolor=theme.surface,
                linewidth=1.5,
            )
            if driver_of(config) != "mysqlclient":
                bar_label(ax, theme, value, i, longest)
        style_axis(ax, theme, panel_title(panel), longest)
    names = [sweep_label(c, run) for c in configs]
    first = axes[0][0]
    first.set_yticks(range(len(names)), names, color=theme.text, fontsize=9)
    first.invert_yaxis()
    title = run.get("title", "executemany INSERT: throughput relative to mysqlclient")
    drivers = tuple(d for d in DRIVERS if d in run["drivers"])
    finish(fig, theme, path, f"{title} ({run['python']})", drivers)


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
    # orm.py runs through SQLAlchemy and records its version.
    sqlalchemy = f"SQLAlchemy {run['sqlalchemy']} · " if "sqlalchemy" in run else ""
    # pipeline.py names each run's network conditions.
    title = f"{run['title']} · " if "title" in run else ""
    setup = (
        f"**{run['python']}** · {title}{sqlalchemy}{run['server']} · "
        f"{run['machine']} · "
        f"{driver_versions(drivers)} · best of {run['repeat']} · {run['date']}"
    )
    return f"{setup}\n\n{header}" + "\n".join(lines) + "\n"


def driver_versions(drivers: dict[str, str]) -> str:
    """The drivers of a run with their versions (PyMySQL may be absent)."""
    return ", ".join(f"{LABELS[name]} {version}" for name, version in drivers.items())


def sweep_table(run: dict[str, Any]) -> str:
    panels = run["sweep"]
    header = (
        "| Configuration | "
        + " | ".join(panel_title(p) for p in panels)
        + " | "
        + " | ".join(f"vs mysqlclient ({panel_title(p)})" for p in panels)
        + " |\n|---|"
        + "--:|" * (2 * len(panels))
        + "\n"
    )
    lines = []
    for config in panels[0]["seconds"]:
        name = sweep_label(config, run)
        times = []
        for p in panels:
            seconds = p["seconds"][config]
            cell = f"{seconds * 1000:.1f} ms"
            times.append(
                f"**{cell}**" if seconds == min(p["seconds"].values()) else cell
            )
        ratios = [
            f"{p['seconds']['mysqlclient'] / p['seconds'][config]:.2f}x" for p in panels
        ]
        lines.append(f"| {name} | {' | '.join(times)} | {' | '.join(ratios)} |")
    drivers = run["drivers"]
    setup = (
        f"**{run['python']}** · {run['server']} · {run['machine']} · "
        f"{driver_versions(drivers)} · best of {run['repeat']} · {run['date']}"
    )
    return f"{setup}\n\n{header}" + "\n".join(lines) + "\n"


def windows_table(run: dict[str, Any]) -> str:
    """rustmy at each ``max_pipeline`` (``pipeline.py --sweep``)."""
    scenarios = run["windows"]
    configs = list(scenarios[0]["seconds"])
    header = (
        f"\n`max_pipeline` sweep, {run.get('title', run['python'])}:\n\n"
        "| Scenario | "
        + " | ".join(c.removeprefix("rustmy w") for c in configs)
        + " |\n|---|"
        + "--:|" * len(configs)
        + "\n"
    )
    lines = []
    for scenario in scenarios:
        seconds = scenario["seconds"]
        fastest = min(seconds.values())
        cells = [
            f"**{seconds[c] * 1000:.1f} ms**"
            if seconds[c] == fastest
            else f"{seconds[c] * 1000:.1f} ms"
            for c in configs
        ]
        lines.append(f"| {scenario['name']} | {' | '.join(cells)} |")
    return header + "\n".join(lines) + "\n"


def memory_table(run: dict[str, Any]) -> str:
    """Peak memory growth while reading a result set (``cursors.py --memory``)."""
    header = (
        "| Cursor | Rows | rustmy | mysqlclient | PyMySQL |\n|---|--:|--:|--:|--:|\n"
    )
    lines = []
    for entry in run["memory"]:
        peak = entry["peak_mib"]
        cells = " | ".join(f"{peak[d]:.1f} MiB" for d in DRIVERS)
        lines.append(f"| {entry['cursor']} | {entry['rows']:,} | {cells} |")
    return (
        "\nPeak memory growth while reading the result set, each in a fresh "
        "process (buffered: the whole result at once, as `fetchall()` holds it; "
        "streaming: iterating):\n\n" + header + "\n".join(lines) + "\n"
    )


def readme_block(runs: list[dict[str, Any]], output: Path) -> str:
    rel = output.relative_to(HERE).as_posix()
    picture = (
        "<picture>\n"
        f'  <source media="(prefers-color-scheme: dark)" srcset="{rel}-dark.svg">\n'
        f'  <img alt="Throughput relative to mysqlclient" src="{rel}-light.svg">\n'
        "</picture>\n"
    )
    if all("sweep" in run for run in runs):
        tables = "\n".join(sweep_table(run) for run in runs)
        note = "\nTimes per configuration; the fastest is in bold.\n"
        return f"{picture}\n{tables}{note}"
    tables = "\n".join(table(run) for run in runs)
    note = (
        "\nTimes per driver; the fastest is in bold. Ratios above 1 mean rustmy "
        "is faster.\n"
    )
    note += "".join(memory_table(run) for run in runs if "memory" in run)
    note += "".join(windows_table(run) for run in runs if "windows" in run)
    if any(s["informative"] for run in runs for s in run["scenarios"]):
        note += (
            "¹ Not comparable yet: rustmy runs one INSERT per row, mysqlclient "
            "and PyMySQL batch them into multi-row INSERTs (Phase 2). Left out "
            "of the chart.\n"
        )
    return f"{picture}\n{tables}{note}"


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("results", nargs="+", type=Path)
    parser.add_argument(
        "--section",
        default="phase0",
        help="README block to rewrite: <!-- results:SECTION:start/end -->",
    )
    parser.add_argument(
        "--output",
        type=Path,
        help="path prefix of the SVG files (default: results/SECTION)",
    )
    args = parser.parse_args()
    section: str = args.section
    output: Path = (args.output or HERE / "results" / section).resolve()
    runs = [load(path) for path in args.results]

    plt.rcParams["svg.fonttype"] = "none"  # keep text as text
    plt.rcParams["svg.hashsalt"] = "rustmy"  # deterministic ids, stable diffs
    for theme in (LIGHT, DARK):
        path = output.with_name(f"{output.name}-{theme.name}.svg")
        if all("sweep" in run for run in runs):
            plot_sweep(runs[0], theme, path)
        else:
            plot(runs, theme, path)

    marker = re.compile(
        rf"(<!-- results:{re.escape(section)}:start -->\n).*?"
        rf"(<!-- results:{re.escape(section)}:end -->)",
        flags=re.DOTALL,
    )
    readme = README.read_text()
    block = readme_block(runs, output)
    updated, count = marker.subn(lambda m: m.group(1) + block + m.group(2), readme)
    if count != 1:
        msg = f"expected one '{section}' results block in {README}, found {count}"
        raise SystemExit(msg)
    README.write_text(updated)
    print(f"wrote {output.name}-light.svg, {output.name}-dark.svg and {README.name}")


if __name__ == "__main__":
    main()
