---
description: "Reading and writing pandas DataFrames with rustmy through SQLAlchemy."
---

# pandas

pandas reads and writes through a SQLAlchemy engine:

```python
import pandas as pd
from sqlalchemy import create_engine

engine = create_engine("mysql+rustmy://app:secret@localhost/shop")

orders = pd.read_sql("SELECT * FROM orders WHERE created_at >= '2026-01-01'", engine)
orders.to_sql("orders_copy", engine, if_exists="replace", index=False)
```

For results too large to hold at once, `chunksize` with
`execution_options(stream_results=True)` reads them through the streaming
cursor:

```python
with engine.connect().execution_options(stream_results=True) as conn:
    for chunk in pd.read_sql("SELECT * FROM events", conn, chunksize=100_000):
        handle(chunk)
```
