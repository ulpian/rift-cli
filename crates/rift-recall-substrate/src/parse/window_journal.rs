//! The reader of `window_journal.jsonl`, the rows a session's context
//! windows held when a checkpoint froze them.
//!
//! A module of its own because its grammar is closed and strict where the
//! other artifacts' readers are lenient: each row is stored under a key
//! derived from its canonical bytes, so a member this reader did not check
//! would reach a relation nothing can correct.

use serde_json::{Map, Value};

use super::{is_window_key, parse_validated_lines, redact_err, Outcome};

/// One line of a checkpoint's `window_journal.jsonl`, validated against the
/// artifact's grammar and flattened into the members the recall relation
/// stores; a member the line's kind does not carry is `None`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WindowJournalRow {
    /// SHA-256 of the line's canonical bytes (keys sorted, no whitespace),
    /// recomputed rather than taken from the bytes as read, so every
    /// checkpoint that carries the row, in whatever spelling, names it alike.
    pub line_sha256: String,
    /// The window's key: 64 lowercase hex.
    pub window_key: String,
    /// The subagent the window belongs to, `None` for the session's own.
    pub subagent: Option<String>,
    /// One of the eleven kinds the artifact declares: the eight a window's
    /// own rows take, and the three `inherit:<class>` runs it was handed by
    /// the window that started it.
    pub kind: String,
    /// The turn the row was written in.
    pub turn: u32,
    /// When the row was written (epoch ms).
    pub at: i64,
    /// The `/`-spelled repository path, on every kind but `evict`.
    pub path: Option<String>,
    /// The revision a slice or an expect names, or an edit's `hash_new`.
    pub content_sha256: Option<String>,
    /// An edit's revision before, where the journal knew it.
    pub hash_old: Option<String>,
    /// A slice's `[lo, hi]` run.
    pub range: Option<(u32, u32)>,
    /// The file's length, which only a read records.
    pub total_lines: Option<u32>,
    /// An edit's `[lo, hi]` runs, as their canonical JSON array text.
    pub sites: Option<String>,
    /// How many lines an edit grew the file by, negative when it shrank.
    pub shift: Option<i64>,
    /// Whether an edit left the window holding every line of the new revision.
    pub full: Option<bool>,
    /// Which signal emptied the window, on an `evict`.
    pub reason: Option<String>,
}

/// Parse `window_journal.jsonl`: one frozen context-window row per line.
///
/// Strict where the other readers here are lenient, because the artifact's
/// grammar is closed and each row is stored under a key derived from it: a
/// line with a member its kind does not declare, another `schema_version`,
/// an undeclared kind or eviction reason, a hash that is not 64 lowercase
/// hex, or a path that is not a `/`-spelled repository path is one issue for
/// that line, and the walk goes on.
#[must_use]
pub fn parse_window_journal(bytes: &[u8], checkpoint_id: &str) -> Outcome<Vec<WindowJournalRow>> {
    parse_validated_lines(
        bytes,
        "window_journal.jsonl",
        checkpoint_id,
        window_journal_row,
    )
}

/// One line of `window_journal.jsonl` validated, or why not — in words of
/// its shape only, never a byte of the line.
fn window_journal_row(line: &[u8]) -> Result<WindowJournalRow, String> {
    const HEAD: [&str; 6] = ["at", "kind", "schema_version", "subagent", "turn", "window"];
    let line = match serde_json::from_slice::<Map<String, Value>>(line) {
        Ok(line) => line,
        Err(err) => return Err(redact_err(&err)),
    };
    let Some(kind) = line.get("kind").and_then(Value::as_str) else {
        return Err("no string kind".to_owned());
    };
    let Some(members) = window_kind_members(kind) else {
        return Err("a kind the artifact does not declare".to_owned());
    };
    let declared = |member: &str| HEAD.contains(&member) || members.contains(&member);
    if line.keys().any(|member| !declared(member)) {
        return Err("a member the row's kind does not declare".to_owned());
    }
    if line.get("schema_version").and_then(Value::as_u64) != Some(1) {
        return Err("not a schema-1 window row".to_owned());
    }
    let Ok(at) = i64::try_from(u64_member(&line, "at")?) else {
        return Err("an at past the integer range".to_owned());
    };
    let canonical = serde_json::to_vec(&line).map_err(|err| redact_err(&err))?;
    let mut row = WindowJournalRow {
        line_sha256: crate::worktree::sha256_hex(&canonical),
        window_key: hash_member(&line, "window")?,
        subagent: subagent_member(&line)?,
        kind: kind.to_owned(),
        turn: u32_member(&line, "turn")?,
        at,
        ..WindowJournalRow::default()
    };
    put_window_kind_members(&line, &mut row)?;
    Ok(row)
}

/// The members a window row's kind carries beside the five every row has,
/// `None` for a kind the artifact does not declare. A `read` and an inherited
/// run alone may carry `total_lines`, and an `edit` may lack `hash_old`.
fn window_kind_members(kind: &str) -> Option<&'static [&'static str]> {
    match kind {
        "read" | "inherit:fork" | "inherit:paste" | "inherit:by_name" => {
            Some(&["hash", "path", "range", "total_lines"])
        }
        "deny" | "observe" | "hold" | "prevented" => Some(&["hash", "path", "range"]),
        "expect" => Some(&["hash", "path"]),
        "edit" => Some(&["full", "hash_new", "hash_old", "path", "shift", "sites"]),
        "evict" => Some(&["reason"]),
        _ => None,
    }
}

/// Fill `row` with its kind's members, each checked against its shape.
fn put_window_kind_members(
    line: &Map<String, Value>,
    row: &mut WindowJournalRow,
) -> Result<(), String> {
    const EVICT_REASONS: [&str; 6] = [
        "pre_compact",
        "compact",
        "clear",
        "resume",
        "post_compact",
        "truncate",
    ];
    match row.kind.as_str() {
        "evict" => match line.get("reason").and_then(Value::as_str) {
            Some(reason) if EVICT_REASONS.contains(&reason) => row.reason = Some(reason.to_owned()),
            _ => return Err("a reason the artifact does not declare".to_owned()),
        },
        "edit" => {
            row.path = Some(path_member(line)?);
            row.content_sha256 = Some(hash_member(line, "hash_new")?);
            if line.contains_key("hash_old") {
                row.hash_old = Some(hash_member(line, "hash_old")?);
            }
            row.sites = Some(sites_member(line)?);
            let Some(shift) = line.get("shift").and_then(Value::as_i64) else {
                return Err("no integer shift".to_owned());
            };
            let Some(full) = line.get("full").and_then(Value::as_bool) else {
                return Err("no boolean full".to_owned());
            };
            (row.shift, row.full) = (Some(shift), Some(full));
        }
        kind => {
            row.path = Some(path_member(line)?);
            row.content_sha256 = Some(hash_member(line, "hash")?);
            if kind == "expect" {
                return Ok(());
            }
            row.range = Some(run_of(line.get("range"))?);
            if line.contains_key("total_lines") {
                row.total_lines = Some(u32_member(line, "total_lines")?);
            }
        }
    }
    Ok(())
}

/// A hash member: 64 lowercase hex, the one spelling a window key and a
/// revision have.
fn hash_member(line: &Map<String, Value>, member: &str) -> Result<String, String> {
    match line.get(member).and_then(Value::as_str) {
        Some(hash) if is_window_key(hash) => Ok(hash.to_owned()),
        _ => Err(format!("a {member} that is not 64 lowercase hex")),
    }
}

/// The row's path: the recall plane's safe-path rule and no `\`, since the
/// writer freezes only `/`-spelled paths and a rewritten one would name
/// another file.
fn path_member(line: &Map<String, Value>) -> Result<String, String> {
    match line.get("path").and_then(Value::as_str) {
        Some(path) if rift_recall_types::path_is_safe(path) && !path.contains('\\') => {
            Ok(path.to_owned())
        }
        _ => Err("a path that is not a `/`-spelled repository path".to_owned()),
    }
}

/// The subagent a row names, when it names one: the writer's identifier
/// grammar — at most 128 bytes of ASCII letters, digits and
/// `-._:/@+[]`, or the one bracketed literal it admits — restated because
/// this crate depends on no capture crate.
fn subagent_member(line: &Map<String, Value>) -> Result<Option<String>, String> {
    let is_identifier_char = |c: char| {
        c.is_ascii_alphanumeric()
            || matches!(c, '-' | '.' | '_' | ':' | '/' | '@' | '+' | '[' | ']')
    };
    match line.get("subagent") {
        None => Ok(None),
        Some(Value::String(id)) if id == "<synthetic>" => Ok(Some(id.clone())),
        Some(Value::String(id)) if id.len() <= 128 && id.chars().all(is_identifier_char) => {
            Ok(Some(id.clone()))
        }
        Some(_) => Err("a subagent that is not an identifier".to_owned()),
    }
}

/// An unsigned integer member.
fn u64_member(line: &Map<String, Value>, member: &str) -> Result<u64, String> {
    match line.get(member).and_then(Value::as_u64) {
        Some(n) => Ok(n),
        None => Err(format!("no unsigned integer {member}")),
    }
}

/// An unsigned integer member that fits a line number or a turn.
fn u32_member(line: &Map<String, Value>, member: &str) -> Result<u32, String> {
    match u32::try_from(u64_member(line, member)?) {
        Ok(n) => Ok(n),
        Err(_) => Err(format!("a {member} past a line number's range")),
    }
}

/// One `[lo, hi]` run of line numbers.
fn run_of(value: Option<&Value>) -> Result<(u32, u32), String> {
    let line_number = |v: &Value| v.as_u64().and_then(|n| u32::try_from(n).ok());
    match value.and_then(Value::as_array).map(Vec::as_slice) {
        Some([lo, hi]) => match (line_number(lo), line_number(hi)) {
            (Some(lo), Some(hi)) => Ok((lo, hi)),
            _ => Err("a run whose ends are not line numbers".to_owned()),
        },
        _ => Err("a run that is not a [lo, hi] pair".to_owned()),
    }
}

/// An edit's runs, re-serialized as the canonical JSON array the relation stores.
fn sites_member(line: &Map<String, Value>) -> Result<String, String> {
    let Some(sites) = line.get("sites").and_then(Value::as_array) else {
        return Err("no sites array".to_owned());
    };
    let runs: Vec<[u32; 2]> = sites
        .iter()
        .map(|run| run_of(Some(run)).map(|(lo, hi)| [lo, hi]))
        .collect::<Result<_, _>>()?;
    serde_json::to_string(&runs).map_err(|err| redact_err(&err))
}

#[cfg(test)]
mod tests {
    use super::super::{IssueKind, Locator};
    use super::*;

    /// The every-artifact golden's window journal: a read, a decline, an
    /// edit and an eviction in the session's own window, and a subagent's read.
    const GOLDEN_WINDOW_JOURNAL: &str = r#"{"at":1755999992000,"hash":"2939352b9e838fea4ad4cf9696c68140c0fd433e27c6a1c69b70bf8995078880","kind":"read","path":"src/lib.rs","range":[1,120],"schema_version":1,"total_lines":120,"turn":1,"window":"7b850827a5b91be1c41f15dc1cc2f5ba78ef1c5d5127c3ad9099077b29321fe5"}
{"at":1755999993500,"hash":"2939352b9e838fea4ad4cf9696c68140c0fd433e27c6a1c69b70bf8995078880","kind":"deny","path":"src/lib.rs","range":[1,120],"schema_version":1,"turn":2,"window":"7b850827a5b91be1c41f15dc1cc2f5ba78ef1c5d5127c3ad9099077b29321fe5"}
{"at":1755999994000,"full":true,"hash_new":"f83b559b04cf34c5db167460856714628ec81fcbb3b8ee35fde311aaab4a3767","hash_old":"2939352b9e838fea4ad4cf9696c68140c0fd433e27c6a1c69b70bf8995078880","kind":"edit","path":"src/lib.rs","schema_version":1,"shift":2,"sites":[[10,12]],"turn":2,"window":"7b850827a5b91be1c41f15dc1cc2f5ba78ef1c5d5127c3ad9099077b29321fe5"}
{"at":1755999997000,"kind":"evict","reason":"pre_compact","schema_version":1,"turn":3,"window":"7b850827a5b91be1c41f15dc1cc2f5ba78ef1c5d5127c3ad9099077b29321fe5"}
{"at":1755999995000,"hash":"2939352b9e838fea4ad4cf9696c68140c0fd433e27c6a1c69b70bf8995078880","kind":"read","path":"src/lib.rs","range":[1,40],"schema_version":1,"subagent":"explore-1","total_lines":120,"turn":2,"window":"ae15905ad1c8bd3482557992083aa5c1f8e7fcd42b9e15848a15a420e2110262"}
"#;

    /// The golden's decline line with its kind changed, and the expect line
    /// a whole-file write leaves: the kinds the golden does not carry.
    fn other_kind_lines() -> Vec<String> {
        let deny = GOLDEN_WINDOW_JOURNAL.lines().nth(1).unwrap();
        let mut lines: Vec<String> = ["observe", "hold", "prevented"]
            .iter()
            .map(|kind| deny.replace("\"deny\"", &format!("\"{kind}\"")))
            .collect();
        let mut expect: serde_json::Value = serde_json::from_str(deny).unwrap();
        let members = expect.as_object_mut().unwrap();
        members.remove("range");
        members.insert("kind".to_owned(), "expect".into());
        lines.push(expect.to_string());
        lines
    }

    /// Every kind parses into the members its line carries, keyed by the
    /// SHA-256 of the line's own canonical bytes, and a line spelled in
    /// another key order or spacing lands on the same key.
    #[test]
    fn a_golden_line_of_every_kind_parses_keyed_by_its_canonical_bytes() {
        let out = parse_window_journal(GOLDEN_WINDOW_JOURNAL.as_bytes(), "cp");
        assert!(out.is_clean(), "{:?}", out.issues);
        let kinds: Vec<&str> = out.value.iter().map(|row| row.kind.as_str()).collect();
        assert_eq!(kinds, ["read", "deny", "edit", "evict", "read"]);
        for (row, line) in out.value.iter().zip(GOLDEN_WINDOW_JOURNAL.lines()) {
            assert_eq!(
                row.line_sha256,
                crate::worktree::sha256_hex(line.as_bytes())
            );
        }
        let [read, deny, edit, evict, child] = out.value.as_slice() else {
            panic!("five rows: {:?}", out.value);
        };
        assert_eq!(read.range, Some((1, 120)));
        assert_eq!(read.total_lines, Some(120));
        assert_eq!(read.at, 1_755_999_992_000);
        assert_eq!(deny.total_lines, None);
        assert_eq!(deny.path.as_deref(), Some("src/lib.rs"));
        assert_eq!(edit.content_sha256.as_deref().map(str::len), Some(64));
        assert_eq!(edit.hash_old, read.content_sha256);
        assert_eq!(edit.sites.as_deref(), Some("[[10,12]]"));
        assert_eq!((edit.shift, edit.full), (Some(2), Some(true)));
        assert_eq!(evict.reason.as_deref(), Some("pre_compact"));
        assert_eq!(evict.path, None);
        assert_eq!(child.subagent.as_deref(), Some("explore-1"));
        assert_eq!(read.subagent, None);

        let reordered: serde_json::Value =
            serde_json::from_str(GOLDEN_WINDOW_JOURNAL.lines().next().unwrap()).unwrap();
        let spaced = serde_json::to_string_pretty(&reordered)
            .unwrap()
            .replace('\n', " ");
        let again = parse_window_journal(spaced.as_bytes(), "cp");
        assert_eq!(again.value, vec![read.clone()]);

        let others = other_kind_lines().join("\n");
        let out = parse_window_journal(others.as_bytes(), "cp");
        assert!(out.is_clean(), "{:?}", out.issues);
        let kinds: Vec<&str> = out.value.iter().map(|row| row.kind.as_str()).collect();
        assert_eq!(kinds, ["observe", "hold", "prevented", "expect"]);
        assert_eq!(out.value[3].range, None);
    }

    /// An inherited run parses under its class's token with the members a
    /// read carries, exactly as the writer freezes it; a class the artifact
    /// does not declare, or an inherited row with a member its kind does not
    /// declare, is refused like any other.
    #[test]
    fn an_inherited_run_parses_and_an_undeclared_member_on_one_is_refused() {
        let read = GOLDEN_WINDOW_JOURNAL.lines().next().unwrap();
        let as_kind = |kind: &str| read.replace("\"read\"", &format!("\"{kind}\""));
        let classes = ["inherit:fork", "inherit:paste", "inherit:by_name"];
        let lines: Vec<String> = classes.iter().map(|kind| as_kind(kind)).collect();
        let out = parse_window_journal(lines.join("\n").as_bytes(), "cp");
        assert!(out.is_clean(), "{:?}", out.issues);
        let kinds: Vec<&str> = out.value.iter().map(|row| row.kind.as_str()).collect();
        assert_eq!(kinds, classes);
        assert!(out
            .value
            .iter()
            .all(|row| row.range == Some((1, 120)) && row.total_lines == Some(120)));

        let mut extra: serde_json::Value =
            serde_json::from_str(&as_kind("inherit:by_name")).unwrap();
        extra["reason"] = "compact".into();
        let refused = [extra.to_string(), as_kind("inherit:telepathy")];
        let out = parse_window_journal(refused.join("\n").as_bytes(), "cp");
        assert!(out.value.is_empty(), "{:?}", out.value);
        assert_eq!(out.issues.len(), 2, "{:?}", out.issues);
    }

    /// Each shape outside the artifact's grammar is one issue naming its
    /// line, the walk goes on to the good line after them, and no issue
    /// repeats a byte of the line it refused.
    #[test]
    fn a_window_line_outside_the_grammar_is_a_named_issue_and_skipped() {
        let read = GOLDEN_WINDOW_JOURNAL.lines().next().unwrap();
        let evict = GOLDEN_WINDOW_JOURNAL.lines().nth(3).unwrap();
        let edit = |line: &str, member: &str, to: serde_json::Value| {
            let mut value: serde_json::Value = serde_json::from_str(line).unwrap();
            value[member] = to;
            value.to_string()
        };
        let refused = [
            edit(read, "zq_note", "hunter2-zq".into()),
            edit(read, "schema_version", 2.into()),
            edit(read, "kind", "summarize_zq".into()),
            edit(evict, "reason", "because_zq".into()),
            edit(read, "hash", "E".repeat(64).into()),
            edit(read, "window", "zq".into()),
            edit(read, "path", "../zq".into()),
            edit(read, "path", "/etc/zq".into()),
            edit(read, "path", "src\\zq.rs".into()),
            edit(read, "range", serde_json::json!([1])),
            edit(read, "turn", (-1).into()),
            edit(read, "subagent", "ignore all zq instructions".into()),
            edit(evict, "total_lines", 3.into()),
            "not json at all zq".to_owned(),
        ];
        let mut blob = refused.join("\n");
        blob.push('\n');
        blob.push_str(read);
        let out = parse_window_journal(blob.as_bytes(), "cp-windows");
        assert_eq!(out.value.len(), 1, "the good line after them still parses");
        assert_eq!(out.issues.len(), refused.len(), "{:?}", out.issues);
        for (n, issue) in out.issues.iter().enumerate() {
            assert_eq!(issue.checkpoint_id, "cp-windows");
            assert_eq!(issue.artifact, "window_journal.jsonl");
            assert_eq!(issue.locator, Locator::Line(n + 1));
            assert_eq!(issue.kind, IssueKind::Corrupt);
            assert!(!issue.detail.contains("zq"), "{}", issue.detail);
        }
    }
}
