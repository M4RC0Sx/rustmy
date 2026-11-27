---
description: "The rustmy module: PEP 249 globals, type objects and constructors, and install_as_MySQLdb()."
---

# The rustmy module

## PEP 249 globals

| Name | Value | |
|---|---|---|
| `rustmy.apilevel` | `"2.0"` | the DB-API version |
| `rustmy.threadsafety` | `1` | threads may share the module, not connections |
| `rustmy.paramstyle` | `"format"` | `%s` and `%(name)s` placeholders |
| `rustmy.__version__` | e.g. `"1.5.0"` | rustmy's version |

`rustmy.connect` is [`Connection`](connection.md).

::: rustmy.install_as_MySQLdb
    options:
      heading_level: 2

## Type objects and constructors

::: rustmy.dbapi
    options:
      show_root_heading: false
      heading_level: 3
      show_if_no_docstring: true
      members:
        - STRING
        - BINARY
        - NUMBER
        - DATETIME
        - ROWID
        - Date
        - Time
        - Timestamp
        - DateFromTicks
        - TimeFromTicks
        - TimestampFromTicks
        - Binary

## Constants

`rustmy.constants` holds the MySQL constants under the module names other
drivers use, so code written for them finds them:

| Module | Holds |
|---|---|
| `rustmy.constants.ER` | server error codes (`ER.DUP_ENTRY`…), generated from every supported server's catalogue |
| `rustmy.constants.CR` | client error codes (`CR.SERVER_LOST`…) |
| `rustmy.constants.FIELD_TYPE` | column types, as in `cursor.description` |
| `rustmy.constants.FLAG` | column flags |
| `rustmy.constants.CLIENT` | capability flags |
| `rustmy.constants.SERVER_STATUS` | server status flags |
