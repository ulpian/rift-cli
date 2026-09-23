//! What an arm exercised, beside what it cost.
//!
//! A hook that got fast by ceasing to work looks identical in every timing, so
//! each arm publishes the facts a product surface can be asked for: the
//! projections under its own cache, the trailer, what `verify` re-derived and
//! what `checkpoint explain` says it read.

use std::{collections::BTreeMap, fs, path::Path};

use serde::{Deserialize, Serialize};

/// The commit-message trailer a live capture arm must produce.
const TRAILER: &str = "Rift-Checkpoint";

/// What an arm exercised, published beside what it cost.
///
/// A hook that got fast by ceasing to work looks identical in every timing, so
/// none of these is decorative. They are reported for **every** arm including
/// the control — a check that only ever confirms is not a check, and the idle
/// arm's *absent* trailer is what makes the live arm's present one mean
/// something.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Oracles {
    /// Every `*.db` under this arm's own cache root, with its size.
    ///
    /// A map rather than a number because the defect this replaces was exactly
    /// a second file: the harness wrote a 52 MB projection to a path rift never
    /// opened, rift created a fresh 405 KB one beside it, and "the 50 MB arm"
    /// was measured on the small one. Two entries here is a finding, not a
    /// rounding difference.
    pub projections: BTreeMap<String, u64>,
    /// Whether the last commit's message carries the checkpoint trailer.
    pub trailer_present: bool,
    /// Checkpoints `rift verify` re-derived in this repository.
    pub verify_checkpoints: Option<u64>,
    /// Shards `rift verify` redaction/Merkle-checked.
    pub verify_shards: Option<u64>,
    /// Events the projection holds, from `rift status`.
    pub captured_events: Option<u64>,
    /// What `rift checkpoint explain` reports for the arm's last checkpoint,
    /// under the label the verb itself uses: **"files read"**.
    ///
    /// Named that way on purpose. It is the closest observable the shipped CLI
    /// offers to "how many spans did the opted-in walk record", and it is not
    /// proven to be the same number — so it is published as the verb's own
    /// figure rather than relabelled as a span count the harness has not
    /// established. The inclusion walk's true record count is not on any CLI
    /// surface today; a request for one is open with the owner.
    #[serde(default)]
    pub explain_files_read: Option<u64>,
}

/// Every `*.db` under `cache_root`, by file name, with its size.
#[must_use]
pub fn projection_sizes(cache_root: &Path) -> BTreeMap<String, u64> {
    let mut out = BTreeMap::new();
    let mut stack = vec![cache_root.to_path_buf()];
    while let Some(path) = stack.pop() {
        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.is_dir() {
            if let Ok(entries) = fs::read_dir(&path) {
                stack.extend(entries.flatten().map(|e| e.path()));
            }
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) == Some("db") {
            let name = stable_cache_key(
                &path
                    .strip_prefix(cache_root)
                    .unwrap_or(&path)
                    .display()
                    .to_string(),
            );
            out.insert(name, meta.len());
        }
    }
    out
}

/// Replace a volatile sandbox repo-id path segment with a stable placeholder.
///
/// **The measurement here is the byte count; the id was an accident of the
/// key.** `resolve_cache_root` keys per repository, so a projection's path
/// carries the 32-hex id of the throwaway sandbox the run created — an
/// identifier for a directory that stopped existing when the run ended, and a
/// different one on every run.
///
/// Recording it cost three things. It put 224 high-entropy tokens into the
/// tracked corpus that `rift-shadow-redact`'s false-positive gate walks, for no
/// provenance value. It made **two runs of the same arm textually
/// undiffable**, since every run differed from every other purely by sandbox
/// id — which degrades the ratchet's own comparison. And it invited the fix of
/// raising a security crate's ceiling to accommodate noise.
///
/// Collapsing is lossless: within one arm's map there is exactly one
/// `rift/<id>/projection.db` beside `rift/canopy/ui-state.db`, so the
/// placeholder collides with nothing and drops no row.
#[must_use]
pub fn stable_cache_key(relative: &str) -> String {
    relative
        .split('/')
        .map(|seg| {
            if seg.len() >= 32 && seg.chars().all(|c| c.is_ascii_hexdigit()) {
                "<repo-id>"
            } else {
                seg
            }
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// Read `re-derived N checkpoint(s) … M shard(s)` out of `rift verify`.
#[must_use]
pub fn parse_verify(stdout: &str) -> (Option<u64>, Option<u64>) {
    let checkpoints = stdout
        .split_once("re-derived ")
        .and_then(|(_, rest)| rest.split_whitespace().next())
        .and_then(|n| n.parse().ok());
    let shards = stdout
        .split_once("; ")
        .and_then(|(_, rest)| rest.split_whitespace().next())
        .and_then(|n| n.parse().ok());
    (checkpoints, shards)
}

/// Whether a commit message carries the checkpoint trailer.
#[must_use]
pub fn has_trailer(message: &str) -> bool {
    message.lines().any(|l| l.trim_start().starts_with(TRAILER))
}

/// Read `files read: N` out of `rift checkpoint explain`.
#[must_use]
pub fn parse_explain_files(stdout: &str) -> Option<u64> {
    stdout
        .lines()
        .find_map(|l| l.trim().strip_prefix("files read:"))
        .and_then(|rest| rest.trim().parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The parser was never the problem, and this is the shape it reads —
    /// taken verbatim from a real `rift checkpoint explain` run in an arm
    /// sandbox on this machine.
    #[test]
    fn the_explain_parser_reads_the_real_verbs_real_output() {
        let real = "checkpoint 1a15bfb0fc3d45cf9aaaae4726740e8f85d3d74a66e47afef713d3f8ff11451f\n                      anchored: 6a87047dc463  strategy: manual-commit  canon: 3  created_at_ms: -1\n                      digests: digest.shape.json\n  files read: 1\n    README.md\n";
        assert_eq!(parse_explain_files(real), Some(1));
        // And the output the eighteen recorded runs actually got: a checkpoint
        // with no shape digest, so no line to read.
        let without = "checkpoint 7964925569b1dee3\n  digests: digest.intent.json · digest.log.json\n                         prompt (redacted, transcript line 0):\n";
        assert_eq!(parse_explain_files(without), None);
    }

    #[test]
    fn verify_output_yields_both_counts_and_neither_when_it_did_not_run() {
        let (checkpoints, shards) = parse_verify(
            "verify: OK — re-derived 26 checkpoint(s) from refs/rift/chain/v1; \
             26 shard(s) redaction/Merkle-checked\n",
        );
        assert_eq!((checkpoints, shards), (Some(26), Some(26)));
        assert_eq!(parse_verify(""), (None, None));
    }

    #[test]
    fn the_trailer_oracle_reads_the_trailer_and_not_a_mention_of_it() {
        assert!(has_trailer("step 3\n\nRift-Checkpoint: 01JABC\n"));
        assert!(!has_trailer("step 3\n"));
        assert!(
            !has_trailer("step 3\n\nthis commit talks about Rift-Checkpoint trailers\n"),
            "a body that mentions the trailer does not carry one"
        );
    }

    /// The §8 defect, in test form: a second projection under one arm's cache
    /// is the shape of "the number is about a store rift never opened", so the
    /// oracle enumerates rather than counting.
    #[test]
    fn every_projection_under_an_arms_cache_is_enumerated_with_its_size() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = tmp.path().join("rift-cache");
        fs::create_dir_all(cache.join("rift/abc")).unwrap();
        fs::write(cache.join("rift/abc/projection.db"), vec![0u8; 4096]).unwrap();
        fs::write(cache.join("stray.db"), vec![0u8; 128]).unwrap();
        fs::write(cache.join("notes.txt"), b"ignored").unwrap();

        let found = projection_sizes(&cache);
        assert_eq!(found.len(), 2, "both, so a stray one is visible: {found:?}");
        assert_eq!(found.get("stray.db"), Some(&128));
        assert_eq!(found.get("rift/abc/projection.db"), Some(&4096));
        assert!(projection_sizes(&tmp.path().join("nope")).is_empty());
    }

    /// **No committed artifact carries a volatile sandbox identifier.**
    ///
    /// The artifacts recorded `rift/<32-hex>/projection.db` — 224 tokens, 112
    /// distinct ids, one per throwaway sandbox — because `oracles.projections`
    /// keys disk usage **by path** and the path carries the repo-id. The
    /// measurement was always the byte count; the id was an accident of the
    /// key, identifying a directory that stopped existing when the run ended.
    ///
    /// It reddened `rift-shadow-redact`'s false-positive gate (whose corpus is
    /// `git ls-files` from the **workspace** root, so every tracked file added
    /// here is inside it) and it made two runs of the same arm textually
    /// undiffable, degrading the ratchet's own comparison.
    ///
    /// **This guard reads the artifacts, not the generator.** A generator-side
    /// check would pass forever while stale files kept the tokens.
    ///
    /// The guard matches the offending path shape, rather than treating every
    /// long hexadecimal value as volatile. Full Git object ids, fixture heads,
    /// and binary/script digests are durable provenance in the newer artifact
    /// schemas; an allowlist of today's values would reject each legitimate
    /// future run while still failing to describe the defect this guards.
    #[test]
    fn no_committed_artifact_carries_a_volatile_sandbox_id() {
        let planted = format!("rift/{}/projection.db", "a".repeat(32));
        assert_eq!(
            volatile_cache_repo_ids(&planted),
            vec!["a".repeat(32)],
            "the guard must red on the path shape it exists to prevent"
        );
        assert!(
            volatile_cache_repo_ids(&format!("git_describe={}", "b".repeat(40))).is_empty(),
            "a full Git object id is durable provenance, not a sandbox path"
        );

        let root = crate::leg::default_repo_root().join("benchmarks");
        let mut stack = vec![root.clone()];
        let mut checked = 0usize;
        while let Some(path) = stack.pop() {
            let Ok(meta) = fs::symlink_metadata(&path) else {
                continue;
            };
            if meta.is_dir() {
                if let Ok(entries) = fs::read_dir(&path) {
                    stack.extend(entries.flatten().map(|e| e.path()));
                }
                continue;
            }
            let Ok(text) = fs::read_to_string(&path) else {
                continue;
            };
            checked += 1;
            let volatile = volatile_cache_repo_ids(&text);
            assert!(
                volatile.is_empty(),
                "{} carries volatile sandbox path segment(s) {volatile:?} — the repository id \
                 has no provenance value, makes equivalent runs textually differ, and lands in \
                 a security crate's false-positive corpus",
                path.display(),
            );
        }
        assert!(checked >= 37, "only {checked} artifacts were checked");
    }

    fn volatile_cache_repo_ids(text: &str) -> Vec<String> {
        text.match_indices("rift/")
            .filter_map(|(at, marker)| {
                let tail = &text[at + marker.len()..];
                let repo_id = tail.split('/').next().unwrap_or_default();
                (repo_id.len() >= 32 && repo_id.chars().all(|c| c.is_ascii_hexdigit()))
                    .then(|| repo_id.to_owned())
            })
            .collect()
    }

    #[test]
    fn explain_output_yields_the_files_read_count_it_labels() {
        assert_eq!(parse_explain_files("  files read: 16\n"), Some(16));
        assert_eq!(parse_explain_files("nothing of the sort\n"), None);
    }
}
