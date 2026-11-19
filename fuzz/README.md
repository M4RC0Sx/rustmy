# Fuzzing

Coverage-guided fuzzing of the protocol core with `cargo-fuzz` (libFuzzer).
The property under test: no byte sequence a server can send makes the client
panic, hang or allocate without bound.

| Target | What it feeds untrusted bytes to |
|---|---|
| `handshake` | the server greeting parser |
| `packets` | packet framing (lengths, sequence ids, >16 MiB payloads) |
| `compressed` | the compressed protocol layer (zlib or zstd, picked by the first byte): compressed headers and sequence ids, payloads that do not inflate to their declared length, packets split across compressed ones |
| `result_set` | column definitions, rows, OK / ERR / EOF packets |
| `text_values` | text-protocol integer, float, date and time parsers |
| `sql` | the `INSERT … VALUES` recogniser used by `executemany` and string escaping |
| `session` | the whole client against a scripted server: handshake, every authentication plugin (RSA, ed25519, parsec, auth switches), queries, multiple results, prepared statements, pipelined batches, `ping`, `select_db`, `reset_connection`, `change_user`; each input runs twice, uncompressed and asking for compression (the replies after authentication are then read as compressed packets) |

The hooks live behind the `fuzzing` feature of `rustmy-proto`
(`crates/proto/src/fuzzing.rs`), which normal builds never enable.

## Running

Needs the nightly toolchain and `cargo-fuzz`
(`rustup toolchain install nightly`, `cargo install --locked cargo-fuzz`):

```sh
cargo +nightly fuzz run session fuzz/corpus/session fuzz/seeds/session -- \
    -max_total_time=300 -timeout=10 -rss_limit_mb=2048
```

The first directory is the working corpus (git-ignored, grows as the fuzzer
finds new paths); `fuzz/seeds/<target>` holds committed starting points. A
failure is written to `fuzz/artifacts/<target>/`; reproduce it with
`cargo +nightly fuzz run <target> <file>`, then turn it into a regular unit
test before fixing.

CI runs every target for 10 minutes each week (`.github/workflows/fuzz.yml`),
keeping the corpus between runs.

## Seeds

`seeds/session/*.bin` are real server-to-client transcripts recorded through a
TCP proxy while rustmy ran the same command sequence as the `session` target,
one per authentication path: MySQL `caching_sha2_password` with RSA, MySQL
auth switch to `mysql_native_password`, MariaDB native, `client_ed25519` and
`parsec`. `seeds/handshake/*.bin` are the greeting packets taken from them.
`seeds/session/local-infile.bin` is hand-built: a greeting, an OK for every
reply the session reads before its `LOAD DATA LOCAL INFILE`, then a request
for `/etc/passwd` (refused: not the statement's file) and one in the middle
of a pipelined batch (the connection closes). `seeds/compressed/*.bin` are hand-built: an OK packet and a one-column result
set wrapped in zlib, zstd and stored (uncompressed) compressed packets, one
of them split across two compressed packets.
