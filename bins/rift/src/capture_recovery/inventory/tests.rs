//! The inventory over a real member's agent-queue journal, fed by the real
//! enqueue: counts, caps, classes and refusals as the closed rules state them.
use std::os::unix::ffi::OsStrExt as _;

use rift_shadow::hooks::run_state::RunRoot;

use super::*;
use crate::agent_queue::{AgentJob, BoundaryEdge, BoundarySnapshot, JobKind};
use crate::capture_recovery::eligibility::fixtures::Rig;
use crate::capture_recovery::test_support::Fixture;

const CLAUDE: &str = "claude-code";

/// The member's agent-queue directory, created as a producer creates it.
pub(crate) fn queue_dir(fixture: &Fixture) -> std::path::PathBuf {
    let dir = RunRoot::AgentQueue.under(&fixture.member);
    rift_shadow::hooks::run_state::create_private_dir(&dir).unwrap();
    dir
}

pub(crate) fn boundary_job(
    fixture: &Fixture,
    native: &str,
    ordinal: u32,
    edge: BoundaryEdge,
) -> AgentJob {
    AgentJob {
        schema_version: 1,
        work_tree: fixture.work_tree.clone(),
        queued_at_ms: 1_000 + i64::from(ordinal),
        attempts: 0,
        kind: JobKind::BoundarySnapshot(BoundarySnapshot {
            session_id: native.into(),
            agent: CLAUDE.into(),
            ordinal,
            boundary: edge,
            head_oid: None,
            captured_at_ms: 900 + i64::from(ordinal),
        }),
    }
}

/// One real queued boundary ticket; the path the journal published it at.
pub(crate) fn enqueue(
    fixture: &Fixture,
    native: &str,
    ordinal: u32,
    edge: BoundaryEdge,
) -> std::path::PathBuf {
    crate::agent_queue::enqueue_in(
        &queue_dir(fixture),
        &boundary_job(fixture, native, ordinal, edge),
    )
    .unwrap()
}

fn refused(result: Result<Inventory, Counts>) -> Counts {
    match result {
        Err(counts) => counts,
        Ok(_) => panic!("an actionable inventory was not expected"),
    }
}

#[test]
fn every_direct_entry_counts_toward_the_cap_and_the_sentinel_refuses_before_any_read() {
    let rig = Rig::new();
    let hold = rig.admit();
    let dir = queue_dir(&rig.fixture);
    for index in 0..8191 {
        std::fs::write(dir.join(format!("n{index}.error")), b"note").unwrap();
    }
    // The one ticket is malformed: if it were read, the refusal would say so.
    std::fs::write(dir.join("boundary-x.json"), b"{not json").unwrap();
    let queue = hold.agent_queue_namespace().unwrap();
    let counts = refused(enumerate(&queue));
    assert_eq!(counts.entries_counted, 8192);
    assert_eq!(counts.ancillary, 8191);
    assert_eq!(
        counts.refused,
        [(Refusal::TicketMalformed, 1)].into_iter().collect()
    );
    std::fs::remove_file(dir.join("boundary-x.json")).unwrap();
    std::fs::write(dir.join("n8191.error"), b"note").unwrap();
    let inventory = enumerate(&queue).unwrap();
    assert_eq!(inventory.counts.entries_counted, 8192);
    assert_eq!(inventory.counts.ancillary, 8192);
    assert!(inventory.sources.is_empty());
    std::fs::write(dir.join("boundary-y.json"), b"{not json").unwrap();
    let counts = refused(enumerate(&queue));
    assert_eq!(
        counts.entries_counted, 8192,
        "the sentinel is counted up to the cap only"
    );
    assert_eq!(
        counts.refused,
        [(Refusal::EntriesOverflow, 1)].into_iter().collect()
    );
}

#[test]
fn every_shape_outside_the_closed_classes_is_counted_then_refuses_with_its_reason() {
    let rig = Rig::new();
    let hold = rig.admit();
    let dir = queue_dir(&rig.fixture);
    enqueue(&rig.fixture, "n-1", 1, BoundaryEdge::Start);
    let queue = hold.agent_queue_namespace().unwrap();
    assert_eq!(enumerate(&queue).unwrap().sources.len(), 1);
    let fifo = dir.join("pipe");
    let mkfifo = std::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .unwrap();
    assert!(mkfifo.success());
    let non_ascii = dir.join(std::ffi::OsStr::from_bytes(b"caf\xc3\xa9.json"));
    let plants: Vec<(&str, std::path::PathBuf, Refusal)> = vec![
        ("fifo", fifo, Refusal::NotRegular),
        ("link", dir.join("link.json"), Refusal::Symlink),
        ("dir", dir.join("nested"), Refusal::Directory),
        ("stage", dir.join(".delivery-1-2.stage"), Refusal::Stage),
        ("tmp", dir.join("x.tmp"), Refusal::Stage),
        ("unknown", dir.join("readme.txt"), Refusal::UnknownName),
        ("non-ascii", non_ascii, Refusal::NameNotAscii),
    ];
    for (label, path, reason) in &plants {
        match *label {
            "fifo" => (),
            "link" => std::os::unix::fs::symlink("elsewhere", path).unwrap(),
            "dir" => std::fs::create_dir(path).unwrap(),
            _ => std::fs::write(path, b"").unwrap(),
        }
        let counts = refused(enumerate(&queue));
        assert_eq!(counts.refused.get(reason), Some(&1), "{label}");
        assert_eq!(counts.entries_counted, 2, "{label} is counted");
        assert_eq!(
            counts.tickets, 0,
            "{label}: nothing is read when the listing refuses"
        );
        if *label == "dir" {
            std::fs::remove_dir(path).unwrap();
        } else {
            std::fs::remove_file(path).unwrap();
        }
    }
    std::fs::write(dir.join(format!("condensed-{}", "a".repeat(64))), b"").unwrap();
    std::fs::write(dir.join("agent-x.error"), b"why").unwrap();
    let inventory = enumerate(&queue).unwrap();
    assert_eq!(inventory.counts.ancillary, 2);
    assert_eq!(inventory.counts.entries_counted, 3);
    assert_eq!(inventory.sources.len(), 1);
}

#[test]
fn a_repeated_name_and_an_iterator_error_refuse_the_listing_without_deduplicating() {
    fn listed(
        items: Vec<Result<(&'static str, EntryKind), ()>>,
    ) -> impl Iterator<Item = Result<(std::ffi::OsString, EntryKind), ()>> {
        items
            .into_iter()
            .map(|item| item.map(|(name, kind)| (std::ffi::OsString::from(name), kind)))
    }
    let mut counts = Counts::default();
    let entries = list_entries(
        listed(vec![
            Ok(("a.error", EntryKind::Regular)),
            Ok(("a.error", EntryKind::Regular)),
            Ok(("b.error", EntryKind::Regular)),
        ]),
        8192,
        &mut counts,
    );
    assert_eq!(counts.entries_counted, 3);
    assert_eq!(
        counts.refused,
        [(Refusal::DuplicateName, 1)].into_iter().collect()
    );
    assert_eq!(entries.len(), 2);
    let mut counts = Counts::default();
    list_entries(
        listed(vec![
            Ok(("a.error", EntryKind::Regular)),
            Err(()),
            Ok(("b.error", EntryKind::Regular)),
        ]),
        8192,
        &mut counts,
    );
    assert_eq!(counts.entries_counted, 1, "the listing stops at its error");
    assert_eq!(
        counts.refused,
        [(Refusal::QueueUnavailable, 1)].into_iter().collect()
    );
    let mut counts = Counts::default();
    list_entries(
        listed(vec![
            Ok(("a.error", EntryKind::Regular)),
            Ok(("b.error", EntryKind::Regular)),
        ]),
        1,
        &mut counts,
    );
    assert_eq!(counts.entries_counted, 1);
    assert_eq!(
        counts.refused,
        [(Refusal::EntriesOverflow, 1)].into_iter().collect()
    );
    for reason in [
        Refusal::QueueUnavailable,
        Refusal::NameNotAscii,
        Refusal::SourceAlias,
    ] {
        let spelled = reason.as_str();
        assert!(
            spelled.bytes().all(|b| b.is_ascii_lowercase() || b == b'-'),
            "{spelled}"
        );
    }
    assert_eq!(Refusal::EntriesOverflow.as_str(), "entries-overflow");
}

/// The inventory is the exact observed list: a ticket a producer publishes
/// after enumeration, even before the reads finish, is outside it.
#[test]
fn a_ticket_published_after_enumeration_is_outside_the_frozen_list() {
    let rig = Rig::new();
    let hold = rig.admit();
    enqueue(&rig.fixture, "n-1", 1, BoundaryEdge::Start);
    enqueue(&rig.fixture, "n-1", 1, BoundaryEdge::End);
    let queue = hold.agent_queue_namespace().unwrap();
    let mut late = false;
    let inventory = enumerate_with(&queue, &mut |point| {
        if point == Phase::BeforeRetain(0) && !late {
            late = true;
            enqueue(&rig.fixture, "n-2", 1, BoundaryEdge::Start);
        }
        Ok(())
    })
    .unwrap();
    assert!(late);
    assert_eq!(inventory.counts.entries_counted, 2);
    assert_eq!(inventory.counts.tickets, 2);
    assert!(inventory
        .sources
        .iter()
        .all(|s| s.snapshot.session_id == "n-1"));
    let again = enumerate(&queue).unwrap();
    assert_eq!(again.counts.entries_counted, 3);
}

fn pad_to(path: &std::path::Path, len: usize) {
    let mut bytes = std::fs::read(path).unwrap();
    assert!(bytes.len() <= len);
    bytes.resize(len, b' ');
    std::fs::write(path, bytes).unwrap();
}

#[test]
fn a_ticket_of_exactly_the_cap_reads_whole_and_one_byte_more_refuses_pre_read() {
    let rig = Rig::new();
    let hold = rig.admit();
    let path = enqueue(&rig.fixture, "n-1", 1, BoundaryEdge::Start);
    let queue = hold.agent_queue_namespace().unwrap();
    let compact = enumerate(&queue).unwrap();
    let source = &compact.sources[0];
    assert_eq!(
        std::path::Path::new(&source.name),
        path.file_name().unwrap()
    );
    assert_eq!(source.queued_at_ms, 1_001);
    let witnessed = files::physical_identity(&source.file.metadata().unwrap());
    assert_eq!(
        witnessed, source.identity,
        "the retained descriptor is the listed inode"
    );
    assert_eq!(witnessed.permission_bits, 0o600);
    pad_to(&path, MAX_BYTES);
    let padded = enumerate(&queue).unwrap();
    let (a, b) = (&compact.sources[0], &padded.sources[0]);
    assert_eq!(b.raw_length, MAX_BYTES as u64);
    assert_eq!(
        a.context_sha256, b.context_sha256,
        "whitespace is not context"
    );
    assert_ne!(
        a.ticket_sha256, b.ticket_sha256,
        "the ticket digest is the raw bytes"
    );
    assert_eq!(a.context_id, b.context_id);
    pad_to(&path, MAX_BYTES + 1);
    let counts = refused(enumerate(&queue));
    assert_eq!(
        counts.refused,
        [(Refusal::TicketOversize, 1)].into_iter().collect()
    );
    assert_eq!(counts.tickets, 0);
}

#[test]
fn top_level_attempts_change_the_ticket_digest_and_never_the_context_digest() {
    let rig = Rig::new();
    let hold = rig.admit();
    let path = enqueue(&rig.fixture, "n-1", 1, BoundaryEdge::Start);
    let queue = hold.agent_queue_namespace().unwrap();
    let first = enumerate(&queue).unwrap();
    let mut value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    value["attempts"] = serde_json::json!(2);
    std::fs::write(
        &path,
        crate::capture_queue::delivery::encode(&value).unwrap(),
    )
    .unwrap();
    let second = enumerate(&queue).unwrap();
    assert_eq!(first.sources[0].attempts, 0);
    assert_eq!(second.sources[0].attempts, 2);
    assert_eq!(
        first.sources[0].context_sha256,
        second.sources[0].context_sha256
    );
    assert_ne!(
        first.sources[0].ticket_sha256,
        second.sources[0].ticket_sha256
    );
    // S8's retry members ride a ticket named by its bare stem (a name the queue
    // accepts); the gate tolerates them and, like `attempts`, the context
    // digest strips them while the ticket digest still sees the rewrite.
    let stem = crate::agent_queue::logical_stem(&boundary_job(
        &rig.fixture,
        "n-1",
        1,
        BoundaryEdge::Start,
    ))
    .unwrap();
    std::fs::remove_file(&path).unwrap();
    value["next_due_ms"] = serde_json::json!(4000);
    value["parked"] = serde_json::json!(false);
    std::fs::write(
        path.with_file_name(format!("{stem}.json")),
        crate::capture_queue::delivery::encode(&value).unwrap(),
    )
    .unwrap();
    let third = enumerate(&queue).unwrap();
    assert_eq!(third.sources[0].attempts, 2);
    assert_eq!(
        third.sources[0].context_sha256,
        second.sources[0].context_sha256
    );
    assert_ne!(
        third.sources[0].ticket_sha256,
        second.sources[0].ticket_sha256
    );
}

#[test]
fn an_aliased_source_an_unknown_shape_and_the_aggregate_cap_refuse_the_whole_inventory() {
    let rig = Rig::new();
    let hold = rig.admit();
    let path = enqueue(&rig.fixture, "n-1", 1, BoundaryEdge::Start);
    let queue = hold.agent_queue_namespace().unwrap();
    // A hardlinked twin under the bare stem the queue also accepts: one inode, two names.
    let stem = crate::agent_queue::logical_stem(&boundary_job(
        &rig.fixture,
        "n-1",
        1,
        BoundaryEdge::Start,
    ))
    .unwrap();
    let twin = path.with_file_name(format!("{stem}.json"));
    std::fs::hard_link(&path, &twin).unwrap();
    let counts = refused(enumerate(&queue));
    assert_eq!(
        counts.refused,
        [(Refusal::SourceAlias, 1)].into_iter().collect()
    );
    std::fs::remove_file(&twin).unwrap();
    // An unknown nested field blocks complete selection; the ticket is not understood.
    let mut value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    value["kind"]["extra"] = serde_json::json!(true);
    std::fs::write(
        &twin,
        crate::capture_queue::delivery::encode(&value).unwrap(),
    )
    .unwrap();
    let counts = refused(enumerate(&queue));
    assert_eq!(
        counts.refused,
        [(Refusal::TicketShape, 1)].into_iter().collect()
    );
    std::fs::remove_file(&twin).unwrap();
    // An understood non-boundary job is owed, counted, never read as a source.
    crate::agent_queue::enqueue_in(
        &queue_dir(&rig.fixture),
        &AgentJob {
            schema_version: 1,
            work_tree: rig.fixture.work_tree.clone(),
            queued_at_ms: 5,
            attempts: 0,
            kind: JobKind::DocSweep,
        },
    )
    .unwrap();
    let inventory = enumerate(&queue).unwrap();
    assert_eq!(inventory.counts.understood_other, 1);
    assert_eq!(inventory.counts.tickets, 1);
    assert_eq!(inventory.sources.len(), 1);
}

#[test]
fn the_aggregate_ticket_budget_refuses_before_the_read_that_would_cross_it() {
    let rig = Rig::over(Fixture::with_ticket_budgets(65_536, 70_000));
    let hold = rig.admit();
    let first = enqueue(&rig.fixture, "n-1", 1, BoundaryEdge::Start);
    let second = enqueue(&rig.fixture, "n-1", 1, BoundaryEdge::End);
    pad_to(&first, 60_000);
    let queue = hold.agent_queue_namespace().unwrap();
    assert_eq!(enumerate(&queue).unwrap().sources.len(), 2);
    pad_to(&second, 10_001);
    let counts = refused(enumerate(&queue));
    assert_eq!(
        counts.refused,
        [(Refusal::AggregateOverflow, 1)].into_iter().collect()
    );
}
