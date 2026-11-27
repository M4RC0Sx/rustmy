//! The protocol core against the one released as 1.3.2: the same server
//! bytes must give the same results and errors, command by command.
//!
//! A fake server on a Unix socket sends the input (a recorded session, as
//! for the `session` target), then ends its side and drops whatever the
//! client writes. Each version runs the same session through its public
//! API, with and without compression, and logs the `Debug` output of every
//! result; the logs must match. Sessions that name an authentication plugin
//! 1.3.2 lacks are skipped.

#![no_main]

use std::io::Read;
use std::io::Write;
use std::net::Shutdown;
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::thread;
use std::time::Duration;

/// The listening socket and the local file the sessions may load.
struct Fixture {
    listener: UnixListener,
    socket: PathBuf,
    file: PathBuf,
}

fn fixture() -> &'static Fixture {
    static FIXTURE: OnceLock<Fixture> = OnceLock::new();
    FIXTURE.get_or_init(|| {
        let dir = std::fs::canonicalize(std::env::temp_dir()).unwrap();
        let socket = dir.join(format!("rustmy-fuzz-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&socket);
        let file = dir.join("rustmy-fuzz-local-infile.csv");
        std::fs::write(&file, b"1,a\n2,b\n").unwrap();
        Fixture {
            listener: UnixListener::bind(&socket).unwrap(),
            socket,
            file,
        }
    })
}

/// Serves `data` to the next client while `client` runs.
fn against_server<T>(data: &[u8], client: impl FnOnce() -> T) -> T {
    let data = data.to_vec();
    let server = thread::spawn(move || {
        let (mut stream, _) = fixture().listener.accept().unwrap();
        let _ = stream.write_all(&data);
        // End our side only: the client reads to the end and never sees a
        // reset, whatever it still writes.
        let _ = stream.shutdown(Shutdown::Write);
        let mut sink = [0u8; 16 * 1024];
        while matches!(stream.read(&mut sink), Ok(n) if n > 0) {}
    });
    let result = client();
    server.join().unwrap();
    result
}

/// The session of the `session` target, through one version's public API,
/// as a log of what every call returned.
macro_rules! session {
    ($proto:ident, $compress:expr) => {{
        use $proto as p;
        let fixture = fixture();
        let opts = p::Opts {
            unix_socket: Some(fixture.socket.clone()),
            user: b"fuzz".to_vec(),
            password: "secret".to_owned(),
            database: Some(b"db".to_vec()),
            client_flag: p::constants::capability::MULTI_STATEMENTS,
            connect_timeout: Some(Duration::from_secs(5)),
            tls: p::TlsOpts {
                mode: p::SslMode::Disabled,
                ..p::TlsOpts::default()
            },
            compress: $compress.then_some(p::Compression::Zstd { level: 3 }),
            local_infile_dir: fixture.file.parent().map(Path::to_path_buf),
            ..p::Opts::default()
        };
        let mut log: Vec<String> = Vec::new();
        let mut conn = match p::Conn::connect(&opts) {
            Ok(conn) => conn,
            Err(err) => {
                log.push(format!("connect: {err:?}"));
                return log;
            }
        };
        log.push(format!(
            "connected: {:?} {} {} {:?}",
            conn.server_version(),
            conn.connection_id(),
            // 1.3.2 sends no connection attributes, so never asks for them.
            conn.capabilities() & !p::constants::capability::CONNECT_ATTRS,
            conn.compression()
        ));
        let query = conn.query(b"SELECT 1");
        let ok = query.is_ok();
        log.push(format!("query: {query:?}"));
        if ok {
            loop {
                let next = conn.next_result();
                let more = matches!(next, Ok(Some(_)));
                log.push(format!("next_result: {next:?}"));
                if !more {
                    break;
                }
            }
        }
        match conn.query_streamed(b"SELECT 2") {
            Ok((mut head, Some(id))) => {
                log.push(format!("read_rows: {:?}", conn.read_rows(id, &mut head.rows, 1)));
                log.push(format!("skip_rows: {:?}", conn.skip_rows(id, 1)));
                head.rows.clear();
                log.push(format!("read_rows: {:?}", conn.read_rows(id, &mut head.rows, 64)));
                log.push(format!("streamed: {head:?}"));
            }
            other => log.push(format!("query_streamed: {other:?}")),
        }
        match conn.next_result_streamed() {
            Ok(Some((mut head, Some(id)))) => {
                let read = conn.read_rows(id, &mut head.rows, usize::MAX);
                log.push(format!("next_result_streamed: {read:?} {head:?}"));
            }
            other => log.push(format!("next_result_streamed: {other:?}")),
        }
        let prepared = conn.prepare(b"SELECT ?");
        log.push(format!("prepare: {prepared:?}"));
        if let Ok(statement) = prepared {
            let mut params = p::Execute::default();
            params.int(1);
            params.text(b"x");
            match conn.execute_streamed(&params.finish(statement.id)) {
                Ok((mut head, Some(id))) => {
                    let read = conn.read_rows(id, &mut head.rows, 64);
                    log.push(format!("execute_streamed: {read:?} {head:?}"));
                }
                other => log.push(format!("execute_streamed: {other:?}")),
            }
            log.push(format!("close_statement: {:?}", conn.close_statement(statement.id)));
        }
        let mut one = p::Execute::default();
        one.null();
        log.push(format!("execute_cached: {:?}", conn.execute_cached(b"SELECT ?", &one)));
        log.push(format!(
            "execute_many_cached: {:?}",
            conn.execute_many_cached(b"DO ?", &[one])
        ));
        log.push(format!(
            "pipeline: {:?}",
            conn.pipeline([&b"DO 1"[..], b"DO 2", b"DO 3"])
        ));
        match conn.pipeline_streamed([&b"DO 4"[..], b"DO 5"], b"SELECT 6") {
            Ok((totals, mut head, Some(id))) => {
                let read = conn.read_rows(id, &mut head.rows, 1);
                log.push(format!("pipeline_streamed: {totals:?} {read:?} {head:?}"));
            }
            other => log.push(format!("pipeline_streamed: {other:?}")),
        }
        log.push(format!("ping: {:?}", conn.ping()));
        log.push(format!("select_db: {:?}", conn.select_db(b"db")));
        log.push(format!("reset_connection: {:?}", conn.reset_connection()));
        log.push(format!(
            "change_user: {:?}",
            conn.change_user(b"other", "pw", Some(b"db"))
        ));
        log.push(format!("query: {:?}", conn.query(b"SELECT 2")));
        if let Some(path) = fixture.file.to_str() {
            let load = format!("LOAD DATA LOCAL INFILE '{path}' INTO TABLE t");
            log.push(format!("load: {:?}", conn.query(load.as_bytes())));
            log.push(format!(
                "load pipelined: {:?}",
                conn.pipeline([load.as_bytes(), b"DO 7"])
            ));
        }
        log.push(format!(
            "end: {} {} {}",
            conn.status_flags(),
            conn.has_more_results(),
            conn.is_tls()
        ));
        conn.close();
        log
    }};
}

fn current(data: &[u8], compress: bool) -> Vec<String> {
    against_server(data, || (|| session!(rustmy_proto, compress))())
}

fn released(data: &[u8], compress: bool) -> Vec<String> {
    against_server(data, || (|| session!(rustmy_proto_1_3_2, compress))())
}

libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    // 1.3.2 has no mysql_clear_password: over this Unix socket the current
    // version authenticates with it where 1.3.2 fails.
    if data.windows(20).any(|w| w == b"mysql_clear_password") {
        return;
    }
    for compress in [false, true] {
        let now = current(data, compress);
        let then = released(data, compress);
        assert_eq!(now, then, "the session differs from 1.3.2 (compress: {compress})");
    }
});
