//! The per-shard walk: what every shard of every dialect owes, and the tally a
//! run reports.
//!
//! Separate because this is the driver — it decides which pass applies to
//! which shard and folds the findings — and the passes themselves live beside
//! it, one module per kind of binding.

use rift_core::Result;
use rift_recall_substrate::{Dialect, SubstrateReader};

use super::artifacts::{shard_header, NativeArtifacts};
use super::identity::{checkpoint_id_pass, task_subtree_pass};
use super::merkle::{
    recorded_leaf_pass, strict_merkle_pass, vacuous_root_pass, VACUOUS_MERKLE_CANON_VERSION,
};
use super::verdict::Verdict;

/// What the per-shard passes found.
pub(super) struct ShardPasses {
    /// Shards that got the redaction/Merkle passes.
    pub(super) checked: usize,
    /// Shards in the **vacuous era** — native-dialect shards below the strict
    /// root era: epoch 1 (whose recorded root is the empty-set constant every
    /// native shard published before 2026-08-12, asserted by
    /// [`vacuous_root_pass`]) and a shard declaring no era at all (a deleted
    /// `canon_version` parses as epoch 0 — its root proves nothing, and its id
    /// still must address its bytes). Accepted, and counted so the acceptance
    /// is visible instead of assumed.
    pub(super) vacuous_era: usize,
    /// Shards of the **pre-Rift dialect** — read from the legacy checkpoint
    /// ref, exempt from every native-only pass by invariant §1.5.
    ///
    /// Counted by DIALECT (the ref the shard was read from), never by anything
    /// the shard says about itself. A native shard whose `canon_version` was
    /// deleted used to land in this exemption; it now stays in
    /// the native passes at epoch 0, where its id must still address its own
    /// bytes. A non-zero count here therefore means real legacy history —
    /// never a stripped label.
    pub(super) pre_rift: usize,
    /// Replaced ranges of held substitutions whose revision this run did not
    /// read: counted, so a rewrite verify could not fully check is visible
    /// rather than passed.
    pub(super) held_unchecked: usize,
    /// The worst finding.
    pub(super) worst: Verdict,
}

impl ShardPasses {
    /// No shard was walked at all — the fixture path, and a repository whose
    /// object database will not open. Deliberately not `Ok` dressed as a
    /// result: `checked` is zero, and the summary reports that rather than
    /// letting an unwalked run read as a clean one.
    pub(super) fn nothing_walked() -> Self {
        Self {
            checked: 0,
            vacuous_era: 0,
            pre_rift: 0,
            held_unchecked: 0,
            worst: Verdict::Ok,
        }
    }
}

/// Per-shard passes: re-verify each shard's
/// redaction report, recompute its Merkle root from the recorded leaves (the
/// legacy dialect's `merkle_leaf.txt`), and — for shards of the strict era —
/// re-derive the root from the shard's own artifacts and demand a match.
///
/// **The era gates only the last of those.** The first two run on every shard of
/// every era, because an era split that silenced a check that already worked
/// would be a hole wearing a migration's name (a corrupt `merkle_leaf.txt` in a
/// shard with no `canon_version` at all must keep dying).
pub(super) fn shard_passes(reader: &impl SubstrateReader) -> Result<ShardPasses> {
    let mut found = ShardPasses {
        checked: 0,
        vacuous_era: 0,
        pre_rift: 0,
        held_unchecked: 0,
        worst: Verdict::Ok,
    };
    for shard in &reader.checkpoints()? {
        let id = shard.checkpoint_id.as_str();
        found.checked += 1;

        let gap = reader.read_artifact(id, rift_shadow::backend::BOUNDARY_GAPS_NAME)?;
        if let Some(gap) = gap {
            super::boundary_gaps::present_gap_pass(reader, shard, &gap, &mut found)?;
            continue;
        }

        // Redaction re-verification: the report must exist and be valid.
        let report_bytes = reader.read_artifact(id, "redaction_report.json")?;
        found
            .worst
            .keep_worse(redaction_report_pass(id, report_bytes.as_deref()));

        for v in recorded_leaf_pass(reader, id)? {
            found.worst.keep_worse(v);
        }

        // Everything below is a NATIVE-only pass, gated on the DIALECT the
        // shard was read from — the one signal the shard's own bytes cannot
        // rewrite, because it is where the shard IS, not what it says. The
        // pre-Rift dialect is exempt (invariant §1.5: those ids were never
        // minted by any Rift derivation, and read-compat is forever) and
        // counted. This used to gate on the header's `canon_version`, and that
        // was the recorded escape: deleting the field moved a
        // NATIVE shard into the exemption and every native pass declined to
        // look at it. Now a native-ref shard with no era declares epoch 0 and
        // still owes its id — which no era's rule will grant to edited bytes.
        if shard.dialect != Dialect::Rift {
            found.pre_rift += 1;
            continue;
        }
        native_shard_passes(reader, id, report_bytes.as_deref(), None, &mut found)?;
        for v in super::segments::segments_pass(&|name| reader.read_artifact(id, name), id)? {
            found.worst.keep_worse(v);
        }
    }
    Ok(found)
}

/// The redaction pass, which every shard of every dialect owes: the report
/// exists, parses, and says the redaction actually held.
///
/// A malformed report is a STRUCTURE finding and an invalid one a REDACTION
/// finding, because the first says the artifact cannot be read and the second
/// says it was read and disagrees.
pub(super) fn redaction_report_pass(id: &str, report: Option<&[u8]>) -> Verdict {
    use rift_recall_substrate::parse;

    let Some(bytes) = report else {
        return Verdict::Redaction(format!("{id}: missing redaction report"));
    };
    let out = parse::parse_redaction_report(bytes, id);
    if !out.is_clean() {
        return Verdict::Structure(format!("{id}: redaction report is malformed"));
    }
    if !out.value.is_valid() {
        return Verdict::Redaction(format!("{id}: redaction report is invalid"));
    }
    Verdict::Ok
}

/// The passes only a native-dialect shard owes: its task subtrees, the
/// era-gated root claim, and — on every era, including none at all — the id
/// that must address its own bytes.
///
/// Takes the tally rather than returning one because the vacuous-era count is
/// part of the finding: an accepted era is counted where it is accepted, not
/// re-derived by a caller reading the same header a second time.
pub(super) fn native_shard_passes(
    reader: &impl SubstrateReader,
    id: &str,
    report: Option<&[u8]>,
    gap: Option<Vec<u8>>,
    found: &mut ShardPasses,
) -> Result<()> {
    use rift_shadow::backend::STRICT_MERKLE_MIN_CANON_VERSION;

    let header = shard_header(reader, id)?;
    let native = NativeArtifacts::read_with_gap(reader, id, &header, gap)?;
    if let Some(verdict) = super::boundary_gaps::gap_pass(id, &header, &native, report) {
        found.worst.keep_worse(verdict);
    }

    let spills = super::spills::spill_digest_pass(id, &native.spills);
    let originals = super::originals::original_passes(id, &native);
    let fallback = super::fallback::fallback_pass(
        id,
        super::fallback::header_counts(&header),
        &native,
        report,
    );
    found.held_unchecked += originals.held_unchecked;
    for v in task_subtree_pass(id, &header, &native)
        .into_iter()
        .chain(spills)
        .chain(originals.verdicts)
        .chain(fallback)
    {
        found.worst.keep_worse(v);
    }

    if header.canon_version >= STRICT_MERKLE_MIN_CANON_VERSION {
        if let Some(v) = strict_merkle_pass(reader, id, &native, report)? {
            found.worst.keep_worse(v);
        }
    } else {
        found.vacuous_era += 1;
        if header.canon_version == VACUOUS_MERKLE_CANON_VERSION {
            if let Some(v) = vacuous_root_pass(reader, id)? {
                found.worst.keep_worse(v);
            }
        }
    }

    // The checkpoint-id pass runs on every native shard, whatever its
    // cardinality. The single-session carve-out (skipped + counted) is
    // RETIRED: the derivation is defined for every
    // session set a writer can mint (N ≥ 1, NUL-free), and a set no rule
    // grants — none at all, or a multi-session shard declaring a
    // single-session era — yields an empty candidate set, which the pass
    // maps to tamper rather than to silence.
    if let Some(v) = checkpoint_id_pass(id, &header.sessions, header.canon_version, &native, report)
    {
        found.worst.keep_worse(v);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rift_core::Sha256Id;
    use rift_recall_substrate::MemorySubstrate;

    use crate::verify::source::GitCheckpointSource;
    use crate::verify::tests::{
        era_shard, fixture_report, hex64, honest_legacy, legacy_shard, native_id, shard,
        writer_root, FIXTURE_SESSION, LEGACY_SHORT_HEX_ID, LEGACY_ULID_ID, VACUOUS_ERA_ROOT,
    };
    use rift_store::ChainSource;

    /// **Dropping a shard from the chain removes a crash, not a check.** A
    /// NATIVE shard keyed by an id no derivation could mint is dropped from the
    /// chain by the same parseability rule — and the per-shard walk still
    /// convicts it, because the id pass asks whether the id addresses these
    /// bytes and nothing grants an id that is not even a digest.
    #[test]
    fn a_native_shard_with_an_unaddressable_id_is_tamper_not_a_silent_skip() {
        let content = b"the redacted capture".as_slice();
        let sub = era_shard(
            MemorySubstrate::local(),
            "deadbeef",
            2,
            &writer_root(content),
            content,
        );
        assert!(
            GitCheckpointSource::from_reader(&sub)
                .unwrap()
                .links()
                .unwrap()
                .is_empty(),
            "it cannot hold a position"
        );
        match shard_passes(&sub).unwrap().worst {
            Verdict::Tamper(d) => assert!(d.contains("does not address"), "{d}"),
            other => panic!("a native shard with a forged id must be TAMPER, got {other:?}"),
        }
    }

    /// **The mixed store: verify walks BOTH eras in one repository.** One
    /// native shard that addresses its own bytes and two pre-Rift shards whose
    /// ids no Rift rule ever minted. The chain retains exactly the addressable
    /// one; the per-shard walk sees all three and counts the exemptions, so
    /// nothing is skipped in silence.
    #[test]
    fn verify_walks_a_store_holding_both_id_eras() {
        let content = b"the redacted capture".as_slice();
        let native = native_id(content);
        let sub = era_shard(
            MemorySubstrate::local(),
            &native,
            2,
            &writer_root(content),
            content,
        );
        let sub = honest_legacy(sub, LEGACY_ULID_ID, 2);
        let sub = honest_legacy(sub, LEGACY_SHORT_HEX_ID, 3);

        let links = GitCheckpointSource::from_reader(&sub)
            .unwrap()
            .links()
            .unwrap();
        assert_eq!(links.len(), 1, "only the addressable shard is a position");
        assert_eq!(
            links[0].checkpoint_id,
            Sha256Id::parse_hex(&native).unwrap()
        );

        let out = shard_passes(&sub).unwrap();
        assert!(matches!(out.worst, Verdict::Ok), "got {:?}", out.worst);
        assert_eq!(out.checked, 3, "every shard is walked, both eras");
        assert_eq!(
            out.pre_rift, 2,
            "…and the exemptions are counted, not hidden"
        );
    }

    /// **A corrupt NATIVE shard is REFUSED, by verdict and not by crash**
    /// — the verify half of the corrupt-shard case; its import half is the
    /// corrupt-shard import fixture.
    ///
    /// Damage in three places at once, because a checker that survives one kind
    /// of malformed byte need not survive three: a `metadata.json` truncated
    /// mid-object, a `full.jsonl` whose middle record is truncated, and a
    /// `content_hash.txt` that is not hex. Every parse on this path is lenient
    /// by design, so nothing here raises — and that is the trap: a lenient
    /// parser reads the truncated header as an EMPTY header, which declares no
    /// era, which is epoch 0, whose id rules still refuse an id no derivation
    /// minted. The shard is tamper, the run completes, and reaching the
    /// assertions at all is the "never panics" half of the claim.
    #[test]
    fn a_corrupt_native_shard_is_refused_by_verdict_not_by_crash() {
        let content = b"the redacted capture".as_slice();
        let id = native_id(content);
        let sub = MemorySubstrate::local()
            .with_shard(shard(&id, None, 1))
            .with_artifact(&id, "redaction_report.json", fixture_report().as_bytes())
            .with_artifact(&id, "content.redacted", content)
            // Truncated mid-object — the lenient parser reads an empty header.
            .with_artifact(
                &id,
                "metadata.json",
                br#"{"canon_version":3,"strategy":"manual-commit","merkle_r"#,
            )
            // A truncated record between two intact ones.
            .with_artifact(
                &id,
                "full.jsonl",
                b"{\"session\":\"s\",\"turn\":0}\n{\"session\":\"s\",\"tu\n{\"session\":\"s\",\"turn\":2}\n",
            )
            // Not a hex digest at all.
            .with_artifact(&id, "content_hash.txt", b"not-a-digest\n");

        match shard_passes(&sub).unwrap().worst {
            Verdict::Tamper(d) => assert!(d.contains(&id), "the finding names the shard: {d}"),
            other => panic!("a corrupt native shard must be refused, got {other:?}"),
        }
        // …and the chain derivation survives it too, rather than aborting the
        // whole run over one damaged shard.
        let links = GitCheckpointSource::from_reader(&sub)
            .expect("a corrupt shard must not destroy the chain derivation")
            .links()
            .unwrap();
        assert_eq!(links.len(), 1, "its id still parses, so it keeps its place");
    }

    /// **History stays green.** A shard from the vacuous era carries the
    /// empty-set constant; re-deriving a root for it and demanding a match would
    /// redden every repository's whole history for a defect the history could
    /// not have avoided. It is accepted — and COUNTED, so the acceptance is
    /// visible in the summary rather than assumed.
    #[test]
    fn a_vacuous_era_shard_is_accepted_and_counted() {
        let content = b"whatever the capture was".as_slice();
        let id = native_id(content);
        let sub = era_shard(MemorySubstrate::local(), &id, 1, VACUOUS_ERA_ROOT, content);
        let out = shard_passes(&sub).unwrap();
        assert!(matches!(out.worst, Verdict::Ok), "got {:?}", out.worst);
        assert_eq!(out.checked, 1);
        assert_eq!(out.vacuous_era, 1, "the era is counted, not hidden");
        assert_eq!(out.pre_rift, 0, "it declares an era, so it is not pre-Rift");

        // And this is what the era costs, RE-STATED after the id pass landed.
        // It used to cost everything: editing the capture of a vacuous-era shard
        // changed nothing, because its root never committed to it. The root
        // still does not — but the shard's own path does, so the same edit is
        // now TAMPER. The era's remaining cost is narrow and nameable: a
        // vacuous-era root proves nothing about the shard; the id does.
        let changed = era_shard(
            MemorySubstrate::local(),
            &id,
            1,
            VACUOUS_ERA_ROOT,
            b"a different capture fully",
        );
        match shard_passes(&changed).unwrap().worst {
            Verdict::Tamper(d) => assert!(d.contains("does not address"), "{d}"),
            other => panic!("an edited vacuous-era capture must be TAMPER, got {other:?}"),
        }
    }

    /// **Read-compat is forever (invariant §1.5), pinned from the positive
    /// side.** An honest legacy-dialect shard — no `canon_version`, an id its
    /// own tool minted, no native artifacts at all — is exempt from every
    /// native-only pass and counted, never verdicted for what its dialect
    /// never promised.
    #[test]
    fn an_honest_legacy_dialect_shard_stays_exempt_and_counted() {
        let id = hex64('a');
        let report = format!(
            r#"{{"rule_pack_versions":["builtin@1"],"input_sha256":"{}"}}"#,
            "a".repeat(64)
        );
        let sub = MemorySubstrate::local()
            .with_shard(legacy_shard(&id, 1))
            .with_artifact(&id, "redaction_report.json", report.as_bytes())
            .with_artifact(
                &id,
                "metadata.json",
                format!(
                    r#"{{"strategy":"manual-commit","merkle_root":"{}","sessions":["{FIXTURE_SESSION}"]}}"#,
                    hex64('9')
                )
                .as_bytes(),
            );
        let out = shard_passes(&sub).unwrap();
        assert!(
            matches!(out.worst, Verdict::Ok),
            "an honest legacy shard must stay green: {:?}",
            out.worst
        );
        assert_eq!(out.pre_rift, 1, "…and the exemption is counted, not silent");
    }
}
