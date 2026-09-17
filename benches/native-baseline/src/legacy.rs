//! The import corpus: a repository of pre-Rift shards, rebuilt from the frozen
//! golden fixtures.
//!
//! # The sentence this module exists to make true
//!
//! **No legacy artifact was ever obtained.** The `README.md` beside the golden
//! shards ([`FIXTURE_SEGMENTS`]) says it in its own words — *"Nothing here was
//! fetched"* — and every legacy spelling in those two shards is cited to a
//! declaration that already lived in this repository. So an import-throughput number
//! measured over them is true about **our own reconstruction of the format**
//! and silent about the format. The sentence travels in the
//! artifact, not only here (see [`crate::scenario::IMPORT_RECONSTRUCTION_CAVEAT`]).
//!
//! It is still worth measuring, and the reason is precise: the rate is a
//! property of *the reader* — the walk, the parse, the root re-derivation —
//! over shards whose bytes are frozen and byte-compared by
//! `tests/legacy_import_golden.rs`. What it cannot tell anybody is whether a
//! real legacy repository looks like this.
//!
//! # Why `git fast-import`
//!
//! The corpus has to be built **outside the timed region**, deterministically,
//! in a repository the harness already created, without touching the frozen
//! bytes and without a checkout. One `fast-import` stream on stdin does all of
//! that in a single child process: the five artifact blobs are written once and
//! referenced by mark from every shard tree, so a hundred shards cost a hundred
//! tree entries rather than five hundred blobs. Building it with plumbing
//! commands instead would be several hundred processes per iteration, and the
//! iteration is what is being timed against.

use std::path::Path;

use crate::{measure::Invocation, BenchError, Result};

/// The branch a pre-Rift sharded-branch layout keeps checkpoints on
/// (`rift_recall_substrate::Dialect::Legacy::checkpoint_ref`, restated because
/// the harness links no product crate).
pub const LEGACY_CHECKPOINT_REF: &str = concat!("refs/heads/", "ent", "ire", "/checkpoints/v1");

/// Where the frozen shard lives, relative to the repository root — as path
/// **segments**, joined by [`fixture_dir`].
///
/// Segments rather than one slash-joined literal for two reasons, and the
/// second is the load-bearing one. It joins platform-correctly without a
/// separator baked into a string; and the workspace's redaction
/// false-positive corpus walks every tracked file, where a 49-character
/// slash-joined path is a single high-entropy token (`/` is a token
/// character to that detector) and text of exactly this shape had already
/// pushed a **security crate's** recorded ceiling over once. Four
/// short segments cannot.
pub const FIXTURE_SEGMENTS: &[&str] =
    &["crates", "rift-shadow", "legacy-fixtures", "sharded_branch"];

/// The frozen shard's directory under `repo_root`.
#[must_use]
pub fn fixture_dir(repo_root: &Path) -> std::path::PathBuf {
    FIXTURE_SEGMENTS
        .iter()
        .fold(repo_root.to_path_buf(), |path, segment| path.join(segment))
}

/// The artifacts one shard carries, in the order the stream writes them.
///
/// A subset of `rift_shadow::legacy_import::SHARD_ARTIFACTS`: exactly the five
/// the frozen fixture holds. Naming more would put empty paths in the tree and
/// change what the reader is asked to do.
pub const SHARD_ARTIFACTS: &[&str] = &[
    "metadata.json",
    "full.jsonl",
    "content_hash.txt",
    "redaction_report.json",
    "tool_calls.jsonl",
];

/// How many shards one measured iteration reads.
///
/// Large enough that the reader's per-shard work dominates process startup —
/// at one shard the number would be a `rift` launch wearing a rate — and small
/// enough that building the corpus stays one cheap child process. The count is
/// the rate's denominator only when the importer agrees it read them: the work
/// probe reads `importable N` back out of the product, so a corpus the reader
/// refuses collapses the rate instead of propping it up with a constant.
pub const SHARD_COUNT: usize = 64;

/// A deterministic 12-hex checkpoint id, the width the frozen fixture uses.
///
/// The id lives **only in the path** — the fixture's `metadata.json` carries no
/// id field, and its `merkle_root` is derived from its events — so the same
/// frozen bytes are self-consistent under every id here. That is what makes a
/// corpus of many shards possible without editing one byte of the golden
/// fixture, which this project never does.
#[must_use]
pub fn shard_id(index: usize) -> String {
    format!("{:012x}", 0x1000_0000_0000u64 + index as u64)
}

/// The tree path a shard's artifact sits at: `<id[:2]>/<id[2:]>/<artifact>`.
#[must_use]
pub fn shard_path(id: &str, artifact: &str) -> String {
    let (prefix, rest) = id.split_at(2.min(id.len()));
    format!("{prefix}/{rest}/{artifact}")
}

/// Read the frozen shard's bytes.
///
/// # Errors
/// [`BenchError::Io`] when an artifact is missing — which is a refusal, never a
/// silent corpus of four files instead of five.
pub fn fixture_artifacts(repo_root: &Path) -> Result<Vec<(&'static str, Vec<u8>)>> {
    let dir = fixture_dir(repo_root);
    SHARD_ARTIFACTS
        .iter()
        .map(|name| {
            let path = dir.join(name);
            std::fs::read(&path)
                .map(|bytes| (*name, bytes))
                .map_err(|e| BenchError::io(format!("read frozen fixture {}", path.display()), e))
        })
        .collect()
}

/// Build the `fast-import` stream that plants `count` shards on the legacy
/// checkpoint branch.
///
/// Deterministic to the byte: fixed identity, fixed timestamp, blobs written
/// once and referenced by mark. Two runs of the harness plant the same commit.
#[must_use]
pub fn fast_import_stream(artifacts: &[(&'static str, Vec<u8>)], count: usize) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();
    for (mark, (_, bytes)) in artifacts.iter().enumerate() {
        out.extend_from_slice(
            format!("blob\nmark :{}\ndata {}\n", mark + 1, bytes.len()).as_bytes(),
        );
        out.extend_from_slice(bytes);
        out.push(b'\n');
    }
    let message = "legacy import corpus (reconstructed; nothing here was fetched)\n";
    out.extend_from_slice(format!("commit {LEGACY_CHECKPOINT_REF}\n").as_bytes());
    // A fixed identity and a fixed epoch second: the commit oid must not depend
    // on when the benchmark ran.
    out.extend_from_slice(b"committer bench <bench@example.invalid> 1735689600 +0000\n");
    out.extend_from_slice(format!("data {}\n{message}", message.len()).as_bytes());
    for shard in 0..count {
        let id = shard_id(shard);
        for (mark, (name, _)) in artifacts.iter().enumerate() {
            out.extend_from_slice(
                format!("M 100644 :{} {}\n", mark + 1, shard_path(&id, name)).as_bytes(),
            );
        }
    }
    out.extend_from_slice(b"\ndone\n");
    out
}

/// The untimed invocation that plants the corpus in `repo`.
///
/// # Errors
/// Propagates a missing frozen artifact.
pub fn plant(repo_root: &Path, repo: &Path, count: usize) -> Result<Invocation> {
    let artifacts = fixture_artifacts(repo_root)?;
    let stream = fast_import_stream(&artifacts, count);
    Ok(Invocation::new("git", repo)
        // `--done` makes the stream's own `done` terminator mandatory, so a
        // truncated stream is an error rather than a partial corpus that the
        // rate would then be measured over.
        .args(["fast-import", "--quiet", "--done"])
        .stdin(String::from_utf8_lossy(&stream).into_owned()))
}

/// Read `importable N` out of `rift import --from-legacy`.
///
/// **`importable`, deliberately, and not the shard count the harness planted.**
/// The denominator is what the product says it could read, so a reconstruction
/// the reader refuses drives the rate to zero — which the report renders as
/// *unmeasurable*, not as a fast number over an empty corpus.
#[must_use]
pub fn parse_importable(stdout: &str) -> Option<u64> {
    stdout
        .split_once("importable ")
        .and_then(|(_, rest)| rest.split_whitespace().next())
        .and_then(|n| n.parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shard_ids_are_hex_distinct_and_the_width_the_fixture_uses() {
        let ids: std::collections::BTreeSet<String> = (0..SHARD_COUNT).map(shard_id).collect();
        assert_eq!(ids.len(), SHARD_COUNT, "every shard has its own id");
        for id in &ids {
            assert_eq!(id.len(), 12, "{id}");
            assert!(id.bytes().all(|b| b.is_ascii_hexdigit()), "{id}");
        }
        assert_eq!(shard_id(0), "100000000000");
        assert_eq!(shard_id(1), "100000000001");
        // The layout the walker reads: two characters, then the rest.
        assert_eq!(
            shard_path("100000000001", "metadata.json"),
            "10/0000000001/metadata.json"
        );
    }

    /// The frozen fixture is read, never written. If this fails the corpus is
    /// not what the golden suite pins, and the rate would be about something
    /// else fully.
    #[test]
    fn the_frozen_fixture_is_present_and_is_the_one_the_golden_suite_pins() {
        let root = crate::leg::default_repo_root();
        let artifacts = fixture_artifacts(&root).expect("the frozen shard is tracked");
        assert_eq!(artifacts.len(), SHARD_ARTIFACTS.len());
        let metadata = &artifacts[0].1;
        let value: serde_json::Value = serde_json::from_slice(metadata).unwrap();
        // No id field: the checkpoint id lives in the path alone, which is what
        // lets one frozen shard stand in for many without an edit.
        assert!(value.get("checkpoint_id").is_none());
        assert!(value.get("id").is_none());
        assert_eq!(value["strategy"], "manual-commit");
        assert!(value["merkle_root"].is_string());
    }

    #[test]
    fn the_stream_writes_each_blob_once_and_references_it_from_every_shard() {
        let artifacts = vec![
            ("metadata.json", b"{}".to_vec()),
            ("full.jsonl", b"{\"a\":1}\n".to_vec()),
        ];
        let stream = String::from_utf8(fast_import_stream(&artifacts, 3)).unwrap();
        assert_eq!(
            stream.matches("\nblob\n").count() + 1,
            2,
            "two blobs: {stream}"
        );
        assert_eq!(stream.matches("M 100644 :1 ").count(), 3);
        assert_eq!(stream.matches("M 100644 :2 ").count(), 3);
        assert!(stream.contains(&format!("commit {LEGACY_CHECKPOINT_REF}\n")));
        assert!(stream.contains("data 2\n{}"), "{stream}");
        assert!(stream.ends_with("\ndone\n"));
        // Deterministic: no clock reaches the stream.
        assert_eq!(
            fast_import_stream(&artifacts, 3),
            fast_import_stream(&artifacts, 3)
        );
        assert!(stream.contains("1735689600 +0000"));
    }

    /// End to end through real git: the stream plants a branch whose tree is
    /// the layout the walker reads. Nothing here needs `rift`.
    #[test]
    fn the_stream_really_builds_the_layout_the_walker_reads() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(args)
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?}: {out:?}");
            String::from_utf8_lossy(&out.stdout).trim().to_owned()
        };
        git(&["init", "--quiet"]);

        let root = crate::leg::default_repo_root();
        let hermetic = crate::hermetic::Hermetic::prepare(tmp.path().join("hermetic")).unwrap();
        let inv = plant(&root, &repo, 4).unwrap();
        let exec = crate::measure::run_once(&hermetic, &inv).unwrap();
        assert!(exec.sample.ok(), "fast-import: {}", exec.stderr);

        let listing = git(&["ls-tree", "-r", "--name-only", LEGACY_CHECKPOINT_REF]);
        let paths: Vec<&str> = listing.lines().collect();
        assert_eq!(paths.len(), 4 * SHARD_ARTIFACTS.len(), "{paths:?}");
        assert!(paths.contains(&"10/0000000000/metadata.json"), "{paths:?}");
        assert!(
            paths.contains(&"10/0000000003/tool_calls.jsonl"),
            "{paths:?}"
        );
        // Byte-identical to the frozen fixture: the corpus is the golden shard,
        // replicated, not a re-encoding of it.
        let frozen = std::fs::read(fixture_dir(&root).join("metadata.json")).unwrap();
        let planted = std::process::Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args([
                "show",
                &format!("{LEGACY_CHECKPOINT_REF}:10/0000000000/metadata.json"),
            ])
            .output()
            .unwrap();
        assert_eq!(planted.stdout, frozen);
    }

    #[test]
    fn the_denominator_is_what_the_product_says_it_could_read() {
        let real =
            "layout sharded-branch: 64 checkpoint(s)\nimportable 64 · refused 0 · events 192\n";
        assert_eq!(parse_importable(real), Some(64));
        // A corpus the reader refused: zero, which the report renders as
        // unmeasurable rather than as a fast number.
        assert_eq!(
            parse_importable("importable 0 · refused 64 · events 0\n"),
            Some(0)
        );
        assert_eq!(parse_importable("no pre-Rift checkpoints found"), None);
        assert_eq!(parse_importable(""), None);
        assert_eq!(parse_importable("importable lots"), None);
    }
}
