//! The byte stream under a connection, as its operations see it: refill
//! the input buffer, read a large body straight into its destination,
//! write, flush.
//!
//! The protocol's operations are `async fn`s over a [`Transport`], written
//! once. A blocking stream ([`Blocking`]) never leaves them pending, so
//! [`block_on`] runs one to the end with a single poll, with no runtime and
//! no thread; an event loop drives the same operations with a transport of
//! its own, polling them again as bytes arrive.

use std::future::Future;
use std::io;
use std::io::Read;
use std::io::Write;
use std::pin::pin;
use std::task::Context;
use std::task::Poll;
use std::task::Waker;

use crate::error::Error;
use crate::error::Result;
use crate::packet::Input;

/// Bytes read from the stream per refill, at most.
pub(crate) const READ_BUFFER_CAPACITY: usize = 64 * 1024;

/// What the protocol's operations need from the byte stream.
pub(crate) trait Transport {
    /// Reads what the server sent into `input`: how many bytes, at least
    /// one, or 0 at the end of the stream.
    fn fill(&mut self, input: &mut Input) -> impl Future<Output = io::Result<usize>>;

    /// Appends `len` bytes of a large body straight to `out` (fewer only
    /// at the end of the stream), if this transport can; `None` to go
    /// through [`Transport::fill`] instead.
    fn read_body(
        &mut self,
        out: &mut Vec<u8>,
        len: usize,
    ) -> impl Future<Output = Option<io::Result<usize>>>;

    /// Writes every part, in order.
    fn write(&mut self, parts: &[&[u8]]) -> impl Future<Output = io::Result<()>>;

    fn flush(&mut self) -> impl Future<Output = io::Result<()>>;

    /// Closes the stream, best effort: the peer may already be gone.
    fn shutdown(&mut self);
}

/// A blocking byte stream: every operation completes before it returns.
#[derive(Debug)]
pub(crate) struct Blocking<S>(pub(crate) S);

impl<S: Read + Write> Blocking<S> {
    fn fill_now(&mut self, input: &mut Input) -> io::Result<usize> {
        loop {
            match self.0.read(input.spare(READ_BUFFER_CAPACITY)) {
                Ok(n) => {
                    input.filled(n);
                    return Ok(n);
                }
                Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                Err(err) => return Err(err),
            }
        }
    }

    fn write_now(&mut self, parts: &[&[u8]]) -> io::Result<()> {
        parts.iter().try_for_each(|part| self.0.write_all(part))
    }
}

impl<S: Read + Write + Close> Transport for Blocking<S> {
    fn fill(&mut self, input: &mut Input) -> impl Future<Output = io::Result<usize>> {
        std::future::ready(self.fill_now(input))
    }

    fn read_body(
        &mut self,
        out: &mut Vec<u8>,
        len: usize,
    ) -> impl Future<Output = Option<io::Result<usize>>> {
        let wanted = u64::try_from(len).unwrap_or(u64::MAX);
        std::future::ready(Some((&mut self.0).take(wanted).read_to_end(out)))
    }

    fn write(&mut self, parts: &[&[u8]]) -> impl Future<Output = io::Result<()>> {
        std::future::ready(self.write_now(parts))
    }

    fn flush(&mut self) -> impl Future<Output = io::Result<()>> {
        std::future::ready(self.0.flush())
    }

    fn shutdown(&mut self) {
        self.0.close();
    }
}

/// Closing a blocking stream (a no-op for in-memory ones).
pub(crate) trait Close {
    fn close(&mut self) {}
}

/// Runs an operation over a [`Blocking`] transport to its end: it is never
/// left pending, so one poll does it.
pub(crate) fn block_on<T>(operation: impl Future<Output = Result<T>>) -> Result<T> {
    let mut operation = pin!(operation);
    match operation
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
    {
        Poll::Ready(output) => output,
        Poll::Pending => Err(Error::protocol("a blocking operation was left pending")),
    }
}
