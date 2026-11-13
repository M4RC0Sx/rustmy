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
uv run --no-sync python benches/spike.py --repeat 5
```

Fairness rules the script enforces: all drivers use `autocommit=True`, plain
TCP (mysqlclient would otherwise negotiate TLS, which rustmy does not support
yet) and the same queries against the same tables.

## Phase 0 results (2026-11-13)

Go/no-go criteria from the plan: not slower than mysqlclient on int/varchar,
and at least 1.3x faster on the mixed workload.

Setup: Apple Silicon (macOS), MySQL 8.4.13 in Docker on the same machine,
mysqlclient 2.3.1, PyMySQL 2.2.8, rustmy built per Python version (not abi3),
best of 5. Speedup = mysqlclient time / rustmy time.

| Scenario | CPython 3.10 | CPython 3.14 |
|---|---|---|
| fetchall 100k rows int/varchar | 1.01x | 1.05x |
| fetchall 100k rows mixed (double/datetime/date/decimal) | **2.30x** | **2.02x** |
| fetchall 100k rows datetime + date | **5.3x** | **3.7x** |
| fetchall 100k rows decimal | 1.00x | 0.98x |
| 10k x `SELECT 1` | 1.04x | 1.06x |
| 10k x INSERT with parameters | 1.01x | 1.02x |
| executemany 10k rows | 0.03x | 0.02x |

PyMySQL is 3–7x slower than both on every fetch scenario.

**Decision: both criteria are met; the project continues.**

Reading the numbers:

- Temporal types are where rustmy wins: mysqlclient calls a Python function
  per `date`/`datetime` cell, rustmy builds them directly through the
  `datetime` C API.
- int/varchar and `DECIMAL` are at parity, as predicted: object creation
  dominates in both drivers (`Decimal` is built by calling the Python class
  in both). There is room to optimise the row loop itself.
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
