# Benchmarks

## Running

`spike.py` compares rustmy, mysqlclient and PyMySQL on the same server, in
the same process run. It reports the best of N attempts per scenario and the
speedup against mysqlclient; absolute times depend on the machine and are not
the point.

```sh
docker compose up -d --wait mysql
# mysqlclient compiles against libmysqlclient (macOS: brew install mysql-client pkg-config)
PKG_CONFIG_PATH=/opt/homebrew/opt/mysql-client/lib/pkgconfig uv sync --group bench
uv run --no-sync maturin develop --uv --release   # never benchmark a debug build
uv run --no-sync python benches/spike.py --repeat 5 --json benches/results/phase0-py310.json
# charts and the tables below are generated from the JSON files:
uv run --no-sync python benches/report.py --section phase0 benches/results/phase0-*.json
```

Fairness rules the script enforces: all drivers use `autocommit=True`, plain
TCP (rustmy and mysqlclient would otherwise negotiate TLS) and the same
queries against the same tables.

Which Python: each phase or block section below is a record of that block,
measured before and after its change on Python 3.10, the support floor and
the development environment's version, so they compare with each other
(Phase 0 also ran 3.14, to compare abi3 with per-version builds).
"Current results" measures today's code on the latest Python, what a new
project installs, and is regenerated when a phase closes.

## Current results (Python 3.15)

The latest code on CPython 3.15 (and SQLAlchemy 2.1 for the ORM), against
MySQL 8.4. Regenerated when a phase closes; the sections after this one keep
each block's own numbers. The environment needs the `bench` group and a
release build for 3.15:

```sh
export UV_PROJECT_ENVIRONMENT=.venv-3.15
PKG_CONFIG_PATH=/opt/homebrew/opt/mysql-client/lib/pkgconfig \
    uv sync --python 3.15 --group bench
uv run --no-sync --python 3.15 maturin develop --uv --release
P="uv run --no-sync --python 3.15 python"
$P benches/spike.py --repeat 7 --json benches/results/current-spike-py315.json
$P benches/cursors.py --memory --json benches/results/current-cursors-py315.json
$P benches/charsets.py --json benches/results/current-charsets-py315.json
$P benches/conv.py --json benches/results/current-conv-py315.json
$P benches/orm.py --repeat 10 --memory --json benches/results/current-orm-py315.json
for section in spike cursors charsets conv orm; do
    $P benches/report.py --section current-$section \
        benches/results/current-$section-py315.json
done
$P benches/hero.py   # the summary chart at the top of the main README
```

### Main scenarios (`spike.py`)

<!-- results:current-spike:start -->
<picture>
  <source media="(prefers-color-scheme: dark)" srcset="results/current-spike-dark.svg">
  <img alt="Throughput relative to mysqlclient" src="results/current-spike-light.svg">
</picture>

**CPython 3.15.2** · 8.4.13 · Darwin arm64 · rustmy 0.0.0, mysqlclient 2.3.1, PyMySQL 2.2.8 · best of 7 · 2026-11-16T00:51:58+00:00

| Scenario | rustmy | mysqlclient | PyMySQL | rustmy vs mysqlclient | rustmy vs PyMySQL |
|---|--:|--:|--:|--:|--:|
| fetchall 100k int/varchar | **24.5 ms** | 38.2 ms | 152.6 ms | 1.56x | 6.22x |
| fetchall 100k mixed (double/datetime/date/decimal) | **44.7 ms** | 124.6 ms | 278.0 ms | 2.78x | 6.21x |
| fetchall 100k datetime + date | **14.1 ms** | 74.5 ms | 185.6 ms | 5.30x | 13.18x |
| fetchall 100k decimal | **17.1 ms** | 21.1 ms | 82.2 ms | 1.24x | 4.82x |
| 10k x SELECT 1 | **632.6 ms** | 650.2 ms | 758.7 ms | 1.03x | 1.20x |
| 10k x INSERT with params | **1171.3 ms** | 1188.4 ms | 1273.5 ms | 1.01x | 1.09x |
| executemany 10k rows | **17.9 ms** | 29.4 ms | 28.1 ms | 1.65x | 1.57x |

Times per driver; the fastest is in bold. Ratios above 1 mean rustmy is faster.
<!-- results:current-spike:end -->

### Cursors (`cursors.py`)

<!-- results:current-cursors:start -->
<picture>
  <source media="(prefers-color-scheme: dark)" srcset="results/current-cursors-dark.svg">
  <img alt="Throughput relative to mysqlclient" src="results/current-cursors-light.svg">
</picture>

**CPython 3.15.2** · 8.4.13 · Darwin arm64 · rustmy 0.0.0, mysqlclient 2.3.1, PyMySQL 2.2.8 · best of 5 · 2026-11-16T00:52:15+00:00

| Scenario | rustmy | mysqlclient | PyMySQL | rustmy vs mysqlclient | rustmy vs PyMySQL |
|---|--:|--:|--:|--:|--:|
| Cursor fetchall 100k int/varchar | **24.2 ms** | 37.9 ms | 152.3 ms | 1.56x | 6.29x |
| DictCursor fetchall 100k int/varchar | **30.4 ms** | 44.0 ms | 180.9 ms | 1.45x | 5.95x |
| DictCursor fetchall 100k mixed | **47.5 ms** | 133.1 ms | 318.8 ms | 2.80x | 6.71x |
| SSCursor iterate 100k int/varchar | **21.5 ms** | 33.4 ms | 165.9 ms | 1.55x | 7.72x |
| SSCursor iterate 100k mixed | **39.2 ms** | 107.8 ms | 284.3 ms | 2.75x | 7.25x |
| SSCursor fetchmany(1000) 100k int/varchar | 21.1 ms | **21.1 ms** | 160.2 ms | 1.00x | 7.58x |
| SSCursor fetchone 100k int/varchar | **24.2 ms** | 29.7 ms | 163.2 ms | 1.23x | 6.75x |
| SSCursor fetchall 100k int/varchar | **28.3 ms** | 28.8 ms | 166.7 ms | 1.02x | 5.90x |
| SSDictCursor iterate 100k int/varchar | **21.8 ms** | 42.3 ms | 187.0 ms | 1.94x | 8.58x |
| SSCursor iterate 1M int/varchar | **128.8 ms** | 258.0 ms | 1213.1 ms | 2.00x | 9.42x |
| SSCursor first row of 1M | 0.3 ms | **0.3 ms** | 0.4 ms | 0.89x | 1.36x |

Times per driver; the fastest is in bold. Ratios above 1 mean rustmy is faster.

Peak memory growth while reading the result set, each in a fresh process (buffered: the whole result at once, as `fetchall()` holds it; streaming: iterating):

| Cursor | Rows | rustmy | mysqlclient | PyMySQL |
|---|--:|--:|--:|--:|
| Cursor | 100,000 | 21.5 MiB | 29.1 MiB | 21.5 MiB |
| SSCursor | 100,000 | 0.1 MiB | 0.0 MiB | 0.0 MiB |
| Cursor | 1,000,000 | 214.3 MiB | 303.6 MiB | 221.4 MiB |
| SSCursor | 1,000,000 | 0.1 MiB | 0.0 MiB | 0.1 MiB |
<!-- results:current-cursors:end -->

### Character sets (`charsets.py`)

<!-- results:current-charsets:start -->
<picture>
  <source media="(prefers-color-scheme: dark)" srcset="results/current-charsets-dark.svg">
  <img alt="Throughput relative to mysqlclient" src="results/current-charsets-light.svg">
</picture>

**CPython 3.15.2** · 8.4.13 · Darwin arm64 · rustmy 0.0.0, mysqlclient 2.3.1, PyMySQL 2.2.8 · best of 5 · 2026-11-16T00:53:30+00:00

| Scenario | rustmy | mysqlclient | PyMySQL | rustmy vs mysqlclient | rustmy vs PyMySQL |
|---|--:|--:|--:|--:|--:|
| fetchall 100k utf8mb4 (es + ru + zh) | **45.3 ms** | 63.1 ms | 128.5 ms | 1.39x | 2.84x |
| fetchall 100k latin1 (Spanish) | **28.0 ms** | 56.4 ms | 99.5 ms | 2.02x | 3.55x |
| fetchall 100k cp1251 (Russian) | **30.9 ms** | 59.0 ms | 101.7 ms | 1.91x | 3.30x |
| fetchall 100k gbk (Chinese) | **35.6 ms** | 55.2 ms | 96.8 ms | 1.55x | 2.72x |
| fetchall 100k latin1 (ASCII int/varchar) | **26.3 ms** | 74.0 ms | 197.0 ms | 2.82x | 7.50x |
| executemany 10k latin1 (Spanish) | **22.1 ms** | 33.4 ms | 35.3 ms | 1.51x | 1.60x |
| executemany 10k cp1251 (Russian) | **23.1 ms** | 32.3 ms | 48.3 ms | 1.40x | 2.09x |
| executemany 10k gbk (Chinese) | **27.5 ms** | 29.3 ms | 37.3 ms | 1.06x | 1.35x |

Times per driver; the fastest is in bold. Ratios above 1 mean rustmy is faster.
<!-- results:current-charsets:end -->

### `conv=` (`conv.py`)

<!-- results:current-conv:start -->
<picture>
  <source media="(prefers-color-scheme: dark)" srcset="results/current-conv-dark.svg">
  <img alt="Throughput relative to mysqlclient" src="results/current-conv-light.svg">
</picture>

**CPython 3.15.2** · 8.4.13 · Darwin arm64 · rustmy 0.0.0, mysqlclient 2.3.1, PyMySQL 2.2.8 · best of 5 · 2026-11-16T00:53:51+00:00

| Scenario | rustmy | mysqlclient | PyMySQL | rustmy vs mysqlclient | rustmy vs PyMySQL |
|---|--:|--:|--:|--:|--:|
| fetchall 100k decimal, no conv | **17.4 ms** | 21.1 ms | 79.7 ms | 1.21x | 4.57x |
| fetchall 100k decimal, conv NEWDECIMAL: float | **10.7 ms** | 16.8 ms | 72.0 ms | 1.57x | 6.72x |
| fetchall 100k int/varchar, no conv | **24.2 ms** | 38.6 ms | 153.4 ms | 1.59x | 6.33x |
| fetchall 100k int/varchar, conv TIME changed (Django) | **24.4 ms** | 39.1 ms | 151.7 ms | 1.60x | 6.21x |
| fetchall 100k int/varchar, conv LONG: Python function | **28.6 ms** | 42.9 ms | 156.1 ms | 1.50x | 5.45x |
| executemany 10k rows, conv encoder for an enum | **15.6 ms** | 22.2 ms | 21.4 ms | 1.42x | 1.37x |

Times per driver; the fastest is in bold. Ratios above 1 mean rustmy is faster.
<!-- results:current-conv:end -->

### SQLAlchemy Core and ORM (`orm.py`)

<!-- results:current-orm:start -->
<picture>
  <source media="(prefers-color-scheme: dark)" srcset="results/current-orm-dark.svg">
  <img alt="Throughput relative to mysqlclient" src="results/current-orm-light.svg">
</picture>

**CPython 3.15.2** · SQLAlchemy 2.1.8 · 8.4.13 · Darwin arm64 · rustmy 0.0.0, mysqlclient 2.3.1, PyMySQL 2.2.8 · best of 10 · 2026-11-16T00:54:35+00:00

| Scenario | rustmy | mysqlclient | PyMySQL | rustmy vs mysqlclient | rustmy vs PyMySQL |
|---|--:|--:|--:|--:|--:|
| Core fetchall 100k int/varchar | **32.9 ms** | 48.1 ms | 162.0 ms | 1.46x | 4.93x |
| Core fetchall 100k mixed | **63.3 ms** | 142.4 ms | 294.5 ms | 2.25x | 4.65x |
| ORM load 10k objects (mixed) | **27.4 ms** | 33.3 ms | 43.7 ms | 1.22x | 1.60x |
| ORM 1,000 x session.get | **125.9 ms** | 132.4 ms | 159.4 ms | 1.05x | 1.27x |
| ORM add_all + commit 10k | **122.5 ms** | 161.8 ms | 147.9 ms | 1.32x | 1.21x |
| Core insert 10k (executemany) | **22.9 ms** | 35.5 ms | 34.9 ms | 1.55x | 1.53x |
| ORM yield_per 100k objects | 186.0 ms | **185.6 ms** | 323.7 ms | 1.00x | 1.74x |

Times per driver; the fastest is in bold. Ratios above 1 mean rustmy is faster.

Peak memory growth while reading the result set, each in a fresh process (buffered: the whole result at once, as `fetchall()` holds it; streaming: iterating):

| Cursor | Rows | rustmy | mysqlclient | PyMySQL |
|---|--:|--:|--:|--:|
| ORM yield_per | 1,000,000 | 4.6 MiB | 6.5 MiB | 4.1 MiB |
<!-- results:current-orm:end -->

## Phase 0 results (2026-11-13, Python 3.10 and 3.14)

Go/no-go criteria from the plan: not slower than mysqlclient on int/varchar,
and at least 1.3x faster on the mixed workload.

Speedup = mysqlclient time / rustmy time; best of 5 runs, MySQL 8.4 in Docker on
the same machine, rustmy built per Python version (not abi3).

<!-- results:phase0:start -->
<picture>
  <source media="(prefers-color-scheme: dark)" srcset="results/phase0-dark.svg">
  <img alt="Throughput relative to mysqlclient" src="results/phase0-light.svg">
</picture>

**CPython 3.10.19** · 8.4.13 · Darwin arm64 · rustmy 0.0.0, mysqlclient 2.3.1, PyMySQL 2.2.8 · best of 5 · 2026-11-13T21:29:06+00:00

| Scenario | rustmy | mysqlclient | PyMySQL | rustmy vs mysqlclient | rustmy vs PyMySQL |
|---|--:|--:|--:|--:|--:|
| fetchall 100k int/varchar | **34.9 ms** | 35.6 ms | 239.7 ms | 1.02x | 6.88x |
| fetchall 100k mixed (double/datetime/date/decimal) | **62.1 ms** | 142.8 ms | 416.8 ms | 2.30x | 6.72x |
| fetchall 100k datetime + date | **18.1 ms** | 96.3 ms | 283.2 ms | 5.32x | 15.64x |
| fetchall 100k decimal | **17.7 ms** | 18.3 ms | 112.2 ms | 1.03x | 6.33x |
| 10k x SELECT 1 | **646.0 ms** | 674.3 ms | 813.7 ms | 1.04x | 1.26x |
| 10k x INSERT with params | **1211.8 ms** | 1213.1 ms | 1328.9 ms | 1.00x | 1.10x |
| executemany 10k rows ¹ | 1220.6 ms | **30.0 ms** | 31.2 ms | 0.02x | 0.03x |

**CPython 3.14.3** · 8.4.13 · Darwin arm64 · rustmy 0.0.0, mysqlclient 2.3.1, PyMySQL 2.2.8 · best of 5 · 2026-11-13T21:29:54+00:00

| Scenario | rustmy | mysqlclient | PyMySQL | rustmy vs mysqlclient | rustmy vs PyMySQL |
|---|--:|--:|--:|--:|--:|
| fetchall 100k int/varchar | 37.0 ms | **35.5 ms** | 153.9 ms | 0.96x | 4.16x |
| fetchall 100k mixed (double/datetime/date/decimal) | **66.0 ms** | 132.8 ms | 296.2 ms | 2.01x | 4.49x |
| fetchall 100k datetime + date | **22.2 ms** | 81.6 ms | 192.6 ms | 3.67x | 8.66x |
| fetchall 100k decimal | **21.5 ms** | 22.1 ms | 83.5 ms | 1.03x | 3.89x |
| 10k x SELECT 1 | **626.5 ms** | 645.5 ms | 786.5 ms | 1.03x | 1.26x |
| 10k x INSERT with params | **1225.3 ms** | 1231.1 ms | 1278.0 ms | 1.00x | 1.04x |
| executemany 10k rows ¹ | 1220.9 ms | 28.4 ms | **28.3 ms** | 0.02x | 0.02x |

Times per driver; the fastest is in bold. Ratios above 1 mean rustmy is faster.
¹ Not comparable yet: rustmy runs one INSERT per row, mysqlclient and PyMySQL batch them into multi-row INSERTs (Phase 2). Left out of the chart.
<!-- results:phase0:end -->

PyMySQL is 2–7x slower than mysqlclient on every fetch scenario.

**Decision: both criteria are met; the project continues.**

Reading the numbers:

- Temporal types are where rustmy wins: mysqlclient calls a Python function
  per `date`/`datetime` cell, rustmy builds them directly through the
  `datetime` C API.
- int/varchar and `DECIMAL` are at parity, as predicted: object creation
  dominates in both drivers (`Decimal` is built by calling the Python class
  in both). Repeated runs land between 0.96x and 1.05x for int/varchar,
  i.e. within noise. There is room to optimise the row loop itself.
- Small queries and single INSERTs are bound by the network round trip
  (here through Docker's VM). Results over a real network will be closer to
  1.0x; a Unix socket would show more of the client-side difference.
- `executemany` is not comparable yet: rustmy runs one INSERT per row while
  mysqlclient rewrites it into multi-row INSERTs. Phase 2 implements the
  rewrite.

### abi3 vs per-version builds

Same machine, CPython 3.14:

| Scenario | per-version | abi3 |
|---|---|---|
| fetchall int/varchar | 1.05x | 0.98x |
| fetchall mixed | 2.02x | 1.60x |
| fetchall datetime + date | 3.7x | 2.2x |

The limited API has no `datetime` C API, so abi3 builds create temporal
objects through Python calls and lose most of rustmy's advantage. **Wheels
are built per Python version** (cp310–cp315 plus free-threaded cp314t and
cp315t).

## Phase 2, block 1: interpolation in Rust and batched `executemany` (2026-11-14, Python 3.10)

`executemany` now batches single-row `INSERT`/`REPLACE … VALUES (…)` statements
into multi-row INSERTs. `benches/executemany.py` sweeps the batch size of rustmy
(`Cursor.max_stmt_length`) against mysqlclient (64 KiB batches) and PyMySQL
(~1 MB) with their defaults; rows are `(int, varchar, double, datetime)`.

```sh
uv run --no-sync python benches/executemany.py --repeat 5 \
    --json benches/results/phase2-block1-executemany-py310.json
uv run --no-sync python benches/report.py --section phase2-block1-executemany \
    benches/results/phase2-block1-executemany-py310.json
```

<!-- results:phase2-block1-executemany:start -->
<picture>
  <source media="(prefers-color-scheme: dark)" srcset="results/phase2-block1-executemany-dark.svg">
  <img alt="Throughput relative to mysqlclient" src="results/phase2-block1-executemany-light.svg">
</picture>

**CPython 3.10.19** · 8.4.13 · Darwin arm64 · rustmy 0.0.0, mysqlclient 2.3.1, PyMySQL 2.2.8 · best of 5 · 2026-11-14T08:45:35+00:00

| Configuration | 10,000 rows | 100,000 rows | vs mysqlclient (10,000) | vs mysqlclient (100,000) |
|---|--:|--:|--:|--:|
| mysqlclient (64 KiB) | 76.1 ms | 613.8 ms | 1.00x | 1.00x |
| PyMySQL (~1 MB) | 48.2 ms | 464.5 ms | 1.58x | 1.32x |
| rustmy 16 KiB | 30.4 ms | 307.6 ms | 2.50x | 2.00x |
| rustmy 64 KiB | 24.3 ms | 255.1 ms | 3.13x | 2.41x |
| rustmy 256 KiB (default) | **23.4 ms** | 250.6 ms | 3.25x | 2.45x |
| rustmy 1 MiB | 23.7 ms | **228.9 ms** | 3.21x | 2.68x |

Times per configuration; the fastest is in bold.
<!-- results:phase2-block1-executemany:end -->

**Decision: 256 KiB**, the smallest batch on par with the best. Over three
runs, 256 KiB and 1 MiB traded first place (100k rows: 221/234/251 ms against
224/239/229 ms, i.e. within noise), while 64 KiB was consistently slower and
noisier. Before this block `executemany` ran one INSERT per row (0.02x against
mysqlclient); it is now 2.2–3.6x faster than mysqlclient.

The general benchmark after the block, now with `executemany` comparable:

```sh
uv run --no-sync python benches/spike.py --repeat 5 \
    --json benches/results/phase2-block1-py310.json
uv run --no-sync python benches/report.py --section phase2-block1 \
    benches/results/phase2-block1-py310.json
```

<!-- results:phase2-block1:start -->
<picture>
  <source media="(prefers-color-scheme: dark)" srcset="results/phase2-block1-dark.svg">
  <img alt="Throughput relative to mysqlclient" src="results/phase2-block1-light.svg">
</picture>

**CPython 3.10.19** · 8.4.13 · Darwin arm64 · rustmy 0.0.0, mysqlclient 2.3.1, PyMySQL 2.2.8 · best of 5 · 2026-11-14T08:46:14+00:00

| Scenario | rustmy | mysqlclient | PyMySQL | rustmy vs mysqlclient | rustmy vs PyMySQL |
|---|--:|--:|--:|--:|--:|
| fetchall 100k int/varchar | **33.0 ms** | 33.3 ms | 242.7 ms | 1.01x | 7.36x |
| fetchall 100k mixed (double/datetime/date/decimal) | **61.5 ms** | 141.1 ms | 422.2 ms | 2.29x | 6.86x |
| fetchall 100k datetime + date | **17.8 ms** | 96.0 ms | 289.2 ms | 5.40x | 16.26x |
| fetchall 100k decimal | **17.4 ms** | 17.7 ms | 114.1 ms | 1.02x | 6.54x |
| 10k x SELECT 1 | **640.9 ms** | 675.1 ms | 815.4 ms | 1.05x | 1.27x |
| 10k x INSERT with params | **1168.0 ms** | 1254.1 ms | 1345.8 ms | 1.07x | 1.15x |
| executemany 10k rows | **17.4 ms** | 31.3 ms | 31.9 ms | 1.80x | 1.84x |

Times per driver; the fastest is in bold. Ratios above 1 mean rustmy is faster.
<!-- results:phase2-block1:end -->

Small queries and single INSERTs are unchanged by moving interpolation to Rust:
they are bound by the network round trip. Fetch scenarios are unchanged within
noise.

## Phase 2, block 2: `DictCursor` and streaming cursors (2026-11-15, Python 3.10)

`benches/cursors.py` compares the cursors of the three drivers. `DictCursor`
builds each row's dict in Rust, with the keys created once per result set.
`SSCursor`/`SSDictCursor` read rows from the socket in batches with the GIL
released and convert them batch by batch; mysqlclient reads (and releases the
GIL for) one row at a time, PyMySQL reads one row at a time in Python.

```sh
uv run --no-sync python benches/cursors.py --repeat 5 --memory \
    --json benches/results/phase2-block2-py310.json \
    --sweep-json benches/results/phase2-block2-batch-py310.json
uv run --no-sync python benches/report.py --section phase2-block2 \
    benches/results/phase2-block2-py310.json
uv run --no-sync python benches/report.py --section phase2-block2-batch \
    benches/results/phase2-block2-batch-py310.json
```

<!-- results:phase2-block2:start -->
<picture>
  <source media="(prefers-color-scheme: dark)" srcset="results/phase2-block2-dark.svg">
  <img alt="Throughput relative to mysqlclient" src="results/phase2-block2-light.svg">
</picture>

**CPython 3.10.19** · 8.4.13 · Darwin arm64 · rustmy 0.0.0, mysqlclient 2.3.1, PyMySQL 2.2.8 · best of 5 · 2026-11-15T19:06:24+00:00

| Scenario | rustmy | mysqlclient | PyMySQL | rustmy vs mysqlclient | rustmy vs PyMySQL |
|---|--:|--:|--:|--:|--:|
| Cursor fetchall 100k int/varchar | **35.2 ms** | 35.4 ms | 238.0 ms | 1.01x | 6.76x |
| DictCursor fetchall 100k int/varchar | 39.4 ms | **39.1 ms** | 261.6 ms | 0.99x | 6.63x |
| DictCursor fetchall 100k mixed | **69.8 ms** | 149.6 ms | 469.2 ms | 2.14x | 6.72x |
| SSCursor iterate 100k int/varchar | **21.0 ms** | 49.4 ms | 283.2 ms | 2.35x | 13.47x |
| SSCursor iterate 100k mixed | **41.2 ms** | 140.7 ms | 467.5 ms | 3.42x | 11.35x |
| SSCursor fetchmany(1000) 100k int/varchar | 21.0 ms | **20.7 ms** | 274.3 ms | 0.99x | 13.06x |
| SSCursor fetchone 100k int/varchar | **29.9 ms** | 47.6 ms | 280.5 ms | 1.59x | 9.39x |
| SSCursor fetchall 100k int/varchar | 23.5 ms | **23.0 ms** | 283.7 ms | 0.98x | 12.06x |
| SSDictCursor iterate 100k int/varchar | **21.4 ms** | 56.6 ms | 300.7 ms | 2.64x | 14.06x |
| SSCursor iterate 1M int/varchar | **133.0 ms** | 420.0 ms | 2078.7 ms | 3.16x | 15.63x |
| SSCursor first row of 1M | 0.3 ms | **0.3 ms** | 0.4 ms | 0.90x | 1.39x |

Times per driver; the fastest is in bold. Ratios above 1 mean rustmy is faster.

Peak memory growth while reading the result set, each in a fresh process (buffered: the whole result at once, as `fetchall()` holds it; streaming: iterating):

| Cursor | Rows | rustmy | mysqlclient | PyMySQL |
|---|--:|--:|--:|--:|
| Cursor | 100,000 | 27.3 MiB | 27.3 MiB | 20.0 MiB |
| SSCursor | 100,000 | 0.1 MiB | 0.0 MiB | 0.1 MiB |
| Cursor | 1,000,000 | 312.2 MiB | 288.8 MiB | 206.2 MiB |
| SSCursor | 1,000,000 | 0.2 MiB | 0.0 MiB | 0.5 MiB |
<!-- results:phase2-block2:end -->

- Dict rows cost ~12% more than tuples in rustmy (~10% in mysqlclient,
  which also caches its keys): parity with mysqlclient on int/varchar and
  2.1x ahead on the mixed workload.
- Iterating a streaming cursor is 2.4–3.4x faster than mysqlclient, whose
  iteration goes through a Python `fetchone()` per row; where mysqlclient
  loops in C (`fetchmany`, `fetchall`) the two are at parity.
- Memory: a streaming cursor's peak stays flat (well under 1 MiB) whatever
  the number of rows; buffered cursors hold the whole result.

The batch size (`SSCursor._batch_bytes`) was swept from 4 KiB to 1 MiB.
Throughput is flat from 4 to 64 KiB and drops slightly above; ~1 KiB rows are
network-bound for every driver. The time to the first row grows with the
batch, since the first batch is read and converted before it is returned:

<!-- results:phase2-block2-batch:start -->
<picture>
  <source media="(prefers-color-scheme: dark)" srcset="results/phase2-block2-batch-dark.svg">
  <img alt="Throughput relative to mysqlclient" src="results/phase2-block2-batch-light.svg">
</picture>

**CPython 3.10.19** · 8.4.13 · Darwin arm64 · rustmy 0.0.0, mysqlclient 2.3.1, PyMySQL 2.2.8 · best of 5 · 2026-11-15T19:06:24+00:00

| Configuration | iterate 100k int/varchar | iterate 100k mixed | iterate 100k 1 KiB | first row of 1M | vs mysqlclient (iterate 100k int/varchar) | vs mysqlclient (iterate 100k mixed) | vs mysqlclient (iterate 100k 1 KiB) | vs mysqlclient (first row of 1M) |
|---|--:|--:|--:|--:|--:|--:|--:|--:|
| mysqlclient (row by row) | 50.5 ms | 143.5 ms | 258.7 ms | **0.2 ms** | 1.00x | 1.00x | 1.00x | 1.00x |
| PyMySQL (row by row) | 284.3 ms | 477.6 ms | 282.6 ms | 0.4 ms | 0.18x | 0.30x | 0.92x | 0.60x |
| rustmy 4 KiB (default) | 21.0 ms | 41.1 ms | 260.5 ms | 0.3 ms | 2.40x | 3.50x | 0.99x | 0.87x |
| rustmy 8 KiB | 20.9 ms | **39.9 ms** | 260.1 ms | 0.3 ms | 2.42x | 3.60x | 0.99x | 0.87x |
| rustmy 16 KiB | 21.0 ms | 40.4 ms | 259.2 ms | 0.4 ms | 2.41x | 3.55x | 1.00x | 0.63x |
| rustmy 64 KiB | **19.6 ms** | 41.4 ms | **253.1 ms** | 0.8 ms | 2.58x | 3.47x | 1.02x | 0.31x |
| rustmy 256 KiB | 21.2 ms | 41.7 ms | 254.9 ms | 2.5 ms | 2.39x | 3.44x | 1.01x | 0.10x |
| rustmy 1 MiB | 22.8 ms | 43.8 ms | 257.2 ms | 8.4 ms | 2.22x | 3.28x | 1.01x | 0.03x |

Times per configuration; the fastest is in bold.
<!-- results:phase2-block2-batch:end -->

**Decision: 4 KiB**, the smallest batch on par with the best, and the only one
as quick as mysqlclient to return the first row. Reading one row per batch
costs only ~10% more on narrow rows (about 20 ns per batch for releasing the
GIL and taking the lock), so small batches carry no hidden cost.

## Phase 2, block 2b: buffered cursors read in batches (2026-11-15, Python 3.10)

Block 2 showed that iterating an `SSCursor` was faster than a buffered
`Cursor.fetchall()`, and an `SSCursor` reading its whole result in one batch
was exactly as slow as the buffered cursor: the gain comes from overlapping
the network with the conversion. While a batch is converted, the server keeps
sending the next one; reading everything first and converting afterwards
serialises the two. The buffered cursors now read in batches too (the
connection stays locked for the whole statement, only the GIL is released
per batch), and check for signals between batches, so Ctrl+C interrupts a
long read.

The general benchmark, before → after (`spike.py`):

```sh
uv run --no-sync python benches/spike.py --repeat 7 \
    --json benches/results/phase2-block2b-py310.json
uv run --no-sync python benches/report.py --section phase2-block2b \
    benches/results/phase2-block2b-py310.json
```

<!-- results:phase2-block2b:start -->
<picture>
  <source media="(prefers-color-scheme: dark)" srcset="results/phase2-block2b-dark.svg">
  <img alt="Throughput relative to mysqlclient" src="results/phase2-block2b-light.svg">
</picture>

**CPython 3.10.19** · 8.4.13 · Darwin arm64 · rustmy 0.0.0, mysqlclient 2.3.1, PyMySQL 2.2.8 · best of 7 · 2026-11-15T19:35:33+00:00

| Scenario | rustmy | mysqlclient | PyMySQL | rustmy vs mysqlclient | rustmy vs PyMySQL |
|---|--:|--:|--:|--:|--:|
| fetchall 100k int/varchar | **23.7 ms** | 32.7 ms | 238.4 ms | 1.38x | 10.08x |
| fetchall 100k mixed (double/datetime/date/decimal) | **44.1 ms** | 139.8 ms | 415.6 ms | 3.17x | 9.42x |
| fetchall 100k datetime + date | **13.7 ms** | 94.5 ms | 278.8 ms | 6.90x | 20.36x |
| fetchall 100k decimal | **11.8 ms** | 17.4 ms | 110.1 ms | 1.48x | 9.35x |
| 10k x SELECT 1 | **644.8 ms** | 674.1 ms | 825.2 ms | 1.05x | 1.28x |
| 10k x INSERT with params | **1168.3 ms** | 1221.9 ms | 1297.8 ms | 1.05x | 1.11x |
| executemany 10k rows | **17.3 ms** | 31.0 ms | 30.8 ms | 1.79x | 1.78x |

Times per driver; the fastest is in bold. Ratios above 1 mean rustmy is faster.
<!-- results:phase2-block2b:end -->

Fetch scenarios went from 1.00x to 1.38x (int/varchar), 2.24x to 3.17x
(mixed), 5.29x to 6.90x (dates) and 1.00x to 1.48x (decimal) against
mysqlclient; small queries are unchanged (one GIL release still covers the
statement and its first batch). The geometric mean went from 1.58x to 1.89x.

The cursor benchmark, now with the buffered cursors' batch size swept:

```sh
uv run --no-sync python benches/cursors.py --repeat 5 --memory \
    --sweep-cursor Cursor \
    --json benches/results/phase2-block2b-cursors-py310.json \
    --sweep-json benches/results/phase2-block2b-batch-py310.json
uv run --no-sync python benches/report.py --section phase2-block2b-cursors \
    benches/results/phase2-block2b-cursors-py310.json
uv run --no-sync python benches/report.py --section phase2-block2b-batch \
    benches/results/phase2-block2b-batch-py310.json
```

<!-- results:phase2-block2b-cursors:start -->
<picture>
  <source media="(prefers-color-scheme: dark)" srcset="results/phase2-block2b-cursors-dark.svg">
  <img alt="Throughput relative to mysqlclient" src="results/phase2-block2b-cursors-light.svg">
</picture>

**CPython 3.10.19** · 8.4.13 · Darwin arm64 · rustmy 0.0.0, mysqlclient 2.3.1, PyMySQL 2.2.8 · best of 5 · 2026-11-15T19:35:50+00:00

| Scenario | rustmy | mysqlclient | PyMySQL | rustmy vs mysqlclient | rustmy vs PyMySQL |
|---|--:|--:|--:|--:|--:|
| Cursor fetchall 100k int/varchar | **22.4 ms** | 35.4 ms | 240.9 ms | 1.58x | 10.75x |
| DictCursor fetchall 100k int/varchar | **25.7 ms** | 38.4 ms | 264.7 ms | 1.50x | 10.30x |
| DictCursor fetchall 100k mixed | **46.6 ms** | 150.4 ms | 468.8 ms | 3.23x | 10.06x |
| SSCursor iterate 100k int/varchar | **20.8 ms** | 49.4 ms | 285.6 ms | 2.37x | 13.70x |
| SSCursor iterate 100k mixed | **39.1 ms** | 141.5 ms | 464.1 ms | 3.62x | 11.87x |
| SSCursor fetchmany(1000) 100k int/varchar | 20.9 ms | **20.6 ms** | 274.2 ms | 0.98x | 13.09x |
| SSCursor fetchone 100k int/varchar | **29.5 ms** | 47.6 ms | 279.9 ms | 1.61x | 9.49x |
| SSCursor fetchall 100k int/varchar | 23.6 ms | **22.8 ms** | 286.3 ms | 0.97x | 12.15x |
| SSDictCursor iterate 100k int/varchar | **21.3 ms** | 57.4 ms | 302.6 ms | 2.70x | 14.21x |
| SSCursor iterate 1M int/varchar | **135.0 ms** | 414.4 ms | 2078.8 ms | 3.07x | 15.40x |
| SSCursor first row of 1M | 0.3 ms | **0.2 ms** | 0.4 ms | 0.90x | 1.38x |

Times per driver; the fastest is in bold. Ratios above 1 mean rustmy is faster.

Peak memory growth while reading the result set, each in a fresh process (buffered: the whole result at once, as `fetchall()` holds it; streaming: iterating):

| Cursor | Rows | rustmy | mysqlclient | PyMySQL |
|---|--:|--:|--:|--:|
| Cursor | 100,000 | 20.1 MiB | 27.7 MiB | 20.0 MiB |
| SSCursor | 100,000 | 0.0 MiB | 0.1 MiB | 0.0 MiB |
| Cursor | 1,000,000 | 199.3 MiB | 288.9 MiB | 206.2 MiB |
| SSCursor | 1,000,000 | 0.1 MiB | 0.0 MiB | 0.5 MiB |
<!-- results:phase2-block2b-cursors:end -->

The buffered read no longer holds the whole raw result next to the Python
objects: the peak for 1M rows went from 316 MiB to 199 MiB, below
mysqlclient (289 MiB) and PyMySQL (206 MiB).

<!-- results:phase2-block2b-batch:start -->
<picture>
  <source media="(prefers-color-scheme: dark)" srcset="results/phase2-block2b-batch-dark.svg">
  <img alt="Throughput relative to mysqlclient" src="results/phase2-block2b-batch-light.svg">
</picture>

**CPython 3.10.19** · 8.4.13 · Darwin arm64 · rustmy 0.0.0, mysqlclient 2.3.1, PyMySQL 2.2.8 · best of 5 · 2026-11-15T19:35:50+00:00

| Configuration | fetchall 100k int/varchar | fetchall 100k mixed | fetchall 100k 1 KiB | vs mysqlclient (fetchall 100k int/varchar) | vs mysqlclient (fetchall 100k mixed) | vs mysqlclient (fetchall 100k 1 KiB) |
|---|--:|--:|--:|--:|--:|--:|
| mysqlclient | 34.2 ms | 143.0 ms | 287.7 ms | 1.00x | 1.00x | 1.00x |
| PyMySQL | 259.6 ms | 444.6 ms | 287.5 ms | 0.13x | 0.32x | 1.00x |
| rustmy 4 KiB (default) | 23.9 ms | 43.2 ms | 273.1 ms | 1.43x | 3.31x | 1.05x |
| rustmy 8 KiB | 23.4 ms | 43.1 ms | 272.6 ms | 1.46x | 3.32x | 1.06x |
| rustmy 16 KiB | **22.5 ms** | **42.7 ms** | 272.1 ms | 1.52x | 3.35x | 1.06x |
| rustmy 64 KiB | 23.4 ms | 43.7 ms | 270.0 ms | 1.46x | 3.27x | 1.07x |
| rustmy 256 KiB | 23.6 ms | 43.8 ms | 270.5 ms | 1.45x | 3.26x | 1.06x |
| rustmy 1 MiB | 25.7 ms | 45.7 ms | **260.9 ms** | 1.33x | 3.13x | 1.10x |

Times per configuration; the fastest is in bold.
<!-- results:phase2-block2b-batch:end -->

**Decision: 4 KiB** for buffered cursors too (one setting, `Cursor._batch_bytes`,
shared with the streaming cursors): every size from 4 to 256 KiB is within
noise, 1 MiB is slower.

## Phase 2, block 3a: connection character sets (2026-11-15, Python 3.10)

Any MySQL character set with a Python codec can now be the connection's.
Single-byte ones (latin1, cp1251…) are decoded and encoded in Rust through a
256-entry table built at connect time from Python's own codec, so results
are identical to Python's without calling it per cell; multi-byte ones (gbk,
big5, sjis…) call CPython's decoder directly. ASCII text skips both.
mysqlclient and PyMySQL decode every text cell through Python's codec
machinery, ASCII included.

`benches/charsets.py` reads text stored in utf8mb4 columns through
connections in each character set (the server converts it, so every driver
decodes the same bytes) and inserts it with `executemany`:

```sh
uv run --no-sync python benches/charsets.py --repeat 5 \
    --json benches/results/phase2-block3a-charsets-py310.json
uv run --no-sync python benches/report.py --section phase2-block3a-charsets \
    benches/results/phase2-block3a-charsets-py310.json
```

<!-- results:phase2-block3a-charsets:start -->
<picture>
  <source media="(prefers-color-scheme: dark)" srcset="results/phase2-block3a-charsets-dark.svg">
  <img alt="Throughput relative to mysqlclient" src="results/phase2-block3a-charsets-light.svg">
</picture>

**CPython 3.10.19** · 8.4.13 · Darwin arm64 · rustmy 0.0.0, mysqlclient 2.3.1, PyMySQL 2.2.8 · best of 5 · 2026-11-15T20:22:51+00:00

| Scenario | rustmy | mysqlclient | PyMySQL | rustmy vs mysqlclient | rustmy vs PyMySQL |
|---|--:|--:|--:|--:|--:|
| fetchall 100k utf8mb4 (es + ru + zh) | **47.8 ms** | 60.6 ms | 194.4 ms | 1.27x | 4.06x |
| fetchall 100k latin1 (Spanish) | **28.9 ms** | 51.2 ms | 128.2 ms | 1.77x | 4.43x |
| fetchall 100k cp1251 (Russian) | **31.4 ms** | 56.2 ms | 134.6 ms | 1.79x | 4.29x |
| fetchall 100k gbk (Chinese) | **28.8 ms** | 46.6 ms | 131.1 ms | 1.62x | 4.55x |
| fetchall 100k latin1 (ASCII int/varchar) | **25.0 ms** | 65.5 ms | 287.9 ms | 2.62x | 11.53x |
| executemany 10k latin1 (Spanish) | **23.0 ms** | 35.1 ms | 35.9 ms | 1.53x | 1.57x |
| executemany 10k cp1251 (Russian) | **22.6 ms** | 33.5 ms | 38.3 ms | 1.49x | 1.70x |

Times per driver; the fastest is in bold. Ratios above 1 mean rustmy is faster.
<!-- results:phase2-block3a-charsets:end -->

The UTF-8 path is unchanged (`spike.py` after the block; three runs gave
int/varchar 22.8–24.1 ms against 22.4–23.7 ms before, i.e. within noise):

```sh
uv run --no-sync python benches/spike.py --repeat 7 \
    --json benches/results/phase2-block3a-py310.json
uv run --no-sync python benches/report.py --section phase2-block3a \
    benches/results/phase2-block3a-py310.json
```

<!-- results:phase2-block3a:start -->
<picture>
  <source media="(prefers-color-scheme: dark)" srcset="results/phase2-block3a-dark.svg">
  <img alt="Throughput relative to mysqlclient" src="results/phase2-block3a-light.svg">
</picture>

**CPython 3.10.19** · 8.4.13 · Darwin arm64 · rustmy 0.0.0, mysqlclient 2.3.1, PyMySQL 2.2.8 · best of 7 · 2026-11-15T20:23:55+00:00

| Scenario | rustmy | mysqlclient | PyMySQL | rustmy vs mysqlclient | rustmy vs PyMySQL |
|---|--:|--:|--:|--:|--:|
| fetchall 100k int/varchar | **23.6 ms** | 34.3 ms | 231.1 ms | 1.45x | 9.81x |
| fetchall 100k mixed (double/datetime/date/decimal) | **43.8 ms** | 139.3 ms | 403.3 ms | 3.18x | 9.21x |
| fetchall 100k datetime + date | **13.2 ms** | 92.5 ms | 272.2 ms | 7.02x | 20.64x |
| fetchall 100k decimal | **11.5 ms** | 17.3 ms | 107.9 ms | 1.51x | 9.41x |
| 10k x SELECT 1 | **650.9 ms** | 689.5 ms | 822.9 ms | 1.06x | 1.26x |
| 10k x INSERT with params | **1182.1 ms** | 1208.7 ms | 1329.1 ms | 1.02x | 1.12x |
| executemany 10k rows | **17.0 ms** | 30.5 ms | 30.9 ms | 1.80x | 1.82x |

Times per driver; the fastest is in bold. Ratios above 1 mean rustmy is faster.
<!-- results:phase2-block3a:end -->

## Phase 2, block 3b: `conv=` and the rest of `connect()` (2026-11-15, Python 3.10)

`conv=` takes MySQLdb's format (column types to decoders, Python types to
encoders). Entries equal to `rustmy.converters.conversions` stay on the
native path, so a mapping that changes one type only slows down the columns
of that type. `benches/conv.py` gives each driver its own default mapping
plus the same change:

```sh
uv run --no-sync python benches/conv.py --repeat 5 \
    --json benches/results/phase2-block3b-conv-py310.json
uv run --no-sync python benches/report.py --section phase2-block3b-conv \
    benches/results/phase2-block3b-conv-py310.json
```

<!-- results:phase2-block3b-conv:start -->
<picture>
  <source media="(prefers-color-scheme: dark)" srcset="results/phase2-block3b-conv-dark.svg">
  <img alt="Throughput relative to mysqlclient" src="results/phase2-block3b-conv-light.svg">
</picture>

**CPython 3.10.19** · 8.4.13 · Darwin arm64 · rustmy 0.0.0, mysqlclient 2.3.1, PyMySQL 2.2.8 · best of 5 · 2026-11-15T20:53:57+00:00

| Scenario | rustmy | mysqlclient | PyMySQL | rustmy vs mysqlclient | rustmy vs PyMySQL |
|---|--:|--:|--:|--:|--:|
| fetchall 100k decimal, no conv | **12.8 ms** | 17.9 ms | 102.1 ms | 1.40x | 7.96x |
| fetchall 100k decimal, conv NEWDECIMAL: float | **9.6 ms** | 14.7 ms | 99.9 ms | 1.54x | 10.45x |
| fetchall 100k int/varchar, no conv | **24.7 ms** | 34.4 ms | 248.2 ms | 1.39x | 10.06x |
| fetchall 100k int/varchar, conv TIME changed (Django) | **23.7 ms** | 33.4 ms | 248.0 ms | 1.41x | 10.46x |
| fetchall 100k int/varchar, conv LONG: Python function | **32.0 ms** | 43.8 ms | 255.8 ms | 1.37x | 7.98x |
| executemany 10k rows, conv encoder for an enum | **18.7 ms** | 24.0 ms | 24.4 ms | 1.28x | 1.31x |

Times per driver; the fastest is in bold. Ratios above 1 mean rustmy is faster.
<!-- results:phase2-block3b-conv:end -->

The Django-style mapping (`TIME` changed, on a table without `TIME` columns)
costs what no `conv=` costs. A Python decoder on an integer column adds about
40 ns per value (mysqlclient about 100 ns).

`spike.py` after the block, unchanged within noise (geometric mean 1.91x
against 1.92x before):

```sh
uv run --no-sync python benches/spike.py --repeat 7 \
    --json benches/results/phase2-block3b-py310.json
uv run --no-sync python benches/report.py --section phase2-block3b \
    benches/results/phase2-block3b-py310.json
```

<!-- results:phase2-block3b:start -->
<picture>
  <source media="(prefers-color-scheme: dark)" srcset="results/phase2-block3b-dark.svg">
  <img alt="Throughput relative to mysqlclient" src="results/phase2-block3b-light.svg">
</picture>

**CPython 3.10.19** · 8.4.13 · Darwin arm64 · rustmy 0.0.0, mysqlclient 2.3.1, PyMySQL 2.2.8 · best of 7 · 2026-11-15T20:54:51+00:00

| Scenario | rustmy | mysqlclient | PyMySQL | rustmy vs mysqlclient | rustmy vs PyMySQL |
|---|--:|--:|--:|--:|--:|
| fetchall 100k int/varchar | **23.7 ms** | 35.1 ms | 233.1 ms | 1.48x | 9.84x |
| fetchall 100k mixed (double/datetime/date/decimal) | **43.5 ms** | 139.5 ms | 414.7 ms | 3.21x | 9.53x |
| fetchall 100k datetime + date | **13.3 ms** | 93.4 ms | 278.6 ms | 7.02x | 20.92x |
| fetchall 100k decimal | **11.7 ms** | 17.6 ms | 109.4 ms | 1.51x | 9.38x |
| 10k x SELECT 1 | **655.7 ms** | 679.6 ms | 818.0 ms | 1.04x | 1.25x |
| 10k x INSERT with params | **1201.1 ms** | 1220.2 ms | 1317.6 ms | 1.02x | 1.10x |
| executemany 10k rows | **17.9 ms** | 30.8 ms | 31.3 ms | 1.72x | 1.75x |

Times per driver; the fastest is in bold. Ratios above 1 mean rustmy is faster.
<!-- results:phase2-block3b:end -->

## Phase 2, block 4: character sets follow the server (2026-11-15, Python 3.10)

Block 4's tests found that Python's codecs and the server disagree on a few
characters. In sjis and ujis, Python encodes `¥` as 0x5C, which the server
reads as a backslash, so escaping the text before encoding it let a quote
through. In multi-byte character sets, encoded text must now decode back to
itself, or it is refused. That costs one more decode, only for non-ASCII
text in multi-byte sessions. `charsets.py` gained an `executemany` scenario
in gbk to measure it: 23.3 ms before and 27.0 ms after (1.36x → 1.18x
against mysqlclient). Every other scenario is within noise of the run
before the change (same machine, same server, minutes apart):

```sh
uv run --no-sync python benches/charsets.py --repeat 5 \
    --json benches/results/phase2-block4-charsets-py310.json
uv run --no-sync python benches/report.py --section phase2-block4-charsets \
    benches/results/phase2-block4-charsets-py310.json
```

<!-- results:phase2-block4-charsets:start -->
<picture>
  <source media="(prefers-color-scheme: dark)" srcset="results/phase2-block4-charsets-dark.svg">
  <img alt="Throughput relative to mysqlclient" src="results/phase2-block4-charsets-light.svg">
</picture>

**CPython 3.10.19** · 8.4.13 · Darwin arm64 · rustmy 0.0.0, mysqlclient 2.3.1, PyMySQL 2.2.8 · best of 5 · 2026-11-15T21:49:25+00:00

| Scenario | rustmy | mysqlclient | PyMySQL | rustmy vs mysqlclient | rustmy vs PyMySQL |
|---|--:|--:|--:|--:|--:|
| fetchall 100k utf8mb4 (es + ru + zh) | **46.5 ms** | 59.0 ms | 196.1 ms | 1.27x | 4.22x |
| fetchall 100k latin1 (Spanish) | **28.9 ms** | 52.0 ms | 130.9 ms | 1.80x | 4.52x |
| fetchall 100k cp1251 (Russian) | **30.9 ms** | 55.7 ms | 139.2 ms | 1.80x | 4.50x |
| fetchall 100k gbk (Chinese) | **29.5 ms** | 47.4 ms | 133.8 ms | 1.61x | 4.54x |
| fetchall 100k latin1 (ASCII int/varchar) | **25.8 ms** | 66.7 ms | 292.6 ms | 2.59x | 11.35x |
| executemany 10k latin1 (Spanish) | **21.5 ms** | 35.9 ms | 38.1 ms | 1.67x | 1.77x |
| executemany 10k cp1251 (Russian) | **23.1 ms** | 34.8 ms | 39.0 ms | 1.51x | 1.69x |
| executemany 10k gbk (Chinese) | **27.0 ms** | 32.0 ms | 33.4 ms | 1.18x | 1.24x |

Times per driver; the fastest is in bold. Ratios above 1 mean rustmy is faster.
<!-- results:phase2-block4-charsets:end -->

## Phase 3: SQLAlchemy Core and ORM (2026-11-16, Python 3.10)

The same work through SQLAlchemy, with each driver's dialect
(`mysql+rustmy`, `mysql+mysqldb`, `mysql+pymysql`). SQLAlchemy's own work
(compiling statements, building rows and ORM objects, the unit of work) is
the same for every driver and is included in each time, so these ratios are
what an application sees rather than the driver alone, and they are smaller
than in the sections above. Python 3.10, like the other blocks, so it runs
SQLAlchemy 2.0 (2.1 needs Python 3.11+; "Current results" has 3.15 with
SQLAlchemy 2.1):

```sh
uv run --no-sync python benches/orm.py --repeat 10 --memory \
    --json benches/results/phase3-orm-py310.json
uv run --no-sync python benches/report.py --section phase3-orm \
    benches/results/phase3-orm-py310.json
```

<!-- results:phase3-orm:start -->
<picture>
  <source media="(prefers-color-scheme: dark)" srcset="results/phase3-orm-dark.svg">
  <img alt="Throughput relative to mysqlclient" src="results/phase3-orm-light.svg">
</picture>

**CPython 3.10.19** · SQLAlchemy 2.0.57 · 8.4.13 · Darwin arm64 · rustmy 0.0.0, mysqlclient 2.3.1, PyMySQL 2.2.8 · best of 10 · 2026-11-16T00:50:49+00:00

| Scenario | rustmy | mysqlclient | PyMySQL | rustmy vs mysqlclient | rustmy vs PyMySQL |
|---|--:|--:|--:|--:|--:|
| Core fetchall 100k int/varchar | **39.8 ms** | 50.7 ms | 258.3 ms | 1.27x | 6.49x |
| Core fetchall 100k mixed | **77.2 ms** | 179.5 ms | 480.2 ms | 2.33x | 6.22x |
| ORM load 10k objects (mixed) | **31.3 ms** | 45.3 ms | 71.9 ms | 1.45x | 2.30x |
| ORM 1,000 x session.get | **164.1 ms** | 170.0 ms | 207.0 ms | 1.04x | 1.26x |
| ORM add_all + commit 10k | **156.7 ms** | 179.2 ms | 187.1 ms | 1.14x | 1.19x |
| Core insert 10k (executemany) | **23.9 ms** | 37.4 ms | 39.4 ms | 1.57x | 1.65x |
| ORM yield_per 100k objects | **266.7 ms** | 268.9 ms | 538.1 ms | 1.01x | 2.02x |

Times per driver; the fastest is in bold. Ratios above 1 mean rustmy is faster.

Peak memory growth while reading the result set, each in a fresh process (buffered: the whole result at once, as `fetchall()` holds it; streaming: iterating):

| Cursor | Rows | rustmy | mysqlclient | PyMySQL |
|---|--:|--:|--:|--:|
| ORM yield_per | 1,000,000 | 4.8 MiB | 4.8 MiB | 7.1 MiB |
<!-- results:phase3-orm:end -->

What the numbers say: rustmy's lead survives where the driver's share of the
work is large (Core reads, bulk inserts); where SQLAlchemy builds an object
per row or runs one small query at a time, the drivers converge. Two caveats:

- "ORM load 10k objects" is noisy. On Python 3.15, three runs of the whole
  script gave 0.95x (best of 5), 1.63x and 1.22x (best of 10), and repeating
  the scenario alone gave rustmy 23.7–27.5 ms against mysqlclient's
  27.9–28.4 ms: creating thousands of ORM objects triggers the garbage
  collector at varying points. Read it as "on par or ahead".
- `yield_per` streams through each driver's server-side cursor and fetches
  in chunks with `fetchmany()`, where rustmy and mysqlclient are on par
  (block 2: mysqlclient runs that loop in C). The memory stays flat for all
  three drivers.

## Phase 5, block 2: pipelined `executemany` for `UPDATE`/`DELETE` (2026-11-17, Python 3.10)

`executemany` of anything but `INSERT … VALUES` ran one statement per item in
every driver, each waiting for its reply: one network round trip per row.
rustmy now sends up to `Cursor.max_pipeline` statements (256, at most 16 KiB)
before reading their replies. `pipeline.py` measures it through
[toxiproxy](https://github.com/Shopify/toxiproxy) (the compose service of
the `bench` profile), which can delay every reply from the server: the
first panel adds nothing (the proxy's own hop, about 0.3 ms per round
trip), the second adds 1 ms per reply, about 3 ms per round trip through
Docker Desktop. Before this block all three drivers were on par in every
scenario (0.97–1.04x against mysqlclient, `phase5-block2-before-*.json`).

```sh
docker compose --profile bench up -d --wait mysql toxiproxy
uv run --no-sync python benches/pipeline.py --latency 0 --rows 1000 10000 \
    --repeat 5 --sweep --json benches/results/phase5-block2-after-0ms-py310.json
uv run --no-sync python benches/pipeline.py --latency 1 --rows 1000 10000 \
    --repeat 3 --sweep --json benches/results/phase5-block2-after-1ms-py310.json
uv run --no-sync python benches/report.py --section phase5-block2 \
    benches/results/phase5-block2-after-0ms-py310.json \
    benches/results/phase5-block2-after-1ms-py310.json
```

<!-- results:phase5-block2:start -->
<picture>
  <source media="(prefers-color-scheme: dark)" srcset="results/phase5-block2-dark.svg">
  <img alt="Throughput relative to mysqlclient" src="results/phase5-block2-light.svg">
</picture>

**CPython 3.10.19** · +0 ms per reply (0.3 ms round trip) · 8.4.13 · Darwin arm64 · rustmy 0.0.0, mysqlclient 2.3.1, PyMySQL 2.2.8 · best of 5 · 2026-11-17T17:50:48+00:00

| Scenario | rustmy | mysqlclient | PyMySQL | rustmy vs mysqlclient | rustmy vs PyMySQL |
|---|--:|--:|--:|--:|--:|
| UPDATE by primary key (1,000 rows) | **80.8 ms** | 153.3 ms | 167.7 ms | 1.90x | 2.07x |
| UPDATE matching no row (1,000 rows) | **10.9 ms** | 98.4 ms | 105.8 ms | 9.06x | 9.74x |
| DELETE by primary key (1,000 rows) | **75.7 ms** | 142.0 ms | 156.6 ms | 1.88x | 2.07x |
| UPDATE by primary key (10,000 rows) | **819.8 ms** | 1592.5 ms | 1705.1 ms | 1.94x | 2.08x |
| UPDATE matching no row (10,000 rows) | **112.1 ms** | 965.2 ms | 1066.2 ms | 8.61x | 9.51x |
| DELETE by primary key (10,000 rows) | **762.7 ms** | 1481.1 ms | 1615.8 ms | 1.94x | 2.12x |

**CPython 3.10.19** · +1 ms per reply (3.0 ms round trip) · 8.4.13 · Darwin arm64 · rustmy 0.0.0, mysqlclient 2.3.1, PyMySQL 2.2.8 · best of 3 · 2026-11-17T17:45:03+00:00

| Scenario | rustmy | mysqlclient | PyMySQL | rustmy vs mysqlclient | rustmy vs PyMySQL |
|---|--:|--:|--:|--:|--:|
| UPDATE by primary key (1,000 rows) | **91.0 ms** | 3156.3 ms | 3119.3 ms | 34.67x | 34.27x |
| UPDATE matching no row (1,000 rows) | **22.5 ms** | 2969.5 ms | 2960.6 ms | 131.94x | 131.54x |
| DELETE by primary key (1,000 rows) | **89.6 ms** | 3146.2 ms | 3075.0 ms | 35.11x | 34.31x |
| UPDATE by primary key (10,000 rows) | **949.4 ms** | 31809.0 ms | 31733.9 ms | 33.50x | 33.42x |
| UPDATE matching no row (10,000 rows) | **243.9 ms** | 31137.5 ms | 30931.4 ms | 127.65x | 126.81x |
| DELETE by primary key (10,000 rows) | **823.5 ms** | 32516.7 ms | 33038.5 ms | 39.49x | 40.12x |

Times per driver; the fastest is in bold. Ratios above 1 mean rustmy is faster.

`max_pipeline` sweep, +0 ms per reply (0.3 ms round trip):

| Scenario | 1 | 16 | 64 | 256 | 1024 |
|---|--:|--:|--:|--:|--:|
| UPDATE by primary key (1,000 rows) | 142.2 ms | 82.6 ms | 81.8 ms | 84.6 ms | **79.2 ms** |
| UPDATE matching no row (1,000 rows) | 88.8 ms | 13.8 ms | 11.1 ms | **8.1 ms** | 10.2 ms |
| DELETE by primary key (1,000 rows) | 137.6 ms | 72.3 ms | **67.0 ms** | 71.2 ms | 73.5 ms |
| UPDATE by primary key (10,000 rows) | 1551.4 ms | 903.4 ms | 857.0 ms | **836.1 ms** | 851.1 ms |
| UPDATE matching no row (10,000 rows) | 881.6 ms | 174.1 ms | 128.1 ms | **107.7 ms** | 122.3 ms |
| DELETE by primary key (10,000 rows) | 1446.8 ms | 799.8 ms | 780.4 ms | **750.7 ms** | 760.8 ms |

`max_pipeline` sweep, +1 ms per reply (3.0 ms round trip):

| Scenario | 1 | 16 | 64 | 256 | 1024 |
|---|--:|--:|--:|--:|--:|
| UPDATE by primary key (1,000 rows) | 3206.5 ms | 307.8 ms | 136.9 ms | 87.4 ms | **85.4 ms** |
| UPDATE matching no row (1,000 rows) | 2964.8 ms | 247.5 ms | 93.7 ms | 31.1 ms | **22.2 ms** |
| DELETE by primary key (1,000 rows) | 3119.8 ms | 388.0 ms | 124.3 ms | 87.1 ms | **78.2 ms** |
| UPDATE by primary key (10,000 rows) | 35032.8 ms | 3947.8 ms | 1264.6 ms | 895.6 ms | **890.8 ms** |
| UPDATE matching no row (10,000 rows) | 30429.2 ms | 2498.4 ms | 792.3 ms | 252.4 ms | **185.9 ms** |
| DELETE by primary key (10,000 rows) | 33444.8 ms | 3930.1 ms | 1174.7 ms | 815.5 ms | **788.3 ms** |
<!-- results:phase5-block2:end -->

## Phase 5, block 3: prepared statements (2026-11-17, Python 3.10)

`PreparedCursor` runs server-side prepared statements over the binary
protocol: typed parameters, binary rows, one prepare per statement and
connection. `prepared.py` compares it with rustmy's own `Cursor`,
mysqlclient and PyMySQL on the same table (int, bigint, varchar, double,
datetime, date, decimal), every scenario passing its values as parameters:

```sh
uv run --no-sync python benches/prepared.py --repeat 5 \
    --json benches/results/phase5-block3-prepared-py310.json
uv run --no-sync python benches/prepared.py --repeat 5 --port 13307 \
    --json benches/results/phase5-block3-prepared-mariadb-py310.json
uv run --no-sync python benches/report.py --section phase5-block3 \
    benches/results/phase5-block3-prepared-py310.json
uv run --no-sync python benches/report.py --section phase5-block3-mariadb \
    --output benches/results/phase5-block3-mariadb \
    benches/results/phase5-block3-prepared-mariadb-py310.json
```

MySQL 8.4:

<!-- results:phase5-block3:start -->
<picture>
  <source media="(prefers-color-scheme: dark)" srcset="results/phase5-block3-dark.svg">
  <img alt="Throughput relative to mysqlclient" src="results/phase5-block3-light.svg">
</picture>

**CPython 3.10.19** · 8.4.13 · Darwin arm64 · rustmy 0.0.0, mysqlclient 2.3.1, PyMySQL 2.2.8 · best of 5 · 2026-11-17T21:31:07+00:00

| Configuration | fetchall 100k mixed rows | 10k SELECT by primary key | executemany INSERT 10k rows | executemany UPDATE 10k rows | vs mysqlclient (fetchall 100k mixed rows) | vs mysqlclient (10k SELECT by primary key) | vs mysqlclient (executemany INSERT 10k rows) | vs mysqlclient (executemany UPDATE 10k rows) |
|---|--:|--:|--:|--:|--:|--:|--:|--:|
| mysqlclient | 162.6 ms | 940.2 ms | 71.1 ms | 1237.6 ms | 1.00x | 1.00x | 1.00x | 1.00x |
| PyMySQL | 473.9 ms | 1261.9 ms | 73.7 ms | 1182.9 ms | 0.34x | 0.75x | 0.96x | 1.05x |
| rustmy Cursor (default) | 45.1 ms | 807.6 ms | **36.6 ms** | **684.6 ms** | 3.60x | 1.16x | 1.94x | 1.81x |
| rustmy PreparedCursor | **25.0 ms** | **739.1 ms** | 53.1 ms | 690.0 ms | 6.51x | 1.27x | 1.34x | 1.79x |

Times per configuration; the fastest is in bold.
<!-- results:phase5-block3:end -->

MariaDB 11.4:

<!-- results:phase5-block3-mariadb:start -->
<picture>
  <source media="(prefers-color-scheme: dark)" srcset="results/phase5-block3-mariadb-dark.svg">
  <img alt="Throughput relative to mysqlclient" src="results/phase5-block3-mariadb-light.svg">
</picture>

**CPython 3.10.19** · 11.4.13-MariaDB-ubu2404 · Darwin arm64 · rustmy 0.0.0, mysqlclient 2.3.1, PyMySQL 2.2.8 · best of 5 · 2026-11-17T21:31:57+00:00

| Configuration | fetchall 100k mixed rows | 10k SELECT by primary key | executemany INSERT 10k rows | executemany UPDATE 10k rows | vs mysqlclient (fetchall 100k mixed rows) | vs mysqlclient (10k SELECT by primary key) | vs mysqlclient (executemany INSERT 10k rows) | vs mysqlclient (executemany UPDATE 10k rows) |
|---|--:|--:|--:|--:|--:|--:|--:|--:|
| mysqlclient | 164.9 ms | 818.8 ms | 60.8 ms | 777.0 ms | 1.00x | 1.00x | 1.00x | 1.00x |
| PyMySQL | 469.5 ms | 1256.4 ms | 69.1 ms | 862.7 ms | 0.35x | 0.65x | 0.88x | 0.90x |
| rustmy Cursor (default) | 47.4 ms | 788.5 ms | 25.8 ms | 232.9 ms | 3.48x | 1.04x | 2.35x | 3.34x |
| rustmy PreparedCursor | **23.8 ms** | **754.8 ms** | **18.8 ms** | **215.4 ms** | 6.94x | 1.08x | 3.23x | 3.61x |

Times per configuration; the fastest is in bold.
<!-- results:phase5-block3-mariadb:end -->

What the numbers say: reading is where prepared statements pay off, with
about half the time of `Cursor` for the same rows (the server sends numbers
and dates in binary, a third less data here, and nothing is parsed). A
statement already prepared saves the server its parsing, which shows a
little in short lookups. Batches are on par with `Cursor`: `UPDATE`s are
pipelined the same way, and `INSERT … VALUES` becomes multi-row prepared
statements, faster than `Cursor`'s multi-row text on MariaDB and slower on
MySQL (still ahead of both other drivers).

## Phase 5, block 4: compression (2026-11-18, Python 3.10)

`compress.py` reads 100,000 mixed rows (the table of block 3), runs 1,000
primary-key lookups and inserts 10,000 rows with `executemany`, with rustmy
uncompressed, zlib and zstd against mysqlclient without and with
`compress=True` (zlib; PyMySQL has no compressed protocol). Locally the
drivers connect straight to the server; the capped runs go through the
`toxiproxy` service of compose's `bench` profile, whose bandwidth toxic caps
each direction:

```sh
docker compose --profile bench up -d --wait mysql mariadb toxiproxy
uv run --no-sync python benches/compress.py \
    --json benches/results/phase5-block4-compress-local-py310.json
uv run --no-sync python benches/compress.py --bandwidth 12500 \
    --json benches/results/phase5-block4-compress-100mbit-py310.json
uv run --no-sync python benches/compress.py --bandwidth 2500 \
    --json benches/results/phase5-block4-compress-20mbit-py310.json
uv run --no-sync python benches/compress.py --port 13307 --bandwidth 12500 \
    --json benches/results/phase5-block4-compress-mariadb-100mbit-py310.json
for run in local 100mbit 20mbit mariadb-100mbit; do
  uv run --no-sync python benches/report.py --section phase5-block4-$run \
      benches/results/phase5-block4-compress-$run-py310.json
done
```

MySQL 8.4, local (no cap):

<!-- results:phase5-block4-local:start -->
<picture>
  <source media="(prefers-color-scheme: dark)" srcset="results/phase5-block4-local-dark.svg">
  <img alt="Throughput relative to mysqlclient" src="results/phase5-block4-local-light.svg">
</picture>

**CPython 3.10.19** · 8.4.13 · Darwin arm64 · rustmy 0.0.0, mysqlclient 2.3.1 · best of 5 · 2026-11-18T10:19:12+00:00

| Configuration | fetchall 100k mixed rows | 1,000 SELECT by primary key | executemany INSERT 10k rows | vs mysqlclient (fetchall 100k mixed rows) | vs mysqlclient (1,000 SELECT by primary key) | vs mysqlclient (executemany INSERT 10k rows) |
|---|--:|--:|--:|--:|--:|--:|
| mysqlclient | 161.2 ms | 83.6 ms | 71.2 ms | 1.00x | 1.00x | 1.00x |
| mysqlclient zlib | 297.1 ms | 111.6 ms | 126.6 ms | 0.54x | 0.75x | 0.56x |
| rustmy (default) | **49.6 ms** | **79.3 ms** | **35.6 ms** | 3.25x | 1.05x | 2.00x |
| rustmy zlib | 179.1 ms | 94.7 ms | 43.2 ms | 0.90x | 0.88x | 1.65x |
| rustmy zstd | 77.8 ms | 88.7 ms | 39.0 ms | 2.07x | 0.94x | 1.82x |

Times per configuration; the fastest is in bold.
<!-- results:phase5-block4-local:end -->

MySQL 8.4, 100 Mbit/s each way:

<!-- results:phase5-block4-100mbit:start -->
<picture>
  <source media="(prefers-color-scheme: dark)" srcset="results/phase5-block4-100mbit-dark.svg">
  <img alt="Throughput relative to mysqlclient" src="results/phase5-block4-100mbit-light.svg">
</picture>

**CPython 3.10.19** · 8.4.13 · Darwin arm64 · rustmy 0.0.0, mysqlclient 2.3.1 · best of 5 · 2026-11-18T10:19:47+00:00

| Configuration | fetchall 100k mixed rows | 1,000 SELECT by primary key | executemany INSERT 10k rows | vs mysqlclient (fetchall 100k mixed rows) | vs mysqlclient (1,000 SELECT by primary key) | vs mysqlclient (executemany INSERT 10k rows) |
|---|--:|--:|--:|--:|--:|--:|
| mysqlclient | 906.0 ms | 146.8 ms | 169.8 ms | 1.00x | 1.00x | 1.00x |
| mysqlclient zlib | 309.0 ms | 161.0 ms | 152.2 ms | 2.93x | 0.91x | 1.12x |
| rustmy (default) | 776.6 ms | 147.9 ms | 147.3 ms | 1.17x | 0.99x | 1.15x |
| rustmy zlib | 195.5 ms | 142.2 ms | 75.4 ms | 4.63x | 1.03x | 2.25x |
| rustmy zstd | **173.0 ms** | **137.1 ms** | **56.5 ms** | 5.24x | 1.07x | 3.01x |

Times per configuration; the fastest is in bold.
<!-- results:phase5-block4-100mbit:end -->

MySQL 8.4, 20 Mbit/s each way:

<!-- results:phase5-block4-20mbit:start -->
<picture>
  <source media="(prefers-color-scheme: dark)" srcset="results/phase5-block4-20mbit-dark.svg">
  <img alt="Throughput relative to mysqlclient" src="results/phase5-block4-20mbit-light.svg">
</picture>

**CPython 3.10.19** · 8.4.13 · Darwin arm64 · rustmy 0.0.0, mysqlclient 2.3.1 · best of 5 · 2026-11-18T10:21:17+00:00

| Configuration | fetchall 100k mixed rows | 1,000 SELECT by primary key | executemany INSERT 10k rows | vs mysqlclient (fetchall 100k mixed rows) | vs mysqlclient (1,000 SELECT by primary key) | vs mysqlclient (executemany INSERT 10k rows) |
|---|--:|--:|--:|--:|--:|--:|
| mysqlclient | 3967.8 ms | 509.1 ms | 701.1 ms | 1.00x | 1.00x | 1.00x |
| mysqlclient zlib | 1079.5 ms | 308.5 ms | 250.1 ms | 3.68x | 1.65x | 2.80x |
| rustmy (default) | 3837.2 ms | 492.9 ms | 567.2 ms | 1.03x | 1.03x | 1.24x |
| rustmy zlib | 949.0 ms | **293.5 ms** | 183.9 ms | 4.18x | 1.73x | 3.81x |
| rustmy zstd | **856.0 ms** | 305.6 ms | **147.6 ms** | 4.64x | 1.67x | 4.75x |

Times per configuration; the fastest is in bold.
<!-- results:phase5-block4-20mbit:end -->

MariaDB 11.4 (zlib only), 100 Mbit/s each way:

<!-- results:phase5-block4-mariadb-100mbit:start -->
<picture>
  <source media="(prefers-color-scheme: dark)" srcset="results/phase5-block4-mariadb-100mbit-dark.svg">
  <img alt="Throughput relative to mysqlclient" src="results/phase5-block4-mariadb-100mbit-light.svg">
</picture>

**CPython 3.10.19** · 11.4.13-MariaDB-ubu2404 · Darwin arm64 · rustmy 0.0.0, mysqlclient 2.3.1 · best of 5 · 2026-11-18T10:22:55+00:00

| Configuration | fetchall 100k mixed rows | 1,000 SELECT by primary key | executemany INSERT 10k rows | vs mysqlclient (fetchall 100k mixed rows) | vs mysqlclient (1,000 SELECT by primary key) | vs mysqlclient (executemany INSERT 10k rows) |
|---|--:|--:|--:|--:|--:|--:|
| mysqlclient | 908.7 ms | **3108.5 ms** | 300.8 ms | 1.00x | 1.00x | 1.00x |
| mysqlclient zlib | 305.2 ms | 3327.8 ms | 229.6 ms | 2.98x | 0.93x | 1.31x |
| rustmy (default) | 782.6 ms | 3207.2 ms | 193.9 ms | 1.16x | 0.97x | 1.55x |
| rustmy zlib | **195.5 ms** | 3222.2 ms | **96.0 ms** | 4.65x | 0.96x | 3.13x |

Times per configuration; the fastest is in bold.
<!-- results:phase5-block4-mariadb-100mbit:end -->

What the numbers say: compression is for slow or metered links. Locally it
only costs time, mostly on the server, which compresses every result (zlib
level 6 more than triples the time to read 100,000 rows; zstd costs much
less). At 100 Mbit/s the rows travel about four times smaller and reading
them is 4.5x (zstd) and 4x (zlib) faster than uncompressed; with the same
algorithm rustmy is about 1.6x faster than mysqlclient, which spends its
own time decompressing and converting. Short lookups gain nothing until the
link is slow enough for their few bytes to matter. Through toxiproxy's
bandwidth toxic every MariaDB reply to a lookup pays about 3 ms for every
driver alike (MariaDB writes its small replies in more pieces), so that
panel says nothing about compression.

## Phase 5, block 4: LOAD DATA LOCAL INFILE (2026-11-18, Python 3.10)

`local_infile.py` loads a CSV file of a million rows (about 45 MB) into a
`BLACKHOLE` table, where the server parses the rows and drops them, so what
is left is the client sending the file and the protocol; into an InnoDB
table; and a file of 1,000 rows 100 times. rustmy against mysqlclient
(`local_infile_dir`) and PyMySQL (`local_infile=True`); at 100 Mbit/s, through
toxiproxy as in `compress.py`, also with compression:

```sh
docker compose --profile bench up -d --wait mysql toxiproxy
uv run --no-sync python benches/local_infile.py \
    --json benches/results/phase5-block4-infile-local-py310.json
uv run --no-sync python benches/local_infile.py --bandwidth 12500 \
    --json benches/results/phase5-block4-infile-100mbit-py310.json
for run in local 100mbit; do
  uv run --no-sync python benches/report.py --section phase5-block4-infile-$run \
      benches/results/phase5-block4-infile-$run-py310.json
done
```

MySQL 8.4, local (no cap):

<!-- results:phase5-block4-infile-local:start -->
<picture>
  <source media="(prefers-color-scheme: dark)" srcset="results/phase5-block4-infile-local-dark.svg">
  <img alt="Throughput relative to mysqlclient" src="results/phase5-block4-infile-local-light.svg">
</picture>

**CPython 3.10.19** · 8.4.13 · Darwin arm64 · rustmy 0.0.0, mysqlclient 2.3.1, PyMySQL 2.2.8 · best of 5 · 2026-11-18T16:17:21+00:00

| Configuration | 1M rows into BLACKHOLE | 1M rows into InnoDB | 100 loads of 1,000 rows | vs mysqlclient (1M rows into BLACKHOLE) | vs mysqlclient (1M rows into InnoDB) | vs mysqlclient (100 loads of 1,000 rows) |
|---|--:|--:|--:|--:|--:|--:|
| mysqlclient | 610.9 ms | 1300.6 ms | 81.7 ms | 1.00x | 1.00x | 1.00x |
| PyMySQL | 621.4 ms | **1299.5 ms** | 86.1 ms | 0.98x | 1.00x | 0.95x |
| rustmy (default) | **591.6 ms** | 1306.1 ms | **77.5 ms** | 1.03x | 1.00x | 1.05x |

Times per configuration; the fastest is in bold.
<!-- results:phase5-block4-infile-local:end -->

MySQL 8.4, 100 Mbit/s each way:

<!-- results:phase5-block4-infile-100mbit:start -->
<picture>
  <source media="(prefers-color-scheme: dark)" srcset="results/phase5-block4-infile-100mbit-dark.svg">
  <img alt="Throughput relative to mysqlclient" src="results/phase5-block4-infile-100mbit-light.svg">
</picture>

**CPython 3.10.19** · 8.4.13 · Darwin arm64 · rustmy 0.0.0, mysqlclient 2.3.1, PyMySQL 2.2.8 · best of 5 · 2026-11-18T16:25:26+00:00

| Configuration | 1M rows into BLACKHOLE | 1M rows into InnoDB | 100 loads of 1,000 rows | vs mysqlclient (1M rows into BLACKHOLE) | vs mysqlclient (1M rows into InnoDB) | vs mysqlclient (100 loads of 1,000 rows) |
|---|--:|--:|--:|--:|--:|--:|
| mysqlclient | 4905.9 ms | 4851.6 ms | 578.1 ms | 1.00x | 1.00x | 1.00x |
| PyMySQL | 4924.0 ms | 4884.6 ms | 722.0 ms | 1.00x | 0.99x | 0.80x |
| rustmy (default) | 4880.1 ms | 4860.7 ms | 686.1 ms | 1.01x | 1.00x | 0.84x |
| mysqlclient zlib | 1186.0 ms | 1466.5 ms | 296.9 ms | 4.14x | 3.31x | 1.95x |
| rustmy zlib | 1221.2 ms | 1389.0 ms | 271.6 ms | 4.02x | 3.49x | 2.13x |
| rustmy zstd | **931.8 ms** | **1334.8 ms** | **258.1 ms** | 5.27x | 3.63x | 2.24x |

Times per configuration; the fastest is in bold.
<!-- results:phase5-block4-infile-100mbit:end -->

The file is the client's only work here, and it is little: locally the
server sets the pace (parsing rows, writing InnoDB pages) and the three
drivers are level, rustmy 3–5% ahead where the server does least
(`BLACKHOLE`, small files). At 100 Mbit/s the link sets it: 45 MB take
about 4.9 s with any driver, and compression is what matters, zstd loading
the large file 5.3x faster than an uncompressed mysqlclient and 1.27x faster
than its zlib (rustmy's zlib is 3% behind mysqlclient's on that file and
ahead on the others).

The 0.84x of the uncompressed small loads at 100 Mbit/s is not rustmy
getting slower: it is mysqlclient getting faster after the large loads that
run before it on the same connection. Measured on fresh connections
(best of five rounds of 100 loads, in four runs), mysqlclient took
673–700 ms, rustmy 681–710 ms and PyMySQL 665–693 ms; after one 1M-row load
on each connection first, mysqlclient's best dropped to 592 ms and rustmy's
stayed at 693 ms. libmysqlclient keeps buffers the large file grew, which
apparently suits toxiproxy's bandwidth model with these small files; the
sweep below shows rustmy's packet size does not change the time on an
uncompressed capped link.

Packet size (`LOCAL_INFILE_CHUNK`), 1M rows into `BLACKHOLE`, best of
three rounds, ms:

| Link | 4 KiB | 8 KiB | 16 KiB | 32 KiB | 64 KiB |
|---|--:|--:|--:|--:|--:|
| local | 586 | 576 | 569 | 565 | **564** |
| local, TLS | 606 | 596 | 593 | 589 | **585** |
| 100 Mbit/s | 4864 | **4858** | 4863 | 4880 | 4879 |
| 100 Mbit/s, zlib | **1210** | 1223 | 1219 | 1232 | 1234 |
| 100 Mbit/s, zstd | 979 | 973 | **931** | 944 | 977 |

16 KiB: within 1% of the best locally and 5% faster than 64 KiB with zstd,
the configuration meant for slow links.
