# stig

A SQLite migration and schema CLI.

Forward-only migrations, filesystem snapshots for local rollback, and
schema-aware codegen — all in a single binary.

```sh
cargo install stig
```

## Quickstart

Initialize a project, create a migration, apply it, and generate types:

```sh
# Bootstrap stig in the current directory
stig init

# Create a new migration (opens $EDITOR)
stig new create_users

# Apply all pending migrations
stig migrate

# Generate TypeScript types from the live schema
stig generate
```

After `init` you'll have a `stig.toml` config file, a `db/migrations/`
directory, and a `schema_migrations` tracking table in your database.

## Commands

### init

```text
stig init
```

Bootstraps a new stig project in the current directory. Creates the config
file (`stig.toml`), migrations directory, backups directory (with
`snapshots/` and `resets/` subdirs), and ensures `schema_migrations` exists
in the database.



### new

```text
stig new [OPTIONS] <DESCRIPTION>
```

Scaffolds a timestamped migration file (`db/migrations/<timestamp>_<slug>.sql`)
and opens it in `$EDITOR`. The description is slugified: lowercased,
non-alphanumeric characters collapsed to `_`, capped at 60 chars.

```sh
stig new create_users        # → 20260524103000_create_users.sql
stig new "Add Widget Types"  # → 20260524103000_add_widget_types.sql
stig new fix-bug-#123        # → 20260524103000_fix_bug_123.sql
```

Options:

- `--no-edit` — Skip opening `$EDITOR` after creating the file.

### migrate

```text
stig migrate [OPTIONS]
```

Applies all pending migrations in lexicographic order. Before each
migration, a filesystem snapshot of the database is taken (unless
`auto_snapshot = false`). Checksums are verified against
`schema_migrations` to detect drift.

Options:

- `--dry-run` — Preview what would be applied without mutating state.

```text
$ stig migrate
apply  20260524103000_add_widgets.sql  (snapshot: pre-20260524103000.db)
skip   20260323081155_initial_schema.sql
✓ 1 applied, 1 already up to date
```

### status

```text
stig status
```

Reports migration state without changing anything. Shows a table of applied,
pending, and drifted migrations along with snapshot availability.

```text
$ stig status
database: app.db
migrations dir: db/migrations
checksum check: on

  applied  drifted  snapshot   version                          file
  -------  -------  --------   --------------------------------  -----------------------------------------
  yes      no       pruned     20260323081155                    20260323081155_initial_schema.sql
  yes      no       yes        20260524103000                    20260524103000_add_widgets.sql
  no       —        —          20260525090000                    20260525090000_widget_indexes.sql

summary: 2 applied, 1 pending, 0 drifted
```

### redo

```text
stig redo [OPTIONS] [VERSION]
```

Restores the snapshot taken before the named version and replays all
migrations from that point forward. With no argument, defaults to the most
recent applied migration.

Options:

- `--yes` — Skip confirmation prompt.

Use this to pick up edits to an already-applied migration whose snapshot
still exists.

```text
$ stig redo
restoring pre-20260524103000.db
re-applying 20260524103000_add_widgets.sql
✓ redo complete
```

### reset

```text
stig reset [OPTIONS]
```

Destructive. Renames the live database into the resets backup directory and
re-migrates from empty. Prompts for confirmation unless `--yes` is passed.

If re-applying migrations fails partway through, the original database is
automatically restored from the reset backup. If that restore also fails, the
backup remains in `resets/` for manual recovery.

Use this when a migration has been edited but its snapshot has already been
pruned, or when you want a clean slate.

```sh
# Chain with your project's seed command
stig reset --yes && my-project seed
```

### restore

```text
stig restore [TIMESTAMP] [OPTIONS]
```

Restores the database from a reset backup created by `stig reset`. With no
`TIMESTAMP`, restores the most recent reset backup. With a timestamp, restores
the matching `reset-<TIMESTAMP>.db` file.

Prompts for confirmation unless `--yes` is passed.

Use this to return to a previous database state after testing against a fresh
reset.

```text
$ stig reset --yes
$ # ... test with empty database ...
$ stig restore --yes
✓ restored database from reset-20260520T101500Z.db
```

### generate

```text
stig generate [TARGET_NAME]
```

Runs configured codegen targets against the live schema. With no argument,
runs all targets. With a name (matched against `kind` or an optional `name`
field), runs only that target.

```text
$ stig generate
✓ typescript → lib/database/types.ts
```

### backups list

```text
stig backups list
```

Lists snapshots and reset backups with sizes and ages.

```text
$ stig backups list
snapshots (5 of max 5):
  pre-20260525090000.db        48 KiB   2 minutes ago
  pre-20260524103000.db        44 KiB   1 day ago
  ...

resets (1 of max 3):
  reset-20260520T101500Z.db   42 KiB   5 days ago
```

### backups prune

```text
stig backups prune [OPTIONS]
```

Removes old backups according to the `snapshot_keep` and `reset_keep`
policies. Prompts for confirmation unless `--yes` is passed.

## Configuration

Config lives in `stig.toml` at the project root. All keys have defaults;
the minimal config is an empty file.

```toml
# Path to the live SQLite database, relative to the project root or absolute.
database_path = "app.db"

# Where migration files live.
migrations_dir = "db/migrations"

# Where snapshots and reset backups live. Created by `init` with a .gitignore.
backups_dir = "db"

# How many pre-migration snapshots to retain.
snapshot_keep = 5

# How many reset backups to retain.
reset_keep = 3

# Whether to snapshot automatically before applying migrations.
auto_snapshot = true

# Whether to verify migration checksums on migrate/status.
checksum_check = true

# SQLite PRAGMAs applied on every connection open.
[pragmas]
journal_mode = "WAL"
foreign_keys = "ON"

# Codegen targets.
[[generate]]
kind   = "typescript"
path   = "lib/database/types.ts"
# Additional tables to exclude from codegen (glob patterns).
# sqlite_% and schema_migrations are always excluded internally.
# Values here are additive to those defaults.
exclude = ["posts"]
```

### Environment variables

| Env var | Overrides | Notes |
|---|---|---|
| `STIG_CONFIG` | config file path | Skips upward search |
| `STIG_DATABASE_PATH` | `database_path` | |
| `DATABASE_PATH` | `database_path` | Fallback for legacy setups |
| `STIG_MIGRATIONS_DIR` | `migrations_dir` | |
| `STIG_BACKUPS_DIR` | `backups_dir` | |
| `STIG_NO_SNAPSHOT` | sets `auto_snapshot = false` | Any non-empty value |
| `STIG_NO_CHECKSUM` | sets `checksum_check = false` | Any non-empty value |

`.env` is loaded automatically at the start of every command.

### Precedence

For any setting, highest wins:

1. CLI flag
2. Environment variable
3. `stig.toml` value
4. Built-in default

## Troubleshooting

### Drift errors

If `stig migrate` or `stig status` reports drift, a migration file has been
edited after it was applied. The fix depends on whether the snapshot still
exists:

```text
$ stig migrate
✗ migration 20260524103000_add_widgets has been edited since it was applied
  snapshot pre-20260524103000.db is available
  → run: stig redo 20260524103000
```

- **Snapshot available:** Run `stig redo <version>` to restore and re-apply.
- **Snapshot pruned:** Either revert the edit, or run `stig reset` to start
  fresh (the old database is backed up to the resets directory).
- **Intentional edits:** Set `checksum_check = false` in `stig.toml` or
  `STIG_NO_CHECKSUM=1` to skip verification entirely (e.g. in production
  deploys from immutable images).

### Missing snapshots

Snapshots are pruned automatically based on `snapshot_keep` (default 5). If
you need to redo a migration whose snapshot is gone, use `stig reset`
instead. Adjust `snapshot_keep` in `stig.toml` to keep more history.

### `:memory:` databases

Setting `database_path = ":memory:"` opens an in-memory database. In this
mode:

- `PRAGMA journal_mode = WAL` is skipped (WAL is incompatible with
  in-memory databases).
- Snapshots and resets are disabled.
- Migrations and codegen still work.

This is useful for CI or testing, but not for development workflows that
rely on snapshots.

## Exit codes

| Code | Meaning |
|---|---|
| 0 | Success |
| 1 | Generic failure (SQL error, IO error, unexpected state) |
| 2 | Usage / config error (invalid config, user declined prompt) |
| 3 | Drift detected |
| 4 | Prerequisite missing (snapshot gone, target unknown) |
| 5 | Database locked or otherwise unavailable |

## Development

Git hooks are managed by [hk](https://hk.jdx.dev) via [`hk.pkl`](./hk.pkl):

```sh
hk install
```

CI pipeline:

```sh
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test --all
```

## Versioning

This project follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html):

- **MAJOR** — incompatible CLI or configuration changes
- **MINOR** — new functionality (commands, options, codegen targets)
- **PATCH** — bug fixes and documentation improvements

Until v1.0.0, minor releases may contain breaking changes. These will be
documented in the [CHANGELOG](./CHANGELOG.md) with migration guidance.

### Installing a specific version

```sh
# Via cargo-install from crates.io
cargo install stig --version 0.1.0

# Via prebuilt binary from GitHub Releases
curl -sSL https://github.com/jackboberg/stig/releases/download/v0.1.0/stig-v0.1.0-aarch64-apple-darwin.tar.gz | tar xz
```

## License

MIT. See [`LICENSE`](./LICENSE).
