//! `rustmy._core`: the native extension module behind the `rustmy` package.

mod connection;
mod convert;
mod errors;
mod params;
mod stream;

use pyo3::prelude::*;

/// Native module initialiser.
///
/// `gil_used = false` declares free-threaded (3.14t+) support; every type
/// exposed here must therefore be safe to use without the GIL.
#[pymodule(gil_used = false)]
#[pyo3(name = "_core")]
fn core(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    m.add("MAX_PACKET_PAYLOAD", rustmy_proto::MAX_PACKET_PAYLOAD)?;
    m.add_class::<connection::Connection>()?;
    m.add_class::<stream::ResultStream>()?;
    m.add_function(wrap_pyfunction!(connection::connect, m)?)?;
    m.add_function(wrap_pyfunction!(connection::literal, m)?)?;
    m.add_function(wrap_pyfunction!(connection::escape_string, m)?)?;
    m.add_function(wrap_pyfunction!(connection::interpolate, m)?)?;
    errors::register(m)?;
    Ok(())
}
