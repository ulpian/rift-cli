use rift_core::SecretBytes;
use rift_shadow_redact::RulePackSet;

use rift_recall_substrate::worktree::git_blob_oid;

use super::{
    seal_session_end_through, seal_turn, seal_turn_through, ChainBreak, PriorChain, PriorPart,
    TurnSeal,
};
use crate::backend::{RedactionFallback, SegmentPart, TranscriptBody};

fn record(n: usize) -> String {
    format!("{{\"type\":\"user\",\"message\":\"turn line {n}\"}}\n")
}

fn doc(lines: &[String]) -> SecretBytes {
    SecretBytes::new(lines.concat().into_bytes())
}

fn records(range: std::ops::Range<usize>) -> Vec<String> {
    range.map(record).collect()
}

fn packs() -> RulePackSet {
    RulePackSet::builtin()
}

/// The chain the previous seal left, read back as the shard tree holds it.
fn chain_of(seal: &TurnSeal) -> PriorChain {
    let TurnSeal::Appended { body, .. } = seal else {
        panic!("expected an appended seal");
    };
    let TranscriptBody::Segments(segments) = body else {
        panic!("an appended seal is segmented");
    };
    PriorChain {
        parts: segments
            .parts()
            .iter()
            .map(|p| PriorPart {
                oid: git_blob_oid(p.as_slice()),
                bytes: p.as_slice().to_vec(),
            })
            .collect(),
        raw_lines: segments.raw_lines(),
        leaf: rift_hashchain::leaf_hash(body.as_slice()),
        packs: segments.packs().map(str::to_owned),
        fallbacks: segments.fallbacks().to_vec(),
    }
}

fn segments_of(seal: &TurnSeal) -> Vec<Vec<u8>> {
    chain_of(seal).parts.into_iter().map(|p| p.bytes).collect()
}

#[test]
fn a_first_turn_seals_up_to_the_last_admitted_cut_and_leaves_the_final_line_open() {
    let lines = records(0..4);
    let seal = seal_turn(&doc(&lines), None, &packs()).unwrap();
    let TurnSeal::Appended { document_lines, .. } = &seal else {
        panic!("a first turn with records appends");
    };
    // The cut needs a line after it, so the last complete record waits.
    assert_eq!(*document_lines, 3);
    assert_eq!(segments_of(&seal), vec![lines[..3].concat().into_bytes()]);
}

#[test]
fn a_later_turn_seals_only_the_new_lines_and_keeps_every_earlier_part_byte_equal() {
    let first = seal_turn(&doc(&records(0..4)), None, &packs()).unwrap();
    let lines = records(0..9);
    let second = seal_turn(&doc(&lines), Some(chain_of(&first)), &packs()).unwrap();
    let parts = segments_of(&second);
    assert_eq!(parts.len(), 2);
    assert_eq!(parts[0], segments_of(&first)[0]);
    assert_eq!(parts[1], lines[3..8].concat().into_bytes());
    let whole = rift_shadow_redact::redact_with(&doc(&lines[..8]), &packs()).unwrap();
    assert_eq!(parts.concat(), whole.redacted.as_slice());
}

#[test]
fn a_secret_in_the_new_lines_is_sealed_exactly_as_the_whole_document_seals_it() {
    let first = seal_turn(&doc(&records(0..3)), None, &packs()).unwrap();
    let mut lines = records(0..3);
    lines.push(
        "{\"type\":\"user\",\"message\":\"token ghp_0123456789abcdefghijABCDEFGHIJ012345\"}\n"
            .into(),
    );
    lines.extend(records(4..6));
    let second = seal_turn(&doc(&lines), Some(chain_of(&first)), &packs()).unwrap();
    let joined = segments_of(&second).concat();
    assert!(!String::from_utf8_lossy(&joined).contains("ghp_0123456789"));
    let whole = rift_shadow_redact::redact_with(&doc(&lines[..5]), &packs()).unwrap();
    assert_eq!(joined, whole.redacted.as_slice());
}

#[test]
fn a_raw_block_at_the_end_holds_the_cut_before_it() {
    let mut lines = records(0..3);
    lines.push("-----BEGIN RSA PRIVATE KEY-----\n".into());
    lines.push("MIIEowIBAAKCAQEA0123456789abcdef\n".into());
    let seal = seal_turn(&doc(&lines), None, &packs()).unwrap();
    let TurnSeal::Appended { document_lines, .. } = seal else {
        panic!("the records before the block are admitted");
    };
    assert_eq!(document_lines, 2);
}

#[test]
fn no_admitted_cut_past_the_frontier_leaves_the_chain_unchanged() {
    let first = seal_turn(&doc(&records(0..3)), None, &packs()).unwrap();
    let again = seal_turn(&doc(&records(0..3)), Some(chain_of(&first)), &packs()).unwrap();
    assert!(matches!(again, TurnSeal::Unchanged));
}

#[test]
fn a_tampered_earlier_part_falls_back_to_a_whole_reseal_naming_the_leaf() {
    let first = seal_turn(&doc(&records(0..4)), None, &packs()).unwrap();
    let mut chain = chain_of(&first);
    chain.parts[0].bytes = records(10..13).concat().into_bytes();
    let lines = records(0..6);
    let seal = seal_turn(&doc(&lines), Some(chain), &packs()).unwrap();
    let TurnSeal::Whole { body, broken, .. } = seal else {
        panic!("a broken chain reseals whole");
    };
    assert_eq!(broken, ChainBreak::LeafMismatch);
    assert!(matches!(body, TranscriptBody::Whole(_)));
    assert_eq!(body.as_slice(), lines.concat().as_bytes());
    assert_eq!(broken.finding(), "segment-chain-broken");
}

#[test]
fn a_declared_line_count_the_part_does_not_hold_breaks_the_chain() {
    let first = seal_turn(&doc(&records(0..4)), None, &packs()).unwrap();
    let mut chain = chain_of(&first);
    chain.raw_lines = vec![2];
    let seal = seal_turn(&doc(&records(0..6)), Some(chain), &packs()).unwrap();
    assert!(matches!(
        seal,
        TurnSeal::Whole {
            broken: ChainBreak::LineCount,
            ..
        }
    ));
}

#[test]
fn a_transcript_shorter_than_the_sealed_frontier_breaks_the_chain() {
    let first = seal_turn(&doc(&records(0..6)), None, &packs()).unwrap();
    let seal = seal_turn(&doc(&records(0..3)), Some(chain_of(&first)), &packs()).unwrap();
    assert!(matches!(
        seal,
        TurnSeal::Whole {
            broken: ChainBreak::Truncated,
            ..
        }
    ));
}

#[test]
fn an_earlier_part_whose_bytes_miss_its_recorded_oid_reseals_whole_with_the_note() {
    let first = seal_turn(&doc(&records(0..4)), None, &packs()).unwrap();
    let mut chain = chain_of(&first);
    // Same line count, a consistent leaf, but not the blob the tree records.
    chain.parts[0].bytes = records(20..23).concat().into_bytes();
    chain.leaf = rift_hashchain::leaf_hash(&chain.parts[0].bytes);
    let lines = records(20..26);
    let seal = seal_turn(&doc(&lines), Some(chain), &packs()).unwrap();
    let TurnSeal::Whole { body, broken, .. } = seal else {
        panic!("a part off its oid reseals whole");
    };
    assert_eq!(broken, ChainBreak::OidMismatch);
    assert_eq!(broken.finding(), "segment-chain-broken");
    let whole = rift_shadow_redact::redact_with(&doc(&lines), &packs()).unwrap();
    assert_eq!(body.as_slice(), whole.redacted.as_slice());
}

#[test]
fn a_later_turn_passes_only_its_new_lines_through_the_gate() {
    let mut calls: Vec<(Vec<u8>, usize)> = Vec::new();
    let mut gate = |raw: &SecretBytes, at: usize| {
        calls.push((raw.as_slice().to_vec(), at));
        rift_shadow_redact::redact_with_at(raw, &packs(), at)
    };
    let first = seal_turn_through(&doc(&records(0..4)), None, &packs(), &mut gate).unwrap();
    let lines = records(0..9);
    let chain = chain_of(&first);
    seal_turn_through(&doc(&lines), Some(chain), &packs(), &mut gate).unwrap();
    // The prefix (lines 0..3) entered the gate once, on the first turn only.
    let first_turn = (lines[..3].concat().into_bytes(), 0);
    let second_turn = (lines[3..8].concat().into_bytes(), 3);
    assert_eq!(calls, vec![first_turn, second_turn]);
}

#[test]
fn a_later_turn_carries_earlier_parts_by_the_previous_shards_oids() {
    let first = seal_turn(&doc(&records(0..4)), None, &packs()).unwrap();
    let chain = chain_of(&first);
    let recorded = chain.parts[0].oid.clone();
    let lines = records(0..9);
    let second = seal_turn(&doc(&lines), Some(chain), &packs()).unwrap();
    let TurnSeal::Appended {
        body: TranscriptBody::Segments(segments),
        ..
    } = &second
    else {
        panic!("a later turn appends");
    };
    let [SegmentPart::Carried(carried), SegmentPart::Sealed(_)] = segments.parts() else {
        panic!("one carried part, then the new sealed one");
    };
    assert_eq!(carried.oid(), recorded);
    let one_shot = rift_shadow_redact::redact_with(&doc(&lines[..8]), &packs()).unwrap();
    let joined = TranscriptBody::Segments(segments.clone());
    assert_eq!(joined.as_slice(), one_shot.redacted.as_slice());
    assert_eq!(
        rift_hashchain::leaf_hash(joined.as_slice()),
        rift_hashchain::leaf_hash(one_shot.redacted.as_slice())
    );
}

#[test]
fn a_segmented_seal_records_the_fingerprint_of_the_packs_it_ran_under() {
    let seal = seal_turn(&doc(&records(0..4)), None, &packs()).unwrap();
    assert_eq!(chain_of(&seal).packs, Some(packs().fingerprint()));
}

#[test]
fn an_earlier_chain_sealed_under_other_packs_reseals_whole_instead_of_carrying() {
    let lines = records(0..6);
    let first = seal_turn(&doc(&lines[..4]), None, &packs()).unwrap();
    let broader = RulePackSet::builtin()
        .with_vendor_pattern("acme", r"acme_[a-z0-9]{8}", "acme@1")
        .unwrap();
    let seal = seal_turn(&doc(&lines), Some(chain_of(&first)), &broader).unwrap();
    let TurnSeal::Whole { broken, .. } = seal else {
        panic!("a pack change must not carry earlier segments");
    };
    assert_eq!(broken, ChainBreak::PacksChanged);
    assert_eq!(broken.finding(), "segment-chain-broken");
}

#[test]
fn an_earlier_chain_that_names_no_packs_reseals_whole() {
    let lines = records(0..6);
    let first = seal_turn(&doc(&lines[..4]), None, &packs()).unwrap();
    let mut chain = chain_of(&first);
    chain.packs = None;
    let seal = seal_turn(&doc(&lines), Some(chain), &packs()).unwrap();
    assert!(matches!(
        seal,
        TurnSeal::Whole {
            broken: ChainBreak::PacksChanged,
            ..
        }
    ));
}

#[test]
fn a_later_turn_carries_the_earlier_parts_fallback_counts_and_adds_its_own() {
    let first = seal_turn(&doc(&records(0..4)), None, &packs()).unwrap();
    let mut chain = chain_of(&first);
    assert_eq!(chain.fallbacks, vec![RedactionFallback::default()]);
    let recorded = RedactionFallback {
        lines: 3,
        placeholders: 1,
    };
    chain.fallbacks = vec![recorded];
    let second = seal_turn(&doc(&records(0..8)), Some(chain), &packs()).unwrap();
    assert_eq!(
        chain_of(&second).fallbacks,
        vec![recorded, RedactionFallback::default()]
    );
}

#[test]
fn a_previous_shard_without_per_part_counts_carries_its_parts_as_zero() {
    let first = seal_turn(&doc(&records(0..4)), None, &packs()).unwrap();
    let mut chain = chain_of(&first);
    chain.fallbacks.clear();
    let second = seal_turn(&doc(&records(0..8)), Some(chain), &packs()).unwrap();
    assert_eq!(
        chain_of(&second).fallbacks,
        vec![RedactionFallback::default(); 2]
    );
}

/// The session-end seal over the real gate, counting each call's first line.
fn session_end(raw: &SecretBytes, prior: Option<PriorChain>, calls: &mut Vec<usize>) -> TurnSeal {
    let packs = packs();
    let mut gate = |bytes: &SecretBytes, first_line: usize| {
        calls.push(first_line);
        rift_shadow_redact::redact_with_at(bytes, &packs, first_line)
    };
    seal_session_end_through(raw, prior, &packs, &mut gate).unwrap()
}

#[test]
fn a_session_end_seals_the_tail_through_the_last_line_and_matches_the_whole_seal() {
    let first = seal_turn(&doc(&records(0..4)), None, &packs()).unwrap();
    let lines = records(0..6);
    let mut calls = Vec::new();
    let end = session_end(&doc(&lines), Some(chain_of(&first)), &mut calls);
    assert_eq!(calls, vec![3], "only the tail past the turn's three lines");
    let parts = segments_of(&end);
    assert_eq!(parts[0], segments_of(&first)[0]);
    assert_eq!(parts[1], lines[3..].concat().into_bytes());
    let whole = rift_shadow_redact::redact_with(&doc(&lines), &packs()).unwrap();
    assert_eq!(parts.concat(), whole.redacted.as_slice());
}

#[test]
fn a_session_end_with_nothing_past_the_frontier_carries_the_parts_and_seals_nothing() {
    let first = seal_turn(&doc(&records(0..4)), None, &packs()).unwrap();
    let mut calls = Vec::new();
    let end = session_end(&doc(&records(0..3)), Some(chain_of(&first)), &mut calls);
    assert!(
        calls.is_empty(),
        "no turn-end content passes the gate again"
    );
    let TurnSeal::Appended { body, .. } = &end else {
        panic!("the carried parts are the final body");
    };
    let TranscriptBody::Segments(segments) = body else {
        panic!("still segmented");
    };
    assert!(matches!(segments.parts(), [SegmentPart::Carried(_)]));
}

#[test]
fn a_session_end_over_a_truncated_transcript_reseals_whole() {
    let first = seal_turn(&doc(&records(0..4)), None, &packs()).unwrap();
    let mut calls = Vec::new();
    let end = session_end(&doc(&records(0..1)), Some(chain_of(&first)), &mut calls);
    assert!(matches!(
        end,
        TurnSeal::Whole {
            broken: ChainBreak::Truncated,
            ..
        }
    ));
    assert_eq!(calls, vec![0]);
}
