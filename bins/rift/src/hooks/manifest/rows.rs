//! The entries one manifest is built from: the rows the session's own held
//! revisions make, and the rows the projection reports against the baseline.

use super::super::recovery::basis_snapshots_at;
use super::held::HeldRevision;

/// The most files one manifest asks the diff about — the cap the diff module
/// declares, applied here because a turn boundary is one of the two callers it
/// was declared for. It bounds the hashing too: a file past this point in the
/// held set is not read, not hashed and not named, so a session that has read
/// six hundred files pays for sixteen.
///
/// The warm end is the useful end. The held set is ranked by the last time the
/// window saw each file, and what a turn is about is what the agent was just
/// working on.
pub(super) const MAX_MANIFEST_HELD: usize = rift_recall_serve::held_diff::MAX_DIFFED_FILES;

/// What a manifest row says about a file beyond naming it — the owned half of
/// the renderer's own vocabulary, so the borrowed entries can be built in one
/// pass at the end.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum RowDetail {
    ChangedAt { ranges: Vec<(u32, u32)>, shift: i64 },
    Changed,
    Binary,
    Removed,
    Unstated,
}

/// One entry of the manifest: the path, the revision the session holds of it
/// (empty when it holds none), the revision the checkout has now, and what is
/// true of it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ManifestRow {
    pub(super) path: String,
    pub(super) hash: String,
    pub(super) current: String,
    pub(super) detail: RowDetail,
    /// The content hashes this one entry accounts for, kept beside the entry
    /// rather than pooled per turn.
    ///
    /// A pooled list cannot say *which* entry a hash came from, and that is
    /// exactly the question the baseline has to answer: an entry the push
    /// guard withheld was never reported, so absorbing its hash would tell
    /// the session it had been told about a file it was only counted for
    /// ([`decide_turn`](super::decide::decide_turn)).
    pub(super) accounts: Vec<String>,
}

/// A held file that is no longer the revision the session holds, with the
/// bytes to diff it against when they are worth diffing.
pub(super) struct MovedFile {
    pub(super) path: String,
    pub(super) hash: String,
    pub(super) current: String,
}

impl ManifestRow {
    /// A row the projection reported against the baseline and the session's own
    /// reads say nothing about: the entry this manifest has always rendered.
    ///
    /// The path is re-spelled on the way in for the same reason
    /// [`held_revisions`](super::held::held_revisions) re-spells its own: a row this manifest draws from a
    /// projection built before the `/` rule would otherwise be compared
    /// against — and rendered beside — held entries spelled the other way, and
    /// the push guard would withhold it outright.
    pub(super) fn from_baseline(path: &str, current: &str) -> Self {
        Self {
            path: rift_recall_substrate::worktree::slash_spelled(path).into_owned(),
            hash: String::new(),
            current: current.to_owned(),
            detail: RowDetail::Unstated,
            accounts: vec![current.to_owned()],
        }
    }

    /// The borrowed entry the renderer takes.
    pub(super) fn entry(&self) -> rift_recall_serve::ManifestEntry<'_> {
        rift_recall_serve::ManifestEntry {
            path: &self.path,
            hash: &self.hash,
            detail: match &self.detail {
                RowDetail::ChangedAt { ranges, shift } => {
                    rift_recall_serve::EntryDetail::ChangedAt {
                        ranges,
                        shift: *shift,
                    }
                }
                RowDetail::Changed => rift_recall_serve::EntryDetail::Changed,
                RowDetail::Binary => rift_recall_serve::EntryDetail::Binary,
                RowDetail::Removed => rift_recall_serve::EntryDetail::Removed,
                RowDetail::Unstated => rift_recall_serve::EntryDetail::Unstated,
            },
        }
    }
}

/// The turn's entries: what the session **holds** and has moved since, then
/// what the projection reports against the baseline.
///
/// Held first, and that order is the honesty of the frame's own sentence. It
/// says "file(s) you have read changed", and a file the journal has a read or
/// an edit for is one the session demonstrably read; the baseline is a set of
/// hashes the manifest once *announced*, which is a weaker claim and stays the
/// fallback for the sessions the journal knows nothing about.
///
/// Three rules bound it. A path this repository does not serve is not served
/// by a new renderer either, and is left out before its bytes are touched. A
/// revision the session was already told about is not announced twice — the
/// same `known` set that decides the baseline's own entries. And at most
/// [`MAX_MANIFEST_HELD`] files are looked at, so the walk is bounded by a
/// constant rather than by how much the session has read.
pub(super) fn manifest_rows(
    repo: &std::path::Path,
    store: &rift_store::SqliteStore,
    rt: &tokio::runtime::Runtime,
    held: &[HeldRevision],
    known: &[String],
    changed: &[(String, String)],
) -> Vec<ManifestRow> {
    let mut rows = held_rows(repo, store, rt, held, known);
    for (path, current) in changed {
        let row = ManifestRow::from_baseline(path, current);
        match rows.iter_mut().find(|held| held.path == row.path) {
            // The session's own reads already said something exact about this
            // file. Naming it twice would spend the budget to say less — and
            // the comparison is between two re-spelled names, so two spellings
            // of one file cannot slip past it as two files. The projection's
            // own revision of it still rides that one entry, so reporting the
            // entry accounts for both and withholding it accounts for neither.
            Some(held) => held.accounts.push(current.clone()),
            None => rows.push(row),
        }
    }
    rows
}

/// The rows the session's own held revisions make — an entry per file it read
/// or wrote whose bytes are no longer the bytes it holds.
fn held_rows(
    repo: &std::path::Path,
    store: &rift_store::SqliteStore,
    rt: &tokio::runtime::Runtime,
    held: &[HeldRevision],
    known: &[String],
) -> Vec<ManifestRow> {
    let moved = moved_held_files(repo, held, known);
    if moved.is_empty() {
        return Vec::new();
    }
    // The revision a window holds is reachable only through the family's
    // store and the read-only substrate. The store is already open on this
    // turn; the substrate is opened once, here, and only because at least
    // one held file has actually moved. An unreachable one costs ranges,
    // never the entry.
    let substrate = rift_recall_substrate::GixSubstrate::open(repo).ok();
    // And the ladder's second rung, which is the one that answers for a
    // revision the window edited after reading: the blob route reaches
    // nothing for it, and this manifest would say `(changed)` with no
    // lines.
    let snapshots = basis_snapshots_at(repo);
    let mut rows = Vec::with_capacity(moved.len());
    for (file, bytes) in moved {
        // A file with no current revision is one the checkout no longer has:
        // there is nothing to diff and nothing to read, and the fact itself is
        // what the entry is for.
        if file.current.is_empty() {
            rows.push(ManifestRow {
                path: file.path,
                hash: file.hash,
                current: file.current,
                detail: RowDetail::Removed,
                // Nothing to account for: a file the checkout no longer
                // has has no revision to record as told.
                accounts: Vec::new(),
            });
            continue;
        }
        let detail = match (&substrate, bytes) {
            (Some(substrate), Some(bytes)) => {
                let diff = rt.block_on(rift_recall_serve::held_diff::held_diff_against_basis(
                    store,
                    substrate,
                    snapshots.as_ref(),
                    &file.hash,
                    &bytes,
                ));
                where_it_moved(diff)
            }
            _ => RowDetail::Changed,
        };
        rows.push(ManifestRow {
            path: file.path,
            hash: file.hash,
            accounts: vec![file.current.clone()],
            current: file.current,
            detail,
        });
    }
    rows
}

/// What a diff of the held revision against the checkout lets an entry say.
///
/// Unchanged cannot arrive (the hashes differ), and the rest — a basis nobody
/// could reach, more changed places than an entry may name — is the same
/// honest "it changed and I will not say where".
fn where_it_moved(diff: rift_recall_serve::held_diff::HeldDiff) -> RowDetail {
    match diff {
        rift_recall_serve::held_diff::HeldDiff::Changed { ranges, shift } => {
            RowDetail::ChangedAt { ranges, shift }
        }
        rift_recall_serve::held_diff::HeldDiff::Binary => RowDetail::Binary,
        _ => RowDetail::Changed,
    }
}

/// The held files that moved, paired with the current bytes where a diff is
/// possible — the pass that decides whether the substrate is worth opening at
/// all, and the only place a held path's bytes are read.
///
/// `None` bytes mean the file changed and cannot be diffed here: past the
/// diff module's own byte cap, or gone from the checkout. Both are entries;
/// neither is a range.
pub(super) fn moved_held_files(
    repo: &std::path::Path,
    held: &[HeldRevision],
    known: &[String],
) -> Vec<(MovedFile, Option<Vec<u8>>)> {
    if held.is_empty() {
        return Vec::new();
    }
    // A policy that cannot be read turns this channel off rather than open: the
    // conservative direction everywhere, and here it costs a saving and never a
    // path that should not have been named.
    let Ok(cfg) = rift_config::EffectiveConfig::load(&repo.join(".rift")) else {
        return Vec::new();
    };
    // The serve overlay too, not the config alone: "the same filter every
    // serve applies" is only true of an engine that carries the operator's own
    // denied globs, and a manifest entry is a served fact about a file.
    let engine = rift_recall_policy::PolicyEngine::new(&cfg)
        .with_serve_policy(rift_recall_policy::ServePolicy::load(&repo.join(".rift")));
    let mut out = Vec::new();
    for file in held.iter().take(MAX_MANIFEST_HELD) {
        if engine.is_denied_path(&file.path) {
            // The same filter every serve applies, asked before anything is
            // read: a path this repository does not serve has no bytes taken
            // from it here either. It is not counted, because saying "one more
            // not shown" would claim a delta nobody looked for.
            continue;
        }
        let Some((blob, bytes)) =
            rift_recall_substrate::worktree::hash_blob_bytes(repo, &file.path)
        else {
            if repo.join(&file.path).exists() {
                // Unreadable is not "removed" and not "changed": nothing is
                // known, so nothing is said.
                continue;
            }
            out.push((
                MovedFile {
                    path: file.path.clone(),
                    hash: file.hash.clone(),
                    current: String::new(),
                },
                None,
            ));
            continue;
        };
        if blob.content_sha256 == file.hash {
            // The bytes the session read are the bytes in the checkout. This
            // is the silence property at the file level: nothing changed, so
            // nothing is announced.
            continue;
        }
        if known.contains(&blob.content_sha256) {
            // This session has already been told about these bytes. Announcing
            // them again spends its context to repeat itself, which is the same
            // rule the baseline's own entries follow.
            continue;
        }
        let diffable = bytes.len() <= rift_recall_serve::held_diff::MAX_DIFF_BYTES;
        out.push((
            MovedFile {
                path: file.path.clone(),
                hash: file.hash.clone(),
                current: blob.content_sha256,
            },
            diffable.then_some(bytes),
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hooks::manifest::fixtures::fixture_repo_with_recall_on;

    /// The diff's cap is the manifest's cap, applied where the reading
    /// happens: past sixteen files the held set is not hashed, not diffed and
    /// not named, whatever the budget would have carried.
    #[test]
    fn at_most_sixteen_held_files_are_looked_at() {
        assert_eq!(
            MAX_MANIFEST_HELD,
            rift_recall_serve::held_diff::MAX_DIFFED_FILES,
            "the cap is the diff module's own, not a second number"
        );
        let tmp = tempfile::tempdir().unwrap();
        let repo = fixture_repo_with_recall_on();
        let _ = tmp;
        let held: Vec<HeldRevision> = (0..MAX_MANIFEST_HELD + 8)
            .map(|i| {
                let rel = format!("held{i:02}.txt");
                std::fs::write(repo.path().join(&rel), format!("was {i}\n")).unwrap();
                HeldRevision {
                    path: rel,
                    // A revision nothing in the checkout has: every one of
                    // these files has moved, so only the cap can bound them.
                    hash: format!("{i:064x}"),
                    at_ms: 1000 - i as u64,
                }
            })
            .collect();
        let moved = moved_held_files(repo.path(), &held, &[]);
        assert_eq!(
            moved.len(),
            MAX_MANIFEST_HELD,
            "the warm end of the held set, and no more"
        );
        assert_eq!(moved[0].0.path, "held00.txt", "ranked by recency");
        assert!(
            !moved.iter().any(|(f, _)| f.path == "held16.txt"),
            "a file past the cap is not looked at at all"
        );
    }

    /// The three things a held file can be that are not a range, each with the
    /// entry it renders: gone from the checkout, still the bytes the session
    /// read, and already announced.
    #[test]
    fn a_held_file_that_cannot_be_diffed_is_still_an_honest_entry() {
        let repo = fixture_repo_with_recall_on();
        let held = |rel: &str, hash: &str| HeldRevision {
            path: rel.to_owned(),
            hash: hash.to_owned(),
            at_ms: 10,
        };
        std::fs::write(repo.path().join("same.txt"), "one\n").unwrap();
        std::fs::write(repo.path().join("told.txt"), "two\n").unwrap();
        let same = rift_shadow::sha256_hex(&[b"one\n".as_slice()]);
        let told = rift_shadow::sha256_hex(&[b"two\n".as_slice()]);
        let gone = format!("{:064x}", 7);
        let moved = moved_held_files(
            repo.path(),
            &[
                held("same.txt", &same),
                held("told.txt", &format!("{:064x}", 8)),
                held("gone.txt", &gone),
            ],
            &[told],
        );
        let paths: Vec<&str> = moved.iter().map(|(f, _)| f.path.as_str()).collect();
        assert_eq!(
            paths,
            vec!["gone.txt"],
            "unchanged bytes and an already-announced revision are both silence"
        );
        assert!(
            moved[0].0.current.is_empty() && moved[0].1.is_none(),
            "a file the checkout no longer has has no current revision to name"
        );
    }
}
