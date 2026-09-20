//! The capture projection's migration set (`projection.db`): the price seeds,
//! the fifteen checksum-locked SQL builders and the identity backfill that
//! runs on open.
//!
//! A separate module because every builder here is frozen once shipped: the
//! text it returns is checksummed into every deployed projection, so growth
//! lands as a new version at the end of [`default_migrations`], never as an
//! edit to one of these bodies.

use rift_core::Result;

use crate::schema::capture_v1_tables;
use crate::sqlite_store::SqliteStore;

use super::{Migration, META_SQL};

/// The original price seed: a versioned starting catalog, in **micro-USD per
/// MILLION tokens**, for the models the first-party adapters report. Prices are
/// data, not code constants — this seed is simply the first append. Provenance:
/// the provider's public per-MTok pricing as of the 2026-07 pin date;
/// `valid_from = 0` (in force since before capture began), `recorded_at` fixed
/// at 2026-07-01T00:00:00Z for migration byte-stability. A price change or
/// correction is a NEW superseding row (`record_price` / a later migration) —
/// never an UPDATE. Models without a seeded price simply produce no cost fact
/// (honest absence beats a fabricated number).
const PRICE_SEED_SQL: &str = "\
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
const PRICE_SEED_CURRENT_SQL: &str = "\
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
pub(super) fn backfill_capture_session_identities(store: &SqliteStore) -> Result<()> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::migrate::fixtures::store;
    use crate::migrate::{
        apply_migrations, checksum_hex, is_additive, open_capture_projection, open_migrated,
    };
    use crate::schema::CanonicalTable;

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

    /// Every shipped capture slice, labelled by its migration, with the table
    /// that must still close it: the registry accessor is the slice's live
    /// reading, the name its frozen boundary.
    fn shipped_capture_slices() -> [(&'static str, &'static [CanonicalTable], &'static str); 12] {
        use crate::schema::{
            capture_checkpoint_commit_tables, capture_identity_tables,
            capture_imported_checkpoint_tables, capture_model_observation_tables,
            capture_projection_meta_tables, capture_read_tokens_tables, capture_system_tables,
            capture_tool_result_tables, capture_turn_boundary_tables,
            capture_verification_witness_tables, capture_worktree_tables,
        };
        [
            ("v1 canonical_core", capture_v1_tables(), "key_metadata"),
            (
                "v3 capture_identity",
                capture_identity_tables(),
                "session_binding",
            ),
            (
                "v4 capture_turn_boundary",
                capture_turn_boundary_tables(),
                "turn_diff_hunk",
            ),
            (
                "v5 capture_projection_meta",
                capture_projection_meta_tables(),
                "projection_meta",
            ),
            (
                "v6 capture_read_tokens",
                capture_read_tokens_tables(),
                "file_read_tokens",
            ),
            (
                "v7 capture_system_truth",
                capture_system_tables(),
                "main_context_observation",
            ),
            (
                "v9 capture_model_observation",
                capture_model_observation_tables(),
                "model_observation",
            ),
            (
                "v11 capture_checkpoint_commit",
                capture_checkpoint_commit_tables(),
                "checkpoint_commit",
            ),
            (
                "v12 capture_verification_witness",
                capture_verification_witness_tables(),
                "verification_witness",
            ),
            (
                "v13 capture_imported_checkpoint",
                capture_imported_checkpoint_tables(),
                "imported_checkpoint",
            ),
            (
                "v14 capture_worktree_provenance",
                capture_worktree_tables(),
                "checkpoint_worktree",
            ),
            (
                "v15 capture_tool_result_tokens",
                capture_tool_result_tables(),
                "tool_result_tokens",
            ),
        ]
    }

    /// Every capture table lands in exactly one migration slice, and the frozen
    /// boundaries hold: growing or reordering a shipped slice would change its
    /// checksum-locked SQL on every deployed projection.
    #[test]
    fn capture_registry_growth_lands_past_frozen_boundaries() {
        let slices = shipped_capture_slices();
        let n_sliced_tables: usize = slices.iter().map(|(_, tables, _)| tables.len()).sum();
        assert_eq!(
            n_sliced_tables,
            crate::schema::canonical_tables().len(),
            "a new capture table must extend a NEW slice (and migration), not fall between"
        );
        for (label, tables, last) in slices {
            assert_eq!(
                tables.last().map(|t| t.name),
                Some(last),
                "the {label} boundary moved — its checksum-locked SQL breaks on every deployed projection"
            );
        }
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
}
