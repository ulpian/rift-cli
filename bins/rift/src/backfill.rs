//! `rift backfill` — put back what a projection rebuild could not derive.
//!
//! A projection is *rebuildable* from the checkpoint branch, and that is the
//! design. It is not, however, rebuildable from the branch **alone**: two
//! classes of fact live outside it.
//!
//! * **Derivable, but not from checkpoints.** `file_revision` and
//!   `changed_hunk` describe commits, and the commits are still there — the
//!   diff is a pure function of the object store. A rebuild does not produce
//!   them because it walks checkpoints, not history. [`derive_from_history`]
//!   walks history.
//! * **Not derivable from shards written before the usage artifact.** What
//!   such a session was, which model it ran, what a call cost when it was
//!   priced: nothing in git recorded it, so nothing can recompute it. The only
//!   copy is in a projection that was set aside.
//!   [`import_from`] copies those rows back, through the ordinary append-only
//!   writers.
//! * **Not derivable from shards at all, on the recall side.** A serve a person
//!   made at a terminal, a serve booked before shards froze serves, a grant's
//!   typed reason: a recall rebuild sets the old projection aside, and
//!   `--recall-from` reads those rows back out of it ([`recall`]).
//!
//! **Why this is its own verb rather than a flag on `rift rebuild`.** Rebuild
//! means one thing — the artifact-bridge walk over the checkpoint branch,
//! idempotent by content address, no arguments. This walks git history and
//! optionally reads a foreign file by path. Folding them would give `rebuild` a
//! `--from <path>` it has no business accepting, and make one verb mean three
//! things.
//!
//! **Source canonical rows are never written.** An explicitly selected
//! historical source uses [`rift_store::SqliteStore::open_historical_snapshot`],
//! READ_ONLY with one pinned read transaction, including preserved WAL pages.
//! SQLite may manage WAL/SHM files; no DB or concurrency marker is created.
//! Live family/destination identities are refused, including aliases. Unlike this route, the two ordinary openers in this workspace would CREATE the
//! file if the path were mistyped, or MIGRATE it — and migrating the artifact
//! you set aside in order to preserve it is a way to lose it.
//!
//! **Everything is insert-only, and re-running converges** — which is what makes
//! a repair safe to attempt twice after an interruption. It is not free: four of
//! the five writers absorb a replay through `insert_or_ignore`, but
//! `record_cost_fact` is a plain insert and the second run died on
//! `sqlite constraint-violation (code 1555)` the first time this was bottled.
//! That one asks before it writes, here rather than in the shared writer —
//! changing what a duplicate cost fact means for every caller in the workspace
//! is a money-semantics decision and belongs to `rift-shadow-cost`.

mod recall;

use std::path::Path;

use rift_core::Result;
use rift_git::BlobReader;
use rift_shadow::attribution::capture_commit;
use rift_shadow::persist_commit_capture;
use rift_store::{
    record_cost_fact, record_model_call, record_session, record_token_usage, record_turn,
    CanonicalStore, CostFact, ModelCallRow, SessionRow, SqliteStore, TokenUsageRow, TurnRow,
};

use crate::cli::{exit, Outcome};

/// What one table's import did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TableCount {
    /// The canonical table.
    pub(crate) table: &'static str,
    /// Rows read from the source.
    pub(crate) read: usize,
    /// Rows the writer actually inserted (0 on a converged re-run).
    pub(crate) inserted: usize,
}

/// What a whole backfill run did — the report, and the evidence a second run
/// converged.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct BackfillReport {
    /// Commits walked in the derive phase.
    pub(crate) commits: usize,
    /// Files whose revision + hunks were persisted.
    pub(crate) files: usize,
    /// Per-table import counts, in insert order.
    pub(crate) imported: Vec<TableCount>,
    /// Per-relation counts read back into the recall projection.
    pub(crate) recalled: Vec<TableCount>,
}

impl BackfillReport {
    /// The stable human rendering.
    pub(crate) fn render(&self, dry_run: bool) -> String {
        use std::fmt::Write as _;
        let mut s = String::new();
        let _ = writeln!(
            s,
            "backfill{}: {} commit(s) walked, {} changed file(s) persisted",
            if dry_run {
                " (dry run: nothing written)"
            } else {
                ""
            },
            self.commits,
            self.files
        );
        for t in self.imported.iter().chain(&self.recalled) {
            let _ = writeln!(s, "  {}: {} read, {} inserted", t.table, t.read, t.inserted);
        }
        // Attribution is NOT here, and its absence is the honest half of the
        // report: `persist_commit_capture` anchors attribution to a checkpoint,
        // a historical commit has none, and minting an anchor to fill the column
        // would write a false fact into a table that can never withdraw it.
        let _ = writeln!(
            s,
            "  attribution: not derived — a historical commit has no checkpoint \
             to anchor it to, and an invented anchor is a permanent false fact"
        );
        s
    }
}

/// Walk `HEAD`'s first-parent history oldest-first.
///
/// First-parent because that is exactly what the post-commit hook covered: one
/// commit diffed against its own first parent. Oldest-first so two runs visit
/// the same commits in the same order — the writers are idempotent either way,
/// but a stable order makes the reported counts comparable between runs.
fn first_parent_history(reader: &impl BlobReader, head: &str) -> Result<Vec<String>> {
    let mut out = Vec::new();
    let mut cur = Some(head.to_owned());
    while let Some(c) = cur {
        cur = reader.parent_of(&c)?;
        out.push(c);
    }
    out.reverse();
    Ok(out)
}

/// Persist the `file_revision`/`changed_hunk` facts for every commit in
/// `HEAD`'s first-parent history. Returns `(commits, files)`.
///
/// # Errors
/// [`rift_core::RiftError`] on a blob read or insert failure.
pub(crate) async fn derive_from_history(
    reader: &impl BlobReader,
    store: &impl CanonicalStore,
    head: &str,
    dry_run: bool,
) -> Result<(usize, usize)> {
    let commits = first_parent_history(reader, head)?;
    let mut files = 0usize;
    for commit in &commits {
        let capture = capture_commit(reader, commit)?;
        files += capture.files.len();
        if !dry_run {
            persist_commit_capture(store, &capture, None).await?;
        }
    }
    Ok((commits.len(), files))
}

/// The tables imported from a set-aside projection, in FK-documented insert
/// order.
///
/// The set is a closure, not a wish-list. `cost_fact` is the row an operator
/// actually misses, but a cost whose `call_id` names nothing restores a number
/// and destroys its provenance — so its parents come with it. `session` is
/// here for its own sake: which agent, which model, when it started is a fact
/// no git object recorded.
const IMPORT_ORDER: &[&str] = &["session", "turn", "model_call", "token_usage", "cost_fact"];

/// Whether `source` looks like a capture projection at all.
///
/// A path that is a database but not a projection must be REFUSED, not
/// imported: reading five tables out of an arbitrary SQLite file and inserting
/// whatever comes back is how a repair becomes a corruption.
async fn is_a_projection(source: &SqliteStore) -> bool {
    source
        .query(
            "SELECT name FROM sqlite_master WHERE type='table' AND name='session'",
            &[],
        )
        .await
        .is_ok_and(|rows| !rows.is_empty())
}

/// Copy the non-derivable rows out of a set-aside projection at `path` into
/// `store`, through the ordinary append-only writers.
///
/// # Errors
/// [`rift_core::RiftError::Storage`] if the source cannot be opened read-only,
/// is not a projection, or an insert fails.
pub(crate) async fn import_from(
    store: &(impl CanonicalStore + rift_store::Store),
    selected: rift_store::HistoricalProjectionSnapshot,
    dry_run: bool,
) -> Result<Vec<TableCount>> {
    selected.refuse_live_destination(Path::new(store.descriptor().path()))?;
    let source = SqliteStore::open_historical_snapshot(selected)?;
    if !is_a_projection(&source).await {
        return Err(rift_core::RiftError::storage(
            "the source is not a capture projection (no `session` table) — refusing to \
             import rows out of a file whose shape is unknown",
        ));
    }
    let mut out = Vec::new();
    for table in IMPORT_ORDER {
        out.push(import_table(store, &source, table, dry_run).await?);
    }
    Ok(out)
}

/// Import one table, by name.
///
/// A dispatcher over one function per table, rather than one function with
/// five arms: each arm names its columns explicitly instead of `SELECT *`,
/// because the row struct is the contract and a column added to the schema
/// later must be a visible edit here — never a silent shift in ordinal
/// positions.
async fn import_table(
    store: &impl CanonicalStore,
    source: &SqliteStore,
    table: &'static str,
    dry_run: bool,
) -> Result<TableCount> {
    let (read, inserted) = match table {
        "session" => import_sessions(store, source, dry_run).await?,
        "turn" => import_turns(store, source, dry_run).await?,
        "model_call" => import_model_calls(store, source, dry_run).await?,
        "token_usage" => import_token_usage(store, source, dry_run).await?,
        "cost_fact" => import_cost_facts(store, source, dry_run).await?,
        _ => (0, 0),
    };
    Ok(TableCount {
        table,
        read,
        inserted,
    })
}

/// `(read, inserted)` for one table.
type Imported = (usize, usize);

async fn import_sessions(
    store: &impl CanonicalStore,
    source: &SqliteStore,
    dry_run: bool,
) -> Result<Imported> {
    let rows = source
        .query(
            "SELECT session_id, agent, model_primary, repo_id, started_at, schema_version \
             FROM session",
            &[],
        )
        .await?;
    let mut inserted = 0usize;
    for r in &rows {
        let row = SessionRow {
            session_id: r.get_text(0)?,
            agent: r.get_text(1)?,
            model_primary: r.get_text(2)?,
            repo_id: r.get_text(3)?,
            started_at: r.get_i64(4)?,
            schema_version: r.get_i64(5)?,
        };
        if !dry_run && record_session(store, &row).await? {
            inserted += 1;
        }
    }
    Ok((rows.len(), inserted))
}

async fn import_turns(
    store: &impl CanonicalStore,
    source: &SqliteStore,
    dry_run: bool,
) -> Result<Imported> {
    let rows = source
        .query(
            "SELECT turn_id, session_id, ordinal, started_at, schema_version FROM turn",
            &[],
        )
        .await?;
    let mut inserted = 0usize;
    for r in &rows {
        let row = TurnRow {
            turn_id: r.get_text(0)?,
            session_id: r.get_text(1)?,
            ordinal: r.get_i64(2)?,
            started_at: r.get_i64(3)?,
            schema_version: r.get_i64(4)?,
        };
        if !dry_run && record_turn(store, &row).await? {
            inserted += 1;
        }
    }
    Ok((rows.len(), inserted))
}

async fn import_model_calls(
    store: &impl CanonicalStore,
    source: &SqliteStore,
    dry_run: bool,
) -> Result<Imported> {
    let rows = source
        .query(
            "SELECT call_id, turn_id, model, started_at, schema_version FROM model_call",
            &[],
        )
        .await?;
    let mut inserted = 0usize;
    for r in &rows {
        let row = ModelCallRow {
            call_id: r.get_text(0)?,
            turn_id: r.get_text(1)?,
            model: r.get_text(2)?,
            started_at: r.get_i64(3)?,
            schema_version: r.get_i64(4)?,
        };
        if !dry_run && record_model_call(store, &row).await? {
            inserted += 1;
        }
    }
    Ok((rows.len(), inserted))
}

async fn import_token_usage(
    store: &impl CanonicalStore,
    source: &SqliteStore,
    dry_run: bool,
) -> Result<Imported> {
    let rows = source
        .query(
            "SELECT call_id, uncached_input, cached_input, cache_creation, output, \
             counter_method, schema_version FROM token_usage",
            &[],
        )
        .await?;
    let mut inserted = 0usize;
    for r in &rows {
        let row = TokenUsageRow {
            call_id: r.get_text(0)?,
            uncached_input: r.get_i64(1)?,
            cached_input: r.get_i64(2)?,
            cache_creation: r.get_i64(3)?,
            output: r.get_i64(4)?,
            counter_method: r.get_text(5)?,
            schema_version: r.get_i64(6)?,
        };
        if !dry_run && record_token_usage(store, &row).await? {
            inserted += 1;
        }
    }
    Ok((rows.len(), inserted))
}

/// **`record_cost_fact` is NOT idempotent, and the bottle is what found that.**
///
/// Every other writer here goes through `insert_or_ignore` and absorbs a
/// replay; this one is a plain insert, so the second import run died on
/// `sqlite constraint-violation (code 1555)` the first time this was bottled —
/// and a repair that cannot be re-run after an interruption is not a repair.
///
/// The fix is LOCAL on purpose. Making the shared writer OR-IGNORE would change
/// what a duplicate cost fact means for every caller in the workspace, and money
/// semantics belong to `rift-shadow-cost`, not to a backfill verb
/// So this asks first. The id is content-addressed, so a row
/// already present IS this row.
async fn import_cost_facts(
    store: &impl CanonicalStore,
    source: &SqliteStore,
    dry_run: bool,
) -> Result<Imported> {
    let rows = source
        .query(
            "SELECT cost_fact_id, call_id, amount_micros, valid_from, valid_to, recorded_at, \
             supersedes_id, schema_version FROM cost_fact",
            &[],
        )
        .await?;
    let mut inserted = 0usize;
    for r in &rows {
        let row = CostFact {
            cost_fact_id: r.get_text(0)?,
            call_id: r.get_text(1)?,
            amount_micros: r.get_i64(2)?,
            valid_from: r.get_i64(3)?,
            valid_to: r.get_opt_i64(4)?,
            recorded_at: r.get_i64(5)?,
            supersedes_id: r.get_opt_text(6)?,
            schema_version: r.get_i64(7)?,
        };
        let present = !store
            .query(
                "SELECT 1 FROM cost_fact WHERE cost_fact_id = ?1",
                &[rift_store::Value::text(row.cost_fact_id.clone())],
            )
            .await?
            .is_empty();
        if !dry_run && !present {
            record_cost_fact(store, &row).await?;
            inserted += 1;
        }
    }
    Ok((rows.len(), inserted))
}

/// `rift backfill` — derive what git still knows, and re-import what only a
/// set-aside projection remembers.
///
/// # Errors
/// [`rift_core::RiftError`] if the repository or projection cannot be opened.
pub(crate) fn cmd_backfill(root: &Path, args: &crate::cli::BackfillArgs) -> Result<Outcome> {
    let Ok(repo) = rift_git::discover(root) else {
        return Ok(Outcome::coded(
            exit::NOT_INIT,
            "backfill: not a git repository",
            serde_json::json!({ "error": "not_a_repo" }),
        ));
    };
    let Some(head) = rift_git::resolve_commit(&repo, "HEAD")? else {
        return Ok(Outcome::coded(
            exit::PRECONDITION,
            "backfill: HEAD resolves to nothing — there is no history to walk",
            serde_json::json!({ "error": "unborn_head" }),
        ));
    };
    let selected_capture = select_family_source(root, args.from.as_deref())?;
    let selected_recall = select_family_source(root, args.recall_from.as_deref())?;

    let reader = rift_git::GixBlobReader::open(&repo.work_tree)?;
    let store =
        rift_store::open_capture_projection(crate::hooks::projection_db_at(&repo.work_tree)?)?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .map_err(rift_core::RiftError::from)?;

    let report = rt.block_on(async {
        let (commits, files) = derive_from_history(&reader, &store, &head, args.dry_run).await?;
        let imported = match selected_capture {
            Some(selected) => import_from(&store, selected, args.dry_run).await?,
            None => Vec::new(),
        };
        let recalled = match selected_recall {
            Some(selected) => recall_from(root, selected, args.dry_run).await?,
            None => Vec::new(),
        };
        Ok::<_, rift_core::RiftError>(BackfillReport {
            commits,
            files,
            imported,
            recalled,
        })
    })?;

    let counts = |tables: &[TableCount]| {
        tables
            .iter()
            .map(
                |t| serde_json::json!({ "table": t.table, "read": t.read, "inserted": t.inserted }),
            )
            .collect::<Vec<_>>()
    };
    let json = serde_json::json!({
        "backfill": if args.dry_run { "dry-run" } else { "applied" },
        "commits": report.commits,
        "files": report.files,
        "imported": counts(&report.imported),
        "recall_imported": counts(&report.recalled),
    });
    Ok(Outcome::ok(report.render(args.dry_run), json))
}

/// Read the serve relations back out of the set-aside recall projection at
/// `source`, into the family's own recall projection.
///
/// A dry run opens only the source: the target is not opened, so it is not
/// created either.
///
/// # Errors
/// [`rift_core::RiftError`] when the family cannot be resolved, the source is
/// refused, or a write fails.
fn select_family_source(
    root: &Path,
    path: Option<&str>,
) -> Result<Option<rift_store::HistoricalProjectionSnapshot>> {
    let Some(path) = path else {
        return Ok(None);
    };
    let selected = rift_store::HistoricalProjectionSnapshot::user_selected(path)?;
    selected.refuse_live_destination(&crate::family::projection_db(root)?)?;
    selected.refuse_live_destination(&crate::family::recall_db(root)?)?;
    Ok(Some(selected))
}

async fn recall_from(
    root: &Path,
    source: rift_store::HistoricalProjectionSnapshot,
    dry_run: bool,
) -> Result<Vec<TableCount>> {
    if dry_run {
        return recall::count_recall_rows(source).await;
    }
    let db = crate::family::recall_db(root)?;
    let store = rift_store::open_migrated(&db, &rift_store::recall_migrations())?;
    recall::import_recall_from(&store, source).await
}

#[cfg(test)]
mod tests {
    #[test]
    fn both_cli_import_options_refuse_both_live_family_identities_including_dry_run() {
        let checkout = tempfile::tempdir().unwrap();
        for args in [
            vec!["init", "--quiet"],
            vec!["commit", "--quiet", "--allow-empty", "-m", "fixture"],
        ] {
            assert!(std::process::Command::new("git")
                .args([
                    "-c",
                    "user.name=Fixture",
                    "-c",
                    "user.email=fixture-id",
                    "-c",
                    "core.hooksPath=/dev/null"
                ])
                .args(args)
                .env("GIT_AUTHOR_DATE", "2026-10-01T12:00:00Z")
                .env("GIT_COMMITTER_DATE", "2026-10-01T12:00:00Z")
                .current_dir(checkout.path())
                .status()
                .unwrap()
                .success());
        }
        for live in [
            crate::family::projection_db(checkout.path()).unwrap(),
            crate::family::recall_db(checkout.path()).unwrap(),
        ] {
            std::fs::create_dir_all(live.parent().unwrap()).unwrap();
            std::fs::write(&live, b"live bytes").unwrap();
            let alias = live.with_extension("alias");
            std::fs::hard_link(&live, &alias).unwrap();
            for selected in [&live, &alias] {
                for dry_run in [false, true] {
                    for recall in [false, true] {
                        let source = Some(selected.display().to_string());
                        let args = crate::cli::BackfillArgs {
                            from: (!recall).then(|| source.clone()).flatten(),
                            recall_from: recall.then(|| source.clone()).flatten(),
                            dry_run,
                        };
                        let error = cmd_backfill(checkout.path(), &args).unwrap_err();
                        assert!(
                            error.to_string().contains("aliases a live projection"),
                            "{error}"
                        );
                        assert_eq!(std::fs::read(&live).unwrap(), b"live bytes");
                    }
                }
            }
        }
    }
    fn historical_source(
        path: &Path,
        destination: &Path,
    ) -> Result<rift_store::HistoricalProjectionSnapshot> {
        let selected = rift_store::HistoricalProjectionSnapshot::user_selected(path)?;
        selected.refuse_live_destination(destination)?;
        Ok(selected)
    }

    async fn import_from(
        store: &SqliteStore,
        path: &Path,
        dry_run: bool,
    ) -> Result<Vec<TableCount>> {
        let selected = rift_store::HistoricalProjectionSnapshot::user_selected(path)?;
        super::import_from(store, selected, dry_run).await
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn historical_sources_refuse_canonical_symlink_parent_and_hardlink_destinations() {
        let dir = tempfile::tempdir().unwrap();
        let parent = dir.path().join("live");
        std::fs::create_dir(&parent).unwrap();
        let destination = parent.join("projection.db");
        drop(rift_store::SqliteStore::open(&destination).unwrap());
        let hardlink = dir.path().join("hardlink.db");
        std::fs::hard_link(&destination, &hardlink).unwrap();
        let symlink = dir.path().join("symlink.db");
        let parent_alias = dir.path().join("parent-alias");
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&destination, &symlink).unwrap();
            std::os::unix::fs::symlink(&parent, &parent_alias).unwrap();
        }
        #[cfg(windows)]
        {
            std::os::windows::fs::symlink_file(&destination, &symlink)
                .expect("Windows alias regression requires Developer Mode or symlink privilege");
            std::os::windows::fs::symlink_dir(&parent, &parent_alias)
                .expect("Windows alias regression requires Developer Mode or symlink privilege");
        }
        for source in [
            &destination,
            &hardlink,
            &symlink,
            &parent_alias.join("projection.db"),
        ] {
            assert!(historical_source(source, &destination).is_err());
        }
        let distinct = dir.path().join("selected-history.db");
        std::fs::write(&distinct, b"historical").unwrap();
        assert!(historical_source(&distinct, &destination).is_ok());
        assert_eq!(std::fs::read(&distinct).unwrap(), b"historical");
    }
    use super::*;
    use rift_git::{MemoryBlobReader, TreeEntry};

    /// The bottle's source: a projection carrying the five non-derivable
    /// tables, written through the SAME writers the import uses, so the
    /// fixture's shape is the product's shape rather than a hand-rolled guess.
    async fn seed_source(path: &Path) {
        let s = rift_store::open_capture_projection(path).unwrap();
        record_session(
            &s,
            &SessionRow {
                session_id: "sess-1".into(),
                agent: "claude-code".into(),
                model_primary: "claude-sonnet-4-5".into(),
                repo_id: "repo-1".into(),
                started_at: 1_000,
                schema_version: 1,
            },
        )
        .await
        .unwrap();
        record_turn(
            &s,
            &TurnRow {
                turn_id: "turn-1".into(),
                session_id: "sess-1".into(),
                ordinal: 0,
                started_at: 1_001,
                schema_version: 1,
            },
        )
        .await
        .unwrap();
        record_model_call(
            &s,
            &ModelCallRow {
                call_id: "call-1".into(),
                turn_id: "turn-1".into(),
                model: "claude-sonnet-4-5".into(),
                started_at: 1_002,
                schema_version: 1,
            },
        )
        .await
        .unwrap();
        record_token_usage(
            &s,
            &TokenUsageRow {
                call_id: "call-1".into(),
                uncached_input: 10,
                cached_input: 20,
                cache_creation: 5,
                output: 7,
                counter_method: "provider_reported".into(),
                schema_version: 1,
            },
        )
        .await
        .unwrap();
        record_cost_fact(
            &s,
            &CostFact {
                cost_fact_id: "cost-1".into(),
                call_id: "call-1".into(),
                amount_micros: 4_242,
                valid_from: 1_003,
                valid_to: None,
                recorded_at: 1_004,
                supersedes_id: None,
                schema_version: 1,
            },
        )
        .await
        .unwrap();
    }

    /// SHA-256 of the file's bytes, through the hash layer this workspace
    /// already links (no new dependency for a test helper).
    fn sha256_of(path: &Path) -> String {
        rift_hashchain::leaf_hash(&std::fs::read(path).unwrap()).to_hex()
    }

    async fn count(store: &impl CanonicalStore, table: &str) -> i64 {
        store.count(table).await.unwrap()
    }

    /// **The bottle, end to end on a synthetic source.** The real
    /// `.pre-repair` file is the owner's data and this test must never need it:
    /// the fixture is shape-matched by construction because it is written
    /// through the product's own writers.
    #[tokio::test]
    async fn import_restores_the_non_derivable_rows_and_converges_on_a_re_run() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("projection.db.pre-repair");
        seed_source(&src).await;
        let before = sha256_of(&src);

        let target = rift_store::open_capture_projection(tmp.path().join("target.db")).unwrap();
        let counts = import_from(&target, &src, false).await.unwrap();
        assert_eq!(
            counts
                .iter()
                .map(|c| (c.table, c.read, c.inserted))
                .collect::<Vec<_>>(),
            vec![
                ("session", 1, 1),
                ("turn", 1, 1),
                ("model_call", 1, 1),
                ("token_usage", 1, 1),
                ("cost_fact", 1, 1),
            ]
        );
        for t in ["session", "turn", "model_call", "token_usage", "cost_fact"] {
            assert_eq!(count(&target, t).await, 1, "{t} restored");
        }
        // The cost survived as a VALUE, not as a shape: the number is the point.
        let rows = target
            .query("SELECT amount_micros FROM cost_fact", &[])
            .await
            .unwrap();
        assert_eq!(rows[0].get_i64(0).unwrap(), 4_242);

        // READ-ONLY, measured rather than taken from the flag.
        assert_eq!(sha256_of(&src), before, "the source file must not change");

        // CONVERGENCE: a second run inserts nothing new and the totals hold.
        let again = import_from(&target, &src, false).await.unwrap();
        assert!(
            again.iter().all(|c| c.read == 1 && c.inserted == 0),
            "the source is read again in full and nothing new is written: {again:?}"
        );
        for t in ["session", "turn", "model_call", "token_usage", "cost_fact"] {
            assert_eq!(count(&target, t).await, 1, "{t} did not duplicate");
        }
        assert_eq!(sha256_of(&src), before, "still untouched after two runs");
    }

    /// `--dry-run` writes nothing at all — checked on the target's rows AND on
    /// the source's bytes.
    #[tokio::test]
    async fn a_dry_run_writes_nothing_anywhere() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("projection.db.pre-repair");
        seed_source(&src).await;
        let before = sha256_of(&src);
        let target = rift_store::open_capture_projection(tmp.path().join("target.db")).unwrap();
        let counts = import_from(&target, &src, true).await.unwrap();
        assert!(
            counts.iter().all(|c| c.read == 1 && c.inserted == 0),
            "a dry run reads and reports, and inserts nothing: {counts:?}"
        );
        for t in ["session", "turn", "model_call", "token_usage", "cost_fact"] {
            assert_eq!(count(&target, t).await, 0, "{t} untouched");
        }
        assert_eq!(sha256_of(&src), before);
    }

    /// A file that is not a projection is REFUSED by name. Reading five tables
    /// out of an arbitrary database and inserting what comes back is how a
    /// repair becomes a corruption.
    #[tokio::test]
    async fn a_source_that_is_not_a_projection_is_refused_not_imported() {
        let tmp = tempfile::tempdir().unwrap();
        // A real SQLite database that is not a projection: opened and left
        // with no canonical schema at all.
        let stranger = tmp.path().join("notes.db");
        drop(rift_store::SqliteStore::open(&stranger).unwrap());
        let target = rift_store::open_capture_projection(tmp.path().join("target.db")).unwrap();
        let err = import_from(&target, &stranger, false)
            .await
            .expect_err("an unknown shape is refused");
        assert!(
            err.to_string().contains("not a capture projection"),
            "the refusal says what it refused: {err}"
        );
    }

    /// A missing source is an error, never a silent empty import — which is
    /// exactly what the ordinary opener would have produced, because it CREATES.
    #[tokio::test]
    async fn a_missing_source_is_an_error_not_an_empty_import() {
        let tmp = tempfile::tempdir().unwrap();
        let target = rift_store::open_capture_projection(tmp.path().join("target.db")).unwrap();
        let missing = tmp.path().join("nope.db");
        assert!(import_from(&target, &missing, false).await.is_err());
        assert!(
            !missing.exists(),
            "and the read-only open did not create the file it was pointed at"
        );
    }

    /// The derive half: the per-commit diff facts come back from git history
    /// alone, and re-running is a no-op.
    #[tokio::test]
    async fn history_derives_the_diff_facts_and_re_running_changes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let store = rift_store::open_capture_projection(tmp.path().join("t.db")).unwrap();
        let reader = MemoryBlobReader::new()
            .with_blob("A0", b"one\ntwo\n")
            .with_blob("A1", b"one\nTWO\n")
            .with_tree(
                "c0",
                vec![TreeEntry {
                    path: "a.txt".into(),
                    blob_oid: "A0".into(),
                }],
            )
            .with_tree(
                "c1",
                vec![TreeEntry {
                    path: "a.txt".into(),
                    blob_oid: "A1".into(),
                }],
            )
            .with_parent("c1", "c0");

        let (commits, files) = derive_from_history(&reader, &store, "c1", false)
            .await
            .unwrap();
        assert_eq!(commits, 2, "c1 and its parent c0");
        assert!(files >= 1);
        let revisions = count(&store, "file_revision").await;
        let hunks = count(&store, "changed_hunk").await;
        assert!(revisions >= 1 && hunks >= 1, "diff facts landed");
        // Attribution is deliberately absent: no checkpoint anchors these.
        assert_eq!(count(&store, "attribution").await, 0);

        // Idempotent: the content-addressed keys absorb a second walk.
        derive_from_history(&reader, &store, "c1", false)
            .await
            .unwrap();
        assert_eq!(count(&store, "file_revision").await, revisions);
        assert_eq!(count(&store, "changed_hunk").await, hunks);
    }

    /// The walk is oldest-first over the FIRST-parent chain, which is the
    /// history the post-commit hook covered.
    #[test]
    fn the_history_walk_is_first_parent_and_oldest_first() {
        let reader = MemoryBlobReader::new()
            .with_parent("c2", "c1")
            .with_parent("c1", "c0");
        assert_eq!(
            first_parent_history(&reader, "c2").unwrap(),
            vec!["c0".to_owned(), "c1".to_owned(), "c2".to_owned()]
        );
    }
}
