use rift_core::SecretBytes;
use rift_shadow_redact::RulePackSet;

use super::{seal_turn, ChainBreak, PriorChain, TurnSeal};
use crate::backend::TranscriptBody;

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
            .map(|p| p.as_slice().to_vec())
            .collect(),
        raw_lines: segments.raw_lines(),
        leaf: rift_hashchain::leaf_hash(body.as_slice()),
    }
}

fn segments_of(seal: &TurnSeal) -> Vec<Vec<u8>> {
    chain_of(seal).parts
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
    chain.parts[0] = records(10..13).concat().into_bytes();
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
fn an_earlier_part_the_current_packs_would_seal_differently_breaks_the_chain() {
    let first = seal_turn(&doc(&records(0..4)), None, &packs()).unwrap();
    let mut chain = chain_of(&first);
    // A part holding an unmasked secret whose leaf still matches: only the
    // second pass through the gate can tell.
    chain.parts[0] = "{\"type\":\"user\",\"message\":\"ghp_0123456789abcdefghijABCDEFGHIJ012345\"}\n{\"a\":1}\n{\"a\":2}\n".as_bytes().to_vec();
    chain.leaf = rift_hashchain::leaf_hash(&chain.parts[0]);
    let seal = seal_turn(&doc(&records(0..6)), Some(chain), &packs()).unwrap();
    assert!(matches!(
        seal,
        TurnSeal::Whole {
            broken: ChainBreak::Regated,
            ..
        }
    ));
}
