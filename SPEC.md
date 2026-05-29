# SPEC: SQLite Migration & Schema CLI (Rust)

> **Status:** Design document. No code exists yet.
> **Audience:** A fresh agent session (or human) bootstrapping a new Rust repo from scratch.
> **Scope:** Define the tool's user experience, file formats, internal architecture, and an initial issue backlog detailed enough to drive PR-sized chunks of work.

The tool is named `stig`. The name is Swedish for "path" or "footpath" (the verb form means "to step" or "to ascend"). It evokes the migration sequence as a forward path with each `migrate` invocation as a step along it — and, by extension, the snapshot trail as the footprints left behind.

---

## 1. Overview

`stig` is a project-agnostic command-line utility for managing SQLite schema evolution. It is written in Rust and distributed via `cargo install`. It is intended as a personal-quality replacement for ad-hoc inline migration scripts that tend to accrete in individual projects (Deno tasks, Node scripts, bash, etc.).

It deliberately mirrors the ergonomics of Supabase's local-development CLI and Rails' Active Record migrations, but is focused on a single backend: SQLite. There is no generic SQL abstraction.

### 1.1 Goals

- Apply forward-only SQL migrations against a SQLite database.
- Provide a coherent local-development workflow for iterating on the most recent migrations without committing to formal "down" migrations.
- Detect when a checked-in migration has been edited after being applied locally, and explain how to recover.
- Generate type bindings for the live schema. Ship TypeScript out of the box; design for additional targets later.
- Be safe by default: snapshot before destructive operations, prune backups on a configurable cadence.

### 1.2 Non-goals

- **No support for databases other than SQLite.** No Postgres, no MySQL, no driver abstraction layer.
- **No `down` migrations.** Local rollback is implemented via filesystem snapshots, not paired up/down files. Production rollback is "deploy a fix-forward migration."
- **No seeding.** Seed data is application-specific (it often calls into application code, e.g. a project's content-creation API); seeding remains in the consuming project.
- **No ORM-style query helpers.** This is a migration runner plus a codegen tool, not an ORM.
- **No GUI.** CLI only.

### 1.3 Why a dedicated tool?

Hand-rolled migration scripts (typically a single file in a project's `tasks/` or `scripts/` directory) tend to share the same shortcomings:

- They are duplicated per project.
- They do not detect drift (an edited-but-already-applied migration is silently ignored).
- Codegen, when present, is hand-written and not extensible.
- "Reset" routines tend to write sidecar backups next to the live DB with manual cleanup.

A standalone tool centralizes this logic, adds the missing safety rails, and removes the per-project maintenance burden.

---

## 2. Conceptual model

Four concepts:

1. **Migrations.** Ordered, immutable-once-applied `.sql` files. Each is identified by a `yyyyMMddHHmmss_<slug>` version string and tracked in a `schema_migrations` table by version and content checksum.
2. **Snapshots.** A pre-migration filesystem copy of the entire SQLite database (including WAL/SHM sidecars), captured automatically before each migration is applied. Snapshots are how local rollback works.
3. **Reset backups.** Distinct from snapshots: created by the explicit `reset` command, which renames the live DB out of the way and re-migrates from empty.
4. **Codegen targets.** Configured generators that introspect the live schema and emit type definitions. TypeScript is the first and only built-in target in the MVP.

### 2.1 The dev-iteration story

The driving use case is: *I'm writing a new migration, applied it locally, ran the app, realized the schema is wrong, want to edit the .sql and try again — without rebuilding seeded data if I can help it.*

Workflow:

```
$ stig new add_widgets        # writes db/migrations/20260524103000_add_widgets.sql
$ $EDITOR db/migrations/20260524103000_add_widgets.sql
$ stig migrate                # snapshots app.db, applies the migration, records checksum
# ...realize the schema is wrong...
$ $EDITOR db/migrations/20260524103000_add_widgets.sql
$ stig migrate                # detects checksum drift on the tail, points at `redo`
$ stig redo                   # restores pre-snapshot, re-applies the edited file
$ stig generate               # regenerate TS types
```

Snapshots are bounded by `snapshot_keep` (default 5). As long as a migration's `pre-<version>.db` snapshot still exists, `stig redo <version>` can restore the DB to the state immediately before that migration and replay everything from there forward. Once a snapshot has been pruned, that migration is effectively immutable for local-redo purposes and editing it requires a full `reset`.

This gives you a small, deterministic rollback window for a feature-branch's worth of in-progress migrations, without ever writing a `down.sql`.

### 2.2 The drift story

`schema_migrations.checksum` stores the SHA-256 of each migration file at the moment it was applied. On every `migrate` invocation, `stig` recomputes checksums and compares.

- **No drift:** proceed.
- **Drift on a migration whose `pre-<version>.db` still exists:** fail with a message naming the affected version and instructing the user to run `stig redo <version>`.
- **Drift on a migration whose snapshot has been pruned:** hard fail with a message instructing the user to either revert their edit or run `stig reset` (which prompts for confirmation, since reset destroys local data).
- **`checksum_check = false` in config:** the check is skipped entirely. Intended for production images where migrations are immutable and snapshots are not maintained.

### 2.3 What lives where

```
<project-root>/
├── stig.toml               # config
├── app.db                    # live SQLite database (path is configurable)
├── db/
│   └── migrations/
│       ├── 20260323081155_initial_schema.sql
│       └── 20260524103000_add_widgets.sql
├── lib/database/types.ts     # generated TS types (path is configurable)
└── .local/
    └── db-backups/
        ├── snapshots/
        │   ├── pre-20260524103000.db
        │   ├── pre-20260524103000.db-wal
        │   └── pre-20260524103000.db-shm
        └── resets/
            └── reset-20260524T103045Z.db
```

---

## 3. CLI surface

All commands accept `--config <path>` (default: walk upward from CWD looking for `stig.toml`) and `-v` / `-vv` for verbosity. Long-running commands print structured progress; quiet on success unless `-v`.

```
stig init [--force]
stig new <description> [--no-edit]
stig migrate [--dry-run]
stig status
stig redo [<version>] [--yes]
stig reset [--yes]
stig generate [<target-name>]
stig backups list
stig backups prune [--yes]
```

Below, each command is described in full with example output.

### 3.1 `init`

Bootstraps a new project to use `stig`.

- Writes `stig.toml` with default values if absent. With `--force`, overwrites an existing config (asks for confirmation first unless `--yes` is also passed).
- Creates the migrations directory if missing.
- Creates the backups directory if missing, including the `snapshots/` and `resets/` subdirs and a `.gitignore` containing `*`.
- Opens (or creates and opens) the database at `database_path` and ensures `schema_migrations` exists.

```
$ stig init
✓ wrote stig.toml
✓ created db/migrations/
✓ created .local/db-backups/{snapshots,resets}/ (gitignored)
✓ created schema_migrations in app.db
```

Exit codes: 0 ok, 2 if a config exists and `--force` was not passed.

### 3.2 `new <description>`

Scaffolds a new migration file and opens it in `$EDITOR` (skipped with `--no-edit`).

- Description is sluggified: lowercased, non-alphanumeric characters collapsed to `_`, leading/trailing underscores stripped, capped at 60 chars.
- Filename: `<UTC timestamp yyyyMMddHHmmss>_<slug>.sql`. The timestamp is generated at invocation time; if a file with the same timestamp already exists, exits 2 with an error message (wait a second and retry).
- Initial contents:

  ```sql
  -- Migration: <description>
  -- Created:   <ISO 8601 UTC timestamp>
  --
  -- To make this migration apply outside a transaction (e.g. to run
  -- PRAGMA or FTS5 rebuild statements that don't allow transactions),
  -- uncomment the directive on the next line:
  -- stig: non-transactional


  ```

```
$ stig new add_widgets
✓ db/migrations/20260524103000_add_widgets.sql
  opening in $EDITOR ...
```

Exit codes: 0 ok, 2 if description is empty after slugification.

### 3.3 `migrate`

Applies all pending migrations.

Order of operations:

1. Verify the migrations directory exists and `schema_migrations` is present.
2. Compute SHA-256 of every file in `db/migrations/*.sql`.
3. Compare against `schema_migrations` rows. Detect drift per §2.2.
4. For each pending migration (in lexicographic order):
   - If `auto_snapshot` is true, copy `app.db` (plus `-wal` and `-shm` if they exist) to `<backups_dir>/snapshots/pre-<version>.db{,-wal,-shm}`. Perform a WAL checkpoint first to ensure the snapshot is consistent.
   - Parse the migration. If the file contains a `-- stig: non-transactional` directive in a comment in the first ~10 lines, apply it outside a transaction; otherwise wrap the entire file in a single `BEGIN ... COMMIT`.
   - Execute via `rusqlite::Connection::execute_batch`.
   - Insert `(version, checksum)` into `schema_migrations`.
   - Prune snapshots beyond `snapshot_keep`.
5. Print a summary.

`--dry-run` performs steps 1–3 plus the parse step of 4, but executes no SQL and writes no snapshots. Useful in CI to verify migrations are well-formed without mutating state.

```
$ stig migrate
apply  20260524103000_add_widgets.sql  (snapshot: pre-20260524103000.db)
skip   20260323081155_initial_schema.sql
✓ 1 applied, 1 already up to date
```

Drift example:

```
$ stig migrate
✗ migration 20260524103000_add_widgets has been edited since it was applied
  snapshot pre-20260524103000.db is available
  → run: stig redo 20260524103000
```

Exit codes: 0 ok, 1 SQL/IO failure, 3 drift detected, 4 prerequisite missing (e.g. no config).

### 3.4 `status`

Reports schema state without changing anything. Output is a table:

```
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

Columns:

- `applied`: present in `schema_migrations`.
- `drifted`: applied checksum does not match file checksum.
- `snapshot`: `yes` if `pre-<version>.db` exists, `pruned` if applied but no snapshot, `—` if not applicable.

Exit codes: 0 ok, 3 if any drift is detected (so CI can fail).

### 3.5 `redo [<version>]`

Restores the snapshot taken before the named version and replays from there forward.

- With no argument, `version` defaults to the most recent applied migration.
- Requires `<backups_dir>/snapshots/pre-<version>.db` to exist; fails otherwise with the list of versions that *do* have snapshots.
- Prompts for confirmation unless `--yes` is passed: "this will discard local data added since version X."
- Procedure:
  1. Close the active DB connection.
  2. Move the live DB to a temporary location.
  3. Copy `pre-<version>.db{,-wal,-shm}` into place as the live DB.
  4. Delete `(version, …)` rows from `schema_migrations` for `version` and any newer version (the snapshot already reflects pre-version state, so these rows would be inconsistent).
  5. Run the equivalent of `migrate` to re-apply `version` and any newer migrations on disk. Note: this creates a fresh `pre-<version>.db` snapshot (the old one is overwritten as part of normal migrate flow).
  6. On any failure during steps 3–5, restore the temporary DB and report the error.

```
$ stig redo
restoring pre-20260524103000.db
re-applying 20260524103000_add_widgets.sql
✓ redo complete
```

Exit codes: 0 ok, 1 IO failure, 4 snapshot missing.

### 3.6 `reset`

Destructive. Renames the live database into the resets dir and re-migrates from empty.

- Prompts for confirmation unless `--yes` is passed.
- Procedure:
  1. WAL checkpoint, close connection.
  2. Move `app.db{,-wal,-shm}` to `<backups_dir>/resets/reset-<UTC timestamp>.db{,-wal,-shm}`. All three must move successfully or any partial moves are rolled back.
  3. Open a fresh DB at `database_path`.
  4. Run `migrate` (which will snapshot each migration as it goes).
  5. Prune resets beyond `reset_keep`.

`reset` does **not** run seeds. Seeding is project-specific and lives outside this tool. The consuming project is expected to chain reset with whatever seeding command it has, e.g.:

```
$ stig reset --yes && <project's seed command>
```

Exit codes: 0 ok, 1 IO failure, 2 user declined confirmation.

### 3.7 `generate [<target-name>]`

Runs configured codegen targets.

- With no argument, runs every target in `[[generate]]` config order.
- With a name (matched against `kind` or an optional `name` field), runs only that target.
- Each target receives a read-only handle to the live database and writes to the configured `path`.
- After writing, if the target's config has a `format` command (e.g. `deno fmt {path}`), the command is run with `{path}` substituted. Failures here are warnings, not errors.

```
$ stig generate
✓ typescript → lib/database/types.ts (formatted with `deno fmt`)
```

Exit codes: 0 ok, 1 target failure, 4 named target not found.

### 3.8 `backups list` / `backups prune`

```
$ stig backups list
snapshots (5 of max 5):
  pre-20260525090000.db        48 KiB   2 minutes ago
  pre-20260524103000.db        44 KiB   1 day ago
  ...

resets (1 of max 3):
  reset-20260520T101500Z.db   42 KiB   5 days ago
```

`backups prune` applies the keep policies immediately. Prompts unless `--yes`.

Exit codes: 0 ok.

---

## 4. Migration file format

- Location: `<migrations_dir>` (default `db/migrations`).
- Filename: `<yyyyMMddHHmmss>_<slug>.sql`. Both parts are required. Files not matching the pattern are ignored with a warning.
- Contents: plain SQLite DDL/DML. No `--@up`/`--@down` sections. No JSON sidecar.
- Optional directive in the first 10 lines: `-- stig: non-transactional`. This opts the file out of the implicit `BEGIN ... COMMIT` wrapper. Required for statements SQLite does not allow inside a transaction (certain PRAGMAs, FTS5 vtable rebuilds, WAL checkpoints, vacuum, etc.).
- No multi-statement-per-line restrictions. The parser hands the file contents to `execute_batch`.

Filename rules:

- Timestamp portion: exactly 14 digits, interpreted as UTC.
- Slug portion: `[a-z0-9_]+`, 1–60 chars.
- Two files with the same timestamp are an error.

---

## 5. Tracking table

`stig` owns the `schema_migrations` table. Schema:

```sql
CREATE TABLE IF NOT EXISTS schema_migrations (
  version     TEXT NOT NULL PRIMARY KEY,
  checksum    TEXT NOT NULL,                                -- hex-encoded SHA-256
  applied_at  TEXT NOT NULL DEFAULT (datetime('now'))       -- ISO 8601 UTC
);
```

- `version` is the migration filename with the `.sql` suffix stripped (e.g. `20260524103000_add_widgets`).
- `checksum` is the lower-case hex SHA-256 of the file's exact bytes at the moment it was applied (no normalization, no whitespace stripping — the file is what it is).
- The table is created automatically by `init` and `migrate`; it is also excluded from codegen output by default.

### 5.1 Upgrading from a tool that lacked the checksum column

Not in scope for MVP. Document explicitly: a user migrating from a hand-rolled tool whose `schema_migrations` table lacks a `checksum` column should `stig init` against the existing DB, then manually run `ALTER TABLE schema_migrations ADD COLUMN checksum TEXT NOT NULL DEFAULT ''` and `UPDATE schema_migrations SET checksum = '<computed>'` once before relying on drift detection. Treat this as a v0.2 nicety — out of scope for v0.1.

---

## 6. Configuration

Config lives in `stig.toml` at the project root (or any path passed via `--config`). All keys have defaults; minimal usable config is the empty file.

### 6.1 Full example

```toml
# Path to the live SQLite database, relative to the project root or absolute.
database_path = "app.db"

# Where migration files live.
migrations_dir = "db/migrations"

# Where snapshots and reset backups live. The tool creates this on init and
# writes a `.gitignore` containing `*` inside it.
backups_dir = ".local/db-backups"

# How many pre-migration snapshots to retain. Older snapshots are pruned at
# the end of each `migrate` call.
snapshot_keep = 5

# How many reset backups to retain.
reset_keep = 3

# Whether to take snapshots automatically before applying migrations.
# Turn this off in production where snapshots are unnecessary overhead.
auto_snapshot = true

# Whether to verify migration checksums against schema_migrations on every
# `migrate` and `status` call. Turn this off if you intentionally edit
# old migrations in place and want the tool to stop complaining (e.g. in
# production deploys from immutable images).
checksum_check = true

# SQLite PRAGMAs applied on every connection open.
[pragmas]
journal_mode = "WAL"
foreign_keys = "ON"

# Codegen targets. Each entry produces one output file when `generate` runs.
[[generate]]
kind   = "typescript"
path   = "lib/database/types.ts"
format = "deno fmt {path}"            # optional; {path} is substituted
exclude = ["sqlite_%", "schema_migrations"]
```

### 6.2 Precedence

For any given setting:

1. CLI flag (highest)
2. Environment variable
3. `stig.toml` value
4. Built-in default

Environment variable names (full list):

| Env var | Overrides | Notes |
|---|---|---|
| `STIG_CONFIG` | path to config file | Otherwise upward search from CWD. |
| `STIG_DATABASE_PATH` or `DATABASE_PATH` | `database_path` | `DATABASE_PATH` is honored for parity with widely-used ad-hoc conventions. |
| `STIG_MIGRATIONS_DIR` | `migrations_dir` | |
| `STIG_BACKUPS_DIR` | `backups_dir` | |
| `STIG_NO_SNAPSHOT` | sets `auto_snapshot = false` | Any non-empty value. |
| `STIG_NO_CHECKSUM` | sets `checksum_check = false` | Any non-empty value. |

`.env` is loaded automatically via `dotenvy::dotenv()` at the start of every command. `.env.local` and friends are *not* loaded by `stig` — that is the consuming project's responsibility.

---

## 7. TypeScript codegen

The MVP ships a single codegen target: `kind = "typescript"`. It produces a single `.ts` file whose shape matches the Supabase `gen types typescript` convention so it can drop in as a replacement for the existing `lib/database/types.ts`.

### 7.1 Output shape

```ts
// Generated by stig. Do not edit by hand.
// Source: 5 tables, 4 enums introspected from app.db.

export type Enums = {
  posts_type: "entry" | "event";
  posts_subtype: "note" | "article" | "photo" | "bookmark" | "reply" | "like" | "repost" | "rsvp" | "review";
  references_type: "in-reply-to" | "like-of" | "repost-of" | "bookmark-of";
  // ...
};

export type Tables = {
  "users": {
    Row: {
      id: string;
      email: string;
      password_hash: string;
      created_at: string;
      updated_at: string;
    };
    Insert: {
      id?: string;
      email: string;
      password_hash: string;
      created_at?: string;
      updated_at?: string;
    };
    Update: {
      id?: string;
      email?: string;
      password_hash?: string;
      created_at?: string;
      updated_at?: string;
    };
  };
  // ...
};

export type TableName = keyof Tables;
export type Row<T extends TableName>    = Tables[T]["Row"];
export type Insert<T extends TableName> = Tables[T]["Insert"];
export type Update<T extends TableName> = Tables[T]["Update"];
```

### 7.2 Introspection sources

For each table not matched by `exclude`:

1. `PRAGMA table_info('<table>')` — column name, declared type, nullability, default, primary-key flag.
2. `PRAGMA foreign_key_list('<table>')` — captured for future targets, not used in the TS output.
3. The table's `CREATE TABLE` statement from `sqlite_master.sql` — regex-extracted to find `CHECK (<col> IN ('a','b',...))` constraints, which become string-literal-union enums.

### 7.3 SQLite affinity → TS type

| SQLite type (case-insensitive) | TS type |
|---|---|
| `INTEGER`, `INT`, `BIGINT`, `SMALLINT`, `TINYINT`, `MEDIUMINT`, `REAL`, `DOUBLE`, `FLOAT`, `NUMERIC`, `DECIMAL` | `number` |
| `TEXT`, `VARCHAR`, `CHAR`, `CLOB`, `DATE`, `DATETIME`, `TIME` | `string` |
| `BLOB` | `Uint8Array` |
| anything else | `string` (with a warning logged at `-v`) |

A column constrained by `CHECK (col IN (...))` overrides the affinity mapping with the corresponding `Enums.<table>_<column>` reference.

### 7.4 Nullability and default rules

- `Row`: every column present. Nullable columns get `| null`.
- `Insert`: a column is optional (`?`) if it has a `DEFAULT`, is nullable, or is an `INTEGER PRIMARY KEY` rowid alias. Nullable columns also get `| null`.
- `Update`: every column optional. Nullable columns get `| null`.

### 7.5 Exclusions

- `sqlite_%` (system tables) — always excluded.
- `schema_migrations` — always excluded.
- Patterns in `[[generate]].exclude` — SQL `LIKE`-style glob, matched against the table name. Default is `["sqlite_%", "schema_migrations"]`. Projects with extension-managed tables (e.g. queue extensions that create internal tables at runtime) typically add a pattern like `"_<ext>_%"` to keep the generated types reproducible.

### 7.6 Extensibility — the codegen trait

```rust
pub trait CodegenTarget {
    /// Stable identifier used by config (`kind = "..."`).
    fn kind(&self) -> &'static str;

    /// Run introspection and write the output. Receives a read-only
    /// connection plus the target's resolved config.
    fn generate(
        &self,
        conn: &rusqlite::Connection,
        config: &CodegenConfig,
    ) -> Result<GenerateOutput, CodegenError>;
}

pub struct CodegenConfig {
    pub path: PathBuf,
    pub exclude: Vec<String>,
    pub format: Option<String>,
    pub extra: toml::Table,        // kind-specific options
}

pub struct GenerateOutput {
    pub path: PathBuf,
    pub bytes_written: u64,
    pub formatted: bool,
}
```

Future targets (Zod schemas, Kysely, Rust types, Python TypedDicts) are added as new implementations of this trait, registered in a target dispatcher. Adding a target should be one new module plus one line in the dispatcher.

---

## 8. Internal architecture (Rust)

### 8.1 Crate layout

Single crate, binary + library. Library exposes the migration runner and codegen trait so they can be embedded; the binary is a thin clap front-end.

```
stig/
├── Cargo.toml
├── src/
│   ├── main.rs                  # clap entrypoint, dispatches to commands
│   ├── lib.rs
│   ├── cli/
│   │   ├── mod.rs
│   │   ├── init.rs
│   │   ├── new.rs
│   │   ├── migrate.rs
│   │   ├── status.rs
│   │   ├── redo.rs
│   │   ├── reset.rs
│   │   ├── generate.rs
│   │   └── backups.rs
│   ├── config.rs                # TOML loader, precedence rules
│   ├── db.rs                    # rusqlite open + PRAGMAs + checkpoint helpers
│   ├── migrate/
│   │   ├── mod.rs
│   │   ├── discover.rs          # scan migrations_dir, parse filenames
│   │   ├── plan.rs              # diff applied vs on-disk, detect drift
│   │   └── apply.rs             # snapshot + execute + record
│   ├── snapshot.rs              # copy/restore/prune (snapshots + resets)
│   ├── checksum.rs              # sha256 helpers
│   ├── codegen/
│   │   ├── mod.rs               # trait + dispatcher
│   │   └── typescript.rs        # built-in target
│   └── errors.rs                # exit-code-bearing error enum
├── tests/
│   ├── cli_init.rs
│   ├── cli_migrate.rs
│   ├── cli_redo.rs
│   ├── cli_reset.rs
│   ├── cli_generate.rs
│   └── golden/                  # snapshot test fixtures for TS codegen
└── README.md
```

### 8.2 Dependencies

| Crate | Purpose |
|---|---|
| `clap` (derive) | CLI parsing |
| `rusqlite` with `bundled` feature | SQLite (no system dependency) |
| `serde`, `serde_derive`, `toml` | Config |
| `sha2`, `hex` | Checksums |
| `chrono` | Timestamps, formatting |
| `anyhow` | CLI-level error wrapping |
| `thiserror` | Library-level typed errors |
| `tracing`, `tracing-subscriber` | Logging at `-v`/`-vv` |
| `dotenvy` | `.env` loading |
| `dialoguer` | Confirmation prompts (`--yes` bypasses) |
| `walkdir` | Migrations + snapshots directory traversal |
| `tempfile` | Test fixtures |
| `assert_cmd`, `predicates` | Integration tests against the compiled binary |
| `insta` | Golden-file tests for codegen output |

### 8.3 Error model and exit codes

A single `enum CliError` carries an exit code per variant. The `main` function converts the error to an exit code on the way out.

| Exit code | Meaning |
|---|---|
| 0 | Success |
| 1 | Generic failure (SQL error, IO error, unexpected state) |
| 2 | Usage / config error |
| 3 | Drift detected |
| 4 | Prerequisite missing (snapshot gone, target unknown, etc.) |
| 5 | Database locked or otherwise unavailable |

### 8.4 Connection lifecycle

- Every command that needs the DB opens a single `rusqlite::Connection`, applies the configured PRAGMAs, and uses it for the entire command.
- Commands that need to *replace* the DB file (`redo`, `reset`) close the connection explicitly before any file moves.
- `:memory:` is supported (set `database_path = ":memory:"`) but renders snapshots and resets meaningless; `stig` warns about this and disables both at runtime.

### 8.5 Filesystem safety

- All snapshot and reset operations move sidecars (`-wal`, `-shm`) along with the main file.
- WAL checkpoint runs before any copy to ensure the snapshot is internally consistent.
- File moves use rename-within-same-filesystem semantics where possible; fallback to copy + delete when crossing filesystems.
- Partial moves are unwound on failure (rollback to original state, then surface the error).

---

## 9. Testing strategy

### 9.1 Unit tests

- `checksum.rs`: known-input/known-output SHA-256 vectors.
- `migrate/discover.rs`: filename parsing, including malformed timestamps and slugs.
- `migrate/plan.rs`: diff logic — fully applied, partial, with drift, with missing files (applied-but-not-on-disk).
- `config.rs`: precedence (CLI > env > TOML > default).

### 9.2 Integration tests

Each test builds a temp project layout with `tempfile::TempDir`, invokes the binary via `assert_cmd`, and asserts on exit code + stdout + filesystem state.

Coverage:

- `init` on an empty dir.
- `init --force` over an existing config.
- `new` produces a properly-named file with the expected template; the slug rules are exercised.
- `migrate` on a fresh DB applies a sample migration set; `schema_migrations` is correct; snapshots exist.
- `migrate` is a no-op when up-to-date.
- `migrate` detects drift and exits 3.
- `migrate --dry-run` does not mutate state.
- `status` reports applied/pending/drifted/snapshot correctly across scenarios.
- `redo` restores a snapshot and re-applies; the new snapshot reflects the re-applied state.
- `redo` errors cleanly when the requested snapshot has been pruned.
- `reset --yes` renames the live DB into resets and re-migrates.
- `generate` produces the expected TS file; golden-file comparison via `insta`.
- `backups list` and `backups prune` show and prune correctly.

### 9.3 Golden tests for TS codegen

`tests/golden/` contains migration sets and expected TS outputs. `insta` is used for snapshot review (`cargo insta review`).

Cover at minimum:

- Single simple table (all SQLite affinities present).
- Nullable columns, defaults, primary keys (TEXT pk, INTEGER rowid alias).
- Enums (`CHECK (col IN (...))`).
- Reserved-word table names (e.g. `"references"`).
- Excluded tables (`schema_migrations` and a glob-matched table).
- Foreign keys (introspected but not yet rendered — verify the introspection result, not output).

### 9.4 CI

- Matrix: stable Rust on `ubuntu-latest` and `macos-latest`.
- Steps: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test --all-features`.
- Cache: `actions/cache` keyed on `Cargo.lock`.
- Release workflow (optional, in a follow-up): tag-triggered `cargo publish` after passing CI.

---

## 10. Open items (intentionally deferred)

- **Distribution beyond `cargo install`.** Homebrew tap, prebuilt binaries via `cargo-dist`, GitHub Releases artifacts. Capture as a stretch issue (§12, issue 17).
- **Cross-database support.** Permanently out of scope per design constraints. If a future need arises, fork the tool — do not retrofit.
- **`down` migrations.** Permanently out of scope. The snapshot/redo model replaces this for dev; production is fix-forward.
- **Seeding.** Permanently out of scope. Lives in the consuming project.
- **Language bindings (Deno, Node, etc.).** Not planned. The CLI is the API; codegen targets are the integration point.

---

## 11. Issue backlog

Each issue below is sized to ship as a single PR. Issues are listed in suggested implementation order; explicit prerequisites are called out. Titles are GitHub-issue-ready.

---

### Issue 1 — Bootstrap repo skeleton

**Description.** Create a new Rust binary crate. Set up the directory layout described in §8.1 with empty modules. Add `Cargo.toml` with the dependencies listed in §8.2. Add MIT license, a README skeleton pointing at this spec, and a GitHub Actions workflow that runs `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, and `cargo test` on Ubuntu and macOS with stable Rust. Cache `~/.cargo` and `target/` keyed on `Cargo.lock`.

**Acceptance.**
- `cargo build` succeeds.
- `cargo test` succeeds (the empty suite).
- CI runs green on a PR.
- README links to `SPEC.md` and states the tool is pre-alpha.

**Prerequisites.** None.

---

### Issue 2 — Config loader

**Description.** Implement `src/config.rs` per §6. Load `stig.toml` from the project root (upward search from CWD), apply environment-variable overrides, and expose a `Config` struct to the rest of the crate. CLI-flag overrides are wired up by individual commands but the loader must accept programmatic overrides. Defaults match §6.1. `.env` loading via `dotenvy::dotenv()` happens here.

**Acceptance.**
- Unit tests cover: empty config (all defaults), partial config, full config, env var override, CLI override.
- Invalid TOML produces a clear error with exit code 2.
- Missing config file is not an error — defaults are used.

**Prerequisites.** Issue 1.

---

### Issue 3 — Connection layer

**Description.** Implement `src/db.rs`: open a `rusqlite::Connection` at the configured path, apply PRAGMAs from config, expose `checkpoint()` and `close()` helpers. Support `:memory:` (skip WAL-incompatible PRAGMAs, warn that snapshots/resets are disabled).

**Acceptance.**
- Opening a file DB applies `journal_mode = WAL` and `foreign_keys = ON` by default.
- Opening `:memory:` skips WAL but applies `foreign_keys`.
- Unit tests cover both modes.

**Prerequisites.** Issue 2.

---

### Issue 4 — `init` command

**Description.** Implement `stig init` per §3.1. Creates config file (if absent), migrations directory, backups directory with `.gitignore`, and the `schema_migrations` table. `--force` overwrites the config.

**Acceptance.**
- Integration test: in an empty temp dir, `stig init` succeeds, creates all expected files/dirs, and `schema_migrations` is queryable.
- Re-running `stig init` without `--force` exits 2 and changes nothing.
- `stig init --force` overwrites the config.

**Prerequisites.** Issue 3.

---

### Issue 5 — Migration discovery and filename parsing

**Description.** Implement `src/migrate/discover.rs`: scan `migrations_dir` for `.sql` files, parse `<14-digit timestamp>_<slug>.sql` filenames, validate slug rules from §4. Return a sorted `Vec<MigrationFile>`. Files that don't match the pattern emit a warning at `-v` and are skipped.

**Acceptance.**
- Unit tests cover: valid filenames, invalid timestamp, invalid slug chars, duplicate timestamps (error), empty dir, missing dir.
- Files are returned in lexicographic order.

**Prerequisites.** Issue 2.

---

### Issue 6 — `new` command

**Description.** Implement `stig new <description>` per §3.2. Slugify, generate timestamp, write template, open in `$EDITOR` unless `--no-edit`.

**Acceptance.**
- Integration test: `new "Add Widgets!!!"` produces `<ts>_add_widgets.sql` with the expected template.
- Empty/whitespace-only description exits 2.
- Same-timestamp collision exits 2 (wait a second and retry).
- `--no-edit` skips `$EDITOR`.

**Prerequisites.** Issues 2, 5.

---

### Issue 7 — Checksum module and tracking table upgrades

**Description.** Implement `src/checksum.rs` (`sha256_hex(bytes) -> String`). Update the `schema_migrations` definition to include the `checksum` column (§5). Ensure `init` and `migrate` both ensure-this-column-exists on entry; this is forward-only.

**Acceptance.**
- Unit test against known SHA-256 vectors.
- Integration test: `init` creates `schema_migrations` with the `checksum` column.

**Prerequisites.** Issue 4.

---

### Issue 8 — Snapshot module

**Description.** Implement `src/snapshot.rs`. Functions: `take_snapshot(version, db_path, backups_dir)` (WAL checkpoint, then copy main file + sidecars to `snapshots/pre-<version>.db{,-wal,-shm}`), `restore_snapshot(version, ...)` (reverse), `prune_snapshots(keep)` (delete oldest beyond keep), and equivalents for resets (`reset-<UTC>.db`).

**Acceptance.**
- Unit/integration tests cover: take snapshot, snapshot includes sidecars, restore round-trips, prune keeps N newest.
- Failures during sidecar moves roll back partial state.

**Prerequisites.** Issue 3.

---

### Issue 9 — Migration planner

**Description.** Implement `src/migrate/plan.rs`: given the discovered files (Issue 5) and the rows in `schema_migrations`, produce a `Plan` enumerating `Pending`, `Applied { drifted: bool }`, and `OrphanApplied` (in DB but no file on disk — a warning, not a fatal). Compute checksums (Issue 7) and detect drift per §2.2.

**Acceptance.**
- Unit tests cover every combination in the matrix.
- Orphan-applied migrations produce a warning and do not block.

**Prerequisites.** Issues 5, 7.

---

### Issue 10 — `migrate` command

**Description.** Implement `stig migrate` per §3.3 using the planner (Issue 9) and snapshot module (Issue 8). Apply each pending migration in a transaction (unless the file contains the non-transactional directive per §4), record `(version, checksum)`, and prune snapshots. `--dry-run` skips application.

**Acceptance.**
- Integration tests: fresh DB applies all pending migrations; subsequent `migrate` is a no-op; non-transactional directive is honored; drift exits 3 with the documented hint message; `--dry-run` mutates nothing.

**Prerequisites.** Issues 8, 9.

---

### Issue 11 — `status` command

**Description.** Implement `stig status` per §3.4. Reuses the planner. Output is a fixed-width table. Exits 3 when any drift is detected.

**Acceptance.**
- Integration tests cover: all-applied, pending present, drift present, orphan-applied present. Output is asserted via `insta` snapshots.

**Prerequisites.** Issue 9.

---

### Issue 12 — `redo` command

**Description.** Implement `stig redo [<version>]` per §3.5. Close connection, restore the requested snapshot, delete relevant `schema_migrations` rows, re-run the migration planner from that version forward. Prompt for confirmation unless `--yes`.

**Acceptance.**
- Integration test: apply two migrations, edit the second, `redo` (no args) restores and re-applies the edited second migration; data added after the snapshot is gone.
- `redo <version>` re-applies from that version forward.
- Missing snapshot exits 4 with the list of redo-eligible versions.

**Prerequisites.** Issue 10.

---

### Issue 13 — `reset` command

**Description.** Implement `stig reset` per §3.6. Close connection, move live DB into `resets/`, re-run `migrate`. Prompt unless `--yes`. Prune resets beyond `reset_keep`.

**Acceptance.**
- Integration test: a populated DB, after `reset --yes`, contains an empty schema_migrations + freshly-applied migrations; sidecars are moved correctly; backups dir has the reset artifact; prune respects `reset_keep`.
- Declining the prompt exits 2 without changes.

**Prerequisites.** Issues 8, 10.

---

### Issue 14 — Codegen trait and dispatcher

**Description.** Implement the trait surface in §7.6. Add a dispatcher in `src/codegen/mod.rs` that loads `[[generate]]` entries from config, instantiates the matching target, and runs it. Unknown `kind` exits 4 with the list of registered kinds.

**Acceptance.**
- Unit tests cover: zero targets, one target, multiple targets, unknown kind.
- A stub `kind = "noop"` target (for tests only) demonstrates the trait works end-to-end.

**Prerequisites.** Issue 3.

---

### Issue 15 — TypeScript codegen target

**Description.** Implement `src/codegen/typescript.rs` per §7. Introspect tables via the PRAGMAs and `sqlite_master.sql` regex. Render the `Enums`/`Tables`/`Row`/`Insert`/`Update` shape. Apply exclusions. Run the optional `format` command after writing.

**Acceptance.**
- Golden-file tests via `insta` cover: simple table, all affinities, nullable + default columns, INTEGER PRIMARY KEY rowid alias, CHECK-IN enum, reserved-word table name, exclude glob, schema_migrations excluded by default.
- `format = "deno fmt {path}"` invokes the configured formatter and ignores non-zero exit (warn only).

**Prerequisites.** Issue 14.

---

### Issue 16 — `generate` command

**Description.** Wire `stig generate [<target-name>]` per §3.7 to the dispatcher and the TypeScript target.

**Acceptance.**
- Integration test: with `[[generate]]` for the TS target, `stig generate` produces the expected file; running with a name selects only that target; unknown name exits 4.

**Prerequisites.** Issues 14, 15.

---

### Issue 17 — `backups list` and `backups prune` commands

**Description.** Implement `stig backups list` and `stig backups prune` per §3.8. Listing reads the backups dir and prints sizes + ages. Pruning applies `snapshot_keep` and `reset_keep` immediately.

**Acceptance.**
- Integration tests cover both subcommands across populated/empty states.

**Prerequisites.** Issue 8.

---

### Issue 18 — Documentation pass

**Description.** Write the README: quickstart (install, init, new, migrate, generate), full command reference (extracted from `--help`), config reference, troubleshooting (drift errors, missing snapshots, `:memory:` caveats). Cross-link to `SPEC.md` for design rationale.

**Acceptance.**
- README compiles via `cargo readme` (or equivalent) or is manually maintained with `--help` snippets verified by a CI step.
- All exit codes from §8.3 are documented.

**Prerequisites.** Issues 1, 4, 6, 10, 11, 12, 13, 16, 17.

---

### Issue 19 — Release tooling (stretch)

**Description.** Configure `cargo-dist` (or hand-write a release workflow) to build prebuilt binaries for macOS (x86_64 + aarch64) and Linux (x86_64) on tag pushes, attach them to a GitHub Release, and optionally publish to crates.io. Add a CHANGELOG. Document version policy (semver, pre-1.0 caveats).

**Acceptance.**
- A dry-run on a test tag produces the expected artifacts.
- `CHANGELOG.md` exists and is populated for v0.1.0.

**Prerequisites.** All prior issues.

---

## 12. Glossary

- **Migration.** A single `.sql` file that mutates the schema or data, identified by a timestamp and slug, applied at most once per database.
- **Apply.** To execute a migration's SQL and record the result in `schema_migrations`.
- **Drift.** A migration whose recorded checksum no longer matches its file on disk.
- **Snapshot.** A pre-migration filesystem copy of the live database, used to support `redo`.
- **Reset backup.** A filesystem copy of the live database produced by `stig reset` before recreating the schema from scratch.
- **Target.** A codegen output configured under `[[generate]]`. The MVP ships one target kind: `typescript`.
- **Pruning.** Deleting old snapshots or reset backups beyond the configured retention count.
