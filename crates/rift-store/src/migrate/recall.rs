//! The recall projection's migration set (`recall.db`): the fifteen
//! checksum-locked SQL builders over the recall registry slices and their
//! secondary indexes.
//!
//! A separate module for the same reason as the capture set: each body here
//! is frozen once applied anywhere, and the retired spellings inside the v7
//! and v9 bodies survive as read-compat history.

use super::Migration;

/// The recall projection's migration set (applied to a fresh `recall.db`).
/// [`super::apply_migrations`] creates the meta tables itself, so version 1 is the
/// recall graph schema directly; version 2 adds the outcome-label store;
/// version 3 adds the key-custody table; version 4 adds the memd serve-audit
/// relation; version 5 adds the content→blob-oid map; version 6 adds the
/// durable ingest watermark; version 7 adds per-thread call attribution;
/// version 8 adds the eval experiment records; version 9 adds
/// client connection lifecycle history; version 10 adds the
/// `memd_audit_citation` companion relation; version 11 adds the
/// `ServeChannel`-discriminated `serve_call_event`/
/// `serve_session_event` relations; version 12 adds the worktree-scope
/// mirrors and the grant-reason store; version 13 adds the
/// `memd_audit_session` serve attribution; version 14 adds the
/// `memd_audit_pull_claim` record of what a pull's caller claimed to hold;
/// version 15 adds the `n_window_row` relation a checkpoint's frozen window
/// rows are read back into.
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
        Migration {
            version: 13,
            name: "recall_serve_attribution",
            sql: recall_serve_attribution_schema_sql(),
        },
        Migration {
            version: 14,
            name: "recall_pull_claims",
            sql: recall_pull_claim_schema_sql(),
        },
        Migration {
            version: 15,
            name: "recall_window_rows",
            sql: recall_window_row_schema_sql(),
        },
    ]
}

/// Build the recall-schema migration SQL from the recall table registry (the
/// `recall.db` graph) plus its secondary indexes. Same shape as
/// [`super::canonical_schema_sql`], over [`crate::schema::recall_canonical_tables`].
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

/// The v13 additive recall migration: the `memd_audit_session` companion
/// relation that names the session a serve was made in, and the index the
/// archive's per-session query reads through.
///
/// The table is found by name in the registry rather than through a slice
/// function, because a name cannot drift the way a count can.
fn recall_serve_attribution_schema_sql() -> String {
    const INDEX_SQL: &str = "\
CREATE INDEX IF NOT EXISTS idx_memd_audit_session_session ON memd_audit_session(session_id);\n";
    let tables = || {
        crate::schema::recall_canonical_tables()
            .iter()
            .filter(|table| table.name == "memd_audit_session")
    };
    let mut sql = String::new();
    for table in tables() {
        sql.push_str(&table.create_table_sql());
        sql.push('\n');
        sql.push_str(&table.guard_trigger_sql());
        sql.push('\n');
    }
    sql.push_str(INDEX_SQL);
    for table in tables() {
        sql.push_str(&format!(
            "INSERT INTO _schema_version(table_name, schema_version) VALUES ('{}', 1);\n",
            table.name
        ));
    }
    sql
}

/// The v14 additive recall migration: the `memd_audit_pull_claim`
/// companion relation, one row per pull that stated the revisions its caller
/// held.
///
/// Found by name for the reason v13's relation is. No secondary index: the
/// relation is read by its key or summed whole.
fn recall_pull_claim_schema_sql() -> String {
    let tables = || {
        crate::schema::recall_canonical_tables()
            .iter()
            .filter(|table| table.name == "memd_audit_pull_claim")
    };
    let mut sql = String::new();
    for table in tables() {
        sql.push_str(&table.create_table_sql());
        sql.push('\n');
        sql.push_str(&table.guard_trigger_sql());
        sql.push('\n');
    }
    for table in tables() {
        sql.push_str(&format!(
            "INSERT INTO _schema_version(table_name, schema_version) VALUES ('{}', 1);\n",
            table.name
        ));
    }
    sql
}

/// The v15 additive recall migration: the `n_window_row` relation, one row
/// per line a checkpoint's `window_journal.jsonl` froze.
///
/// Found by name for the reason v13's relation is. No secondary index: no
/// reader asks it a question yet, and an index is additive whenever one does.
fn recall_window_row_schema_sql() -> String {
    let tables = || {
        crate::schema::recall_canonical_tables()
            .iter()
            .filter(|table| table.name == "n_window_row")
    };
    let mut sql = String::new();
    for table in tables() {
        sql.push_str(&table.create_table_sql());
        sql.push('\n');
        sql.push_str(&table.guard_trigger_sql());
        sql.push('\n');
    }
    for table in tables() {
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
    use crate::migrate::apply_migrations;
    use crate::migrate::fixtures::store;

    /// One row per shipped recall version: its migration name, the relations
    /// its SQL must create (with their append-only guards and indexes), and the
    /// next version's relation it must NOT mention. Every body is
    /// checksum-locked once applied, so a table that leaks into an earlier
    /// version moves a checksum on every deployed `recall.db`.
    const VERSION_CONTENTS: &[(&str, &[&str], &[&str])] = &[
        ("recall_core", &[], &["outcome_label"]),
        (
            "recall_outcome_labels",
            &[
                "CREATE TABLE IF NOT EXISTS outcome_label",
                "outcome_label_no_update",
                "idx_outcome_subject",
            ],
            &["key_metadata"],
        ),
        (
            "recall_key_custody",
            &[
                "CREATE TABLE IF NOT EXISTS key_metadata",
                "key_metadata_no_update",
            ],
            &["memd_audit_event"],
        ),
        (
            "recall_memd_audit",
            &[
                "CREATE TABLE IF NOT EXISTS memd_audit_event",
                "memd_audit_event_no_update",
            ],
            &["n_blob_oid_map"],
        ),
        (
            "recall_blob_oid_map",
            &[
                "CREATE TABLE IF NOT EXISTS n_blob_oid_map",
                "n_blob_oid_map_no_update",
            ],
            &["ingest_watermark"],
        ),
        (
            "recall_ingest_watermark",
            &[
                "CREATE TABLE IF NOT EXISTS ingest_watermark",
                "ingest_watermark_no_update",
            ],
            &["memd_call_event"],
        ),
        (
            "recall_memd_calls",
            &[
                "CREATE TABLE IF NOT EXISTS memd_call_event",
                "memd_call_event_no_update",
                "idx_memd_call_thread_recorded",
            ],
            &["eval_run"],
        ),
        (
            "recall_eval_run",
            &[
                "CREATE TABLE IF NOT EXISTS eval_run",
                "eval_run_no_update",
                "eval_run_no_delete",
            ],
            &["mcp_connection_event"],
        ),
        (
            "recall_mcp_connections",
            &[
                "CREATE TABLE IF NOT EXISTS mcp_connection_event",
                "mcp_connection_event_no_update",
                "idx_mcp_connection_client_time",
            ],
            &["memd_audit_citation"],
        ),
        (
            "recall_memd_audit_citation",
            &[
                "CREATE TABLE IF NOT EXISTS memd_audit_citation",
                "memd_audit_citation_no_update",
                "idx_memd_audit_citation_pack_ordinal",
            ],
            &["serve_call_event"],
        ),
        (
            "recall_serve_channel",
            &[
                "CREATE TABLE IF NOT EXISTS serve_call_event",
                "CREATE TABLE IF NOT EXISTS serve_session_event",
                "serve_call_event_no_update",
                "serve_session_event_no_update",
                "idx_serve_call_channel_recorded",
            ],
            &["n_checkpoint_commit"],
        ),
        (
            "recall_worktree_scope",
            &[
                "CREATE TABLE IF NOT EXISTS n_checkpoint_commit",
                "CREATE TABLE IF NOT EXISTS serve_grant_event",
                "serve_grant_event_no_update",
            ],
            &["memd_audit_session"],
        ),
        (
            "recall_serve_attribution",
            &[
                "CREATE TABLE IF NOT EXISTS memd_audit_session",
                "memd_audit_session_no_update",
                "memd_audit_session_no_delete",
                "idx_memd_audit_session_session",
            ],
            &["serve_grant_event", "memd_audit_pull_claim"],
        ),
        (
            "recall_pull_claims",
            &[
                "CREATE TABLE IF NOT EXISTS memd_audit_pull_claim",
                "memd_audit_pull_claim_no_update",
                "memd_audit_pull_claim_no_delete",
            ],
            &["memd_audit_session", "n_window_row"],
        ),
        (
            "recall_window_rows",
            &[
                "CREATE TABLE IF NOT EXISTS n_window_row",
                "n_window_row_no_update",
                "n_window_row_no_delete",
            ],
            &["memd_audit_pull_claim"],
        ),
    ];

    /// The additive contract for the recall projection: every shipped version
    /// creates exactly its own relations and never absorbs the next version's
    /// table, so each checksum-locked body stays byte-stable as the registry
    /// grows.
    #[test]
    fn every_recall_version_creates_only_its_own_relations() {
        let migs = recall_migrations();
        assert_eq!(migs.len(), 15);
        for (version, (name, creates, excludes)) in VERSION_CONTENTS.iter().enumerate() {
            assert_eq!(migs[version].name, *name);
            let sql = &migs[version].sql;
            for needle in *creates {
                assert!(sql.contains(needle), "{name} must create {needle}");
            }
            for needle in *excludes {
                assert!(
                    !sql.contains(needle),
                    "{name} absorbed {needle} (additive checksum lock)"
                );
            }
        }
    }

    /// Applying v1 then v2 to a fresh recall.db is a clean upgrade (no checksum
    /// conflict), and re-running both is idempotent.
    #[test]
    fn recall_migrations_apply_and_rerun_cleanly() {
        let s = store();
        let out = apply_migrations(&s, &recall_migrations()).unwrap();
        assert_eq!(out.applied, 15);
        assert_eq!(out.high_water, 15);
        let again = apply_migrations(&s, &recall_migrations()).unwrap();
        assert_eq!(again.applied, 0);
        assert_eq!(again.skipped, 15);
    }
}

#[cfg(test)]
mod pull_claim_tests {
    use super::*;
    use crate::migrate::apply_migrations;
    use crate::migrate::fixtures::store;

    const ROW: &str = "INSERT INTO memd_audit_pull_claim \
        (audit_id, stated_count, recorded_count, unrecorded_count, basis, schema_version) \
        VALUES ('a1', 3, 2, 1, 'stated_recorded', 1)";

    #[test]
    fn a_store_at_the_previous_version_gains_the_pull_claim_relation_alone() {
        let s = store();
        let migs = recall_migrations();
        let before = apply_migrations(&s, &migs[..13]).unwrap();
        assert_eq!(before.high_water, 13);
        let after = apply_migrations(&s, &migs[..14]).unwrap();
        assert_eq!(after.applied, 1);
        assert_eq!(after.high_water, 14);
        assert_eq!(s.execute_raw(ROW).unwrap(), 1);
    }

    #[test]
    fn a_fresh_store_takes_a_pull_claim_row() {
        let s = store();
        apply_migrations(&s, &recall_migrations()).unwrap();
        assert_eq!(s.execute_raw(ROW).unwrap(), 1);
    }

    #[test]
    fn a_pull_claim_row_can_be_neither_updated_nor_deleted() {
        let s = store();
        apply_migrations(&s, &recall_migrations()).unwrap();
        s.execute_raw(ROW).unwrap();
        let update = s.execute_raw("UPDATE memd_audit_pull_claim SET stated_count = 9");
        let delete = s.execute_raw("DELETE FROM memd_audit_pull_claim");
        assert!(matches!(update, Err(rift_core::RiftError::Immutability(_))));
        assert!(matches!(delete, Err(rift_core::RiftError::Immutability(_))));
    }

    #[test]
    fn a_basis_outside_the_two_a_stated_hash_leads_to_is_refused() {
        let s = store();
        apply_migrations(&s, &recall_migrations()).unwrap();
        let none_stated = ROW.replace("stated_recorded", "none_stated");
        assert!(s.execute_raw(&none_stated).is_err());
        let unverified = ROW.replace("stated_recorded", "unverified");
        assert_eq!(s.execute_raw(&unverified).unwrap(), 1);
    }
}

#[cfg(test)]
mod window_row_tests {
    use super::*;
    use crate::migrate::apply_migrations;
    use crate::migrate::fixtures::store;

    const ROW: &str = "INSERT INTO n_window_row \
        (row_id, window_key, subagent, kind, turn, at, path, content_sha256, hash_old, \
         range_lo, range_hi, total_lines, sites, shift, full, reason, file_revision, \
         checkpoint_id, schema_version) \
        VALUES ('r1', 'w1', NULL, 'read', 1, 2, 'src/a.rs', 'h1', NULL, 1, 40, 400, \
         NULL, NULL, NULL, NULL, 'fr1', 'c1', 1)";

    const PULL_CLAIM: &str = "INSERT INTO memd_audit_pull_claim \
        (audit_id, stated_count, recorded_count, unrecorded_count, basis, schema_version) \
        VALUES ('a1', 3, 2, 1, 'stated_recorded', 1)";

    #[test]
    fn a_store_at_the_previous_version_gains_the_window_relation_and_keeps_its_rows() {
        let s = store();
        let migs = recall_migrations();
        let before = apply_migrations(&s, &migs[..14]).unwrap();
        assert_eq!(before.high_water, 14);
        assert_eq!(s.execute_raw(PULL_CLAIM).unwrap(), 1);
        let after = apply_migrations(&s, &migs).unwrap();
        assert_eq!(after.applied, 1);
        assert_eq!(after.high_water, 15);
        let kept: i64 = s
            .with_conn(|c| {
                c.query_row("SELECT COUNT(*) FROM memd_audit_pull_claim", [], |r| {
                    r.get(0)
                })
            })
            .unwrap();
        assert_eq!(kept, 1);
        assert_eq!(s.execute_raw(ROW).unwrap(), 1);
    }

    #[test]
    fn a_window_row_can_be_neither_updated_nor_deleted() {
        let s = store();
        apply_migrations(&s, &recall_migrations()).unwrap();
        s.execute_raw(ROW).unwrap();
        let update = s.execute_raw("UPDATE n_window_row SET turn = 9");
        let delete = s.execute_raw("DELETE FROM n_window_row");
        assert!(matches!(update, Err(rift_core::RiftError::Immutability(_))));
        assert!(matches!(delete, Err(rift_core::RiftError::Immutability(_))));
    }

    #[test]
    fn a_kind_the_journal_does_not_declare_is_refused() {
        let s = store();
        apply_migrations(&s, &recall_migrations()).unwrap();
        assert!(s
            .execute_raw(&ROW.replace("'read'", "'summarize'"))
            .is_err());
        assert_eq!(
            s.execute_raw(&ROW.replace("'read'", "'prevented'"))
                .unwrap(),
            1
        );
    }
}
