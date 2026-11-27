---
description: "How rustmy compares with mysqlclient, PyMySQL and mysql-connector-python."
---

# How it compares

| | rustmy | mysqlclient | PyMySQL | mysql-connector-python |
|---|---|---|---|---|
| Written in | Rust | C (wraps libmysqlclient) | Python | Python, optional C extension |
| License | MIT or Apache-2.0 | GPL-2.0 | MIT | GPL-2.0 (with a FOSS exception) |
| Linux and macOS install | wheel, nothing else | builds from source: libmysqlclient, its headers, `pkg-config`, a C compiler | pure Python | wheel |
| Server-side prepared statements | yes (`PreparedCursor`) | no | no | yes |
| `executemany` of `UPDATE`/`DELETE` | pipelined: up to 256 statements per round trip | batched only with `multi_statements` (2.3+) | one round trip each | one round trip each |
| `LOAD DATA LOCAL INFILE` | only the file the statement names, inside one allowed directory | any file the server asks for, or one directory (`local_infile_dir`) | any file the server asks for | any file the server asks for, or one directory (`allow_local_infile_in_path`) |
| asyncio API | yes (`rustmy.aio`), the same Rust core | no | no | yes (`mysql.connector.aio`, pure Python) |
| Connection pool | blocking and asyncio, serving waiters in order | no | no | yes, failing instead of waiting when every connection is in use |
| SQLAlchemy dialect | `mysql+rustmy`, in this package | in SQLAlchemy | in SQLAlchemy | in SQLAlchemy |
| Django | through `install_as_MySQLdb()` | Django's own backend | through `install_as_MySQLdb()` | its own backend |

## What it is good at

- **Fast where it counts.** Reading a million rows takes **0.40 s**, against
  1.32 s with mysqlclient, 1.63 s with mysql-connector-python and 3.19 s with
  PyMySQL: **3.3x faster than mysqlclient** and 8x faster than PyMySQL.
  Inserting them with `executemany` is **3.3x faster** than mysqlclient, and
  Django's `values_list()` 2.7x. `executemany` of `UPDATE`/`DELETE`
  pipelines its statements: one network round trip per 256 of them instead
  of one each.
- **Faster ORMs too.** SQLAlchemy and Django build an object per row, which
  takes the same time with any driver; even so, loading a million objects is
  1.3–1.5x faster than with mysqlclient and 2.1–2.7x faster than with PyMySQL.
- **asyncio too.** Against asyncmy, the fastest of the others, reading a
  million rows is 1.1x faster (1.3x as dicts) and inserting them 1.5x;
  against aiomysql and `mysql.connector.aio`, 11–14x faster.
- **Less memory.** A million rows held at once take 28% less memory than with
  mysqlclient; streaming cursors stay flat whatever the size.
- **Built for free-threaded Python.** Wheels for 3.14t and 3.15t; all network
  I/O runs without the GIL.
- **Correct with every character set.** The driver follows the server's
  definition of each character set, not Python's, and refuses text the server
  would store as something else, including characters that Python's codecs
  turn into a backslash byte (an injection path in `sjis`/`ujis`).
- **Familiar.** PEP 249 with `%s` / `%(name)s` placeholders, `Cursor`,
  `DictCursor`, `SSCursor`, `conv=`, `rustmy.constants`: code written for
  MySQLdb or PyMySQL mostly just works.

## Where it gains little

Latency-bound workloads, many tiny queries one after another, gain little:
the network dominates them with any driver. `executemany` is the exception,
since it pipelines its statements. The [performance page](../performance.md)
has every figure.
