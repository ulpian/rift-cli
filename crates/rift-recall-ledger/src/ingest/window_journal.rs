//! The window rows a rebuild restores: a checkpoint's `window_journal.jsonl`,
//! read into the `n_window_row` relation.
//!
//! A module of its own because these rows are keyed unlike every other row
//! the ingest writes: by the SHA-256 of the frozen line itself, since the
//! artifact is cumulative per session and every later checkpoint carries the
//! earlier rows again. Writing them through the same insert-or-ignore as the
//! graph rows turns that repetition into a set union, so one row lands once
//! whichever checkpoints carry it and in whichever order they are read, and
//! the first checkpoint to carry it is the one its row names.

use rift_core::Result;
use rift_recall_substrate::parse::{self, ParseIssue, WindowJournalRow};
use rift_recall_substrate::SubstrateReader;
use rift_store::Value;

use crate::graph::{file_revision_id, PendingInsert};

/// The artifact the window rows are frozen into.
const ARTIFACT: &str = "window_journal.jsonl";

/// The relation's columns, in the order [`window_row_insert`] writes them.
const COLUMNS: [&str; 19] = [
    "row_id",
    "window_key",
    "subagent",
    "kind",
    "turn",
    "at",
    "path",
    "content_sha256",
    "hash_old",
    "range_lo",
    "range_hi",
    "total_lines",
    "sites",
    "shift",
    "full",
    "reason",
    "file_revision",
    "checkpoint_id",
    "schema_version",
];

/// The `schema_version` every row is written at.
const SCHEMA_VERSION: i64 = 1;

/// Read and validate one checkpoint's frozen window rows as inserts; an
/// absent artifact reads as none, exactly as a shard written before it.
///
/// # Errors
/// Propagates [`SubstrateReader::read_artifact`] read errors.
pub(super) fn read_window_rows(
    reader: &impl SubstrateReader,
    cid: &str,
    issues: &mut Vec<ParseIssue>,
) -> Result<Vec<PendingInsert>> {
    let rows = super::read_parsed(reader, cid, ARTIFACT, parse::parse_window_journal, issues)?;
    let rows = rows.unwrap_or_default();
    Ok(rows.iter().map(|row| window_row_insert(cid, row)).collect())
}

/// The `n_window_row` insert for one frozen row, written by checkpoint `cid`.
fn window_row_insert(cid: &str, row: &WindowJournalRow) -> PendingInsert {
    let text = |value: Option<&str>| value.map_or(Value::Null, Value::text);
    let int = |value: Option<i64>| value.map_or(Value::Null, Value::Int);
    let (range_lo, range_hi) = row.range.unzip();
    let file_revision = match (&row.path, &row.content_sha256) {
        (Some(path), Some(hash)) => Some(file_revision_id(path, hash)),
        _ => None,
    };
    let values = vec![
        Value::text(row.line_sha256.as_str()),
        Value::text(row.window_key.as_str()),
        text(row.subagent.as_deref()),
        Value::text(row.kind.as_str()),
        Value::Int(i64::from(row.turn)),
        Value::Int(row.at),
        text(row.path.as_deref()),
        text(row.content_sha256.as_deref()),
        text(row.hash_old.as_deref()),
        int(range_lo.map(i64::from)),
        int(range_hi.map(i64::from)),
        int(row.total_lines.map(i64::from)),
        text(row.sites.as_deref()),
        int(row.shift),
        int(row.full.map(i64::from)),
        text(row.reason.as_deref()),
        text(file_revision.as_deref()),
        Value::text(cid),
        Value::Int(SCHEMA_VERSION),
    ];
    PendingInsert {
        table: "n_window_row",
        columns: COLUMNS.to_vec(),
        values,
    }
}

#[cfg(test)]
mod tests {
    use rift_recall_substrate::{CheckpointShard, Dialect, MemorySubstrate};
    use rift_store::{recall_migrations, CanonicalStore, SqliteStore};

    use crate::ingest::ingest;

    use super::*;

    fn block<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime")
            .block_on(f)
    }

    fn recall_store() -> SqliteStore {
        let s = SqliteStore::open_in_memory().unwrap();
        rift_store::apply_migrations(&s, &recall_migrations()).unwrap();
        s
    }

    fn shard(id: &str, ord: u64) -> CheckpointShard {
        CheckpointShard {
            checkpoint_id: id.into(),
            dialect: Dialect::Rift,
            prev: None,
            ordinal: ord,
        }
    }

    /// A window key or revision: one hex digit, repeated.
    fn hex(digit: char) -> String {
        digit.to_string().repeat(64)
    }

    /// The canonical lines of one session's window: a read, a decline, an
    /// edit and an eviction.
    fn session_lines() -> Vec<String> {
        let (w, old, new) = (hex('a'), hex('b'), hex('c'));
        vec![
            format!(
                r#"{{"at":1,"hash":"{old}","kind":"read","path":"src/a.rs","range":[1,40],"schema_version":1,"total_lines":40,"turn":1,"window":"{w}"}}"#
            ),
            format!(
                r#"{{"at":2,"hash":"{old}","kind":"deny","path":"src/a.rs","range":[1,40],"schema_version":1,"turn":2,"window":"{w}"}}"#
            ),
            format!(
                r#"{{"at":3,"full":false,"hash_new":"{new}","hash_old":"{old}","kind":"edit","path":"src/a.rs","schema_version":1,"shift":-1,"sites":[[3,4]],"turn":2,"window":"{w}"}}"#
            ),
            format!(
                r#"{{"at":4,"kind":"evict","reason":"compact","schema_version":1,"turn":3,"window":"{w}"}}"#
            ),
        ]
    }

    fn artifact(lines: &[String]) -> Vec<u8> {
        lines
            .iter()
            .flat_map(|line| format!("{line}\n").into_bytes())
            .collect()
    }

    /// `(kind, file_revision, checkpoint_id)` of every window row, by time.
    fn window_rows(store: &SqliteStore) -> Vec<(String, Option<String>, String)> {
        let sql = "SELECT kind, file_revision, checkpoint_id FROM n_window_row ORDER BY at";
        block(store.query(sql, &[]))
            .unwrap()
            .iter()
            .map(|row| {
                let kind = row.get_text(0).unwrap();
                (kind, row.get_opt_text(1).unwrap(), row.get_text(2).unwrap())
            })
            .collect()
    }

    /// One row per line, each on the revision node its line names: a slice
    /// on its hash, an edit on its new revision, an eviction on none.
    #[test]
    fn a_shard_with_the_artifact_writes_one_row_per_line_on_its_revision() {
        let sub = MemorySubstrate::local()
            .with_shard(shard("c1", 1))
            .with_artifact("c1", ARTIFACT, &artifact(&session_lines()));
        let store = recall_store();
        let report = block(ingest(&store, &sub)).unwrap();
        assert!(report.issues.is_empty(), "{:?}", report.issues);
        let old = Some(file_revision_id("src/a.rs", &hex('b')));
        let new = Some(file_revision_id("src/a.rs", &hex('c')));
        let c1 = "c1".to_owned();
        assert_eq!(
            window_rows(&store),
            [
                ("read".to_owned(), old.clone(), c1.clone()),
                ("deny".to_owned(), old, c1.clone()),
                ("edit".to_owned(), new, c1.clone()),
                ("evict".to_owned(), None, c1),
            ]
        );
        let sql =
            "SELECT row_id, sites, shift, full, total_lines FROM n_window_row WHERE kind = 'edit'";
        let edit = block(store.query(sql, &[])).unwrap();
        let line = &session_lines()[2];
        let key = rift_recall_substrate::worktree::sha256_hex(line.as_bytes());
        assert_eq!(edit[0].get_text(0).unwrap(), key);
        assert_eq!(edit[0].get_text(1).unwrap(), "[[3,4]]");
        assert_eq!(edit[0].get_i64(2).unwrap(), -1);
        assert_eq!(edit[0].get_i64(3).unwrap(), 0);
        assert_eq!(edit[0].get_opt_i64(4).unwrap(), None);
    }

    /// A session's second checkpoint carries the first one's rows again and
    /// adds one: each row lands once, under the checkpoint that first carried
    /// it, and a second pass writes nothing.
    #[test]
    fn cumulative_checkpoints_leave_each_row_once_and_a_reingest_writes_nothing() {
        let lines = session_lines();
        let sub = MemorySubstrate::local()
            .with_shard(shard("c1", 1))
            .with_shard(shard("c2", 2))
            .with_artifact("c1", ARTIFACT, &artifact(&lines[..3]))
            .with_artifact("c2", ARTIFACT, &artifact(&lines));
        let store = recall_store();
        let report = block(ingest(&store, &sub)).unwrap();
        assert!(report.issues.is_empty(), "{:?}", report.issues);
        let rows = window_rows(&store);
        let firsts: Vec<&str> = rows.iter().map(|(.., cid)| cid.as_str()).collect();
        assert_eq!(firsts, ["c1", "c1", "c1", "c2"]);

        let cold = recall_store();
        let rebuilt = MemorySubstrate::local()
            .with_shard(shard("c1", 1))
            .with_shard(shard("c2", 2))
            .with_artifact("c1", ARTIFACT, &artifact(&lines[..3]))
            .with_artifact("c2", ARTIFACT, &artifact(&lines));
        block(ingest(&cold, &rebuilt)).unwrap();
        block(ingest(&cold, &rebuilt)).unwrap();
        assert_eq!(window_rows(&cold), rows, "a second pass writes nothing");
        assert_eq!(block(cold.count("n_window_row")).unwrap(), 4);
    }

    /// A checkpoint without the artifact projects the rows it always did and
    /// no window row; a line outside the grammar is an issue, and the lines
    /// around it still land.
    #[test]
    fn a_shard_without_the_artifact_writes_none_and_a_bad_line_is_reported() {
        let tool_calls =
            b"{\"id\":\"t1\",\"name\":\"Read\",\"file_path\":\"a.rs\",\"sha256\":\"h1\",\"input_tokens\":7}\n";
        let mut lines = session_lines();
        lines.insert(1, lines[0].replace("\"read\"", "\"summarize\""));
        let sub = MemorySubstrate::local()
            .with_shard(shard("c1", 1))
            .with_shard(shard("c2", 2))
            .with_artifact("c1", "tool_calls.jsonl", tool_calls)
            .with_artifact("c2", ARTIFACT, &artifact(&lines));
        let store = recall_store();
        let report = block(ingest(&store, &sub)).unwrap();
        assert_eq!(block(store.count("n_file_read")).unwrap(), 1);
        let [issue] = report.issues.as_slice() else {
            panic!("one issue: {:?}", report.issues);
        };
        assert_eq!(
            (issue.checkpoint_id.as_str(), issue.artifact),
            ("c2", ARTIFACT)
        );
        let firsts: Vec<String> = window_rows(&store).into_iter().map(|(.., c)| c).collect();
        assert_eq!(firsts, ["c2", "c2", "c2", "c2"], "none from c1");
    }
}
