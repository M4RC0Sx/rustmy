"""rustmy's SQLAlchemy dialects, beyond what SQLAlchemy's own suite checks."""

from __future__ import annotations

import datetime as dt
import uuid

from decimal import Decimal
from typing import TYPE_CHECKING
from typing import Any

import pytest
import sqlalchemy as sa

from sqlalchemy import orm
from sqlalchemy.dialects import mysql
from sqlalchemy.dialects import registry

import rustmy

from rustmy.constants import CLIENT
from rustmy.cursors import SSCursor
from rustmy.dialect import RustmyMariaDBDialect
from rustmy.dialect import RustmyMySQLDialect


if TYPE_CHECKING:
    from collections.abc import Iterator

    from tests.conftest import Server


def _url(server: Server, query: str = "") -> str:
    scheme = "mariadb" if server.name == "mariadb" else "mysql"
    url = f"{scheme}+rustmy://rustmy:rustmy@{server.host}:{server.port}/rustmy_test"
    return f"{url}?{query}" if query else url


@pytest.fixture
def engine(server: Server) -> Iterator[sa.Engine]:
    engine = sa.create_engine(_url(server))
    yield engine
    engine.dispose()


# Registration and URL arguments (no server needed) ---------------------------


def test_entry_points_register_both_dialects() -> None:
    assert registry.load("mysql.rustmy") is RustmyMySQLDialect
    assert registry.load("mariadb.rustmy") is RustmyMariaDBDialect
    engine = sa.create_engine("mysql+rustmy://")
    assert engine.dialect.loaded_dbapi.__name__ == "rustmy"
    assert engine.dialect.driver == "rustmy"


def test_url_cursorclass_names_a_rustmy_cursor() -> None:
    dialect = sa.create_engine("mysql+rustmy://").dialect
    url = sa.make_url("mysql+rustmy://u@h/db?cursorclass=PreparedCursor")
    _, kwargs = dialect.create_connect_args(url)
    assert kwargs["cursorclass"] is rustmy.cursors.PreparedCursor
    with pytest.raises(ValueError, match="not a cursor class"):
        dialect.create_connect_args(sa.make_url("mysql+rustmy://u@h/db?cursorclass=os"))


def test_url_compress_keeps_the_algorithm() -> None:
    dialect = sa.create_engine("mysql+rustmy://").dialect
    for query, compress in [("true", True), ("0", False), ("zstd", "zstd")]:
        url = sa.make_url(f"mysql+rustmy://u@h/db?compress={query}&zstd_level=9")
        _, kwargs = dialect.create_connect_args(url)
        assert (kwargs["compress"], kwargs["zstd_level"]) == (compress, 9)


def test_url_local_infile() -> None:
    dialect = sa.create_engine("mysql+rustmy://").dialect
    url = sa.make_url("mysql+rustmy://u@h/db?local_infile=1&local_infile_dir=/srv/in")
    _, kwargs = dialect.create_connect_args(url)
    assert (kwargs["local_infile"], kwargs["local_infile_dir"]) == (True, "/srv/in")


def test_url_arguments_use_rustmy_names() -> None:
    engine = sa.create_engine(
        "mysql+rustmy://app:s%40cret@db.example:3307/shop"
        "?charset=latin1&collation=latin1_bin&ssl_mode=verify_identity"
        "&read_timeout=2.5&connect_timeout=1&autocommit=true"
        "&multi_statements=false&ssl_ca=/etc/ca.pem&client_flag=2"
    )
    args, kwargs = engine.dialect.create_connect_args(engine.url)
    assert args == []
    assert kwargs == {
        "host": "db.example",
        "port": 3307,
        "user": "app",
        "password": "s@cret",
        "database": "shop",
        "charset": "latin1",
        "collation": "latin1_bin",
        "ssl_mode": "verify_identity",
        "read_timeout": 2.5,
        "connect_timeout": 1.0,
        "autocommit": True,
        "multi_statements": False,
        "ssl": {"ca": "/etc/ca.pem"},
        # FOUND_ROWS is always added: the ORM relies on matched rows.
        "client_flag": 2 | CLIENT.FOUND_ROWS,
    }


@pytest.mark.parametrize(("value", "expected"), [("false", False), ("1", True)])
def test_get_server_public_key_from_the_url(value: str, *, expected: bool) -> None:
    dialect = sa.create_engine("mysql+rustmy://").dialect
    url = sa.make_url(f"mysql+rustmy://u@h/db?get_server_public_key={value}")
    _, kwargs = dialect.create_connect_args(url)
    assert kwargs["get_server_public_key"] is expected


def test_program_name_from_the_url() -> None:
    dialect = sa.create_engine("mysql+rustmy://").dialect
    url = sa.make_url("mysql+rustmy://u@h/db?program_name=orm-app")
    _, kwargs = dialect.create_connect_args(url)
    assert kwargs["program_name"] == "orm-app"


# Sessions ----------------------------------------------------------------------


def test_no_set_names_on_connect(server: Server) -> None:
    # The handshake sets the character set and the server's default
    # collation; an explicit collation in the URL survives (mysqlclient's
    # dialect would reset it with SET NAMES on every new connection).
    engine = sa.create_engine(_url(server, "charset=latin1&collation=latin1_bin"))
    try:
        with engine.connect() as conn:
            row = conn.exec_driver_sql(
                "SELECT @@character_set_client, @@collation_connection"
            ).one()
            assert tuple(row) == ("latin1", "latin1_bin")
            assert engine.dialect._connection_charset == "latin1"  # type: ignore[attr-defined]  # noqa: SLF001
    finally:
        engine.dispose()
    with sa.create_engine(_url(server)).connect() as conn:
        session, database = conn.exec_driver_sql(
            "SELECT @@collation_connection, @@collation_database"
        ).one()
        assert session == database


def test_found_rows_makes_rowcount_count_matched_rows(engine: sa.Engine) -> None:
    with engine.begin() as conn:
        conn.exec_driver_sql("CREATE TEMPORARY TABLE t (id INT PRIMARY KEY, v INT)")
        conn.exec_driver_sql("INSERT INTO t VALUES (1, 10)")
        # Matched but unchanged: 1 with FOUND_ROWS, 0 without.
        assert conn.exec_driver_sql("UPDATE t SET v = 10 WHERE id = 1").rowcount == 1


def test_isolation_levels_and_autocommit(engine: sa.Engine) -> None:
    for level in ("READ COMMITTED", "SERIALIZABLE", "REPEATABLE READ"):
        with engine.connect().execution_options(isolation_level=level) as conn:
            assert conn.get_isolation_level() == level
    with engine.connect().execution_options(isolation_level="AUTOCOMMIT") as conn:
        dbapi_connection = conn.connection.dbapi_connection
        assert isinstance(dbapi_connection, rustmy.Connection)
        assert dbapi_connection.get_autocommit()


def test_stream_results_use_a_server_side_cursor(engine: sa.Engine) -> None:
    with engine.connect() as conn:
        result = conn.execution_options(stream_results=True, yield_per=100).execute(
            sa.text("SELECT seq FROM seq_1_to_1000")
            if engine.dialect.name == "mariadb"
            else sa.text(
                "WITH RECURSIVE s (seq) AS (SELECT 1 UNION ALL "
                "SELECT seq + 1 FROM s WHERE seq < 1000) SELECT seq FROM s"
            )
        )
        assert isinstance(result.cursor, SSCursor)
        assert sum(row.seq for row in result) == 500500


# Disconnects -------------------------------------------------------------------


def test_pre_ping_replaces_a_killed_connection(server: Server) -> None:
    engine = sa.create_engine(_url(server), pool_size=1, pool_pre_ping=True)
    try:
        with engine.connect() as conn:
            victim = conn.exec_driver_sql("SELECT CONNECTION_ID()").scalar()
        with server.connect() as admin:
            admin.cursor().execute(f"KILL {victim}")
        with engine.connect() as conn:
            assert conn.exec_driver_sql("SELECT CONNECTION_ID()").scalar() != victim
    finally:
        engine.dispose()


def test_a_closed_connection_counts_as_a_disconnect(engine: sa.Engine) -> None:
    with engine.connect() as conn:
        conn.connection.dbapi_connection.close()  # type: ignore[union-attr]
        with pytest.raises(sa.exc.DBAPIError) as excinfo:
            conn.exec_driver_sql("SELECT 1")
        assert excinfo.value.connection_invalidated
    with engine.connect() as conn:
        assert conn.exec_driver_sql("SELECT 1").scalar() == 1


# Types and the ORM -------------------------------------------------------------


class Base(orm.DeclarativeBase):
    pass


class Item(Base):
    __tablename__ = "dialect_items"

    id: orm.Mapped[int] = orm.mapped_column(primary_key=True)
    key: orm.Mapped[uuid.UUID] = orm.mapped_column(sa.Uuid, unique=True)
    name: orm.Mapped[str] = orm.mapped_column(sa.String(50))
    price: orm.Mapped[Decimal] = orm.mapped_column(sa.Numeric(10, 2))
    created: orm.Mapped[dt.datetime] = orm.mapped_column(mysql.DATETIME(fsp=6))
    data: orm.Mapped[dict[str, Any]] = orm.mapped_column(sa.JSON)
    # Every UPDATE checks this column and the matched rowcount: a wrong
    # rowcount raises StaleDataError.
    version: orm.Mapped[int] = orm.mapped_column(nullable=False)

    __mapper_args__ = {"version_id_col": version}  # noqa: RUF012


@pytest.fixture
def tables(engine: sa.Engine) -> Iterator[None]:
    Base.metadata.drop_all(engine)
    Base.metadata.create_all(engine)
    yield
    Base.metadata.drop_all(engine)


@pytest.mark.usefixtures("tables")
def test_orm_round_trip(engine: sa.Engine) -> None:
    created = dt.datetime(2026, 11, 16, 12, 30, 45, 123456)
    items = [
        Item(
            key=uuid.uuid4(),
            name=f"item ñ {i}",
            price=Decimal(f"{i}.50"),
            created=created,
            data={"tags": ["a", "ü"], "n": i},
        )
        for i in range(50)
    ]
    with orm.Session(engine) as session:
        session.add_all(items)
        session.commit()
        keys = {item.key for item in items}
    with orm.Session(engine) as session:
        loaded = session.scalars(sa.select(Item).order_by(Item.id)).all()
        assert {item.key for item in loaded} == keys
        first = loaded[0]
        assert (first.name, first.price, first.created) == (
            "item ñ 0",
            Decimal("0.50"),
            created,
        )
        assert first.data == {"tags": ["a", "ü"], "n": 0}
        # An unchanged value still matches the row (FOUND_ROWS): no
        # StaleDataError from the version check.
        first.name = "item ñ 0"
        first.price = Decimal("9.99")
        session.commit()
        assert first.version == 2
        assert session.get(Item, first.id) is first
        by_key = session.scalars(sa.select(Item).where(Item.key == first.key)).one()
        assert by_key is first


@pytest.mark.usefixtures("tables")
def test_insert_many_values_with_returning(engine: sa.Engine) -> None:
    table = Base.metadata.tables[Item.__tablename__]
    rows = [
        {
            "key": uuid.uuid4(),
            "name": f"r{i}",
            "price": Decimal(1),
            "created": dt.datetime(2026, 1, 1),
            "data": {},
            "version": 1,
        }
        for i in range(10)
    ]
    with engine.begin() as conn:
        if engine.dialect.insert_executemany_returning:  # MariaDB 10.5+
            ids = conn.execute(sa.insert(table).returning(table.c.id), rows).scalars()
            assert len(set(ids)) == 10
        else:
            assert conn.execute(sa.insert(table), rows).rowcount == 10
        assert (
            conn.execute(sa.select(sa.func.count()).select_from(table)).scalar() == 10
        )


def _items(count: int) -> list[Item]:
    return [
        Item(
            key=uuid.uuid4(),
            name=f"item {i}",
            price=Decimal(1),
            created=dt.datetime(2026, 1, 1),
            data={},
        )
        for i in range(count)
    ]


@pytest.mark.usefixtures("tables")
def test_orm_flush_deletes_through_executemany(engine: sa.Engine) -> None:
    """A flush runs versioned DELETEs as one executemany (pipelined) and
    checks that the matched rowcount adds up. (Versioned UPDATEs go one by
    one: SQLAlchemy needs each row's new version.)"""
    many: list[str] = []

    def record(*args: Any) -> None:
        # (conn, cursor, statement, parameters, context, executemany)
        statement, executemany = args[2], args[5]
        if executemany:
            many.append(statement.split()[0])

    sa.event.listen(engine, "before_cursor_execute", record)
    with orm.Session(engine) as session:
        items = _items(300)
        session.add_all(items)
        session.commit()
        for item in items[:200]:
            session.delete(item)
        session.commit()
        assert session.scalar(sa.select(sa.func.count()).select_from(Item)) == 100
    assert "DELETE" in many


@pytest.mark.usefixtures("tables")
def test_orm_flush_detects_a_stale_row_among_deletes(engine: sa.Engine) -> None:
    with orm.Session(engine, expire_on_commit=False) as session:
        items = _items(20)
        session.add_all(items)
        session.commit()
        # The session believes one row has another version, as if someone had
        # changed it since it was loaded. (A real concurrent change inside
        # the session's transaction fails earlier on MariaDB 11.8+, whose
        # innodb_snapshot_isolation raises error 1020 instead.)
        orm.attributes.set_committed_value(items[10], "version", 99)
        for item in items:
            session.delete(item)
        with pytest.raises(orm.exc.StaleDataError):
            session.commit()


@pytest.mark.usefixtures("tables")
def test_core_executemany_update_rowcount(engine: sa.Engine) -> None:
    table = Base.metadata.tables[Item.__tablename__]
    with orm.Session(engine) as session:
        session.add_all(_items(50))
        session.commit()
    update = (
        sa.update(table)
        .where(table.c.name == sa.bindparam("old"))
        .values(price=sa.bindparam("new_price"))
    )
    rows = [{"old": f"item {i}", "new_price": Decimal(5)} for i in range(60)]
    with engine.begin() as conn:
        assert conn.execute(update, rows).rowcount == 50
        total = conn.execute(sa.select(sa.func.sum(table.c.price))).scalar()
        assert total == Decimal(250)
