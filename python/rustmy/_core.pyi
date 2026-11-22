import asyncio

from collections.abc import Iterable
from os import PathLike
from typing import Any
from typing import Final
from typing import TypeAlias

__version__: Final[str]
MAX_PACKET_PAYLOAD: Final[int]

# (name, type_code, display_size, internal_size, precision, scale, null_ok)
ColumnDescription: TypeAlias = tuple[str, int, None, int, int, int, bool]
Description: TypeAlias = tuple[ColumnDescription, ...]
# (description, rows, rowcount, last_insert_id, warnings); description and
# rows are None for statements without a result set.
# Rows are tuples, or dicts with as_dict=True.
QueryResult: TypeAlias = tuple[
    Description | None, tuple[Any, ...] | None, int, int, int
]
# (description, stream, rowcount, last_insert_id, warnings); description and
# stream are None for statements without a result set, and rowcount is -1
# when there is one.
StreamedResult: TypeAlias = tuple[
    Description | None, ResultStream | None, int, int, int
]

class Connection:
    def query(self, sql: str, /, batch_bytes: int = 4096) -> QueryResult: ...
    def execute(
        self,
        query: str,
        args: object = None,
        as_dict: bool = False,
        batch_bytes: int = 4096,
    ) -> QueryResult: ...
    def execute_streamed(
        self,
        query: str,
        args: object = None,
        as_dict: bool = False,
        batch_bytes: int = 4096,
    ) -> StreamedResult: ...
    def execute_prepared(
        self,
        query: str,
        args: object = None,
        as_dict: bool = False,
        batch_bytes: int = 4096,
    ) -> QueryResult: ...
    def execute_prepared_streamed(
        self,
        query: str,
        args: object = None,
        as_dict: bool = False,
        batch_bytes: int = 4096,
    ) -> StreamedResult: ...
    def mogrify(self, query: str, args: object = None) -> str: ...
    def literal(self, value: object, /) -> str: ...
    def decode_statement(self, statement: bytes, /) -> str: ...
    def set_character_set(self, charset: str, collation: str | None = None) -> None: ...
    def set_converters(
        self,
        decoders: dict[int, Any] | None,
        encoders: dict[type, Any] | None,
        conv: object,
    ) -> None: ...
    @property
    def charset(self) -> str: ...
    @property
    def server_charset(self) -> str | None: ...
    @property
    def encoding(self) -> str: ...
    def executemany(
        self, query: str, seq_of_args: Iterable[object], max_stmt_length: int
    ) -> tuple[int, int | None, bytes | None] | None: ...
    def executemany_pipelined(
        self,
        query: str,
        seq_of_args: Iterable[object],
        max_pipeline: int,
        as_dict: bool = False,
        batch_bytes: int = 4096,
    ) -> tuple[QueryResult, int, bytes] | None: ...
    def executemany_pipelined_streamed(
        self,
        query: str,
        seq_of_args: Iterable[object],
        max_pipeline: int,
        as_dict: bool = False,
        batch_bytes: int = 4096,
    ) -> tuple[StreamedResult, int, bytes] | None: ...
    def executemany_prepared_insert(
        self, query: str, seq_of_args: Iterable[object], max_stmt_length: int
    ) -> tuple[int, int | None, bytes | None] | None: ...
    def executemany_prepared(
        self,
        query: str,
        seq_of_args: Iterable[object],
        max_pipeline: int,
        as_dict: bool = False,
        batch_bytes: int = 4096,
    ) -> tuple[QueryResult, int, bytes] | None: ...
    def next_result(
        self, as_dict: bool = False, batch_bytes: int = 4096
    ) -> QueryResult | None: ...
    def next_result_streamed(
        self, as_dict: bool = False, batch_bytes: int = 4096
    ) -> StreamedResult | None: ...
    def ping(self) -> None: ...
    def select_db(self, database: str, /) -> None: ...
    def reset_connection(self) -> None: ...
    def change_user(
        self, user: str, password: str, database: str | None = None
    ) -> None: ...
    def close(self) -> None: ...
    @property
    def tls(self) -> bool: ...
    @property
    def compression(self) -> str | None: ...
    @property
    def closed(self) -> bool: ...
    @property
    def server_version(self) -> str: ...
    @property
    def connection_id(self) -> int: ...
    @property
    def server_status(self) -> int: ...
    @property
    def has_more_results(self) -> bool: ...

class AioConnection:
    """The core of the asyncio connection (first cut, ``rustmy._aio``)."""

    def __init__(
        self,
        event_loop: asyncio.AbstractEventLoop,
        target: str,
        user: str,
        password: str,
        database: str | None = None,
        unix: bool = False,
    ) -> None: ...
    def connected(self) -> asyncio.Future[None] | None: ...
    def connection_made(self, transport: asyncio.BaseTransport, /) -> None: ...
    def get_buffer(self, sizehint: int, /) -> bytearray: ...
    def buffer_updated(self, nbytes: int, /) -> None: ...
    def eof_received(self) -> bool: ...
    def connection_lost(self, exc: Exception | None, /) -> None: ...
    def execute(
        self,
        query: str,
        args: object = None,
        as_dict: bool = False,
    ) -> asyncio.Future[QueryResult]: ...
    def close(self) -> None: ...

class ResultStream:
    def fetchone(self) -> Any: ...
    def fetchmany(self, size: int) -> tuple[Any, ...]: ...
    def fetchall(self) -> tuple[Any, ...]: ...
    def skip(self, count: int) -> int: ...
    def close(self) -> None: ...
    @property
    def rownumber(self) -> int: ...
    def __iter__(self) -> ResultStream: ...
    def __next__(self) -> Any: ...

def connect(
    *,
    host: str,
    port: int,
    user: str,
    password: str,
    database: str | None = None,
    unix_socket: str | PathLike[str] | None = None,
    connect_timeout: float | None = None,
    read_timeout: float | None = None,
    write_timeout: float | None = None,
    client_flag: int = 0,
    charset: str = "utf8mb4",
    ssl_mode: str = "preferred",
    ssl_ca: str | PathLike[str] | None = None,
    ssl_cert: str | PathLike[str] | None = None,
    ssl_key: str | PathLike[str] | None = None,
    server_public_key: str | PathLike[str] | None = None,
    get_server_public_key: bool = True,
    binary_prefix: bool = True,
    statement_cache_size: int = 32,
    compress: str | None = None,
    zstd_level: int = 3,
    local_infile: bool = False,
    local_infile_dir: str | PathLike[str] | None = None,
) -> Connection: ...
def literal(value: object, no_backslash_escapes: bool = False) -> str: ...
def names_clause(charset: str, collation: str | None = None) -> str: ...
def escape_string(value: str, no_backslash_escapes: bool = False) -> str: ...
def interpolate(
    query: str,
    args: object,
    no_backslash_escapes: bool = False,
) -> str: ...

class Warning(Exception): ...  # noqa: A001, N818
class Error(Exception): ...
class InterfaceError(Error): ...
class DatabaseError(Error): ...
class DataError(DatabaseError): ...
class OperationalError(DatabaseError): ...
class IntegrityError(DatabaseError): ...
class InternalError(DatabaseError): ...
class ProgrammingError(DatabaseError): ...
class NotSupportedError(DatabaseError): ...
class OutOfRangeError(DataError, OperationalError): ...
