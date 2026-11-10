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

mod canopy;
mod capture;
#[cfg(test)]
mod fixtures;
mod recall;

use std::collections::BTreeMap;
use std::sync::LazyLock;

use rift_core::{Result, RiftError, Timestamp};
use sha2::{Digest, Sha256};

use crate::sqlite_store::SqliteStore;

use capture::backfill_capture_session_identities;

pub use canopy::{canopy_canonical_schema_sql, canopy_migrations};
pub use capture::{
    canonical_schema_sql, capture_checkpoint_commit_schema_sql,
    capture_hook_event_identity_index_sql, capture_identity_schema_sql,
    capture_imported_checkpoint_schema_sql, capture_model_observation_schema_sql,
    capture_price_seed_current_sql, capture_projection_meta_schema_sql,
    capture_read_tokens_schema_sql, capture_system_schema_sql, capture_tool_result_schema_sql,
    capture_turn_boundary_schema_sql, capture_verification_witness_schema_sql,
    capture_worktree_schema_sql, default_migrations,
};
pub use recall::{
    recall_blob_oid_map_schema_sql, recall_canonical_schema_sql, recall_eval_run_schema_sql,
    recall_ingest_watermark_schema_sql, recall_key_custody_schema_sql,
    recall_mcp_connection_schema_sql, recall_memd_audit_citation_schema_sql,
    recall_memd_audit_schema_sql, recall_memd_call_schema_sql, recall_migrations,
    recall_outcome_label_schema_sql, recall_serve_channel_schema_sql,
    recall_worktree_scope_schema_sql,
};

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

/// A production migration set, generated and fingerprinted once per process.
///
/// Every agent hook opens the capture projection, and a long-lived process
/// (Canopy, the worker) opens it again and again. Planning each open meant
/// regenerating every migration's SQL, running the additive guard over all of
/// it and hashing it. The fingerprint is over exactly what the planner
/// compares — each `(version, checksum)` — so a projection whose `_migrations`
/// ledger hashes to the same value is one the full plan would have found
/// complete and untampered, and its open can skip the plan.
struct CodeMigrations {
    migrations: Vec<Migration>,
    fingerprint: String,
}

impl CodeMigrations {
    fn of(migrations: Vec<Migration>) -> Self {
        let fingerprint = ledger_fingerprint(
            migrations
                .iter()
                .map(|m| (m.version, checksum_hex(&m.sql)))
                .collect(),
        );
        Self {
            migrations,
            fingerprint,
        }
    }
}

static CAPTURE_MIGRATIONS: LazyLock<CodeMigrations> =
    LazyLock::new(|| CodeMigrations::of(default_migrations()));

static RECALL_MIGRATIONS: LazyLock<CodeMigrations> =
    LazyLock::new(|| CodeMigrations::of(recall_migrations()));

fn checksum_hex(sql: &str) -> String {
    let mut h = Sha256::new();
    h.update(sql.as_bytes());
    hex(&h.finalize())
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    use core::fmt::Write as _;
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// One digest over a `(version, checksum)` ledger in version order. The line
/// form `version:checksum\n` is unambiguous, so equal digests mean equal
/// ledgers.
fn ledger_fingerprint(mut ledger: Vec<(u32, String)>) -> String {
    ledger.sort_by_key(|(version, _)| *version);
    let mut h = Sha256::new();
    for (version, checksum) in ledger {
        h.update(format!("{version}:{checksum}\n").as_bytes());
    }
    hex(&h.finalize())
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
    let store = open_hardened(path.as_ref())?;
    apply_migrations(&store, migrations)?;
    Ok(store)
}

/// [`open_migrated`] with the recall projection's migration set
/// ([`recall_migrations`]) — the standard way to open `recall.db`. An open
/// whose ledger already matches the code skips the planner (see
/// [`apply_code_migrations`]).
///
/// # Errors
/// See [`open_migrated`].
pub fn open_recall_projection(path: impl AsRef<std::path::Path>) -> Result<SqliteStore> {
    let store = open_hardened(path.as_ref())?;
    apply_code_migrations(&store, &RECALL_MIGRATIONS)?;
    Ok(store)
}

/// Create the parent, open the store, and make both owner-only (the
/// local-disk-reader mitigation).
fn open_hardened(path: &std::path::Path) -> Result<SqliteStore> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let store = SqliteStore::open(path)?;
    crate::paths::harden_projection_permissions(path)?;
    Ok(store)
}

/// Bring `store` to a production set, skipping the planner when its applied
/// ledger is exactly the code's.
///
/// The skip keeps the tamper lock whole: it compares the projection's own
/// `_migrations` rows, read on every open, against the code. An edited
/// migration, an edited ledger row, a missing version or a pending one all
/// change the digest and fall through to [`apply_migrations`], which plans,
/// refuses tampering and applies what is owed exactly as before. The only
/// thing skipped is work whose answer is already known: "nothing to apply".
fn apply_code_migrations(store: &SqliteStore, set: &CodeMigrations) -> Result<MigrationOutcome> {
    if let Some(outcome) = ledger_matches(store, set) {
        return Ok(outcome);
    }
    apply_migrations(store, &set.migrations)
}

/// The no-op outcome when `store`'s ledger hashes to `set`'s fingerprint.
/// Any read failure — a fresh file has no `_migrations` yet — is a miss.
fn ledger_matches(store: &SqliteStore, set: &CodeMigrations) -> Option<MigrationOutcome> {
    let ledger = store.applied_migrations().ok()?;
    let high_water = ledger.iter().map(|(version, _)| *version).max()?;
    let skipped = ledger.len();
    (ledger_fingerprint(ledger) == set.fingerprint).then_some(MigrationOutcome {
        applied: 0,
        skipped,
        high_water,
    })
}

/// Open only an existing projection after acquiring its shared lifetime guard.
/// Legitimate absence never creates a database; replacement reports busy.
///
/// # Errors
/// Store opening and checksum-locked migration failures are propagated.
pub fn open_existing_migrated(
    path: &std::path::Path,
    migrations: &[Migration],
) -> Result<Option<SqliteStore>> {
    let Some(store) = SqliteStore::open_existing(path)? else {
        return Ok(None);
    };
    crate::paths::harden_projection_permissions(path)?;
    apply_migrations(&store, migrations)?;
    Ok(Some(store))
}

/// Transfer exclusive replacement ownership through migration and all uses
/// of the returned store. Its guard carries the only path opened here.
///
/// # Errors
/// Store opening and checksum-locked migration failures are propagated.
pub fn open_replacement_migrated(
    guard: crate::ProjectionReplacementGuard,
    migrations: &[Migration],
) -> Result<SqliteStore> {
    let path = guard.path().to_path_buf();
    let store = SqliteStore::open_replacement(guard)?;
    crate::paths::harden_projection_permissions(&path)?;
    apply_migrations(&store, migrations)?;
    Ok(store)
}

/// [`open_migrated`] with the capture projection's migration set
/// ([`default_migrations`]) — the standard way to open `projection.db`.
///
/// # Errors
/// See [`open_migrated`].
pub fn open_capture_projection(path: impl AsRef<std::path::Path>) -> Result<SqliteStore> {
    let store = open_hardened(path.as_ref())?;
    apply_code_migrations(&store, &CAPTURE_MIGRATIONS)?;
    backfill_capture_session_identities(&store)?;
    Ok(store)
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
    use crate::migrate::fixtures::store;
    use crate::{CanonicalStore, Value};

    #[tokio::test]
    async fn applies_pending_migrations_in_order() {
        let s = store();
        let out = apply_migrations(&s, &default_migrations()).unwrap();
        assert_eq!(out.applied, 22);
        assert_eq!(out.high_water, 22);
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
        assert_eq!(out.applied, 22);
        assert_eq!(out.high_water, 22);
        // The sync runner sees everything applied (same _migrations ledger).
        let again = apply_migrations(&s, &default_migrations()).unwrap();
        assert_eq!(again.applied, 0);
        assert_eq!(again.skipped, 22);
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
            proptest::prop_assert_eq!(rest.high_water, 22);

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
        assert_eq!(out.skipped, 22);
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
        assert_eq!(migs, 22);
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

    fn capture_db() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("projection.db");
        (dir, db)
    }

    #[test]
    fn a_projection_whose_ledger_matches_the_code_skips_the_plan() {
        let (_dir, db) = capture_db();
        let store = open_capture_projection(&db).unwrap();
        let outcome = ledger_matches(&store, &CAPTURE_MIGRATIONS).unwrap();
        assert_eq!(
            outcome,
            MigrationOutcome {
                applied: 0,
                skipped: 22,
                high_water: 22
            }
        );
        // The skip answers what the full plan would have: nothing owed.
        assert_eq!(
            apply_migrations(&store, &default_migrations()).unwrap(),
            outcome
        );
    }

    #[test]
    fn the_code_fingerprint_is_over_the_planners_own_ledger() {
        let s = store();
        apply_migrations(&s, &default_migrations()).unwrap();
        let ledger = s.applied_migrations().unwrap();
        assert_eq!(ledger_fingerprint(ledger), CAPTURE_MIGRATIONS.fingerprint);
        let r = store();
        apply_migrations(&r, &recall_migrations()).unwrap();
        assert!(ledger_matches(&r, &RECALL_MIGRATIONS).is_some());
        assert!(ledger_matches(&r, &CAPTURE_MIGRATIONS).is_none());
    }

    #[test]
    fn a_projection_owed_a_migration_misses_and_is_brought_current() {
        let (_dir, db) = capture_db();
        let old = open_migrated(&db, &default_migrations()[..21]).unwrap();
        assert!(ledger_matches(&old, &CAPTURE_MIGRATIONS).is_none());
        drop(old);
        let store = open_capture_projection(&db).unwrap();
        assert!(ledger_matches(&store, &CAPTURE_MIGRATIONS).is_some());
    }

    #[test]
    fn a_fresh_file_with_no_ledger_misses() {
        let s = store();
        assert!(ledger_matches(&s, &CAPTURE_MIGRATIONS).is_none());
    }

    #[test]
    fn a_tampered_ledger_row_misses_and_the_plan_refuses_it() {
        let (_dir, db) = capture_db();
        let store = open_capture_projection(&db).unwrap();
        store
            .with_conn(|c| {
                c.execute(
                    "UPDATE _migrations SET checksum = 'x' WHERE version = 3",
                    [],
                )
            })
            .unwrap();
        assert!(ledger_matches(&store, &CAPTURE_MIGRATIONS).is_none());
        drop(store);
        let refused = open_capture_projection(&db).err().unwrap();
        assert!(matches!(refused, RiftError::Integrity(_)), "{refused:?}");
    }

    fn backfill_marked(store: &SqliteStore) -> bool {
        store
            .with_conn(|c| {
                c.query_row(
                    "SELECT EXISTS(SELECT 1 FROM projection_meta WHERE key = ?)",
                    [capture::IDENTITY_BACKFILL_COVERED],
                    |r| r.get(0),
                )
            })
            .unwrap()
    }

    fn insert_bare_session(store: &SqliteStore, native: &str) {
        let sid = rift_core::capture_session_row_id("claude-code", native);
        store
            .with_conn(|c| {
                c.execute(
                    "INSERT INTO session \
                     (session_id, agent, model_primary, repo_id, started_at, schema_version) \
                     VALUES (?, 'claude-code', '', 'repo', 1, 1)",
                    [&sid],
                )
            })
            .unwrap();
    }

    #[test]
    fn a_covered_projection_is_marked_once_and_later_opens_skip_the_guard() {
        let (_dir, db) = capture_db();
        let store = open_capture_projection(&db).unwrap();
        assert!(backfill_marked(&store));
        // A bare session added after the mark stays bare: the guard no
        // longer runs, which is the marker's whole point.
        insert_bare_session(&store, "late");
        drop(store);
        let reopened = open_capture_projection(&db).unwrap();
        let rows: i64 = reopened
            .with_conn(|c| {
                c.query_row(
                    "SELECT COUNT(*) FROM projection_meta WHERE key = ?",
                    [capture::IDENTITY_BACKFILL_COVERED],
                    |r| r.get(0),
                )
            })
            .unwrap();
        assert_eq!(rows, 1, "the mark is inserted once, never updated");
    }

    #[test]
    fn an_uncovered_projection_is_not_marked_and_keeps_the_guard() {
        let (_dir, db) = capture_db();
        let old = open_migrated(&db, &default_migrations()).unwrap();
        // No hook-audit fact exists to derive this session's identity from.
        insert_bare_session(&old, "orphan");
        drop(old);
        let store = open_capture_projection(&db).unwrap();
        assert!(!backfill_marked(&store));
    }
}
