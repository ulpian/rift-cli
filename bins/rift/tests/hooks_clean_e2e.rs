//! End-to-end: the repair and housekeeping verbs — every doctor finding
//! has a broken fixture and a safe `--fix`, `clean` prunes transients and
//! never touches a canonical ref, and a marker the worker left is said once.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use std::path::{Path, PathBuf};

mod common;
#[allow(dead_code)] // The hooks suites share these builders; each file uses a subset.
#[path = "common/hooks_fixture.rs"]
mod hooks_fixture;
use hooks_fixture::{
    archive_head, checkpoint_shards, doctor_fix_json, doctor_json, git_in, json_str, rev_parse,
    rift_json, run_rift_capture_in, run_rift_stdout, seed_live_session_repo, seed_session,
    sync_env, RETIRED_BRAND,
};

const RETIRED_SHADOW_PREFIX: &str = concat!("refs/", "ent", "ire", "/shadow/");
const RETIRED_CHECKPOINT_PREFIX: &str = concat!("refs/", "ent", "ire", "/checkpoints/");

/// A repository that archived once, with the transients `clean` is for
/// planted beside its canonical refs: an ephemeral shadow ref, the LEGACY
/// metadata ref that must survive (read-compat import source), an ancient
/// queue entry, a manifest baseline cache, and a spent recovery marker.
struct PlantedTransients {
    repo: PathBuf,
    cache: PathBuf,
    queue_entry: PathBuf,
    state: PathBuf,
    marker: PathBuf,
    checkpoints_before: String,
    chain_before: String,
}

impl PlantedTransients {
    fn plant(tmp: &Path) -> Self {
        let repo = seed_live_session_repo(tmp);
        std::fs::create_dir_all(repo.join(".rift")).unwrap();
        let cache = tmp.join("cache");
        archive_head(&repo, &sync_env(&cache));
        let head = rev_parse(&repo, "HEAD");
        git_in(
            &repo,
            &[
                "update-ref",
                concat!("refs/", "ent", "ire", "/shadow/deadbeef"),
                &head,
            ],
        );
        git_in(
            &repo,
            &[
                "update-ref",
                concat!("refs/", "ent", "ire", "/checkpoints/v1"),
                &head,
            ],
        );
        let queue_entry = plant_queue_entry(&repo, &head, 1);
        let state = repo.join(".rift").join("state").join("manifest").join("k");
        std::fs::create_dir_all(&state).unwrap();
        std::fs::write(state.join("1.json"), "{}").unwrap();
        let marker = plant_spent_recovery_marker(&repo);
        let checkpoints_before = git_in(&repo, &["rev-parse", "refs/heads/rift/checkpoints/v1"]);
        let chain_before = git_in(&repo, &["rev-parse", "refs/rift/chain/v1"]);
        Self {
            repo,
            cache,
            queue_entry,
            state,
            marker,
            checkpoints_before,
            chain_before,
        }
    }

    fn env(&self) -> [(&'static str, &str); 3] {
        sync_env(&self.cache)
    }

    /// `rift clean <args> --json`: the document.
    fn clean(&self, extra: &[&str]) -> serde_json::Value {
        let mut args = vec!["clean"];
        args.extend_from_slice(extra);
        args.push("--json");
        rift_json(&self.repo, &args, &self.env())
    }

    /// Whether the planted ephemeral shadow ref is still there.
    fn shadow_ref_present(&self) -> bool {
        git_in(&self.repo, &["for-each-ref", RETIRED_SHADOW_PREFIX]).contains("deadbeef")
    }
}

/// Every capture finding-class doctor reports has a broken
/// fixture here and a safe `--fix` that heals exactly it. All plants live in
/// THIS test's throwaway tempdir repo — every destructive step is rooted at
/// the fixture path, never the process cwd.
#[test]
fn doctor_findings_have_broken_fixtures_and_safe_fixes() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    std::fs::create_dir_all(repo.join(".rift")).unwrap();
    let cache = tmp.path().join("cache");
    let env = sync_env(&cache);

    // A real archive first: it gives the repo a checkpoint branch, a chain
    // entry, and a projected row — the healthy baseline every plant breaks.
    archive_head(&repo, &env);
    assert_eq!(checkpoint_shards(&repo).len(), 1);
    plant_every_finding(&repo, &cache);

    // REPORT (no --fix): every finding is named and NOTHING is repaired.
    let d = doctor_json(&repo, &env);
    assert_eq!(
        d["sessions"]["stuck"][0], "e2e-live-1",
        "the stuck session is a finding: {d}"
    );
    assert_eq!(
        d["capture_chain"]["gap"], 1,
        "the tail gap is a finding: {d}"
    );
    assert!(
        d["watermark"]["drift"].as_i64().unwrap() >= 1,
        "the un-projected shard is a finding: {d}"
    );
    assert!(
        d["capture_queue"]["pending"].as_i64().unwrap_or(0) >= 1,
        "the queue entry is a finding: {d}"
    );
    // Where it was seeded: a report-only doctor moves no run state either.
    let journal = std::fs::read_to_string(
        common::seed_sessions_dir(&repo.join(".git")).join("e2e-live-1.json"),
    )
    .unwrap();
    assert!(
        journal.contains(r#""state":"active""#) || journal.contains(r#""state": "active""#),
        "report-only doctor did NOT finalize the session: {journal}"
    );

    // FIX: each finding's safe repair runs, and a second doctor is clean.
    let f = doctor_fix_json(&repo, &env);
    assert_eq!(f["fix"], true, "{f}");
    let after = doctor_json(&repo, &env);
    assert_eq!(
        after["sessions"]["stuck"].as_array().map(Vec::len),
        Some(0),
        "the stuck session was finalized: {after}"
    );
    assert_eq!(
        after["capture_chain"]["gap"], 0,
        "the tail gap healed: {after}"
    );
    assert_eq!(
        after["watermark"]["drift"], 0,
        "the projection leveled: {after}"
    );
    assert_eq!(
        after["capture_queue"]["pending"].as_i64().unwrap_or(0),
        0,
        "the queue drained: {after}"
    );
}

/// Every finding class, planted. The stuck session needs no plant: the seeded
/// journal is already `active` with an ancient stamp — that IS the stuck
/// shape, and the archive did not close it (manual-commit archives never do).
/// Every destructive step is rooted at the fixture path, never the process cwd.
fn plant_every_finding(repo: &Path, cache: &Path) {
    // PLANT watermark drift: drop the projection db (a rebuildable cache in
    // this fixture's own tempdir) so the shard on the branch has no row.
    let db = cache.join("rift");
    assert!(db.is_dir(), "the projection cache exists before the plant");
    std::fs::remove_dir_all(&db).unwrap();
    rewind_chain_by_one(repo);
    // PLANT a queue entry: a commit awaiting condense that no worker drained.
    let head = rev_parse(repo, "HEAD");
    plant_queue_entry(repo, &head, 1_783_000_000_000);
    // PLANT a stuck session: the seeded one, untouched since the epoch, which
    // no sweep has reached because no worker has run since.
    seed_session(repo, "e2e-live-1", 0);
}

/// PLANT chain tail gap: rewind the chain ref by one entry (throwaway repo;
/// this is the broken fixture, not an operation on real history).
fn rewind_chain_by_one(repo: &Path) {
    let chain_tip = rev_parse(repo, "refs/rift/chain/v1");
    let parents = git_in(repo, &["rev-list", "--parents", "-1", &chain_tip]);
    match parents.split_whitespace().count() {
        1 => git_in(repo, &["update-ref", "-d", "refs/rift/chain/v1"]),
        _ => git_in(
            repo,
            &["update-ref", "refs/rift/chain/v1", &format!("{chain_tip}^")],
        ),
    };
}

/// A capture-queue entry for `head` in the shape the hook journals one,
/// queued at `queued_at_ms`. Returns the entry's path.
fn plant_queue_entry(repo: &Path, head: &str, queued_at_ms: i64) -> PathBuf {
    let qdir = hooks_fixture::capture_queue_of(repo);
    std::fs::create_dir_all(&qdir).unwrap();
    let entry = qdir.join(format!("{head}.json"));
    std::fs::write(
        &entry,
        format!(
            r#"{{"schema_version":1,"commit":"{head}","work_tree":{},"session_id":null,"agent":null,"session_state":null,"queued_at_ms":{queued_at_ms},"attempts":0}}"#,
            json_str(repo)
        ),
    )
    .unwrap();
    entry
}

/// A spent recovery marker, in the shape and the place the boundary hook
/// writes one, aged to the epoch. Nothing removed these before, so one file
/// per compaction generation per window accumulated for the life of the
/// repository.
fn plant_spent_recovery_marker(repo: &Path) -> PathBuf {
    let recovery = repo.join(".rift").join("state").join("recovery");
    std::fs::create_dir_all(&recovery).unwrap();
    let marker = recovery.join(format!("{}.json", "c".repeat(64)));
    std::fs::write(&marker, "").unwrap();
    std::fs::OpenOptions::new()
        .write(true)
        .open(&marker)
        .unwrap()
        .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1))
        .unwrap();
    marker
}

/// **DOCTOR'S HUMAN MODE, WITH THE REF ACTUALLY PLANTED.** The sibling guard
/// on this line swept `rift doctor` for the retired brand in a sandbox that
/// never created a retired shadow ref, so `doctor_shadow_refs` returned its
/// empty arm and the sweep read an empty string: vacuous by construction, and
/// it stayed green while the branded branch printed the brand. Here the ref
/// exists, so the branch is REACHED — which is asserted first, because a purge
/// assertion over output that never contains the leg is the same vacuity
/// wearing the other sign.
///
/// Plain `rift doctor` (no `--fix`) is report-only for the session sweep
/// (`main.rs`'s `doctor_session_sweep` early-returns), so it perturbs nothing
/// the sibling tests measure, and `RIFT_CACHE_DIR` is this sandbox's (a leg
/// that runs doctor must not write into a real projection).
#[test]
fn doctors_human_mode_reaches_the_shadow_refs_leg_without_the_retired_brand() {
    let tmp = tempfile::tempdir().unwrap();
    let fx = PlantedTransients::plant(tmp.path());
    let (code, doc) = run_rift_stdout(&fx.repo, &["doctor"], &fx.env());
    assert_eq!(code, 0, "{doc}");
    assert!(
        doc.contains("shadow refs:") && doc.contains("orphaned"),
        "the shadow-refs leg must be REACHED, or the purge below asserts nothing: {doc}"
    );
    assert!(
        !doc.to_ascii_lowercase().contains(RETIRED_BRAND),
        "the previous tool's brand reached a human surface: {doc}"
    );
}

/// A dry run reports the transients and removes NOTHING; a TTL is honored — at
/// 14 days the fresh shadow ref is KEPT while the ANCIENT queue entry (queued
/// in 1970) and the run-state caches are legitimately past any TTL and go now.
#[test]
fn a_dry_run_removes_nothing_and_a_ttl_keeps_the_fresh_shadow_ref() {
    let tmp = tempfile::tempdir().unwrap();
    let fx = PlantedTransients::plant(tmp.path());

    let v = fx.clean(&["--dry-run", "--ttl-days", "0"]);
    assert_eq!(v["mode"], "dry-run", "{v}");
    assert_eq!(
        v["pruned_shadow_refs"][0],
        concat!("refs/", "ent", "ire", "/shadow/deadbeef"),
        "{v}"
    );
    assert!(fx.shadow_ref_present(), "dry run deleted nothing");
    assert!(fx.queue_entry.exists(), "dry run kept the queue entry");

    let kept = fx.clean(&["--ttl-days", "14"]);
    assert_eq!(kept["kept_shadow_refs"], 1, "{kept}");
    assert_eq!(kept["pruned_queue_entries"], 1, "{kept}");
    assert!(kept["dropped_state_dirs"].as_i64().unwrap() >= 1, "{kept}");
    assert_eq!(
        kept["expired_recovery_markers"], 1,
        "the spent recovery marker is swept with the rest of the run state: {kept}"
    );
    assert!(!fx.marker.exists(), "and it is really gone");
    assert!(fx.shadow_ref_present(), "a ref inside TTL survives");
}

/// The `clean` half: transients go (ephemeral shadow refs past TTL, stale
/// queue entries, run-state caches) and the canonical surfaces stay
/// byte-identical — pinned, not assumed.
#[test]
fn clean_prunes_transients_and_never_touches_canonical_refs() {
    let tmp = tempfile::tempdir().unwrap();
    let fx = PlantedTransients::plant(tmp.path());

    // The real clean at TTL 0: the shadow ref goes too; canonical stays.
    let c = fx.clean(&["--ttl-days", "0"]);
    assert_eq!(
        c["pruned_shadow_refs"][0],
        concat!("refs/", "ent", "ire", "/shadow/deadbeef"),
        "{c}"
    );
    assert!(!fx.shadow_ref_present(), "the ephemeral shadow ref is gone");
    assert!(
        git_in(&fx.repo, &["for-each-ref", RETIRED_CHECKPOINT_PREFIX]).contains("checkpoints/v1"),
        "the legacy metadata ref (read-compat import source) SURVIVES"
    );
    assert_eq!(
        git_in(&fx.repo, &["rev-parse", "refs/heads/rift/checkpoints/v1"]),
        fx.checkpoints_before,
        "the canonical checkpoint branch is byte-identical"
    );
    assert_eq!(
        git_in(&fx.repo, &["rev-parse", "refs/rift/chain/v1"]),
        fx.chain_before,
        "the chain ref is byte-identical"
    );
    assert!(!fx.queue_entry.exists(), "the stale entry is gone");
    assert!(!fx.state.exists(), "the run-state cache dir is gone");
}

/// A developer diagnosing a reset soak streak must be shown the REASON, not
/// only the counter. The marker is written by the detached worker and was read
/// only by the next hook, so between commits the explanation existed on disk
/// and nothing would print it. A report-only doctor reads it without taking
/// it (CFX-7): a second report prints the same reason, and only `--fix`
/// consumes it, exactly once.
#[test]
fn report_only_doctor_says_a_pending_marker_every_run_and_fix_consumes_it_once() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    let cache = tmp.path().join("cache");
    let env = [
        ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
        ("RIFT_NO_KEYCHAIN", "1"),
        ("RIFT_CAPTURE_SYNC", "1"),
    ];

    // A marker left behind exactly as the worker leaves one.
    let queue = common::capture_queue_dir(&repo.join(".git"), &cache);
    std::fs::create_dir_all(&queue).unwrap();
    let marker = queue.join("stale-entry.error");
    std::fs::write(&marker, "soak verify is not clean: tamper").unwrap();
    let soak_markers = |args: &[&str]| {
        let (code, out) = run_rift_capture_in(&repo, args, "", &env);
        assert_eq!(code, 0, "doctor: {out}");
        let v: serde_json::Value = serde_json::from_str(&out).expect("doctor emits json");
        v["soak"]["pending_markers"]
            .as_array()
            .expect("pending_markers is an array")
            .clone()
    };
    let says_reason = |markers: &[serde_json::Value]| {
        markers.iter().any(|m| {
            m.as_str()
                .is_some_and(|s| s.contains("soak verify is not clean"))
        })
    };

    for run in ["first", "second"] {
        let markers = soak_markers(&["doctor", "--json"]);
        assert!(
            says_reason(&markers),
            "{run} report must say why: {markers:?}"
        );
        assert!(
            marker.exists(),
            "a report consumed the marker on its {run} run"
        );
    }

    let fixed = soak_markers(&["doctor", "--fix", "--json"]);
    assert!(
        says_reason(&fixed),
        "the fix run says it once more: {fixed:?}"
    );
    assert!(!marker.exists(), "--fix consumes the said-once marker");
    let after = soak_markers(&["doctor", "--json"]);
    assert!(after.is_empty(), "consumed exactly once: {after:?}");
}

/// **A planted capture note is reported by doctor as a finding on every
/// report** (cause E of `turn_boundary`'s register, 2026-08-29). The
/// `capture_queue` leg and the soak leg both read the note channel through
/// `peek_errors`; a report-only doctor consumes nothing (CFX-7), so a second
/// report reads the same finding. Saying it once is the hook's job and
/// `--fix`'s, never a report's.
#[test]
fn a_planted_capture_note_is_reported_by_every_report_only_doctor_run() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    std::fs::create_dir_all(repo.join(".rift")).unwrap();
    let cache = tmp.path().join("cache");
    let env = [
        ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
        ("RIFT_NO_KEYCHAIN", "1"),
    ];
    // The note channel's shape, planted by hand: `<key>.error` under the
    // capture queue, one line, the body a boundary-snapshot failure would leave.
    let qdir = common::capture_queue_dir(&repo.join(".git"), &cache);
    std::fs::create_dir_all(&qdir).unwrap();
    let marker = qdir.join("boundary-0123456789abcdef.error");
    let body =
        "turn-boundary snapshot failed; the session's H_i/A_i windows are incomplete (planted)";
    std::fs::write(&marker, body).unwrap();

    let (code, out) = run_rift_stdout(&repo, &["doctor", "--json"], &env);
    assert_eq!(code, 0, "{out}");
    let d: serde_json::Value = serde_json::from_slice(out.as_bytes()).unwrap();
    let notes = d["capture_queue"]["notes"].as_array().unwrap();
    assert!(
        notes.iter().any(|n| n == body),
        "the planted note is not a finding: {d}"
    );
    assert!(marker.exists(), "a report-only doctor consumed the note");

    let (code, out) = run_rift_stdout(&repo, &["doctor", "--json"], &env);
    assert_eq!(code, 0, "{out}");
    let d: serde_json::Value = serde_json::from_slice(out.as_bytes()).unwrap();
    assert!(
        d["capture_queue"]["notes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n == body),
        "the second report must read the same note: {d}"
    );
}
