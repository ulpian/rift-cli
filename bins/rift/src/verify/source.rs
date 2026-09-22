//! Where a chain comes from: the committed fixture skeleton and the
//! git-authoritative walk of `rift/checkpoints/v1`.
//!
//! Separate because "what does this repository claim its chain is" is one
//! question, answered by a reader, and everything else in `verify` is a check
//! over the answer.

use std::path::Path;

use rift_core::{Result, Sha256Id};
use rift_hashchain::{leaf_hash, merkle_root, ChainLink};
use rift_recall_substrate::{GixSubstrate, SubstrateReader};
use rift_store::ChainSource;

/// The committed single-checkpoint fixture (genesis event leaves, one per line).
pub(super) const FIXTURE: &str = include_str!("../../fixtures/genesis_checkpoint.txt");

/// The authoritative chain source for the skeleton: one genesis checkpoint whose
/// Merkle root is recomputed over its event leaves.
pub(super) struct FixtureSource {
    leaves: Vec<Vec<u8>>,
}

impl FixtureSource {
    /// Parse a fixture into event-leaf rows (skips blank lines and `#` comments).
    pub(super) fn parse(raw: &str) -> Self {
        let leaves = raw
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .map(|l| l.as_bytes().to_vec())
            .collect();
        Self { leaves }
    }

    /// The single genesis [`ChainLink`]: content-addressed (`checkpoint_id` is the
    /// Merkle root) with no predecessor.
    fn genesis_link(&self) -> ChainLink {
        let root = merkle_root(&self.leaves.iter().map(|l| leaf_hash(l)).collect::<Vec<_>>());
        ChainLink {
            checkpoint_id: root,
            merkle_root: root,
            prev: None,
        }
    }
}

impl ChainSource for FixtureSource {
    fn links(&self) -> Result<Vec<ChainLink>> {
        Ok(vec![self.genesis_link()])
    }
}

/// The per-shard artifact holding a checkpoint's Merkle root (lowercase hex). When
/// absent the checkpoint id is treated as its own root (a content-addressed
/// checkpoint). The exact upstream artifact name is `(verify)` at release-cut.
const MERKLE_ROOT_ARTIFACT: &str = "merkle_root.txt";

/// The **git-authoritative** chain source: re-derives the checkpoint chain
/// by walking `rift/checkpoints/v1` through the read-only gix [`SubstrateReader`]
/// — the production replacement for the committed fixture. Each shard yields a
/// [`ChainLink`] whose `prev` comes from the substrate's deterministic walk order,
/// so a re-walk of the same branch is byte-stable.
pub(super) struct GitCheckpointSource {
    links: Vec<ChainLink>,
}

impl GitCheckpointSource {
    /// Build the chain by walking `reader`'s checkpoint shards and reading each
    /// shard's Merkle-root artifact (falling back to the checkpoint id as its own
    /// root). Deterministic: the substrate walks shards in sorted id order.
    ///
    /// **TWO ERAS OF CHECKPOINT ID, and the filter is PARSEABILITY — never the
    /// dialect.** A chain position is a [`Sha256Id`] by type, so an id that is
    /// not one cannot occupy one. This used to `?` on the first such id, which
    /// turned every repository holding a pre-Rift shard into an internal error
    /// (exit 70) — the standing read-compatibility promise is that those
    /// repositories stay *readable*, and aborting is the loudest possible way
    /// to break that promise. Measured before it was written: the two committed legacy
    /// fixtures carry a 12-hex id (`ab34cd56ef78`) and a 26-char ULID
    /// (`01JQZ8T9WQKX3M7VN2P4RB6CDE`), so both short forms are real.
    ///
    /// Filtering on the DIALECT instead would have regressed the other half: a
    /// legacy shard whose id *is* 64 hex characters takes a chain position today
    /// and still must, or an honest repository moves from clean to a false
    /// TAMPER on the missing link. So the rule is about the id's shape and
    /// nothing else.
    ///
    /// **`prev` is re-derived, not copied.** `CheckpointShard::prev` is "the
    /// prior id in walk order", so a shard following a dropped one names a
    /// predecessor that has no chain position — and a second `?` there aborted
    /// on exactly the repositories the first one did. Each retained link's
    /// `prev` is therefore the last *retained* id: the nearest preceding shard
    /// whose id parses. When every id parses that is byte-identical to the old
    /// behaviour (the walk sets `prev` to the previous element), so this is a
    /// generalization rather than a change.
    ///
    /// **A dropped shard is not a silent one.** `shard_passes` walks the same
    /// list independently: a dropped LEGACY shard is counted in
    /// `VerifySummary::pre_rift_shards` and reported, and a NATIVE shard with an
    /// unaddressable id still fails `checkpoint_id_pass` — no era's derivation
    /// grants an id no derivation could have produced — so dropping it from the
    /// chain removes a crash, not a check.
    ///
    /// # Errors
    /// Substrate read errors propagate. A malformed id is a skip, not an error.
    pub(super) fn from_reader(reader: &impl SubstrateReader) -> Result<Self> {
        let mut links = Vec::new();
        let mut last_retained: Option<Sha256Id> = None;
        for shard in reader.checkpoints()? {
            let Ok(checkpoint_id) = Sha256Id::parse_hex(&shard.checkpoint_id) else {
                continue;
            };
            let merkle_root = recorded_root(reader, &shard.checkpoint_id)?.unwrap_or(checkpoint_id);
            links.push(ChainLink {
                checkpoint_id,
                merkle_root,
                prev: last_retained,
            });
            last_retained = Some(checkpoint_id);
        }
        Ok(Self { links })
    }
}

impl ChainSource for GitCheckpointSource {
    fn links(&self) -> Result<Vec<ChainLink>> {
        Ok(self.links.clone())
    }
}

/// **The one derivation of a shard's recorded Merkle root**, in the order the
/// writers actually use it.
///
/// `metadata.json`'s `merkle_root` first, because that is the field the native
/// archive writes and the ONLY field the artifact bridge will record a chain row
/// from. Then `merkle_root.txt`, the legacy dialect's separate artifact. `None`
/// means neither exists and the caller falls back to the id (a content-addressed
/// checkpoint is its own root).
///
/// **Measured on this repository before it was written** — the same lesson, one
/// field over: all 102 native shards carry `merkle_root` in `metadata.json` and
/// none carries a `merkle_root.txt`. Reading only the artifact therefore fell
/// through to the id on every single row, while the bridge recorded the metadata
/// value — two writers, one fact, disagreeing everywhere. That is invisible
/// while `verify` seeds the projection it compares against, and it is a red
/// verify on every repository the moment it stops.
pub(super) fn recorded_root(reader: &impl SubstrateReader, id: &str) -> Result<Option<Sha256Id>> {
    use rift_recall_substrate::parse;

    if let Some(bytes) = reader.read_artifact(id, "metadata.json")? {
        if let Some(hex) = parse::parse_metadata(&bytes, id).value.merkle_root {
            if let Ok(root) = Sha256Id::parse_hex(hex.trim()) {
                return Ok(Some(root));
            }
        }
    }
    Ok(reader
        .read_artifact(id, MERKLE_ROOT_ARTIFACT)?
        .and_then(|bytes| parse_root_hex(&bytes)))
}

/// Parse a trimmed 64-char SHA-256 hex artifact into a [`Sha256Id`]; `None` if the
/// bytes are not exactly one valid hex digest (tolerates a trailing newline).
fn parse_root_hex(bytes: &[u8]) -> Option<Sha256Id> {
    let text = std::str::from_utf8(bytes).ok()?;
    Sha256Id::parse_hex(text.trim()).ok()
}

/// The git-authoritative source for `repo_root`, or `None` when there is no
/// checkpoint branch yet (a fresh repo / pre-capture), so the caller falls back to
/// the M0 fixture skeleton. Opening a non-repository path is also `None`.
pub(super) fn git_source(repo_root: &Path) -> Result<Option<GitCheckpointSource>> {
    let Ok(reader) = GixSubstrate::open(repo_root) else {
        return Ok(None); // not a readable git repo → fixture skeleton
    };
    if reader.checkpoints()?.is_empty() {
        return Ok(None); // no checkpoint branch yet
    }
    Ok(Some(GitCheckpointSource::from_reader(&reader)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rift_recall_substrate::MemorySubstrate;

    use crate::verify::tests::{
        hex64, honest_legacy, legacy_shard_after, shard, LEGACY_SHORT_HEX_ID, LEGACY_ULID_ID,
    };

    #[test]
    fn fixture_parses_to_nonempty_leaves() {
        let src = FixtureSource::parse(FIXTURE);
        assert!(!src.leaves.is_empty(), "committed fixture must have leaves");
        // The genesis link is self-consistent (id == root, no predecessor).
        let link = src.genesis_link();
        assert_eq!(link.checkpoint_id, link.merkle_root);
        assert!(link.prev.is_none());
    }

    #[test]
    fn git_source_rederives_links_with_artifact_and_fallback_roots() {
        let id_a = hex64('a');
        let id_b = hex64('b');
        let root_a = hex64('c'); // a has an explicit merkle-root artifact
        let sub = MemorySubstrate::local()
            .with_shard(shard(&id_a, None, 1))
            .with_shard(shard(&id_b, Some(&id_a), 2))
            .with_artifact(
                &id_a,
                MERKLE_ROOT_ARTIFACT,
                format!("{root_a}\n").as_bytes(),
            );

        let source = GitCheckpointSource::from_reader(&sub).unwrap();
        let links = source.links().unwrap();
        assert_eq!(links.len(), 2);
        // a: root from the artifact; prev None (genesis)
        assert_eq!(links[0].checkpoint_id, Sha256Id::parse_hex(&id_a).unwrap());
        assert_eq!(links[0].merkle_root, Sha256Id::parse_hex(&root_a).unwrap());
        assert!(links[0].prev.is_none());
        // b: no artifact → self-root; prev links to a (continuity)
        assert_eq!(links[1].merkle_root, Sha256Id::parse_hex(&id_b).unwrap());
        assert_eq!(links[1].prev, Some(Sha256Id::parse_hex(&id_a).unwrap()));
        // the re-derived chain is internally continuous
        rift_hashchain::verify_chain(&links).unwrap();
    }

    /// **The abort, closed.** `Sha256Id::parse_hex(&shard.checkpoint_id)?` made
    /// the mere presence of a pre-Rift shard an internal error — exit 70, on a
    /// repository whose readability is invariant §1.5. Both frozen short-id
    /// shapes now build a source instead of destroying one.
    ///
    /// What they do NOT do is take a chain position: a position is a
    /// [`Sha256Id`] by type, and an id that is not one cannot be made into one
    /// without inventing a value the writer never minted.
    #[test]
    fn the_chain_source_accepts_both_pre_rift_id_eras_instead_of_aborting() {
        for id in [LEGACY_SHORT_HEX_ID, LEGACY_ULID_ID] {
            let sub = honest_legacy(MemorySubstrate::local(), id, 1);
            let source = GitCheckpointSource::from_reader(&sub)
                .unwrap_or_else(|e| panic!("{id}: verify aborted on a pre-Rift id: {e:?}"));
            assert!(
                source.links().unwrap().is_empty(),
                "{id}: an id that is not a sha256 must not be given a chain position"
            );
        }
    }

    /// **The regression a DIALECT filter would have caused, pinned.** A
    /// pre-Rift shard whose id happens to be 64 hex characters holds a chain
    /// position today; filtering the legacy dialect out of the source would
    /// have dropped it and turned an honest repository's clean verify into a
    /// missing-link TAMPER. The filter is the id's shape, and only that.
    #[test]
    fn a_64_hex_pre_rift_id_still_takes_a_chain_position() {
        let id = hex64('d');
        let sub = honest_legacy(MemorySubstrate::local(), &id, 1);
        let links = GitCheckpointSource::from_reader(&sub)
            .unwrap()
            .links()
            .unwrap();
        assert_eq!(links.len(), 1, "a parseable id keeps its position");
        assert_eq!(links[0].checkpoint_id, Sha256Id::parse_hex(&id).unwrap());
    }

    /// **`prev` is re-derived, and this is the case that forces it.** The walk
    /// hands each shard the previous element's id, so the second native shard
    /// names the dropped legacy one as its predecessor. Copying that through
    /// was the *second* `?` — the one that kept aborting after the first was
    /// fixed. Each retained link points at the last retained id instead, so the
    /// chain the checker re-derives stays continuous across the gap.
    #[test]
    fn a_dropped_pre_rift_shard_does_not_break_chain_continuity() {
        let a = hex64('a');
        let b = hex64('b');
        let sub = MemorySubstrate::local()
            .with_shard(shard(&a, None, 1))
            .with_shard(legacy_shard_after(LEGACY_ULID_ID, Some(&a), 2))
            .with_shard(shard(&b, Some(LEGACY_ULID_ID), 3));
        let links = GitCheckpointSource::from_reader(&sub)
            .unwrap()
            .links()
            .unwrap();
        assert_eq!(links.len(), 2, "the two addressable shards");
        assert!(links[0].prev.is_none());
        assert_eq!(
            links[1].prev,
            Some(Sha256Id::parse_hex(&a).unwrap()),
            "prev is the last RETAINED id, not the walk's dropped neighbour"
        );
        rift_hashchain::verify_chain(&links).unwrap();
    }

    /// The root a shard RECORDS is the one in its own metadata, not the id.
    ///
    /// Measured on this repository: 102 native shards carry `merkle_root` in
    /// `metadata.json` and none carries `merkle_root.txt`, so the artifact-only
    /// derivation fell through to the id on every row while the bridge recorded
    /// the metadata value.
    #[test]
    fn the_recorded_root_comes_from_the_shard_metadata_the_bridge_reads() {
        let id = hex64('a');
        let root = hex64('c');
        let sub = MemorySubstrate::local()
            .with_shard(shard(&id, None, 1))
            .with_artifact(
                &id,
                "metadata.json",
                format!(r#"{{"merkle_root":"{root}"}}"#).as_bytes(),
            );
        let links = GitCheckpointSource::from_reader(&sub)
            .unwrap()
            .links()
            .unwrap();
        assert_eq!(links[0].merkle_root, Sha256Id::parse_hex(&root).unwrap());
        assert_ne!(
            links[0].merkle_root, links[0].checkpoint_id,
            "the id fallback would have hidden the disagreement"
        );

        // With neither metadata root nor artifact, the id is its own root.
        let bare = MemorySubstrate::local().with_shard(shard(&id, None, 1));
        let bare_links = GitCheckpointSource::from_reader(&bare)
            .unwrap()
            .links()
            .unwrap();
        assert_eq!(bare_links[0].merkle_root, bare_links[0].checkpoint_id);
    }

    /// **REPLACES `malformed_checkpoint_id_is_validity_error`, which asserted
    /// the defect.** That test said `from_reader(&sub).is_err()` on a shard
    /// keyed `"not-hex"` — it could only pass while a malformed id destroyed
    /// the whole chain derivation, which is the exit-70 abort the two-era id
    /// union exists to remove. Its old form is the restore-the-defect calibration, recorded in
    /// the diff: checking out the previous version of this function against
    /// this tree reds it.
    ///
    /// The contract it is replaced by: a malformed id is SKIPPED, the run
    /// survives, and the shard is still walked by the per-shard passes.
    #[test]
    fn a_malformed_checkpoint_id_is_skipped_rather_than_fatal() {
        let sub = MemorySubstrate::local().with_shard(shard("not-hex", None, 1));
        let source = GitCheckpointSource::from_reader(&sub)
            .expect("a malformed id must not destroy the chain derivation");
        assert!(source.links().unwrap().is_empty());
    }
}
