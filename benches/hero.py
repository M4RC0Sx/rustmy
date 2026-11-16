"""The summary chart at the top of the main README.

Three everyday workloads, absolute times per driver (lower is better), rustmy
highlighted and the other drivers in a neutral gray (each bar is named, so
identity never rests on color). Reads the "Current results" JSON files, so it
is regenerated with them when a phase closes:

    uv run --no-sync python benches/hero.py

Writes benches/results/hero-light.svg and hero-dark.svg.
"""

from __future__ import annotations

import json

from dataclasses import dataclass
from pathlib import Path

import matplotlib as mpl


mpl.use("Agg")

import matplotlib.pyplot as plt

from report import DARK
from report import LIGHT
from report import Theme


HERE = Path(__file__).resolve().parent
RESULTS = HERE / "results"
DRIVERS = ("rustmy", "mysqlclient", "pymysql")
LABELS = {"rustmy": "rustmy", "mysqlclient": "mysqlclient", "pymysql": "PyMySQL"}
#: The other drivers recede: gray at >= 3:1 against each surface.
MUTED = {"light": "#8a8883", "dark": "#6e6d68"}


@dataclass(frozen=True)
class Panel:
    title: str
    file: str
    scenario: str


PANELS = (
    Panel(
        "Read 100,000 rows",
        "current-spike-py315.json",
        "fetchall 100k mixed (double/datetime/date/decimal)",
    ),
    Panel("Insert 10,000 rows", "current-spike-py315.json", "executemany 10k rows"),
    Panel(
        "SQLAlchemy, 100,000 rows",
        "current-orm-py315.json",
        "Core fetchall 100k mixed",
    ),
)


def seconds(panel: Panel) -> tuple[dict[str, float], dict[str, str]]:
    run = json.loads((RESULTS / panel.file).read_text())
    scenario = next(s for s in run["scenarios"] if s["name"] == panel.scenario)
    return scenario["seconds"], run


def plot(theme: Theme, path: Path) -> None:
    fig, axes = plt.subplots(1, len(PANELS), figsize=(11, 2.6))
    fig.patch.set_facecolor(theme.surface)
    run: dict[str, str] = {}
    for ax, panel in zip(axes, PANELS, strict=True):
        times, run = seconds(panel)
        ms = [times[d] * 1000 for d in DRIVERS]
        colors = [
            theme.series["rustmy"] if d == "rustmy" else MUTED[theme.name]
            for d in DRIVERS
        ]
        y = range(len(DRIVERS))
        ax.barh(y, ms, height=0.62, color=colors, edgecolor=theme.surface, linewidth=2)
        ax.invert_yaxis()
        ax.set_yticks(y, [LABELS[d] for d in DRIVERS], fontsize=10)
        for label, driver in zip(ax.get_yticklabels(), DRIVERS, strict=True):
            label.set_color(theme.text if driver == "rustmy" else theme.text_secondary)
            label.set_fontweight("bold" if driver == "rustmy" else "normal")
        longest = max(ms)
        for i, value in enumerate(ms):
            ax.text(
                value + longest * 0.02,
                i,
                f"{value:.0f} ms",
                va="center",
                fontsize=9,
                color=theme.text if i == 0 else theme.text_secondary,
            )
        ax.set_xlim(0, longest * 1.3)
        ax.set_xticks([])
        ax.set_facecolor(theme.surface)
        for side in ("top", "right", "bottom"):
            ax.spines[side].set_visible(False)
        ax.spines["left"].set_color(theme.grid)
        ax.tick_params(axis="y", length=0, pad=6)
        # The headline, with the workload above it in secondary ink.
        speedup = times["mysqlclient"] / times["rustmy"]
        ax.text(
            0,
            1.3,
            panel.title,
            transform=ax.transAxes,
            fontsize=9.5,
            color=theme.text_secondary,
        )
        ax.text(
            0,
            1.08,
            f"{speedup:.1f}× faster than mysqlclient",  # noqa: RUF001  # a real "times"
            transform=ax.transAxes,
            fontsize=11.5,
            fontweight="bold",
            color=theme.text,
        )
    fig.text(
        0.01,
        0.01,
        f"Time per run, lower is better · {run['python']}, MySQL {run['server']}, "
        f"{run['machine']} · details in benches/README.md",
        fontsize=8,
        color=theme.text_secondary,
    )
    fig.subplots_adjust(left=0.09, right=0.98, top=0.68, bottom=0.12, wspace=0.55)
    fig.savefig(path, facecolor=theme.surface, metadata={"Date": None})
    plt.close(fig)


def main() -> None:
    plt.rcParams["svg.fonttype"] = "none"  # keep text as text
    plt.rcParams["svg.hashsalt"] = "rustmy"  # deterministic ids, stable diffs
    for theme in (LIGHT, DARK):
        path = RESULTS / f"hero-{theme.name}.svg"
        plot(theme, path)
        print(f"wrote {path.name}")


if __name__ == "__main__":
    main()
