//! Transcript records as a conversation: each record becomes the items a
//! reader scans for — a prompt, the agent's words, a folded file change, a
//! tool call, a Rift recall — instead of one undifferentiated block of text.
//!
//! Only explicit text leaves and recognised tool-call fields are shown. An
//! argument the compactor elided is reported by the size it recorded and is
//! never reconstructed, and every string passes the terminal sanitiser on its
//! way out, because all of it was written by an agent or a tool.

use rift_canopy::{ViewerItemTone, ViewerLine, ViewerLineKind, ViewerTimelineItem};
use rift_recall_types::checkpoint::CompactTranscriptRecord;
use serde_json::{Map, Value};

use super::text::bounded;

/// Longest argument shown beside a tool's name in its headline.
const HEADLINE_CHARS: usize = 96;
/// A result this short stays open; a longer one starts folded.
const OPEN_RESULT_LINES: usize = 4;
/// Ceiling on one change's unfolded body, so a call carrying hundreds of
/// edits cannot make a single item unbounded.
const MAX_CHANGE_LINES: usize = 600;

/// The argument names the supported agents use for the file a tool edits.
const PATH_KEYS: [&str; 6] = [
    "file_path",
    "path",
    "filePath",
    "absolute_path",
    "notebook_path",
    "target_file",
];
/// The argument that best says what a call did, in order of preference.
const HEADLINE_KEYS: [&str; 9] = [
    "command",
    "cmd",
    "file_path",
    "path",
    "pattern",
    "query",
    "url",
    "description",
    "prompt",
];
/// Text the harness writes into a user record; it is not something the
/// person typed, so it must not read as a prompt.
const HARNESS_OPENERS: [&str; 5] = [
    "<system-reminder>",
    "<local-command",
    "<command-",
    "<task-notification>",
    "[SYSTEM NOTIFICATION",
];

/// Where a record sits in its transcript: what its items need besides its content.
pub(super) struct RecordPlace {
    /// Ordinal of the record among the parsed records; the stem of every item id.
    pub(super) index: usize,
    /// The record's own timestamp, already formatted.
    pub(super) timestamp: Option<String>,
    /// The record precedes the checkpoint-new boundary.
    pub(super) before_boundary: bool,
}

/// One item before it is given an id and a place.
struct Draft {
    tone: ViewerItemTone,
    title: String,
    body: Vec<ViewerLine>,
    collapsed: bool,
}

/// One displayable leaf of a record's content, in stored order.
enum Part<'a> {
    Text(&'a str),
    ToolUse(&'a Map<String, Value>),
    ToolResult(&'a Map<String, Value>),
}

/// A file edit as the lines it removes and adds.
#[derive(Default)]
struct Change {
    body: Vec<ViewerLine>,
    added: u64,
    removed: u64,
    dropped: usize,
}

impl Change {
    fn push(&mut self, kind: ViewerLineKind, text: String) {
        if self.body.len() >= MAX_CHANGE_LINES {
            self.dropped += 1;
            return;
        }
        self.body.push(ViewerLine::plain(kind, text));
    }

    /// One side of an edit. A string is shown line by line; an elision handle
    /// contributes its recorded line count and a row saying so.
    fn side(&mut self, value: Option<&Value>, kind: ViewerLineKind) {
        let Some(value) = value else {
            return;
        };
        let mut count = 0_u64;
        match (value.as_str(), elision(value)) {
            (Some(text), _) => {
                for line in text.lines() {
                    self.push(kind, bounded(line));
                    count = count.saturating_add(1);
                }
            }
            (None, Some((chars, lines))) => {
                self.push(
                    ViewerLineKind::Gap,
                    format!("{lines} lines ({chars} chars) not kept in the compact transcript"),
                );
                count = lines;
            }
            (None, None) => {}
        }
        // Sizes in an elision handle are agent-written numbers: never let one
        // overflow the counter it is added to.
        match kind {
            ViewerLineKind::Removal => self.removed = self.removed.saturating_add(count),
            _ => self.added = self.added.saturating_add(count),
        }
    }

    fn into_draft(mut self, verb: &str, path: &str) -> Draft {
        if self.dropped > 0 {
            self.body.push(ViewerLine::plain(
                ViewerLineKind::Gap,
                format!("{} more lines not shown", self.dropped),
            ));
        }
        Draft {
            tone: ViewerItemTone::CodeChange,
            title: format!(
                "{verb} {}  +{} -{}",
                bounded(path),
                self.added,
                self.removed
            ),
            body: self.body,
            collapsed: true,
        }
    }
}

/// Every item one transcript record becomes. The first carries the bare
/// record id and the timestamp; a record with nothing displayable still
/// yields one item, so no stored record is silently absent from the list.
pub(super) fn record_items(
    place: &RecordPlace,
    record: &CompactTranscriptRecord,
) -> Vec<ViewerTimelineItem> {
    let role = record.record_type.as_deref().unwrap_or("unknown record");
    let content = if record.content.is_null() {
        record.message.get("content").unwrap_or(&record.message)
    } else {
        &record.content
    };
    let mut parts = Vec::new();
    collect_parts(content, &mut parts);
    let mut drafts = Vec::new();
    let mut speech = Speech::default();
    for part in parts {
        match part {
            Part::Text(text) if text.trim().is_empty() => {}
            Part::Text(text) => speech.say(role, text, &mut drafts),
            Part::ToolUse(call) => {
                speech.flush(&mut drafts);
                drafts.push(tool_call(call));
            }
            Part::ToolResult(result) => {
                speech.flush(&mut drafts);
                drafts.push(tool_result(result));
            }
        }
    }
    speech.flush(&mut drafts);
    if drafts.is_empty() {
        drafts.push(Draft {
            tone: ViewerItemTone::Neutral,
            title: bounded(role),
            body: vec![ViewerLine::plain(
                ViewerLineKind::Text,
                "structured record; no displayable text",
            )],
            collapsed: false,
        });
    }
    drafts
        .into_iter()
        .enumerate()
        .map(|(ordinal, draft)| placed(place, ordinal, draft))
        .collect()
}

fn placed(place: &RecordPlace, ordinal: usize, draft: Draft) -> ViewerTimelineItem {
    let index = place.index;
    ViewerTimelineItem {
        id: if ordinal == 0 {
            format!("transcript:{index}")
        } else {
            format!("transcript:{index}:{ordinal}")
        },
        title: draft.title,
        timestamp: place.timestamp.clone().filter(|_| ordinal == 0),
        summary: Vec::new(),
        body: draft.body,
        expanded_body: None,
        collapsed: draft.collapsed,
        full_session_only: place.before_boundary,
        tone: draft.tone,
    }
}

/// The same leaves `record_text` selects, kept apart so a tool call can be
/// drawn as a call instead of as a sentence about one.
fn collect_parts<'a>(value: &'a Value, out: &mut Vec<Part<'a>>) {
    match value {
        Value::String(text) => out.push(Part::Text(text)),
        Value::Array(values) => {
            for value in values {
                collect_parts(value, out);
            }
        }
        Value::Object(object) => match object.get("type").and_then(Value::as_str) {
            Some("tool_use") => out.push(Part::ToolUse(object)),
            Some("tool_result") => out.push(Part::ToolResult(object)),
            _ => {
                if let Some(text) = object.get("text").and_then(Value::as_str) {
                    out.push(Part::Text(text));
                }
            }
        },
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

/// A run of consecutive text leaves that share one voice. A user record
/// routinely carries a harness reminder beside what the person typed, so each
/// leaf is judged on its own and a change of voice closes the run: a reminder
/// never borrows the prompt's colour, and a prompt is never dimmed as a note.
#[derive(Default)]
struct Speech<'a> {
    voice: Option<(ViewerItemTone, String)>,
    texts: Vec<&'a str>,
}

impl<'a> Speech<'a> {
    fn say(&mut self, role: &str, text: &'a str, drafts: &mut Vec<Draft>) {
        let voice = speaker(role, text);
        if self.voice.as_ref() != Some(&voice) {
            self.flush(drafts);
            self.voice = Some(voice);
        }
        self.texts.push(text);
    }

    fn flush(&mut self, drafts: &mut Vec<Draft>) {
        let Some((tone, title)) = self.voice.take() else {
            return;
        };
        let text = bounded(&self.texts.join("\n"));
        self.texts.clear();
        drafts.push(Draft {
            tone,
            title,
            body: text
                .lines()
                .map(|line| ViewerLine::plain(ViewerLineKind::Text, bounded(line)))
                .collect(),
            collapsed: false,
        });
    }
}

/// Who a run of text belongs to. A user record is a prompt only when the
/// person wrote it: hook answers and harness notes arrive in the same role.
fn speaker(role: &str, text: &str) -> (ViewerItemTone, String) {
    if is_recall_text(text) {
        return (ViewerItemTone::Recall, "rift recall".to_owned());
    }
    let opening = text.trim_start();
    match role.to_ascii_lowercase().as_str() {
        "user" | "human" if HARNESS_OPENERS.iter().any(|tag| opening.starts_with(tag)) => {
            (ViewerItemTone::System, "harness note".to_owned())
        }
        "user" | "human" => (ViewerItemTone::Prompt, "prompt".to_owned()),
        "assistant" | "model" | "agent" => (ViewerItemTone::Assistant, "assistant".to_owned()),
        "system" | "developer" => (ViewerItemTone::System, bounded(role)),
        _ => (ViewerItemTone::Neutral, bounded(role)),
    }
}

/// Text Rift itself put in front of the agent: a hook answer or the session
/// pointer. Matched on the frames the hooks write, so a person quoting the
/// word "rift" in a prompt is still a prompt.
fn is_recall_text(text: &str) -> bool {
    let opening = text.trim_start();
    opening.starts_with("[rift]")
        || opening.contains("hook error: [rift]")
        || opening.contains("hook additional context: [rift]")
        || opening.contains("hook additional context: Rift is capturing")
}

fn is_recall_command(command: &str) -> bool {
    let command = command.trim_start();
    command == "rift"
        || command.starts_with("rift ")
        || ["rift recall ", "rift why ", "rift agent-help"]
            .iter()
            .any(|verb| command.contains(verb))
}

fn tool_call(call: &Map<String, Value>) -> Draft {
    let name = call.get("name").and_then(Value::as_str).unwrap_or("tool");
    let no_input = Map::new();
    let input = call
        .get("input")
        .and_then(Value::as_object)
        .unwrap_or(&no_input);
    if let Some(change) = code_change(name, input) {
        return change;
    }
    let headline = HEADLINE_KEYS
        .iter()
        .find_map(|key| Some((*key, input.get(*key)?.as_str()?)));
    let recall = headline.is_some_and(|(_, text)| is_recall_command(text));
    let body = argument_lines(input, headline);
    let title = match headline {
        Some((_, text)) if recall => first_line(text),
        Some((_, text)) => format!("{} {}", bounded(name), first_line(text)),
        None => bounded(name),
    };
    Draft {
        tone: if recall {
            ViewerItemTone::Recall
        } else {
            ViewerItemTone::Tool
        },
        title,
        collapsed: !body.is_empty(),
        body,
    }
}

/// A call's remaining arguments, one fact per line. The headline argument is
/// left out when the headline already shows all of it.
fn argument_lines(input: &Map<String, Value>, headline: Option<(&str, &str)>) -> Vec<ViewerLine> {
    let mut lines = Vec::new();
    for (key, value) in input {
        let shown_in_headline = headline.is_some_and(|(shown, text)| {
            shown == key && !text.contains('\n') && text.chars().count() <= HEADLINE_CHARS
        });
        if shown_in_headline {
            continue;
        }
        let label = bounded(key);
        match (value.as_str(), elision(value)) {
            (Some(text), _) => {
                let mut rows = text.lines();
                let first = rows.next().unwrap_or_default();
                lines.push(dim(format!("{label}: {}", bounded(first))));
                lines.extend(rows.map(|row| dim(format!("  {}", bounded(row)))));
            }
            (None, Some((chars, rows))) => lines.push(dim(format!(
                "{label}: {chars} chars on {rows} lines, not kept in the compact transcript"
            ))),
            (None, None) => lines.push(dim(format!(
                "{label}: {}",
                clipped(&bounded(&value.to_string()))
            ))),
        }
    }
    lines
}

/// A tool call that edits a file, as a folded change: Claude Code's `Edit`,
/// `MultiEdit` and `Write`, the same shapes under other agents' names, and a
/// patch tool's patch text. `None` for every other call.
fn code_change(name: &str, input: &Map<String, Value>) -> Option<Draft> {
    let text_of = |keys: &[&str]| keys.iter().find_map(|key| input.get(*key));
    let path = PATH_KEYS
        .iter()
        .find_map(|key| input.get(*key)?.as_str())
        .unwrap_or("(file not named)");
    let mut change = Change::default();
    if let Some(edits) = input.get("edits").and_then(Value::as_array) {
        for (ordinal, edit) in edits.iter().filter_map(Value::as_object).enumerate() {
            if edits.len() > 1 {
                change.push(
                    ViewerLineKind::DiffHunk,
                    format!("change {} of {}", ordinal + 1, edits.len()),
                );
            }
            change.side(
                edit.get("old_string").or_else(|| edit.get("old_str")),
                ViewerLineKind::Removal,
            );
            change.side(
                edit.get("new_string").or_else(|| edit.get("new_str")),
                ViewerLineKind::Addition,
            );
        }
        return Some(change.into_draft("edit", path));
    }
    let old = text_of(&["old_string", "old_str"]);
    let new = text_of(&["new_string", "new_str"]);
    if old.is_some() || new.is_some() {
        change.side(old, ViewerLineKind::Removal);
        change.side(new, ViewerLineKind::Addition);
        return Some(change.into_draft("edit", path));
    }
    let named = PATH_KEYS.iter().any(|key| input.contains_key(*key));
    if let Some(content) = text_of(&["content", "contents", "file_text"]).filter(|_| named) {
        change.side(Some(content), ViewerLineKind::Addition);
        return Some(change.into_draft("write", path));
    }
    if name.to_ascii_lowercase().contains("patch") {
        return Some(patch_change(text_of(&["patch", "input", "diff"])));
    }
    None
}

/// A patch tool's text, read as the diff it already is: file headers name the
/// files, `+` and `-` rows are the change, everything else is context.
fn patch_change(patch: Option<&Value>) -> Draft {
    let mut change = Change::default();
    let mut file = None;
    let Some(text) = patch.and_then(Value::as_str) else {
        change.side(patch, ViewerLineKind::Addition);
        return change.into_draft("patch", "(files not kept)");
    };
    for line in text.lines() {
        let header = ["*** Update File: ", "*** Add File: ", "*** Delete File: "]
            .iter()
            .find_map(|prefix| line.strip_prefix(prefix));
        if let Some(path) = header {
            file.get_or_insert(path);
            change.push(ViewerLineKind::DiffFile, bounded(line));
        } else if let Some(added) = line.strip_prefix('+') {
            change.push(ViewerLineKind::Addition, bounded(added));
            change.added = change.added.saturating_add(1);
        } else if let Some(removed) = line.strip_prefix('-') {
            change.push(ViewerLineKind::Removal, bounded(removed));
            change.removed = change.removed.saturating_add(1);
        } else if line.starts_with("@@") {
            change.push(ViewerLineKind::DiffHunk, bounded(line));
        } else if !line.starts_with("***") {
            change.push(ViewerLineKind::Context, bounded(line));
        }
    }
    change.into_draft("patch", file.unwrap_or("(files not named)"))
}

fn tool_result(result: &Map<String, Value>) -> Draft {
    let failed = result
        .get("is_error")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let mut texts = Vec::new();
    let mut elided = (0_u64, 0_u64);
    if let Some(payload) = result.get("result").or_else(|| result.get("content")) {
        collect_result(payload, &mut texts, &mut elided);
    }
    let text = bounded(&texts.join("\n"));
    let recall = is_recall_text(&text);
    let kind = if failed && !recall {
        ViewerLineKind::Warning
    } else {
        ViewerLineKind::Dim
    };
    let mut body: Vec<ViewerLine> = text
        .lines()
        .map(|line| ViewerLine::plain(kind, bounded(line)))
        .collect();
    let mut title = match (recall, failed) {
        (true, _) => "rift recall",
        (false, true) => "result · failed",
        (false, false) => "result",
    }
    .to_owned();
    if elided.0 > 0 {
        title.push_str(&format!(" · {} lines", elided.1));
        body.push(dim(format!(
            "{} chars on {} lines, not kept in the compact transcript",
            elided.0, elided.1
        )));
    }
    Draft {
        tone: if recall {
            ViewerItemTone::Recall
        } else {
            ViewerItemTone::ToolResult
        },
        title,
        collapsed: body.len() > OPEN_RESULT_LINES,
        body,
    }
}

fn collect_result<'a>(value: &'a Value, texts: &mut Vec<&'a str>, elided: &mut (u64, u64)) {
    if let Some((chars, lines)) = elision(value) {
        elided.0 = elided.0.saturating_add(chars);
        elided.1 = elided.1.saturating_add(lines);
        return;
    }
    match value {
        Value::String(text) => texts.push(text),
        Value::Array(values) => {
            for value in values {
                collect_result(value, texts, elided);
            }
        }
        Value::Object(object) => {
            if let Some(text) = object.get("text") {
                collect_result(text, texts, elided);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

/// The compactor's stand-in for a long string: its size, never its bytes.
fn elision(value: &Value) -> Option<(u64, u64)> {
    let handle = value.as_object()?;
    Some((
        handle.get("elided_chars")?.as_u64()?,
        handle.get("elided_lines")?.as_u64()?,
    ))
}

fn dim(text: String) -> ViewerLine {
    ViewerLine::plain(ViewerLineKind::Dim, text)
}

/// The first non-blank line, sanitised and cut to headline length.
fn first_line(text: &str) -> String {
    let line = text
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or_default();
    clipped(&bounded(line.trim()))
}

fn clipped(text: &str) -> String {
    if text.chars().count() <= HEADLINE_CHARS {
        return text.to_owned();
    }
    let mut cut: String = text.chars().take(HEADLINE_CHARS).collect();
    cut.push('…');
    cut
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items(role: &str, content: Value) -> Vec<ViewerTimelineItem> {
        let record = CompactTranscriptRecord {
            record_type: Some(role.to_owned()),
            content,
            ..CompactTranscriptRecord::default()
        };
        let place = RecordPlace {
            index: 7,
            timestamp: Some("1970-01-01 00:00Z".to_owned()),
            before_boundary: true,
        };
        record_items(&place, &record)
    }

    fn body_text(item: &ViewerTimelineItem) -> Vec<String> {
        item.body
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.text.as_str())
                    .collect::<String>()
            })
            .collect()
    }

    fn kinds(item: &ViewerTimelineItem) -> Vec<ViewerLineKind> {
        item.body.iter().map(|line| line.kind).collect()
    }

    #[test]
    fn what_the_person_typed_is_a_prompt_and_what_the_agent_said_is_its_own() {
        let prompt = items("user", serde_json::json!("fix the installer"));
        assert_eq!(prompt.len(), 1);
        assert_eq!(prompt[0].tone, ViewerItemTone::Prompt);
        assert_eq!(prompt[0].title, "prompt");
        assert_eq!(body_text(&prompt[0]), ["fix the installer"]);
        let reply = items(
            "assistant",
            serde_json::json!([{"type": "text", "text": "On it."}]),
        );
        assert_eq!(reply[0].tone, ViewerItemTone::Assistant);
        let note = items(
            "user",
            serde_json::json!("<system-reminder>\nnot typed by anyone"),
        );
        assert_eq!(note[0].tone, ViewerItemTone::System);
    }

    #[test]
    fn a_reminder_beside_a_prompt_in_one_record_keeps_each_in_its_own_voice() {
        let mixed = items(
            "user",
            serde_json::json!([
                {"type": "text", "text": "<system-reminder>\ncontext"},
                {"type": "text", "text": "fix the bug"},
                {"type": "text", "text": "and add a test"}
            ]),
        );
        let voices: Vec<(ViewerItemTone, Vec<String>)> = mixed
            .iter()
            .map(|item| (item.tone, body_text(item)))
            .collect();
        assert_eq!(voices.len(), 2, "{voices:?}");
        assert_eq!(voices[0].0, ViewerItemTone::System);
        assert_eq!(voices[1].0, ViewerItemTone::Prompt);
        assert_eq!(voices[1].1, ["fix the bug", "and add a test"]);
    }

    #[test]
    fn an_absurd_recorded_size_saturates_instead_of_overflowing() {
        let huge = serde_json::json!({"elided_chars": 1, "elided_lines": u64::MAX});
        let edit = items(
            "assistant",
            serde_json::json!([{"type": "tool_use", "name": "MultiEdit", "input": {
                "file_path": "a.rs",
                "edits": [{"new_string": huge.clone()}, {"new_string": huge.clone()}]
            }}, {"type": "tool_result", "result": [huge.clone(), huge]}]),
        );
        assert_eq!(edit[0].title, format!("edit a.rs  +{} -0", u64::MAX));
        assert!(edit[1].title.contains(&u64::MAX.to_string()));
    }

    #[test]
    fn an_edit_folds_into_its_file_with_line_counts_and_unfolds_to_the_change() {
        let edit = items(
            "assistant",
            serde_json::json!([{
                "type": "tool_use", "id": "call-1", "name": "Edit",
                "input": {"file_path": "src/lib.rs", "old_string": "a\nb", "new_string": "a\nc\nd"}
            }]),
        );
        assert_eq!(edit.len(), 1);
        assert_eq!(edit[0].tone, ViewerItemTone::CodeChange);
        assert_eq!(edit[0].title, "edit src/lib.rs  +3 -2");
        assert!(edit[0].collapsed, "a change starts folded");
        assert_eq!(
            kinds(&edit[0]),
            [
                ViewerLineKind::Removal,
                ViewerLineKind::Removal,
                ViewerLineKind::Addition,
                ViewerLineKind::Addition,
                ViewerLineKind::Addition
            ]
        );
        let write = items(
            "assistant",
            serde_json::json!([{"type": "tool_use", "name": "Write",
                "input": {"file_path": "notes.md", "content": "one\ntwo"}}]),
        );
        assert_eq!(write[0].title, "write notes.md  +2 -0");
    }

    #[test]
    fn several_edits_in_one_call_are_one_change_with_each_part_marked() {
        let multi = items(
            "assistant",
            serde_json::json!([{"type": "tool_use", "name": "MultiEdit", "input": {
                "file_path": "a.rs",
                "edits": [
                    {"old_string": "x", "new_string": "y"},
                    {"old_string": "p\nq", "new_string": ""}
                ]
            }}]),
        );
        assert_eq!(multi[0].title, "edit a.rs  +1 -3");
        assert_eq!(body_text(&multi[0])[0], "change 1 of 2");
        let patch = items(
            "assistant",
            serde_json::json!([{"type": "tool_use", "name": "apply_patch", "input": {
                "input": "*** Begin Patch\n*** Update File: b.rs\n@@ fn main\n-old\n+new\n same\n*** End Patch"
            }}]),
        );
        assert_eq!(patch[0].title, "patch b.rs  +1 -1");
        assert!(kinds(&patch[0]).contains(&ViewerLineKind::Context));
    }

    #[test]
    fn an_elided_argument_is_reported_by_its_recorded_size_and_never_reconstructed() {
        let edit = items(
            "assistant",
            serde_json::json!([{"type": "tool_use", "name": "Edit", "input": {
                "file_path": "big.rs",
                "old_string": "gone",
                "new_string": {"elided_chars": 900, "elided_lines": 40}
            }}]),
        );
        assert_eq!(edit[0].title, "edit big.rs  +40 -1");
        assert_eq!(
            kinds(&edit[0]),
            [ViewerLineKind::Removal, ViewerLineKind::Gap]
        );
        assert!(body_text(&edit[0])[1].contains("40 lines (900 chars)"));
    }

    #[test]
    fn a_rift_command_and_a_served_hook_answer_read_as_recalls() {
        let asked = items(
            "assistant",
            serde_json::json!([{"type": "tool_use", "name": "Bash",
                "input": {"command": "rift recall known-state --file src/lib.rs --json"}}]),
        );
        assert_eq!(asked[0].tone, ViewerItemTone::Recall);
        assert_eq!(
            asked[0].title,
            "rift recall known-state --file src/lib.rs --json"
        );
        assert!(asked[0].body.is_empty() && !asked[0].collapsed);
        let served = items(
            "user",
            serde_json::json!([{"type": "tool_result", "tool_use_id": "call-1", "is_error": true,
                "result": "PreToolUse:Read hook error: [rift] Read outlined: src/lib.rs"}]),
        );
        assert_eq!(served[0].tone, ViewerItemTone::Recall);
        let quoted = items("user", serde_json::json!("why did rift recall nothing?"));
        assert_eq!(quoted[0].tone, ViewerItemTone::Prompt);
    }

    #[test]
    fn a_call_shows_its_telling_argument_and_a_result_says_how_it_ended() {
        let record = items(
            "assistant",
            serde_json::json!([
                {"type": "text", "text": "Running the tests."},
                {"type": "tool_use", "name": "Bash",
                    "input": {"command": "cargo test -p rift-canopy", "timeout": 600}},
                {"type": "tool_result", "tool_use_id": "c", "is_error": true, "result": "exit 101"},
                {"type": "tool_result", "tool_use_id": "d",
                    "result": {"elided_chars": 5000, "elided_lines": 120}}
            ]),
        );
        let titles: Vec<&str> = record.iter().map(|item| item.title.as_str()).collect();
        assert_eq!(
            titles,
            [
                "assistant",
                "Bash cargo test -p rift-canopy",
                "result · failed",
                "result · 120 lines"
            ]
        );
        assert_eq!(body_text(&record[1]), ["timeout: 600"]);
        assert_eq!(kinds(&record[2]), [ViewerLineKind::Warning]);
        assert_eq!(record[3].tone, ViewerItemTone::ToolResult);
    }

    #[test]
    fn every_item_of_a_record_keeps_its_boundary_and_a_distinct_id() {
        let record = items(
            "assistant",
            serde_json::json!([
                {"type": "text", "text": "first"},
                {"type": "tool_use", "name": "Read", "input": {"file_path": "a.rs"}},
                {"type": "text", "text": "second"}
            ]),
        );
        let ids: Vec<&str> = record.iter().map(|item| item.id.as_str()).collect();
        assert_eq!(ids, ["transcript:7", "transcript:7:1", "transcript:7:2"]);
        assert!(record.iter().all(|item| item.full_session_only));
        assert!(record[0].timestamp.is_some());
        assert!(record[1..].iter().all(|item| item.timestamp.is_none()));
        let empty = items("system", serde_json::json!({"ignored": true}));
        assert_eq!(empty.len(), 1);
        assert_eq!(empty[0].id, "transcript:7");
        assert!(body_text(&empty[0])[0].contains("no displayable text"));
    }

    #[test]
    fn hostile_paths_commands_and_text_reach_the_screen_without_control_characters() {
        let hostile = items(
            "assistant",
            serde_json::json!([
                {"type": "text", "text": "ok\u{1b}[2J\u{202e}"},
                {"type": "tool_use", "name": "Bash\u{1b}]0;x\u{7}",
                    "input": {"command": "echo \u{1b}[31mred", "note\u{1b}": "\u{9b}31m"}},
                {"type": "tool_use", "name": "Edit", "input": {
                    "file_path": "a\u{1b}[1mb.rs", "old_string": "\u{1b}[0m", "new_string": "\u{7}"}}
            ]),
        );
        for item in &hostile {
            let mut shown = body_text(item);
            shown.push(item.title.clone());
            for text in shown {
                assert!(
                    !text
                        .chars()
                        .any(|c| c == '\u{1b}' || c == '\u{9b}' || c == '\u{7}'),
                    "{text:?}"
                );
            }
        }
    }
}
