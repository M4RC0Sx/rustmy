//! RSA key exchange of `caching_sha2_password` / `sha256_password` against a
//! fake server playing a man in the middle over plain TCP: it may hand the
//! client a public key of its own at any point, and the client must take one
//! only right after asking for it, never when a key is pinned with
//! `server_public_key`. `mysql_clear_password` never sends the password
//! over plain TCP, whether the greeting or an auth switch asks for it.
//!
//! No real server needed: these run with the ordinary tests.

#![expect(
    clippy::unwrap_used,
    clippy::unwrap_in_result,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers outside #[test] functions must fail loudly too"
)]

use std::fs;
use std::io::Read;
use std::io::Write;
use std::net::TcpListener;
use std::net::TcpStream;
use std::thread;

use rustmy_proto::Conn;
use rustmy_proto::Error;
use rustmy_proto::Opts;
use rustmy_proto::SslMode;
use rustmy_proto::TlsOpts;
use rustmy_proto::constants::capability;

const OK: &[u8] = &[0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00];

/// The key the client pins with `server_public_key`.
const PINNED_KEY: &str = "-----BEGIN PUBLIC KEY-----
MIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEA0pl/7KX+Y+Op9v7bsw4l
xub/tDUFXzIkb4+JMH4MTOP+Rs0BTkt8Bsrn/xbsY0WJPA3U5ad0s7qCGQvkPaRJ
1bdAGEprDIB+w6yfF38V8se4VY4NfKEYopXbMXhiMziRlFjjA5ms3RN9NqkFYUYc
xf2/HQ4oWZsrwqpMqbEm9dJZkg3W4okYgKKYvI9BuxMCwkmPjj5NGQnjVeVJWt8T
8uXKDB0K7zsTJXgTBIqdMhWs2GNv0IW+j0s9amkbdk4Qb6hUSFOnq8alt5g2jLlD
qtxAu90xs0VGVpseU4/Q+kEeyBLaH4DgCONoLxICAQfdh7iRLVk/LnkKTX6BwjLu
bwIDAQAB
-----END PUBLIC KEY-----
";

/// The key the man in the middle offers instead.
const ATTACKER_KEY: &str = "-----BEGIN PUBLIC KEY-----
MIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEAl72uiz9x9gCkBVZsxku8
LhtnNRMreIKx5jwRBnpMT2Dfy3pYTIc7GqnWOcTn3NYqho9bNgzDEA0XVZCLnobN
aQC1UuMkO/QUYhJt1DgZ7Kjup8hs8pafQhmzcgiSnHHmuRAIx3m2kvwYYcMTGfUI
iy7bzXCsZl7mVhVnuRYsMqjRDKvHu0rJMlXxfoT2M9DsOUCHQBusChIfxh5g/ezr
QJ7ede7QhJLEFQAmKFGE2UWwXxG5I3GeYRy5MXPO3UR2aWRJYjic/zxseCpq4gND
uM/JnY3XAx6db2y2gobVLNXHqoVjnBDiXBQdUYDFSYxrh7h0zP1MgT8m+uTlLnw4
vwIDAQAB
-----END PUBLIC KEY-----
";

/// RSA-OAEP output for a 2048-bit key: the encrypted password.
const ENCRYPTED_LEN: usize = 256;

/// The server's side of the connection.
struct Peer {
    stream: TcpStream,
    /// Sequence id of the next packet the server writes.
    seq: u8,
}

impl Peer {
    /// One payload, or `None` once the client closed the connection.
    fn read(&mut self) -> Option<Vec<u8>> {
        let mut header = [0; 4];
        self.stream.read_exact(&mut header).ok()?;
        assert_eq!(header[3], self.seq, "client packet sequence");
        let len = u32::from_le_bytes([header[0], header[1], header[2], 0]) as usize;
        let mut payload = vec![0; len];
        self.stream.read_exact(&mut payload).ok()?;
        self.seq = self.seq.wrapping_add(1);
        Some(payload)
    }

    fn write(&mut self, payload: &[u8]) {
        let len = u32::try_from(payload.len()).unwrap().to_le_bytes();
        let mut packet = vec![len[0], len[1], len[2], self.seq];
        packet.extend_from_slice(payload);
        self.stream.write_all(&packet).unwrap();
        self.seq = self.seq.wrapping_add(1);
    }

    /// `AuthMoreData` carrying `data`.
    fn more_data(&mut self, data: &[u8]) {
        let mut packet = vec![0x01];
        packet.extend_from_slice(data);
        self.write(&packet);
    }

    /// Switches the client to `sha256_password` with a fresh nonce.
    fn switch_to_sha256(&mut self) {
        let mut packet = vec![0xFE];
        packet.extend_from_slice(b"sha256_password\0");
        packet.extend_from_slice(b"ABCDEFGHIJKLMNOPQRST\0");
        self.write(&packet);
    }

    /// The client must have closed the connection without sending anything
    /// else (in particular, no password encrypted with the attacker's key).
    fn expect_hang_up(&mut self) {
        assert_eq!(self.read(), None, "the client must not answer");
    }
}

/// Greets with `caching_sha2_password`, reads the handshake response and
/// runs `script`; returns what `Conn::connect` gave.
fn exchange(pinned: bool, script: impl FnOnce(&mut Peer) + Send + 'static) -> Result<Conn, Error> {
    exchange_with(pinned, true, script)
}

/// [`exchange`], with `get_server_public_key` set to `get_key`.
fn exchange_with(
    pinned: bool,
    get_key: bool,
    script: impl FnOnce(&mut Peer) + Send + 'static,
) -> Result<Conn, Error> {
    serve(b"caching_sha2_password", pinned, get_key, |peer| {
        peer.read().expect("handshake response");
        script(peer);
    })
}

/// Greets with `plugin` as the server's default and runs `script` from
/// there; returns what `Conn::connect` gave.
fn serve(
    plugin: &'static [u8],
    pinned: bool,
    get_key: bool,
    script: impl FnOnce(&mut Peer) + Send + 'static,
) -> Result<Conn, Error> {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut peer = Peer { stream, seq: 0 };
        let capabilities = capability::LONG_PASSWORD
            | capability::LONG_FLAG
            | capability::PROTOCOL_41
            | capability::TRANSACTIONS
            | capability::SECURE_CONNECTION
            | capability::MULTI_RESULTS
            | capability::PLUGIN_AUTH
            | capability::PLUGIN_AUTH_LENENC_CLIENT_DATA
            | capability::DEPRECATE_EOF;
        let [c0, c1, c2, c3] = capabilities.to_le_bytes();
        let mut greeting = vec![10];
        greeting.extend_from_slice(b"8.4.0-fake\0");
        greeting.extend_from_slice(&7u32.to_le_bytes());
        greeting.extend_from_slice(b"abcdefgh\0");
        greeting.extend_from_slice(&[c0, c1, 255, 0x02, 0x00, c2, c3, 21]);
        greeting.extend_from_slice(&[0; 10]);
        greeting.extend_from_slice(b"ijklmnopqrst\0");
        greeting.extend_from_slice(plugin);
        greeting.push(0);
        peer.write(&greeting);
        script(&mut peer);
    });

    let key_file = pinned.then(|| {
        let path = std::env::temp_dir().join(format!("rustmy-pinned-key-{port}.pem"));
        fs::write(&path, PINNED_KEY).unwrap();
        path
    });
    let result = Conn::connect(&Opts {
        host: "127.0.0.1".to_owned(),
        port,
        user: b"fake".to_vec(),
        password: "secret".to_owned(),
        tls: TlsOpts {
            mode: SslMode::Disabled,
            ..TlsOpts::default()
        },
        server_public_key: key_file.clone(),
        get_server_public_key: get_key,
        ..Opts::default()
    });
    if let Some(path) = key_file {
        fs::remove_file(path).unwrap();
    }
    // On failure the client has already closed its side, which is what the
    // server waits for; on success the script ends with the OK.
    server.join().unwrap();
    result
}

fn assert_refused(result: Result<Conn, Error>) {
    match result {
        Err(Error::Protocol(message)) => {
            assert!(message.contains("did not request"), "{message}");
        }
        other => panic!("expected the key to be refused, got {other:?}"),
    }
}

fn offer_attacker_key(peer: &mut Peer) {
    peer.more_data(ATTACKER_KEY.as_bytes());
    peer.expect_hang_up();
}

#[test]
fn a_pinned_key_is_never_replaced() {
    assert_refused(exchange(true, offer_attacker_key));
}

#[test]
fn a_key_nobody_asked_for_is_refused() {
    assert_refused(exchange(false, offer_attacker_key));
}

#[test]
fn full_authentication_uses_the_pinned_key_and_refuses_another() {
    assert_refused(exchange(true, |peer| {
        peer.more_data(&[0x04]);
        let encrypted = peer.read().expect("the encrypted password");
        assert_eq!(encrypted.len(), ENCRYPTED_LEN);
        offer_attacker_key(peer);
    }));
}

#[test]
fn full_authentication_takes_the_key_it_requested_once() {
    assert_refused(exchange(false, |peer| {
        peer.more_data(&[0x04]);
        assert_eq!(peer.read().expect("a key request"), [0x02]);
        peer.more_data(ATTACKER_KEY.as_bytes());
        let encrypted = peer.read().expect("the encrypted password");
        assert_eq!(encrypted.len(), ENCRYPTED_LEN);
        // A second key was not requested.
        offer_attacker_key(peer);
    }));
}

#[test]
fn full_authentication_with_a_requested_key_connects() {
    exchange(false, |peer| {
        peer.more_data(&[0x04]);
        assert_eq!(peer.read().expect("a key request"), [0x02]);
        peer.more_data(PINNED_KEY.as_bytes());
        let encrypted = peer.read().expect("the encrypted password");
        assert_eq!(encrypted.len(), ENCRYPTED_LEN);
        peer.write(OK);
    })
    .unwrap();
}

#[test]
fn sha256_password_with_a_pinned_key_refuses_another() {
    assert_refused(exchange(true, |peer| {
        peer.switch_to_sha256();
        let encrypted = peer.read().expect("the encrypted password");
        assert_eq!(encrypted.len(), ENCRYPTED_LEN);
        offer_attacker_key(peer);
    }));
}

#[test]
fn sha256_password_takes_the_key_it_requested() {
    exchange(false, |peer| {
        peer.switch_to_sha256();
        assert_eq!(peer.read().expect("a key request"), [0x01]);
        peer.more_data(PINNED_KEY.as_bytes());
        let encrypted = peer.read().expect("the encrypted password");
        assert_eq!(encrypted.len(), ENCRYPTED_LEN);
        peer.write(OK);
    })
    .unwrap();
}

/// With `get_server_public_key` off, the client must hang up instead of
/// asking for a key, and say why.
fn assert_key_not_requested(result: Result<Conn, Error>, plugin: &str) {
    match result {
        Err(Error::Auth(message)) => {
            assert!(message.contains(plugin), "{message}");
            assert!(message.contains("get_server_public_key"), "{message}");
        }
        other => panic!("expected an authentication error, got {other:?}"),
    }
}

#[test]
fn full_authentication_without_key_requests_hangs_up() {
    assert_key_not_requested(
        exchange_with(false, false, |peer| {
            peer.more_data(&[0x04]);
            peer.expect_hang_up();
        }),
        "caching_sha2_password",
    );
}

#[test]
fn sha256_password_without_key_requests_hangs_up() {
    assert_key_not_requested(
        exchange_with(false, false, |peer| {
            peer.switch_to_sha256();
            peer.expect_hang_up();
        }),
        "sha256_password",
    );
}

#[test]
fn without_key_requests_a_pinned_key_still_works() {
    exchange_with(true, false, |peer| {
        peer.more_data(&[0x04]);
        let encrypted = peer.read().expect("the encrypted password");
        assert_eq!(encrypted.len(), ENCRYPTED_LEN);
        peer.write(OK);
    })
    .unwrap();
}

#[test]
fn without_key_requests_fast_authentication_still_works() {
    exchange_with(false, false, |peer| {
        peer.more_data(&[0x03]);
        peer.write(OK);
    })
    .unwrap();
}

/// Over plain TCP, `mysql_clear_password` must end the connection without
/// sending the password, and say why.
fn assert_clear_password_refused(result: Result<Conn, Error>) {
    match result {
        Err(Error::Auth(message)) => {
            assert!(message.contains("mysql_clear_password"), "{message}");
            assert!(message.contains("TLS"), "{message}");
        }
        other => panic!("expected an authentication error, got {other:?}"),
    }
}

#[test]
fn a_switch_to_the_clear_password_plugin_hangs_up_over_plain_tcp() {
    assert_clear_password_refused(exchange(false, |peer| {
        peer.write(b"\xFEmysql_clear_password\0");
        peer.expect_hang_up();
    }));
}

#[test]
fn a_greeting_asking_for_the_clear_password_gets_no_response() {
    assert_clear_password_refused(serve(b"mysql_clear_password", false, true, |peer| {
        peer.expect_hang_up();
    }));
}
