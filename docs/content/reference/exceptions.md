---
description: "The exceptions rustmy raises, PEP 249's hierarchy plus OutOfRangeError and PoolTimeout."
---

# Exceptions

Server errors are classed by their SQLSTATE, as
[Errors](../guide/queries.md#errors) explains; errors of the driver itself
carry the code 0.

```text
Exception
├── Warning
└── Error
    ├── InterfaceError
    └── DatabaseError
        ├── DataError
        │   └── OutOfRangeError  (also an OperationalError)
        ├── OperationalError
        │   └── PoolTimeout
        ├── IntegrityError
        ├── InternalError
        ├── ProgrammingError
        └── NotSupportedError
```

They are importable from `rustmy`, `rustmy.err` and `rustmy.aio`, and are
attributes of every connection (`conn.ProgrammingError`), as PEP 249
suggests.

::: rustmy.err
    options:
      show_root_heading: false
      show_if_no_docstring: true
      members:
        - Error
        - Warning
        - InterfaceError
        - DatabaseError
        - DataError
        - OperationalError
        - IntegrityError
        - InternalError
        - ProgrammingError
        - NotSupportedError
        - OutOfRangeError
        - PoolTimeout
