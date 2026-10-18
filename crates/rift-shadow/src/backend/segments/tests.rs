use std::collections::BTreeMap;

use rift_recall_substrate::parse::segments::resolve_transcript;

use super::super::fixtures::{archive, redacted_of, task, transcript_of};
use super::super::{ArchivedTranscript, NativeBackend, ShadowBackend};
use super::*;

const DOC: &[u8] = b"{\"type\":\"user\",\"text\":\"one\"}\n\
{\"type\":\"assistant\",\"text\":\"two\"}\n\
{\"type\":\"user\",\"text\":\"three\"}\n";

/// The pair condense builds over `DOC`, its full body cut after line one.
fn segmented() -> ArchivedTranscript {
    let mut pair = transcript_of(DOC);
    let cut = DOC.iter().position(|b| *b == b'\n').unwrap() + 1;
    let (head, tail) = DOC.split_at(cut);
    let parts = Segments::new(vec![redacted_of(head), redacted_of(tail)]).unwrap();
    pair.full = TranscriptBody::Segments(parts);
    pair
}

fn resolved(map: &BTreeMap<String, Vec<u8>>, rel: &str) -> Option<Vec<u8>> {
    resolve_transcript(rel, |name| Ok(map.get(name).cloned())).unwrap()
}

#[test]
fn a_segmented_transcript_binds_the_root_of_the_whole_one() {
    let mut whole = archive();
    whole.transcript = Some(transcript_of(DOC));
    let mut cut = archive();
    cut.transcript = Some(segmented());
    assert_eq!(
        cut.merkle_root(),
        whole.merkle_root(),
        "leaf over the concatenation"
    );

    let meta = String::from_utf8(cut.metadata_json()).unwrap();
    assert!(meta.contains(r#""segment_raw_lines":[1,2]"#), "{meta}");
    assert!(meta.contains(r#""transcript_segments":2"#), "{meta}");
    let plain = String::from_utf8(whole.metadata_json()).unwrap();
    assert!(
        !plain.contains("segment"),
        "a whole body writes no member: {plain}"
    );
}

#[test]
fn a_segmented_shard_carries_its_parts_and_no_whole_blob() {
    let mut a = archive();
    a.transcript = Some(segmented());
    let mut t = task("toolu_01");
    t.transcript = Some(segmented());
    a.tasks = vec![t];
    let map = a.artifact_map();
    for dir in ["", "tasks/toolu_01/"] {
        assert!(map.contains_key(&format!("{dir}transcript.segments/000000")));
        assert!(map.contains_key(&format!("{dir}transcript.segments/000001")));
        assert!(
            !map.contains_key(&format!("{dir}transcript.jsonl")),
            "{dir}"
        );
        assert_eq!(
            resolved(&map, &format!("{dir}transcript.jsonl")).unwrap(),
            DOC
        );
    }
}

#[test]
fn a_part_off_a_line_boundary_is_refused() {
    assert!(Segments::new(Vec::new()).is_err(), "no parts");
    assert!(
        Segments::new(vec![redacted_of(b"")]).is_err(),
        "an empty part"
    );
    assert!(
        Segments::new(vec![redacted_of(b"{}")]).is_err(),
        "no trailing newline"
    );
    assert!(Segments::new(vec![redacted_of(b"{}\n")]).is_ok());
}

#[test]
fn the_writer_takes_every_part_through_the_durable_door() {
    let mut a = archive();
    a.transcript = Some(segmented());
    a.checkpoint_id = a.derived_checkpoint_id().unwrap();
    let n = NativeBackend::in_memory();
    n.archive_checkpoint(&a).unwrap();
    // content.redacted + transcript.compact.jsonl + two parts.
    assert_eq!(n.writer().durable_count(), 4);
}
