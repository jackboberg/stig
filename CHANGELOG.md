# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

<!-- next-header -->

## [Unreleased] - ReleaseDate

### Added

- Global CLI flags: `--config`, `--database-path`, `--migrations-dir`, `--backups-dir`, `--schema-path`, `--no-snapshot`, and `--no-checksum`. CLI flags now take precedence over environment variables and `stig.toml` values as documented. `--config` specifically overrides `STIG_CONFIG` (previously the env var won). When passed to `stig init`, the values of these flags are persisted into the generated `stig.toml`; for all other commands they apply only to that invocation.

### Changed

- Internal config API refactored: the public `Config` struct is replaced by a `Runtime` wrapper around a `pub(crate) ConfigFile` (the serde mirror of `stig.toml`) plus the resolved `project_root`. Path-resolution accessors (`db_path`, `migrations_path`, `backups_path`, `snapshots_path`, `resets_path`, `schema_file_path`, `is_memory_db`) now live on `Runtime`, so callers should use them instead of joining `project_root` with raw string fields. The supporting types `CliContext` and `CliOverrides` are renamed to `RunContext` and `ConfigOverrides`.
- Extracted shared `reapply_pending` into `src/migrate/mod.rs`, eliminating duplication between `redo` and `reset`. The schema-manifest fast path remains exclusive to `reset` (documented inline).
- Eliminated remaining bare `"snapshots"` / `"resets"` string literals from production code (`init.rs`) and unit tests (`snapshot.rs`). All call sites now use `Runtime::snapshots_path()` and `Runtime::resets_path()` helpers.
- `Db::open` now ensures the `schema_migrations` table exists internally, eliminating duplication across CLI call sites (`status`, `schema`, `migrate`, `redo`, `reset`).
- Extracted the duplicated migrations-directory existence check into a shared `require_migrations_dir` helper in `src/cli/guards.rs`, used by all six subcommands that need to read migration files (`status`, `schema`, `migrate`, `redo`, `reset`, `new`). The error message now includes a friendly hint: `"run \`stig init\` first"`.
- `stig new` now returns exit code 4 (`Prerequisite`) when the migrations directory is missing, matching the other subcommands. Previously it returned exit code 2 (`Usage`).
- Removed the redundant existence check from `discover()` in `src/migrate/discover.rs`. The CLI guard is now the single source of truth for this precondition; `discover()` relies on `read_dir()` to surface any TOCTOU races.

### Fixed

- `redo`, `reset`, and `restore` now exit with code 2 (`Usage`) when `database_path` is `:memory:`, instead of producing confusing file-copy errors or silently creating a file named `:memory:`.
- `redo` now exits with code 2 (`Declined`) when confirmation is run in a non-TTY environment, matching `reset`, `restore`, and `backups prune`.

<!-- next-url -->
[Unreleased]: https://github.com/jackboberg/stig/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/jackboberg/stig/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/jackboberg/stig/releases/tag/v0.1.0

## [0.2.0] - 2026-06-20

### Added

- `restore` — restore the database from a reset backup
- `stig new` now supports `$EDITOR` values with arguments (e.g. `EDITOR="code -w"`)
- SQLite busy/locked errors now exit with code 5 (`Locked`)

### Changed

- `reset` — auto-restores the original database if re-applying migrations fails

### Fixed

- Schema manifest (`schema.sql`) application is now atomic; a failed statement no longer leaves partial DDL/INSERT state

## [0.1.0] - 2026-06-14

### Added

- Forward-only SQLite migrations with filesystem snapshots for rollback
- Schema-aware TypeScript codegen
- `init` — bootstrap a new stig project
- `new` — scaffold timestamped migration files
- `migrate` — apply pending migrations with snapshot support
- `status` — report migration state
- `redo` — restore snapshot and re-apply a migration
- `reset` — destructive re-migration from empty
- `generate` — run codegen targets against live schema
- `schema diff` — generate migrations from schema differences
- `backups list` / `backups prune` — manage snapshots and reset backups
- Configuration via `stig.toml` with environment variable overrides
- Drift detection via migration checksums
