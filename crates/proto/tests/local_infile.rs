//! `LOAD DATA LOCAL INFILE` against a fake, possibly malicious, server: a
//! TCP listener that greets, accepts any password and then replies to each
//! command as the test scripts it, recording what the client sends.
//!
//! No real server needed: these run with the ordinary tests.

#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers outside #[test] functions must fail loudly too"
)]

use std::fs;
use std::io::Read;
use std::io::Write;
use std::net::TcpListener;
use std::net::TcpStream;
use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::AtomicU32;
use std::sync::atomic::Ordering;
use std::thread;
use std::thread::JoinHandle;

use rustmy_proto::Conn;
use rustmy_proto::Error;
use rustmy_proto::Opts;
use rustmy_proto::SslMode;
use rustmy_proto::TlsOpts;
use rustmy_proto::constants::capability;

const OK: &[u8] = &[0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00];

/// The server's side of the connection.
struct Peer {
    stream: TcpStream,
    /// Capabilities the client asked for in its handshake response.
    client_capabilities: u32,
}

impl Peer {
    /// One packet, or `None` once the client closed the connection.
    fn read(&mut self) -> Option<(u8, Vec<u8>)> {
        let mut header = [0; 4];
        self.stream.read_exact(&mut header).ok()?;
        let len = u32::from_le_bytes([header[0], header[1], header[2], 0]) as usize;
        let mut payload = vec![0; len];
        self.stream.read_exact(&mut payload).ok()?;
        Some((header[3], payload))
    }

    fn write(&mut self, seq: u8, payload: &[u8]) {
        let len = u32::try_from(payload.len()).unwrap().to_le_bytes();
        let mut packet = vec![len[0], len[1], len[2], seq];
        packet.extend_from_slice(payload);
        self.stream.write_all(&packet).unwrap();
    }

    /// Reads a `COM_QUERY` and returns its SQL.
    fn query(&mut self) -> String {
        let (seq, payload) = self.read().expect("a command");
        assert_eq!((seq, payload[0]), (0, 0x03), "expected COM_QUERY");
        String::from_utf8(payload[1..].to_vec()).unwrap()
    }

    /// Asks for `name` (`seq` 1, as the reply to a command) and returns
    /// what the client sent back as the file.
    fn request(&mut self, name: &[u8]) -> Vec<u8> {
        let mut request = vec![0xFB];
        request.extend_from_slice(name);
        self.write(1, &request);
        let mut file = Vec::new();
        let mut expected = 2;
        loop {
            let (seq, payload) = self.read().expect("file packets");
            assert_eq!(seq, expected, "file packet sequence");
            expected += 1;
            if payload.is_empty() {
                // The reply continues the sequence.
                self.write(expected, OK);
                return file;
            }
            file.extend_from_slice(&payload);
        }
    }
}

/// Starts a server that runs `script` once the client is authenticated.
fn serve(script: impl FnOnce(&mut Peer) + Send + 'static) -> (u16, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut peer = Peer {
            stream,
            client_capabilities: 0,
        };
        let capabilities = capability::LONG_PASSWORD
            | capability::LONG_FLAG
            | capability::PROTOCOL_41
            | capability::TRANSACTIONS
            | capability::SECURE_CONNECTION
            | capability::MULTI_STATEMENTS
            | capability::MULTI_RESULTS
            | capability::PLUGIN_AUTH
            | capability::PLUGIN_AUTH_LENENC_CLIENT_DATA
            | capability::DEPRECATE_EOF
            | capability::LOCAL_FILES;
        let [c0, c1, c2, c3] = capabilities.to_le_bytes();
        let mut greeting = vec![10];
        greeting.extend_from_slice(b"8.4.0-fake\0");
        greeting.extend_from_slice(&7u32.to_le_bytes());
        greeting.extend_from_slice(b"abcdefgh\0");
        greeting.extend_from_slice(&[c0, c1, 255, 0x02, 0x00, c2, c3, 21]);
        greeting.extend_from_slice(&[0; 10]);
        greeting.extend_from_slice(b"ijklmnopqrst\0");
        greeting.extend_from_slice(b"mysql_native_password\0");
        peer.write(0, &greeting);
        let (seq, response) = peer.read().expect("handshake response");
        assert_eq!(seq, 1);
        peer.client_capabilities = u32::from_le_bytes(response[..4].try_into().unwrap());
        peer.write(2, OK);
        script(&mut peer);
    });
    (port, handle)
}

fn connect(port: u16, local_infile_dir: Option<&Path>) -> Conn {
    Conn::connect(&Opts {
        host: "127.0.0.1".to_owned(),
        port,
        user: b"fake".to_vec(),
        password: "secret".to_owned(),
        tls: TlsOpts {
            mode: SslMode::Disabled,
            ..TlsOpts::default()
        },
        local_infile_dir: local_infile_dir.map(Path::to_path_buf),
        ..Opts::default()
    })
    .unwrap()
}

/// A fresh directory under the system's temporary one, removed on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let path = std::env::temp_dir().join(format!(
            "rustmy-local-infile-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        Self(fs::canonicalize(path).unwrap())
    }

    fn file(&self, name: &str, contents: &[u8]) -> String {
        let path = self.0.join(name);
        fs::write(&path, contents).unwrap();
        path.to_str().unwrap().to_owned()
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn rejected(result: Result<impl std::fmt::Debug, Error>) -> String {
    match result {
        Err(Error::LocalInfile(reason)) => reason,
        other => panic!("expected a rejected request, got {other:?}"),
    }
}

#[test]
fn sends_the_file_the_statement_names() {
    let dir = TempDir::new();
    // Several packets' worth, with every byte value.
    let contents: Vec<u8> = (0..300_000u32).flat_map(u32::to_le_bytes).collect();
    let path = dir.file("data.csv", &contents);
    let sql = format!("LOAD DATA LOCAL INFILE '{path}' INTO TABLE t");
    let (port, server) = serve(move |peer| {
        let sql = peer.query();
        let name = sql.split('\'').nth(1).unwrap().to_owned();
        assert_eq!(peer.request(name.as_bytes()), contents);
        assert_eq!(peer.query(), "DO 1");
        peer.write(1, OK);
    });
    let mut conn = connect(port, Some(&dir.0));
    conn.query(sql.as_bytes()).unwrap();
    conn.query(b"DO 1").unwrap();
    server.join().unwrap();
}

#[test]
fn refuses_files_after_other_statements() {
    let dir = TempDir::new();
    let (port, server) = serve(|peer| {
        assert_eq!(peer.query(), "SELECT 1");
        // A malicious server asks for a file the statement never named.
        assert_eq!(peer.request(b"/etc/passwd"), b"");
        // The connection is still in sync.
        assert_eq!(peer.query(), "DO 1");
        peer.write(1, OK);
    });
    let mut conn = connect(port, Some(&dir.0));
    let reason = rejected(conn.query(b"SELECT 1"));
    assert!(reason.contains("does not name"), "{reason}");
    conn.query(b"DO 1").unwrap();
    server.join().unwrap();
}

#[test]
fn refuses_another_file_than_the_one_named() {
    let dir = TempDir::new();
    let named = dir.file("named.csv", b"1\n");
    let other = dir.file("other.csv", b"secret\n");
    let sql = format!("LOAD DATA LOCAL INFILE '{named}' INTO TABLE t");
    let (port, server) = serve(move |peer| {
        peer.query();
        assert_eq!(peer.request(other.as_bytes()), b"");
    });
    let mut conn = connect(port, Some(&dir.0));
    let reason = rejected(conn.query(sql.as_bytes()));
    assert!(reason.contains("does not name"), "{reason}");
    server.join().unwrap();
}

#[test]
fn refuses_files_outside_the_directory() {
    let dir = TempDir::new();
    let outside = TempDir::new();
    let secret = outside.file("secret.csv", b"secret\n");
    let sql = format!("LOAD DATA LOCAL INFILE '{secret}' INTO TABLE t");
    let (port, server) = serve(move |peer| {
        let sql = peer.query();
        let name = sql.split('\'').nth(1).unwrap().to_owned();
        assert_eq!(peer.request(name.as_bytes()), b"");
    });
    let mut conn = connect(port, Some(&dir.0));
    let reason = rejected(conn.query(sql.as_bytes()));
    assert!(reason.contains("outside local_infile_dir"), "{reason}");
    server.join().unwrap();
}

#[cfg(unix)]
#[test]
fn refuses_symbolic_links_out_of_the_directory() {
    let dir = TempDir::new();
    let outside = TempDir::new();
    let secret = outside.file("secret.csv", b"secret\n");
    let link = dir.0.join("link.csv");
    std::os::unix::fs::symlink(&secret, &link).unwrap();
    // `..` out and back in counts as inside; out and elsewhere does not.
    let escape = format!(
        "{}/../{}",
        dir.0.display(),
        outside.0.file_name().unwrap().to_str().unwrap()
    );
    for name in [
        link.to_str().unwrap().to_owned(),
        format!("{escape}/secret.csv"),
    ] {
        let sql = format!("LOAD DATA LOCAL INFILE '{name}' INTO TABLE t");
        let (port, server) = serve(move |peer| {
            peer.query();
            assert_eq!(peer.request(name.as_bytes()), b"");
        });
        let mut conn = connect(port, Some(&dir.0));
        let reason = rejected(conn.query(sql.as_bytes()));
        assert!(reason.contains("outside local_infile_dir"), "{reason}");
        server.join().unwrap();
    }
}

#[test]
fn refuses_everything_when_disabled() {
    let dir = TempDir::new();
    let path = dir.file("data.csv", b"1\n");
    let sql = format!("LOAD DATA LOCAL INFILE '{path}' INTO TABLE t");
    let (port, server) = serve(move |peer| {
        assert_eq!(peer.client_capabilities & capability::LOCAL_FILES, 0);
        peer.query();
        assert_eq!(peer.request(path.as_bytes()), b"");
    });
    let mut conn = connect(port, None);
    let reason = rejected(conn.query(sql.as_bytes()));
    assert!(reason.contains("disabled"), "{reason}");
    server.join().unwrap();
}

#[test]
fn announces_local_files_only_when_allowed() {
    let dir = TempDir::new();
    let (port, server) = serve(|peer| {
        assert_ne!(peer.client_capabilities & capability::LOCAL_FILES, 0);
    });
    drop(connect(port, Some(&dir.0)));
    server.join().unwrap();
}

#[test]
fn a_request_inside_a_pipeline_closes_the_connection() {
    let dir = TempDir::new();
    let path = dir.file("data.csv", b"1\n");
    let sql = format!("LOAD DATA LOCAL INFILE '{path}' INTO TABLE t");
    let statements = [sql.clone(), "DO 2".to_owned()];
    let (port, server) = serve(move |peer| {
        peer.query();
        let mut request = vec![0xFB];
        request.extend_from_slice(path.as_bytes());
        peer.write(1, &request);
        // The second statement, then nothing: no file, no empty packet.
        assert_eq!(peer.query(), "DO 2");
        assert_eq!(peer.read(), None);
    });
    let mut conn = connect(port, Some(&dir.0));
    match conn.pipeline(statements.iter().map(String::as_bytes)) {
        Err(Error::Protocol(message)) => assert!(message.contains("pipelined"), "{message}"),
        other => panic!("expected a protocol error, got {other:?}"),
    }
    server.join().unwrap();
}

#[test]
fn a_pipeline_of_one_is_a_plain_query() {
    let dir = TempDir::new();
    let path = dir.file("data.csv", b"1\n2\n");
    let sql = format!("LOAD DATA LOCAL INFILE '{path}' INTO TABLE t");
    let (port, server) = serve(move |peer| {
        peer.query();
        assert_eq!(peer.request(path.as_bytes()), b"1\n2\n");
    });
    let mut conn = connect(port, Some(&dir.0));
    conn.pipeline([sql.as_bytes()]).unwrap();
    server.join().unwrap();
}

#[test]
fn the_directory_must_exist() {
    let dir = TempDir::new();
    let file = dir.file("not-a-dir", b"");
    for path in [dir.0.join("missing"), PathBuf::from(file)] {
        // Checked before connecting: nothing listens on port 1.
        let result = Conn::connect(&Opts {
            host: "127.0.0.1".to_owned(),
            port: 1,
            local_infile_dir: Some(path),
            ..Opts::default()
        });
        assert!(matches!(result, Err(Error::Config(_))), "{result:?}");
    }
}
