## What and why

<!-- What the change does and the problem it solves. Link the issue if there is one ("Fixes #123"). -->

## How it was tested

<!-- Tests added or run, servers used, benchmark before and after for performance changes. -->

## Checklist

See [CONTRIBUTING.md](https://github.com/M4RC0Sx/rustmy/blob/develop/CONTRIBUTING.md) for the setup and the rules.

- [ ] The title follows [Conventional Commits](https://www.conventionalcommits.org) (`feat:`, `fix:`, `perf:`, `docs:`...): it becomes the changelog entry.
- [ ] Tests cover the change, and `uv run pytest` and `cargo nextest run --workspace --exclude rustmy-pydriver` pass with the compose servers up.
- [ ] `uv run ruff check`, `uv run ruff format --check`, `uv run mypy` and `cargo clippy --workspace --all-targets -- -D warnings` are clean.
- [ ] Performance changes come with numbers before and after (`benches/`, against mysqlclient in the same run).
- [ ] The README is updated if the change is visible to users.
- [ ] No code, tests or documentation copied from GPL drivers (mysqlclient, mysql-connector-python).
