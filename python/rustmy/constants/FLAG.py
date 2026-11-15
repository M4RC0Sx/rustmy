"""Column definition flags (``conv=`` entries can pick a converter by them)."""

NOT_NULL = 1
PRI_KEY = 1 << 1
UNIQUE_KEY = 1 << 2
MULTIPLE_KEY = 1 << 3
BLOB = 1 << 4
UNSIGNED = 1 << 5
ZEROFILL = 1 << 6
#: Set for binary strings (in ``conv=`` flag lists, decided by the column's
#: binary collation, as in MySQLdb).
BINARY = 1 << 7
ENUM = 1 << 8
AUTO_INCREMENT = 1 << 9
TIMESTAMP = 1 << 10
SET = 1 << 11
NO_DEFAULT_VALUE = 1 << 12
ON_UPDATE_NOW = 1 << 13
PART_KEY = 1 << 14
NUM = 1 << 15
GROUP = NUM
UNIQUE = 1 << 16
