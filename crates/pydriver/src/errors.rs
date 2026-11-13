//! PEP 249 exception hierarchy and the mapping from protocol errors to it.
//!
//! Every exception carries `(code, message)` as its args, like other MySQL
//! drivers: `SQLAlchemy` reads `args[0]` to recognise disconnects.

use pyo3::create_exception;
use pyo3::exceptions::PyException;
use pyo3::prelude::*;
use rustmy_proto::Error as ProtoError;

create_exception!(
    rustmy,
    Warning,
    PyException,
    "Important warnings such as data truncation."
);
create_exception!(
    rustmy,
    Error,
    PyException,
    "Base class of all rustmy errors."
);
create_exception!(
    rustmy,
    InterfaceError,
    Error,
    "Misuse of the driver itself rather than a database problem."
);
create_exception!(
    rustmy,
    DatabaseError,
    Error,
    "Errors related to the database."
);
create_exception!(
    rustmy,
    DataError,
    DatabaseError,
    "Problems with processed data (division by zero, value out of range...)."
);
create_exception!(
    rustmy,
    OperationalError,
    DatabaseError,
    "Errors in the database's operation (lost connection, deadlock...)."
);
create_exception!(
    rustmy,
    IntegrityError,
    DatabaseError,
    "Relational integrity violations (duplicate key, foreign key...)."
);
create_exception!(
    rustmy,
    InternalError,
    DatabaseError,
    "Internal database errors."
);
create_exception!(
    rustmy,
    ProgrammingError,
    DatabaseError,
    "SQL errors (syntax, missing table...) and API misuse."
);
create_exception!(
    rustmy,
    NotSupportedError,
    DatabaseError,
    "A feature not supported by the server or the driver."
);

/// Client-side error codes, numbered like the official client's so that
/// tools that inspect `args[0]` keep working.
pub(crate) mod client_code {
    pub(crate) const CONN_HOST_ERROR: u16 = 2003;
    pub(crate) const SERVER_GONE: u16 = 2006;
    pub(crate) const SERVER_LOST: u16 = 2013;
    pub(crate) const MALFORMED_PACKET: u16 = 2027;
}

pub(crate) fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = m.py();
    m.add("Warning", py.get_type::<Warning>())?;
    m.add("Error", py.get_type::<Error>())?;
    m.add("InterfaceError", py.get_type::<InterfaceError>())?;
    m.add("DatabaseError", py.get_type::<DatabaseError>())?;
    m.add("DataError", py.get_type::<DataError>())?;
    m.add("OperationalError", py.get_type::<OperationalError>())?;
    m.add("IntegrityError", py.get_type::<IntegrityError>())?;
    m.add("InternalError", py.get_type::<InternalError>())?;
    m.add("ProgrammingError", py.get_type::<ProgrammingError>())?;
    m.add("NotSupportedError", py.get_type::<NotSupportedError>())?;
    Ok(())
}

/// Where a protocol error happened, which decides the client error code.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Phase {
    Connect,
    Command,
}

pub(crate) fn from_proto(err: ProtoError, phase: Phase, target: &str) -> PyErr {
    match err {
        ProtoError::Server {
            code,
            sql_state,
            message,
        } => server_error(code, &sql_state, message),
        ProtoError::Io(io) => match phase {
            Phase::Connect => OperationalError::new_err((
                client_code::CONN_HOST_ERROR,
                format!("Can't connect to MySQL server on {target} ({io})"),
            )),
            Phase::Command => OperationalError::new_err((
                client_code::SERVER_LOST,
                format!("Lost connection to MySQL server during query ({io})"),
            )),
        },
        ProtoError::Protocol(message) => OperationalError::new_err((
            client_code::MALFORMED_PACKET,
            format!("Malformed packet: {message}"),
        )),
        ProtoError::Unsupported(message) => NotSupportedError::new_err((0, message)),
        other => InterfaceError::new_err((0, other.to_string())),
    }
}

/// Picks the exception class from the SQLSTATE class (its first two
/// characters), the portable classification defined by the SQL standard.
fn server_error(code: u16, sql_state: &str, message: String) -> PyErr {
    let args = (code, message);
    match sql_state.get(..2) {
        Some("23") => IntegrityError::new_err(args),
        Some("42") => ProgrammingError::new_err(args),
        Some("22") => DataError::new_err(args),
        Some("0A") => NotSupportedError::new_err(args),
        Some("XX") => InternalError::new_err(args),
        // 08 connection, 28 auth, 40 rollback/deadlock, HY general...
        _ => OperationalError::new_err(args),
    }
}
