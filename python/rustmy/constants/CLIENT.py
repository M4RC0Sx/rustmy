"""Capability flags (``client_flag=``), as defined by the client/server
protocol. ``FOUND_ROWS`` makes ``rowcount`` count matched rather than changed
rows; ``MULTI_STATEMENTS`` is what ``multi_statements=True`` sets.
"""

LONG_PASSWORD = 1
FOUND_ROWS = 1 << 1
LONG_FLAG = 1 << 2
CONNECT_WITH_DB = 1 << 3
NO_SCHEMA = 1 << 4
COMPRESS = 1 << 5
ODBC = 1 << 6
LOCAL_FILES = 1 << 7
IGNORE_SPACE = 1 << 8
PROTOCOL_41 = 1 << 9
#: Old name of the same bit as ``PROTOCOL_41``.
CHANGE_USER = PROTOCOL_41
INTERACTIVE = 1 << 10
SSL = 1 << 11
IGNORE_SIGPIPE = 1 << 12
TRANSACTIONS = 1 << 13
RESERVED = 1 << 14
SECURE_CONNECTION = 1 << 15
MULTI_STATEMENTS = 1 << 16
MULTI_RESULTS = 1 << 17
PS_MULTI_RESULTS = 1 << 18
PLUGIN_AUTH = 1 << 19
CONNECT_ATTRS = 1 << 20
PLUGIN_AUTH_LENENC_CLIENT_DATA = 1 << 21
HANDLE_EXPIRED_PASSWORDS = 1 << 22
SESSION_TRACK = 1 << 23
DEPRECATE_EOF = 1 << 24
