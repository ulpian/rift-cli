//! A levelling walk projects only what the last completed walk did not: the
//! shard count it projects, the artifacts it reads, and the rows it ends with.

use std::collections::BTreeSet;
use std::sync::Mutex;

use rift_recall_substrate::{CheckpointShard, Dialect, MemorySubstrate, SubstrateReader};
use rift_store::{CanonicalStore, SqliteStore};

use super::ingest_from_substrate;

const REPORT: &str = r#"{"total_spans":1,"rule_pack_versions":["builtin@1"],"input_sha256":"cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"}"#;

/// The tables a shard's projection writes, each with an order that makes a
/// dump comparable across two stores.
const DUMPS: &[&str] = &[
    "SELECT checkpoint_id, strategy, created_at, merkle_root, prev_hash FROM checkpoint \
     ORDER BY checkpoint_id",
    "SELECT checkpoint_id, session_id, ordinal FROM session_in_checkpoint \
     ORDER BY checkpoint_id, ordinal",
    "SELECT idempotency_key, session_id, kind FROM captured_event ORDER BY idempotency_key",
    "SELECT session_id, agent, started_at FROM session ORDER BY session_id",
    "SELECT tool_call_id, turn_id, tool_name, started_at FROM tool_call ORDER BY tool_call_id",
];

/// A reader that records which shards a walk opened an artifact of.
struct CountingReader {
    inner: MemorySubstrate,
    opened: Mutex<BTreeSet<String>>,
}

impl CountingReader {
    fn new(inner: MemorySubstrate) -> Self {
        Self {
            inner,
            opened: Mutex::new(BTreeSet::new()),
        }
    }

    fn opened(&self) -> BTreeSet<String> {
        self.opened.lock().unwrap().clone()
    }
}

impl SubstrateReader for CountingReader {
    fn is_local_only(&self) -> bool {
        self.inner.is_local_only()
    }
    fn checkpoints(&self) -> rift_core::Result<Vec<CheckpointShard>> {
        self.inner.checkpoints()
    }
    fn blob(&self, id: &str) -> rift_core::Result<Option<Vec<u8>>> {
        self.inner.blob(id)
    }
    fn read_artifact(&self, id: &str, rel: &str) -> rift_core::Result<Option<Vec<u8>>> {
        self.opened.lock().unwrap().insert(id.to_owned());
        self.inner.read_artifact(id, rel)
    }
}

fn store() -> SqliteStore {
    let s = SqliteStore::open_in_memory().unwrap();
    s.create_canonical_schema().unwrap();
    s
}

fn shard_id(n: u8) -> String {
    format!("{n:02x}").repeat(32)
}

/// A branch of valid shards, in walk order, one session each.
fn branch(numbers: &[u8]) -> MemorySubstrate {
    let mut sub = MemorySubstrate::local();
    for (ordinal, n) in (1_u64..).zip(numbers) {
        sub = with_valid_shard(sub, *n, ordinal);
    }
    sub
}

fn with_valid_shard(sub: MemorySubstrate, n: u8, ordinal: u64) -> MemorySubstrate {
    let id = shard_id(n);
    let session = format!("native-{n}");
    let metadata = format!(
        r#"{{"strategy":"manual-commit","merkle_root":"{}","sessions":["{session}"]}}"#,
        "d".repeat(64)
    );
    let full = format!(
        r#"{{"session_id":"{session}","turn":0,"event":0,"kind":"session_start","verb":"SessionStart","occurred_at_ms":{n},"payload":{{}}}}
{{"session_id":"{session}","turn":1,"event":0,"kind":"tool_call","verb":"PreToolUse","occurred_at_ms":{n},"payload":{{"tool_name":"Bash"}}}}"#
    );
    sub.with_shard(CheckpointShard {
        checkpoint_id: id.clone(),
        dialect: Dialect::Rift,
        prev: None,
        ordinal,
    })
    .with_artifact(&id, "redaction_report.json", REPORT.as_bytes())
    .with_artifact(&id, "metadata.json", metadata.as_bytes())
    .with_artifact(&id, "full.jsonl", full.as_bytes())
}

async fn dump(s: &SqliteStore) -> Vec<Vec<String>> {
    let mut out = Vec::new();
    for sql in DUMPS {
        let rows = s.query(sql, &[]).await.unwrap();
        out.push(rows.iter().map(|r| format!("{r:?}")).collect());
    }
    out
}

#[tokio::test]
async fn a_second_walk_reads_and_projects_only_the_new_shards() {
    let s = store();
    ingest_from_substrate(&s, &branch(&[1, 2, 3]), "repo", "claude-code", 10)
        .await
        .unwrap();

    let grown = CountingReader::new(branch(&[1, 2, 3, 4, 5]));
    let second = ingest_from_substrate(&s, &grown, "repo", "claude-code", 20)
        .await
        .unwrap();

    assert_eq!(
        second.shards_seen, 5,
        "the branch count is still the branch"
    );
    assert_eq!(second.shards_projected, 2);
    assert_eq!(second.shards_skipped, 3);
    let new: BTreeSet<String> = [shard_id(4), shard_id(5)].into();
    assert_eq!(
        grown.opened(),
        new,
        "no artifact of a projected shard is read"
    );
}

#[tokio::test]
async fn a_second_walk_over_an_unchanged_branch_projects_nothing() {
    let s = store();
    ingest_from_substrate(&s, &branch(&[1, 2, 3]), "repo", "claude-code", 10)
        .await
        .unwrap();

    let same = CountingReader::new(branch(&[1, 2, 3]));
    let second = ingest_from_substrate(&s, &same, "repo", "claude-code", 20)
        .await
        .unwrap();

    assert_eq!(second.shards_projected, 0);
    assert_eq!(second.shards_skipped, 3);
    assert!(same.opened().is_empty());
    assert_eq!(
        second.tip,
        Some(shard_id(3)),
        "the watermark still names the tip"
    );
    assert!(
        second.render().contains("0 projected, 3 already projected"),
        "{}",
        second.render()
    );
}

#[tokio::test]
async fn an_incremental_walk_ends_with_the_rows_a_full_walk_writes() {
    let incremental = store();
    ingest_from_substrate(&incremental, &branch(&[1, 2]), "repo", "claude-code", 10)
        .await
        .unwrap();
    ingest_from_substrate(
        &incremental,
        &branch(&[1, 2, 3, 4]),
        "repo",
        "claude-code",
        20,
    )
    .await
    .unwrap();

    let full = store();
    ingest_from_substrate(&full, &branch(&[1, 2, 3, 4]), "repo", "claude-code", 20)
        .await
        .unwrap();

    assert_eq!(dump(&incremental).await, dump(&full).await);
}

/// A shard whose chain entry has not landed sorts after every chained one, so
/// the next chained checkpoint is walked BEFORE the recorded watermark. Position
/// alone would skip it; the missing checkpoint row is what projects it.
#[tokio::test]
async fn a_new_shard_that_sorts_before_the_watermark_is_still_projected() {
    let s = store();
    ingest_from_substrate(&s, &branch(&[1, 9]), "repo", "claude-code", 10)
        .await
        .unwrap();

    let reordered = CountingReader::new(branch(&[1, 2, 9]));
    let second = ingest_from_substrate(&s, &reordered, "repo", "claude-code", 20)
        .await
        .unwrap();

    assert_eq!(second.shards_projected, 1);
    assert_eq!(reordered.opened(), [shard_id(2)].into());
    let full = store();
    ingest_from_substrate(&full, &branch(&[1, 2, 9]), "repo", "claude-code", 20)
        .await
        .unwrap();
    let rows = |d: Vec<Vec<String>>| d.into_iter().skip(1).collect::<Vec<_>>();
    assert_eq!(
        rows(dump(&s).await),
        rows(dump(&full).await),
        "every row but the chain links, which the first walk fixed in its own order"
    );
}

#[tokio::test]
async fn a_watermark_naming_a_shard_no_longer_on_the_branch_means_a_full_walk() {
    let s = store();
    ingest_from_substrate(&s, &branch(&[1, 2, 3]), "repo", "claude-code", 10)
        .await
        .unwrap();

    let rewritten = CountingReader::new(branch(&[1, 2]));
    let second = ingest_from_substrate(&s, &rewritten, "repo", "claude-code", 20)
        .await
        .unwrap();

    assert_eq!(second.shards_projected, 2);
    assert_eq!(second.shards_skipped, 0);
    assert_eq!(rewritten.opened(), [shard_id(1), shard_id(2)].into());
}

#[tokio::test]
async fn a_walk_into_an_empty_projection_projects_every_shard() {
    let s = store();
    let first = CountingReader::new(branch(&[1, 2, 3]));
    let report = ingest_from_substrate(&s, &first, "repo", "claude-code", 10)
        .await
        .unwrap();

    assert_eq!(report.shards_projected, 3);
    assert_eq!(report.shards_skipped, 0);
    assert_eq!(first.opened().len(), 3);
}
