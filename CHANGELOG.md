# Changelog

All notable changes to rustmy are documented in this file. Versions follow [Semantic Versioning](https://semver.org) and entries come from [Conventional Commits](https://www.conventionalcommits.org).

## 1.0.0 (2026-11-16)

### Features

* connection character sets and the server's default collation ([#12](https://github.com/M4RC0Sx/rustmy/issues/12)) ([5610a45](https://github.com/M4RC0Sx/rustmy/commit/5610a454ba84372b16a58a600b05b50b5996cd39))
* DB-API connection and cursor with Rust row conversion ([#2](https://github.com/M4RC0Sx/rustmy/issues/2)) ([c579dbf](https://github.com/M4RC0Sx/rustmy/commit/c579dbf54aae2abdb8ede5f1d5b18bf28591a1eb))
* DictCursor, streaming SSCursor/SSDictCursor, scroll and callproc ([#10](https://github.com/M4RC0Sx/rustmy/issues/10)) ([bb5d364](https://github.com/M4RC0Sx/rustmy/commit/bb5d364f63f3e4118e0f6d48041da6cb9bcb8529))
* MariaDB client_ed25519 and parsec authentication ([#6](https://github.com/M4RC0Sx/rustmy/issues/6)) ([958bd65](https://github.com/M4RC0Sx/rustmy/commit/958bd658c722d16b9c5734ec7e2c2effaedc5d1e))
* **proto:** connection, handshake, auth and text-protocol queries ([#1](https://github.com/M4RC0Sx/rustmy/issues/1)) ([03f7747](https://github.com/M4RC0Sx/rustmy/commit/03f77475de1b7fd7cee8bced6de6a83358ce7b45))
* RSA password exchange for caching_sha2_password and sha256_password ([#5](https://github.com/M4RC0Sx/rustmy/issues/5)) ([2efe788](https://github.com/M4RC0Sx/rustmy/commit/2efe7885c2b758e1880cd370f0ccf345780a49d3))
* select_db, reset_connection and change_user commands ([#7](https://github.com/M4RC0Sx/rustmy/issues/7)) ([740d054](https://github.com/M4RC0Sx/rustmy/commit/740d0543f6a6aa2a68192a2b8fca90537dfda0a1))
* sql_mode, multi_statements, conv= and protocol constants ([#13](https://github.com/M4RC0Sx/rustmy/issues/13)) ([5fb3b91](https://github.com/M4RC0Sx/rustmy/commit/5fb3b917efdd8bedf424d1934fee7f8f070869ba))
* SQLAlchemy dialects for MySQL and MariaDB ([#15](https://github.com/M4RC0Sx/rustmy/issues/15)) ([f64f9ac](https://github.com/M4RC0Sx/rustmy/commit/f64f9ac9dccdb7040410b027cb35fa136cde1c0a))
* TLS with ssl_mode, CA verification and client certificates ([#4](https://github.com/M4RC0Sx/rustmy/issues/4)) ([f3d54b3](https://github.com/M4RC0Sx/rustmy/commit/f3d54b35916b0318316594214297e4d48175cbd8))

### Bug Fixes

* character sets follow the server; PEP 249, property and differential tests ([#14](https://github.com/M4RC0Sx/rustmy/issues/14)) ([335cea3](https://github.com/M4RC0Sx/rustmy/commit/335cea3012f675471082711c14b28e4a1f0f787a))

### Performance Improvements

* add Phase 0 spike benchmarks against mysqlclient and PyMySQL ([#3](https://github.com/M4RC0Sx/rustmy/issues/3)) ([d0599b0](https://github.com/M4RC0Sx/rustmy/commit/d0599b05d30330b818d92763e3218a66ac28deed))
* interpolate parameters in Rust and batch executemany inserts ([#9](https://github.com/M4RC0Sx/rustmy/issues/9)) ([c1f56e0](https://github.com/M4RC0Sx/rustmy/commit/c1f56e0891850f81d1a939014d09b933c2eac559))
* read buffered result sets in batches ([#11](https://github.com/M4RC0Sx/rustmy/issues/11)) ([ecf4904](https://github.com/M4RC0Sx/rustmy/commit/ecf49046be931fca3ea32212a187ff9330e13280))

### Documentation

* empty readme ([65dea6a](https://github.com/M4RC0Sx/rustmy/commit/65dea6a82ad165bd297a15967ff2c503d5916fed))
* summary chart at the top of the README ([#18](https://github.com/M4RC0Sx/rustmy/issues/18)) ([a68022e](https://github.com/M4RC0Sx/rustmy/commit/a68022e91bf62d5352cd839c5f8fd7f0dc27a153))

### Build System

* scaffold Rust workspace, PyO3 extension and Python package ([fe2b528](https://github.com/M4RC0Sx/rustmy/commit/fe2b528cbb65087aa7d8f573a42e67dcbd2decc9))
* wheels for every platform, release automation and package meta ([#17](https://github.com/M4RC0Sx/rustmy/issues/17)) ([449ede6](https://github.com/M4RC0Sx/rustmy/commit/449ede6f65fdd6e2c676b4018e90dcb3c53beb95))

### Tests

* fuzz the protocol parsers and the client session ([#8](https://github.com/M4RC0Sx/rustmy/issues/8)) ([f87c34d](https://github.com/M4RC0Sx/rustmy/commit/f87c34d41f1f21d5e6587773c571bac2a5cb5bb5))
* keep the cyclic collector off in the Ctrl+C test ([#16](https://github.com/M4RC0Sx/rustmy/issues/16)) ([5127f7f](https://github.com/M4RC0Sx/rustmy/commit/5127f7f086b14c8b373a81cce88c4b33a8a6d0d5))

### Continuous Integration

* plan releases from the checkout, so pull requests rehearse them ([#20](https://github.com/M4RC0Sx/rustmy/issues/20)) ([3e20ac3](https://github.com/M4RC0Sx/rustmy/commit/3e20ac39e551f2ba7ae6ed707039798fb07fccb9))
* render the release notes in the plan; conventionalcommits preset 9.3.1 ([4820cea](https://github.com/M4RC0Sx/rustmy/commit/4820ceada276eff04fcb1f389a7558a029c03fb6))
* test full CPython matrix including free-threaded builds ([914441a](https://github.com/M4RC0Sx/rustmy/commit/914441a94aae390a3753b937a28ff40fa25d696f))
