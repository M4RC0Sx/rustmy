---
description: "LOAD DATA LOCAL INFILE with rustmy, limited to one directory and to the file the statement names."
---

# Loading local files

`LOAD DATA LOCAL INFILE` sends a file from the client's disk. It is off by
default, because the server names the file the client sends: a malicious or
compromised server could ask for any file, after any statement. Allow a
directory to turn it on:

```python
conn = rustmy.connect(..., local_infile_dir="/srv/imports")
cursor = conn.cursor()
cursor.execute(
    "LOAD DATA LOCAL INFILE %s INTO TABLE orders FIELDS TERMINATED BY ','",
    ("/srv/imports/orders.csv",),
)
print(cursor.rowcount)  # rows loaded
```

## What is sent

A request is answered only when:

1. the statement being run is a `LOAD DATA LOCAL INFILE` (or `LOAD XML LOCAL
   INFILE`) that names that very file, and
2. the file, with symbolic links and `..` resolved, is a regular file inside
   the directory.

Anything else (another file, a request after a `SELECT`, a link pointing out
of the directory, a directory or a FIFO) gets an empty file and raises
`OperationalError` 2068, and nothing is read unless the statement named that
file; the connection stays usable. Relative names start at the working
directory, as in other clients.

The file is streamed, never read into memory whole, with the GIL released,
and compressed like everything else on a compressed connection. `executemany`
runs these statements one at a time. On asyncio, the file is read in the
loop's executor, so the loop never waits on the disk.

!!! note "The server must allow it too"

    MySQL 8.0+ turns `local_infile` off by default (MariaDB leaves it on).

## Speed

Locally the server sets the pace and rustmy loads as fast as mysqlclient. On
a 100 Mbit/s link a 45 MB file takes 4.9 s uncompressed with any driver and
0.93 s with `compress="zstd"`
([numbers](https://github.com/M4RC0Sx/rustmy/blob/master/benches/README.md#phase-5-block-4-load-data-local-infile-2026-11-18-python-310)).

## Elsewhere

SQLAlchemy takes `?local_infile_dir=…` in the URL, and option files
`load-data-local-dir`. `local_infile=True` alone, which lets mysqlclient and
PyMySQL send any file the server asks for, raises `ValueError`.
