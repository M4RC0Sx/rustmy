"""Charts and tables of ``showcase.py`` results.

    uv run --no-sync python benches/showcase.py \\
        --render benches/results/showcase-py314.json \\
        benches/results/showcase-latency-*-py314.json

Writes ``benches/results/showcase-<area>-{light,dark}.svg`` (one panel per
workload and size: absolute times, lower is better, rustmy highlighted and
the other drivers in a neutral gray, each bar named), the summary charts at
the top of the main README (``hero-{light,dark}.svg`` for the blocking
drivers, ``hero-async-{light,dark}.svg`` for asyncio), and the tables between
the ``showcase`` markers of README.md and benches/README.md.
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
#: The asyncio areas' drivers, compared with asyncmy (the fastest of the
#: others) instead of mysqlclient.
AREA_ORDER = {
    "asyncio": ("rustmy", "asyncmy", "aiomysql", "mysqlconnector"),
    "sqlalchemy-asyncio": ("rustmy", "asyncmy", "aiomysql"),
}
BASELINE = {"asyncio": "asyncmy", "sqlalchemy-asyncio": "asyncmy"}
LABELS = {
    "rustmy": "rustmy",
    "rustmy-prepared": "rustmy (prepared)",
    "rustmy-text": "rustmy (text)",
    "mysqlclient": "mysqlclient",
    "pymysql": "PyMySQL",
    "mysqlconnector": "mysql-connector",
    "mysqlconnector-prepared": "mysql-connector (prepared)",
    "asyncmy": "asyncmy",
    "aiomysql": "aiomysql",
}
#: mysql-connector-python's asyncio API is another module.
ASYNC_LABELS = {**LABELS, "mysqlconnector": "mysql.connector.aio"}
AREAS = {
    "dbapi": "DB-API",
    "prepared": "Prepared statements",
    "sqlalchemy": "SQLAlchemy 2",
    "django": "Django",
    "memory": "Memory",
    "asyncio": "asyncio",
    "sqlalchemy-asyncio": "SQLAlchemy 2, asyncio",
}


@dataclass(frozen=True)
class Panel:
    title: str
    rows: int
    values: dict[str, float]  # config -> seconds (MiB for memory)
    unit: str = "s"
    area: str = ""
    count: str = "rows"  # what ``rows`` counts

    @property
    def baseline(self) -> str:
        return BASELINE.get(self.area, "mysqlclient")

    @property
    def labels(self) -> dict[str, str]:
        return ASYNC_LABELS if self.area in AREA_ORDER else LABELS


def load(path: Path) -> dict[str, Any]:
    data: dict[str, Any] = json.loads(path.read_text())
    return data


def panels(run: dict[str, Any], area: str, loop: str = "asyncio") -> list[Panel]:
    """One panel per workload and size, in the order measured (the asyncio
    areas' on ``loop``)."""
    grouped: dict[tuple[str, int], dict[str, float]] = {}
    counts: dict[tuple[str, int], str] = {}
    for record in run["results"]:
        if record["area"] != area or record.get("loop", "asyncio") != loop:
            continue
        name = record["name"]
        config = record["driver"]
        if area == "prepared":
            # "Read rows (rustmy-prepared)": the configuration is in the name.
            config = name[name.index("(") + 1 : -1]
            name = "Read rows"
        grouped.setdefault((name, record["rows"]), {})[config] = record["value"]
        counts[name, record["rows"]] = record.get("unit", "rows")
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
    # By size, workloads counted otherwise (the pool's queries) last.
    ordered.sort(
        key=lambda item: (
            counts.get(item[0], "rows") != "rows",
            item[0][1],
            names.index(item[0][0]),
        )
    )
    return [
        Panel(name, rows, values, unit, area, counts.get((name, rows), "rows"))
        for (name, rows), values in ordered
    ]


def configs(panel: Panel) -> list[str]:
    preferred = AREA_ORDER.get(panel.area) or (
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
    name = panel.labels[panel.baseline]
    base = panel.values.get(panel.baseline)
    if ours is None or base is None:
        return ""
    if panel.unit == "MiB":
        saved = (1 - ours / base) * 100
        if abs(saved) < 5:
            return f"as much memory as {name}"
        more_or_less = "less" if saved > 0 else "more"
        return f"{abs(saved):.0f}% {more_or_less} memory than {name}"
    ratio = base / ours
    # Within 5% either way is noise, not a result; slower says so.
    if ratio >= 1.05:
        return f"{ratio:.1f}× faster than {name}"  # noqa: RUF001  # a real "times"
    if ratio > 0.95:
        return f"on par with {name}"
    return f"{1 / ratio:.1f}× slower than {name}"  # noqa: RUF001


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
    ax.set_yticks(y, [panel.labels[c] for c in names], fontsize=9)
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


def grid(items: list[Panel]) -> list[list[Panel]]:
    """Columns of panels: one per size, the smaller on the left; workloads
    counted otherwise (the pool's queries) go below the first."""
    sizes = sorted({p.rows for p in items if p.count == "rows"})
    columns = [[p for p in items if p.count == "rows" and p.rows == s] for s in sizes]
    if not columns:
        columns = [[]]
    columns[0].extend(p for p in items if p.count != "rows")
    return columns


def area_chart(run: dict[str, Any], area: str, theme: Theme, path: Path) -> None:
    columns = grid(panels(run, area))
    rows = max(len(column) for column in columns)
    bars = max(len(configs(p)) for column in columns for p in column)
    fig, axes = plt.subplots(rows, len(columns), squeeze=False)
    layout(fig, rows, len(columns), bars)
    fig.patch.set_facecolor(theme.surface)
    for x, column in enumerate(columns):
        for y in range(rows):
            if y < len(column):
                panel = column[y]
                title = f"{panel.title}, {panel.rows:,} {panel.count}"
                draw(axes[y][x], theme, panel, title=title)
            else:
                axes[y][x].set_visible(False)
    note = (
        "Peak memory, lower is better" if area == "memory" else "Time, lower is better"
    )
    footer(fig, theme, run, note)
    fig.savefig(path, facecolor=theme.surface, metadata={"Date": None})
    plt.close(fig)


#: The summary charts: (area, workload, title) per panel, each workload at
#: the largest size measured.
HERO = (
    ("dbapi", "Read rows (tuples)", "Read 1,000,000 rows"),
    ("dbapi", "Insert rows (executemany)", "Insert 1,000,000 rows"),
    ("sqlalchemy", "ORM: load objects", "SQLAlchemy ORM, 1,000,000 objects"),
    ("django", "QuerySet: load objects", "Django ORM, 1,000,000 objects"),
)
HERO_ASYNC = (
    ("asyncio", "Read rows (tuples)", "asyncio: read 1,000,000 rows"),
    ("asyncio", "Insert rows (executemany)", "asyncio: insert 1,000,000 rows"),
    (
        "sqlalchemy-asyncio",
        "ORM: load objects",
        "SQLAlchemy asyncio ORM, 1,000,000 objects",
    ),
    (
        "asyncio",
        "Pool: 32 tasks, 8 connections",
        "asyncio pool: 32 tasks, 8 connections, 6,400 queries",
    ),
)


def hero(
    run: dict[str, Any],
    theme: Theme,
    path: Path,
    spec: tuple[tuple[str, str, str], ...],
    note: str,
) -> None:
    fig, axes = plt.subplots(2, 2, squeeze=False)
    layout(fig, 2, 2, max(len(AREA_ORDER.get(area, ORDER)) for area, _, _ in spec))
    fig.patch.set_facecolor(theme.surface)
    for index, (area, name, title) in enumerate(spec):
        panel = max(
            (p for p in panels(run, area) if p.title == name), key=lambda p: p.rows
        )
        draw(axes[index // 2][index % 2], theme, panel, title=title)
    footer(fig, theme, run, note)
    fig.savefig(path, facecolor=theme.surface, metadata={"Date": None})
    plt.close(fig)


def table(run: dict[str, Any], area: str, loop: str = "asyncio") -> str:
    items = panels(run, area, loop)
    order: tuple[str, ...] = AREA_ORDER.get(area, ORDER)
    labels = ASYNC_LABELS if area in AREA_ORDER else LABELS
    baseline = BASELINE.get(area, "mysqlclient")
    head = [
        "Workload",
        "Rows",
        *(labels[c] for c in order),
        f"vs {labels[baseline]}",
    ]
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
        order = (
            "rustmy-prepared",
            "rustmy-text",
            "mysqlconnector-prepared",
            "mysqlclient",
            "pymysql",
        )
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
        verdict = headline(panel).replace(f" than {labels[baseline]}", "")
        count = f"{panel.rows:,}" + ("" if panel.count == "rows" else f" {panel.count}")
        lines.append(
            f"| {panel.title} | {count} | " + " | ".join(cells) + f" | {verdict} |"
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


def rustmy_version(run: dict[str, Any]) -> str:
    """rustmy's version, or what was measured before a release."""
    version: str = run["versions"]["rustmy"]
    return "(a release build of develop)" if version == "0.0.0" else version


def section(run: dict[str, Any], base: str, *, folded: bool = False) -> str:
    v = run["versions"]
    parts = [
        (
            f"**{run['python']}** · MySQL {run['server']} · {run['machine']} · "
            f"rustmy {rustmy_version(run)}, mysqlclient {v['mysqlclient']}, "
            f"PyMySQL {v['pymysql']}, mysql-connector-python {v['mysqlconnector']} "
            f"(C extension), SQLAlchemy {v['sqlalchemy']}, Django {v['django']} · "
            f"best of {run['repeat']} (100k) and {run['repeat_large']} (1M) · "
            f"{run['date']}"
        )
    ]
    for area, title in AREAS.items():
        measure = "peak memory" if area == "memory" else "time"
        if area == "asyncio":
            parts.append(
                f"asyncio: asyncmy {v['asyncmy']}, aiomysql {v['aiomysql']} and "
                f"mysql.connector.aio (mysql-connector-python {v['mysqlconnector']}, "
                "pure Python), on asyncio's default event loop; compared with "
                "asyncmy, the fastest of them. mysql.connector.aio's pool fails "
                "instead of waiting when no connection is idle, so it has no pool "
                "figure; SQLAlchemy has no asyncio dialect for it."
            )
        parts.append(f"#### {title}")
        parts.append(
            picture(
                f"showcase-{area}",
                f"{title}: {measure} per driver, lower is better",
                base,
            )
        )
        numbers = table(run, area)
        if folded:
            # The main README leads with the charts; the figures are a click
            # away. `markdown="1"` lets the documentation site, which includes
            # this block, read the table inside (GitHub and PyPI ignore it).
            numbers = (
                f'<details markdown="1">\n<summary>Numbers</summary>\n\n'
                f"{numbers}\n\n</details>"
            )
        parts.append(numbers)
        if not folded and area in AREA_ORDER and panels(run, area, "uvloop"):
            parts.append(f"With uvloop {v['uvloop']}:")
            parts.append(table(run, area, "uvloop"))
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


def latency_section(runs: list[dict[str, Any]]) -> str:
    """The tables of ``--latency`` runs, by latency."""
    parts = []
    for run in sorted(runs, key=lambda run: run["latency_ms"]):
        parts.append(
            f"#### +{run['latency_ms']} ms per reply "
            f"({run['round_trip_ms']:.1f} ms round trip)"
        )
        parts.append(
            f"{run['python']} · MySQL {run['server']} through toxiproxy · "
            f"rustmy {rustmy_version(run)} · best of {run['repeat']} · {run['date']}"
        )
        for area, title in AREAS.items():
            if any(record["area"] == area for record in run["results"]):
                parts.append(f"**{title}**")
                parts.append(table(run, area))
    return "\n\n".join(parts)


def render(paths: list[Path]) -> None:
    """The showcase from the run without added latency, and the latency
    tables from the others (either may be absent)."""
    plt.rcParams["svg.fonttype"] = "none"  # keep text as text
    plt.rcParams["svg.hashsalt"] = "rustmy"  # deterministic ids, stable diffs
    runs = [load(path) for path in paths]
    latency = [run for run in runs if run.get("latency_ms") is not None]
    for run in runs:
        if run in latency:
            continue
        for theme in (LIGHT, DARK):
            for area in AREAS:
                path = RESULTS / f"showcase-{area}-{theme.name}.svg"
                area_chart(run, area, theme, path)
            hero(
                run,
                theme,
                RESULTS / f"hero-{theme.name}.svg",
                HERO,
                "Time, lower is better",
            )
            hero(
                run,
                theme,
                RESULTS / f"hero-async-{theme.name}.svg",
                HERO_ASYNC,
                "Time, lower is better, on asyncio's default event loop",
            )
        replace_block(BENCH_README, "results:showcase", section(run, "results"))
        replace_block(README, "showcase", section(run, RAW, folded=True))
        print("wrote the showcase and hero charts, README.md and benches/README.md")
    if latency:
        replace_block(BENCH_README, "results:latency", latency_section(latency))
        print("wrote the latency tables in benches/README.md")
