---
description: "Using rustmy with FastAPI, from sync endpoints through SQLAlchemy and from async endpoints through a rustmy.aio pool."
---

# FastAPI

=== "async def endpoints"

    Open a `rustmy.aio` pool in the lifespan and have each request borrow a
    connection from it:

    ```python
    from collections.abc import AsyncIterator
    from contextlib import asynccontextmanager

    from fastapi import FastAPI, HTTPException
    from rustmy import aio

    pool: aio.Pool


    @asynccontextmanager
    async def lifespan(app: FastAPI) -> AsyncIterator[None]:
        global pool
        async with aio.create_pool(
            minsize=2,
            maxsize=20,
            host="db.internal",
            user="app",
            password="secret",
            database="shop",
            ssl_mode="verify_identity",
            connect_timeout=5,
            read_timeout=30,
        ) as pool:
            yield


    app = FastAPI(lifespan=lifespan)


    @app.get("/products/{product_id}")
    async def product(product_id: int) -> dict[str, object]:
        async with pool.acquire(timeout=5) as conn, conn.cursor(aio.DictCursor) as cursor:
            await cursor.execute(
                "SELECT id, name, price FROM products WHERE id = %s", (product_id,)
            )
            row = await cursor.fetchone()
        if row is None:
            raise HTTPException(status_code=404)
        return row
    ```

=== "def endpoints"

    Plain `def` endpoints run in FastAPI's thread pool, and each borrows a
    pooled connection, here from SQLAlchemy's pool:

    ```python
    from fastapi import FastAPI
    from sqlalchemy import create_engine, text

    engine = create_engine(
        "mysql+rustmy://app:secret@db.internal/shop"
        "?ssl_mode=verify_identity&connect_timeout=5&read_timeout=30",
        pool_size=10,
        pool_pre_ping=True,
        pool_recycle=3600,
    )
    app = FastAPI()


    @app.get("/products/{product_id}")
    def product(product_id: int) -> dict[str, object]:
        with engine.connect() as conn:
            row = conn.execute(
                text("SELECT id, name, price FROM products WHERE id = :id"),
                {"id": product_id},
            ).one()
        return row._asdict()
    ```

The pool's `acquire(timeout=5)` raises `rustmy.PoolTimeout` when no
connection comes free in time, which an exception handler can turn into a 503.
