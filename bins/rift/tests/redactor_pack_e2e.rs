//! End-to-end: **a rule pack a user writes into their repository redacts the
//! bytes the real binary seals** (the parity row
//! `custom.pack_rule_unloadable`).
//!
//! The row this closes had been a `Verdict::Gap` since the loader landed, and
//! the record is explicit about why nothing cheaper closes it: re-pointing
//! `parity.rs` at a hand-built `RulePackSet::with_pack` composition would test
//! `rift-shadow-redact` against itself. There is no crate edge from
//! `rift-shadow-redact` to `rift-config`, and `capture_rule_packs` — the one
//! function that joins the loader to the compiler — is `pub(crate)` in a
//! **lib-less binary**, so it is unreachable from any unit or integration test
//! that is not this one. The only instrument that can answer the row's question
//! is the shipped binary, driven through a real capture hook, read back out of
//! the durable artifact.
//!
//! So: write a pack into a temp repository's `.rift/policies/redactors/`, put a
//! token in a file the capture path includes, run `rift hooks git post-commit`,
//! and read `content.redacted` out of the checkpoint shard git actually stores.
//!
//! **Both arms run, and the second is what makes the first mean anything.** A
//! test that only asserts "the token is gone" passes when the built-in pipeline
//! happened to catch it, when the file was never captured, and when the hook did
//! nothing at all. So the pack-absent arm asserts the same token **survives** in
//! the same shard of the same repository, and the pack-present arm additionally
//! reads `rule_pack_versions` out of the sealed report and requires the pack's
//! own `name@version` there. That string can only be in the artifact because
//! `RulePackSet::with_pack` compiled this file's rules into the pipeline that
//! produced these bytes — no built-in detector, no coincidence and no empty
//! capture can put it there.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::path::Path;
use std::process::{Command, Stdio};

mod common;
use common::rift_cmd;

/// The pack a user writes. `name` must equal the file stem, which is the
/// loader's rule; the rule carries a self-test so the pack is the shape the
/// documentation asks for and not a minimal one.
const PACK: &str = r#"schema_version = 1
name = "acme"
version = "2026.08.0"
description = "internal token shapes"

[[rules]]
id = "internal_token"
description = "ACME-XXXXXXXX"
regex = "ACME-[A-Z0-9]{8}"

[[rules.samples]]
input = "ACME-AB12CD34"
redacted = true
"#;

/// The token only the pack can see.
///
/// Chosen so that **no built-in detector can claim it**, which is what makes the
/// pack-absent arm a real calibration rather than a formality: 13 characters is
/// under the entropy layer's 20-byte floor, it is no vendor prefix, it is not a
/// credential key/value, and it is not a digit run of any length a numeric PII
/// detector reads. It is carried on its own line with spaces around it so no
/// neighbouring text can join it into a longer token.
const MARKER: &str = "ACME-AB12CD34";

/// The pack's `name@version`, as `with_pack` mints it into
/// `report.rule_pack_versions` and the archive carries it into the shard.
const PACK_VERSION: &str = "acme@2026.08.0";

fn git(repo: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(repo)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", repo.join("absent-gitconfig"))
        .env("GIT_AUTHOR_NAME", "T")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "T")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

fn git_out(repo: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(repo)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", repo.join("absent-gitconfig"))
        .output()
        .expect("git is on PATH");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// A hermetic repository with one commit and a live native-hook session, so the
/// post-commit leg has something to archive. Fixed timestamps only.
fn seed_repo(tmp: &Path) -> std::path::PathBuf {
    let repo = tmp.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    std::fs::write(repo.join("a.txt"), "one\n").unwrap();
    git(&repo, &["add", "a.txt"]);
    let sessions = repo.join(".git").join("rift-sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    std::fs::write(
        sessions.join("pack-e2e-1.json"),
        r#"{"schema":1,"session_id":"pack-e2e-1","state":"active","agent":"claude-code","current_turn_ordinal":1,"turn_open":true,"compaction_boundary":null,"updated_at_ms":1783000000000,"native_id_map":{}}"#,
    )
    .unwrap();
    git(&repo, &["commit", "-q", "-m", "seed"]);
    repo
}

/// What one arm produced: the sealed capture document, and the report beside it.
struct Sealed {
    blob: String,
    report: serde_json::Value,
}

/// Run one arm end to end through the real binary and read the shard back.
fn run_arm(tmp: &Path, write_pack: bool) -> Sealed {
    let repo = seed_repo(tmp);

    if write_pack {
        // Exactly where a user puts one, spelled the way the product documents
        // it rather than through a helper that could drift from the product.
        let dir = repo.join(".rift").join("policies").join("redactors");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("acme.toml"), PACK).unwrap();
    }

    // A file the capture path includes, carrying the marker and one ordinary
    // line beside it. The ordinary line is the proof that capture happened and
    // that redaction masked rather than dropped.
    std::fs::write(repo.join(".gitignore"), "notes.txt\n").unwrap();
    std::fs::write(
        repo.join("notes.txt"),
        format!("build is green\nmarker {MARKER}\n"),
    )
    .unwrap();
    std::fs::write(repo.join(".worktreeinclude"), "notes.txt\n").unwrap();

    let cache = tmp.join("cache");
    let data = tmp.join("data");
    let mut cmd = rift_cmd(&repo);
    cmd.args([
        "hooks",
        "git",
        "post-commit",
        "--git-dir",
        repo.join(".git").to_str().unwrap(),
    ])
    .stdin(Stdio::piped())
    .stdout(Stdio::null())
    .stderr(Stdio::piped())
    .env("RIFT_CACHE_DIR", cache.to_str().unwrap())
    .env("XDG_DATA_HOME", data.to_str().unwrap())
    .env("LOCALAPPDATA", data.to_str().unwrap())
    .env("RIFT_NO_KEYCHAIN", "1")
    .env("RIFT_CAPTURE_SYNC", "1");
    let child = cmd.spawn().unwrap();
    let out = child.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert_eq!(
        out.status.code().unwrap_or(-1),
        0,
        "the hook never fails the git operation; stderr: {stderr}"
    );

    let listing = git_out(
        &repo,
        &[
            "ls-tree",
            "-r",
            "--name-only",
            "refs/heads/rift/checkpoints/v1",
        ],
    );
    let mut shards: Vec<&str> = listing
        .lines()
        .filter_map(|p| p.rsplit_once('/').map(|(dir, _)| dir))
        .collect();
    shards.sort_unstable();
    shards.dedup();
    assert_eq!(shards.len(), 1, "exactly one checkpoint: {shards:?}");
    let shard = shards[0];

    let blob = git_out(
        &repo,
        &[
            "show",
            &format!("refs/heads/rift/checkpoints/v1:{shard}/content.redacted"),
        ],
    );
    let report = git_out(
        &repo,
        &[
            "show",
            &format!("refs/heads/rift/checkpoints/v1:{shard}/redaction_report.json"),
        ],
    );
    Sealed {
        blob,
        report: serde_json::from_str(&report).unwrap(),
    }
}

/// **The row's question, answered by the artifact git stores.**
#[test]
fn a_pack_in_the_repository_masks_the_bytes_the_binary_seals() {
    let tmp = tempfile::tempdir().unwrap();
    let sealed = run_arm(tmp.path(), true);

    // The capture is real and the file is in it: without this the next
    // assertion passes on an empty document.
    assert!(
        sealed.blob.contains("build is green"),
        "the included file's ordinary content is not in the shard: {:?}",
        sealed.blob
    );
    // The pack's token never reached the bytes git stores.
    assert!(
        !sealed.blob.contains(MARKER),
        "a pack rule did not reach the capture path: {:?}",
        sealed.blob
    );
    // Masked, not dropped — a pack rule is a vendor-class detector.
    assert!(
        sealed.blob.contains("[REDACTED:vendor]"),
        "the marker was removed rather than masked: {:?}",
        sealed.blob
    );
    // And the sealed report says which pipeline produced these bytes. Only
    // `with_pack` can mint this string, so this is the assertion that
    // distinguishes "a pack ran" from "something else caught it".
    let versions = sealed.report["rule_pack_versions"].as_array().unwrap();
    assert!(
        versions.iter().any(|v| v.as_str() == Some(PACK_VERSION)),
        "the sealed report does not record the pack that ran: {}",
        sealed.report
    );
    assert!(
        sealed.report["total_spans"].as_u64().unwrap() >= 1,
        "the report accounts for the masking: {}",
        sealed.report
    );
}

/// **The calibration, and it is not optional.**
///
/// The same repository, the same file, the same hook, the same marker — and no
/// pack. The marker must SURVIVE into the shard. Without this arm the test above
/// would pass on a tree where `capture_rule_packs` was deleted and the marker
/// happened to be caught by a built-in, which is precisely the false pass the
/// parity row's old note (`no pack a user writes reaches the capture path`) was
/// written under.
#[test]
fn without_a_pack_the_same_marker_survives_into_the_shard() {
    let tmp = tempfile::tempdir().unwrap();
    let sealed = run_arm(tmp.path(), false);

    assert!(
        sealed.blob.contains("build is green"),
        "the included file's ordinary content is not in the shard: {:?}",
        sealed.blob
    );
    assert!(
        sealed.blob.contains(MARKER),
        "the marker is supposed to be invisible to every BUILT-IN detector, so \
         this arm cannot calibrate the other one: {:?}",
        sealed.blob
    );
    let versions = sealed.report["rule_pack_versions"].as_array().unwrap();
    assert!(
        !versions.iter().any(|v| v.as_str() == Some(PACK_VERSION)),
        "a pack version appeared with no pack on disk: {}",
        sealed.report
    );
}
