//! Integration tests against real servers started with `docker compose up -d --wait`.
//!
//! Ignored by default; run with `cargo nextest run --run-ignored only`.
//! Override endpoints with `RUSTMY_TEST_MYSQL` / `RUSTMY_TEST_MARIADB`
//! (`host:port`) and restrict servers with `RUSTMY_TEST_SERVERS`
//! (`mysql`, `mariadb` or `mysql,mariadb`, the default).

#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers outside #[test] functions must fail loudly too"
)]

use rustmy_proto::Conn;
use rustmy_proto::Error;
use rustmy_proto::Opts;
use rustmy_proto::QueryResult;
use rustmy_proto::ReadOutcome;
use rustmy_proto::RowSet;
use rustmy_proto::SslMode;
use rustmy_proto::TlsOpts;
use rustmy_proto::constants::ColumnType;
use rustmy_proto::constants::capability;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Server {
    Mysql,
    Mariadb,
}

/// Servers under test, from `RUSTMY_TEST_SERVERS`.
fn servers() -> Vec<Server> {
    let list = std::env::var("RUSTMY_TEST_SERVERS").unwrap_or_else(|_| "mysql,mariadb".to_owned());
    list.split(',')
        .map(|name| match name.trim() {
            "mysql" => Server::Mysql,
            "mariadb" => Server::Mariadb,
            other => panic!("unknown server {other:?} in RUSTMY_TEST_SERVERS"),
        })
        .collect()
}

fn server_major_version(server: Server) -> u32 {
    let version = connect(server).server_version().to_owned();
    version.split('.').next().unwrap().parse().unwrap()
}

/// An account whose failed logins are reported as errors over plain TCP.
///
/// MySQL's `rustmy` uses `caching_sha2_password`, which works over plain TCP
/// only once the server has cached it (the compose healthcheck does that).
/// Failed logins never hit the cache and need full auth (TLS or RSA, Phase
/// 1), so error-path tests need a `mysql_native_password` account, which
/// MySQL 9+ no longer has: those cases are skipped there until Phase 1.
fn native_user(server: Server) -> Option<&'static str> {
    match server {
        Server::Mariadb => Some("rustmy"),
        Server::Mysql => {
            let major = server_major_version(server);
            (major < 9).then_some("rustmy_native")
        }
    }
}

fn opts(server: Server, user: &str) -> Opts {
    let (var, default) = match server {
        Server::Mysql => ("RUSTMY_TEST_MYSQL", "127.0.0.1:13306"),
        Server::Mariadb => ("RUSTMY_TEST_MARIADB", "127.0.0.1:13307"),
    };
    let endpoint = std::env::var(var).unwrap_or_else(|_| default.to_owned());
    let (host, port) = endpoint.rsplit_once(':').expect("host:port");
    Opts {
        host: host.to_owned(),
        port: port.parse().expect("numeric port"),
        user: user.into(),
        password: "rustmy".to_owned(),
        database: Some(b"rustmy_test".to_vec()),
        ..Opts::default()
    }
}

fn connect(server: Server) -> Conn {
    Conn::connect(&opts(server, "rustmy")).unwrap_or_else(|e| panic!("{server:?}: {e}"))
}

/// Cells of every row as UTF-8 strings, with SQL NULL rendered as "NULL".
fn table(result: &QueryResult) -> Vec<Vec<String>> {
    result
        .rows
        .rows()
        .map(|row| {
            row.cells()
                .map(|c| {
                    c.map_or_else(
                        || "NULL".to_owned(),
                        |b| String::from_utf8(b.to_vec()).unwrap(),
                    )
                })
                .collect()
        })
        .collect()
}

#[test]
#[ignore = "needs `docker compose up -d --wait`"]
fn selects_scalars_with_column_metadata() {
    for server in servers() {
        let mut conn = connect(server);
        let result = conn
            .query(b"SELECT 1 AS one, 'h\xc3\xa9llo' AS txt, NULL AS nothing, 1.5e0 AS dbl")
            .unwrap();
        assert_eq!(table(&result), [["1", "héllo", "NULL", "1.5"]]);

        let names: Vec<_> = result.columns.iter().map(|c| c.name.as_slice()).collect();
        assert_eq!(
            names,
            [&b"one"[..], b"txt", b"nothing", b"dbl"],
            "{server:?}"
        );
        // MySQL types integer literals as LONG, MariaDB as LONGLONG.
        let int_type = result.columns[0].column_type;
        assert!(
            [ColumnType::LONG, ColumnType::LONGLONG].contains(&int_type),
            "{server:?}: {int_type:?}"
        );
        assert_eq!(result.columns[3].column_type, ColumnType::DOUBLE);
        assert!(!result.columns[1].is_binary());
    }
}

#[test]
#[ignore = "needs `docker compose up -d --wait`"]
fn selects_default_database() {
    for server in servers() {
        let mut conn = connect(server);
        let result = conn.query(b"SELECT DATABASE()").unwrap();
        assert_eq!(table(&result), [["rustmy_test"]], "{server:?}");
    }
}

#[test]
#[ignore = "needs `docker compose up -d --wait`"]
fn native_password_auth_including_auth_switch() {
    // MySQL greets with caching_sha2_password and switches to native for
    // this account; MariaDB uses native from the start.
    for server in servers() {
        let Some(user) = native_user(server) else {
            eprintln!("{server:?}: no mysql_native_password on this version, skipped");
            continue;
        };
        let mut conn = Conn::connect(&opts(server, user)).unwrap();
        let result = conn.query(b"SELECT CURRENT_USER()").unwrap();
        let who = &table(&result)[0][0];
        assert!(who.starts_with(user), "{server:?}: {who}");
    }
}

#[test]
#[ignore = "needs `docker compose up -d --wait`"]
fn wrong_password_is_access_denied() {
    // Over TLS a failed caching_sha2_password login gets a real answer.
    for server in servers() {
        let mut o = opts(server, "rustmy");
        o.password = "wrong".to_owned();
        o.tls.mode = SslMode::Required;
        let err = Conn::connect(&o).unwrap_err();
        assert!(
            matches!(err, Error::Server { code: 1045, .. }),
            "{server:?}: {err}"
        );
    }
}

#[test]
#[ignore = "needs `docker compose up -d --wait`"]
fn server_error_leaves_connection_usable() {
    for server in servers() {
        let mut conn = connect(server);
        let err = conn.query(b"SELECT * FROM no_such_table").unwrap_err();
        let Error::Server {
            code, sql_state, ..
        } = err
        else {
            panic!("{server:?}: expected server error, got {err}");
        };
        assert_eq!((code, sql_state.as_str()), (1146, "42S02"));
        conn.ping().unwrap();
        assert_eq!(table(&conn.query(b"SELECT 2").unwrap()), [["2"]]);
    }
}

const DIGITS: &str = "(SELECT 0 d UNION ALL SELECT 1 UNION ALL SELECT 2 UNION ALL \
     SELECT 3 UNION ALL SELECT 4 UNION ALL SELECT 5 UNION ALL SELECT 6 UNION ALL \
     SELECT 7 UNION ALL SELECT 8 UNION ALL SELECT 9)";

/// 10,000 rows: `n` from 0 to 9999 in order and a short padding column.
fn many_rows_sql() -> String {
    format!(
        "SELECT a.d * 1000 + b.d * 100 + c.d * 10 + e.d AS n, REPEAT('x', e.d) AS pad \
         FROM {DIGITS} a, {DIGITS} b, {DIGITS} c, {DIGITS} e ORDER BY n"
    )
}

/// First cell of every row, as UTF-8.
fn first_cells(rows: &RowSet) -> Vec<String> {
    rows.rows()
        .map(|row| String::from_utf8(row.cells().next().unwrap().unwrap().to_vec()).unwrap())
        .collect()
}

#[test]
#[ignore = "needs `docker compose up -d --wait`"]
fn many_rows() {
    let sql = many_rows_sql();
    for server in servers() {
        let mut conn = connect(server);
        let result = conn.query(sql.as_bytes()).unwrap();
        let rows = table(&result);
        assert_eq!(rows.len(), 10_000, "{server:?}");
        assert_eq!(rows[9_999], ["9999", "xxxxxxxxx"]);
    }
}

#[test]
#[ignore = "needs `docker compose up -d --wait`"]
fn payloads_larger_than_one_packet_both_ways() {
    let size = 20 * 1024 * 1024;
    for server in servers() {
        let mut conn = connect(server);
        // Server -> client: one 20 MiB cell spans two packets.
        let result = conn
            .query(format!("SELECT REPEAT('x', {size})").as_bytes())
            .unwrap();
        let cell = result.rows.rows().next().unwrap().cells().next().unwrap();
        assert_eq!(cell.map(<[u8]>::len), Some(size), "{server:?}");

        // Client -> server: a 20 MiB query must be split by the client.
        let sql = format!("SELECT LENGTH('{}')", "y".repeat(size));
        let result = conn.query(sql.as_bytes()).unwrap();
        assert_eq!(table(&result), [[size.to_string()]]);
    }
}

#[test]
#[ignore = "needs `docker compose up -d --wait`"]
fn insert_reports_affected_rows_and_last_insert_id() {
    for server in servers() {
        let mut conn = connect(server);
        conn.query(b"CREATE TEMPORARY TABLE t (id INT AUTO_INCREMENT PRIMARY KEY, v INT)")
            .unwrap();
        let result = conn.query(b"INSERT INTO t (v) VALUES (10), (20)").unwrap();
        assert_eq!(result.columns.len(), 0);
        assert_eq!(result.affected_rows, 2, "{server:?}");
        assert_eq!(result.last_insert_id, 1);
    }
}

#[test]
#[ignore = "needs `docker compose up -d --wait`"]
fn multiple_result_sets() {
    for server in servers() {
        let mut o = opts(server, "rustmy");
        o.client_flag = capability::MULTI_STATEMENTS;
        let mut conn = Conn::connect(&o).unwrap();

        let first = conn.query(b"SELECT 1; SELECT 2; SELECT 3").unwrap();
        assert_eq!(table(&first), [["1"]]);
        assert!(conn.has_more_results());
        let second = conn.next_result().unwrap().unwrap();
        assert_eq!(table(&second), [["2"]]);

        // Unread results are drained before the next command.
        assert!(conn.has_more_results());
        assert_eq!(table(&conn.query(b"SELECT 4").unwrap()), [["4"]]);
        assert!(conn.next_result().unwrap().is_none());
    }
}

#[test]
#[ignore = "needs `docker compose up -d --wait`"]
fn close_sends_quit() {
    for server in servers() {
        let mut admin = connect(server);
        let conn = connect(server);
        let id = conn.connection_id();
        conn.close();
        // The server drops the session shortly after COM_QUIT.
        let sql = format!("SELECT COUNT(*) FROM information_schema.PROCESSLIST WHERE ID = {id}");
        let gone = (0..50).any(|_| {
            if table(&admin.query(sql.as_bytes()).unwrap()) == [["0"]] {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
            false
        });
        assert!(gone, "{server:?}: connection {id} still open");
    }
}

/// Throwaway certificates generated by the `certs` compose service.
fn cert(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/docker/certs")
        .join(name)
}

fn tls_opts(server: Server, mode: SslMode, ca: Option<&str>) -> Opts {
    let mut o = opts(server, "rustmy");
    o.tls = TlsOpts {
        mode,
        ca: ca.map(cert),
        ..TlsOpts::default()
    };
    o
}

fn ssl_cipher(conn: &mut Conn) -> String {
    let result = conn
        .query(b"SHOW SESSION STATUS LIKE 'Ssl_cipher'")
        .unwrap();
    table(&result)[0][1].clone()
}

#[test]
#[ignore = "needs `docker compose up -d --wait`"]
fn tls_modes_encrypt_or_not() {
    for server in servers() {
        for (mode, encrypted) in [
            (SslMode::Disabled, false),
            (SslMode::Preferred, true),
            (SslMode::Required, true),
        ] {
            let mut conn = Conn::connect(&tls_opts(server, mode, None)).unwrap();
            assert_eq!(conn.is_tls(), encrypted, "{server:?} {mode:?}");
            // The server agrees: a cipher is in use exactly when encrypted.
            assert_eq!(!ssl_cipher(&mut conn).is_empty(), encrypted);
        }
    }
}

#[test]
#[ignore = "needs `docker compose up -d --wait`"]
fn verification_against_the_test_ca() {
    for server in servers() {
        for mode in [SslMode::VerifyCa, SslMode::VerifyIdentity] {
            for host in ["127.0.0.1", "localhost"] {
                let mut o = tls_opts(server, mode, Some("ca.pem"));
                o.host = host.to_owned();
                let conn = Conn::connect(&o).unwrap_or_else(|e| panic!("{server:?} {host}: {e}"));
                assert!(conn.is_tls());
            }
        }
        let err =
            Conn::connect(&tls_opts(server, SslMode::VerifyCa, Some("other-ca.pem"))).unwrap_err();
        assert!(matches!(err, Error::Tls(_)), "{server:?}: {err}");
        // The test CA is not in the operating system's store.
        let err = Conn::connect(&tls_opts(server, SslMode::VerifyIdentity, None)).unwrap_err();
        assert!(matches!(err, Error::Tls(_)), "{server:?}: {err}");
    }
}

#[test]
#[ignore = "needs `docker compose up -d --wait`"]
fn client_certificate_for_require_x509_account() {
    for server in servers() {
        let mut o = tls_opts(server, SslMode::VerifyIdentity, Some("ca.pem"));
        o.user = b"rustmy_x509".to_vec();
        o.tls.cert = Some(cert("client.pem"));
        o.tls.key = Some(cert("client-key.pem"));
        let mut conn = Conn::connect(&o).unwrap_or_else(|e| panic!("{server:?}: {e}"));
        assert_ne!(ssl_cipher(&mut conn), "");

        // Same account without the certificate is refused by the server.
        o.tls.cert = None;
        o.tls.key = None;
        let err = Conn::connect(&o).unwrap_err();
        assert!(
            matches!(err, Error::Server { code: 1045, .. }),
            "{server:?}: {err}"
        );
    }
}

#[test]
#[ignore = "needs `docker compose up -d --wait`"]
fn caching_sha2_full_authentication_over_tls() {
    if !servers().contains(&Server::Mysql) {
        return;
    }
    // A brand-new account cannot be in the server's auth cache.
    let user = format!("fresh_{}", std::process::id());
    let mut root = tls_opts(Server::Mysql, SslMode::Required, None);
    root.user = b"root".to_vec();
    root.database = None;
    let mut admin = Conn::connect(&root).unwrap();
    admin
        .query(format!("CREATE USER '{user}'@'%' IDENTIFIED BY 'fresh-pw'").as_bytes())
        .unwrap();

    let mut o = tls_opts(Server::Mysql, SslMode::Required, None);
    o.user = user.clone().into_bytes();
    o.password = "fresh-pw".to_owned();
    o.database = None;
    let mut conn = Conn::connect(&o).unwrap();
    assert_eq!(table(&conn.query(b"SELECT 1").unwrap()), [["1"]]);

    admin
        .query(format!("DROP USER '{user}'@'%'").as_bytes())
        .unwrap();
}

#[test]
#[ignore = "needs `docker compose up -d --wait`"]
fn large_payloads_over_tls() {
    let size = 20 * 1024 * 1024;
    for server in servers() {
        let mut conn = Conn::connect(&tls_opts(server, SslMode::Required, None)).unwrap();
        assert!(conn.is_tls());
        let sql = format!("SELECT LENGTH('{}'), REPEAT('x', {size})", "y".repeat(size));
        let result = conn.query(sql.as_bytes()).unwrap();
        let row = result.rows.rows().next().unwrap();
        let cells: Vec<_> = row.cells().collect();
        assert_eq!(cells[0], Some(size.to_string().as_bytes()), "{server:?}");
        assert_eq!(cells[1].map(<[u8]>::len), Some(size));
    }
}

/// Creates MySQL accounts that cannot be in the server's auth cache, so
/// every login takes the full-authentication path; dropped on drop.
struct FreshUsers {
    admin: Conn,
    created: Vec<String>,
}

impl FreshUsers {
    fn new() -> Self {
        let mut root = tls_opts(Server::Mysql, SslMode::Required, None);
        root.user = b"root".to_vec();
        root.database = None;
        Self {
            admin: Conn::connect(&root).unwrap(),
            created: Vec::new(),
        }
    }

    /// `None` when the server lacks the plugin (e.g. `sha256_password` on 9+).
    fn create(&mut self, plugin: &str) -> Option<String> {
        let name = format!("fresh_{}_{}", std::process::id(), self.created.len());
        let sql = format!("CREATE USER '{name}'@'%' IDENTIFIED WITH {plugin} BY 'fresh-pw'");
        match self.admin.query(sql.as_bytes()) {
            Ok(_) => {
                self.created.push(name.clone());
                Some(name)
            }
            Err(Error::Server { code: 1524, .. }) => None, // plugin not loaded
            Err(err) => panic!("{err}"),
        }
    }

    fn server_public_key(&mut self) -> std::path::PathBuf {
        let result = self
            .admin
            .query(b"SHOW STATUS LIKE 'Caching_sha2_password_rsa_public_key'")
            .unwrap();
        let path =
            std::env::temp_dir().join(format!("rustmy-server-key-{}.pem", std::process::id()));
        std::fs::write(&path, &table(&result)[0][1]).unwrap();
        path
    }
}

impl Drop for FreshUsers {
    fn drop(&mut self) {
        for name in &self.created {
            let _ = self
                .admin
                .query(format!("DROP USER '{name}'@'%'").as_bytes());
        }
    }
}

fn plain_login(
    user: &str,
    password: &str,
    key: Option<&std::path::Path>,
) -> rustmy_proto::Result<Conn> {
    let mut o = tls_opts(Server::Mysql, SslMode::Disabled, None);
    o.user = user.into();
    password.clone_into(&mut o.password);
    o.database = None;
    o.server_public_key = key.map(std::path::Path::to_path_buf);
    Conn::connect(&o)
}

#[test]
#[ignore = "needs `docker compose up -d --wait`"]
fn rsa_full_authentication_over_plain_tcp() {
    if !servers().contains(&Server::Mysql) {
        return; // caching_sha2_password and sha256_password are MySQL-only
    }
    let mut users = FreshUsers::new();
    let key = users.server_public_key();
    for plugin in ["caching_sha2_password", "sha256_password"] {
        let Some(requested) = users.create(plugin) else {
            eprintln!("{plugin} not available on this server, skipped");
            continue;
        };
        // Key requested from the server.
        let conn =
            plain_login(&requested, "fresh-pw", None).unwrap_or_else(|e| panic!("{plugin}: {e}"));
        assert!(!conn.is_tls());
        // Pinned key: no request.
        let pinned = users.create(plugin).unwrap();
        plain_login(&pinned, "fresh-pw", Some(&key)).unwrap_or_else(|e| panic!("{plugin}: {e}"));
        // Wrong password: a proper "access denied".
        let wrong = users.create(plugin).unwrap();
        let err = plain_login(&wrong, "nope", None).unwrap_err();
        assert!(
            matches!(err, Error::Server { code: 1045, .. }),
            "{plugin}: {err}"
        );
    }
}

#[test]
#[ignore = "needs `docker compose up -d --wait`"]
fn pinning_the_wrong_public_key_is_refused() {
    use rsa::pkcs8::EncodePublicKey;
    use rsa::pkcs8::LineEnding;

    if !servers().contains(&Server::Mysql) {
        return;
    }
    let mut users = FreshUsers::new();
    let user = users.create("caching_sha2_password").unwrap();
    let stranger = rsa::PrivateKey::new(&mut getrandom::SysRng, 2048).unwrap();
    let path = std::env::temp_dir().join(format!("rustmy-wrong-key-{}.pem", std::process::id()));
    std::fs::write(
        &path,
        stranger
            .to_public_key()
            .to_public_key_pem(LineEnding::LF)
            .unwrap(),
    )
    .unwrap();
    // The server cannot decrypt what was encrypted for another key.
    let err = plain_login(&user, "fresh-pw", Some(&path)).unwrap_err();
    assert!(matches!(err, Error::Server { code: 1045, .. }), "{err}");

    let err = plain_login(
        &user,
        "fresh-pw",
        Some(std::path::Path::new("/nonexistent.pem")),
    )
    .unwrap_err();
    assert!(matches!(err, Error::Config(_)), "{err}");
}

/// `(major, minor)` of a MariaDB server, e.g. `(11, 8)`.
fn mariadb_version() -> (u32, u32) {
    let conn = connect(Server::Mariadb);
    let mut parts = conn.server_version().split(['.', '-']);
    let major = parts.next().unwrap().parse().unwrap();
    let minor = parts.next().unwrap().parse().unwrap();
    (major, minor)
}

#[test]
#[ignore = "needs `docker compose up -d --wait`"]
fn mariadb_ed25519_and_parsec_authentication() {
    if !servers().contains(&Server::Mariadb) {
        return; // MariaDB-only plugins
    }
    let mut accounts = vec!["rustmy_ed25519"];
    if mariadb_version() >= (11, 6) {
        accounts.push("rustmy_parsec");
    } else {
        eprintln!("parsec needs MariaDB 11.6+, skipped");
    }
    for user in accounts {
        for mode in [SslMode::Disabled, SslMode::Required] {
            let mut o = tls_opts(Server::Mariadb, mode, None);
            o.user = user.as_bytes().to_vec();
            let mut conn = Conn::connect(&o).unwrap_or_else(|e| panic!("{user} {mode:?}: {e}"));
            let who = &table(&conn.query(b"SELECT CURRENT_USER()").unwrap())[0][0];
            assert!(who.starts_with(user), "{who}");

            "wrong".clone_into(&mut o.password);
            let err = Conn::connect(&o).unwrap_err();
            assert!(
                matches!(err, Error::Server { code: 1045, .. }),
                "{user} {mode:?}: {err}"
            );
        }
    }
}

#[test]
#[ignore = "needs `docker compose up -d --wait`"]
fn select_db_reset_connection_and_change_user() {
    for server in servers() {
        let mut conn = connect(server);
        conn.select_db(b"information_schema").unwrap();
        assert_eq!(
            table(&conn.query(b"SELECT DATABASE()").unwrap()),
            [["information_schema"]]
        );
        let err = conn.select_db(b"no_such_db").unwrap_err();
        assert!(matches!(err, Error::Server { .. }), "{server:?}: {err}");

        let id = conn.connection_id();
        conn.query(b"SET @x = 1").unwrap();
        conn.reset_connection().unwrap();
        assert_eq!(table(&conn.query(b"SELECT @x").unwrap()), [["NULL"]]);
        assert_eq!(
            table(&conn.query(b"SELECT CONNECTION_ID()").unwrap()),
            [[id.to_string()]]
        );

        conn.change_user(b"root", "rustmy", Some(b"rustmy_test"))
            .unwrap();
        let who = &table(&conn.query(b"SELECT CURRENT_USER()").unwrap())[0][0];
        assert!(who.starts_with("root@"), "{server:?}: {who}");

        let err = conn.change_user(b"root", "wrong", None).unwrap_err();
        assert!(
            matches!(err, Error::Server { code: 1045, .. }),
            "{server:?}: {err}"
        );
    }
}

#[test]
#[ignore = "needs `docker compose up -d --wait`"]
fn commands_drain_unread_results_first() {
    for server in servers() {
        let mut o = opts(server, "rustmy");
        o.client_flag = capability::MULTI_STATEMENTS;
        let mut conn = Conn::connect(&o).unwrap();
        conn.query(b"SELECT 1; SELECT 2").unwrap();
        assert!(conn.has_more_results());
        conn.ping().unwrap();
        conn.query(b"SELECT 1; SELECT 2").unwrap();
        conn.reset_connection().unwrap();
        assert_eq!(table(&conn.query(b"SELECT 3").unwrap()), [["3"]]);
    }
}

#[test]
#[ignore = "needs `docker compose up -d --wait`"]
fn read_timeout_interrupts_a_slow_query() {
    for server in servers() {
        let mut o = opts(server, "rustmy");
        o.read_timeout = Some(std::time::Duration::from_millis(300));
        let mut conn = Conn::connect(&o).unwrap();
        let start = std::time::Instant::now();
        let err = conn.query(b"SELECT SLEEP(3)").unwrap_err();
        assert!(matches!(err, Error::Io(_)), "{server:?}: {err}");
        assert!(start.elapsed() < std::time::Duration::from_secs(2));
    }
}

#[test]
#[ignore = "needs `docker compose up -d --wait`"]
fn streamed_rows_arrive_in_batches() {
    let sql = many_rows_sql();
    for server in servers() {
        let mut conn = connect(server);
        let (head, id) = conn.query_streamed(sql.as_bytes()).unwrap();
        let id = id.expect("a result set");
        assert_eq!(head.columns.len(), 2);
        assert!(head.rows.is_empty());

        let mut rows = head.rows;
        let mut seen = Vec::new();
        let mut batches = 0;
        loop {
            rows.clear();
            let outcome = conn.read_rows(id, &mut rows, 4096).unwrap();
            batches += 1;
            seen.extend(first_cells(&rows));
            if matches!(outcome, ReadOutcome::Done { .. }) {
                break;
            }
            assert!(!rows.is_empty(), "{server:?}: a batch holds a row at least");
        }
        let expected: Vec<String> = (0..10_000).map(|n| n.to_string()).collect();
        assert_eq!(seen, expected, "{server:?}");
        assert!(batches > 10, "{server:?}: {batches} batches");

        // Finished: the id no longer reads anything.
        assert!(matches!(
            conn.read_rows(id, &mut rows, 1),
            Err(Error::ResultDiscarded)
        ));
        assert_eq!(table(&conn.query(b"SELECT 2").unwrap()), [["2"]]);
    }
}

#[test]
#[ignore = "needs `docker compose up -d --wait`"]
fn the_next_command_discards_unread_streamed_rows() {
    let sql = many_rows_sql();
    for server in servers() {
        let mut conn = connect(server);
        let (head, id) = conn.query_streamed(sql.as_bytes()).unwrap();
        let id = id.unwrap();
        assert_eq!(conn.skip_rows(id, 10).unwrap(), (10, ReadOutcome::More));
        let mut rows = head.rows;
        conn.read_rows(id, &mut rows, 1).unwrap();
        assert_eq!(first_cells(&rows), ["10"], "{server:?}");

        assert_eq!(table(&conn.query(b"SELECT 2").unwrap()), [["2"]]);
        assert!(matches!(
            conn.read_rows(id, &mut rows, 1),
            Err(Error::ResultDiscarded)
        ));
        assert!(matches!(conn.skip_rows(id, 1), Err(Error::ResultDiscarded)));

        // Skipping past the end stops there.
        let (_, id) = conn.query_streamed(b"SELECT 1 UNION ALL SELECT 2").unwrap();
        let (skipped, outcome) = conn.skip_rows(id.unwrap(), 5).unwrap();
        assert_eq!(skipped, 2);
        assert!(matches!(outcome, ReadOutcome::Done { .. }));
        conn.ping().unwrap();
    }
}

#[test]
#[ignore = "needs `docker compose up -d --wait`"]
fn streamed_multiple_results() {
    for server in servers() {
        let mut o = opts(server, "rustmy");
        o.client_flag = capability::MULTI_STATEMENTS;
        let mut conn = Conn::connect(&o).unwrap();

        let (_, first) = conn
            .query_streamed(b"SELECT 1 UNION ALL SELECT 2; DO 0; SELECT 3")
            .unwrap();
        assert!(first.is_some());
        // Moving on discards the rows left in the first result.
        let (ok, none) = conn.next_result_streamed().unwrap().unwrap();
        assert!(none.is_none() && ok.columns.is_empty(), "{server:?}");
        let (mut third, id) = conn.next_result_streamed().unwrap().unwrap();
        let outcome = conn
            .read_rows(id.unwrap(), &mut third.rows, usize::MAX)
            .unwrap();
        assert!(matches!(outcome, ReadOutcome::Done { .. }));
        assert_eq!(first_cells(&third.rows), ["3"]);
        assert!(conn.next_result_streamed().unwrap().is_none());

        // Unread rows and unread results are both drained by a command.
        conn.query_streamed(b"SELECT 1 UNION ALL SELECT 2; SELECT 3")
            .unwrap();
        conn.ping().unwrap();
        assert!(!conn.has_more_results());
        assert_eq!(table(&conn.query(b"SELECT 4").unwrap()), [["4"]]);
    }
}

#[test]
#[ignore = "needs `docker compose up -d --wait`"]
fn server_error_mid_stream_ends_the_result_set() {
    // The scalar subquery fails at run time, at the row with n = 5000,
    // after the rows before it were sent (the ordered derived table is
    // materialised, so they come first).
    let sql = format!(
        "SELECT n, IF(n < 5000, 1, (SELECT 1 UNION ALL SELECT 2)) AS x \
         FROM ({} LIMIT 10000) AS numbers",
        many_rows_sql()
    );
    for server in servers() {
        let mut conn = connect(server);
        let (mut head, id) = conn.query_streamed(sql.as_bytes()).unwrap();
        let id = id.unwrap();
        let err = conn.read_rows(id, &mut head.rows, usize::MAX).unwrap_err();
        assert!(
            matches!(err, Error::Server { code: 1242, .. }),
            "{server:?}: {err}"
        );
        assert_eq!(head.rows.len(), 5000, "{server:?}: rows before the error");
        assert!(matches!(
            conn.read_rows(id, &mut head.rows, 1),
            Err(Error::ResultDiscarded)
        ));

        // Discarding such a result swallows its error: nobody reads it.
        conn.query_streamed(sql.as_bytes()).unwrap();
        assert_eq!(table(&conn.query(b"SELECT 2").unwrap()), [["2"]]);
    }
}

#[test]
#[ignore = "needs `docker compose up -d --wait`"]
fn pipelined_statements_run_in_order_past_an_error() {
    for server in servers() {
        let mut conn = connect(server);
        conn.query(b"CREATE TEMPORARY TABLE p (id INT PRIMARY KEY, v INT)")
            .unwrap();
        conn.query(b"INSERT INTO p VALUES (1, 0), (2, 0), (3, 0)")
            .unwrap();
        let statements: [&[u8]; 6] = [
            b"UPDATE p SET v = 1 WHERE id = 1",
            b"UPDATE p SET v = WHERE",
            b"SELECT id FROM p ORDER BY id",
            b"UPDATE p SET v = v + 1",
            b"SELECT * FROM missing_table",
            b"DELETE FROM p WHERE id = 3",
        ];
        let totals = conn.pipeline(statements).unwrap();
        let (index, error) = totals.error.unwrap();
        assert_eq!(index, 1, "{server:?}");
        assert!(
            matches!(error, Error::Server { code: 1064, .. }),
            "{error:?}"
        );
        // 1 + 3 rows of the SELECT + 3 + 1.
        assert_eq!(totals.rowcount, 8, "{server:?}");
        let rows = conn.query(b"SELECT id, v FROM p ORDER BY id").unwrap();
        assert_eq!(table(&rows), [["1", "2"], ["2", "1"]]);

        // The last reply is left as query_streamed leaves it.
        let (totals, head, id) = conn
            .pipeline_streamed([&b"UPDATE p SET v = 0"[..]], b"SELECT id FROM p")
            .unwrap();
        assert_eq!(totals.rowcount, 2);
        let mut rows = head.rows;
        conn.read_rows(id.unwrap(), &mut rows, usize::MAX).unwrap();
        assert_eq!(first_cells(&rows), ["1", "2"]);

        // An error before the last statement is reported once every reply
        // has been read.
        let err = conn
            .pipeline_streamed([&b"SELECT * FROM missing_table"[..]], b"SELECT 1")
            .unwrap_err();
        assert!(matches!(err, Error::Server { code: 1146, .. }), "{err:?}");
        assert_eq!(table(&conn.query(b"SELECT 2").unwrap()), [["2"]]);
    }
}
