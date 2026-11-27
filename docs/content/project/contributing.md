---
description: "Contributing to rustmy, reporting vulnerabilities, and its license."
---

# Contributing

Issues and pull requests are welcome. Development needs Rust (via
[rustup](https://rustup.rs)), [uv](https://docs.astral.sh/uv/) and Docker for
the test servers:

```sh
uv sync                         # builds the extension
docker compose up -d --wait     # MySQL and MariaDB test servers
uv run pytest                   # Python tests
cargo nextest run --workspace --exclude rustmy-pydriver
```

[`CONTRIBUTING.md`](https://github.com/M4RC0Sx/rustmy/blob/master/CONTRIBUTING.md)
covers the setup, the test servers and suites, and the design behind the
code. Pull request titles follow
[Conventional Commits](https://www.conventionalcommits.org); they become the
changelog.

## This site

The site is built with [Zensical](https://zensical.org) from `docs/content/`,
and the API reference from the sources by mkdocstrings:

```sh
uv run --isolated --only-group docs --python 3.14 zensical serve
```

## Security

Please report vulnerabilities privately, as the
[security policy](https://github.com/M4RC0Sx/rustmy/blob/master/SECURITY.md)
explains.

## License

Licensed under either of
[Apache License, Version 2.0](https://github.com/M4RC0Sx/rustmy/blob/master/LICENSE-APACHE)
or [MIT license](https://github.com/M4RC0Sx/rustmy/blob/master/LICENSE-MIT),
at your option.

"MySQL" is a trademark of Oracle Corporation and "MariaDB" is a trademark of
MariaDB plc; rustmy is not affiliated with or endorsed by either.
