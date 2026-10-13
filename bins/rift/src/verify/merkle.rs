//! The Merkle-root passes: what a shard's recorded root is allowed to be under
//! each canonicalization era, and the legacy recompute over a recorded leaf
//! list.
//!
//! Separate because these are the passes an era gates, and gathering them here
//! keeps the era rules readable as a set rather than as branches scattered
//! through a loop.

use rift_core::{Result, Sha256Id};
use rift_hashchain::merkle_root;
use rift_recall_substrate::SubstrateReader;

use super::artifacts::NativeArtifacts;
use super::source::recorded_root;
use super::verdict::Verdict;

/// The one `canon_version` whose `merkle_root` is a constant rather than a
/// claim — see `STRICT_MERKLE_MIN_CANON_VERSION`.
pub(super) const VACUOUS_MERKLE_CANON_VERSION: u32 = 1;

/// The strict Merkle pass: for a shard whose era says its recorded root is a
/// claim about these bytes, re-derive the root from the artifacts and demand a
/// match.
///
/// The leaves are re-derived by `shard_merkle_root` — literally the function the
/// archive writer calls, over the artifacts read back out of the shard — so the
/// writer and the verifier cannot drift apart into two answers about one fact.
pub(super) fn strict_merkle_pass(
    reader: &impl SubstrateReader,
    id: &str,
    native: &NativeArtifacts,
    report: Option<&[u8]>,
) -> Result<Option<Verdict>> {
    use rift_shadow::backend::shard_merkle_root;

    let (Some(content), Some(report)) = (native.content.as_deref(), report) else {
        // Both artifacts are written unconditionally by every shard of this
        // era, so one being gone is a removal of content the shard's own
        // root commits to — not an incompleteness.
        return Ok(Some(Verdict::Tamper(format!(
            "{id}: a leaf artifact its recorded Merkle root commits to is missing"
        ))));
    };
    let task_leaves = native.task_leaves();
    let session_meta_leaves = native.session_meta_leaves();
    let spill_leaves = super::spills::leaves(&native.spills);
    let derived = shard_merkle_root(&native.leaves(
        content,
        report,
        &task_leaves,
        &session_meta_leaves,
        &spill_leaves,
    ));
    Ok(match recorded_root(reader, id)? {
        Some(recorded) if recorded == derived => None,
        Some(_) => Some(Verdict::Tamper(format!(
            "{id}: the Merkle root re-derived from this shard's artifacts does not match the root \
             it records"
        ))),
        None => Some(Verdict::Structure(format!(
            "{id}: records no Merkle root, but its canon_version says it must"
        ))),
    })
}

/// The vacuous era's one checkable claim: an epoch-1 shard's recorded root is
/// the empty-leaf-set constant, on every shard, because `condense` handed the
/// archive an empty event set and took the root over it.
///
/// This exists because the summary told operators that vacuous-era shards "are
/// accepted with their empty-set constant" while nothing anywhere asserted it
/// — a verifier caught the gap. A sentence about behaviour that no test would
/// redden is an argument, not a claim.
///
/// **It does not close the relabelling hole and is not meant to.** A tamperer
/// who rewrites `canon_version` to 1 writes the constant too; what catches them
/// is the checkpoint-id pass below, which runs on this era as well. What this
/// catches is a shard claiming an era whose one invariant it does not satisfy —
/// no Rift build ever wrote one, measured across all 103 shards here.
pub(super) fn vacuous_root_pass(
    reader: &impl SubstrateReader,
    id: &str,
) -> Result<Option<Verdict>> {
    Ok(match recorded_root(reader, id)? {
        Some(root) if root == rift_hashchain::empty_root() => None,
        _ => Some(Verdict::Tamper(format!(
            "{id}: declares the vacuous canonicalization era but does not record that era's \
             empty-leaf-set root"
        ))),
    })
}

/// The **recorded-leaf** recompute: when a shard writes its own leaf list
/// (`merkle_leaf.txt`, the legacy dialect's artifact), the root over those leaves
/// must equal the root the shard records.
///
/// Runs on every shard of every era, which is why it is a named pass rather than
/// a branch inside the loop: a corrupt `merkle_leaf.txt` in a shard with no
/// `canon_version` at all must keep dying.
/// Returns every finding rather than the worst, so a malformed list and a
/// mismatching root are two facts and not one.
pub(super) fn recorded_leaf_pass(reader: &impl SubstrateReader, id: &str) -> Result<Vec<Verdict>> {
    use rift_recall_substrate::parse;

    let Some(bytes) = reader.read_artifact(id, "merkle_leaf.txt")? else {
        return Ok(Vec::new());
    };
    let mut found = Vec::new();
    let leaves = parse::parse_merkle_leaves(&bytes, id);
    if !leaves.is_clean() {
        found.push(Verdict::Structure(format!("{id}: malformed merkle leaves")));
    }
    let parsed: Vec<Sha256Id> = leaves
        .value
        .iter()
        .filter_map(|h| Sha256Id::parse_hex(h).ok())
        .collect();
    if !parsed.is_empty() {
        let recomputed = merkle_root(&parsed);
        // The same derivation the chain comparison uses — one answer to "what
        // root did this shard record", or the recompute would be checking a
        // different claim than the chain does.
        let recorded = recorded_root(reader, id)?.or_else(|| Sha256Id::parse_hex(id).ok());
        if recorded.is_some_and(|recorded| recomputed != recorded) {
            found.push(Verdict::Tamper(format!(
                "{id}: recomputed Merkle root does not match the recorded root"
            )));
        }
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rift_recall_substrate::MemorySubstrate;

    use crate::verify::passes::shard_passes;
    use crate::verify::tests::{
        era_shard, hex64, legacy_shard, native_id, shard, writer_root, FIXTURE_SESSION,
        VACUOUS_ERA_ROOT,
    };

    /// The strict era: the recorded root is a claim about these bytes, and
    /// verify re-derives it with the archive writer's own function.
    #[test]
    fn a_strict_era_shard_verifies_against_its_own_artifacts() {
        let content = b"the redacted capture".as_slice();
        let sub = era_shard(
            MemorySubstrate::local(),
            &native_id(content),
            2,
            &writer_root(content),
            content,
        );
        let out = shard_passes(&sub).unwrap();
        assert!(matches!(out.worst, Verdict::Ok), "got {:?}", out.worst);
        assert_eq!(out.vacuous_era, 0, "nothing was waved through");
        assert_eq!(out.pre_rift, 0);
    }

    /// The other direction, and the one with teeth: a NEW shard whose leaf bytes
    /// were altered after its root was recorded is TAMPER, and the finding names
    /// the checkpoint.
    #[test]
    fn a_tampered_strict_era_shard_is_named_as_tamper() {
        let id = native_id(b"the redacted capture");
        let recorded = writer_root(b"the redacted capture");
        let sub = era_shard(
            MemorySubstrate::local(),
            &id,
            2,
            &recorded,
            b"the redacted capture, edited",
        );
        match shard_passes(&sub).unwrap().worst {
            Verdict::Tamper(d) => {
                assert!(d.contains(&id), "the finding names the shard: {d}");
                assert!(d.contains("re-derived"), "and says what disagreed: {d}");
            }
            other => panic!("a tampered strict-era shard must be TAMPER, got {other:?}"),
        }
    }

    /// Deleting a leaf artifact is not an incompleteness: the shard's own era
    /// says its root commits to those bytes, so their absence is a removal.
    #[test]
    fn a_strict_era_shard_missing_a_leaf_artifact_is_tamper() {
        let id = hex64('d');
        let content = b"the redacted capture".as_slice();
        let report = format!(
            r#"{{"rule_pack_versions":["builtin@1"],"input_sha256":"{}"}}"#,
            "a".repeat(64)
        );
        // Everything a strict-era shard has, except `content.redacted`.
        let sub = MemorySubstrate::local()
            .with_shard(shard(&id, None, 1))
            .with_artifact(&id, "redaction_report.json", report.as_bytes())
            .with_artifact(
                &id,
                "metadata.json",
                format!(
                    r#"{{"canon_version":2,"strategy":"manual-commit","merkle_root":"{}","sessions":["{FIXTURE_SESSION}"]}}"#,
                    writer_root(content)
                )
                .as_bytes(),
            );
        assert!(
            matches!(shard_passes(&sub).unwrap().worst, Verdict::Tamper(_)),
            "a missing leaf artifact in the strict era is a removal"
        );
    }

    /// **The dialect exemption must not silence a check that already worked.**
    /// The recorded-leaf recompute predates every native pass and runs on every
    /// shard of every dialect: a LEGACY shard whose `merkle_leaf.txt` does not
    /// produce its recorded root has to keep dying even though the native
    /// passes decline it. If the exemption ever gates the *existing* recompute
    /// rather than only the native-only passes, this reds.
    #[test]
    fn the_legacy_merkle_leaf_recompute_still_fires_on_the_legacy_dialect() {
        let id = hex64('e');
        let report = format!(
            r#"{{"rule_pack_versions":["builtin@1"],"input_sha256":"{}"}}"#,
            "a".repeat(64)
        );
        let leaf = hex64('7');
        let sub = MemorySubstrate::local()
            .with_shard(legacy_shard(&id, 1))
            .with_artifact(&id, "redaction_report.json", report.as_bytes())
            // The legacy dialect: no canon_version, its own recorded root…
            .with_artifact(
                &id,
                "metadata.json",
                format!(
                    r#"{{"strategy":"manual-commit","merkle_root":"{}"}}"#,
                    hex64('9')
                )
                .as_bytes(),
            )
            // …and a leaf list that does not produce that root.
            .with_artifact(&id, "merkle_leaf.txt", leaf.as_bytes());
        let out = shard_passes(&sub).unwrap();
        assert!(
            matches!(out.worst, Verdict::Tamper(_)),
            "the pre-existing merkle_leaf pass must run on every dialect, got {:?}",
            out.worst
        );
        assert_eq!(out.pre_rift, 1, "the dialect exemption is counted");
        assert_eq!(out.vacuous_era, 0, "…and it is not the vacuous era's");
    }

    /// **The sentence the summary makes is now a check.**
    ///
    /// `rift verify` told operators that vacuous-era shards "are accepted with
    /// their empty-set constant" while nothing asserted any such thing. A shard
    /// that declares epoch 1 and records some other root did not come from any
    /// Rift build: `condense` took the root over an empty event set on every
    /// archive of that era, measured across all 103 shards in this repository.
    #[test]
    fn a_vacuous_era_shard_must_actually_carry_the_empty_set_constant() {
        let content = b"whatever the capture was".as_slice();
        let id = native_id(content);
        let sub = era_shard(
            MemorySubstrate::local(),
            &id,
            VACUOUS_MERKLE_CANON_VERSION,
            &writer_root(content), // a real root — but not that era's
            content,
        );
        match shard_passes(&sub).unwrap().worst {
            Verdict::Tamper(d) => assert!(d.contains("empty-leaf-set"), "{d}"),
            other => panic!("an epoch-1 shard with a derived root must be TAMPER, got {other:?}"),
        }
        // The constant itself is the archive's, not a literal this test chose.
        assert_eq!(VACUOUS_ERA_ROOT, rift_hashchain::empty_root().to_hex());
    }
}
