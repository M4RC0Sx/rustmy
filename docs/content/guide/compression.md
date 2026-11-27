---
description: "The MySQL compressed protocol with rustmy, zlib and zstd, and when it pays off."
---

# Compression

Over a slow or metered link, the compressed protocol trades CPU for bytes:

```python
conn = rustmy.connect(host="db.example.com", user="app", compress=True)  # zlib
conn = rustmy.connect(host="db.example.com", user="app", compress="zstd")
print(conn.compression)  # what the server agreed: "zlib", "zstd" or None
```

- `compress=True` is zlib, as in mysqlclient.
- zstd (MySQL 8.0.18+) takes a `zstd_level` from 1 to 22 (default 3).
- MariaDB has no zstd, so `compress="zstd"` falls back to zlib there; a
  server that offers no compression at all gives an uncompressed connection.
- It works with TLS, prepared statements and pipelined `executemany`.

## When it pays off

Over a 100 Mbit/s link, reading 100,000 rows is about **4.5x faster with
zstd** than uncompressed, and 1.6x faster than mysqlclient's `compress=True`
with the same zlib. On a fast local network it costs more CPU, mostly the
server's, than it saves in transfer time, so leave it off there. The
measurements are in the
[benchmark notes](https://github.com/M4RC0Sx/rustmy/blob/master/benches/README.md#phase-5-block-4-compression-2026-11-18-python-310).

## Elsewhere

SQLAlchemy takes it in the URL (`?compress=zstd&zstd_level=3`), and option
files through `compress`, `compression-algorithms` and
`zstd-compression-level`.
