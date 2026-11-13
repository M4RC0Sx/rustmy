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
        user: user.to_owned(),
        password: "rustmy".to_owned(),
        database: Some("rustmy_test".to_owned()),
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

        let names: Vec<_> = result.columns.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["one", "txt", "nothing", "dbl"], "{server:?}");
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

#[test]
#[ignore = "needs `docker compose up -d --wait`"]
fn many_rows() {
    const DIGITS: &str = "(SELECT 0 d UNION ALL SELECT 1 UNION ALL SELECT 2 UNION ALL \
         SELECT 3 UNION ALL SELECT 4 UNION ALL SELECT 5 UNION ALL SELECT 6 UNION ALL \
         SELECT 7 UNION ALL SELECT 8 UNION ALL SELECT 9)";
    let sql = format!(
        "SELECT a.d * 1000 + b.d * 100 + c.d * 10 + e.d AS n, REPEAT('x', e.d) AS pad \
         FROM {DIGITS} a, {DIGITS} b, {DIGITS} c, {DIGITS} e ORDER BY n"
    );
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
        o.user = "rustmy_x509".to_owned();
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
    root.user = "root".to_owned();
    root.database = None;
    let mut admin = Conn::connect(&root).unwrap();
    admin
        .query(format!("CREATE USER '{user}'@'%' IDENTIFIED BY 'fresh-pw'").as_bytes())
        .unwrap();

    let mut o = tls_opts(Server::Mysql, SslMode::Required, None);
    o.user.clone_from(&user);
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
