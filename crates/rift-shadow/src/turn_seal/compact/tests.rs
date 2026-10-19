use proptest::prelude::*;
use rift_core::SecretBytes;
use rift_recall_substrate::worktree::git_blob_oid;
use rift_shadow_redact::RulePackSet;

use super::{seal_turn_compact, CompactCarry, CompactCut, CompactSeal};
use crate::backend::{SegmentPart, TranscriptBody};
use crate::transcript::compact_jsonl;
use crate::turn_seal::PriorPart;

/// One assistant fragment of message `id`; fragments sharing an id merge.
fn fragment(id: u8, text: u8) -> String {
    format!(
        r#"{{"type":"assistant","message":{{"id":"m{id}","content":[{{"type":"text","text":"t{text}"}}]}}}}"#
    )
}

fn transcript(records: &[(u8, u8)]) -> Vec<u8> {
    records
        .iter()
        .map(|(id, text)| fragment(*id, *text) + "\n")
        .collect::<String>()
        .into_bytes()
}

/// The bytes of `raw`'s first `lines` `\n`-terminated lines.
fn prefix(raw: &[u8], lines: u64) -> &[u8] {
    let end = raw
        .iter()
        .enumerate()
        .filter(|(_, b)| **b == b'\n')
        .map(|(at, _)| at + 1)
        .take(lines as usize)
        .last()
        .unwrap_or(0);
    &raw[..end]
}

fn one_shot(raw: &[u8], lines: u64) -> Vec<u8> {
    let raw = SecretBytes::new(prefix(raw, lines).to_vec());
    compact_jsonl(&raw, 0).compact().as_slice().to_vec()
}

fn prior_of(body: &TranscriptBody) -> Vec<PriorPart> {
    let TranscriptBody::Segments(segments) = body else {
        panic!("a turn's compact seal is segmented");
    };
    segments
        .parts()
        .iter()
        .map(|p| PriorPart {
            oid: git_blob_oid(p.as_slice()),
            bytes: p.as_slice().to_vec(),
        })
        .collect()
}

/// Seal one turn as the hook does, with the earlier parts and, unless the
/// sidecar was lost, the carry the previous turn left.
fn turn(
    raw: &SecretBytes,
    cut: CompactCut,
    previous: Option<&CompactSeal>,
    lost: bool,
) -> CompactSeal {
    let prior = previous.map(|l| prior_of(&l.body));
    let carry = previous.filter(|_| !lost).map(|l| &l.carry);
    seal_turn_compact(raw, cut, prior, carry, &RulePackSet::default()).unwrap()
}

proptest! {
    #[test]
    fn the_joined_compact_parts_equal_the_one_shot_compact_form_at_every_cut(
        records in proptest::collection::vec((0u8..4, 0u8..9), 1..24),
        cuts in proptest::collection::vec((0usize..24, any::<bool>()), 0..6),
    ) {
        let raw = SecretBytes::new(transcript(&records));
        let mut cuts: Vec<(u64, bool)> =
            cuts.into_iter().map(|(c, lost)| ((c % records.len()) as u64 + 1, lost)).collect();
        cuts.sort_unstable();
        cuts.dedup_by_key(|(c, _)| *c);
        let mut last: Option<CompactSeal> = None;
        let mut before = 0;
        for (now, lost) in cuts {
            let sealed = turn(&raw, CompactCut { before, now }, last.as_ref(), lost);
            prop_assert_eq!(sealed.body.as_slice(), one_shot(raw.as_slice(), now));
            last = Some(sealed);
            before = now;
        }
    }
}

#[test]
fn a_line_growing_a_closed_record_starts_the_turn_over_and_still_joins_exactly() {
    let raw = SecretBytes::new(transcript(&[(1, 1), (2, 2), (3, 3), (1, 4)]));
    let first = turn(&raw, CompactCut { before: 0, now: 2 }, None, false);
    let second = turn(&raw, CompactCut { before: 2, now: 3 }, Some(&first), false);
    let third = turn(&raw, CompactCut { before: 3, now: 4 }, Some(&second), false);
    let TranscriptBody::Segments(segments) = &third.body else {
        panic!("segmented");
    };
    assert_eq!(
        segments.parts().len(),
        3,
        "the earlier parts are still carried"
    );
    assert_eq!(third.body.as_slice(), one_shot(raw.as_slice(), 4));
}

#[test]
fn a_carry_from_another_transcript_does_not_fit_and_is_rebuilt() {
    let other = transcript(&[(2, 2), (3, 3)]);
    let raw = transcript(&[(1, 1), (2, 2), (1, 3)]);
    let carry = CompactCarry::rebuilt(&other, 2);
    assert!(!carry.fits(&raw, 2));
    let rebuilt = CompactCarry::rebuilt(&raw, 2);
    assert!(rebuilt.fits(&raw, 2));
    assert_eq!(rebuilt.records(), 2);
}

#[test]
fn a_later_turn_carries_the_earlier_compact_part_and_reemits_the_record_it_grew() {
    let packs = RulePackSet::default();
    let raw = SecretBytes::new(transcript(&[(1, 1), (2, 2), (1, 3), (3, 4)]));
    let first = seal_turn_compact(&raw, CompactCut { before: 0, now: 2 }, None, None, &packs)
        .unwrap()
        .body;
    let second = seal_turn_compact(
        &raw,
        CompactCut { before: 2, now: 4 },
        Some(prior_of(&first)),
        None,
        &packs,
    )
    .unwrap()
    .body;
    let TranscriptBody::Segments(segments) = &second else {
        panic!("segmented");
    };
    assert!(matches!(
        segments.parts(),
        [SegmentPart::Carried(_), SegmentPart::Sealed(_)]
    ));
    let new = String::from_utf8(segments.parts()[1].as_slice().to_vec()).unwrap();
    assert!(
        new.contains(r#""id":"m1""#) && new.contains(r#""id":"m3""#),
        "{new}"
    );
    assert!(
        !new.contains(r#""id":"m2""#),
        "an unchanged record is not re-emitted: {new}"
    );
    assert_eq!(second.as_slice(), one_shot(raw.as_slice(), 4));
}

#[test]
fn an_earlier_compact_part_off_its_oid_reseals_the_compact_form_whole() {
    let packs = RulePackSet::default();
    let raw = SecretBytes::new(transcript(&[(1, 1), (2, 2), (1, 3)]));
    let first = seal_turn_compact(&raw, CompactCut { before: 0, now: 2 }, None, None, &packs)
        .unwrap()
        .body;
    let mut prior = prior_of(&first);
    prior[0].oid = "0".repeat(40);
    let second = seal_turn_compact(
        &raw,
        CompactCut { before: 2, now: 3 },
        Some(prior),
        None,
        &packs,
    )
    .unwrap()
    .body;
    let TranscriptBody::Segments(segments) = &second else {
        panic!("segmented");
    };
    assert!(matches!(segments.parts(), [SegmentPart::Sealed(_)]));
    assert_eq!(second.as_slice(), one_shot(raw.as_slice(), 3));
}

#[test]
#[ignore = "a timing, not a verdict: run by hand with --release -- --ignored --nocapture"]
fn a_turn_over_a_28_mb_carried_prefix_compacts_only_its_new_lines() {
    const LINES: u64 = 117_587;
    let line = |i: u64| {
        format!(
            r#"{{"type":"assistant","message":{{"id":"m{}","content":[{{"type":"text","text":"{}"}}]}}}}"#,
            i / 3,
            "x".repeat(170)
        ) + "\n"
    };
    let raw = SecretBytes::new((0..LINES).map(line).collect::<String>().into_bytes());
    let packs = RulePackSet::default();
    let before = LINES - 199;
    let first = turn(
        &raw,
        CompactCut {
            before: 0,
            now: before,
        },
        None,
        false,
    );
    let cut = CompactCut { before, now: LINES };
    for carry in [Some(&first.carry), None] {
        let prior = Some(prior_of(&first.body));
        let started = std::time::Instant::now();
        let sealed = seal_turn_compact(&raw, cut, prior, carry, &packs).unwrap();
        eprintln!(
            "compact step, carry {}: {:?} over {} bytes",
            carry.is_some(),
            started.elapsed(),
            raw.as_slice().len()
        );
        assert_eq!(sealed.body.as_slice(), one_shot(raw.as_slice(), LINES));
    }
}
