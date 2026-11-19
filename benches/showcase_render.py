"""Charts and tables of ``showcase.py`` results.

    uv run --no-sync python benches/showcase.py \\
        --render benches/results/showcase-py314.json

Writes ``benches/results/showcase-<area>-{light,dark}.svg`` (one panel per
workload and size: absolute times, lower is better, rustmy highlighted and
the other drivers in a neutral gray, each bar named), the summary chart at
the top of the main README (``hero-{light,dark}.svg``), and the tables
between the ``showcase`` markers of README.md and benches/README.md.
"""

from __future__ import annotations

import json
import re
import textwrap

from dataclasses import dataclass
from pathlib import Path
from typing import Any

import matplotlib as mpl


mpl.use("svg")

import matplotlib.pyplot as plt  # backend must be set first

from report import DARK
from report import LIGHT
from report import Theme


HERE = Path(__file__).resolve().parent
RESULTS = HERE / "results"
README = HERE.parent / "README.md"
BENCH_README = HERE / "README.md"
RAW = "https://raw.githubusercontent.com/M4RC0Sx/rustmy/master/benches/results"

#: The other drivers recede: gray at >= 3:1 against each surface.
MUTED = {"light": "#8a8883", "dark": "#6e6d68"}
ORDER = ("rustmy", "mysqlclient", "pymysql", "mysqlconnector")
LABELS = {
    "rustmy": "rustmy",
    "rustmy-prepared": "rustmy (prepared)",
    "rustmy-text": "rustmy (text)",
    "mysqlclient": "mysqlclient",
    "pymysql": "PyMySQL",
    "mysqlconnector": "mysql-connector",
    "mysqlconnector-prepared": "mysql-connector (prepared)",
}
AREAS = {
    "dbapi": "DB-API",
    "prepared": "Prepared statements",
    "sqlalchemy": "SQLAlchemy 2",
    "django": "Django",
    "memory": "Memory",
}


@dataclass(frozen=True)
class Panel:
    title: str
    rows: int
    values: dict[str, float]  # config -> seconds (MiB for memory)
    unit: str = "s"


def load(path: Path) -> dict[str, Any]:
    data: dict[str, Any] = json.loads(path.read_text())
    return data


def panels(run: dict[str, Any], area: str) -> list[Panel]:
    """One panel per workload and size, in the order measured."""
    grouped: dict[tuple[str, int], dict[str, float]] = {}
    for record in run["results"]:
        if record["area"] != area:
            continue
        name = record["name"]
        config = record["driver"]
        if area == "prepared":
            # "Read rows (rustmy-prepared)": the configuration is in the name.
            config = name[name.index("(") + 1 : -1]
            name = "Read rows"
        grouped.setdefault((name, record["rows"]), {})[config] = record["value"]
    if area == "prepared":
        # rustmy's text protocol, for scale: the DB-API read of the same rows.
        for record in run["results"]:
            if record["area"] == "dbapi" and record["name"] == "Read rows (tuples)":
                key = ("Read rows", record["rows"])
                if record["driver"] == "rustmy" and key in grouped:
                    grouped[key]["rustmy-text"] = record["value"]
    unit = "MiB" if area == "memory" else "s"
    ordered = sorted(grouped.items(), key=lambda item: item[0][1])
    names = list(dict.fromkeys(name for (name, _), _ in grouped.items()))
    ordered.sort(key=lambda item: (item[0][1], names.index(item[0][0])))
    return [Panel(name, rows, values, unit) for (name, rows), values in ordered]


def configs(panel: Panel) -> list[str]:
    preferred = (
        "rustmy-prepared",
        "rustmy",
        "rustmy-text",
        "mysqlconnector-prepared",
        "mysqlclient",
        "pymysql",
        "mysqlconnector",
    )
    return [c for c in preferred if c in panel.values]


def fmt(value: float, unit: str) -> str:
    if unit == "MiB":
        return f"{value:.0f} MiB"
    if value >= 1:
        return f"{value:.2f} s"
    return f"{value * 1000:.0f} ms"


def headline(panel: Panel) -> str:
    ours = panel.values.get("rustmy-prepared", panel.values.get("rustmy"))
    base = panel.values.get("mysqlclient")
    if ours is None or base is None:
        return ""
    if panel.unit == "MiB":
        saved = (1 - ours / base) * 100
        if abs(saved) < 5:
            return "as much memory as mysqlclient"
        more_or_less = "less" if saved > 0 else "more"
        return f"{abs(saved):.0f}% {more_or_less} memory than mysqlclient"
    ratio = base / ours
    # Within 5% either way is noise, not a result; slower says so.
    if ratio >= 1.05:
        return f"{ratio:.1f}× faster than mysqlclient"  # noqa: RUF001  # a real "times"
    if ratio > 0.95:
        return "on par with mysqlclient"
    return f"{1 / ratio:.1f}× slower than mysqlclient"  # noqa: RUF001


def draw(ax: Any, theme: Theme, panel: Panel, *, title: str) -> None:
    names = configs(panel)
    values = [panel.values[c] for c in names]
    colors = [
        theme.series["rustmy"] if c.startswith("rustmy") else MUTED[theme.name]
        for c in names
    ]
    y = range(len(names))
    ax.barh(y, values, height=0.62, color=colors, edgecolor=theme.surface, linewidth=2)
    ax.invert_yaxis()
    ax.set_yticks(y, [LABELS[c] for c in names], fontsize=9)
    for label, config in zip(ax.get_yticklabels(), names, strict=True):
        ours = config.startswith("rustmy")
        label.set_color(theme.text if ours else theme.text_secondary)
        label.set_fontweight("bold" if ours else "normal")
    longest = max(values)
    for i, (config, value) in enumerate(zip(names, values, strict=True)):
        ax.text(
            value + longest * 0.02,
            i,
            fmt(value, panel.unit),
            va="center",
            fontsize=8.5,
            color=theme.text if config.startswith("rustmy") else theme.text_secondary,
        )
    ax.set_xlim(0, longest * 1.32)
    ax.set_xticks([])
    ax.set_facecolor(theme.surface)
    for side in ("top", "right", "bottom"):
        ax.spines[side].set_visible(False)
    ax.spines["left"].set_color(theme.grid)
    ax.tick_params(axis="y", length=0, pad=6)
    ax.text(
        0, 1.30, title, transform=ax.transAxes, fontsize=9, color=theme.text_secondary
    )
    ax.text(
        0,
        1.07,
        headline(panel),
        transform=ax.transAxes,
        fontsize=10.5,
        fontweight="bold",
        color=theme.text,
    )


def footer(fig: Any, theme: Theme, run: dict[str, Any], note: str) -> None:
    text = (
        f"{note} · {run['python']}, MySQL {run['server']}, {run['machine']}, "
        "plain TCP, best of several runs · benches/README.md"
    )
    width = fig.get_size_inches()[0]
    fig.text(
        0.01,
        0.01,
        textwrap.fill(text, int(width * 17)),
        fontsize=7.5,
        color=theme.text_secondary,
        va="bottom",
    )


#: Layout in inches, so panels keep their proportions whatever their count.
LABEL_IN = 2.0  # room for the driver names left of each panel
TOP_IN = 0.8  # the workload and headline above a row of panels
GAP_IN = 0.85  # between rows (the next row's headline goes there)
BOTTOM_IN = 0.4  # the footer
PANEL_W_IN = 3.6


def layout(fig: Any, rows: int, columns: int, bars: int) -> tuple[float, float]:
    """Size ``fig`` for a ``rows`` x ``columns`` grid; returns its size."""
    axes_h = 0.3 * bars + 0.1
    # One column is narrow: the footer takes two lines.
    bottom = BOTTOM_IN + (0.15 if columns == 1 else 0)
    height = TOP_IN + rows * axes_h + (rows - 1) * GAP_IN + bottom
    width = columns * (LABEL_IN + PANEL_W_IN) + 0.2
    fig.set_size_inches(width, height)
    fig.subplots_adjust(
        left=LABEL_IN / width,
        right=1 - 0.2 / width,
        top=1 - TOP_IN / height,
        bottom=bottom / height,
        wspace=LABEL_IN / PANEL_W_IN,
        hspace=GAP_IN / axes_h,
    )
    return width, height


def area_chart(run: dict[str, Any], area: str, theme: Theme, path: Path) -> None:
    items = panels(run, area)
    columns = 2 if len(items) > 1 else 1
    rows = (len(items) + columns - 1) // columns
    bars = max(len(configs(p)) for p in items)
    fig, axes = plt.subplots(rows, columns, squeeze=False)
    layout(fig, rows, columns, bars)
    fig.patch.set_facecolor(theme.surface)
    # Fill column by column: the smaller size on the left.
    for index, panel in enumerate(items):
        ax = axes[index % rows][index // rows]
        draw(ax, theme, panel, title=f"{panel.title}, {panel.rows:,} rows")
    for extra in range(len(items), rows * columns):
        axes[extra % rows][extra // rows].set_visible(False)
    note = (
        "Peak memory, lower is better" if area == "memory" else "Time, lower is better"
    )
    footer(fig, theme, run, note)
    fig.savefig(path, facecolor=theme.surface, metadata={"Date": None})
    plt.close(fig)


HERO = (
    ("dbapi", "Read rows (tuples)", "Read 1,000,000 rows"),
    ("dbapi", "Insert rows (executemany)", "Insert 1,000,000 rows"),
    ("sqlalchemy", "ORM: load objects", "SQLAlchemy ORM, 1,000,000 objects"),
    ("django", "QuerySet: load objects", "Django ORM, 1,000,000 objects"),
)


def hero(run: dict[str, Any], theme: Theme, path: Path) -> None:
    size = max(r["rows"] for r in run["results"])
    fig, axes = plt.subplots(2, 2, squeeze=False)
    layout(fig, 2, 2, len(ORDER))
    fig.patch.set_facecolor(theme.surface)
    for index, (area, name, title) in enumerate(HERO):
        panel = next(p for p in panels(run, area) if p.title == name and p.rows == size)
        draw(axes[index // 2][index % 2], theme, panel, title=title)
    footer(fig, theme, run, "Time, lower is better")
    fig.savefig(path, facecolor=theme.surface, metadata={"Date": None})
    plt.close(fig)


def table(run: dict[str, Any], area: str) -> str:
    items = panels(run, area)
    head = ["Workload", "Rows", *(LABELS[c] for c in ORDER), "vs mysqlclient"]
    if area == "prepared":
        head = [
            "Workload",
            "Rows",
            "rustmy (prepared)",
            "rustmy (text)",
            "mysql-connector (prepared)",
            "mysqlclient",
            "PyMySQL",
            "vs mysqlclient",
        ]
        order: tuple[str, ...] = (
            "rustmy-prepared",
            "rustmy-text",
            "mysqlconnector-prepared",
            "mysqlclient",
            "pymysql",
        )
    else:
        order = ORDER
    lines = ["| " + " | ".join(head) + " |", "|---|--:|" + "--:|" * (len(head) - 2)]
    for panel in items:
        values = [panel.values.get(c) for c in order]
        present = [v for v in values if v is not None]
        fastest = min(present)
        cells = []
        for value in values:
            if value is None:
                cells.append("n/a")
                continue
            text = fmt(value, panel.unit)
            cells.append(f"**{text}**" if value == fastest else text)
        verdict = headline(panel).replace(" than mysqlclient", "")
        lines.append(
            f"| {panel.title} | {panel.rows:,} | "
            + " | ".join(cells)
            + f" | {verdict} |"
        )
    return "\n".join(lines)


def picture(name: str, alt: str, base: str) -> str:
    return (
        "<picture>\n"
        '  <source media="(prefers-color-scheme: dark)" '
        f'srcset="{base}/{name}-dark.svg">\n'
        f'  <img alt="{alt}" src="{base}/{name}-light.svg">\n'
        "</picture>"
    )


def section(run: dict[str, Any], base: str, *, folded: bool = False) -> str:
    v = run["versions"]
    parts = [
        (
            f"**{run['python']}** · MySQL {run['server']} · {run['machine']} · "
            f"rustmy {v['rustmy']}, mysqlclient {v['mysqlclient']}, "
            f"PyMySQL {v['pymysql']}, mysql-connector-python {v['mysqlconnector']} "
            f"(C extension), SQLAlchemy {v['sqlalchemy']}, Django {v['django']} · "
            f"best of {run['repeat']} (100k) and {run['repeat_large']} (1M) · "
            f"{run['date']}"
        )
    ]
    for area, title in AREAS.items():
        parts.append(f"#### {title}")
        parts.append(
            picture(
                f"showcase-{area}",
                f"{title}: time per driver, lower is better",
                base,
            )
        )
        numbers = table(run, area)
        if folded:
            # The main README leads with the charts; the figures are a click away.
            numbers = (
                f"<details>\n<summary>Numbers</summary>\n\n{numbers}\n\n</details>"
            )
        parts.append(numbers)
    return "\n\n".join(parts)


def replace_block(path: Path, marker: str, content: str) -> None:
    text = path.read_text()
    pattern = re.compile(
        rf"(<!-- {marker}:start -->\n).*?(<!-- {marker}:end -->)", re.DOTALL
    )
    if not pattern.search(text):
        msg = f"{path.name} has no {marker} markers"
        raise SystemExit(msg)
    path.write_text(pattern.sub(lambda m: m[1] + content + "\n" + m[2], text))


def render(path: Path) -> None:
    plt.rcParams["svg.fonttype"] = "none"  # keep text as text
    plt.rcParams["svg.hashsalt"] = "rustmy"  # deterministic ids, stable diffs
    run = load(path)
    for theme in (LIGHT, DARK):
        for area in AREAS:
            area_chart(run, area, theme, RESULTS / f"showcase-{area}-{theme.name}.svg")
        hero(run, theme, RESULTS / f"hero-{theme.name}.svg")
    replace_block(BENCH_README, "results:showcase", section(run, "results"))
    replace_block(README, "showcase", section(run, RAW, folded=True))
    print("wrote the showcase and hero charts, README.md and benches/README.md")
