---
description: "rustmy's SQLAlchemy dialects, mysql+rustmy and mariadb+rustmy, blocking and asyncio."
---

# SQLAlchemy

Install `rustmy[sqlalchemy]` and use the `mysql+rustmy` (or `mariadb+rustmy`)
URL; nothing needs importing, since the dialects are registered as
SQLAlchemy entry points:

```python
from sqlalchemy import create_engine, select
from sqlalchemy.orm import DeclarativeBase, Mapped, Session, mapped_column


class Base(DeclarativeBase):
    pass


class Product(Base):
    __tablename__ = "products"

    id: Mapped[int] = mapped_column(primary_key=True)
    name: Mapped[str]
    price: Mapped[float]


engine = create_engine(
    "mysql+rustmy://app:secret@localhost/shop?ssl_mode=verify_identity",
    pool_pre_ping=True,
)

with Session(engine) as session:
    cheap = session.scalars(select(Product).where(Product.price < 10)).all()
```

SQLAlchemy 2.0 and 2.1 are supported. The dialects pass SQLAlchemy's dialect
compliance suite on every supported server, blocking and asyncio, and its
whole test suite (about 25,000 tests) as well as PyMySQL does (asyncmy, on
asyncio).

## URL arguments

Query arguments use rustmy's names, those of
[`connect()`](../reference/connection.md):

```text
mysql+rustmy://app:secret@db.internal/shop?charset=utf8mb4&ssl_mode=verify_identity&ssl_ca=/etc/ssl/db-ca.pem&connect_timeout=5&read_timeout=30&compress=zstd&program_name=shop-api
```

- Timeouts may be fractional.
- `cursorclass` names a class of `rustmy.cursors`:
  `?cursorclass=PreparedCursor` runs every statement as a
  [prepared statement](../guide/prepared.md).
- `stream_results=True` and `yield_per` use the streaming cursor.

## Differences from the mysqlclient dialect

The dialects build on SQLAlchemy's mysqlclient dialect, whose DB-API rustmy
follows, and change only this:

- **No `SET NAMES` on every new connection.** The handshake already sets the
  character set, with the server's default collation for it, and the
  statement would override an explicit `collation`.
- **A connection closed under SQLAlchemy counts as a disconnect**, so the
  pool replaces it.

## asyncio

Install `rustmy[sqlalchemy-asyncio]` and use the same URLs with
`create_async_engine` (or `mysql+rustmy_async://` explicitly). The URL
arguments are those of the blocking dialect, and `stream_results` uses the
unbuffered cursor:

```python
from sqlalchemy import select
from sqlalchemy.ext.asyncio import async_sessionmaker, create_async_engine

engine = create_async_engine("mysql+rustmy://app:secret@localhost/shop")
Session = async_sessionmaker(engine, expire_on_commit=False)

async with Session() as session:
    cheap = (await session.scalars(select(Product).where(Product.price < 10))).all()

async with engine.connect() as conn:
    result = await conn.stream(select(Product))
    async for product in result.scalars():
        ...
```

!!! note "Free-threaded Python"

    SQLAlchemy's asyncio extension runs through greenlet, which does not work
    on free-threaded builds yet. `rustmy.aio` itself does.

## pandas

`pandas.read_sql` and `DataFrame.to_sql` take an engine: see
[pandas](pandas.md).
