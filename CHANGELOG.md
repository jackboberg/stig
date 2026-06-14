# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0] - YYYY-MM-DD

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
- `backups list` / `backups prune` — manage snapshots and reset backups
- Configuration via `stig.toml` with environment variable overrides
- Drift detection via migration checksums
