//! `rift session stop` and `rift clean`: the two verbs that remove things.
//!
//! Both are held to one rule — a transient whose loss costs a recomputation
//! goes, and anything canonical is out of scope by construction — so the
//! predicates live together where they can be read against each other.

use std::path::Path;

use rift_core::Result;

use super::journals::journal_sessions;
use super::{exit, safe, Outcome};
use crate::provenance::Label;

/// `rift session stop [ID]` — finalize through the state machine's
/// IdleTimeout → condense → CondenseDone path (the crashed-session sweep's
/// exact machinery, timeout zero: "stale" is now). With an ID, only that
/// session is even looked at.
pub(crate) fn stop(root: &Path, target: Option<&str>) -> Result<Outcome> {
    use rift_shadow::hooks::session_cache::read_cache;
    let repo = rift_git::discover(root)?;
    if let Some(id) = target {
        match read_cache(&repo.git_dir, id).ok().flatten() {
            None => {
                return Ok(Outcome::coded(
                    exit::PRECONDITION,
                    format!("no session {} in this worktree's journal", safe(id)),
                    serde_json::json!({ "error": "unknown_session", "session_id": id }),
                ))
            }
            Some(c) if c.state == "ended" => {
                return Ok(Outcome::ok(
                    format!("session {} is already ended", safe(id)),
                    serde_json::json!({ "finalized": [], "deferred": [], "already_ended": [id] }),
                ))
            }
            Some(_) => {}
        }
    }
    let now = crate::hooks::now_ms();
    let sweep = crate::hooks::sweep_stale_sessions(root, now, 0, target)?;
    // The machine's sweep reaches only its live states (active / compacting /
    // ending). A cache parked in `idle` is in the machine's PRE-SESSION state:
    // `(Idle, Start)` is the only edge out and `(Idle, End)` is deliberately
    // illegal — there is no open phase to condense. Five such caches sat on
    // this repository for 3–57 days, counted STUCK by doctor and reachable by
    // nothing. The explicit stop verb closes them at the
    // journal level: state → `ended`, no transition invented, and it says so.
    // Stop-all keeps a one-hour guard so a cache an agent wrote moments before
    // its `Start` is never closed out from under it; a targeted stop is the
    // operator naming the session, so it closes regardless of age.
    let closed_idle = close_idle_caches(root, now, target)?;
    let mut human = String::new();
    for id in &sweep.finalized {
        human.push_str(&format!(
            "finalized {} (condensed to a checkpoint)\n",
            Label::of(id)
        ));
    }
    for id in &sweep.condense_deferred {
        human.push_str(&format!(
            "{}: stop deferred (no commit to anchor to yet — the next sweep retries)\n",
            Label::of(id)
        ));
    }
    for id in &closed_idle {
        human.push_str(&format!(
            "closed {} (idle: the machine's pre-session state — nothing to condense)\n",
            Label::of(id)
        ));
    }
    if sweep.finalized.is_empty() && sweep.condense_deferred.is_empty() && closed_idle.is_empty() {
        human.push_str("nothing to stop");
    }
    Ok(Outcome::ok(
        human.trim_end().to_owned(),
        serde_json::json!({
            "finalized": sweep.finalized,
            "deferred": sweep.condense_deferred,
            "closed_idle": closed_idle,
            "already_ended": [],
        }),
    ))
}

/// How long an `idle` cache must have gone untouched before stop-all closes
/// it. Generous for the one legitimate gap (an agent between writing its
/// state file and its `Start` hook), unreachable for every stuck cache the
/// verb exists to clear — doctor's own STUCK floor is 24 hours.
const IDLE_CLOSE_GUARD_MS: i64 = 3_600_000;

/// Close journal caches parked in the machine's `idle` state (see the comment
/// at the call site). Returns the ids closed, oldest first.
fn close_idle_caches(root: &Path, now: i64, target: Option<&str>) -> Result<Vec<String>> {
    use rift_shadow::hooks::session_cache::{is_idle_past, write_cache};
    let repo = rift_git::discover(root)?;
    let mut caches: Vec<_> = journal_sessions(&repo.git_dir)
        .into_iter()
        .filter(|c| c.state == "idle")
        .filter(|c| match target {
            Some(id) => c.session_id == id,
            None => is_idle_past(c, now, IDLE_CLOSE_GUARD_MS),
        })
        .collect();
    caches.sort_by_key(|c| c.updated_at_ms);
    let mut closed = Vec::new();
    for mut cache in caches {
        "ended".clone_into(&mut cache.state);
        write_cache(&repo.git_dir, &cache)?;
        closed.push(cache.session_id);
    }
    Ok(closed)
}

/// The previous tool's ephemeral per-base-commit checkpoint refs — the
/// "orphaned shadow refs" doctor reports and `clean` prunes. ONLY the
/// ephemeral namespace: the legacy checkpoint metadata (the read-compat
/// import source) and every rift ref are out of scope by construction.
pub(crate) fn shadow_refs(repo: &rift_git::RepoPaths) -> Vec<(String, i64)> {
    let args = vec![
        "for-each-ref".to_owned(),
        "--format=%(refname) %(committerdate:unix)".to_owned(),
        concat!("refs/", "ent", "ire", "/shadow/").to_owned(),
    ];
    let Ok((true, out)) = rift_git::run_git_status(repo, &args, &[], &[]) else {
        return Vec::new();
    };
    String::from_utf8_lossy(&out)
        .lines()
        .filter_map(|l| {
            let (name, date) = l.split_once(' ')?;
            Some((name.to_owned(), date.trim().parse::<i64>().ok()?))
        })
        .collect()
}

/// `rift clean [--dry-run] [--ttl-days N]`: GC the
/// transients and nothing else.
///
/// In scope: orphaned ephemeral shadow refs past TTL
/// (the previous tool's per-base-commit shadow namespace
/// checkpoints, which its own GC pruned the same way), capture-queue entries
/// past TTL (their C-diff facts are already durable; what a dropped entry
/// costs is a checkpoint that was never going to land), stale queue temp
/// files, the `.rift/state/` run caches (manifest baselines + injection
/// markers — losing them costs one bootstrap turn, never correctness), the
/// per-window journals past their own TTL (losing one costs a re-read that
/// would have been declined), and the once-per-eviction recovery markers past
/// the same TTL (losing one costs a recovery map handed over twice).
///
/// NEVER in scope, by construction: the canonical checkpoint branch, the
/// chain ref, the legacy checkpoint metadata refs (read-compat import
/// source), the projection, and the session journal. The e2e pins the
/// canonical refs byte-identical across a clean.
pub(crate) fn clean(root: &Path, dry_run: bool, ttl_days: u32) -> Result<Outcome> {
    let repo = rift_git::discover(root)?;
    let report = swept_transients(&repo, crate::hooks::now_ms(), ttl_days, dry_run);
    Ok(Outcome::ok(report.human_line(), report.json_document()))
}

/// Everything one run of the sweep removed, kept together so the human line
/// and the `--json` document are two readings of the same tally rather than
/// two independent ones.
struct CleanReport {
    dry_run: bool,
    ttl_days: u32,
    pruned_refs: Vec<String>,
    kept_refs: usize,
    pruned_queue: usize,
    swept_tmps: usize,
    dropped_state: usize,
    expired_window_journals: usize,
    expired_recovery_markers: usize,
    search_windows: usize,
    search_answers: usize,
    /// The family's document sidecars and this member's views of them —
    /// including what one conversation was told about documents, which a
    /// conversation that ended a month ago will not be told any more of.
    documents: crate::doc_store::SweepReport,
    /// The family's read-time basis snapshots. Losing one costs a delta frame
    /// and never an answer — the blob route is untouched and the file is
    /// still in the checkout.
    bases: rift_recall_serve::basis_store::SweepReport,
}

/// Collect every transient past its TTL, in the order the report reads them.
///
/// Each leg is total and quiet on its own: a directory that is not there, a
/// family that cannot be resolved from here and a removal that fails are all
/// "nothing collected", never an error that would abandon the legs after it.
fn swept_transients(
    repo: &rift_git::RepoPaths,
    now: i64,
    ttl_days: u32,
    dry_run: bool,
) -> CleanReport {
    let ttl_ms = i64::from(ttl_days) * 86_400_000;
    let cutoff_unix = (now - ttl_ms) / 1000;
    let (pruned_refs, kept_refs) = pruned_shadow_refs(repo, cutoff_unix, dry_run);
    let (pruned_queue, swept_tmps) = pruned_queue_entries(repo, now, ttl_ms, dry_run);
    // The per-window journals, by their own age: a window still being
    // written to keeps its file, so a live conversation is never disarmed
    // mid-sentence, and nothing in the directory that this tool did not write
    // is touched however it got there.
    let expired_window_journals =
        crate::window_journal::sweep_expired(&repo.work_tree, now, ttl_ms, dry_run);
    // Run-state caches: bootstrap-cost transients only.
    let dropped_state = drop_run_state_caches(&repo.work_tree, dry_run);
    // What ended conversations left in this member's run state.
    let (expired_recovery_markers, search_windows, search_answers) =
        sweep_window_state(&repo.work_tree, now, ttl_ms, dry_run);
    CleanReport {
        dry_run,
        ttl_days,
        pruned_refs,
        kept_refs,
        pruned_queue,
        swept_tmps,
        dropped_state,
        expired_window_journals,
        expired_recovery_markers,
        search_windows,
        search_answers,
        documents: swept_documents(repo, ttl_days, now, dry_run).unwrap_or_default(),
        bases: swept_bases(repo, ttl_days, now, dry_run).unwrap_or_default(),
    }
}

/// The ephemeral shadow refs past TTL (by their tip's committer time): the
/// names pruned, and how many were kept inside the TTL.
fn pruned_shadow_refs(
    repo: &rift_git::RepoPaths,
    cutoff_unix: i64,
    dry_run: bool,
) -> (Vec<String>, usize) {
    let shadow = shadow_refs(repo);
    let (prunable, kept): (Vec<_>, Vec<_>) = shadow
        .iter()
        .partition(|(_, tip_unix)| *tip_unix < cutoff_unix);
    let mut pruned_refs = Vec::new();
    for (name, _) in &prunable {
        // Belt and braces: the enumeration is already namespace-scoped, but a
        // deletion never trusts a list it did not build this line.
        if !name.starts_with(concat!("refs/", "ent", "ire", "/shadow/")) {
            continue;
        }
        if !dry_run
            && rift_git::run_git(
                repo,
                &["update-ref".to_owned(), "-d".to_owned(), name.clone()],
            )
            .is_ok()
        {
            pruned_refs.push(name.clone());
        }
        if dry_run {
            pruned_refs.push(name.clone());
        }
    }
    (pruned_refs, kept.len())
}

/// Queue entries past TTL, plus the stale temp files every drain sweeps.
fn pruned_queue_entries(
    repo: &rift_git::RepoPaths,
    now: i64,
    ttl_ms: i64,
    dry_run: bool,
) -> (usize, usize) {
    let mut pruned_queue = 0usize;
    for (path, entry) in crate::capture_queue::entries(&repo.git_dir) {
        if now.saturating_sub(entry.queued_at_ms) > ttl_ms {
            if !dry_run {
                crate::capture_queue::remove(&path);
            }
            pruned_queue += 1;
        }
    }
    let swept_tmps = if dry_run {
        0
    } else {
        crate::capture_queue::sweep_stale_tmps(&repo.git_dir, now)
    };
    (pruned_queue, swept_tmps)
}

impl CleanReport {
    /// `dry-run` when nothing was written, `clean` when it was — the word the
    /// human line opens with and the `--json` document's `mode`.
    fn mode_word(&self) -> &'static str {
        if self.dry_run {
            "dry-run"
        } else {
            "clean"
        }
    }

    fn human_line(&self) -> String {
        // Named for the placeholders the sentence below spells, so the
        // sentence stays the one an operator has been reading.
        let mode = self.mode_word();
        let pruned_queue = self.pruned_queue;
        let swept_tmps = self.swept_tmps;
        let dropped_state = self.dropped_state;
        let expired_window_journals = self.expired_window_journals;
        let expired_recovery_markers = self.expired_recovery_markers;
        let sidecars = self.documents.expired;
        let orphan_links = self.documents.orphan_links;
        let doc_windows = self.documents.swept_windows;
        let search_windows = self.search_windows;
        let search_answers = self.search_answers;
        let basis_snapshots = self.bases.expired;
        let orphan_basis_metas = self.bases.orphan_metas;
        format!(
            "clean ({mode}): {} shadow ref(s) pruned ({} kept inside TTL), \
             {pruned_queue} stale queue entr{} removed, {swept_tmps} temp file(s) swept, \
             {dropped_state} run-state cache dir(s) dropped, \
             {expired_window_journals} expired window-journal file(s) removed, \
             {expired_recovery_markers} expired recovery marker(s) removed, \
             {sidecars} expired document sidecar(s), {orphan_links} orphan link(s) \
             and {doc_windows} ended document conversation(s) removed, \
             {search_windows} ended search conversation(s) and {search_answers} \
             unpriced answer(s) removed, \
             {basis_snapshots} expired basis snapshot(s) and {orphan_basis_metas} \
             orphan snapshot record(s) removed\n  \
             canonical refs and the chain: untouched",
            self.pruned_refs.len(),
            self.kept_refs,
            if pruned_queue == 1 { "y" } else { "ies" },
        )
    }

    fn json_document(&self) -> serde_json::Value {
        serde_json::json!({
            "mode": self.mode_word(),
            "pruned_shadow_refs": self.pruned_refs,
            "kept_shadow_refs": self.kept_refs,
            "pruned_queue_entries": self.pruned_queue,
            "swept_tmp_files": self.swept_tmps,
            "dropped_state_dirs": self.dropped_state,
            "expired_window_journals": self.expired_window_journals,
            "expired_recovery_markers": self.expired_recovery_markers,
            "expired_doc_sidecars": self.documents.expired,
            "orphan_doc_links": self.documents.orphan_links,
            "swept_doc_tmp_files": self.documents.swept_tmps,
            "swept_doc_windows": self.documents.swept_windows,
            "swept_search_windows": self.search_windows,
            "swept_search_answers": self.search_answers,
            "expired_basis_snapshots": self.bases.expired,
            "orphan_basis_metas": self.bases.orphan_metas,
            "swept_basis_tmp_files": self.bases.swept_tmps,
            "ttl_days": self.ttl_days,
        })
    }
}

/// Drop the whole-directory run-state caches, and say how many went.
///
/// The two here are **bootstrap cost and nothing else**: the manifest cache is
/// a stamp that lets a turn answer "nothing to say" without opening the
/// projection, and the injection markers say a pointer has already been sent.
/// Losing either costs one recomputation or one duplicate pointer, which is
/// why they go whole rather than by age — unlike everything else this sweep
/// touches, where dropping a fresh file would disarm a live conversation.
fn drop_run_state_caches(work_tree: &Path, dry_run: bool) -> usize {
    let mut dropped = 0usize;
    for sub in ["manifest", "injection"] {
        let dir = work_tree.join(".rift").join("state").join(sub);
        if dir.is_dir() {
            dropped += 1;
            if !dry_run {
                let _ = std::fs::remove_dir_all(&dir);
            }
        }
    }
    dropped
}

/// The family's read-time basis snapshots, collected under the more
/// conservative of the two TTLs that have a claim.
///
/// Same rule as [`swept_documents`], for the same reason: a sweep may be asked
/// to keep things longer than the policy promised, and is never asked to
/// collect a basis the repository still expects to find.
fn swept_bases(
    repo: &rift_git::RepoPaths,
    ttl_days: u32,
    now_ms: i64,
    dry_run: bool,
) -> Option<rift_recall_serve::basis_store::SweepReport> {
    let family = crate::family::try_at(&repo.work_tree)?;
    let ttl_days = ttl_days.max(
        rift_recall_policy::ServePolicy::load(&repo.work_tree.join(".rift"))
            .interception
            .basis_ttl_days,
    );
    Some(
        rift_recall_serve::basis_store::BasisStore::for_family(&family.common_dir).sweep(
            now_ms,
            i64::from(ttl_days) * 86_400_000,
            dry_run,
        ),
    )
}

/// This member's view of its family's document store, when the family has an
/// identity to look it up by.
///
/// `None` is not a failure: a checkout whose family cannot be resolved from
/// here has no shared store to collect, and nothing about that is this
/// command's to repair.
pub(crate) fn document_store(repo: &rift_git::RepoPaths) -> Option<crate::doc_store::DocStore> {
    let family = crate::family::try_at(&repo.work_tree)?;
    Some(crate::doc_store::DocStore::for_member(
        &family.common_dir,
        &repo.work_tree,
    ))
}

/// Remove the recovery markers older than the TTL, and say how many went.
///
/// A marker is an empty file named for the fingerprint of one recovery map's
/// input, written `create_new` so a retried boundary hook stays silent. It is
/// spent the moment the map it names has been handed over, and after that it
/// is disk — but only after that, which is why age decides rather than the
/// directory as a whole.
///
/// Total and quiet, like every other leg of this sweep: a file whose age
/// cannot be read is kept (removing on an unknown age would disarm a live
/// window on a filesystem that reports no times), a name this tool did not
/// write is left alone, and a failed removal is simply not counted.
fn sweep_recovery_markers(work_tree: &Path, now_ms: i64, ttl_ms: i64, dry_run: bool) -> usize {
    let dir = work_tree.join(".rift").join("state").join("recovery");
    let cutoff = now_ms.saturating_sub(ttl_ms);
    let mut removed = 0usize;
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return 0;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        // The one shape this tool writes there: a 64-hex fingerprint, `.json`.
        let Some(stem) = name.strip_suffix(".json") else {
            continue;
        };
        if stem.len() != 64 || !stem.bytes().all(|b| b.is_ascii_hexdigit()) {
            continue;
        }
        let path = entry.path();
        let modified_ms = std::fs::metadata(&path)
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .and_then(|d| i64::try_from(d.as_millis()).ok());
        let Some(modified_ms) = modified_ms else {
            continue;
        };
        if modified_ms >= cutoff {
            continue;
        }
        if dry_run || std::fs::remove_file(&path).is_ok() {
            removed += 1;
        }
    }
    removed
}

/// The family's sidecars and this member's views of them, collected under the
/// more conservative of the two TTLs that have a claim.
///
/// A sweep may be asked to keep things longer than the policy promised; it is
/// never asked to collect a sidecar the repository still expects to find,
/// which would cost a parse the next time an agent opened the file.
fn swept_documents(
    repo: &rift_git::RepoPaths,
    ttl_days: u32,
    now_ms: i64,
    dry_run: bool,
) -> Option<crate::doc_store::SweepReport> {
    let store = document_store(repo)?;
    let ttl_days = ttl_days.max(
        rift_recall_policy::ServePolicy::load(&repo.work_tree.join(".rift"))
            .doc
            .ttl_days,
    );
    Some(store.sweep(now_ms, i64::from(ttl_days) * 86_400_000, dry_run))
}

/// The per-window marks ended conversations leave behind, collected by their
/// own age.
///
/// Three kinds, one rule: a marker is what stops a live window being handed
/// the same thing twice — a recovery map after a compaction, an answer to a
/// search it already asked — so dropping a fresh one costs a duplicate in a
/// conversation that is still open, which is exactly the spend this program
/// exists to remove. Age, never the whole directory, and nothing that this
/// tool did not write is touched however it got there.
fn sweep_window_state(
    work_tree: &Path,
    now_ms: i64,
    ttl_ms: i64,
    dry_run: bool,
) -> (usize, usize, usize) {
    let recovery = sweep_recovery_markers(work_tree, now_ms, ttl_ms, dry_run);
    let (windows, answers) = crate::search_gate::sweep(work_tree, now_ms, ttl_ms, dry_run);
    (recovery, windows, answers)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An `idle` journal cache is the machine's pre-session
    /// state — no live-state sweep reaches it and no legal transition ends it,
    /// so five sat STUCK for weeks. The explicit verb closes them at the
    /// journal level; a fresh idle cache (an agent between its state file and
    /// its `Start`) survives stop-all and falls only to a targeted stop.
    #[test]
    fn stop_closes_stuck_idle_caches_and_spares_fresh_ones() {
        use rift_shadow::hooks::session_cache::{read_cache, write_cache, SessionCache};
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("repo");
        let git_dir = rift_git::test_support::fake_repo(&root).unwrap();
        let cache = |id: &str, updated_at_ms: i64| {
            SessionCache::new(id, "claude-code", "idle", updated_at_ms)
        };
        write_cache(&git_dir, &cache("stuck-idle", 1_000)).unwrap();
        let fresh_at = crate::hooks::now_ms();
        write_cache(&git_dir, &cache("fresh-idle", fresh_at)).unwrap();

        let out = stop(&root, None).unwrap();
        assert!(out.human.contains("closed stuck-idle"), "{}", out.human);
        assert_eq!(
            read_cache(&git_dir, "stuck-idle").unwrap().unwrap().state,
            "ended"
        );
        assert_eq!(
            read_cache(&git_dir, "fresh-idle").unwrap().unwrap().state,
            "idle",
            "stop-all must not race an agent that just opened its state file"
        );

        // The operator naming the session overrides the freshness guard.
        let out = stop(&root, Some("fresh-idle")).unwrap();
        assert!(out.human.contains("closed fresh-idle"), "{}", out.human);
        assert_eq!(
            read_cache(&git_dir, "fresh-idle").unwrap().unwrap().state,
            "ended"
        );
    }

    /// ★ **The recovery markers are swept by their own age, and only they
    /// are.** One marker per compaction generation per window accumulated
    /// with nothing to remove it; a fresh one stays, because dropping it hands
    /// a live window the same recovery map a second time, and a name this tool
    /// did not write is never touched however it got there.
    #[test]
    fn the_recovery_markers_expire_and_a_fresh_one_and_a_stranger_do_not() {
        let tmp = tempfile::tempdir().unwrap();
        let work_tree = tmp.path();
        let dir = work_tree.join(".rift").join("state").join("recovery");
        std::fs::create_dir_all(&dir).unwrap();
        let marker = |name: &str| {
            let path = dir.join(name);
            std::fs::write(&path, b"").unwrap();
            path
        };
        let old = marker(&format!("{}.json", "a".repeat(64)));
        let fresh = marker(&format!("{}.json", "b".repeat(64)));
        let stranger = marker("notes.txt");
        let ttl_ms = 7 * 86_400_000;
        let now = 1_800_000_000_000;
        // The two ages, set outright rather than waited for.
        let at = |path: &Path, ms: i64| {
            let when = std::time::UNIX_EPOCH
                + std::time::Duration::from_millis(u64::try_from(ms).unwrap());
            std::fs::OpenOptions::new()
                .write(true)
                .open(path)
                .unwrap()
                .set_modified(when)
                .unwrap();
        };
        at(&old, now - ttl_ms - 1);
        at(&fresh, now - 1);
        at(&stranger, now - ttl_ms - 1);

        assert_eq!(
            sweep_recovery_markers(work_tree, now, ttl_ms, true),
            1,
            "a dry run counts the one marker past its TTL and removes nothing"
        );
        assert!(old.exists(), "and it is still there afterwards");

        assert_eq!(sweep_recovery_markers(work_tree, now, ttl_ms, false), 1);
        assert!(!old.exists(), "the spent marker goes");
        assert!(fresh.exists(), "the live window keeps its own");
        assert!(
            stranger.exists(),
            "and a file this tool did not write stays"
        );
    }

    /// A checkout with no recovery directory at all sweeps nothing and says
    /// so, rather than treating absence as a failure.
    #[test]
    fn a_checkout_that_has_never_recovered_sweeps_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(sweep_recovery_markers(tmp.path(), 1_000, 1, false), 0);
    }
}
