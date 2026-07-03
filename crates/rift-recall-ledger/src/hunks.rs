//! Changed-hunk computation between `FileRevision` pairs (Stage 3 Phase 2 Step 3).
//!
//! Line-range hunks via `imara-diff`, pinned to **`Algorithm::Histogram`** so
//! output is stable across runs/machines (a different algorithm yields different
//! — though valid — boundaries, which would break content-addressing). Hunks are
//! the rung between *summary* and *exact range* in the manifest-first ladder:
//! [`serve_hunks`] returns just the changed lines under a token budget, with the
//! overflow behind a retrieval handle.

use std::ops::Range;

use imara_diff::intern::InternedInput;
use imara_diff::sources::byte_lines_with_terminator;
use imara_diff::{diff, Algorithm};
use rift_core::Result;
use rift_recall_types::{Handle, LineRange};
use rift_store::CanonicalStore;

use crate::graph::{sha256_id, Built};

/// A single changed line-range hunk (0-based line indices, `with_terminator`
/// tokenization so concatenating lines round-trips the file bytes exactly).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hunk {
    /// First changed line in the old revision.
    pub old_start: u32,
    /// Number of old lines replaced.
    pub old_len: u32,
    /// First changed line in the new revision.
    pub new_start: u32,
    /// Number of new lines.
    pub new_len: u32,
    /// Whole-file binary change (never line-diffed).
    pub binary: bool,
}

/// Whether `bytes` are treated as binary (a NUL byte) — routed to a whole-file
/// hunk and a handle rather than inlined as garbage.
#[must_use]
pub fn is_binary(bytes: &[u8]) -> bool {
    bytes.contains(&0)
}

/// Split into lines **including** the trailing `\n` (matches imara-diff's
/// `byte_lines_with_terminator`), so concatenation reproduces the input exactly.
#[must_use]
pub fn split_lines(data: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut start = 0usize;
    for (i, &b) in data.iter().enumerate() {
        if b == b'\n' {
            if let Some(line) = data.get(start..=i) {
                out.push(line);
            }
            start = i + 1;
        }
    }
    if let Some(tail) = data.get(start..) {
        if !tail.is_empty() {
            out.push(tail);
        }
    }
    out
}

/// Diff `old`→`new` into line-range hunks. Pure function of the two byte slices +
/// the fixed Histogram algorithm → deterministic. A binary side yields a single
/// whole-file hunk flagged `binary`.
#[must_use]
pub fn compute_hunks(old: &[u8], new: &[u8]) -> Vec<Hunk> {
    if is_binary(old) || is_binary(new) {
        return vec![Hunk {
            old_start: 0,
            old_len: u32::try_from(split_lines(old).len()).unwrap_or(u32::MAX),
            new_start: 0,
            new_len: u32::try_from(split_lines(new).len()).unwrap_or(u32::MAX),
            binary: true,
        }];
    }
    let input = InternedInput::new(
        byte_lines_with_terminator(old),
        byte_lines_with_terminator(new),
    );
    let mut hunks = Vec::new();
    diff(
        Algorithm::Histogram,
        &input,
        |before: Range<u32>, after: Range<u32>| {
            hunks.push(Hunk {
                old_start: before.start,
                old_len: before.end - before.start,
                new_start: after.start,
                new_len: after.end - after.start,
                binary: false,
            });
        },
    );
    hunks.sort_by_key(|h| (h.old_start, h.new_start));
    hunks
}

/// The content-addressed id of a changed hunk: `sha256(old_rev ‖ new_rev ‖
/// ranges)`, so recomputation is an idempotent set-union no-op.
#[must_use]
pub fn changed_hunk_id(old_rev: &str, new_rev: &str, h: &Hunk) -> String {
    sha256_id(
        "rift.hunk.v1",
        &[
            old_rev.as_bytes(),
            new_rev.as_bytes(),
            &h.old_start.to_le_bytes(),
            &h.old_len.to_le_bytes(),
            &h.new_start.to_le_bytes(),
            &h.new_len.to_le_bytes(),
        ],
    )
}

/// Persist the hunks for a revision pair as content-addressed `ChangedHunk`
/// nodes with `Commit→changed→ChangedHunk` and `ChangedHunk→in→FileRevision`
/// edges. Idempotent (re-persisting the same pair adds no rows). Returns the
/// number of hunks.
///
/// # Errors
/// Propagates store write errors.
pub async fn persist_hunks(
    store: &impl CanonicalStore,
    commit_id: &str,
    old_rev: &str,
    new_rev: &str,
    old_bytes: &[u8],
    new_bytes: &[u8],
) -> Result<usize> {
    let hunks = compute_hunks(old_bytes, new_bytes);
    let mut b = Built::default();
    for h in &hunks {
        let id = changed_hunk_id(old_rev, new_rev, h);
        b.node(&id, "ChangedHunk", &id);
        b.changed_hunk(
            &id,
            commit_id,
            new_rev,
            Some(i64::from(h.old_start)),
            Some(i64::from(h.old_len)),
            Some(i64::from(h.new_start)),
            Some(i64::from(h.new_len)),
            h.binary,
        );
        b.edge(commit_id, &id, "changed"); // Commit→changed→ChangedHunk
        b.edge(&id, new_rev, "in"); // ChangedHunk→in→FileRevision
    }
    crate::ingest::apply_rows(store, &b).await?;
    Ok(hunks.len())
}

/// A served changed hunk: its new-revision line range + the exact changed text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServedHunk {
    /// New-revision line range served (1-based inclusive, for handles).
    pub range: LineRange,
    /// The exact changed lines.
    pub text: String,
}

/// A budgeted page of served hunks (the "changed hunks" ladder rung).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HunkPage {
    /// The hunks that fit the budget.
    pub hunks: Vec<ServedHunk>,
    /// Whether the budget cut off remaining hunks.
    pub truncated: bool,
    /// Estimated tokens returned (never exceeds the budget).
    pub estimated_tokens_returned: u32,
    /// A handle to the remaining content when truncated (`available_by_handle`).
    pub overflow_handle: Option<Handle>,
}

/// The honest token count for hunk budgeting — the one workspace counting
/// authority (`rift-tokens`, ADR-0006), replacing the old `bytes/4` placeholder.
/// Non-empty-or-not, the cost floor stays ≥ 1 so budget math never treats a
/// hunk as free.
#[must_use]
pub fn estimate_tokens(text: &str) -> u32 {
    rift_tokens::count(text).max(1)
}

/// Serve changed hunks for `(path, new_rev)` under a token budget. When the
/// budget is exhausted the remaining hunks are **not** inlined — they are
/// offered via a `file:` handle (`available_by_handle`), keeping placement and
/// budget in the contract.
#[must_use]
pub fn serve_hunks(
    new_bytes: &[u8],
    hunks: &[Hunk],
    path: &str,
    new_sha256: &str,
    max_tokens: u32,
) -> HunkPage {
    let lines = split_lines(new_bytes);
    let mut page = HunkPage::default();
    for (idx, h) in hunks.iter().enumerate() {
        let lo = h.new_start as usize;
        let hi = (h.new_start + h.new_len) as usize;
        let text: String = lines
            .get(lo..hi.min(lines.len()))
            .unwrap_or(&[])
            .iter()
            .map(|l| String::from_utf8_lossy(l))
            .collect();
        let cost = estimate_tokens(&text);
        if page.estimated_tokens_returned.saturating_add(cost) > max_tokens {
            page.truncated = true;
            // remaining region → a content-addressed file handle (no inlining)
            let remaining_lo = hunks.get(idx).map_or(0, |h| h.new_start) + 1; // 1-based
            let remaining_hi = hunks
                .last()
                .map_or(remaining_lo, |last| last.new_start + last.new_len);
            page.overflow_handle = Some(Handle::File {
                path: path.to_string(),
                sha256: new_sha256.to_string(),
                range: Some(LineRange {
                    lo: remaining_lo,
                    hi: remaining_hi.max(remaining_lo),
                }),
            });
            break;
        }
        page.estimated_tokens_returned += cost;
        page.hunks.push(ServedHunk {
            range: LineRange {
                lo: h.new_start + 1,
                hi: (h.new_start + h.new_len).max(h.new_start + 1),
            },
            text,
        });
    }
    page
}

#[cfg(test)]
mod tests {
    use super::*;
    use rift_store::{recall_migrations, SqliteStore};

    fn block<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime")
            .block_on(f)
    }

    fn recall_store() -> SqliteStore {
        let s = SqliteStore::open_in_memory().unwrap();
        rift_store::apply_migrations(&s, &recall_migrations()).unwrap();
        s
    }

    /// Apply hunks to `old` to rebuild `new` — the diff-correctness oracle.
    fn reconstruct(old: &[u8], new: &[u8], hunks: &[Hunk]) -> Vec<u8> {
        let old_lines = split_lines(old);
        let new_lines = split_lines(new);
        let mut out: Vec<u8> = Vec::new();
        let mut old_pos = 0usize;
        for h in hunks {
            for l in old_lines.get(old_pos..h.old_start as usize).unwrap_or(&[]) {
                out.extend_from_slice(l);
            }
            let lo = h.new_start as usize;
            let hi = (h.new_start + h.new_len) as usize;
            for l in new_lines.get(lo..hi.min(new_lines.len())).unwrap_or(&[]) {
                out.extend_from_slice(l);
            }
            old_pos = (h.old_start + h.old_len) as usize;
        }
        for l in old_lines.get(old_pos..).unwrap_or(&[]) {
            out.extend_from_slice(l);
        }
        out
    }

    #[test]
    fn computes_simple_insert_delete_modify_hunks() {
        let old = b"a\nb\nc\n";
        let new = b"a\nB\nc\nd\n";
        let hunks = compute_hunks(old, new);
        assert!(!hunks.is_empty());
        assert_eq!(reconstruct(old, new, &hunks), new);
    }

    #[test]
    fn new_file_is_all_added() {
        let hunks = compute_hunks(b"", b"x\ny\n");
        assert_eq!(reconstruct(b"", b"x\ny\n", &hunks), b"x\ny\n");
    }

    #[test]
    fn binary_file_is_whole_file_changed_not_linediffed() {
        let hunks = compute_hunks(b"a\nb\n", b"a\x00b");
        assert_eq!(hunks.len(), 1);
        assert!(hunks[0].binary);
    }

    #[test]
    fn hunk_node_is_content_addressed_idempotent() {
        let store = recall_store();
        let old = b"a\nb\nc\n";
        let new = b"a\nB\nc\n";
        let n1 = block(persist_hunks(
            &store, "ckpt1", "rev_old", "rev_new", old, new,
        ))
        .unwrap();
        let rows1 = block(store.count("n_changed_hunk")).unwrap();
        let n2 = block(persist_hunks(
            &store, "ckpt1", "rev_old", "rev_new", old, new,
        ))
        .unwrap();
        let rows2 = block(store.count("n_changed_hunk")).unwrap();
        assert_eq!(n1, n2);
        assert_eq!(rows1, rows2, "re-persist is a set-union no-op");
    }

    #[test]
    fn serve_hunks_emits_estimated_tokens_and_respects_budget() {
        let new = b"line one is fairly long here\nline two also long enough\n";
        let hunks = compute_hunks(b"", new);
        // tiny budget → truncated + an overflow handle, tokens within budget
        let page = serve_hunks(new, &hunks, "f.rs", "deadbeef", 2);
        assert!(page.estimated_tokens_returned <= 2);
        if page.truncated {
            assert!(page.overflow_handle.is_some());
        }
    }

    proptest::proptest! {
        /// Applying the computed hunks to `old` reproduces `new` exactly — the
        /// diff-correctness invariant (`06 §6.1.2`).
        #[test]
        fn hunks_reconstruct_new_from_old(
            old in proptest::collection::vec(0u8..4, 0..40),
            new in proptest::collection::vec(0u8..4, 0..40),
        ) {
            // map small ints to a few line-ish tokens so diffs are meaningful
            let to_text = |v: &[u8]| -> Vec<u8> {
                v.iter().flat_map(|b| match b { 0 => b"a\n".to_vec(), 1 => b"b\n".to_vec(),
                    2 => b"c\n".to_vec(), _ => b"d\n".to_vec() }).collect()
            };
            let old = to_text(&old);
            let new = to_text(&new);
            let hunks = compute_hunks(&old, &new);
            proptest::prop_assert_eq!(reconstruct(&old, &new, &hunks), new);
        }

        /// `serve_hunks` never exceeds the budget and sets truncated+overflow
        /// when it would (the budget invariant).
        #[test]
        fn hunk_serving_respects_budget(
            budget in 0u32..50,
            nlines in 1usize..30,
        ) {
            let new: Vec<u8> = (0..nlines).flat_map(|i| format!("line number {i} content\n").into_bytes()).collect();
            let hunks = compute_hunks(b"", &new);
            let page = serve_hunks(&new, &hunks, "f.rs", "sha", budget);
            proptest::prop_assert!(page.estimated_tokens_returned <= budget);
            if page.truncated {
                proptest::prop_assert!(page.overflow_handle.is_some());
            }
        }
    }
}
