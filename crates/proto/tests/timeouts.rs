//! `connect_timeout` bounds the whole handshake, not only opening the TCP
//! connection: a server (or a proxy in front of it) that accepts and then
//! never answers must fail. Once authenticated, `read_timeout` takes over.
//!
//! Fake servers on loopback: these run with the ordinary tests.

#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers outside #[test] functions must fail loudly too"
)]

use std::io;
use std::io::Read;
use std::io::Write;
use std::net::TcpListener;
use std::net::TcpStream;
use std::thread;
use std::time::Duration;
use std::time::Instant;

use rustmy_proto::Conn;
use rustmy_proto::Error;
use rustmy_proto::Opts;
use rustmy_proto::constants::capability;

const OK: &[u8] = &[0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00];
const SHORT: Duration = Duration::from_millis(300);

/// The server's side of the connection.
struct Peer {
    stream: TcpStream,
    seq: u8,
}

impl Peer {
    fn read(&mut self) -> Option<Vec<u8>> {
        let mut header = [0; 4];
        self.stream.read_exact(&mut header).ok()?;
        let len = u32::from_le_bytes([header[0], header[1], header[2], 0]) as usize;
        let mut payload = vec![0; len];
        self.stream.read_exact(&mut payload).ok()?;
        self.seq = header[3].wrapping_add(1);
        Some(payload)
    }

    fn write(&mut self, payload: &[u8]) {
        let len = u32::try_from(payload.len()).unwrap().to_le_bytes();
        let mut packet = vec![len[0], len[1], len[2], self.seq];
        packet.extend_from_slice(payload);
        self.stream.write_all(&packet).unwrap();
        self.seq = self.seq.wrapping_add(1);
    }

    fn greet(&mut self) {
        let capabilities = capability::LONG_PASSWORD
            | capability::LONG_FLAG
            | capability::PROTOCOL_41
            | capability::TRANSACTIONS
            | capability::SECURE_CONNECTION
            | capability::MULTI_RESULTS
            | capability::PLUGIN_AUTH
            | capability::DEPRECATE_EOF;
        let [c0, c1, c2, c3] = capabilities.to_le_bytes();
        let mut greeting = vec![10];
        greeting.extend_from_slice(b"8.4.0-fake\0");
        greeting.extend_from_slice(&7u32.to_le_bytes());
        greeting.extend_from_slice(b"abcdefgh\0");
        greeting.extend_from_slice(&[c0, c1, 255, 0x02, 0x00, c2, c3, 21]);
        greeting.extend_from_slice(&[0; 10]);
        greeting.extend_from_slice(b"ijklmnopqrst\0");
        greeting.extend_from_slice(b"mysql_native_password\0");
        self.write(&greeting);
    }

    /// Greets and accepts whatever the client answers.
    fn log_in(&mut self) {
        self.greet();
        self.read().expect("handshake response");
        self.write(OK);
    }

    /// Waits, silent, until the client hangs up.
    fn wait_for_hang_up(&mut self) {
        let mut rest = Vec::new();
        let _ = self.stream.read_to_end(&mut rest);
    }
}

/// Runs `script` as the server and connects to it with `opts`.
fn connect(
    opts: Opts,
    script: impl FnOnce(&mut Peer) + Send + 'static,
) -> (Result<Conn, Error>, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        script(&mut Peer { stream, seq: 0 });
    });
    let result = Conn::connect(&Opts {
        host: "127.0.0.1".to_owned(),
        port,
        user: b"fake".to_vec(),
        ..opts
    });
    (result, server)
}

fn assert_timed_out<T: std::fmt::Debug>(result: Result<T, Error>, started: Instant) {
    match result {
        Err(Error::Io(err)) => assert!(
            matches!(
                err.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
            ),
            "{err:?}"
        ),
        other => panic!("expected a timeout, got {other:?}"),
    }
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
}

#[test]
fn a_server_that_never_greets_times_out() {
    let started = Instant::now();
    let (result, server) = connect(
        Opts {
            connect_timeout: Some(SHORT),
            ..Opts::default()
        },
        Peer::wait_for_hang_up,
    );
    assert_timed_out(result, started);
    server.join().unwrap();
}

#[test]
fn the_connect_timeout_bounds_authentication() {
    let started = Instant::now();
    let (result, server) = connect(
        Opts {
            connect_timeout: Some(SHORT),
            ..Opts::default()
        },
        |peer| {
            peer.greet();
            peer.read().expect("handshake response");
            peer.wait_for_hang_up();
        },
    );
    assert_timed_out(result, started);
    server.join().unwrap();
}

/// The server answers the first command after `delay`.
fn slow_ping(delay: Duration) -> impl FnOnce(&mut Peer) + Send + 'static {
    move |peer| {
        peer.log_in();
        if peer.read().is_some() {
            thread::sleep(delay);
            peer.write(OK);
        }
        peer.wait_for_hang_up();
    }
}

#[test]
fn the_connect_timeout_ends_with_authentication() {
    let (result, server) = connect(
        Opts {
            connect_timeout: Some(SHORT),
            ..Opts::default()
        },
        slow_ping(SHORT * 2),
    );
    let mut conn = result.unwrap();
    conn.ping().unwrap();
    drop(conn);
    server.join().unwrap();
}

#[test]
fn the_read_timeout_applies_once_connected() {
    let (result, server) = connect(
        Opts {
            connect_timeout: Some(Duration::from_secs(10)),
            read_timeout: Some(SHORT),
            ..Opts::default()
        },
        slow_ping(SHORT * 3),
    );
    let mut conn = result.unwrap();
    let started = Instant::now();
    assert_timed_out(conn.ping(), started);
    drop(conn);
    server.join().unwrap();
}
