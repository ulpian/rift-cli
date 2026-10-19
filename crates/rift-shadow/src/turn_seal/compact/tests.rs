use proptest::prelude::*;
use rift_core::SecretBytes;
use rift_recall_substrate::parse::segments::join_compact_segments;
use rift_recall_substrate::worktree::git_blob_oid;
use rift_shadow_redact::RulePackSet;

use super::{compact_part, prefix, seal_turn_compact, CompactCut};
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

proptest! {
    #[test]
    fn the_joined_compact_parts_equal_the_one_shot_compact_form_at_every_cut(
        records in proptest::collection::vec((0u8..4, 0u8..9), 1..24),
        cuts in proptest::collection::vec(0usize..24, 0..6),
    ) {
        let raw = transcript(&records);
        let mut cuts: Vec<u64> = cuts.into_iter().map(|c| (c % records.len()) as u64 + 1).collect();
        cuts.sort_unstable();
        cuts.dedup();
        let mut parts = Vec::new();
        let mut before = 0;
        for now in cuts {
            parts.push(compact_part(&raw, before, now));
            prop_assert_eq!(join_compact_segments(&parts), one_shot(&raw, now));
            before = now;
        }
    }
}

#[test]
fn a_later_turn_carries_the_earlier_compact_part_and_reemits_the_record_it_grew() {
    let packs = RulePackSet::default();
    let raw = SecretBytes::new(transcript(&[(1, 1), (2, 2), (1, 3), (3, 4)]));
    let first = seal_turn_compact(&raw, CompactCut { before: 0, now: 2 }, None, &packs).unwrap();
    let second = seal_turn_compact(
        &raw,
        CompactCut { before: 2, now: 4 },
        Some(prior_of(&first)),
        &packs,
    )
    .unwrap();
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
    let first = seal_turn_compact(&raw, CompactCut { before: 0, now: 2 }, None, &packs).unwrap();
    let mut prior = prior_of(&first);
    prior[0].oid = "0".repeat(40);
    let second =
        seal_turn_compact(&raw, CompactCut { before: 2, now: 3 }, Some(prior), &packs).unwrap();
    let TranscriptBody::Segments(segments) = &second else {
        panic!("segmented");
    };
    assert!(matches!(segments.parts(), [SegmentPart::Sealed(_)]));
    assert_eq!(second.as_slice(), one_shot(raw.as_slice(), 3));
}
