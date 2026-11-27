---
description: "The rustmy user guide, from connecting to running in production."
---

# Guide

How to use rustmy, topic by topic. Every page shows the blocking API; where
asyncio differs, [asyncio](asyncio.md) says how.

<div class="grid cards" markdown>

-   :lucide-plug:{ .lg .middle } **[Connecting](connecting.md)**

    ---

    `connect()`'s arguments, timeouts, option files, Unix sockets and
    connection attributes.

-   :lucide-terminal:{ .lg .middle } **[Queries and parameters](queries.md)**

    ---

    Placeholders, type mapping, `executemany`, transactions and errors.

-   :lucide-list:{ .lg .middle } **[Cursors](cursors.md)**

    ---

    Tuples or dicts, buffered or streamed, and the PEP 249 extras.

-   :lucide-binary:{ .lg .middle } **[Prepared statements](prepared.md)**

    ---

    The binary protocol with `PreparedCursor`.

-   :lucide-lock:{ .lg .middle } **[TLS and authentication](tls.md)**

    ---

    Verifying the server, client certificates, every authentication method,
    PAM and cloud IAM tokens.

-   :lucide-file-archive:{ .lg .middle } **[Compression](compression.md)**

    ---

    zlib and zstd over slow or metered links.

-   :lucide-file-up:{ .lg .middle } **[Loading local files](local-files.md)**

    ---

    `LOAD DATA LOCAL INFILE`, safely.

-   :lucide-languages:{ .lg .middle } **[Character sets and conversions](charsets.md)**

    ---

    Legacy character sets and `conv=`.

-   :lucide-refresh-cw:{ .lg .middle } **[asyncio](asyncio.md)**

    ---

    `rustmy.aio`: the same driver on the event loop.

-   :lucide-layers:{ .lg .middle } **[Connection pools](pools.md)**

    ---

    `rustmy.Pool` for threads, `rustmy.aio.create_pool` for tasks.

-   :lucide-server:{ .lg .middle } **[In production](production.md)**

    ---

    TLS, timeouts, pooling and naming the application.

-   :lucide-shield:{ .lg .middle } **[Security notes](security.md)**

    ---

    Escaping, building SQL by hand and what an attacker on the network can
    and cannot do.

</div>
