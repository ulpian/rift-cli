//! End-to-end: the offline bundle round-trip and the
//! all-or-nothing tamper refusal.
//!
//! Two properties, both stated as exit criteria and both measured here rather
//! than argued:
//!
//! * **Round-trip.** A bundle exported from a repository carrying a real
//!   checkpoint and a real chain entry, applied into a **fresh clone that has
//!   neither**, leaves that clone's capture refs byte-identical to the source's,
//!   and `rift verify` green over the result.
//! * **Tamper is all-or-nothing.** One byte flipped inside the bundled shard
//!   data is refused, and afterwards **zero refs have moved** and the bundle's
//!   objects are not in the object store — the refusal happens before git is
//!   asked to read anything.
//!
//! Everything is a filesystem path inside a temp dir; the binary is spawned
//! only through `common::rift_cmd`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use std::path::{Path, PathBuf};
use std::process::Command;

#[path = "bundle_e2e/boundary_gaps.rs"]
mod boundary_gaps;
mod common;
use common::rift_cmd;

const CHAIN_REF: &str = "refs/rift/chain/v1";
const CHECKPOINTS_REF: &str = "refs/heads/rift/checkpoints/v1";

fn git_in(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", dir.join("absent-gitconfig"))
        .env("GIT_AUTHOR_NAME", "T")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "T")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_OBJECT_DIRECTORY")
        .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
        .output()
        .expect("git is on PATH");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// `git` whose failure is an answer, not an assertion.
fn git_probe(dir: &Path, args: &[&str]) -> bool {
    Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", dir.join("absent-gitconfig"))
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .output()
        .expect("git is on PATH")
        .status
        .success()
}

/// The shipped binary in `cwd` with an isolated cache: `(exit, stdout, stderr)`.
///
/// `RIFT_CAPTURE_SYNC` on every spawn: the post-commit archive lands before
/// the hook returns. Detached, it could still be running when `doctor --fix`
/// looks, and `doctor --fix` does not wait for a worker that holds the drain
/// lock — it would return with the checkpoint not yet written.
fn rift(cwd: &Path, cache: &Path, args: &[&str]) -> (i32, String, String) {
    let out = rift_cmd(cwd)
        .args(args)
        .env("RIFT_CACHE_DIR", cache)
        .env("RIFT_CONFIG_DIR", cache.join("conf"))
        .env("RIFT_HEADLESS", "1")
        .env("RIFT_CAPTURE_SYNC", "1")
        .output()
        .expect("rift runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// A repository that has archived one **real** checkpoint through the native
/// post-commit path, so both capture refs exist with real content and the
/// projection holds a real chain link.
fn seeded_repo(tmp: &Path) -> (PathBuf, PathBuf) {
    let repo = tmp.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git_in(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("a.txt"), "one\n").unwrap();
    git_in(&repo, &["add", "a.txt"]);
    let sessions = common::seed_sessions_dir(&repo.join(".git"));
    std::fs::create_dir_all(&sessions).unwrap();
    std::fs::write(
        sessions.join("bundle-live-1.json"),
        r#"{"schema":1,"session_id":"bundle-live-1","state":"active","agent":"claude-code","current_turn_ordinal":1,"turn_open":true,"compaction_boundary":null,"updated_at_ms":1783000000000,"native_id_map":{}}"#,
    )
    .unwrap();
    git_in(&repo, &["commit", "-q", "-m", "seed"]);
    std::fs::create_dir_all(repo.join(".rift")).unwrap();
    std::fs::write(
        repo.join(".rift/config.toml"),
        "schema_version = 1\n[project]\nname = \"bundle-fixture\"\n",
    )
    .unwrap();

    let cache = tmp.join("cache");
    std::fs::create_dir_all(&cache).unwrap();
    let gd = repo.join(".git");
    let (code, _, err) = rift(
        &repo,
        &cache,
        &[
            "hooks",
            "git",
            "post-commit",
            "--git-dir",
            gd.to_str().unwrap(),
        ],
    );
    assert_eq!(code, 0, "post-commit must never fail: {err}");
    // The hook drained inline; the doctor pass is the recipe's step and finds
    // nothing left to drain.
    let (code, out, err) = rift(&repo, &cache, &["doctor", "--fix", "--json"]);
    assert_eq!(code, 0, "doctor --fix: {out}{err}");
    (repo, cache)
}

fn ref_oid(dir: &Path, name: &str) -> Option<String> {
    let out = Command::new("git")
        .args(["rev-parse", "--verify", "--quiet", name])
        .current_dir(dir)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .output()
        .expect("git is on PATH");
    let text = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    (out.status.success() && !text.is_empty()).then_some(text)
}

/// A git repository with no capture refs at all — the "fresh clone" a bundle
/// exists to reach.
fn empty_target(tmp: &Path, name: &str) -> PathBuf {
    let dir = tmp.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    git_in(&dir, &["init", "-q", "-b", "main"]);
    std::fs::create_dir_all(dir.join(".rift")).unwrap();
    std::fs::write(
        dir.join(".rift/config.toml"),
        "schema_version = 1\n[project]\nname = \"target\"\n",
    )
    .unwrap();
    dir
}

/// Round-trip: export, import into a repository holding neither ref, and the
/// two capture refs come out identical. Then import again — set-union, so the
/// second application applies nothing and moves nothing.
#[test]
fn a_bundle_round_trips_into_a_fresh_repository_and_re_imports_idempotently() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, cache) = seeded_repo(tmp.path());
    let src_ck = ref_oid(&repo, CHECKPOINTS_REF).expect("the fixture archived a checkpoint");
    let src_chain = ref_oid(&repo, CHAIN_REF).expect("the fixture recorded a chain entry");

    let out = tmp.path().join("x.riftbundle");
    export_whole_chain(&repo, &cache, &out);

    let target = empty_target(tmp.path(), "fresh");
    let tcache = tmp.path().join("tcache");
    assert!(ref_oid(&target, CHECKPOINTS_REF).is_none());
    assert!(ref_oid(&target, CHAIN_REF).is_none());

    first_import_carries_both_refs(&target, &tcache, &out, &src_ck, &src_chain);
    re_import_applies_nothing(&target, &tcache, &out, &src_ck);
    imported_repository_verifies(&target, &tcache);
    rebuild_alone_writes_the_checkpoint_strategy(&target, &tcache);
}

/// Export the whole chain from the origin, asserting the document names the
/// origin cursor and carries the fixture's chain link.
fn export_whole_chain(repo: &Path, cache: &Path, out: &Path) {
    let (code, stdout, err) = rift(
        repo,
        cache,
        &["--json", "bundle", "export", "--out", out.to_str().unwrap()],
    );
    assert_eq!(code, 0, "export failed: {stdout}{err}");
    let doc: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(doc["schema_version"], 1);
    assert_eq!(doc["from_cursor"], "rift-cursor/1:0:");
    assert!(
        doc["links"].as_u64().unwrap() >= 1,
        "the exported segment must carry the fixture's chain link: {doc}"
    );
    assert!(out.exists());
}

/// The first import into a repository holding neither ref applies both, and
/// each comes out identical to the source's.
fn first_import_carries_both_refs(
    target: &Path,
    tcache: &Path,
    out: &Path,
    src_ck: &str,
    src_chain: &str,
) {
    let (code, stdout, err) = rift(
        target,
        tcache,
        &["--json", "bundle", "import", out.to_str().unwrap()],
    );
    assert_eq!(code, 0, "import failed: {stdout}{err}");
    let doc: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(doc["applied"].as_array().unwrap().len(), 2);
    assert_eq!(doc["already_present"], 0);

    assert_eq!(
        ref_oid(target, CHECKPOINTS_REF).as_deref(),
        Some(src_ck),
        "the checkpoint ref must be identical after a round trip"
    );
    assert_eq!(
        ref_oid(target, CHAIN_REF).as_deref(),
        Some(src_chain),
        "the chain ref must be identical after a round trip"
    );
}

/// Set-union: a second import of the same bundle applies nothing.
fn re_import_applies_nothing(target: &Path, tcache: &Path, out: &Path, src_ck: &str) {
    let (code, stdout, err) = rift(
        target,
        tcache,
        &["--json", "bundle", "import", out.to_str().unwrap()],
    );
    assert_eq!(code, 0, "re-import failed: {stdout}{err}");
    let doc: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(doc["applied"].as_array().unwrap().len(), 0);
    assert_eq!(doc["already_present"], 2);
    assert_eq!(ref_oid(target, CHECKPOINTS_REF).as_deref(), Some(src_ck));
}

/// The imported repository verifies: rebuild projects what the bundle brought
/// in, reading every artifact it carries without a parse issue, and `rift
/// verify` re-derives the roots from it.
fn imported_repository_verifies(target: &Path, tcache: &Path) {
    let (code, stdout, err) = rift(target, tcache, &["--json", "rebuild"]);
    assert_eq!(code, 0, "rebuild after import failed: {stdout}{err}");
    let doc: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(
        doc["parse_issues"], 0,
        "the bundle's shards parse whole: {doc}"
    );
    let (code, stdout, err) = rift(target, tcache, &["--json", "verify"]);
    assert_eq!(code, 0, "verify after import must be green: {stdout}{err}");
}

/// REGRESSION PIN (a gap a review caught in this claim's first form): `rift
/// rebuild`'s substrate walk must be the ONLY writer of a checkpoint's
/// `strategy`, because it is the only place that reads the checkpoint's own
/// `metadata.json` to recover the real value. `cmd_import` is refs-only by
/// design (see its doc comment in bins/rift/src/bundle.rs) — it never calls
/// `rift_store::import_segment`, which would otherwise write a placeholder
/// strategy FIRST. `record_checkpoint_if_new` is insert-or-ignore and
/// canonical tables have no `UPDATE`, so a placeholder landing first would
/// PERMANENTLY shadow rebuild's real value the very next time rebuild runs —
/// silently, with no error and no red anywhere else. This assertion is the
/// one place in the tree that reads the row back through the shipped binary
/// and can catch that: wiring `import_segment` into `cmd_import` reds it
/// (verified by hand; see the commit message this test landed in).
fn rebuild_alone_writes_the_checkpoint_strategy(target: &Path, tcache: &Path) {
    let (code, stdout, err) = rift(target, tcache, &["--json", "checkpoint", "list"]);
    assert_eq!(
        code, 0,
        "checkpoint list after rebuild failed: {stdout}{err}"
    );
    let doc: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let checkpoints = doc["checkpoints"].as_array().unwrap();
    assert_eq!(
        checkpoints.len(),
        1,
        "the fixture archives exactly one checkpoint: {doc}"
    );
    assert_eq!(
        checkpoints[0]["strategy"], "manual-commit",
        "the checkpoint's strategy must be rebuild's real, metadata-derived value \
         -- a placeholder written by cmd_import first would shadow it permanently: {doc}"
    );
}

/// Tamper is refused **all-or-nothing**: a byte flipped inside the bundled
/// shard data leaves zero refs moved and the bundle's objects absent from the
/// object store.
///
/// **Which layer refuses is measured, not assumed.** Removing the manifest's
/// git-bundle digest check does NOT redden this test — git's own pack integrity
/// catches a flipped pack byte on its own, and refuses before writing objects.
/// So what this test pins is the *outcome* (refused, nothing moved, no partial
/// state), and it pins it over two independent mechanisms. The digest's unique
/// contribution is a different tamper the pack checksum cannot see — a flipped
/// `merkle_root` inside the segment — and that one is calibrated in
/// `bundle::tests::only_the_digest_catches_a_flipped_merkle_root`.
#[test]
fn a_tampered_bundle_is_refused_and_moves_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, cache) = seeded_repo(tmp.path());
    let src_ck = ref_oid(&repo, CHECKPOINTS_REF).unwrap();
    let out = tmp.path().join("x.riftbundle");
    let (code, stdout, err) = rift(
        &repo,
        &cache,
        &["--json", "bundle", "export", "--out", out.to_str().unwrap()],
    );
    assert_eq!(code, 0, "export failed: {stdout}{err}");

    // Flip one byte deep inside the git-bundle part — real shard data, not the
    // framing, so the length headers still parse and only the digest can catch
    // it. (The control below proves the untampered bundle applies.)
    let mut bytes = std::fs::read(&out).unwrap();
    let n = bytes.len();
    let idx = n - (n / 4).max(2);
    bytes[idx] ^= 0x01;
    let tampered = tmp.path().join("tampered.riftbundle");
    std::fs::write(&tampered, &bytes).unwrap();

    let target = empty_target(tmp.path(), "victim");
    let tcache = tmp.path().join("vcache");
    let (code, stdout, stderr) = rift(
        &target,
        &tcache,
        &["--json", "bundle", "import", tampered.to_str().unwrap()],
    );
    assert_ne!(
        code, 0,
        "a tampered bundle must not import: {stdout}{stderr}"
    );
    assert!(
        stderr.contains("integrity") || stdout.contains("integrity"),
        "the refusal must be an integrity refusal: {stdout}{stderr}"
    );

    assert!(
        ref_oid(&target, CHECKPOINTS_REF).is_none(),
        "no ref may move on a refused import"
    );
    assert!(
        ref_oid(&target, CHAIN_REF).is_none(),
        "no ref may move on a refused import"
    );
    assert!(
        !git_probe(&target, &["cat-file", "-e", &src_ck]),
        "no partial state: a refused import must not have written the bundle's objects"
    );

    // The control: the SAME target accepts the untampered bundle, so the
    // refusal above is about the flipped byte and not about the fixture.
    let (code, stdout, err) = rift(
        &target,
        &tcache,
        &["--json", "bundle", "import", out.to_str().unwrap()],
    );
    assert_eq!(code, 0, "the untampered control must import: {stdout}{err}");
    assert_eq!(
        ref_oid(&target, CHECKPOINTS_REF).as_deref(),
        Some(src_ck.as_str())
    );
}

/// A cursor naming a position this chain does not hold is a refusal, not a
/// clamp — and nothing is written.
#[test]
fn exporting_from_a_cursor_the_chain_does_not_hold_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, cache) = seeded_repo(tmp.path());
    let out = tmp.path().join("never.riftbundle");
    let (code, stdout, stderr) = rift(
        &repo,
        &cache,
        &[
            "--json",
            "bundle",
            "export",
            "--since",
            "rift-cursor/1:99:aaaaaaaa",
            "--out",
            out.to_str().unwrap(),
        ],
    );
    assert_ne!(code, 0, "a cursor past the chain must be refused: {stdout}");
    assert!(!out.exists(), "a refused export writes no file: {stderr}");
}

/// **Step 6b, end to end: an incremental bundle is refused by a repository
/// with no chain, and accepted once the chain it resumes from is there.**
///
/// The origin bundle seeds a fresh repository; a second bundle exported
/// `--since` that bundle's `to_cursor` is an incremental window (its
/// `from_cursor` is not the origin). Into an EMPTY target it is refused —
/// before this check it imported cleanly (a blind review found it) —
/// with both refs still absent. The positive control is the same file into
/// the same target after the origin bundle landed: the cursor now names a
/// held position, so it applies (nothing new to move, nothing refused).
#[test]
fn an_incremental_bundle_is_refused_by_a_repository_with_no_chain() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, cache) = seeded_repo(tmp.path());

    let origin = tmp.path().join("origin.riftbundle");
    let held = export_origin_and_hold_its_head(&repo, &cache, &origin);
    let incremental = tmp.path().join("incremental.riftbundle");
    export_incremental_from(&repo, &cache, &held, &incremental);

    let target = empty_target(tmp.path(), "fresh");
    let tcache = tmp.path().join("tcache");
    an_empty_target_refuses_the_incremental(&target, &tcache, &incremental);
    the_incremental_attaches_after_its_origin(&target, &tcache, &origin, &incremental, &held);
}

/// Export the origin bundle and return its `to_cursor`: the held position an
/// incremental window resumes from.
fn export_origin_and_hold_its_head(repo: &Path, cache: &Path, origin: &Path) -> String {
    let (code, stdout, err) = rift(
        repo,
        cache,
        &[
            "--json",
            "bundle",
            "export",
            "--out",
            origin.to_str().unwrap(),
        ],
    );
    assert_eq!(code, 0, "origin export failed: {stdout}{err}");
    let doc: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let held = doc["to_cursor"].as_str().unwrap().to_owned();
    assert!(
        !held.ends_with(':'),
        "the fixture's head cursor must not be the origin: {held}"
    );
    held
}

/// Export the window that resumes from `held`, asserting it does.
fn export_incremental_from(repo: &Path, cache: &Path, held: &str, incremental: &Path) {
    let (code, stdout, err) = rift(
        repo,
        cache,
        &[
            "--json",
            "bundle",
            "export",
            "--since",
            held,
            "--out",
            incremental.to_str().unwrap(),
        ],
    );
    assert_eq!(code, 0, "incremental export failed: {stdout}{err}");
    let doc: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(
        doc["from_cursor"], held,
        "the window must resume from the held cursor"
    );
}

/// Into a target with no chain the incremental bundle is refused by the attach
/// rule, and both refs stay absent.
fn an_empty_target_refuses_the_incremental(target: &Path, tcache: &Path, incremental: &Path) {
    let (code, stdout, err) = rift(
        target,
        tcache,
        &["--json", "bundle", "import", incremental.to_str().unwrap()],
    );
    assert_ne!(
        code, 0,
        "an incremental bundle must not bootstrap a repository with no chain: {stdout}"
    );
    assert!(
        format!("{stdout}{err}").contains("cannot bootstrap"),
        "the refusal must name the attach rule: {stdout}{err}"
    );
    assert!(
        ref_oid(target, CHECKPOINTS_REF).is_none(),
        "a refusal moves no ref"
    );
    assert!(
        ref_oid(target, CHAIN_REF).is_none(),
        "a refusal moves no ref"
    );
}

/// Positive control: origin first, then the same incremental file attaches.
fn the_incremental_attaches_after_its_origin(
    target: &Path,
    tcache: &Path,
    origin: &Path,
    incremental: &Path,
    held: &str,
) {
    let (code, stdout, err) = rift(
        target,
        tcache,
        &["--json", "bundle", "import", origin.to_str().unwrap()],
    );
    assert_eq!(code, 0, "origin import failed: {stdout}{err}");
    assert!(ref_oid(target, CHAIN_REF).is_some());
    let (code, stdout, err) = rift(
        target,
        tcache,
        &["--json", "bundle", "import", incremental.to_str().unwrap()],
    );
    assert_eq!(
        code, 0,
        "the incremental bundle must attach once its cursor is held: {stdout}{err}"
    );
    let doc: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(doc["from_cursor"], held);
    assert_eq!(
        doc["links"], 0,
        "an empty window past the head moves nothing"
    );
}
