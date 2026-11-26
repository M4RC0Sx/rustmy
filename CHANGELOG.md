# Changelog

All notable changes to rustmy are documented in this file. Versions follow [Semantic Versioning](https://semver.org) and entries come from [Conventional Commits](https://www.conventionalcommits.org).

## [1.4.0](https://github.com/M4RC0Sx/rustmy/compare/v1.3.2...v1.4.0) (2026-11-26)

### Features

* rustmy.aio, an asyncio API over the same protocol code, with connection pools and SQLAlchemy asyncio dialects ([#77](https://github.com/M4RC0Sx/rustmy/issues/77)) ([2c0a592](https://github.com/M4RC0Sx/rustmy/commit/2c0a592c279fab65b3cc4e7e3363bff3ecbd6800))

### Tests

* run Django's suite with fork where its runner does not support forkserver ([#64](https://github.com/M4RC0Sx/rustmy/issues/64)) ([f2f998d](https://github.com/M4RC0Sx/rustmy/commit/f2f998d4b77884a5be81373ca63e49197ec7d057))

### Continuous Integration

* stop running workflows for the merged feature/async branch ([#79](https://github.com/M4RC0Sx/rustmy/issues/79)) ([507beab](https://github.com/M4RC0Sx/rustmy/commit/507beabb030d975c696e5e87c801041fa3501d05))

## [1.3.2](https://github.com/M4RC0Sx/rustmy/compare/v1.3.1...v1.3.2) (2026-11-22)

### Performance Improvements

* keep row tuples of atomic values out of the garbage collector ([#53](https://github.com/M4RC0Sx/rustmy/issues/53)) ([6922f6f](https://github.com/M4RC0Sx/rustmy/commit/6922f6f1e10a6a9a42898d9e2b187e405a968b85))

## [1.3.1](https://github.com/M4RC0Sx/rustmy/compare/v1.3.0...v1.3.1) (2026-11-20)

### Documentation

* benchmark figures measured with the published 1.3.0 wheel ([00ef6cb](https://github.com/M4RC0Sx/rustmy/commit/00ef6cbb3470bcbae21ba5c9029e9a9dab40b8f6))

## [1.3.0](https://github.com/M4RC0Sx/rustmy/compare/v1.2.1...v1.3.0) (2026-11-19)

### Features

* default connect timeout over the whole handshake, TCP keepalive, get_server_public_key, and recovery from a full prepared-statement limit ([#48](https://github.com/M4RC0Sx/rustmy/issues/48)) ([e6f0ac4](https://github.com/M4RC0Sx/rustmy/commit/e6f0ac4c1564e15acb7c171bae152e2419b4a003))

### Bug Fixes

* bound parsec iterations by the connect timeout, ignore option files anyone can write, hide the password in Debug ([#45](https://github.com/M4RC0Sx/rustmy/issues/45)) ([45943e6](https://github.com/M4RC0Sx/rustmy/commit/45943e6dee46889ad61e975c9ee9568bc406d637))

### Performance Improvements

* build the TLS configuration once per process and skip SET NAMES on connect ([#46](https://github.com/M4RC0Sx/rustmy/issues/46)) ([e4bb4a9](https://github.com/M4RC0Sx/rustmy/commit/e4bb4a9f93d8e9cba2d51b05652195a1966356cd))
* classify parameters by exact type first and release buffers grown by large packets ([#47](https://github.com/M4RC0Sx/rustmy/issues/47)) ([a9df0cb](https://github.com/M4RC0Sx/rustmy/commit/a9df0cb672d1d6ab8a776fe1b3453ca57a5be552))

### Build System

* build wheels with the maturin of uv.lock and credit the Rust standard library in THIRD-PARTY-LICENSES ([#49](https://github.com/M4RC0Sx/rustmy/issues/49)) ([8eaf72b](https://github.com/M4RC0Sx/rustmy/commit/8eaf72b7190284272e5916cb0cffc3d66ae34fc7))
* **deps:** bump sha2 from 0.11.0 to 0.11.1 in the cargo group ([5126967](https://github.com/M4RC0Sx/rustmy/commit/51269678d14a0d3b9690973db91d3320931b9e3e))

### Miscellaneous Chores

* drop Pillow 12 from the lock and stop two harmful Dependabot updates ([#44](https://github.com/M4RC0Sx/rustmy/issues/44)) ([9bf5a86](https://github.com/M4RC0Sx/rustmy/commit/9bf5a863c3693504948e961647c3dfa296bcdf15))

## [1.2.1](https://github.com/M4RC0Sx/rustmy/compare/v1.2.0...v1.2.1) (2026-11-19)

### Documentation

* CONTRIBUTING.md with the project's rules, setup, test suites and design notes ([#42](https://github.com/M4RC0Sx/rustmy/issues/42)) ([ad39795](https://github.com/M4RC0Sx/rustmy/commit/ad39795353ba0eba7b01a6ed1fbfab7d7edc9dec))

## [1.2.0](https://github.com/M4RC0Sx/rustmy/compare/v1.1.0...v1.2.0) (2026-11-19)

### Features

* compressed protocol with zlib and zstd (compress=) ([#32](https://github.com/M4RC0Sx/rustmy/issues/32)) ([5b89ed4](https://github.com/M4RC0Sx/rustmy/commit/5b89ed46efd0ea9168c929f983116c66a28fced7))
* LOAD DATA LOCAL INFILE restricted to local_infile_dir ([#33](https://github.com/M4RC0Sx/rustmy/issues/33)) ([5013767](https://github.com/M4RC0Sx/rustmy/commit/501376738780a1b152592f0775e8059efdbe553a))
* prepared statements through PreparedCursor and the binary protocol ([#31](https://github.com/M4RC0Sx/rustmy/issues/31)) ([cd2d75c](https://github.com/M4RC0Sx/rustmy/commit/cd2d75ced3ada51aaa76e8130c09c0bbea5080fa))

### Bug Fixes

* accept an RSA key only when requested, cap payload sizes, harden LOCAL INFILE checks ([#36](https://github.com/M4RC0Sx/rustmy/issues/36)) ([5354508](https://github.com/M4RC0Sx/rustmy/commit/5354508f5ba765c08e2b3e306d398b77af9cbddd))

### Performance Improvements

* recover the row reading and interpolation cost of prepared statements ([#34](https://github.com/M4RC0Sx/rustmy/issues/34)) ([7ce0802](https://github.com/M4RC0Sx/rustmy/commit/7ce080286068d3a7660ee23c45344d01067bfde4))

### Continuous Integration

* pin actions by commit, attest release artifacts, Dependabot and weekly advisories ([#37](https://github.com/M4RC0Sx/rustmy/issues/37)) ([87de56b](https://github.com/M4RC0Sx/rustmy/commit/87de56b58eb8389cb84a49111027ca0827d9e365))
* stamp the release version under bash on Windows too ([1d5084c](https://github.com/M4RC0Sx/rustmy/commit/1d5084c2000f75cbe7cb6a4a31b2845c8a2fdb7c))

### Miscellaneous Chores

* issue and PR templates, Codecov coverage, leak check, showcase benchmarks and README rework ([#35](https://github.com/M4RC0Sx/rustmy/issues/35)) ([8ead10b](https://github.com/M4RC0Sx/rustmy/commit/8ead10b04c5a268809b630a324095c48fa8adb81))

## [1.1.0](https://github.com/M4RC0Sx/rustmy/compare/v1.0.1...v1.1.0) (2026-11-17)

### Features

* Django support through install_as_MySQLdb(), option files and binary_prefix ([#28](https://github.com/M4RC0Sx/rustmy/issues/28)) ([7074eef](https://github.com/M4RC0Sx/rustmy/commit/7074eefe0d851d7b25589073cedd1eebe566d607))
* pipelined executemany for statements other than INSERT … VALUES ([#29](https://github.com/M4RC0Sx/rustmy/issues/29)) ([ad79f6b](https://github.com/M4RC0Sx/rustmy/commit/ad79f6b5b1cace7e940edcaae48d4f2072f65d68))

## [1.0.1](https://github.com/M4RC0Sx/rustmy/compare/v1.0.0...v1.0.1) (2026-11-16)

### Bug Fixes

* fetchall() and fetchmany() return tuples, as mysqlclient does ([#25](https://github.com/M4RC0Sx/rustmy/issues/25)) ([216cc27](https://github.com/M4RC0Sx/rustmy/commit/216cc279ef7cd64a0d80ef7f24be9a355b774d51))

### Build System

* leave our own crates out of THIRD-PARTY-LICENSES ([#27](https://github.com/M4RC0Sx/rustmy/issues/27)) ([75574e5](https://github.com/M4RC0Sx/rustmy/commit/75574e548323df9b35c975f0d95be4e1690f105c))

### Continuous Integration

* fix fuzz workflow ([#24](https://github.com/M4RC0Sx/rustmy/issues/24)) ([558dc91](https://github.com/M4RC0Sx/rustmy/commit/558dc91ccd8293ee24231401b0440c1b7668b8df))

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
