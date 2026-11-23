//! The byte stream under a connection, as its operations see it: refill
//! the input buffer, read a large body straight into its destination,
//! write, flush.
//!
//! The protocol's operations are `async fn`s over a [`Transport`], written
//! once. A blocking stream ([`Blocking`]) never leaves them pending, so
//! [`block_on`] runs one to the end with a single poll, with no runtime and
//! no thread; an event loop drives the same operations with a transport of
//! its own, polling them again as bytes arrive.

use std::fmt;
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
use crate::infile;
use crate::infile::LocalFile;
use crate::packet::Input;
use crate::tls::TlsOpts;
use crate::tls::client_config;
use crate::tls::server_name;
use crate::tls::tls_error;

/// Bytes read from the stream per refill, at most.
pub(crate) const READ_BUFFER_CAPACITY: usize = 64 * 1024;

/// What the protocol's operations need from the byte stream.
pub trait Transport {
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

    /// Replaces what `buf` holds with the next piece of `file`, a local
    /// file being sent (`LOAD DATA LOCAL INFILE`): empty at its end. The
    /// default reads a packet's worth right here; an event loop's
    /// transport reads it elsewhere, so the loop never waits on the disk.
    fn read_file(
        &mut self,
        file: &LocalFile,
        buf: &mut Vec<u8>,
    ) -> impl Future<Output = io::Result<()>> {
        std::future::ready(file.read_into(buf, infile::CHUNK))
    }
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

/// A transport that starts plain and may turn to TLS ([`StartTls`]): the
/// records are encrypted and decrypted in memory (`rustls`), so it works
/// over any transport, an event loop's included, with the configuration
/// the blocking connection uses (the same verification and cache).
pub struct MaybeTls<T> {
    inner: T,
    tls: Option<Box<rustls::ClientConnection>>,
    /// TLS records received and not yet given to `rustls`.
    records: Input,
}

impl<T: fmt::Debug> fmt::Debug for MaybeTls<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MaybeTls")
            .field("inner", &self.inner)
            .field("tls", &self.tls.is_some())
            .finish_non_exhaustive()
    }
}

/// A transport that can switch to TLS in the middle of the handshake,
/// right after the client's `SSLRequest`.
pub trait StartTls: Transport {
    /// Runs the TLS handshake with `host` over the stream; from then on,
    /// everything read and written is encrypted.
    fn start_tls(&mut self, tls: &TlsOpts, host: &str) -> impl Future<Output = Result<()>>;

    /// Whether TLS is on.
    fn is_tls(&self) -> bool;
}

/// A TLS error as the I/O error reads and writes report it.
fn tls_io_error(err: rustls::Error) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, err)
}

impl<T: Transport> MaybeTls<T> {
    /// `inner`, plain until [`StartTls::start_tls`].
    pub const fn new(inner: T) -> Self {
        Self {
            inner,
            tls: None,
            records: Input::new(),
        }
    }

    pub const fn get_ref(&self) -> &T {
        &self.inner
    }

    pub const fn get_mut(&mut self) -> &mut T {
        &mut self.inner
    }

    /// Sends the records `rustls` has ready.
    async fn send_records(&mut self) -> io::Result<()> {
        let Some(tls) = &mut self.tls else {
            return Ok(());
        };
        let mut records = Vec::new();
        while tls.wants_write() {
            tls.write_tls(&mut records)?;
        }
        if !records.is_empty() {
            self.inner.write(&[&records]).await?;
            self.inner.flush().await?;
        }
        Ok(())
    }

    /// Hands `rustls` the next records from the stream: false at its end.
    async fn receive_records(&mut self) -> io::Result<bool> {
        if self.records.is_empty() && self.inner.fill(&mut self.records).await? == 0 {
            return Ok(false);
        }
        let Some(tls) = &mut self.tls else {
            return Ok(true);
        };
        let mut data = self.records.data();
        let before = data.len();
        let read = tls.read_tls(&mut data);
        let used = before - data.len();
        self.records.consume(used);
        read?;
        if let Err(err) = tls.process_new_packets() {
            // Tell the server why, if it still listens.
            let _ = self.send_records().await;
            return Err(tls_io_error(err));
        }
        Ok(true)
    }

    async fn fill_tls(&mut self, input: &mut Input) -> io::Result<usize> {
        loop {
            if let Some(tls) = &mut self.tls {
                match Read::read(&mut tls.reader(), input.spare(READ_BUFFER_CAPACITY)) {
                    Ok(n) => {
                        input.filled(n);
                        return Ok(n);
                    }
                    Err(err) if err.kind() == io::ErrorKind::WouldBlock => {}
                    Err(err) => return Err(err),
                }
            }
            // Alerts and key updates `rustls` answers.
            self.send_records().await?;
            if !self.receive_records().await? {
                return Ok(0);
            }
        }
    }

    async fn write_tls(&mut self, parts: &[&[u8]]) -> io::Result<()> {
        if let Some(tls) = &mut self.tls {
            for part in parts {
                Write::write_all(&mut tls.writer(), part)?;
            }
        }
        self.send_records().await
    }
}

impl<T: Transport> Transport for MaybeTls<T> {
    async fn fill(&mut self, input: &mut Input) -> io::Result<usize> {
        if self.tls.is_none() {
            return self.inner.fill(input).await;
        }
        self.fill_tls(input).await
    }

    async fn read_body(&mut self, out: &mut Vec<u8>, len: usize) -> Option<io::Result<usize>> {
        if self.tls.is_some() {
            return None;
        }
        self.inner.read_body(out, len).await
    }

    async fn write(&mut self, parts: &[&[u8]]) -> io::Result<()> {
        if self.tls.is_none() {
            return self.inner.write(parts).await;
        }
        self.write_tls(parts).await
    }

    fn flush(&mut self) -> impl Future<Output = io::Result<()>> {
        self.inner.flush()
    }

    fn read_file(
        &mut self,
        file: &LocalFile,
        buf: &mut Vec<u8>,
    ) -> impl Future<Output = io::Result<()>> {
        self.inner.read_file(file, buf)
    }

    fn shutdown(&mut self) {
        if let Some(tls) = &mut self.tls {
            tls.send_close_notify();
            let mut records = Vec::new();
            while tls.wants_write() && tls.write_tls(&mut records).is_ok() {}
            // Best effort: sent if the transport takes it right away.
            let _ = block_on(async {
                self.inner.write(&[&records]).await?;
                self.inner.flush().await?;
                Ok(())
            });
        }
        self.inner.shutdown();
    }
}

impl<T: Transport> StartTls for MaybeTls<T> {
    fn start_tls(&mut self, tls: &TlsOpts, host: &str) -> impl Future<Output = Result<()>> {
        let session = client_config(tls).and_then(|config| {
            rustls::ClientConnection::new(config, server_name(host)?).map_err(tls_error)
        });
        async move {
            let mut session = session?;
            // Everything written goes out at once: no plaintext limit.
            session.set_buffer_limit(None);
            self.tls = Some(Box::new(session));
            loop {
                self.send_records().await?;
                let Some(tls) = &self.tls else {
                    return Ok(());
                };
                if !tls.is_handshaking() {
                    return Ok(());
                }
                match self.receive_records().await {
                    Ok(true) => {}
                    Ok(false) => {
                        return Err(Error::Tls(
                            "the server closed the connection during the TLS handshake".to_owned(),
                        ));
                    }
                    Err(err) => return Err(tls_error(err)),
                }
            }
        }
    }

    fn is_tls(&self) -> bool {
        self.tls.is_some()
    }
}
