use std::collections::BTreeMap;

use rift_core::SecretBytes;
use rift_recall_substrate::{MemorySubstrate, SubstrateReader};
use rift_shadow::backend::{Segments, TranscriptBody};

use super::*;
use crate::verify::passes::shard_passes;
use crate::verify::tests::shard;

const DOC: &[u8] = b"{\"type\":\"user\",\"text\":\"one\"}\n\
{\"type\":\"assistant\",\"text\":\"two\"}\n\
{\"type\":\"user\",\"text\":\"three\"}\n";

/// A real archive whose transcript is sealed line by line into three parts,
/// laid out by the writer's own artifact map: `(id, artifacts)`.
fn segmented_artifacts() -> (String, BTreeMap<String, Vec<u8>>) {
    let packs = rift_shadow::redact::RulePackSet::builtin();
    let raw = SecretBytes::new(DOC.to_vec());
    let (mut pair, _) = rift_shadow::archived_transcript(&raw, 0, &packs).unwrap();
    let seal_line = |line: &[u8]| {
        rift_shadow::redact::redact(&SecretBytes::new(line.to_vec()))
            .unwrap()
            .redacted
    };
    let parts = DOC
        .split_inclusive(|b| *b == b'\n')
        .map(seal_line)
        .collect();
    pair.full = TranscriptBody::Segments(Segments::new(parts).unwrap());
    let mut archive = rift_shadow::fixture_archive("two_turn");
    archive.transcript = Some(pair);
    archive.checkpoint_id = archive.derived_checkpoint_id().unwrap();
    (archive.checkpoint_id.clone(), archive.artifact_map())
}

fn substrate(id: &str, artifacts: &BTreeMap<String, Vec<u8>>) -> MemorySubstrate {
    artifacts.iter().fold(
        MemorySubstrate::local().with_shard(shard(id, None, 1)),
        |sub, (name, bytes)| sub.with_artifact(id, name, bytes),
    )
}

fn pass(sub: &MemorySubstrate, id: &str) -> Vec<Verdict> {
    segments_pass(&|name| sub.read_artifact(id, name), id).unwrap()
}

/// The one fault `edit` leaves on the segmented shard.
fn fault_after(edit: impl Fn(&mut BTreeMap<String, Vec<u8>>)) -> String {
    let (id, mut artifacts) = segmented_artifacts();
    edit(&mut artifacts);
    let found = pass(&substrate(&id, &artifacts), &id);
    match found.as_slice() {
        [Verdict::Structure(why)] => why.clone(),
        other => panic!("one structure finding expected, got {other:?}"),
    }
}

#[test]
fn an_honest_segmented_shard_verifies_end_to_end() {
    let (id, artifacts) = segmented_artifacts();
    let sub = substrate(&id, &artifacts);
    assert_eq!(pass(&sub, &id), Vec::new());
    assert_eq!(shard_passes(&sub).unwrap().worst, Verdict::Ok);
}

#[test]
fn a_missing_part_is_named_and_the_root_refuses_the_shard() {
    let (id, mut artifacts) = segmented_artifacts();
    artifacts.remove("transcript.segments/000001");
    let sub = substrate(&id, &artifacts);
    assert_ne!(shard_passes(&sub).unwrap().worst, Verdict::Ok);
    let why = fault_after(|a| {
        a.remove("transcript.segments/000001");
    });
    assert!(
        why.ends_with("transcript.segments/000001 is missing"),
        "{why}"
    );
}

#[test]
fn every_layout_fault_is_named_by_what_it_breaks() {
    // (what the finding says, the artifact written, its bytes)
    let cases: [(&str, String, &[u8]); 4] = [
        ("does not end at a line boundary", segment_name(2), b"{}"),
        (
            "holds 2 lines, metadata records 1",
            segment_name(0),
            b"{}\n{}\n",
        ),
        (
            "holds a segment past the 3 declared",
            segment_name(3),
            b"{}\n",
        ),
        (
            "transcript.jsonl is stored beside its declared segments",
            "transcript.jsonl".into(),
            DOC,
        ),
    ];
    for (want, name, bytes) in cases {
        let why = fault_after(|a| {
            a.insert(name.clone(), bytes.to_vec());
        });
        assert!(why.ends_with(want), "{want}: {why}");
    }
}

#[test]
fn parts_no_metadata_declares_are_named() {
    let sub = MemorySubstrate::local()
        .with_artifact("cp", "metadata.json", br#"{"tasks":["t"]}"#)
        .with_artifact("cp", "tasks/t/metadata.json", b"{}")
        .with_artifact("cp", "tasks/t/transcript.segments/000000", b"{}\n");
    let found = pass(&sub, "cp");
    assert_eq!(
        found,
        vec![Verdict::Structure(
            "cp: tasks/t/transcript.segments is present but undeclared".into()
        )]
    );
}
