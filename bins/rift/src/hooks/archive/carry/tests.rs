use std::collections::BTreeMap;
use std::path::PathBuf;

use rift_core::SecretBytes;
use rift_shadow::backend::{ArchivedTask, ArchivedTranscript, TranscriptBody};
use rift_shadow::redact::RulePackSet;

use super::super::super::originals::{session_spool_dir, SPOOL_SUFFIX};
use super::super::tasks::{session_tasks_noting, TaskSources};
use super::*;

const SESSION: &str = "11111111-2222-3333-4444-555555555555";

/// A previous shard's blobs: path inside the shard → oid and bytes.
type Shard = BTreeMap<String, (String, Vec<u8>)>;

struct Fixture {
    dir: tempfile::TempDir,
    main: PathBuf,
    subagent: PathBuf,
}

fn runs() -> usize {
    GATE_RUNS.with(std::cell::Cell::get)
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let main = dir.path().join(format!("{SESSION}.jsonl"));
    std::fs::write(&main, "{\"type\":\"user\",\"message\":\"go\"}\n").unwrap();
    let subagents = dir.path().join(SESSION).join("subagents");
    std::fs::create_dir_all(&subagents).unwrap();
    let subagent = subagents.join("agent-a1.jsonl");
    let line = "{\"type\":\"user\",\"timestamp\":\"2026-10-19T00:00:00Z\",\"message\":\"sub\"}\n";
    std::fs::write(&subagent, line).unwrap();
    Fixture {
        dir,
        main,
        subagent,
    }
}

fn archive(f: &Fixture, carry: &TurnCarry, notes: &mut Vec<CarriedEntry>) -> Vec<ArchivedTask> {
    let packs = RulePackSet::builtin();
    let src = TaskSources {
        git_dir: f.dir.path(),
        session_id: SESSION,
        packs: &packs,
        observations: &[],
        main: Some(f.main.clone()),
        main_bytes: None,
        spool: None,
        carry: Some(carry),
    };
    session_tasks_noting(&src, notes)
}

fn put(shard: &mut Shard, path: String, body: &TranscriptBody) {
    let bytes = body.as_slice().to_vec();
    shard.insert(path, (body.blob_oid().unwrap(), bytes));
}

fn shard_of(tasks: &[ArchivedTask]) -> Shard {
    let mut shard = Shard::new();
    for task in tasks {
        let pair = task.transcript.as_ref().unwrap();
        let dir = format!("tasks/{}/", task.tool_use_id);
        put(
            &mut shard,
            format!("{dir}{TRANSCRIPT_FULL_NAME}"),
            &pair.full,
        );
        put(
            &mut shard,
            format!("{dir}{TRANSCRIPT_COMPACT_NAME}"),
            &pair.compact,
        );
    }
    shard
}

fn carry_from(notes: &[CarriedEntry], shard: &Shard) -> TurnCarry {
    TurnCarry::of(notes, &|path| shard.get(path).cloned())
}

/// A first turn end over the fixture, and the carry the next one reads.
fn first_turn(f: &Fixture) -> (Vec<ArchivedTask>, Vec<CarriedEntry>, TurnCarry) {
    let before = runs();
    let mut notes = Vec::new();
    let tasks = archive(f, &TurnCarry::default(), &mut notes);
    assert_eq!(runs(), before + 1, "the first turn end seals the subagent");
    assert_eq!(notes.len(), 1);
    let carry = carry_from(&notes, &shard_of(&tasks));
    (tasks, notes, carry)
}

#[test]
fn a_second_turn_end_writes_an_unchanged_subagent_by_its_oids_without_redacting_it() {
    let f = fixture();
    let (first, first_notes, carry) = first_turn(&f);
    let before = runs();
    let mut notes = Vec::new();
    let tasks = archive(&f, &carry, &mut notes);
    assert_eq!(
        runs(),
        before,
        "an unchanged subagent never meets the gate again"
    );
    let pair = tasks[0].transcript.as_ref().unwrap();
    assert!(matches!(pair.full, TranscriptBody::Carried(_)));
    assert!(matches!(pair.compact, TranscriptBody::Carried(_)));
    let was = first[0].transcript.as_ref().unwrap();
    assert_eq!(pair.full.as_slice(), was.full.as_slice());
    assert_eq!(
        notes, first_notes,
        "the sidecar records the same entry again"
    );
}

#[test]
fn a_grown_subagent_is_sealed_whole_again() {
    let f = fixture();
    let (_, first_notes, carry) = first_turn(&f);
    let mut grown = std::fs::read(&f.subagent).unwrap();
    grown.extend_from_slice(b"{\"type\":\"assistant\",\"message\":\"more\"}\n");
    std::fs::write(&f.subagent, &grown).unwrap();
    let before = runs();
    let mut notes = Vec::new();
    let tasks = archive(&f, &carry, &mut notes);
    assert_eq!(runs(), before + 1, "a grown file is resealed");
    let pair = tasks[0].transcript.as_ref().unwrap();
    assert!(matches!(pair.full, TranscriptBody::Whole(_)));
    assert_ne!(notes[0].raw_sha256, first_notes[0].raw_sha256);
}

#[test]
fn a_deleted_subagent_file_carries_nothing_and_seals_nothing() {
    let f = fixture();
    let (_, _, carry) = first_turn(&f);
    std::fs::remove_file(&f.subagent).unwrap();
    let before = runs();
    let mut notes = Vec::new();
    let tasks = archive(&f, &carry, &mut notes);
    assert!(tasks.is_empty());
    assert!(notes.is_empty());
    assert_eq!(runs(), before);
}

#[test]
fn a_blob_whose_oid_moved_is_not_carried() {
    let f = fixture();
    let (first, notes, _) = first_turn(&f);
    let mut shard = shard_of(&first);
    for (oid, _) in shard.values_mut() {
        oid.replace_range(0..1, if oid.starts_with('0') { "1" } else { "0" });
    }
    let carry = carry_from(&notes, &shard);
    let before = runs();
    archive(&f, &carry, &mut Vec::new());
    assert_eq!(
        runs(),
        before + 1,
        "an entry the shard does not hold is resealed"
    );
}

/// The spool's one record attached to a session transcript.
fn spooled(common: &std::path::Path, carry: &TurnCarry) -> (Spool, ArchivedTranscript) {
    let packs = RulePackSet::builtin();
    let spool = Spool::load_carrying(common, SESSION, &packs, Some(carry));
    let raw = SecretBytes::new(b"{\"type\":\"user\",\"message\":\"hi\"}\n".to_vec());
    let (mut pair, _) = rift_shadow::archived_transcript(&raw, 0, &packs).unwrap();
    spool.attach_orphans(Some(&mut pair), &mut []);
    (spool, pair)
}

#[test]
fn a_second_turn_end_writes_an_unchanged_spooled_original_by_its_oid() {
    let common = tempfile::tempdir().unwrap();
    let dir = session_spool_dir(common.path(), SESSION);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(format!("toolu_1{SPOOL_SUFFIX}")),
        b"not a record\n",
    )
    .unwrap();
    let before = runs();
    let (spool, pair) = spooled(common.path(), &TurnCarry::default());
    assert_eq!(runs(), before + 1, "the first turn end seals the record");
    let notes = original_entries(&spool, Some(&pair), &[]);
    assert_eq!(notes.len(), 1);
    let mut shard = Shard::new();
    let original = &pair.originals[0];
    let path = format!("{ORIGINALS_DIR}/{}", original.file_name());
    put(&mut shard, path, original.body.as_ref().unwrap());
    let carry = carry_from(&notes, &shard);
    let before = runs();
    let (_, again) = spooled(common.path(), &carry);
    assert_eq!(
        runs(),
        before,
        "an unchanged record never meets the gate again"
    );
    let body = again.originals[0].body.as_ref().unwrap();
    assert!(matches!(body, TranscriptBody::Carried(_)));
    assert_eq!(body.as_slice(), original.body.as_ref().unwrap().as_slice());
    assert_eq!(again.originals[0].state, original.state);
}
