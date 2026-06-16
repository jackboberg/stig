# AGENTS.md

## Project

stig is a SQLite migration and schema CLI written in Rust (edition 2024).
Single crate: binary (`src/main.rs`) + library (`src/lib.rs`).

## Commands

```sh
# CI pipeline order (run all three before committing):
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test --all

# Run a single integration test file:
cargo test --test cli_migrate

# Review snapshot changes after modifying insta assertions:
cargo insta review
```

Git hooks are managed by `hk` (`hk.pkl`). Install with `hk install`.
Pre-commit: `cargo fmt` + `cargo clippy --all-targets -D warnings`.
Commit-msg: conventional commits enforced (e.g., `feat(cli): add redo command`).

## Architecture

```
src/
├── main.rs          # clap entrypoint, dispatches to cli/*::run()
├── lib.rs           # pub(crate) sha256_hex + module declarations
├── cli/             # one module per subcommand (init, new, migrate, status, redo, reset, restore, generate, backups, schema)
├── config.rs        # TOML loader, precedence: CLI flags > STIG_* env > stig.toml > defaults
├── db.rs            # rusqlite open + PRAGMAs + WAL checkpoint
├── errors.rs        # CliError enum with exit codes (see below)
├── migrate/         # discover (filename parsing), plan (diff logic), apply (execute + record)
├── schema/          # schema diff generation (diff.rs)
├── snapshot.rs      # copy/restore/prune pre-migration DB snapshots + reset backup restore
└── codegen/         # trait + TypeScript target (stub)
```

Commands that need the DB open a single `rusqlite::Connection`, apply PRAGMAs, use it for the
entire invocation, and close before any file moves (redo, reset).

## Testing

- **Unit tests**: inline `#[cfg(test)]` modules in each source file.
- **Integration tests**: `tests/` directory, one file per CLI command.
- **Shared helpers**: `tests/common/mod.rs` provides `stig_cmd(dir)`, `write_migration(dir, ts, slug, sql)`, and `STIG_ENV_KEYS` (list of env vars cleared per test).
- **Isolation**: every test uses `tempfile::TempDir`; `stig_cmd()` clears all `STIG_*` env vars to prevent shell leakage.
- **Snapshots**: `insta` with YAML feature for status output (`tests/snapshots/`).
- **Env isolation**: config tests use `MapEnv(HashMap)` injected into `Config::load()` — structurally cannot read `std::env`, no `set_var`/`serial` needed.

Test pattern:
```rust
let dir = TempDir::new().unwrap();
stig_cmd(&dir).arg("init").assert().success();
write_migration(&dir, "20240101000000", "create_foo", "CREATE TABLE foo (id INTEGER PRIMARY KEY);");
stig_cmd(&dir).arg("migrate").assert().success();
```

## Configuration

Config file: `stig.toml` (searched upward from CWD). Key env var overrides:
- `STIG_CONFIG` — explicit config path
- `STIG_DATABASE_PATH` — override `database_path` (also accepts `DATABASE_PATH` as fallback)
- `STIG_MIGRATIONS_DIR` — override `migrations_dir`
- `STIG_BACKUPS_DIR` — override `backups_dir`
- `STIG_NO_SNAPSHOT` — disable snapshots
- `STIG_NO_CHECKSUM` — skip checksum drift detection
- `STIG_SCHEMA_PATH` — override `schema_path`

Default layout:
```
stig.toml
app.db
db/migrations/*.sql
db/schema.sql
db/{snapshots,resets}/
```

## Exit Codes

| Code | Variant        | Meaning                                   |
|------|----------------|-------------------------------------------|
| 0    | —              | Success                                   |
| 1    | `Generic`      | SQL error, IO error, unexpected state     |
| 2    | `Usage`        | Config or usage error                     |
| 2    | `Declined`     | User declined a confirmation prompt       |
| 3    | `Drift`        | Checksum drift detected                   |
| 4    | `Prerequisite` | Snapshot gone, target unknown, etc.       |
| 5    | `Locked`       | Database unavailable                      |

## Changelog

When opening a PR, add an entry under `## [Unreleased]` in `CHANGELOG.md`
describing the change. Use the same format as existing entries.

