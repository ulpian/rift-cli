//! The verbs that read and record checkpoint history in git refs:
//! `rift import --from-legacy`, which must report the layout the walk that
//! found a shard names rather than a hardcoded fallback, and `rift chain`,
//! which records the branch's **write** order and refuses to invent one.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use std::path::Path;

#[allow(dead_code)] // The CLI suites share these builders; each file uses a subset.
#[path = "common/cli_fixture.rs"]
mod cli_fixture;
mod common;
use cli_fixture::{code, git, rift, RETIRED_BRAND};

/// Build a repo whose ONLY legacy artifact is a per-checkpoint ref — an
/// adversarial review's `onlyrefs` plant, as a fixture: the shape that made the first version of
/// this verb report "no pre-Rift checkpoints found" with exit 0.
fn seed_only_refs_legacy_repo(home: &Path, repo: &Path) -> String {
    const ULID: &str = "01JQZ8T9WQKX3M7VN2P4RB6CDE";
    assert!(git(home, repo, &["init", "-q", "-b", "main"])
        .status
        .success());
    std::fs::write(repo.join("a.txt"), "a\n").unwrap();
    assert!(git(home, repo, &["add", "-A"]).status.success());
    assert!(git(home, repo, &["commit", "-qm", "seed"]).status.success());

    // The shard tree, on an orphan commit reachable ONLY through the ref.
    assert!(git(home, repo, &["checkout", "-q", "--orphan", "staging"])
        .status
        .success());
    assert!(git(home, repo, &["rm", "-rfq", "."]).status.success());
    let (hi, rest) = ULID.split_at(2);
    let shard = repo.join(hi).join(rest);
    std::fs::create_dir_all(&shard).unwrap();
    std::fs::write(
        shard.join("metadata.json"),
        r#"{"strategy":"manual-commit","session_ids":["01JLEGACYSESSIONX"]}"#,
    )
    .unwrap();
    std::fs::write(
        shard.join("full.jsonl"),
        "{\"session_id\":\"01JLEGACYSESSIONX\",\"turn\":0,\"event\":0,\"kind\":\"session.start\",\"verb\":\"SessionStart\",\"occurred_at\":1}\n",
    )
    .unwrap();
    std::fs::write(
        shard.join("redaction_report.json"),
        format!(
            r#"{{"rule_pack_versions":["legacy@0.7.8"],"input_sha256":"{}"}}"#,
            "d".repeat(64)
        ),
    )
    .unwrap();
    assert!(git(home, repo, &["add", "-A"]).status.success());
    assert!(git(home, repo, &["commit", "-qm", "legacy"])
        .status
        .success());
    let oid = String::from_utf8(git(home, repo, &["rev-parse", "HEAD"]).stdout)
        .unwrap()
        .trim()
        .to_owned();
    assert!(git(
        home,
        repo,
        &[
            "update-ref",
            &format!("refs/{RETIRED_BRAND}/checkpoints/{hi}/{ULID}"),
            &oid,
        ],
    )
    .status
    .success());
    assert!(git(home, repo, &["checkout", "-q", "main"])
        .status
        .success());
    assert!(git(home, repo, &["branch", "-qD", "staging"])
        .status
        .success());
    ULID.to_owned()
}

/// The rejection, as a test. A repository carrying ONLY the per-checkpoint-refs
/// layout must be imported and reported — not answered with a confident zero.
#[test]
fn import_from_legacy_reads_a_repo_that_has_only_the_per_checkpoint_refs_layout() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let ulid = seed_only_refs_legacy_repo(home.path(), repo.path());

    let out = rift(home.path(), repo.path(), &["import", "--from-legacy"]);
    assert_eq!(code(&out), 0, "{out:?}");
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        !text.contains("no pre-Rift checkpoints"),
        "the false negative this test exists for: {text}"
    );
    // Reported under the RIGHT layout — the previous version hardcoded the other.
    assert!(
        text.contains("layout per-checkpoint-refs: 1"),
        "layout must be derived from the carrying ref: {text}"
    );
    assert!(text.contains("importable 1"), "{text}");
    // Still no brand on the surface.
    assert!(
        !text.to_ascii_lowercase().contains(RETIRED_BRAND),
        "brand leaked: {text}"
    );

    // …and the JSON agrees, including the count that replaced the always-zero
    // `sources` field.
    let out = rift(
        home.path(),
        repo.path(),
        &["import", "--from-legacy", "--json"],
    );
    assert_eq!(code(&out), 0, "{out:?}");
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("import emits json");
    assert_eq!(v["refs_layout_shards"], serde_json::json!(1), "{v}");
    assert_eq!(v["importable"], serde_json::json!(1), "{v}");
    assert_eq!(
        v["by_layout"]["per-checkpoint-refs"],
        serde_json::json!(1),
        "{v}"
    );
    assert_eq!(v["trust"], serde_json::json!("claim"), "{v}");
    assert!(v["root_checks"].is_object(), "{v}");

    // Read-only: the ref the shard lives on is untouched, byte for byte.
    let refs = git(home.path(), repo.path(), &["for-each-ref"]).stdout;
    assert!(
        String::from_utf8_lossy(&refs)
            .contains(&format!("refs/{RETIRED_BRAND}/checkpoints/01/{ulid}")),
        "the source ref survives the import unchanged"
    );
}
/// Write one legacy shard tree (`<id[:2]>/<id[2:]>/…`) into the working tree.
fn write_legacy_shard(repo: &Path, id: &str) {
    let (hi, rest) = id.split_at(2);
    let shard = repo.join(hi).join(rest);
    std::fs::create_dir_all(&shard).unwrap();
    std::fs::write(
        shard.join("metadata.json"),
        r#"{"strategy":"manual-commit","session_ids":["01JLEGACYSESSIONX"]}"#,
    )
    .unwrap();
    std::fs::write(
        shard.join("full.jsonl"),
        "{\"session_id\":\"01JLEGACYSESSIONX\",\"turn\":0,\"event\":0,\"kind\":\"session.start\",\"verb\":\"SessionStart\",\"occurred_at\":1}\n",
    )
    .unwrap();
    std::fs::write(
        shard.join("redaction_report.json"),
        format!(
            r#"{{"rule_pack_versions":["legacy@0.7.8"],"input_sha256":"{}"}}"#,
            "d".repeat(64)
        ),
    )
    .unwrap();
}

/// Commit the current working tree as an orphan and park it at `full_ref`,
/// reachable ONLY through that ref. Returns nothing; `main` is checked out.
fn park_orphan_tree_at(home: &Path, repo: &Path, full_ref: &str, seed: impl Fn(&Path)) {
    assert!(git(home, repo, &["checkout", "-q", "--orphan", "staging"])
        .status
        .success());
    assert!(git(home, repo, &["rm", "-rfq", "."]).status.success());
    seed(repo);
    assert!(git(home, repo, &["add", "-A"]).status.success());
    assert!(git(home, repo, &["commit", "-qm", "legacy"])
        .status
        .success());
    let oid = String::from_utf8(git(home, repo, &["rev-parse", "HEAD"]).stdout)
        .unwrap()
        .trim()
        .to_owned();
    assert!(git(home, repo, &["update-ref", full_ref, &oid])
        .status
        .success());
    assert!(git(home, repo, &["checkout", "-q", "main"])
        .status
        .success());
    assert!(git(home, repo, &["branch", "-qD", "staging"])
        .status
        .success());
}

fn seed_main_with_one_commit(home: &Path, repo: &Path) {
    assert!(git(home, repo, &["init", "-q", "-b", "main"])
        .status
        .success());
    std::fs::write(repo.join("a.txt"), "a\n").unwrap();
    assert!(git(home, repo, &["add", "-A"]).status.success());
    assert!(git(home, repo, &["commit", "-qm", "seed"]).status.success());
}

/// An adversarial review's junk-ref shape, as a fixture. Three shards live under a ref that is
/// inside the legacy checkpoint namespace but is **not** spelled
/// `<shard>/<id>`, so `layout_of_shard_ref` refuses to classify its name — and
/// the repository has **no** legacy branch at all.
fn seed_junk_ref_legacy_repo(home: &Path, repo: &Path) -> Vec<&'static str> {
    const IDS: [&str; 3] = [
        "01JQZ8T9WQKX3M7VN2P4RB6CD1",
        "01JQZ8T9WQKX3M7VN2P4RB6CD2",
        "01JQZ8T9WQKX3M7VN2P4RB6CD3",
    ];
    seed_main_with_one_commit(home, repo);
    park_orphan_tree_at(
        home,
        repo,
        concat!("refs/", "ent", "ire", "/checkpoints/scratch"),
        |r| {
            for id in IDS {
                write_legacy_shard(r, id);
            }
        },
    );
    IDS.to_vec()
}

/// The layout label must come from the walk that FOUND the shard, not from a
/// hardcoded fallback. This repository has no legacy branch, so any occurrence
/// of `sharded-branch` in the output is a fabricated label — which is exactly
/// what the previous `.unwrap_or(Layout::ShardedBranch)` printed here.
#[test]
fn import_from_legacy_never_labels_a_branchless_repo_sharded_branch() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let ids = seed_junk_ref_legacy_repo(home.path(), repo.path());

    let out = rift(home.path(), repo.path(), &["import", "--from-legacy"]);
    assert_eq!(code(&out), 0, "{out:?}");
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    // THE ASSERTION THIS TEST EXISTS FOR: there is no branch in this repository.
    assert!(
        !text.contains("sharded-branch"),
        "a layout that is not present must never be named: {text}"
    );
    assert!(
        text.contains(&format!("layout per-checkpoint-refs: {}", ids.len())),
        "the ref walk found them, so the ref walk names them: {text}"
    );
    // …and nothing was dropped on the way to being labelled correctly.
    assert!(
        text.contains(&format!("importable {}", ids.len())),
        "{text}"
    );

    let out = rift(
        home.path(),
        repo.path(),
        &["import", "--from-legacy", "--json"],
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("import emits json");
    assert_eq!(v["by_layout"]["per-checkpoint-refs"], 3, "{v}");
    assert_eq!(v["by_layout"]["sharded-branch"], serde_json::Value::Null);
    assert_eq!(v["refs_layout_shards"], 3, "{v}");
    // Always emitted, so a reader can tell "nothing unattributed" from "nobody
    // looked" — and so the residual can never be silently relabelled again.
    assert_eq!(v["unattributed"], 0, "{v}");
}

/// The other direction of the same fix: a repository whose only legacy artifact
/// IS the shared orphan branch must still be labelled `sharded-branch` — which
/// the removed fallback used to do by accident. Nothing covered this before, so
/// dropping the fallback could have silently unlabelled every branch shard.
#[test]
fn import_from_legacy_labels_the_branch_layout_from_the_branch_walk() {
    const HEX_ID: &str = "a1b2c3d4e5f60718";
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    seed_main_with_one_commit(home.path(), repo.path());
    park_orphan_tree_at(
        home.path(),
        repo.path(),
        concat!("refs/heads/", "ent", "ire", "/checkpoints/v1"),
        |r| write_legacy_shard(r, HEX_ID),
    );

    let out = rift(home.path(), repo.path(), &["import", "--from-legacy"]);
    assert_eq!(code(&out), 0, "{out:?}");
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(text.contains("layout sharded-branch: 1"), "{text}");
    assert!(!text.contains("per-checkpoint-refs"), "{text}");
    assert!(text.contains("importable 1"), "{text}");

    let out = rift(
        home.path(),
        repo.path(),
        &["import", "--from-legacy", "--json"],
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("import emits json");
    assert_eq!(v["by_layout"]["sharded-branch"], 1, "{v}");
    assert_eq!(v["refs_layout_shards"], 0, "{v}");
    assert_eq!(v["unattributed"], 0, "{v}");
}

/// `rift import --from-legacy` on a repository with no pre-Rift checkpoints: a
/// clean, quiet zero. The verb is READ-ONLY, so this also pins that it writes
/// nothing — the ref listing before and after is byte-identical.
#[test]
fn import_from_legacy_is_read_only_and_quiet_when_there_is_nothing_to_read() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    assert!(git(home.path(), repo.path(), &["init", "-q", "."])
        .status
        .success());
    std::fs::write(repo.path().join("a.txt"), "a\n").unwrap();
    assert!(git(home.path(), repo.path(), &["add", "a.txt"])
        .status
        .success());
    assert!(git(home.path(), repo.path(), &["commit", "-qm", "seed"])
        .status
        .success());

    let refs_before = git(home.path(), repo.path(), &["for-each-ref"]).stdout;
    let out = rift(home.path(), repo.path(), &["import", "--from-legacy"]);
    assert_eq!(code(&out), 0, "an empty import is success: {out:?}");
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        text.contains("no pre-Rift checkpoints"),
        "says so plainly: {text}"
    );
    // The legacy tool's name appears nowhere a user can see it.
    let lower = text.to_ascii_lowercase();
    assert!(!lower.contains(RETIRED_BRAND), "brand leaked: {text}");

    let refs_after = git(home.path(), repo.path(), &["for-each-ref"]).stdout;
    assert_eq!(refs_before, refs_after, "import is read-only");
}

/// `rift import` with no source selected is a precondition error, not a silent
/// no-op a script would read as "there was nothing to import".
#[test]
fn import_without_a_source_is_a_precondition_error() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    assert!(git(home.path(), repo.path(), &["init", "-q", "."])
        .status
        .success());
    let out = rift(home.path(), repo.path(), &["import"]);
    // 4 = PRECONDITION, not 1: a script can tell "you did not ask for anything"
    // apart from "the import failed".
    assert_eq!(
        code(&out),
        4,
        "no source selected is a precondition: {out:?}"
    );
}

// --- `rift chain` — the checkpoint order -------------------------------------

/// Content-hash order is `A < MID < B`; **write order is A, B, MID**. Any verb
/// that sorts ids and calls the result an order puts B at the head; the
/// branch's own first-parent history puts MID there. Every assertion below
/// turns on that difference, which is the fork this ref exists to end.
const CK_A: &str = "4c5c0a053e73b924663009e1602882b6073d3fdd10ad33aa92c988a643e040f3";
const CK_B: &str = "b970a014c1a3dd6508c5382679051a37f7cc1cd238b66543ecb1472a746d23c7";
const CK_MID: &str = "93ae336492a162b429cf27a5952d8d8f0a7a878eca243585d5bc33893d7d0d9a";

/// Write one checkpoint shard onto the branch currently checked out, as one
/// commit — the shape `NativeBackend::archive` writes.
fn commit_shard(home: &Path, repo: &Path, id: &str) {
    let (hi, rest) = id.split_at(2);
    let shard = repo.join(hi).join(rest);
    std::fs::create_dir_all(&shard).unwrap();
    std::fs::write(
        shard.join("metadata.json"),
        r#"{"strategy":"manual-commit"}"#,
    )
    .unwrap();
    assert!(git(home, repo, &["add", "-A"]).status.success());
    assert!(git(home, repo, &["commit", "-qm", id]).status.success());
}

/// A repository whose orphan `rift/checkpoints/v1` branch carries `ids`, one
/// commit each, in the order given.
fn seed_checkpoint_branch(home: &Path, repo: &Path, ids: &[&str]) {
    assert!(git(home, repo, &["init", "-q", "-b", "main"])
        .status
        .success());
    std::fs::write(repo.join("a.txt"), "a\n").unwrap();
    assert!(git(home, repo, &["add", "-A"]).status.success());
    assert!(git(home, repo, &["commit", "-qm", "seed"]).status.success());
    assert!(git(
        home,
        repo,
        &["checkout", "-q", "--orphan", "rift/checkpoints/v1"]
    )
    .status
    .success());
    assert!(git(home, repo, &["rm", "-rfq", "."]).status.success());
    for id in ids {
        commit_shard(home, repo, id);
    }
    assert!(git(home, repo, &["checkout", "-qf", "main"])
        .status
        .success());
}

fn chain_status_json(home: &Path, repo: &Path) -> serde_json::Value {
    let out = rift(home, repo, &["chain", "status", "--json"]);
    assert_eq!(code(&out), 0, "{out:?}");
    serde_json::from_slice(&out.stdout).expect("chain status --json")
}

/// `rift chain init` records the branch's **write** order and names its head.
#[test]
fn chain_init_records_the_branch_write_order_and_names_the_head() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    assert!(
        CK_A < CK_MID && CK_MID < CK_B,
        "the fixture must carry the sort hazard"
    );
    seed_checkpoint_branch(home.path(), repo.path(), &[CK_A, CK_B, CK_MID]);

    let out = rift(home.path(), repo.path(), &["chain", "init"]);
    assert_eq!(code(&out), 0, "{out:?}");
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(text.contains("3 entries"), "entries written: {text}");
    assert!(text.contains("head entry:"), "the head is named: {text}");
    assert!(
        text.contains(&format!("head checkpoint: {CK_MID}")),
        "the head checkpoint is the LAST WRITTEN one: {text}"
    );

    let st = chain_status_json(home.path(), repo.path());
    assert_eq!(st["positions"], 3);
    assert_eq!(st["exists"], true);
    assert_eq!(st["head_checkpoint"], CK_MID);
    assert_ne!(
        st["head_checkpoint"], CK_B,
        "sorted ids would have put B at the head — that sort is the fork"
    );
}

/// The refusal, rendered as an explanation, with nothing moved behind it.
#[test]
fn a_second_chain_init_is_refused_as_a_rewrite_and_moves_nothing() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    seed_checkpoint_branch(home.path(), repo.path(), &[CK_A, CK_B]);
    assert_eq!(code(&rift(home.path(), repo.path(), &["chain", "init"])), 0);
    let before = chain_status_json(home.path(), repo.path());

    let out = rift(home.path(), repo.path(), &["chain", "init"]);
    // 65 = INTEGRITY: refused on an append-only ground, and not retryable.
    assert_eq!(code(&out), 65, "{out:?}");
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(err.contains("already exists"), "{err}");
    assert!(
        err.contains("rewrite wearing an initializer's name"),
        "the refusal explains itself: {err}"
    );
    assert!(
        err.contains("Nothing was written"),
        "and says what it did not do: {err}"
    );
    assert!(
        out.stdout.is_empty(),
        "a refusal writes nothing to stdout: {out:?}"
    );

    let after = chain_status_json(home.path(), repo.path());
    assert_eq!(before["head_entry"], after["head_entry"], "the ref moved");
    assert_eq!(before["positions"], after["positions"]);
}

/// An absent ref is reported as absent — never invented by the reading verb.
#[test]
fn chain_status_reports_an_absent_ref_without_creating_one() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    seed_checkpoint_branch(home.path(), repo.path(), &[CK_A]);

    let out = rift(home.path(), repo.path(), &["chain", "status"]);
    assert_eq!(code(&out), 0, "reading an absent ref is not a failure");
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(text.contains("does not exist yet"), "{text}");
    assert!(
        text.contains("rift chain init"),
        "says what records it: {text}"
    );

    let st = chain_status_json(home.path(), repo.path());
    assert_eq!(st["exists"], false);
    assert_eq!(st["positions"], 0);
    // …and status is read-only: the ref is still absent afterwards.
    let rp = git(
        home.path(),
        repo.path(),
        &["rev-parse", "--verify", "--quiet", "refs/rift/chain/v1"],
    );
    assert!(!rp.status.success(), "status created the ref: {rp:?}");
}

/// A branch that forked and merged has no single write order. Report it;
/// linearizing it would be the same invention the sorted-hash chain was.
#[test]
fn a_merged_checkpoint_branch_is_refused_rather_than_linearized() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    seed_checkpoint_branch(home.path(), repo.path(), &[CK_A]);
    // Fork the checkpoint branch, write a checkpoint on each side, merge.
    assert!(git(
        home.path(),
        repo.path(),
        &["checkout", "-qf", "rift/checkpoints/v1"]
    )
    .status
    .success());
    assert!(
        git(home.path(), repo.path(), &["checkout", "-q", "-b", "side"])
            .status
            .success()
    );
    commit_shard(home.path(), repo.path(), CK_MID);
    assert!(git(
        home.path(),
        repo.path(),
        &["checkout", "-qf", "rift/checkpoints/v1"]
    )
    .status
    .success());
    commit_shard(home.path(), repo.path(), CK_B);
    assert!(git(
        home.path(),
        repo.path(),
        &["merge", "-q", "--no-edit", "side"]
    )
    .status
    .success());
    assert!(git(home.path(), repo.path(), &["checkout", "-qf", "main"])
        .status
        .success());

    let out = rift(home.path(), repo.path(), &["chain", "init"]);
    assert_eq!(code(&out), 65, "{out:?}");
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(err.contains("not linear"), "names the shape: {err}");
    assert!(err.contains("Nothing was written"), "{err}");
    let rp = git(
        home.path(),
        repo.path(),
        &["rev-parse", "--verify", "--quiet", "refs/rift/chain/v1"],
    );
    assert!(
        !rp.status.success(),
        "a refused init leaves no partial chain: {rp:?}"
    );
}
