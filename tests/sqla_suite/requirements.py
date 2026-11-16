"""What MySQL and MariaDB support, for SQLAlchemy's dialect suite.

The suite's defaults assume a generic backend; this class states what the
supported servers do, by capability (never by driver name), so that every
test that runs is expected to pass. SQLAlchemy's own requirements for its
MySQL drivers were read as a reference.
"""

from __future__ import annotations

from typing import Any

from sqlalchemy.testing import exclusions
from sqlalchemy.testing.requirements import SuiteRequirements


def _not_supported(reason: str) -> exclusions.compound:
    return exclusions.closed(reason)


def _mariadb_13_or_later(config: Any) -> bool:
    dialect = config.db.dialect
    return bool(dialect.is_mariadb and dialect.server_version_info >= (13,))


class Requirements(SuiteRequirements):
    # Reflection ----------------------------------------------------------

    @property
    def reflect_table_options(self) -> exclusions.compound:
        return exclusions.open()

    @property
    def views(self) -> exclusions.compound:
        return exclusions.open()

    @property
    def foreign_keys_reflect_as_index(self) -> exclusions.compound:
        # InnoDB creates an index for every foreign key that lacks one.
        return exclusions.open()

    @property
    def unique_constraints_reflect_as_index(self) -> exclusions.compound:
        # A UNIQUE constraint is a unique index in MySQL and MariaDB...
        return exclusions.open()

    @property
    def unique_index_reflect_as_unique_constraints(self) -> exclusions.compound:
        # ...and reflects as both.
        return exclusions.open()

    @property
    def foreign_key_constraint_option_reflection_ondelete(
        self,
    ) -> exclusions.compound:
        return exclusions.open()

    @property
    def check_constraint_reflection(self) -> exclusions.compound:
        # Every supported server enforces and reports CHECK constraints
        # (MySQL 8.0.16+, MariaDB 10.2+).
        return exclusions.open()

    @property
    def index_reflects_included_columns(self) -> exclusions.compound:
        return _not_supported("no INCLUDE columns in indexes")

    @property
    def indexes_with_expressions(self) -> exclusions.compound:
        return _not_supported("functional indexes are not reflected as such")

    @property
    def reflect_indexes_with_expressions(self) -> exclusions.compound:
        return _not_supported("functional indexes are not reflected as such")

    @property
    def temp_table_names(self) -> exclusions.compound:
        return _not_supported("temporary tables are not listed in the catalog")

    # DDL -----------------------------------------------------------------

    @property
    def table_ddl_if_exists(self) -> exclusions.compound:
        return exclusions.open()

    @property
    def unbounded_varchar(self) -> exclusions.compound:
        return _not_supported("VARCHAR needs a length")

    # SQL -----------------------------------------------------------------

    @property
    def update_returning_dialect_not_supported(self) -> exclusions.compound:
        # MariaDB 13 runs UPDATE ... RETURNING, which SQLAlchemy's dialect
        # does not use yet, so the statement no longer fails.
        return exclusions.skip_if(
            _mariadb_13_or_later, "MariaDB 13 runs UPDATE ... RETURNING"
        )

    @property
    def sql_expression_limit_offset(self) -> exclusions.compound:
        return _not_supported("LIMIT and OFFSET take only literals or parameters")

    # Types ---------------------------------------------------------------

    @property
    def datetime_microseconds(self) -> exclusions.compound:
        # DATETIME and TIME keep fractions only with an explicit precision,
        # which the suite's generic types do not declare.
        return _not_supported("DATETIME without a precision drops microseconds")

    @property
    def time_microseconds(self) -> exclusions.compound:
        return _not_supported("TIME without a precision drops microseconds")

    @property
    def date_implicit_bound(self) -> exclusions.compound:
        # Parameters are interpolated as quoted literals (as every MySQL
        # driver but mariadbconnector does): SELECT %s returns the text.
        return _not_supported("a bound date reaches the server as a string")

    @property
    def time_implicit_bound(self) -> exclusions.compound:
        return _not_supported("a bound time reaches the server as a string")

    @property
    def datetime_implicit_bound(self) -> exclusions.compound:
        return _not_supported("a bound datetime reaches the server as a string")

    @property
    def precision_generic_float_type(self) -> exclusions.compound:
        return _not_supported("FLOAT keeps about seven significant digits")
