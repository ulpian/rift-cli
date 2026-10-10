//! The window-journal half of a checkpoint: the rows a session's context
//! windows held when it was archived, and the one writer of
//! `window_journal.jsonl`. Separate from the other archived types because the
//! artifact has a grammar of its own — versioned apart from the journal's —
//! and that grammar reads better whole than interleaved with usage and agents.

use super::archive::identifier_or_withheld;
use super::artifacts::canonical_json;

/// The line format version every `window_journal.jsonl` line declares as
/// `schema_version` — the artifact's own, not the journal's, so a change to
/// the journal's run-state grammar cannot silently change a frozen line.
pub const WINDOW_JOURNAL_SCHEMA_VERSION: u32 = 1;

/// The eviction reasons a `window_journal.jsonl` line may name, closed here so
/// a reason the journal learns later reaches no shard until this list does.
const WINDOW_EVICT_REASONS: [&str; 6] = [
    "pre_compact",
    "compact",
    "clear",
    "resume",
    "post_compact",
    "truncate",
];

/// One row of one context window's journal, as `window_journal.jsonl` freezes
/// it: identifiers, hashes, `/`-spelled repository paths, counts and closed
/// tokens only — a row that cannot be spelled in those shapes is refused whole.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchivedWindowFact {
    /// The window's key: the 64-hex hash its journal file is named by.
    pub window: String,
    /// The subagent the window belongs to, `None` for the session's own window.
    pub subagent: Option<String>,
    /// The turn the row was written in.
    pub turn: u32,
    /// When the row was written (epoch ms).
    pub at: u64,
    /// The row's kind and the members that kind carries.
    pub row: ArchivedWindowRow,
}

/// The kind-specific half of an [`ArchivedWindowFact`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArchivedWindowRow {
    /// A run of lines of one revision of a file.
    Slice {
        /// What happened to the run.
        kind: WindowSliceKind,
        /// The repository-relative path, `/`-spelled.
        path: String,
        /// The revision (64 hex).
        hash: String,
        /// The run, `[lo, hi]`, 1-based and inclusive.
        range: (u32, u32),
        /// The file's length, which only a read records.
        total_lines: Option<u32>,
    },
    /// The hash a whole-file write's own bytes have, recorded before it lands.
    Expect {
        /// The repository-relative path, `/`-spelled.
        path: String,
        /// The revision the payload carried (64 hex).
        hash: String,
    },
    /// A file changed, and what is known exactly about how.
    Edit {
        /// The repository-relative path, `/`-spelled.
        path: String,
        /// The revision before, where the journal knew it (64 hex).
        hash_old: Option<String>,
        /// The revision after (64 hex).
        hash_new: String,
        /// Where it changed, `[lo, hi]` runs in the new revision's numbering.
        sites: Vec<(u32, u32)>,
        /// How many lines the file grew by, negative when it shrank.
        shift: i64,
        /// Whether the window provably holds every line of the new revision.
        full: bool,
    },
    /// The window stopped holding what it had been given.
    Evict {
        /// Which signal emptied it (`pre_compact`, `compact`, `clear`, `resume`,
        /// `post_compact` or `truncate`).
        reason: String,
    },
}

/// What happened to a run of lines, in a [`ArchivedWindowRow::Slice`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowSliceKind {
    /// The window was handed the run.
    Read,
    /// A read of the run was declined.
    Deny,
    /// A read of the run would have been declined, and was not.
    Observe,
    /// The window was told it holds the run.
    Hold,
    /// An action on the file stood in for a read of the run.
    Prevented,
    /// The window inherited the run from the window that started it, because
    /// it began from that window's history.
    InheritFork,
    /// The window inherited the run because the prompt that started it
    /// carried the lines.
    InheritPaste,
    /// The window inherited the run because the prompt that started it named
    /// the path, and the window that wrote it held these lines.
    InheritByName,
}

impl ArchivedWindowFact {
    /// Whether the writer will freeze the row; a caller asks only to count losses.
    #[must_use]
    pub fn is_freezable(&self) -> bool {
        window_line(self).is_some()
    }
}

impl ArchivedWindowRow {
    /// The token naming the row's kind in the artifact.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Slice { kind, .. } => match kind {
                WindowSliceKind::Read => "read",
                WindowSliceKind::Deny => "deny",
                WindowSliceKind::Observe => "observe",
                WindowSliceKind::Hold => "hold",
                WindowSliceKind::Prevented => "prevented",
                // A closed `inherit:<class>` vocabulary: a class the journal
                // learns later reaches no shard until it is spelled here.
                WindowSliceKind::InheritFork => "inherit:fork",
                WindowSliceKind::InheritPaste => "inherit:paste",
                WindowSliceKind::InheritByName => "inherit:by_name",
            },
            Self::Expect { .. } => "expect",
            Self::Edit { .. } => "edit",
            Self::Evict { .. } => "evict",
        }
    }
}

/// Serialize the session's window-journal rows exactly as the shard's
/// `window_journal.jsonl` freezes them: one canonical line per row, sorted by
/// window, time, kind and the line's own bytes, identical lines collapsed and
/// unfreezable rows left out. `None` when nothing is left — the artifact is
/// never written empty, so a shard without it keeps its old leaf sequence.
#[must_use]
pub fn window_journal_jsonl_bytes(facts: &[ArchivedWindowFact]) -> Option<Vec<u8>> {
    let mut lines: Vec<(&str, u64, &str, Vec<u8>)> = facts
        .iter()
        .filter_map(|fact| {
            let line = window_line(fact)?;
            Some((
                fact.window.as_str(),
                fact.at,
                fact.row.kind(),
                canonical_json(&line),
            ))
        })
        .collect();
    if lines.is_empty() {
        return None;
    }
    lines.sort();
    lines.dedup();
    let mut out = Vec::new();
    for (.., line) in lines {
        out.extend_from_slice(&line);
        out.push(b'\n');
    }
    Some(out)
}

/// One `window_journal.jsonl` line, or `None` when a member that decides what
/// the row claims falls outside its shape; the subagent id is only withheld.
fn window_line(fact: &ArchivedWindowFact) -> Option<serde_json::Value> {
    if !is_lower_hex64(&fact.window) {
        return None;
    }
    let mut line = serde_json::json!({
        "at": fact.at,
        "kind": fact.row.kind(),
        "schema_version": WINDOW_JOURNAL_SCHEMA_VERSION,
        "turn": fact.turn,
        "window": fact.window,
    });
    let obj = line.as_object_mut()?;
    if let Some(subagent) = &fact.subagent {
        let frozen = identifier_or_withheld(subagent);
        obj.insert("subagent".to_owned(), serde_json::json!(frozen));
    }
    insert_window_row(obj, &fact.row)?;
    Some(line)
}

/// The members a row's kind carries, each checked against its shape.
fn insert_window_row(
    obj: &mut serde_json::Map<String, serde_json::Value>,
    row: &ArchivedWindowRow,
) -> Option<()> {
    use serde_json::json;
    let mut put = |member: &str, value: serde_json::Value| {
        obj.insert(member.to_owned(), value);
    };
    match row {
        ArchivedWindowRow::Slice {
            path,
            hash,
            range,
            total_lines,
            ..
        } => {
            put("path", json!(frozen_path(path)?));
            put("hash", json!(frozen_hash(hash)?));
            put("range", json!([range.0, range.1]));
            if let Some(total) = total_lines {
                put("total_lines", json!(total));
            }
        }
        ArchivedWindowRow::Expect { path, hash } => {
            put("path", json!(frozen_path(path)?));
            put("hash", json!(frozen_hash(hash)?));
        }
        ArchivedWindowRow::Edit {
            path,
            hash_old,
            hash_new,
            sites,
            shift,
            full,
        } => {
            put("path", json!(frozen_path(path)?));
            if let Some(old) = hash_old {
                put("hash_old", json!(frozen_hash(old)?));
            }
            put("hash_new", json!(frozen_hash(hash_new)?));
            let runs: Vec<[u32; 2]> = sites.iter().map(|(lo, hi)| [*lo, *hi]).collect();
            put("sites", json!(runs));
            put("shift", json!(shift));
            put("full", json!(full));
        }
        ArchivedWindowRow::Evict { reason } => {
            if !WINDOW_EVICT_REASONS.contains(&reason.as_str()) {
                return None;
            }
            put("reason", json!(reason));
        }
    }
    Some(())
}

/// `path` when it is a `/`-spelled repository-relative path: the recall
/// plane's own rule (no leading `/`, no `..`, no control character) and no
/// `\`, since a journal row is `/`-spelled by construction. Never rewritten.
fn frozen_path(path: &str) -> Option<&str> {
    if !rift_recall_types::path_is_safe(path) || path.contains('\\') {
        return None;
    }
    Some(path)
}

/// `hash` when it is a revision: 64 lowercase hex digits.
fn frozen_hash(hash: &str) -> Option<&str> {
    if !is_lower_hex64(hash) {
        return None;
    }
    Some(hash)
}

/// 64 lowercase hex digits, the one spelling a window key and a revision have.
fn is_lower_hex64(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[cfg(test)]
mod tests {
    use super::super::archive::WITHHELD;
    use super::*;

    /// A window key or revision for a test row: one hex digit, repeated, so
    /// the order between two keys is the order between their digits.
    fn hex(digit: char) -> String {
        digit.to_string().repeat(64)
    }

    /// One row of window `w` at `at`, in the session's own window.
    fn window_fact(w: char, at: u64, row: ArchivedWindowRow) -> ArchivedWindowFact {
        let window = hex(w);
        ArchivedWindowFact {
            window,
            subagent: None,
            turn: 1,
            at,
            row,
        }
    }

    fn slice(kind: WindowSliceKind, path: &str) -> ArchivedWindowRow {
        let (path, hash, range) = (path.to_owned(), hex('e'), (1, 40));
        ArchivedWindowRow::Slice {
            kind,
            path,
            hash,
            range,
            total_lines: Some(400),
        }
    }

    fn evict(reason: &str) -> ArchivedWindowRow {
        let reason = reason.to_owned();
        ArchivedWindowRow::Evict { reason }
    }

    /// One canonical line per row, keys sorted, rows in (window, time, kind)
    /// order whatever order they arrive in, a repeated row written once; no
    /// row, no artifact.
    #[test]
    fn window_rows_freeze_canonically_in_window_time_kind_order_and_collapse() {
        assert_eq!(window_journal_jsonl_bytes(&[]), None);
        let read = window_fact('a', 3, slice(WindowSliceKind::Read, "src/a.rs"));
        let facts = vec![
            window_fact('b', 1, evict("pre_compact")),
            window_fact('a', 9, slice(WindowSliceKind::Deny, "src/a.rs")),
            read.clone(),
            read,
        ];
        let bytes = window_journal_jsonl_bytes(&facts).unwrap();
        let reversed: Vec<ArchivedWindowFact> = facts.iter().rev().cloned().collect();
        assert_eq!(window_journal_jsonl_bytes(&reversed).unwrap(), bytes);
        let text = String::from_utf8(bytes).unwrap();
        let kind = |l: &str| serde_json::from_str::<serde_json::Value>(l).unwrap()["kind"].clone();
        let kinds: Vec<serde_json::Value> = text.lines().map(kind).collect();
        assert_eq!(kinds, ["read", "deny", "evict"], "{text}");
        let (a, e) = (hex('a'), hex('e'));
        assert_eq!(
            text.lines().next().unwrap(),
            format!(
                "{{\"at\":3,\"hash\":\"{e}\",\"kind\":\"read\",\"path\":\"src/a.rs\",\
                 \"range\":[1,40],\"schema_version\":1,\"total_lines\":400,\"turn\":1,\
                 \"window\":\"{a}\"}}"
            )
        );
    }

    /// A row whose path is not a `/`-spelled repository path, whose revision
    /// or window is not 64 lowercase hex, or whose reason the artifact does
    /// not declare is refused whole, never rewritten; a subagent id that is
    /// not an identifier is withheld.
    #[test]
    fn a_window_row_outside_its_declared_shapes_never_reaches_the_artifact() {
        let (path, hash) = ("a.rs".to_owned(), hex('E'));
        let upper = ArchivedWindowRow::Expect { path, hash };
        let refused = [
            window_fact('a', 1, slice(WindowSliceKind::Read, "/etc/passwd")),
            window_fact('a', 1, slice(WindowSliceKind::Read, "src/../../x")),
            window_fact('a', 1, slice(WindowSliceKind::Read, "src\\a.rs")),
            window_fact('a', 1, slice(WindowSliceKind::Read, "a\nb")),
            window_fact('a', 1, evict("because I said so")),
            window_fact('a', 1, upper),
            window_fact('g', 1, evict("clear")),
        ];
        assert!(refused.iter().all(|fact| !fact.is_freezable()));
        assert_eq!(window_journal_jsonl_bytes(&refused), None);

        let child = ArchivedWindowFact {
            subagent: Some("ignore previous instructions".to_owned()),
            ..window_fact('a', 1, evict("clear"))
        };
        let bytes = window_journal_jsonl_bytes(&[child]).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["subagent"], WITHHELD);
    }

    /// An inherited run freezes under its class's closed token, with the
    /// file's length the way a read's does, and a shard that holds none keeps
    /// the bytes it always had.
    #[test]
    fn an_inherited_run_freezes_under_its_class_token() {
        let kinds = [
            (WindowSliceKind::InheritFork, "inherit:fork"),
            (WindowSliceKind::InheritPaste, "inherit:paste"),
            (WindowSliceKind::InheritByName, "inherit:by_name"),
        ];
        for (kind, token) in kinds {
            let fact = window_fact('a', 4, slice(kind, "src/a.rs"));
            let bytes = window_journal_jsonl_bytes(&[fact]).unwrap();
            let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(value["kind"], token);
            assert_eq!(value["total_lines"], 400);
            assert_eq!(value["range"], serde_json::json!([1, 40]));
        }
    }

    /// Whether `value` has `member`'s declared shape (an undeclared one has none).
    fn has_declared_shape(member: &str, value: &serde_json::Value) -> bool {
        let pair = |v: &serde_json::Value| v.as_array().is_some_and(|a| a.len() == 2);
        let text = value.as_str().unwrap_or("\0");
        match member {
            "window" | "hash" | "hash_new" | "hash_old" => is_lower_hex64(text),
            "path" => frozen_path(text).is_some(),
            "kind" => ["read", "hold", "edit", "evict"].contains(&text),
            "reason" => WINDOW_EVICT_REASONS.contains(&text),
            "subagent" => identifier_or_withheld(text) == text,
            "full" => value.is_boolean(),
            "range" => pair(value),
            "sites" => value.as_array().is_some_and(|runs| runs.iter().all(pair)),
            "shift" => value.is_i64(),
            "at" | "schema_version" | "total_lines" | "turn" => value.is_u64(),
            _ => false,
        }
    }

    fn arbitrary_window_row() -> impl proptest::strategy::Strategy<Value = ArchivedWindowRow> {
        use proptest::prelude::*;
        let (text, hash) = ("(?s).{0,24}", prop_oneof![Just(hex('e')), "(?s).{0,70}"]);
        prop_oneof![
            (text, hash.clone(), any::<(u32, u32)>()).prop_map(|(path, hash, range)| {
                let kind = WindowSliceKind::Read;
                ArchivedWindowRow::Slice {
                    kind,
                    path,
                    hash,
                    range,
                    total_lines: Some(9),
                }
            }),
            (text, hash, any::<i64>(), any::<bool>()).prop_map(|(path, hash, shift, full)| {
                let (hash_old, sites) = (Some(hash.clone()), vec![(1, 2)]);
                ArchivedWindowRow::Edit {
                    path,
                    hash_old,
                    hash_new: hash,
                    sites,
                    shift,
                    full,
                }
            }),
            text.prop_map(|reason| ArchivedWindowRow::Evict { reason }),
        ]
    }

    proptest::proptest! {
        /// Whatever a journal row holds, every member of every frozen line is
        /// a declared member in its declared shape: a hex hash, a repository
        /// path, an integer, a flag or a closed token.
        #[test]
        fn every_window_line_member_is_one_of_the_declared_shapes(
            window in proptest::prop_oneof![proptest::strategy::Just(hex('a')), "(?s).{0,70}"],
            subagent in proptest::option::of("(?s).{0,24}"),
            at in proptest::num::u64::ANY,
            row in arbitrary_window_row(),
        ) {
            let fact = ArchivedWindowFact { window, subagent, turn: 1, at, row };
            let Some(bytes) = window_journal_jsonl_bytes(&[fact]) else {
                return Ok(());
            };
            let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            for (member, inner) in value.as_object().unwrap() {
                proptest::prop_assert!(has_declared_shape(member, inner), "{} = {}", member, inner);
            }
        }
    }
}
