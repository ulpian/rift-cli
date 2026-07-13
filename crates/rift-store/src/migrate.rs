//! Additive-only, checksum-locked migration engine.
//!
//! The Turso projection is rebuildable from canonical git, so a "migration" is
//! either creating new tables/columns or bumping a version and rebuilding —
//! never a destructive in-place rewrite (`04-database-design` §4.6). Migration
//! files are append-only and **checksum-locked**: editing an already-applied
//! migration changes its SHA-256 and the runner refuses to run
//! ([`RiftError::Integrity`]). A new schema requires a *new* migration.
//!
//! An additive guard rejects `DROP TABLE` / `DROP COLUMN` / `RENAME` so a
//! destructive migration cannot land. (Statement-level `DELETE`/`UPDATE`
//! detection against canonical tables is left to a real SQL parser — **(verify)**
//! `sqlparser` — to avoid false positives on the append-only trigger bodies,
//! which legitimately contain `BEFORE UPDATE`/`DELETE`.)

use std::collections::BTreeMap;

use rift_core::{Result, RiftError, Timestamp};
use sha2::{Digest, Sha256};

use crate::schema::capture_v1_tables;
use crate::sqlite_store::SqliteStore;

/// The migration meta tables. Themselves insert-only in the runner (a re-run
/// skips on an existing version, never updates).
pub const META_SQL: &str = "\
CREATE TABLE IF NOT EXISTS _migrations (\n\
  version    INTEGER PRIMARY KEY,\n\
  name       TEXT    NOT NULL,\n\
  checksum   TEXT    NOT NULL,\n\
  applied_at TEXT    NOT NULL\n\
) STRICT;\n\
CREATE TABLE IF NOT EXISTS _schema_version (\n\
  table_name     TEXT PRIMARY KEY,\n\
  schema_version INTEGER NOT NULL\n\
) STRICT;";

/// The M1 price seed (P6 S3): a versioned starting catalog, in **micro-USD per
/// MILLION tokens**, for the models the first-party adapters report. Prices are
/// data, not code constants — this seed is simply the first append. Provenance:
/// the provider's public per-MTok pricing as of the M1 pin date (2026-07);
/// `valid_from = 0` (in force since before capture began), `recorded_at` fixed
/// at 2026-07-01T00:00:00Z for migration byte-stability. A price change or
/// correction is a NEW superseding row (`record_price` / a later migration) —
/// never an UPDATE. Models without a seeded price simply produce no cost fact
/// (honest absence beats a fabricated number).
pub(crate) const PRICE_SEED_SQL: &str = "\
INSERT INTO price_table (price_id, model, token_class, unit_price_micros, currency, valid_from, valid_to, recorded_at, supersedes_id, schema_version) VALUES
  ('m1seed-claude-sonnet-4-5-uncached_input', 'claude-sonnet-4-5', 'uncached_input', 3000000, 'USD', 0, NULL, 1782864000000, NULL, 1),
  ('m1seed-claude-sonnet-4-5-cached_input',   'claude-sonnet-4-5', 'cached_input',    300000, 'USD', 0, NULL, 1782864000000, NULL, 1),
  ('m1seed-claude-sonnet-4-5-cache_creation', 'claude-sonnet-4-5', 'cache_creation', 3750000, 'USD', 0, NULL, 1782864000000, NULL, 1),
  ('m1seed-claude-sonnet-4-5-output',         'claude-sonnet-4-5', 'output',        15000000, 'USD', 0, NULL, 1782864000000, NULL, 1),
  ('m1seed-claude-opus-4-1-uncached_input',   'claude-opus-4-1',   'uncached_input', 15000000, 'USD', 0, NULL, 1782864000000, NULL, 1),
  ('m1seed-claude-opus-4-1-cached_input',     'claude-opus-4-1',   'cached_input',    1500000, 'USD', 0, NULL, 1782864000000, NULL, 1),
  ('m1seed-claude-opus-4-1-cache_creation',   'claude-opus-4-1',   'cache_creation', 18750000, 'USD', 0, NULL, 1782864000000, NULL, 1),
  ('m1seed-claude-opus-4-1-output',           'claude-opus-4-1',   'output',         75000000, 'USD', 0, NULL, 1782864000000, NULL, 1),
  ('m1seed-claude-haiku-4-5-uncached_input',  'claude-haiku-4-5',  'uncached_input',  1000000, 'USD', 0, NULL, 1782864000000, NULL, 1),
  ('m1seed-claude-haiku-4-5-cached_input',    'claude-haiku-4-5',  'cached_input',     100000, 'USD', 0, NULL, 1782864000000, NULL, 1),
  ('m1seed-claude-haiku-4-5-cache_creation',  'claude-haiku-4-5',  'cache_creation',  1250000, 'USD', 0, NULL, 1782864000000, NULL, 1),
  ('m1seed-claude-haiku-4-5-output',          'claude-haiku-4-5',  'output',          5000000, 'USD', 0, NULL, 1782864000000, NULL, 1);
";

/// One migration: an ordered, checksum-locked unit of additive DDL.
#[derive(Debug, Clone)]
pub struct Migration {
    /// Strictly-increasing version (1, 2, 3, …).
    pub version: u32,
    /// Human-readable name.
    pub name: &'static str,
    /// The SQL applied (additive only).
    pub sql: String,
}

/// The result of a migration run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MigrationOutcome {
    /// Number of migrations applied this run.
    pub applied: usize,
    /// Number already applied (skipped).
    pub skipped: usize,
    /// The highest version present after the run.
    pub high_water: u32,
}

fn checksum_hex(sql: &str) -> String {
    let mut h = Sha256::new();
    h.update(sql.as_bytes());
    let out = h.finalize();
    let mut s = String::with_capacity(out.len() * 2);
    use core::fmt::Write as _;
    for b in out {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Strip SQL comments (`--` and `/* */`) and single-quoted string literals so
/// the additive guard sees only real tokens — a comment or literal *containing*
/// "DROP TABLE" is not destructive, and `DROP/**/TABLE` is.
fn strip_comments_and_strings(sql: &str) -> String {
    let bytes = sql.as_bytes();
    let mut out = String::with_capacity(sql.len());
    let mut i = 0;
    while let Some(&b) = bytes.get(i) {
        match b {
            b'-' if bytes.get(i + 1) == Some(&b'-') => {
                while bytes.get(i).is_some_and(|&c| c != b'\n') {
                    i += 1;
                }
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                i += 2;
                while bytes.get(i).is_some()
                    && !(bytes.get(i) == Some(&b'*') && bytes.get(i + 1) == Some(&b'/'))
                {
                    i += 1;
                }
                i = (i + 2).min(bytes.len());
                out.push(' '); // a comment is a token separator
            }
            b'\'' => {
                i += 1;
                while let Some(&c) = bytes.get(i) {
                    if c == b'\'' {
                        if bytes.get(i + 1) == Some(&b'\'') {
                            i += 2; // escaped quote inside the literal
                            continue;
                        }
                        break;
                    }
                    i += 1;
                }
                i = (i + 1).min(bytes.len());
                out.push_str("''"); // keep an empty literal as a placeholder token
            }
            c => {
                out.push(c as char);
                i += 1;
            }
        }
    }
    out
}

/// Reject destructive DDL (`DROP`/`RENAME`, incl. `ALTER TABLE … DROP <col>`
/// where SQLite makes the `COLUMN` keyword optional) and destructive DML
/// (`UPDATE`/`DELETE` statements targeting a canonical table). Token-based over
/// comment/string-stripped SQL, so it is neither fooled by `DROP/**/TABLE`
/// splits nor tripped by comments/literals mentioning `DROP TABLE`, and the
/// legitimate `BEFORE UPDATE`/`DELETE` inside append-only trigger *bodies*
/// (never statement-initial) stays clean.
///
/// # Errors
/// [`RiftError::Migration`] if the SQL contains a forbidden destructive form.
pub fn is_additive(sql: &str) -> Result<()> {
    let stripped = strip_comments_and_strings(sql).to_uppercase();

    for stmt in stripped.split(';') {
        let tokens: Vec<&str> = stmt.split_whitespace().collect();
        let mut in_alter = false;
        for (i, tok) in tokens.iter().enumerate() {
            match *tok {
                "ALTER" => in_alter = true,
                "DROP" if in_alter => {
                    return Err(RiftError::Migration(
                        "non-additive migration: ALTER … DROP".to_string(),
                    ));
                }
                "DROP" => {
                    if let Some(next) = tokens.get(i + 1) {
                        if ["TABLE", "COLUMN", "INDEX", "TRIGGER", "VIEW"].contains(next) {
                            return Err(RiftError::Migration(format!(
                                "non-additive migration: contains DROP {next}"
                            )));
                        }
                    }
                }
                "RENAME" => {
                    return Err(RiftError::Migration(
                        "non-additive migration: contains RENAME".to_string(),
                    ));
                }
                _ => {}
            }
        }

        // Destructive DML on canonical data: a statement-initial UPDATE/DELETE
        // whose target is a registered canonical table (any projection).
        let target = match tokens.first() {
            Some(&"UPDATE") => {
                // `UPDATE [OR <action>] <table>`
                match tokens.get(1) {
                    Some(&"OR") => tokens.get(3),
                    t => t,
                }
            }
            Some(&"DELETE") if tokens.get(1) == Some(&"FROM") => tokens.get(2),
            _ => None,
        };
        if let Some(t) = target {
            if crate::schema::any_registry_table(&t.to_lowercase()).is_some() {
                return Err(RiftError::Migration(format!(
                    "non-additive migration: mutates canonical table {}",
                    t.to_lowercase()
                )));
            }
        }
    }
    Ok(())
}

/// Build the canonical-schema migration SQL from the **frozen v1 prefix** of
/// the capture registry (`CAPTURE_V1_TABLE_COUNT`). Deliberately NOT the full
/// registry: this SQL is checksum-locked once applied, so it must stay
/// byte-stable as the registry grows — tables appended after the freeze ship
/// in their own additive migrations (the same contract as `recall_core` vs
/// `recall_outcome_labels`).
#[must_use]
pub fn canonical_schema_sql() -> String {
    let mut sql = String::new();
    for table in capture_v1_tables() {
        sql.push_str(&table.create_table_sql());
        sql.push('\n');
        sql.push_str(&table.guard_trigger_sql());
        sql.push('\n');
    }
    for table in capture_v1_tables() {
        sql.push_str(&format!(
            "INSERT INTO _schema_version(table_name, schema_version) VALUES ('{}', 1);\n",
            table.name
        ));
    }
    sql
}

/// Render one additive capture migration from a registry slice: DDL + guard
/// triggers + the per-table `_schema_version` seed rows.
fn capture_slice_sql(tables: &'static [crate::schema::CanonicalTable]) -> String {
    let mut sql = String::new();
    for table in tables {
        sql.push_str(&table.create_table_sql());
        sql.push('\n');
        sql.push_str(&table.guard_trigger_sql());
        sql.push('\n');
    }
    for table in tables {
        sql.push_str(&format!(
            "INSERT INTO _schema_version(table_name, schema_version) VALUES ('{}', 1);\n",
            table.name
        ));
    }
    sql
}

/// The v3 additive capture migration: the local-only identity relations
/// (`developer_identity_snapshot`, `session_binding`) from phase-04 step-03.
/// Generated from the **bounded** identity slice, so later registry growth can
/// never perturb its checksum-locked bytes.
#[must_use]
pub fn capture_identity_schema_sql() -> String {
    capture_slice_sql(crate::schema::capture_identity_tables())
}

/// The v4 additive capture migration (M1 / P4 S2): the turn-boundary relations
/// (`turn_boundary_snapshot`, `turn_boundary_file`, `turn_diff_hunk`) that make
/// H_i/A_i attribution replay possible, plus the M1 price seed (see
/// [`PRICE_SEED_SQL`]) so cost facts compute against a versioned catalog from
/// first open.
#[must_use]
pub fn capture_turn_boundary_schema_sql() -> String {
    let mut sql = capture_slice_sql(crate::schema::capture_turn_boundary_tables());
    sql.push_str(PRICE_SEED_SQL);
    sql
}

/// The v5 additive capture migration (ADR-0010): the `projection_meta`
/// key/value table carrying the rebuild epoch. Kept separate so adding it
/// never perturbs the v1–v4 checksums; an absent epoch row reads as zero.
#[must_use]
pub fn capture_projection_meta_schema_sql() -> String {
    capture_slice_sql(crate::schema::capture_projection_meta_tables())
}

/// The v6 additive capture migration (M6-12): the `file_read_tokens` table
/// holding read-time token measurements (keyed by the read's tool call, so a
/// fact and its figure land together). Kept separate so adding it never
/// perturbs the v1–v5 checksums; a read without a row simply has no estimate
/// (the recall ingest falls back or records 0).
#[must_use]
pub fn capture_read_tokens_schema_sql() -> String {
    capture_slice_sql(crate::schema::capture_read_tokens_tables())
}

/// The default migration set: meta tables, the canonical schema, the identity
/// relations, the turn-boundary relations (+ price seed), the projection-meta
/// table (the ADR-0010 rebuild epoch), then the read-token estimates (M6-12).
#[must_use]
pub fn default_migrations() -> Vec<Migration> {
    vec![
        Migration {
            version: 1,
            name: "init_meta",
            sql: META_SQL.to_string(),
        },
        Migration {
            version: 2,
            name: "canonical_core",
            sql: canonical_schema_sql(),
        },
        Migration {
            version: 3,
            name: "capture_identity",
            sql: capture_identity_schema_sql(),
        },
        Migration {
            version: 4,
            name: "capture_turn_boundary",
            sql: capture_turn_boundary_schema_sql(),
        },
        Migration {
            version: 5,
            name: "capture_projection_meta",
            sql: capture_projection_meta_schema_sql(),
        },
        Migration {
            version: 6,
            name: "capture_read_tokens",
            sql: capture_read_tokens_schema_sql(),
        },
    ]
}

/// Build the recall-schema migration SQL from the recall table registry (the
/// Stage 3 `recall.db` graph) plus its secondary indexes. Same shape as
/// [`canonical_schema_sql`], over [`crate::schema::recall_canonical_tables`].
#[must_use]
pub fn recall_canonical_schema_sql() -> String {
    use crate::schema::{recall_v1_tables, RECALL_INDEX_SQL};
    let mut sql = String::new();
    for table in recall_v1_tables() {
        sql.push_str(&table.create_table_sql());
        sql.push('\n');
        sql.push_str(&table.guard_trigger_sql());
        sql.push('\n');
    }
    sql.push_str(RECALL_INDEX_SQL);
    for table in recall_v1_tables() {
        sql.push_str(&format!(
            "INSERT INTO _schema_version(table_name, schema_version) VALUES ('{}', 1);\n",
            table.name
        ));
    }
    sql
}

/// The v2 additive recall migration (P4 S2): the bitemporal `outcome_label`
/// store + its subject index. Kept separate from `recall_core` so adding it
/// never perturbs `recall_core`'s checksum (additive-only, checksum-locked).
#[must_use]
pub fn recall_outcome_label_schema_sql() -> String {
    use crate::schema::{recall_v2_outcome_tables, RECALL_OUTCOME_INDEX_SQL};
    let mut sql = String::new();
    for table in recall_v2_outcome_tables() {
        sql.push_str(&table.create_table_sql());
        sql.push('\n');
        sql.push_str(&table.guard_trigger_sql());
        sql.push('\n');
    }
    sql.push_str(RECALL_OUTCOME_INDEX_SQL);
    for table in recall_v2_outcome_tables() {
        sql.push_str(&format!(
            "INSERT INTO _schema_version(table_name, schema_version) VALUES ('{}', 1);\n",
            table.name
        ));
    }
    sql
}

/// The v3 additive recall migration: the `key_metadata` custody table, so
/// `recall.db` carries its own wrapped-DEK rows for Layer-2 column encryption
/// ([`crate::KeyringKeyStore::provision_repo_dek`]).
#[must_use]
pub fn recall_key_custody_schema_sql() -> String {
    use crate::schema::recall_v3_custody_tables;
    let mut sql = String::new();
    for table in recall_v3_custody_tables() {
        sql.push_str(&table.create_table_sql());
        sql.push('\n');
        sql.push_str(&table.guard_trigger_sql());
        sql.push('\n');
    }
    for table in recall_v3_custody_tables() {
        sql.push_str(&format!(
            "INSERT INTO _schema_version(table_name, schema_version) VALUES ('{}', 1);\n",
            table.name
        ));
    }
    sql
}

/// The v4 additive recall migration: the append-only `memd_audit_event` serve
/// trail (the `rift hooks memd` verb writes here). Kept separate so adding it
/// never perturbs the v1–v3 checksums.
#[must_use]
pub fn recall_memd_audit_schema_sql() -> String {
    use crate::schema::recall_v4_audit_tables;
    let mut sql = String::new();
    for table in recall_v4_audit_tables() {
        sql.push_str(&table.create_table_sql());
        sql.push('\n');
        sql.push_str(&table.guard_trigger_sql());
        sql.push('\n');
    }
    for table in recall_v4_audit_tables() {
        sql.push_str(&format!(
            "INSERT INTO _schema_version(table_name, schema_version) VALUES ('{}', 1);\n",
            table.name
        ));
    }
    sql
}

/// The v5 additive recall migration: the ADR-0005 `n_blob_oid_map` content→blob
/// bridge (the worktree hasher populates it; the substrate/exact resolvers
/// consume it). Kept separate so adding it never perturbs the v1–v4 checksums.
#[must_use]
pub fn recall_blob_oid_map_schema_sql() -> String {
    use crate::schema::recall_v5_blob_map_tables;
    let mut sql = String::new();
    for table in recall_v5_blob_map_tables() {
        sql.push_str(&table.create_table_sql());
        sql.push('\n');
        sql.push_str(&table.guard_trigger_sql());
        sql.push('\n');
    }
    for table in recall_v5_blob_map_tables() {
        sql.push_str(&format!(
            "INSERT INTO _schema_version(table_name, schema_version) VALUES ('{}', 1);\n",
            table.name
        ));
    }
    sql
}

/// The v6 additive recall migration: the durable `ingest_watermark` relation the
/// recall ingest path appends to per run (incremental-ingest evidence). Kept
/// separate so adding it never perturbs the v1–v5 checksums.
#[must_use]
pub fn recall_ingest_watermark_schema_sql() -> String {
    use crate::schema::recall_v6_watermark_tables;
    let mut sql = String::new();
    for table in recall_v6_watermark_tables() {
        sql.push_str(&table.create_table_sql());
        sql.push('\n');
        sql.push_str(&table.guard_trigger_sql());
        sql.push('\n');
    }
    for table in recall_v6_watermark_tables() {
        sql.push_str(&format!(
            "INSERT INTO _schema_version(table_name, schema_version) VALUES ('{}', 1);\n",
            table.name
        ));
    }
    sql
}

/// The recall projection's migration set (applied to a fresh `recall.db`).
/// [`apply_migrations`] creates the meta tables itself, so version 1 is the
/// recall graph schema directly; version 2 adds the outcome-label store;
/// version 3 adds the key-custody table; version 4 adds the memd serve-audit
/// relation; version 5 adds the content→blob-oid map; version 6 adds the
/// durable ingest watermark.
#[must_use]
pub fn recall_migrations() -> Vec<Migration> {
    vec![
        Migration {
            version: 1,
            name: "recall_core",
            sql: recall_canonical_schema_sql(),
        },
        Migration {
            version: 2,
            name: "recall_outcome_labels",
            sql: recall_outcome_label_schema_sql(),
        },
        Migration {
            version: 3,
            name: "recall_key_custody",
            sql: recall_key_custody_schema_sql(),
        },
        Migration {
            version: 4,
            name: "recall_memd_audit",
            sql: recall_memd_audit_schema_sql(),
        },
        Migration {
            version: 5,
            name: "recall_blob_oid_map",
            sql: recall_blob_oid_map_schema_sql(),
        },
        Migration {
            version: 6,
            name: "recall_ingest_watermark",
            sql: recall_ingest_watermark_schema_sql(),
        },
    ]
}

/// Build the canopy schema SQL for the registry tables named in `names` (in
/// registry order): create + append-only guard per table, then a
/// `_schema_version` row per table. Splitting by name lets each migration
/// **version** own a frozen, checksum-stable subset (a new table is a new
/// version, never an edit to an applied one — the frozen-prefix rule).
fn canopy_schema_sql_for(names: &[&str]) -> String {
    use crate::schema::canopy_canonical_tables;
    let selected = || {
        canopy_canonical_tables()
            .iter()
            .filter(|t| names.contains(&t.name))
    };
    let mut sql = String::new();
    for table in selected() {
        sql.push_str(&table.create_table_sql());
        sql.push('\n');
        sql.push_str(&table.guard_trigger_sql());
        sql.push('\n');
    }
    for table in selected() {
        sql.push_str(&format!(
            "INSERT INTO _schema_version(table_name, schema_version) VALUES ('{}', 1);\n",
            table.name
        ));
    }
    sql
}

/// The full canopy UI-state schema SQL (every table, all versions) — for the
/// additive-migration check and the DDL snapshot. Not a single migration (the
/// layout table arrived in v2); see [`canopy_migrations`].
#[must_use]
pub fn canopy_canonical_schema_sql() -> String {
    use crate::schema::canopy_canonical_tables;
    let all: Vec<&str> = canopy_canonical_tables().iter().map(|t| t.name).collect();
    canopy_schema_sql_for(&all)
}

/// The canopy UI-state projection's migration set (applied to a fresh or an
/// existing `ui-state.db`). v1 is the frozen project + toggle core (M4/M5); v2
/// adds the per-project layout table (ADR-0035); v3 adds the profile plane
/// (ADR-0036); v4 adds the per-(profile, slot) theme table (ADR-0038) — each a
/// new version so an existing store upgrades additively and the applied
/// checksums stay locked.
#[must_use]
pub fn canopy_migrations() -> Vec<Migration> {
    vec![
        Migration {
            version: 1,
            name: "canopy_core",
            sql: canopy_schema_sql_for(&["canopy_project_event", "canopy_toggle_event"]),
        },
        Migration {
            version: 2,
            name: "canopy_layout",
            sql: canopy_schema_sql_for(&["canopy_layout_event"]),
        },
        Migration {
            version: 3,
            name: "canopy_profiles",
            sql: canopy_schema_sql_for(&[
                "canopy_profile_event",
                "canopy_profile_project_event",
                "canopy_profile_layout_event",
            ]),
        },
        Migration {
            version: 4,
            name: "canopy_profile_theme",
            sql: canopy_schema_sql_for(&["canopy_profile_theme_event"]),
        },
    ]
}

/// Open (creating if needed) a projection DB at `path` and bring it to the
/// current schema through the checksum-locked migration engine. This is the
/// **only** production way to open a projection — it populates `_migrations`
/// (the tamper lock) and `_schema_version`, which the registry fast-path
/// [`SqliteStore::create_canonical_schema`] (tests only) does not.
///
/// # Errors
/// * [`RiftError::Io`] if the parent directory cannot be created.
/// * [`RiftError::Storage`] if the database cannot be opened.
/// * Any [`apply_migrations`] error (checksum tamper, gap, DDL failure).
pub fn open_migrated(
    path: impl AsRef<std::path::Path>,
    migrations: &[Migration],
) -> Result<SqliteStore> {
    let p = path.as_ref();
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let store = SqliteStore::open(p)?;
    // Owner-only dir + file (the step-05 local-disk-reader mitigation).
    crate::paths::harden_projection_permissions(p)?;
    apply_migrations(&store, migrations)?;
    Ok(store)
}

/// [`open_migrated`] with the capture projection's migration set
/// ([`default_migrations`]) — the standard way to open `projection.db`.
///
/// # Errors
/// See [`open_migrated`].
pub fn open_capture_projection(path: impl AsRef<std::path::Path>) -> Result<SqliteStore> {
    open_migrated(path, &default_migrations())
}

/// The pure migration planner shared by the sync (reference-engine) and
/// seam-generic runners: validates additivity, verifies checksums of already
/// applied versions (the tamper lock), enforces gap-free ordering, and returns
/// the migrations still to apply plus the skipped count.
fn plan_migrations<'m>(
    applied: &BTreeMap<u32, String>,
    migs: &'m [Migration],
) -> Result<(Vec<&'m Migration>, usize)> {
    let mut high_water = applied.keys().copied().max().unwrap_or(0);
    let mut ordered: Vec<&Migration> = migs.iter().collect();
    ordered.sort_by_key(|m| m.version);

    let mut todo = Vec::new();
    let mut skipped = 0;
    for m in ordered {
        is_additive(&m.sql)?;
        let cs = checksum_hex(&m.sql);
        if let Some(prev) = applied.get(&m.version) {
            if prev != &cs {
                return Err(RiftError::integrity(format!(
                    "migration {} checksum changed since it was applied",
                    m.version
                )));
            }
            skipped += 1;
            continue;
        }
        if m.version != high_water + 1 {
            return Err(RiftError::Migration(format!(
                "migration {} out of order or gap (expected {})",
                m.version,
                high_water + 1
            )));
        }
        high_water = m.version;
        todo.push(m);
    }
    Ok((todo, skipped))
}

/// Apply pending migrations in order, idempotently and atomically (the
/// synchronous reference-engine runner; engine-generic callers use
/// [`apply_migrations_on`], which shares the same planner).
///
/// # Errors
/// * [`RiftError::Integrity`] if an already-applied migration's checksum changed.
/// * [`RiftError::Migration`] on a non-additive migration, a version gap, or a
///   DDL failure.
pub fn apply_migrations(store: &SqliteStore, migs: &[Migration]) -> Result<MigrationOutcome> {
    store.ensure_meta()?;
    let applied: BTreeMap<u32, String> = store.applied_migrations()?.into_iter().collect();
    let (todo, skipped) = plan_migrations(&applied, migs)?;
    let mut high_water = applied.keys().copied().max().unwrap_or(0);
    let applied_count = todo.len();
    for m in todo {
        store.apply_migration(
            &m.sql,
            m.version,
            m.name,
            &checksum_hex(&m.sql),
            &Timestamp::now().to_string(),
        )?;
        high_water = m.version;
    }
    Ok(MigrationOutcome {
        applied: applied_count,
        skipped,
        high_water,
    })
}

/// [`apply_migrations`] through the engine-agnostic [`Store`] seam: meta-table
/// bootstrap, checksum verification, and per-migration atomicity (DDL +
/// `_migrations` row inside one [`crate::StoreTx`]) all run over
/// `StoreConn`/`StoreTx`, so a future engine (Turso) migrates without touching
/// this logic.
///
/// # Errors
/// As [`apply_migrations`].
pub async fn apply_migrations_on(
    store: &impl crate::Store,
    migs: &[Migration],
) -> Result<MigrationOutcome> {
    let conn = store.conn().await?;
    conn.execute_batch(META_SQL).await?;
    let rows = conn
        .query(
            "SELECT version, checksum FROM _migrations ORDER BY version",
            &[],
        )
        .await?;
    let mut applied: BTreeMap<u32, String> = BTreeMap::new();
    for r in &rows {
        applied.insert(u32::try_from(r.get_i64(0)?).unwrap_or(0), r.get_text(1)?);
    }
    let (todo, skipped) = plan_migrations(&applied, migs)?;
    let mut high_water = applied.keys().copied().max().unwrap_or(0);
    let applied_count = todo.len();
    for m in todo {
        let tx = conn.begin(crate::TxMode::Deferred).await?;
        tx.execute_batch(&m.sql).await?;
        tx.execute(
            "INSERT INTO _migrations(version, name, checksum, applied_at) VALUES (?, ?, ?, ?)",
            &[
                crate::Value::Int(i64::from(m.version)),
                crate::Value::text(m.name),
                crate::Value::text(checksum_hex(&m.sql)),
                crate::Value::text(Timestamp::now().to_string()),
            ],
        )
        .await?;
        tx.commit().await?;
        high_water = m.version;
    }
    Ok(MigrationOutcome {
        applied: applied_count,
        skipped,
        high_water,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CanonicalStore, Value};

    fn store() -> SqliteStore {
        SqliteStore::open_in_memory().unwrap()
    }

    #[tokio::test]
    async fn applies_pending_migrations_in_order() {
        let s = store();
        let out = apply_migrations(&s, &default_migrations()).unwrap();
        assert_eq!(out.applied, 6);
        assert_eq!(out.high_water, 6);
        // canonical tables exist (insert succeeds)
        s.insert(
            "session",
            &[
                "session_id",
                "agent",
                "model_primary",
                "repo_id",
                "started_at",
                "schema_version",
            ],
            &[
                Value::text("sess_1"),
                Value::text("a"),
                Value::text("m"),
                Value::text("repo_1"),
                Value::Int(1),
                Value::Int(1),
            ],
        )
        .await
        .unwrap();
    }

    /// The seam-generic runner is equivalent to the reference runner: applies
    /// the same set, is idempotent across BOTH runners (shared checksum lock),
    /// and refuses a tampered migration.
    #[tokio::test]
    async fn seam_runner_matches_reference_runner() {
        let s = store();
        let out = apply_migrations_on(&s, &default_migrations())
            .await
            .unwrap();
        assert_eq!(out.applied, 6);
        assert_eq!(out.high_water, 6);
        // The sync runner sees everything applied (same _migrations ledger).
        let again = apply_migrations(&s, &default_migrations()).unwrap();
        assert_eq!(again.applied, 0);
        assert_eq!(again.skipped, 6);
        // And the tamper lock holds through the seam.
        let mut edited = default_migrations();
        edited[1].sql.push_str("\n-- tampered");
        assert!(matches!(
            apply_migrations_on(&s, &edited).await,
            Err(RiftError::Integrity(_))
        ));
        // A failed migration mid-run leaves no partial schema (tx rollback).
        let bad = vec![Migration {
            version: 5,
            name: "bad_ddl",
            sql: "CREATE TABLE IF NOT EXISTS half (x INTEGER PRIMARY KEY) STRICT;\n\
                  THIS IS NOT SQL;"
                .to_string(),
        }];
        assert!(apply_migrations_on(&s, &bad).await.is_err());
        let tables = s.table_names().unwrap();
        assert!(
            !tables.contains(&"half".to_string()),
            "failed migration must roll back its DDL"
        );
    }

    proptest::proptest! {
        /// The step-02 idempotency invariant: applying any PREFIX of the
        /// shipping migration set and then the FULL set converges on the same
        /// schema as one full apply, with prefix members skipped (checksums
        /// verified), never re-applied.
        #[test]
        fn apply_then_apply_is_noop(split in 0usize..=5) {
            let migs = default_migrations();
            let s = store();
            let first = apply_migrations(&s, &migs[..split]).unwrap();
            proptest::prop_assert_eq!(first.applied, split);
            let rest = apply_migrations(&s, &migs).unwrap();
            proptest::prop_assert_eq!(rest.skipped, split);
            proptest::prop_assert_eq!(rest.applied, migs.len() - split);
            proptest::prop_assert_eq!(rest.high_water, 6);

            let fresh = store();
            apply_migrations(&fresh, &migs).unwrap();
            proptest::prop_assert_eq!(s.table_names().unwrap(), fresh.table_names().unwrap());
            proptest::prop_assert_eq!(s.trigger_names().unwrap(), fresh.trigger_names().unwrap());
        }
    }

    #[test]
    fn rerun_is_idempotent() {
        let s = store();
        apply_migrations(&s, &default_migrations()).unwrap();
        let out = apply_migrations(&s, &default_migrations()).unwrap();
        assert_eq!(out.applied, 0);
        assert_eq!(out.skipped, 6);
    }

    /// The additive contract for the recall projection: `recall_core` (v1) must
    /// stay byte-stable as the registry grows (its checksum is locked), so the
    /// `outcome_label` table lives in the v2 migration and never leaks into v1.
    #[test]
    fn recall_core_excludes_v2_outcome_label() {
        let migs = recall_migrations();
        assert_eq!(migs.len(), 6);
        assert_eq!(migs[0].name, "recall_core");
        assert_eq!(migs[1].name, "recall_outcome_labels");
        assert_eq!(migs[2].name, "recall_key_custody");
        assert_eq!(migs[3].name, "recall_memd_audit");
        assert_eq!(migs[4].name, "recall_blob_oid_map");
        assert_eq!(migs[5].name, "recall_ingest_watermark");
        // v1 must NOT mention outcome_label (else its checksum would have moved
        // when the table was added to the registry).
        assert!(
            !migs[0].sql.contains("outcome_label"),
            "recall_core must not contain outcome_label (additive checksum lock)"
        );
        // v2 creates exactly the outcome_label table + its guards + index —
        // and must NOT have absorbed the v3 custody table (checksum lock).
        assert!(migs[1]
            .sql
            .contains("CREATE TABLE IF NOT EXISTS outcome_label"));
        assert!(migs[1].sql.contains("outcome_label_no_update"));
        assert!(migs[1].sql.contains("idx_outcome_subject"));
        assert!(!migs[1].sql.contains("key_metadata"));
        // v3 creates exactly the custody table + its guards — and must NOT have
        // absorbed the v4 audit table (checksum lock for existing v3 recall.dbs).
        assert!(migs[2]
            .sql
            .contains("CREATE TABLE IF NOT EXISTS key_metadata"));
        assert!(migs[2].sql.contains("key_metadata_no_update"));
        assert!(!migs[2].sql.contains("memd_audit_event"));
        // v4 creates exactly the memd serve-audit table + its guards — and must
        // NOT have absorbed the v5 blob-oid map (checksum lock).
        assert!(migs[3]
            .sql
            .contains("CREATE TABLE IF NOT EXISTS memd_audit_event"));
        assert!(migs[3].sql.contains("memd_audit_event_no_update"));
        assert!(!migs[3].sql.contains("n_blob_oid_map"));
        // v5 creates exactly the content→blob-oid map + its guards — and must
        // NOT have absorbed the v6 watermark table (checksum lock for existing
        // v5 recall.dbs).
        assert!(migs[4]
            .sql
            .contains("CREATE TABLE IF NOT EXISTS n_blob_oid_map"));
        assert!(migs[4].sql.contains("n_blob_oid_map_no_update"));
        assert!(!migs[4].sql.contains("ingest_watermark"));
        // v6 creates exactly the durable ingest watermark + its guards.
        assert!(migs[5]
            .sql
            .contains("CREATE TABLE IF NOT EXISTS ingest_watermark"));
        assert!(migs[5].sql.contains("ingest_watermark_no_update"));
    }

    /// Applying v1 then v2 to a fresh recall.db is a clean upgrade (no checksum
    /// conflict), and re-running both is idempotent.
    #[test]
    fn recall_migrations_apply_and_rerun_cleanly() {
        let s = store();
        let out = apply_migrations(&s, &recall_migrations()).unwrap();
        assert_eq!(out.applied, 6);
        assert_eq!(out.high_water, 6);
        let again = apply_migrations(&s, &recall_migrations()).unwrap();
        assert_eq!(again.applied, 0);
        assert_eq!(again.skipped, 6);
    }

    /// The canopy UI-state migrations create the append-only tables (v1 core +
    /// v2 layout + v3 profiles) and re-run idempotently (a fresh `ui-state.db`
    /// upgrade).
    #[test]
    fn canopy_migrations_apply_and_rerun_cleanly() {
        let s = store();
        let out = apply_migrations(&s, &canopy_migrations()).unwrap();
        assert_eq!(
            out.applied, 4,
            "v1 core + v2 layout + v3 profiles + v4 theme"
        );
        assert_eq!(out.high_water, 4);
        let v1 = &canopy_migrations()[0].sql;
        assert!(v1.contains("CREATE TABLE IF NOT EXISTS canopy_project_event"));
        assert!(v1.contains("CREATE TABLE IF NOT EXISTS canopy_toggle_event"));
        assert!(v1.contains("canopy_project_event_no_update"));
        assert!(v1.contains("canopy_toggle_event_no_delete"));
        // v1 stays frozen (no layout table — the checksum lock holds for existing
        // stores); v2 adds the layout table with its append-only guards.
        assert!(!v1.contains("canopy_layout_event"), "v1 is the frozen core");
        let v2 = &canopy_migrations()[1].sql;
        assert!(v2.contains("CREATE TABLE IF NOT EXISTS canopy_layout_event"));
        assert!(v2.contains("canopy_layout_event_no_update"));
        assert!(v2.contains("canopy_layout_event_no_delete"));
        // v2 stays frozen too (no profile tables); v3 adds the profile plane
        // (ADR-0036) with its append-only guards.
        assert!(
            !v2.contains("canopy_profile"),
            "v2 is the frozen layout slice"
        );
        let v3 = &canopy_migrations()[2].sql;
        assert!(v3.contains("CREATE TABLE IF NOT EXISTS canopy_profile_event"));
        assert!(v3.contains("CREATE TABLE IF NOT EXISTS canopy_profile_project_event"));
        assert!(v3.contains("CREATE TABLE IF NOT EXISTS canopy_profile_layout_event"));
        assert!(v3.contains("canopy_profile_event_no_update"));
        assert!(v3.contains("canopy_profile_project_event_no_delete"));
        assert!(v3.contains("canopy_profile_layout_event_no_update"));
        // v3 stays frozen (no theme table); v4 adds it (ADR-0038) with guards.
        assert!(!v3.contains("canopy_profile_theme_event"), "v3 is frozen");
        let v4 = &canopy_migrations()[3].sql;
        assert!(v4.contains("CREATE TABLE IF NOT EXISTS canopy_profile_theme_event"));
        assert!(v4.contains("canopy_profile_theme_event_no_update"));
        assert!(v4.contains("canopy_profile_theme_event_no_delete"));
        let again = apply_migrations(&s, &canopy_migrations()).unwrap();
        assert_eq!(again.applied, 0);
        assert_eq!(again.skipped, 4);
    }

    /// Every canopy migration is additive (the destructive-DDL CI gate).
    #[test]
    fn every_canopy_migration_is_additive() {
        for m in canopy_migrations() {
            is_additive(&m.sql).unwrap();
        }
    }

    #[test]
    fn edited_applied_migration_is_refused() {
        let s = store();
        let migs = vec![Migration {
            version: 1,
            name: "m1",
            sql: META_SQL.to_string(),
        }];
        apply_migrations(&s, &migs).unwrap();
        let edited = vec![Migration {
            version: 1,
            name: "m1",
            sql: format!("{META_SQL}\n-- tampered comment"),
        }];
        let err = apply_migrations(&s, &edited);
        assert!(matches!(err, Err(RiftError::Integrity(_))), "got {err:?}");
    }

    #[test]
    fn out_of_order_or_gap_version_is_rejected() {
        let s = store();
        let migs = vec![
            Migration {
                version: 1,
                name: "m1",
                sql: META_SQL.to_string(),
            },
            Migration {
                version: 4,
                name: "m4",
                sql: "CREATE TABLE IF NOT EXISTS extra (x INTEGER PRIMARY KEY) STRICT;".to_string(),
            },
        ];
        let err = apply_migrations(&s, &migs);
        assert!(matches!(err, Err(RiftError::Migration(_))), "got {err:?}");
    }

    #[test]
    fn additive_check_rejects_drop_table() {
        assert!(is_additive("CREATE TABLE x (a INTEGER PRIMARY KEY) STRICT;").is_ok());
        assert!(is_additive("DROP TABLE checkpoint;").is_err());
        assert!(is_additive("ALTER TABLE x RENAME TO y;").is_err());
        // a legitimate append-only trigger body must NOT be flagged
        assert!(is_additive(&canonical_schema_sql()).is_ok());
    }

    /// The guard is token-based, not a substring scan: it is neither fooled by
    /// comment/whitespace splits nor tripped by comments/literals that merely
    /// mention destructive SQL, and it catches statement-level UPDATE/DELETE
    /// against canonical tables (in any of the three registries).
    #[test]
    fn additive_check_is_not_a_substring_scan() {
        // false negatives under the old scan — all destructive, all rejected:
        assert!(is_additive("DROP/**/TABLE checkpoint;").is_err());
        assert!(is_additive("DROP\nTABLE checkpoint;").is_err());
        assert!(is_additive("ALTER TABLE session DROP started_at;").is_err()); // COLUMN optional
        assert!(is_additive("UPDATE session SET agent = 'x';").is_err());
        assert!(is_additive("UPDATE OR ABORT session SET agent = 'x';").is_err());
        assert!(is_additive("DELETE FROM checkpoint;").is_err());
        assert!(is_additive("DELETE FROM outcome_label;").is_err()); // recall registry
        assert!(is_additive("DROP INDEX idx_node_kind;").is_err());

        // false positives under the old scan — all additive, all accepted:
        assert!(is_additive("-- do not DROP TABLE anything\nCREATE TABLE IF NOT EXISTS z (a INTEGER PRIMARY KEY) STRICT;").is_ok());
        assert!(is_additive("/* RENAME TO is forbidden */ CREATE TABLE IF NOT EXISTS z (a INTEGER PRIMARY KEY) STRICT;").is_ok());
        assert!(is_additive("INSERT INTO note(body) VALUES ('how to DROP TABLE safely');").is_ok());
        // trigger bodies legitimately contain BEFORE UPDATE/DELETE — never
        // statement-initial, so they stay clean:
        assert!(is_additive(&recall_canonical_schema_sql()).is_ok());
        assert!(is_additive(&canopy_canonical_schema_sql()).is_ok());
        assert!(is_additive(&capture_identity_schema_sql()).is_ok());
        assert!(is_additive(&capture_turn_boundary_schema_sql()).is_ok());
    }

    /// The destructive-DDL CI gate, made explicit and order-independent: every
    /// migration in the **shipping** set must be additive. A future migration that
    /// sneaks a `DROP`/`RENAME` in fails here, not only when it happens to apply.
    #[test]
    fn every_default_migration_is_additive() {
        for m in default_migrations() {
            is_additive(&m.sql).unwrap_or_else(|e| {
                panic!(
                    "shipping migration {} ({}) is non-additive: {e}",
                    m.version, m.name
                )
            });
        }
    }

    /// The additive contract for the capture projection, byte-exact: the
    /// `canonical_core` (v2) migration SQL is checksum-locked on every existing
    /// `projection.db`, so it must never change. This pins the generated bytes:
    /// if it fails you either (a) appended a new table into the frozen v1 prefix
    /// — move it past `CAPTURE_V1_TABLE_COUNT` and ship it in a NEW migration —
    /// or (b) edited a v1 table's columns/triggers, which is a breaking change
    /// for every deployed projection and needs an additive `ALTER TABLE`
    /// migration instead.
    #[test]
    fn capture_core_v1_sql_is_byte_stable() {
        assert_eq!(
            checksum_hex(&canonical_schema_sql()),
            "1dc6a86bd030cda355dd6998fd97b84a12b0f2b8d5eafde03d36bee8618ccc09",
            "canonical_core migration SQL changed — see this test's doc comment"
        );
    }

    /// Same contract for the shipped additive migrations: once a version has
    /// been applied anywhere, its generated bytes are frozen. Registry growth
    /// belongs in a NEW slice + migration (see `capture_registry_growth_…`).
    #[test]
    fn shipped_capture_migrations_are_byte_stable() {
        assert_eq!(
            checksum_hex(&capture_identity_schema_sql()),
            "189e0ec139713ae08f93fa6a8224375d60105354b14536432dbb173b7514ff33",
            "capture_identity (v3) migration SQL changed — frozen once shipped"
        );
        assert_eq!(
            checksum_hex(&capture_turn_boundary_schema_sql()),
            "ac8b1ac35ba0a943b5a4bc7fcbf285e4d321214d091cce10b91ea7b1a886d243",
            "capture_turn_boundary (v4) migration SQL changed — frozen once shipped"
        );
    }

    /// Every capture table lands in exactly one migration slice, and the frozen
    /// boundaries hold: growing or reordering a shipped slice would change its
    /// checksum-locked SQL on every deployed projection.
    #[test]
    fn capture_registry_growth_lands_past_frozen_boundaries() {
        use crate::schema::{
            canonical_tables, capture_identity_tables, capture_projection_meta_tables,
            capture_read_tokens_tables, capture_turn_boundary_tables,
        };
        assert_eq!(
            capture_v1_tables().len()
                + capture_identity_tables().len()
                + capture_turn_boundary_tables().len()
                + capture_projection_meta_tables().len()
                + capture_read_tokens_tables().len(),
            canonical_tables().len(),
            "a new capture table must extend a NEW slice (and migration), not fall between"
        );
        assert_eq!(
            capture_v1_tables().last().map(|t| t.name),
            Some("key_metadata"),
            "the v1 freeze boundary moved — canonical_core's checksum will break"
        );
        assert_eq!(
            capture_identity_tables().last().map(|t| t.name),
            Some("session_binding"),
            "the v3 boundary moved — capture_identity's checksum will break"
        );
        assert_eq!(
            capture_turn_boundary_tables().last().map(|t| t.name),
            Some("turn_diff_hunk"),
            "the v4 boundary moved — capture_turn_boundary's checksum will break"
        );
        assert_eq!(
            capture_projection_meta_tables().last().map(|t| t.name),
            Some("projection_meta"),
            "the v5 boundary moved — capture_projection_meta's checksum will break"
        );
    }

    /// The real-world upgrade path: a projection created by the legacy
    /// registry fast-path (`create_canonical_schema`, no `_migrations` ledger)
    /// is adopted cleanly by [`open_capture_projection`] — the migration run
    /// no-ops on the existing tables and back-fills the meta tables.
    #[test]
    fn legacy_projection_is_adopted_by_open_capture_projection() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("projection.db");
        {
            let legacy = SqliteStore::open(&db).unwrap();
            legacy.create_canonical_schema().unwrap();
        }
        let adopted = open_capture_projection(&db).unwrap();
        let versions: i64 = adopted
            .with_conn(|c| c.query_row("SELECT COUNT(*) FROM _schema_version", [], |r| r.get(0)))
            .unwrap();
        assert_eq!(
            versions,
            i64::try_from(crate::schema::canonical_tables().len()).unwrap()
        );
        // and a second open is a pure no-op (checksums verified, nothing applied)
        let again = open_capture_projection(&db).unwrap();
        let migs: i64 = again
            .with_conn(|c| c.query_row("SELECT COUNT(*) FROM _migrations", [], |r| r.get(0)))
            .unwrap();
        assert_eq!(migs, 6);
    }

    /// The step-05 filesystem control: a projection opened through the standard
    /// path is owner-only (0o700 dir / 0o600 file) — a local disk reader in
    /// another account cannot read the unencrypted columns.
    #[cfg(unix)]
    #[test]
    fn opened_projection_is_owner_only() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("repo-x").join("projection.db");
        let _store = open_capture_projection(&db).unwrap();
        let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&db), 0o600, "db file must be owner-only");
        assert_eq!(
            mode(db.parent().unwrap()),
            0o700,
            "projection dir must be owner-only"
        );
    }

    /// Pins that every table gets its `_schema_version` seed row. NOTE the
    /// honest limitation: version *bumps* are not yet representable — the row's
    /// PK is `table_name` and meta tables are insert-only, so the bump
    /// mechanism (a superseding-row shape with PK `(table_name, version)`)
    /// must ride the first additive-column migration as a NEW meta migration;
    /// `META_SQL` itself is checksum-locked on every deployed projection and
    /// cannot be edited (see ERRATA).
    #[test]
    fn per_table_schema_version_is_tracked() {
        let s = store();
        apply_migrations(&s, &default_migrations()).unwrap();
        let n: i64 = s
            .with_conn(|c| {
                c.query_row(
                    "SELECT COUNT(*) FROM _schema_version WHERE schema_version = 1",
                    [],
                    |r| r.get(0),
                )
            })
            .unwrap();
        assert!(n >= 1, "expected per-table schema_version rows, got {n}");
    }
}
