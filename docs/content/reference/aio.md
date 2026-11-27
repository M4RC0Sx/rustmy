---
description: "rustmy.aio, the asyncio API: connect(), create_pool(), the connection, cursors and pool."
---

# rustmy.aio

::: rustmy.aio
    options:
      show_root_heading: false
      members: false

## Connecting

::: rustmy.aio.connect
    options:
      heading_level: 3

::: rustmy.aio.Connection
    options:
      heading_level: 3
      # Built by connect(), never directly; the exceptions it carries are in
      # rustmy.err.
      filters: ["!^_", "!Error$", "!^Warning$"]
      merge_init_into_class: false

## Cursors

::: rustmy.aio.cursors
    options:
      show_root_heading: false
      heading_level: 3
      members:
        - Cursor
        - DictCursor
        - SSCursor
        - SSDictCursor
        - PreparedCursor
        - PreparedDictCursor

## Pools

::: rustmy.aio.create_pool
    options:
      heading_level: 3

::: rustmy.aio.Pool
    options:
      heading_level: 3
      # Built by create_pool(), never directly.
      filters: ["!^_"]
      merge_init_into_class: false
