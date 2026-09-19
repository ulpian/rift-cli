//! Additive-only, checksum-locked migration engine.
//!
//! The Turso projection is rebuildable from canonical git, so a "migration" is
//! either creating new tables/columns or bumping a version and rebuilding —
//! never a destructive in-place rewrite. Migration files are append-only and
//! **checksum-locked**: editing an already-applied migration changes its
//! SHA-256 and the runner refuses to run
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

/// The original price seed: a versioned starting catalog, in **micro-USD per
/// MILLION tokens**, for the models the first-party adapters report. Prices are
/// data, not code constants — this seed is simply the first append. Provenance:
/// the provider's public per-MTok pricing as of the 2026-07 pin date;
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

/// The current-model price append: the models this toolchain actually
/// runs. The original seed predates them — a measurement found **zero** intersection
/// between it (sonnet-4-5, opus-4-1, haiku-4-5) and the observed set below, so
/// without this append `effective_prices` returns `None` for every live model
/// and the savings estimate correctly renders nothing at all.
///
/// Provenance: the provider's published per-MTok pricing, transcribed at
/// authoring time (2026-07-20) — never from model memory. Cached-read is 0.1×
/// input and cache-creation 1.25× input (the 5-minute-TTL write premium): the
/// same derivation the original seed above uses.
///
/// `claude-sonnet-5` carries two **valid-time** epochs: its introductory rate
/// holds through 2026-08-31 and the standard rate takes over 2026-09-01. This
/// is what the bitemporal relation is for — `effective_class_price` filters on
/// `valid_to`, so the figure stays correct across the changeover with no
/// further migration. `recorded_at` is fixed at 2026-07-20T00:00:00Z for
/// migration byte-stability. A later correction is a NEW superseding row —
/// never an UPDATE of these.
pub(crate) const PRICE_SEED_CURRENT_SQL: &str = "\
INSERT INTO price_table (price_id, model, token_class, unit_price_micros, currency, valid_from, valid_to, recorded_at, supersedes_id, schema_version) VALUES
  ('m515seed-claude-opus-4-8-uncached_input',       'claude-opus-4-8', 'uncached_input',  5000000, 'USD', 0, NULL, 1784505600000, NULL, 1),
  ('m515seed-claude-opus-4-8-cached_input',         'claude-opus-4-8', 'cached_input',     500000, 'USD', 0, NULL, 1784505600000, NULL, 1),
  ('m515seed-claude-opus-4-8-cache_creation',       'claude-opus-4-8', 'cache_creation',  6250000, 'USD', 0, NULL, 1784505600000, NULL, 1),
  ('m515seed-claude-opus-4-8-output',               'claude-opus-4-8', 'output',         25000000, 'USD', 0, NULL, 1784505600000, NULL, 1),
  ('m515seed-claude-fable-5-uncached_input',        'claude-fable-5',  'uncached_input', 10000000, 'USD', 0, NULL, 1784505600000, NULL, 1),
  ('m515seed-claude-fable-5-cached_input',          'claude-fable-5',  'cached_input',    1000000, 'USD', 0, NULL, 1784505600000, NULL, 1),
  ('m515seed-claude-fable-5-cache_creation',        'claude-fable-5',  'cache_creation', 12500000, 'USD', 0, NULL, 1784505600000, NULL, 1),
  ('m515seed-claude-fable-5-output',                'claude-fable-5',  'output',         50000000, 'USD', 0, NULL, 1784505600000, NULL, 1),
  ('m515seed-claude-sonnet-5-uncached_input-intro', 'claude-sonnet-5', 'uncached_input',  2000000, 'USD', 0, 1788220800000, 1784505600000, NULL, 1),
  ('m515seed-claude-sonnet-5-cached_input-intro',   'claude-sonnet-5', 'cached_input',     200000, 'USD', 0, 1788220800000, 1784505600000, NULL, 1),
  ('m515seed-claude-sonnet-5-cache_creation-intro', 'claude-sonnet-5', 'cache_creation',  2500000, 'USD', 0, 1788220800000, 1784505600000, NULL, 1),
  ('m515seed-claude-sonnet-5-output-intro',         'claude-sonnet-5', 'output',         10000000, 'USD', 0, 1788220800000, 1784505600000, NULL, 1),
  ('m515seed-claude-sonnet-5-uncached_input-std',   'claude-sonnet-5', 'uncached_input',  3000000, 'USD', 1788220800000, NULL, 1784505600000, NULL, 1),
  ('m515seed-claude-sonnet-5-cached_input-std',     'claude-sonnet-5', 'cached_input',     300000, 'USD', 1788220800000, NULL, 1784505600000, NULL, 1),
  ('m515seed-claude-sonnet-5-cache_creation-std',   'claude-sonnet-5', 'cache_creation',  3750000, 'USD', 1788220800000, NULL, 1784505600000, NULL, 1),
  ('m515seed-claude-sonnet-5-output-std',           'claude-sonnet-5', 'output',         15000000, 'USD', 1788220800000, NULL, 1784505600000, NULL, 1);
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
/// (`developer_identity_snapshot`, `session_binding`).
/// Generated from the **bounded** identity slice, so later registry growth can
/// never perturb its checksum-locked bytes.
#[must_use]
pub fn capture_identity_schema_sql() -> String {
    capture_slice_sql(crate::schema::capture_identity_tables())
}

/// The v4 additive capture migration: the turn-boundary relations
/// (`turn_boundary_snapshot`, `turn_boundary_file`, `turn_diff_hunk`) that make
/// H_i/A_i attribution replay possible, plus the original price seed (see
/// `PRICE_SEED_SQL`) so cost facts compute against a versioned catalog from
/// first open.
#[must_use]
pub fn capture_turn_boundary_schema_sql() -> String {
    let mut sql = capture_slice_sql(crate::schema::capture_turn_boundary_tables());
    sql.push_str(PRICE_SEED_SQL);
    sql
}

/// The v5 additive capture migration: the `projection_meta`
/// key/value table carrying the rebuild epoch. Kept separate so adding it
/// never perturbs the v1–v4 checksums; an absent epoch row reads as zero.
#[must_use]
pub fn capture_projection_meta_schema_sql() -> String {
    capture_slice_sql(crate::schema::capture_projection_meta_tables())
}

/// The v6 additive capture migration: the `file_read_tokens` table
/// holding read-time token measurements (keyed by the read's tool call, so a
/// fact and its figure land together). Kept separate so adding it never
/// perturbs the v1–v5 checksums; a read without a row simply has no estimate
/// (the recall ingest falls back or records 0).
#[must_use]
pub fn capture_read_tokens_schema_sql() -> String {
    capture_slice_sql(crate::schema::capture_read_tokens_tables())
}

/// The v7 additive capture migration: explicit native-to-canonical
/// session identity and prospective main-chain context observations.
#[must_use]
pub fn capture_system_schema_sql() -> String {
    capture_slice_sql(crate::schema::capture_system_tables())
}

/// The v8 additive capture migration: append the current models'
/// prices to the bitemporal catalog. Insert-only — it adds no DDL and touches
/// no existing row, so the v1–v7 checksums are untouched (the frozen-prefix
/// rule migration 5's comment states).
#[must_use]
pub fn capture_price_seed_current_sql() -> String {
    PRICE_SEED_CURRENT_SQL.to_string()
}

/// The v9 additive capture migration: later model observations with explicit
/// provenance, separate from immutable session headline fields.
#[must_use]
pub fn capture_model_observation_schema_sql() -> String {
    capture_slice_sql(crate::schema::capture_model_observation_tables())
}

/// v10: the identity-plane index over the hook audit relation.
///
/// `backfill_capture_session_identities` scans `SELECT DISTINCT agent,
/// session_id FROM agent_hook_event` on open. Unindexed, that is a full scan
/// of the projection's largest table — measured at 59.95 ms in-hook against a
/// 52 MB real-world store (57,463 rows → 59 distinct pairs), on
/// `prepare-commit-msg`, the one hook that cannot detach, which is what
/// pushed a live commit to 118.3 ms against the 100 ms budget (measured
/// 2026-08-09). The partial index turns that DISTINCT into an index walk.
#[must_use]
pub fn capture_hook_event_identity_index_sql() -> String {
    "CREATE INDEX IF NOT EXISTS idx_agent_hook_event_identity \
     ON agent_hook_event(agent, session_id) WHERE session_id IS NOT NULL;\n"
        .to_string()
}

/// The v11 additive capture migration: `checkpoint_commit`, the checkpoint↔commit
/// linkage.
///
/// Every shard already carries the anchor commit in `metadata.json`
/// (`CheckpointMetadata::commit`), and the projection dropped it on the floor —
/// so "which commit is this checkpoint about" was answerable only by re-reading
/// git. This lands it as a row. It mints no new canonical fact: it is a
/// PROJECTION of bytes the substrate already holds, fully rebuilt by
/// `ingest_from_substrate`, so no correction row and no `UPDATE` is ever needed.
///
/// A NEW migration rather than a column on `checkpoint` — see the registry note
/// at the table itself; the short version is that `checkpoint` sits in the
/// frozen v1 prefix whose SQL is checksum-locked on every deployed projection.
#[must_use]
pub fn capture_checkpoint_commit_schema_sql() -> String {
    capture_slice_sql(crate::schema::capture_checkpoint_commit_tables())
}

/// The v12 `verification_witness` soak linkage.
///
/// A NEW migration rather than a column on `verification_run`: that table is
/// #28, inside the frozen v1 prefix whose SQL is checksum-locked on every
/// deployed projection (`capture_core_v1_sql_is_byte_stable`), so an ADD would
/// red that test and break every existing install. v11 took the same road for
/// the same reason.
#[must_use]
pub fn capture_verification_witness_schema_sql() -> String {
    capture_slice_sql(crate::schema::capture_verification_witness_tables())
}

/// The v13 `imported_checkpoint` durable-claim relation: a
/// home for a pre-Rift shard `rift import --from-legacy` read, recording its
/// layout and root-consistency check without ever granting it a `checkpoint`
/// row or a chain position (see the table's own doc comment in `schema/capture.rs`).
#[must_use]
pub fn capture_imported_checkpoint_schema_sql() -> String {
    capture_slice_sql(crate::schema::capture_imported_checkpoint_tables())
}

/// The v14 worktree-provenance relations: the append-only
/// `session_worktree` observation relation and the rebuildable
/// `checkpoint_worktree` projection of the shard metadata's `worktree` member.
///
/// NEW tables rather than columns: `session` and `checkpoint` sit in the
/// frozen v1 prefix whose SQL is checksum-locked on every deployed projection
/// (`capture_core_v1_sql_is_byte_stable`); v11–v13 took the same road for the
/// same reason.
#[must_use]
pub fn capture_worktree_schema_sql() -> String {
    capture_slice_sql(crate::schema::capture_worktree_tables())
}

/// The v15 `tool_result_tokens` relation: the measured token count of one tool
/// RESULT that put repository content in front of the model, on whichever
/// channel it arrived.
///
/// A NEW migration and a NEW table rather than a column on `file_read_tokens`:
/// that table is keyed by `tool_call_id` and reachable only through
/// `tool_reads_file`, so a search — which returns repository content and
/// resolves to no file — has no key there at all (see the registry note at the
/// table itself). Every slice since v11 has taken this road, and for the
/// checksum-freeze reason as well.
#[must_use]
pub fn capture_tool_result_schema_sql() -> String {
    capture_slice_sql(crate::schema::capture_tool_result_tables())
}

/// The default migration set: meta tables, the canonical schema, the identity
/// relations, the turn-boundary relations (+ price seed), the projection-meta
/// table (the rebuild epoch), read-token estimates, the System
/// identity/context relations, the current-model price append, the
/// checkpoint↔commit and verification-witness linkages, the v13
/// `imported_checkpoint` durable-claim relation, and the v14
/// worktree-provenance relations.
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
        Migration {
            version: 7,
            name: "capture_system_truth",
            sql: capture_system_schema_sql(),
        },
        Migration {
            version: 8,
            name: "capture_price_seed_current",
            sql: capture_price_seed_current_sql(),
        },
        Migration {
            version: 9,
            name: "capture_model_observation",
            sql: capture_model_observation_schema_sql(),
        },
        Migration {
            version: 10,
            name: "capture_hook_event_identity_index",
            sql: capture_hook_event_identity_index_sql(),
        },
        Migration {
            version: 11,
            name: "capture_checkpoint_commit",
            sql: capture_checkpoint_commit_schema_sql(),
        },
        Migration {
            version: 12,
            name: "capture_verification_witness",
            sql: capture_verification_witness_schema_sql(),
        },
        Migration {
            version: 13,
            name: "capture_imported_checkpoint",
            sql: capture_imported_checkpoint_schema_sql(),
        },
        Migration {
            version: 14,
            name: "capture_worktree_provenance",
            sql: capture_worktree_schema_sql(),
        },
        Migration {
            version: 15,
            name: "capture_tool_result_tokens",
            sql: capture_tool_result_schema_sql(),
        },
    ]
}

/// Build the recall-schema migration SQL from the recall table registry (the
/// `recall.db` graph) plus its secondary indexes. Same shape as
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

/// The v2 additive recall migration: the bitemporal `outcome_label`
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

/// The v5 additive recall migration: the `n_blob_oid_map` content→blob
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

/// The v7 additive recall migration: one append-only neutral fact for every
/// decoded invocation, attributed to its serving process/connection.
///
/// Frozen: this body is checksum-locked and its relation is append-only, so
/// the shipped spellings inside it survive as read-compat history for a
/// retired transport. See the FROZEN NAMES banner in `schema/recall.rs`.
#[must_use]
pub fn recall_memd_call_schema_sql() -> String {
    use crate::schema::{recall_v7_call_tables, RECALL_CALL_EVENT_INDEX_SQL};
    let mut sql = String::new();
    for table in recall_v7_call_tables() {
        sql.push_str(&table.create_table_sql());
        sql.push('\n');
        sql.push_str(&table.guard_trigger_sql());
        sql.push('\n');
    }
    sql.push_str(RECALL_CALL_EVENT_INDEX_SQL);
    for table in recall_v7_call_tables() {
        sql.push_str(&format!(
            "INSERT INTO _schema_version(table_name, schema_version) VALUES ('{}', 1);\n",
            table.name
        ));
    }
    sql
}

/// The v8 additive recall migration: the append-only bitemporal
/// `eval_run` experiment records — what the eval concluded, when it held, and
/// when we learned it. Kept separate so adding it never perturbs the v1–v7
/// checksums.
#[must_use]
pub fn recall_eval_run_schema_sql() -> String {
    use crate::schema::recall_v8_eval_tables;
    let mut sql = String::new();
    for table in recall_v8_eval_tables() {
        sql.push_str(&table.create_table_sql());
        sql.push('\n');
        sql.push_str(&table.guard_trigger_sql());
        sql.push('\n');
    }
    for table in recall_v8_eval_tables() {
        sql.push_str(&format!(
            "INSERT INTO _schema_version(table_name, schema_version) VALUES ('{}', 1);\n",
            table.name
        ));
    }
    sql
}

/// The v9 additive recall migration: append-only client-attributed connection
/// lifecycle history and its status lookup index.
///
/// Frozen for the same reason as the v7 migration above; the function name
/// mirrors the shipped migration name `recall_mcp_connections`, which is
/// recorded in every deployed `recall.db` and cannot change.
#[must_use]
pub fn recall_mcp_connection_schema_sql() -> String {
    use crate::schema::{recall_v9_connection_tables, RECALL_CONNECTION_EVENT_INDEX_SQL};
    let mut sql = String::new();
    for table in recall_v9_connection_tables() {
        sql.push_str(&table.create_table_sql());
        sql.push('\n');
        sql.push_str(&table.guard_trigger_sql());
        sql.push('\n');
    }
    sql.push_str(RECALL_CONNECTION_EVENT_INDEX_SQL);
    for table in recall_v9_connection_tables() {
        sql.push_str(&format!(
            "INSERT INTO _schema_version(table_name, schema_version) VALUES ('{}', 1);\n",
            table.name
        ));
    }
    sql
}

/// The v10 additive recall migration: the `memd_audit_citation`
/// companion relation and its pack/ordinal lookup index. A NEW migration
/// rather than columns on `memd_audit_event` — see that table's own doc
/// comment in `schema/recall.rs` for why widening it would poison every deployed
/// `recall.db`'s v4 checksum.
#[must_use]
pub fn recall_memd_audit_citation_schema_sql() -> String {
    use crate::schema::{recall_v10_audit_citation_tables, RECALL_AUDIT_CITATION_INDEX_SQL};
    let mut sql = String::new();
    for table in recall_v10_audit_citation_tables() {
        sql.push_str(&table.create_table_sql());
        sql.push('\n');
        sql.push_str(&table.guard_trigger_sql());
        sql.push('\n');
    }
    sql.push_str(RECALL_AUDIT_CITATION_INDEX_SQL);
    for table in recall_v10_audit_citation_tables() {
        sql.push_str(&format!(
            "INSERT INTO _schema_version(table_name, schema_version) VALUES ('{}', 1);\n",
            table.name
        ));
    }
    sql
}

/// The v11 additive recall migration: the `ServeChannel`-
/// discriminated `serve_call_event`/`serve_session_event` relations that
/// supersede `memd_call_event`/`mcp_connection_event`. Both superseded
/// tables REMAIN untouched — read-compat only. The write path that produced
/// schema-2/3 envelopes was retired in `c486f74` without touching those tables
/// or their rows, and no reader targets them for a live figure any more.
#[must_use]
pub fn recall_serve_channel_schema_sql() -> String {
    use crate::schema::{recall_v11_serve_channel_tables, RECALL_SERVE_CHANNEL_INDEX_SQL};
    let mut sql = String::new();
    for table in recall_v11_serve_channel_tables() {
        sql.push_str(&table.create_table_sql());
        sql.push('\n');
        sql.push_str(&table.guard_trigger_sql());
        sql.push('\n');
    }
    sql.push_str(RECALL_SERVE_CHANNEL_INDEX_SQL);
    for table in recall_v11_serve_channel_tables() {
        sql.push_str(&format!(
            "INSERT INTO _schema_version(table_name, schema_version) VALUES ('{}', 1);\n",
            table.name
        ));
    }
    sql
}

/// The v12 worktree-scope relations: the recall-side
/// mirrors of the anchor commit and the archiving worktree — `recall.db`
/// never joins a capture table, by design — plus the grant-reason
/// store that keeps untrusted free text out of the frozen neutral-tag
/// `memd_audit_event.reason` column.
#[must_use]
pub fn recall_worktree_scope_schema_sql() -> String {
    use crate::schema::recall_v12_worktree_scope_tables;
    let mut sql = String::new();
    for table in recall_v12_worktree_scope_tables() {
        sql.push_str(&table.create_table_sql());
        sql.push('\n');
        sql.push_str(&table.guard_trigger_sql());
        sql.push('\n');
    }
    for table in recall_v12_worktree_scope_tables() {
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
/// durable ingest watermark; version 7 adds per-thread call attribution;
/// version 8 adds the eval experiment records; version 9 adds
/// client connection lifecycle history; version 10 adds the
/// `memd_audit_citation` companion relation; version 11 adds the
/// `ServeChannel`-discriminated `serve_call_event`/
/// `serve_session_event` relations.
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
        Migration {
            version: 7,
            name: "recall_memd_calls",
            sql: recall_memd_call_schema_sql(),
        },
        Migration {
            version: 8,
            name: "recall_eval_run",
            sql: recall_eval_run_schema_sql(),
        },
        Migration {
            version: 9,
            name: "recall_mcp_connections",
            sql: recall_mcp_connection_schema_sql(),
        },
        Migration {
            version: 10,
            name: "recall_memd_audit_citation",
            sql: recall_memd_audit_citation_schema_sql(),
        },
        Migration {
            version: 11,
            name: "recall_serve_channel",
            sql: recall_serve_channel_schema_sql(),
        },
        Migration {
            version: 12,
            name: "recall_worktree_scope",
            sql: recall_worktree_scope_schema_sql(),
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
/// existing `ui-state.db`). v1 is the frozen project + toggle core; v2
/// adds the per-project layout table; v3 adds the profile plane; v4 adds the
/// per-(profile, slot) theme table; v5 adds
/// the startup-pin table — each a new version so an existing store
/// upgrades additively and the applied checksums stay locked.
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
        Migration {
            version: 5,
            name: "canopy_profile_startup",
            sql: canopy_schema_sql_for(&["canopy_profile_startup_event"]),
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
    // Owner-only dir + file (the local-disk-reader mitigation).
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
    let store = open_migrated(path, &default_migrations())?;
    backfill_capture_session_identities(&store)?;
    Ok(store)
}

/// Backfill the identity bridge from immutable hook-audit facts. Only sessions
/// already present in the normalized projection are admitted; reruns are
/// append-only no-ops and never invent a fallback mapping.
///
/// GUARDED: the scan runs only while some `session` row lacks its
/// `capture_session_identity` row. The guard is a small anti-join over the
/// session table (dozens of rows, PK lookups), not a scan of the audit
/// relation — which, unguarded, cost 59.95 ms per open against a 52 MB store
/// and breached the 100 ms hook budget (measured 2026-08-09).
///
/// Why skipping is lossless when the guard reports full coverage — a
/// DERIVATION, stated because no test can observe a skip directly:
/// `capture_session_row_id(agent, native)` IS the session_id, so for a
/// covered session the scan's `INSERT OR IGNORE` could only ever re-insert
/// the same primary key it already has — a no-op by the same constraint that
/// makes reruns no-ops. New sessions are covered at capture time
/// (`record_capture_session_identity` in the projection path), so the guard
/// converges to permanently-false after one historical backfill; it fires
/// again only if a store predating the live write path gains sessions.
fn backfill_capture_session_identities(store: &SqliteStore) -> Result<()> {
    store.with_conn(|conn| {
        let uncovered: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM session s WHERE NOT EXISTS \
             (SELECT 1 FROM capture_session_identity c \
              WHERE c.session_id = s.session_id))",
            [],
            |row| row.get(0),
        )?;
        if !uncovered {
            return Ok(());
        }
        let facts = {
            let mut stmt = conn.prepare(
                "SELECT DISTINCT agent, session_id FROM agent_hook_event \
                 WHERE session_id IS NOT NULL ORDER BY agent, session_id",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        let tx = conn.unchecked_transaction()?;
        for (agent, native_session_id) in facts {
            let session_id = rift_core::capture_session_row_id(&agent, &native_session_id);
            tx.execute(
                "INSERT OR IGNORE INTO capture_session_identity \
                 (session_id, agent, native_session_id, schema_version) \
                 SELECT ?, ?, ?, 1 WHERE EXISTS \
                 (SELECT 1 FROM session WHERE session_id = ?)",
                rusqlite::params![&session_id, agent, native_session_id, &session_id],
            )?;
        }
        tx.commit()
    })
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
                    "migration {} checksum changed since it was applied. If a \
                     release note names an authorized schema change, the \
                     projection is a rebuildable artifact: set it aside and \
                     run `rift rebuild`, then `rift verify`. Anything else \
                     here is tampering: stop and inspect.",
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
    let (todo, mut skipped) = plan_migrations(&applied, migs)?;
    let mut high_water = applied.keys().copied().max().unwrap_or(0);
    let mut applied_count = 0usize;
    for m in todo {
        // `false` = a concurrent opener applied this version between our plan
        // and its write lock — counted as skipped, never re-applied.
        if store.apply_migration(
            &m.sql,
            m.version,
            m.name,
            &checksum_hex(&m.sql),
            &Timestamp::now().to_string(),
        )? {
            applied_count += 1;
        } else {
            skipped += 1;
        }
        high_water = m.version;
    }
    Ok(MigrationOutcome {
        applied: applied_count,
        skipped,
        high_water,
    })
}

/// [`apply_migrations`] through the engine-agnostic `Store` seam: meta-table
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
    let (todo, mut skipped) = plan_migrations(&applied, migs)?;
    let mut high_water = applied.keys().copied().max().unwrap_or(0);
    let mut applied_count = 0usize;
    for m in todo {
        let tx = conn.begin(crate::TxMode::Deferred).await?;
        // Same race-skip as the sync runner: a concurrent opener may have
        // applied this version since we planned (narrower guarantee here —
        // deferred, not immediate — until the seam grows an immediate mode).
        let raced = tx
            .query(
                "SELECT 1 FROM _migrations WHERE version = ?",
                &[crate::Value::Int(i64::from(m.version))],
            )
            .await?;
        if !raced.is_empty() {
            drop(tx);
            skipped += 1;
            high_water = m.version;
            continue;
        }
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
        applied_count += 1;
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
        assert_eq!(out.applied, 15);
        assert_eq!(out.high_water, 15);
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

    /// The models the toolchain actually runs are priced
    /// across all four classes, the original seed's rows are untouched by the append, and
    /// `claude-sonnet-5` resolves its introductory rate before the changeover
    /// and the standard rate on and after it.
    #[tokio::test]
    async fn current_model_prices_are_seeded_and_bitemporal() {
        let s = store();
        apply_migrations(&s, &default_migrations()).unwrap();
        let now = 1_784_505_600_000_i64; // 2026-07-20T00:00:00Z

        // effective_prices yields None unless ALL FOUR classes are on file, so
        // a Some here is the whole "this model produces a real figure" claim.
        for model in ["claude-opus-4-8", "claude-fable-5", "claude-sonnet-5"] {
            assert!(
                crate::effective_prices(&s, model, now, now)
                    .await
                    .unwrap()
                    .is_some(),
                "{model} must price all four classes"
            );
        }

        let opus = crate::effective_prices(&s, "claude-opus-4-8", now, now)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(opus.uncached_input, 5_000_000);
        assert_eq!(opus.cached_input, 500_000); // 0.1× input
        assert_eq!(opus.cache_creation, 6_250_000); // 1.25× input
        assert_eq!(opus.output, 25_000_000);

        // The original seed is untouched — the append never rewrites a prior belief.
        let legacy = crate::effective_prices(&s, "claude-sonnet-4-5", now, now)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(legacy.uncached_input, 3_000_000);
        assert_eq!(legacy.output, 15_000_000);

        // Bitemporal valid-time: intro rate before 2026-09-01, standard on it.
        let changeover = 1_788_220_800_000_i64; // 2026-09-01T00:00:00Z
        let intro = crate::effective_prices(&s, "claude-sonnet-5", changeover - 1, now)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(intro.uncached_input, 2_000_000);
        assert_eq!(intro.output, 10_000_000);
        let standard = crate::effective_prices(&s, "claude-sonnet-5", changeover, now)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(standard.uncached_input, 3_000_000);
        assert_eq!(standard.output, 15_000_000);
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
        assert_eq!(out.applied, 15);
        assert_eq!(out.high_water, 15);
        // The sync runner sees everything applied (same _migrations ledger).
        let again = apply_migrations(&s, &default_migrations()).unwrap();
        assert_eq!(again.applied, 0);
        assert_eq!(again.skipped, 15);
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
        /// The idempotency invariant: applying any PREFIX of the
        /// shipping migration set and then the FULL set converges on the same
        /// schema as one full apply, with prefix members skipped (checksums
        /// verified), never re-applied.
        #[test]
        fn apply_then_apply_is_noop(split in 0usize..=9) {
            let migs = default_migrations();
            let s = store();
            let first = apply_migrations(&s, &migs[..split]).unwrap();
            proptest::prop_assert_eq!(first.applied, split);
            let rest = apply_migrations(&s, &migs).unwrap();
            proptest::prop_assert_eq!(rest.skipped, split);
            proptest::prop_assert_eq!(rest.applied, migs.len() - split);
            proptest::prop_assert_eq!(rest.high_water, 15);

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
        assert_eq!(out.skipped, 15);
    }

    /// The additive contract for the recall projection: `recall_core` (v1) must
    /// stay byte-stable as the registry grows (its checksum is locked), so the
    /// `outcome_label` table lives in the v2 migration and never leaks into v1.
    #[test]
    fn recall_core_excludes_v2_outcome_label() {
        let migs = recall_migrations();
        assert_eq!(migs.len(), 12);
        assert_eq!(migs[0].name, "recall_core");
        assert_eq!(migs[1].name, "recall_outcome_labels");
        assert_eq!(migs[2].name, "recall_key_custody");
        assert_eq!(migs[3].name, "recall_memd_audit");
        assert_eq!(migs[4].name, "recall_blob_oid_map");
        assert_eq!(migs[5].name, "recall_ingest_watermark");
        assert_eq!(migs[6].name, "recall_memd_calls");
        assert_eq!(migs[7].name, "recall_eval_run");
        assert_eq!(migs[8].name, "recall_mcp_connections");
        assert_eq!(migs[9].name, "recall_memd_audit_citation");
        assert_eq!(migs[10].name, "recall_serve_channel");
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
        // v6 creates exactly the durable ingest watermark + its guards and
        // must not absorb the v7 attributed-call table.
        assert!(migs[5]
            .sql
            .contains("CREATE TABLE IF NOT EXISTS ingest_watermark"));
        assert!(migs[5].sql.contains("ingest_watermark_no_update"));
        assert!(!migs[5].sql.contains("memd_call_event"));
        // v7 creates the invocation-addressed call table, guards, and indexes —
        // and must NOT have absorbed the v8 eval-run table (checksum lock for
        // existing v7 recall.dbs).
        assert!(migs[6]
            .sql
            .contains("CREATE TABLE IF NOT EXISTS memd_call_event"));
        assert!(migs[6].sql.contains("memd_call_event_no_update"));
        assert!(migs[6].sql.contains("idx_memd_call_thread_recorded"));
        assert!(!migs[6].sql.contains("eval_run"));
        // v8 creates exactly the bitemporal eval experiment records + guards.
        assert!(migs[7].sql.contains("CREATE TABLE IF NOT EXISTS eval_run"));
        assert!(migs[7].sql.contains("eval_run_no_update"));
        assert!(migs[7].sql.contains("eval_run_no_delete"));
        assert!(!migs[7].sql.contains("mcp_connection_event"));
        assert!(migs[8]
            .sql
            .contains("CREATE TABLE IF NOT EXISTS mcp_connection_event"));
        assert!(migs[8].sql.contains("mcp_connection_event_no_update"));
        assert!(migs[8].sql.contains("idx_mcp_connection_client_time"));
        assert!(!migs[8].sql.contains("memd_audit_citation"));
        // v10 creates exactly the audit-citation companion relation + its index —
        // and must NOT have absorbed the v11 ServeChannel tables.
        assert!(migs[9]
            .sql
            .contains("CREATE TABLE IF NOT EXISTS memd_audit_citation"));
        assert!(migs[9].sql.contains("memd_audit_citation_no_update"));
        assert!(migs[9].sql.contains("idx_memd_audit_citation_pack_ordinal"));
        assert!(!migs[9].sql.contains("serve_call_event"));
        // v11 creates exactly the two ServeChannel relations + their index.
        assert!(migs[10]
            .sql
            .contains("CREATE TABLE IF NOT EXISTS serve_call_event"));
        assert!(migs[10]
            .sql
            .contains("CREATE TABLE IF NOT EXISTS serve_session_event"));
        assert!(migs[10].sql.contains("serve_call_event_no_update"));
        assert!(migs[10].sql.contains("serve_session_event_no_update"));
        assert!(migs[10].sql.contains("idx_serve_call_channel_recorded"));
    }

    /// Applying v1 then v2 to a fresh recall.db is a clean upgrade (no checksum
    /// conflict), and re-running both is idempotent.
    #[test]
    fn recall_migrations_apply_and_rerun_cleanly() {
        let s = store();
        let out = apply_migrations(&s, &recall_migrations()).unwrap();
        assert_eq!(out.applied, 12);
        assert_eq!(out.high_water, 12);
        let again = apply_migrations(&s, &recall_migrations()).unwrap();
        assert_eq!(again.applied, 0);
        assert_eq!(again.skipped, 12);
    }

    /// The canopy UI-state migrations create the append-only tables (v1 core +
    /// v2 layout + v3 profiles) and re-run idempotently (a fresh `ui-state.db`
    /// upgrade).
    #[test]
    fn canopy_migrations_apply_and_rerun_cleanly() {
        let s = store();
        let out = apply_migrations(&s, &canopy_migrations()).unwrap();
        assert_eq!(
            out.applied, 5,
            "v1 core + v2 layout + v3 profiles + v4 theme + v5 startup"
        );
        assert_eq!(out.high_water, 5);
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
        // with its append-only guards.
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
        // v3 stays frozen (no theme table); v4 adds it with guards.
        assert!(!v3.contains("canopy_profile_theme_event"), "v3 is frozen");
        let v4 = &canopy_migrations()[3].sql;
        assert!(v4.contains("CREATE TABLE IF NOT EXISTS canopy_profile_theme_event"));
        assert!(v4.contains("canopy_profile_theme_event_no_update"));
        assert!(v4.contains("canopy_profile_theme_event_no_delete"));
        // v4 stays frozen (no startup table); v5 adds it with guards.
        assert!(!v4.contains("canopy_profile_startup_event"), "v4 is frozen");
        let v5 = &canopy_migrations()[4].sql;
        assert!(v5.contains("CREATE TABLE IF NOT EXISTS canopy_profile_startup_event"));
        assert!(v5.contains("canopy_profile_startup_event_no_update"));
        assert!(v5.contains("canopy_profile_startup_event_no_delete"));
        let again = apply_migrations(&s, &canopy_migrations()).unwrap();
        assert_eq!(again.applied, 0);
        assert_eq!(again.skipped, 5);
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
        // v11 is pinned from its FIRST commit, not after it has shipped: the
        // window in which an additive migration is quietly editable is exactly
        // the window in which someone edits it.
        assert_eq!(
            checksum_hex(&capture_checkpoint_commit_schema_sql()),
            "931230d3a851c889b9a928a8d5002595aaf00da97a8f5da7f28e8299a6749c8e",
            "capture_checkpoint_commit (v11) migration SQL changed — frozen once shipped"
        );
        // v13, pinned the same way, from this same commit.
        assert_eq!(
            checksum_hex(&capture_imported_checkpoint_schema_sql()),
            "6894b4be5e4c5819ca7466ef0ee19d7427398e3f2ebb41bc6cf00eb33011a61c",
            "capture_imported_checkpoint (v13) migration SQL changed — frozen once shipped"
        );
        // v14, pinned from its FIRST commit, like v11 and v13 before it.
        assert_eq!(
            checksum_hex(&capture_worktree_schema_sql()),
            "6b77adb2e63a92746871fdf1a9b362605b1859e649c1837e9555f824eaf4421d",
            "capture_worktree_provenance (v14) migration SQL changed — frozen once shipped"
        );
        // v15, pinned from its FIRST commit, like v11, v13 and v14 before it.
        assert_eq!(
            checksum_hex(&capture_tool_result_schema_sql()),
            "b5c9899faeda319f768b8a8c3ed0deb52d1c65d07e645f57f05bbd98605582bc",
            "capture_tool_result_tokens (v15) migration SQL changed — frozen once shipped"
        );
    }

    /// Every capture table lands in exactly one migration slice, and the frozen
    /// boundaries hold: growing or reordering a shipped slice would change its
    /// checksum-locked SQL on every deployed projection.
    #[test]
    fn capture_registry_growth_lands_past_frozen_boundaries() {
        use crate::schema::{
            canonical_tables, capture_checkpoint_commit_tables, capture_identity_tables,
            capture_imported_checkpoint_tables, capture_model_observation_tables,
            capture_projection_meta_tables, capture_read_tokens_tables, capture_system_tables,
            capture_tool_result_tables, capture_turn_boundary_tables,
            capture_verification_witness_tables, capture_worktree_tables,
        };
        assert_eq!(
            capture_v1_tables().len()
                + capture_identity_tables().len()
                + capture_turn_boundary_tables().len()
                + capture_projection_meta_tables().len()
                + capture_read_tokens_tables().len()
                + capture_system_tables().len()
                + capture_model_observation_tables().len()
                + capture_checkpoint_commit_tables().len()
                + capture_verification_witness_tables().len()
                + capture_imported_checkpoint_tables().len()
                + capture_worktree_tables().len()
                + capture_tool_result_tables().len(),
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
        assert_eq!(
            capture_system_tables().last().map(|t| t.name),
            Some("main_context_observation"),
            "the v7 boundary moved — capture_system_truth's checksum will break"
        );
        assert_eq!(
            capture_model_observation_tables().last().map(|t| t.name),
            Some("model_observation"),
            "the v9 model-observation boundary moved"
        );
        assert_eq!(
            capture_checkpoint_commit_tables().last().map(|t| t.name),
            Some("checkpoint_commit"),
            "the v11 checkpoint-commit boundary moved"
        );
        assert_eq!(
            capture_verification_witness_tables().last().map(|t| t.name),
            Some("verification_witness"),
            "the v12 verification-witness boundary moved"
        );
        assert_eq!(
            capture_imported_checkpoint_tables().last().map(|t| t.name),
            Some("imported_checkpoint"),
            "the v13 imported-checkpoint boundary moved"
        );
        assert_eq!(
            capture_worktree_tables().last().map(|t| t.name),
            Some("checkpoint_worktree"),
            "the v14 worktree-provenance boundary moved"
        );
        assert_eq!(
            capture_tool_result_tables().last().map(|t| t.name),
            Some("tool_result_tokens"),
            "the v15 tool-result boundary moved"
        );
    }

    /// The v13 table is a NEW relation, not a widened old one — so unlike the
    /// v11/v12 tests above, what this pins is that it landed PAST every frozen
    /// slice rather than inside one (the same mistake, one commit over).
    #[test]
    fn the_v13_import_table_did_not_land_in_a_frozen_migration() {
        for (name, sql) in [
            ("canonical_core (v1 frozen prefix)", canonical_schema_sql()),
            ("capture_identity (v3)", capture_identity_schema_sql()),
            (
                "capture_turn_boundary (v4)",
                capture_turn_boundary_schema_sql(),
            ),
            (
                "capture_checkpoint_commit (v11)",
                capture_checkpoint_commit_schema_sql(),
            ),
            (
                "capture_verification_witness (v12)",
                capture_verification_witness_schema_sql(),
            ),
        ] {
            assert!(
                !sql.contains("imported_checkpoint"),
                "v13's table leaked into the {name} migration's checksum-locked SQL"
            );
        }
        assert!(
            capture_imported_checkpoint_schema_sql()
                .contains("CREATE TABLE IF NOT EXISTS imported_checkpoint"),
            "the v13 migration must itself create the table"
        );
    }

    /// The `checkpoint` table itself is UNCHANGED by the v11 linkage — stated as
    /// a test because "add the anchor commit to the checkpoint row" is the
    /// obvious implementation and it is the one that breaks every deployed
    /// projection. `capture_core_v1_sql_is_byte_stable` catches it globally;
    /// this names the specific table and the specific temptation.
    #[test]
    fn the_v11_linkage_did_not_widen_the_frozen_checkpoint_table() {
        let checkpoint = crate::schema::canonical_table("checkpoint").expect("checkpoint table");
        let names: Vec<&str> = checkpoint.columns.iter().map(|c| c.name).collect();
        assert_eq!(
            names,
            vec![
                "checkpoint_id",
                "strategy",
                "created_at",
                "merkle_root",
                "prev_hash",
                "schema_version"
            ],
            "the checkpoint↔commit link belongs in `checkpoint_commit`, not in the frozen v1 row"
        );
        assert!(
            !canonical_schema_sql().contains("commit_oid"),
            "v11's column leaked into the checksum-locked canonical_core SQL"
        );
    }

    /// The v12 twin of the test above, and it names the temptation it exists
    /// to refuse: the soak needs to know WHICH checkpoint a verify run
    /// witnessed, and the obvious move is a `checkpoint_id` column on
    /// `verification_run`. That table is #28, inside the frozen v1 prefix, so
    /// the obvious move breaks every deployed projection's checksum lock.
    #[test]
    fn the_v12_witness_did_not_widen_the_frozen_verification_run_table() {
        let run = crate::schema::canonical_table("verification_run").expect("verification_run");
        let names: Vec<&str> = run.columns.iter().map(|c| c.name).collect();
        assert_eq!(
            names,
            vec![
                "run_id",
                "branch",
                "exit_code",
                "checked_at",
                "outcome_sha256",
                "schema_version"
            ],
            "the run↔checkpoint link belongs in `verification_witness`, not in the frozen v1 row"
        );
        assert!(
            !canonical_schema_sql().contains("verification_witness"),
            "v12's table leaked into the checksum-locked canonical_core SQL"
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
        assert_eq!(migs, 15);
    }

    /// The filesystem control: a projection opened through the standard
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

    #[test]
    fn v7_identity_backfill_derives_production_session_id_append_only() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("projection.db");
        let old = open_migrated(&db, &default_migrations()[..6]).unwrap();
        let canonical = rift_core::capture_session_row_id("claude-code", "native-42");
        old.with_conn(|conn| {
            conn.execute(
                "INSERT INTO session \
                 (session_id, agent, model_primary, repo_id, started_at, schema_version) \
                 VALUES (?, 'claude-code', '', 'repo', 1, 1)",
                [&canonical],
            )?;
            conn.execute(
                "INSERT INTO agent_hook_event \
                 (hook_event_id, agent, verb, session_id, received_at, outcome, schema_version) \
                 VALUES ('hook', 'claude-code', 'Stop', 'native-42', 2, 'recorded', 1)",
                [],
            )?;
            Ok(())
        })
        .unwrap();
        drop(old);

        let upgraded = open_capture_projection(&db).unwrap();
        let mapping: (String, String, String) = upgraded
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT session_id, agent, native_session_id \
                     FROM capture_session_identity",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
            })
            .unwrap();
        assert_eq!(
            mapping,
            (canonical, "claude-code".into(), "native-42".into())
        );
        drop(upgraded);
        let reopened = open_capture_projection(&db).unwrap();
        let count: i64 = reopened
            .with_conn(|conn| {
                conn.query_row("SELECT COUNT(*) FROM capture_session_identity", [], |row| {
                    row.get(0)
                })
            })
            .unwrap();
        assert_eq!(count, 1, "backfill redelivery is an insert-only no-op");
    }

    /// The guard that keeps the backfill off the hook path's hot open: it must
    /// report coverage EXACTLY — false only when every session has its
    /// identity row. The dangerous mutation is a guard that reports covered
    /// while a session is bare (the scan would be skipped forever); the
    /// uncovered arm here is what reddens it.
    #[test]
    fn the_backfill_coverage_guard_is_exact_in_both_directions() {
        let s = store();
        apply_migrations(&s, &default_migrations()).unwrap();
        let probe = |s: &SqliteStore| -> bool {
            s.with_conn(|conn| {
                conn.query_row(
                    "SELECT EXISTS(SELECT 1 FROM session s WHERE NOT EXISTS \
                     (SELECT 1 FROM capture_session_identity c \
                      WHERE c.session_id = s.session_id))",
                    [],
                    |row| row.get(0),
                )
            })
            .unwrap()
        };
        assert!(!probe(&s), "no sessions at all reads as covered");
        let sid = rift_core::capture_session_row_id("claude-code", "n-1");
        s.with_conn(|conn| {
            conn.execute(
                "INSERT INTO session \
                 (session_id, agent, model_primary, repo_id, started_at, schema_version) \
                 VALUES (?, 'claude-code', '', 'repo', 1, 1)",
                [&sid],
            )
        })
        .unwrap();
        assert!(
            probe(&s),
            "a session without its identity row reads uncovered"
        );
        s.with_conn(|conn| {
            conn.execute(
                "INSERT INTO capture_session_identity \
                 (session_id, agent, native_session_id, schema_version) \
                 VALUES (?, 'claude-code', 'n-1', 1)",
                [&sid],
            )
        })
        .unwrap();
        assert!(
            !probe(&s),
            "the identity row flips the guard back to covered"
        );
    }

    /// v10 pins the identity-plane index: the backfill's DISTINCT scan of the
    /// audit relation measured 59.95 ms unindexed against a 52 MB store, on
    /// the one hook that cannot detach. The index (partial, `session_id IS
    /// NOT NULL`) must exist on every opened projection.
    #[test]
    fn v10_identity_index_exists_on_an_opened_projection() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("projection.db");
        let s = open_capture_projection(&db).unwrap();
        let (name, sql): (String, String) = s
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT name, sql FROM sqlite_master WHERE type = 'index' \
                     AND name = 'idx_agent_hook_event_identity'",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
            })
            .unwrap();
        assert_eq!(name, "idx_agent_hook_event_identity");
        assert!(
            sql.contains("WHERE session_id IS NOT NULL"),
            "the index must stay partial — NULL session_ids carry no identity"
        );
    }

    /// Pins that every table gets its `_schema_version` seed row. NOTE the
    /// honest limitation: version *bumps* are not yet representable — the row's
    /// PK is `table_name` and meta tables are insert-only, so the bump
    /// mechanism (a superseding-row shape with PK `(table_name, version)`)
    /// must ride the first additive-column migration as a NEW meta migration;
    /// `META_SQL` itself is checksum-locked on every deployed projection and
    /// cannot be edited.
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
