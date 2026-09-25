//! The per-file edit stacks the replay consumes: stored hunk rows keyed by
//! their replay position, and the watermark a mid-session commit leaves.

use std::collections::BTreeMap;

use rift_core::Result;
use rift_git::BlobReader;
use rift_shadow_attribution::replay::TurnEdit;
use rift_shadow_attribution::Actor;
use rift_store::{turn_boundary_files_for_session_path, CanonicalStore, TurnDiffHunkRow};

/// The position of one recorded hunk in a session's replay order: the turn
/// it closed, then the window kind in the store's own order (`human`/`start`
/// = 0 before `agent`/`end` = 1). A boundary is addressed by the same key, so
/// a hunk with key ≤ a boundary's key was already visible at that boundary.
pub(super) type StackKey = (i64, u8);

/// Convert stored rows into the replay's per-file edit stacks (rows arrive in
/// replay order from the store query).
pub(super) fn edit_stacks(rows: &[TurnDiffHunkRow]) -> BTreeMap<String, Vec<(StackKey, TurnEdit)>> {
    let mut map: BTreeMap<String, Vec<(StackKey, TurnEdit)>> = BTreeMap::new();
    for r in rows {
        let actor = if r.edit_kind == "agent" {
            Actor::Agent
        } else {
            Actor::Human
        };
        let clamp = |v: i64| u32::try_from(v).unwrap_or(0);
        map.entry(r.path.clone()).or_default().push((
            (r.turn_ordinal, kind_rank(&r.edit_kind)),
            TurnEdit {
                actor,
                hunk: rift_shadow_attribution::Hunk {
                    old_start: clamp(r.old_start),
                    old_len: clamp(r.old_len),
                    new_start: clamp(r.new_start),
                    new_len: clamp(r.new_len),
                },
            },
        ));
    }
    map
}

/// A file already committed mid-session. If a boundary observed
/// exactly the blob this commit diffed against, the hunks up to that boundary
/// were consumed by the earlier commit — drop them and replay from the REAL
/// parent's length (returned as `Some`), so the length guard in
/// [`enrich_attribution`](super::enrich_attribution) stays meaningful instead of failing the whole file
/// to `unknown`. No such boundary → the stack is returned untouched and the
/// caller replays it as recorded (honest unknown).
pub(super) async fn rebase_on_parent(
    store: &impl CanonicalStore,
    reader: &impl BlobReader,
    session_row: &str,
    f: &rift_shadow_attribution::FileChange,
    keyed: Vec<(StackKey, TurnEdit)>,
) -> Result<(Vec<TurnEdit>, Option<u32>)> {
    let mut keyed = keyed;
    let mut parent_len = None;
    if let Some(base_oid) = f.base_blob_oid.as_deref() {
        let observed = turn_boundary_files_for_session_path(store, session_row, &f.path).await?;
        if let Some(watermark) = consumed_through(&observed, base_oid) {
            keyed = stack_after(keyed, watermark);
            parent_len = reader.read_blob(base_oid)?.as_deref().map(line_count);
        }
    }
    Ok((keyed.into_iter().map(|(_, e)| e).collect(), parent_len))
}

/// Count the lines of a byte buffer the way the diff engine does.
#[allow(clippy::naive_bytecount)] // tiny inputs; a bytecount dep is not warranted
pub(super) fn line_count(bytes: &[u8]) -> u32 {
    if bytes.is_empty() {
        return 0;
    }
    let n = bytes.iter().filter(|b| **b == b'\n').count();
    let tail = usize::from(!bytes.ends_with(b"\n"));
    u32::try_from(n + tail).unwrap_or(u32::MAX)
}

/// The window-kind rank of a hunk's `edit_kind` or a boundary's token.
fn kind_rank(token: &str) -> u8 {
    u8::from(matches!(token, "agent" | "end"))
}

/// **The watermark a mid-session commit leaves.** Given the
/// boundaries at which this session observed a file (`(turn, boundary,
/// blob_oid)`, oldest first) and the blob the current commit diffed against,
/// the LATEST boundary whose observed blob IS that parent: every hunk recorded
/// at or before it was already committed by an earlier commit, so replaying it
/// again over the real parent double-applies it (the stack over-lengthens the
/// model and `committed_writers` degrades the whole file to `unknown`). `None`
/// when no boundary observed the parent — the session never saw that content,
/// so there is nothing honest to rebase on and the caller replays the
/// stack as recorded.
fn consumed_through(rows: &[(i64, String, String)], base_oid: &str) -> Option<StackKey> {
    rows.iter()
        .filter(|(_, _, oid)| oid == base_oid)
        .map(|(turn, boundary, _)| (*turn, kind_rank(boundary)))
        .max()
}

/// The hunks NOT yet consumed by an earlier commit: everything whose key is
/// after `watermark`. Order is preserved.
fn stack_after(edits: Vec<(StackKey, TurnEdit)>, watermark: StackKey) -> Vec<(StackKey, TurnEdit)> {
    edits.into_iter().filter(|(k, _)| *k > watermark).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn consumed_through_truth_table() {
        let row = |t: i64, b: &str, oid: &str| (t, b.to_owned(), oid.to_owned());
        assert_eq!(consumed_through(&[], "X"), None);
        assert_eq!(consumed_through(&[row(1, "end", "Y")], "X"), None);
        assert_eq!(consumed_through(&[row(1, "start", "X")], "X"), Some((1, 0)));
        assert_eq!(consumed_through(&[row(1, "end", "X")], "X"), Some((1, 1)));
        // Several boundaries observed the parent (the file sat unchanged
        // across windows): the LATEST is the watermark.
        assert_eq!(
            consumed_through(
                &[row(1, "end", "X"), row(2, "start", "X"), row(2, "end", "Z")],
                "X"
            ),
            Some((2, 0))
        );
        // stack_after keeps strictly-later keys, in order.
        let e = |k: StackKey| {
            (
                k,
                TurnEdit {
                    actor: Actor::Agent,
                    hunk: rift_shadow_attribution::Hunk {
                        old_start: 0,
                        old_len: 0,
                        new_start: 0,
                        new_len: 1,
                    },
                },
            )
        };
        let kept: Vec<StackKey> =
            stack_after(vec![e((1, 0)), e((1, 1)), e((2, 0)), e((2, 1))], (1, 1))
                .into_iter()
                .map(|(k, _)| k)
                .collect();
        assert_eq!(kept, vec![(2, 0), (2, 1)]);
    }
}
