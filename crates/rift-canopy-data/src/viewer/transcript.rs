//! Transcript readers and the timeline items they become: the compact
//! transcript with its checkpoint-new boundary, the redacted fallbacks, and
//! the nested task artifacts a tool-use record points at.

use std::collections::{BTreeMap, BTreeSet};

use rift_canopy::{ViewerLine, ViewerLineKind, ViewerTimelineItem, ViewerTimelineSegment};
use rift_recall_substrate::parse::{self, IssueKind, Locator, ParseIssue};
use rift_recall_substrate::{CheckpointCatalogueEntry, SubstrateReader};
use rift_recall_types::checkpoint::{CompactTranscriptRecord, TaskMetadata};

use super::text::{bounded, compact_utc, fact_line, warning_item};

type TranscriptBundle = (
    Vec<CompactTranscriptRecord>,
    Option<usize>,
    usize,
    Vec<ParseIssue>,
);

pub(super) fn transcript_excerpt(
    substrate: &impl SubstrateReader,
    checkpoint_id: &str,
) -> Option<String> {
    let (records, boundary, artifact_lines, _) = transcript_records(substrate, checkpoint_id)?;
    let start = boundary.filter(|start| *start <= artifact_lines)?;
    records
        .iter()
        .rev()
        .filter(|record| record.artifact_line >= start)
        .filter(|record| {
            record.record_type.as_deref().is_some_and(|role| {
                role.eq_ignore_ascii_case("user") || role.eq_ignore_ascii_case("assistant")
            })
        })
        .find_map(record_text)
        .filter(|text| !text.trim().is_empty())
        .map(|text| text.chars().take(240).collect())
}

pub(super) fn transcript_search_text(
    substrate: &impl SubstrateReader,
    entry: &CheckpointCatalogueEntry,
) -> Option<String> {
    let (records, _, _, _) = transcript_records_for_entry(substrate, entry)?;
    let mut out = String::new();
    for record in records {
        if let Some(text) = record_text(&record) {
            out.push_str(&text);
            out.push('\n');
        }
    }
    Some(out)
}

pub(super) fn transcript_segment(
    substrate: &impl SubstrateReader,
    entry: &CheckpointCatalogueEntry,
) -> ViewerTimelineSegment {
    let Some((records, boundary, artifact_lines, issues)) =
        transcript_records_for_entry(substrate, entry)
    else {
        return unavailable_transcript_segment();
    };
    let mut items = boundary_warnings(boundary, artifact_lines);
    for issue in issues {
        items.push(warning_item(
            &format!("transcript-issue-{}", items.len()),
            "transcript record skipped",
            &format!("{}: {}", issue.artifact, issue.detail),
        ));
    }

    let task_ids = entry
        .metadata
        .tasks
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    let mut attached_tasks = BTreeSet::new();
    let new_from = boundary.filter(|start| *start <= artifact_lines);
    for (index, record) in records.iter().enumerate() {
        items.push(record_item(index, record, new_from));
        for tool_id in record_tool_ids(record) {
            if task_ids.contains(&tool_id) {
                items.push(task_item(substrate, entry, &tool_id));
                attached_tasks.insert(tool_id);
            }
        }
    }
    for task_id in task_ids.difference(&attached_tasks) {
        items.push(unattached_task_item(substrate, entry, task_id));
    }
    ViewerTimelineSegment {
        title: "Transcript".to_owned(),
        boundary: Some(
            "artifact order only; Full session reveals records before compact_transcript_start"
                .to_owned(),
        ),
        ordered_by_time: false,
        items,
    }
}

pub(super) fn task_item(
    substrate: &impl SubstrateReader,
    entry: &CheckpointCatalogueEntry,
    tool_id: &str,
) -> ViewerTimelineItem {
    if !safe_artifact_component(tool_id) {
        return warning_item(
            &format!("task-invalid:{}", bounded(tool_id)),
            "nested task unavailable",
            "task identifier is not a safe artifact path component",
        );
    }
    let metadata = read_task_metadata(substrate, entry, tool_id);
    let transcript_name = metadata
        .value
        .compact_transcript
        .as_deref()
        .unwrap_or("transcript.compact.jsonl");
    let safe_transcript_name = safe_artifact_component(transcript_name);
    let transcript = if safe_transcript_name {
        let transcript_path = format!("tasks/{tool_id}/{transcript_name}");
        substrate
            .read_artifact(&entry.checkpoint_id, &transcript_path)
            .ok()
            .flatten()
            .map(|bytes| parse::parse_task_compact_transcript(&bytes, &entry.checkpoint_id))
    } else {
        None
    };
    ViewerTimelineItem {
        id: format!("task:{tool_id}"),
        title: format!("nested task {tool_id}"),
        timestamp: metadata.value.started_at_ms.and_then(compact_utc),
        summary: task_summary_lines(&metadata, safe_transcript_name),
        body: task_body_lines(transcript),
        expanded_body: None,
        collapsed: true,
        full_session_only: false,
    }
}

fn unavailable_transcript_segment() -> ViewerTimelineSegment {
    ViewerTimelineSegment {
        title: "Transcript".to_owned(),
        boundary: Some("no stored redacted compact or fallback transcript is available".to_owned()),
        ordered_by_time: false,
        items: vec![warning_item(
            "transcript-unavailable",
            "transcript unavailable",
            "compact and redacted full transcript artifacts are absent",
        )],
    }
}

/// The boundary is a claim about which records are new: an absent one is
/// said so, and one past the artifact's end is refused rather than trusted.
fn boundary_warnings(boundary: Option<usize>, artifact_lines: usize) -> Vec<ViewerTimelineItem> {
    match boundary {
        None => vec![warning_item(
            "transcript-boundary",
            "checkpoint-new boundary unavailable",
            "the stored transcript is shown without claiming which records are new",
        )],
        Some(start) if start > artifact_lines => vec![warning_item(
            "transcript-boundary-invalid",
            "checkpoint-new boundary is out of range",
            "no boundary was inferred; all valid records remain inspectable",
        )],
        Some(_) => Vec::new(),
    }
}

fn record_item(
    index: usize,
    record: &CompactTranscriptRecord,
    new_from: Option<usize>,
) -> ViewerTimelineItem {
    let role = record.record_type.as_deref().unwrap_or("unknown record");
    let text =
        record_text(record).unwrap_or_else(|| "structured record; no displayable text".to_owned());
    ViewerTimelineItem {
        id: format!("transcript:{index}"),
        title: bounded(role),
        timestamp: record_timestamp(record),
        summary: Vec::new(),
        body: text
            .lines()
            .map(|line| ViewerLine::plain(ViewerLineKind::Text, bounded(line)))
            .collect(),
        expanded_body: None,
        collapsed: false,
        full_session_only: new_from.is_some_and(|start| record.artifact_line < start),
    }
}

fn unattached_task_item(
    substrate: &impl SubstrateReader,
    entry: &CheckpointCatalogueEntry,
    task_id: &str,
) -> ViewerTimelineItem {
    let mut item = task_item(substrate, entry, task_id);
    item.summary.insert(
        0,
        ViewerLine::plain(
            ViewerLineKind::Warning,
            "matching parent tool-use record was not found",
        ),
    );
    item
}

fn read_task_metadata(
    substrate: &impl SubstrateReader,
    entry: &CheckpointCatalogueEntry,
    tool_id: &str,
) -> parse::Outcome<TaskMetadata> {
    let meta_path = format!("tasks/{tool_id}/metadata.json");
    substrate
        .read_artifact(&entry.checkpoint_id, &meta_path)
        .ok()
        .flatten()
        .map_or_else(
            || parse::Outcome {
                value: TaskMetadata::default(),
                issues: Vec::new(),
            },
            |bytes| parse::parse_task_metadata(&bytes, &entry.checkpoint_id),
        )
}

fn task_summary_lines(
    metadata: &parse::Outcome<TaskMetadata>,
    safe_transcript_name: bool,
) -> Vec<ViewerLine> {
    let mut summary = vec![fact_line(
        "agent",
        if metadata.value.agent_id.is_empty() {
            "unavailable"
        } else {
            &metadata.value.agent_id
        },
    )];
    if let Some(tokens) = metadata
        .value
        .input_tokens
        .zip(metadata.value.output_tokens)
    {
        summary.push(fact_line(
            "tokens",
            &format!("{} input · {} output", tokens.0, tokens.1),
        ));
    }
    summary.push(fact_line(
        "compact boundary",
        &metadata
            .value
            .compact_transcript_start
            .map_or_else(|| "unavailable".to_owned(), |start| start.to_string()),
    ));
    for issue in &metadata.issues {
        summary.push(ViewerLine::plain(
            ViewerLineKind::Warning,
            format!("task metadata: {}", issue.detail),
        ));
    }
    if !safe_transcript_name {
        summary.push(ViewerLine::plain(
            ViewerLineKind::Warning,
            "task compact transcript artifact name is not a safe leaf",
        ));
    }
    summary
}

fn task_body_lines(
    transcript: Option<parse::Outcome<Vec<CompactTranscriptRecord>>>,
) -> Vec<ViewerLine> {
    let Some(parsed) = transcript else {
        return vec![ViewerLine::plain(
            ViewerLineKind::Warning,
            "redacted compact task transcript unavailable",
        )];
    };
    let mut body = Vec::new();
    for record in parsed.value {
        let role = record.record_type.as_deref().unwrap_or("record");
        body.push(ViewerLine::plain(ViewerLineKind::Heading, bounded(role)));
        if let Some(text) = record_text(&record) {
            body.extend(
                text.lines()
                    .map(|line| ViewerLine::plain(ViewerLineKind::Text, bounded(line))),
            );
        }
    }
    for issue in parsed.issues {
        body.push(ViewerLine::plain(
            ViewerLineKind::Warning,
            format!("task transcript: {}", issue.detail),
        ));
    }
    body
}

fn transcript_records(
    substrate: &impl SubstrateReader,
    checkpoint_id: &str,
) -> Option<TranscriptBundle> {
    let metadata = substrate
        .read_artifact(checkpoint_id, "metadata.json")
        .ok()
        .flatten()
        .map(|bytes| parse::parse_metadata(&bytes, checkpoint_id).value)
        .unwrap_or_default();
    transcript_records_for_metadata(substrate, checkpoint_id, &metadata)
}

fn transcript_records_for_entry(
    substrate: &impl SubstrateReader,
    entry: &CheckpointCatalogueEntry,
) -> Option<TranscriptBundle> {
    transcript_records_for_metadata(substrate, &entry.checkpoint_id, &entry.metadata)
}

fn transcript_records_for_metadata(
    substrate: &impl SubstrateReader,
    checkpoint_id: &str,
    metadata: &rift_recall_types::checkpoint::CheckpointMetadata,
) -> Option<TranscriptBundle> {
    let compact_name = match metadata.compact_transcript.as_deref() {
        Some(name) if safe_artifact_component(name) => name,
        Some(_) => {
            return Some((
                Vec::new(),
                metadata.compact_transcript_start,
                0,
                vec![ParseIssue {
                    checkpoint_id: checkpoint_id.to_owned(),
                    artifact: "metadata.json",
                    locator: Locator::WholeFile,
                    kind: IssueKind::Corrupt,
                    detail: "compact transcript artifact name is not a safe leaf".to_owned(),
                }],
            ));
        }
        None => "transcript.compact.jsonl",
    };
    if let Ok(Some(bytes)) = substrate.read_artifact(checkpoint_id, compact_name) {
        let parsed = parse::parse_compact_transcript(&bytes, checkpoint_id);
        return Some((
            parsed.value,
            metadata.compact_transcript_start,
            jsonl_line_count(&bytes),
            parsed.issues,
        ));
    }
    for fallback in ["transcript.jsonl", "full.jsonl"] {
        let Ok(Some(bytes)) = substrate.read_artifact(checkpoint_id, fallback) else {
            continue;
        };
        if fallback == "full.jsonl" {
            let parsed = parse::parse_full_events(&bytes, checkpoint_id);
            let records = parsed
                .value
                .into_iter()
                .enumerate()
                .map(|(artifact_line, event)| CompactTranscriptRecord {
                    artifact_line,
                    record_type: Some(event.kind),
                    content: event.payload,
                    extra: BTreeMap::from([(
                        "ts".to_owned(),
                        serde_json::Value::from(event.occurred_at_ms),
                    )]),
                    ..CompactTranscriptRecord::default()
                })
                .collect();
            return Some((records, None, jsonl_line_count(&bytes), parsed.issues));
        }
        let parsed = parse::parse_compact_transcript(&bytes, checkpoint_id);
        return Some((parsed.value, None, jsonl_line_count(&bytes), parsed.issues));
    }
    None
}

fn jsonl_line_count(bytes: &[u8]) -> usize {
    if bytes.is_empty() {
        0
    } else {
        bytes
            .split(|byte| *byte == b'\n')
            .count()
            .saturating_sub(usize::from(bytes.ends_with(b"\n")))
    }
}

fn safe_artifact_component(value: &str) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && !value.contains('/')
        && !value.contains('\\')
        && !value.chars().any(char::is_control)
}

fn record_text(record: &CompactTranscriptRecord) -> Option<String> {
    let value = if record.content.is_null() {
        record.message.get("content").unwrap_or(&record.message)
    } else {
        &record.content
    };
    let mut text = Vec::new();
    collect_explicit_text(value, &mut text);
    let joined = text
        .into_iter()
        .filter(|part| !part.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    (!joined.is_empty()).then(|| bounded(&joined))
}

fn collect_explicit_text(value: &serde_json::Value, out: &mut Vec<String>) {
    match value {
        serde_json::Value::String(text) => out.push(text.clone()),
        serde_json::Value::Array(values) => {
            for value in values {
                collect_explicit_text(value, out);
            }
        }
        serde_json::Value::Object(object) => {
            match object.get("type").and_then(serde_json::Value::as_str) {
                Some("tool_use") => {
                    let name = object
                        .get("name")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("tool");
                    let id = object
                        .get("id")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("unknown");
                    out.push(format!("tool use: {name} ({id})"));
                }
                Some("tool_result") => {
                    let id = object
                        .get("tool_use_id")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("unknown");
                    out.push(format!("tool result: {id}"));
                }
                _ => {
                    if let Some(text) = object.get("text").and_then(serde_json::Value::as_str) {
                        out.push(text.to_owned());
                    }
                }
            }
        }
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {}
    }
}

fn record_tool_ids(record: &CompactTranscriptRecord) -> Vec<String> {
    let mut ids = Vec::new();
    collect_tool_ids(&record.content, &mut ids);
    collect_tool_ids(&record.message, &mut ids);
    ids.sort();
    ids.dedup();
    ids
}

fn collect_tool_ids(value: &serde_json::Value, out: &mut Vec<String>) {
    match value {
        serde_json::Value::Array(values) => {
            for value in values {
                collect_tool_ids(value, out);
            }
        }
        serde_json::Value::Object(object) => {
            if object.get("type").and_then(serde_json::Value::as_str) == Some("tool_use") {
                if let Some(id) = object.get("id").and_then(serde_json::Value::as_str) {
                    out.push(id.to_owned());
                }
            }
            if let Some(content) = object.get("content") {
                collect_tool_ids(content, out);
            }
        }
        _ => {}
    }
}

fn record_timestamp(record: &CompactTranscriptRecord) -> Option<String> {
    let value = record.extra.get("ts")?;
    match value {
        serde_json::Value::Number(number) => number.as_i64().and_then(compact_utc),
        serde_json::Value::String(text) => rift_recall_substrate::parse_rfc3339_ms(text)
            .and_then(compact_utc)
            .or_else(|| Some(bounded(text))),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use rift_recall_substrate::MemorySubstrate;

    use super::super::fixtures::{entry, item_text, rich_entry};
    use super::*;

    #[test]
    fn display_helpers_cover_structured_and_hostile_factual_inputs() {
        assert_eq!(jsonl_line_count(b""), 0);
        assert_eq!(jsonl_line_count(b"one"), 1);
        assert_eq!(jsonl_line_count(b"one\n"), 1);
        assert_eq!(jsonl_line_count(b"one\ntwo"), 2);

        for safe in ["task", "task-id", "unicode-λ"] {
            assert!(safe_artifact_component(safe), "safe leaf: {safe}");
        }
        for unsafe_name in ["", ".", "..", "a/b", "a\\b", "line\nbreak"] {
            assert!(
                !safe_artifact_component(unsafe_name),
                "unsafe leaf: {unsafe_name:?}"
            );
        }

        let value = serde_json::json!([
            "plain",
            {"text": "object text"},
            {"type": "tool_use", "name": "read", "id": "call-1"},
            {"type": "tool_use"},
            {"type": "tool_result", "tool_use_id": "call-1"},
            {"type": "tool_result"},
            {"content": [{"type": "tool_use", "id": "call-2"}]},
            null,
            true,
            7
        ]);
        let mut text = Vec::new();
        collect_explicit_text(&value, &mut text);
        assert!(text.iter().any(|part| part == "plain"));
        assert!(text.iter().any(|part| part.contains("tool use: read")));
        let mut ids = Vec::new();
        collect_tool_ids(&value, &mut ids);
        assert_eq!(ids, ["call-1", "call-2"]);

        let mut record = CompactTranscriptRecord {
            content: value,
            ..CompactTranscriptRecord::default()
        };
        assert!(record_text(&record).is_some());
        assert_eq!(record_tool_ids(&record), ["call-1", "call-2"]);
        record.content = serde_json::Value::Null;
        record.message = serde_json::json!({"content": "message fallback"});
        assert_eq!(record_text(&record).as_deref(), Some("message fallback"));
        record.message = serde_json::json!({"ignored": 1});
        assert!(record_text(&record).is_none());

        for (value, expected) in [
            (serde_json::json!(0), Some("1970-01-01 00:00Z")),
            (serde_json::json!(-1), None),
            (
                serde_json::json!("1970-01-01T00:00:00Z"),
                Some("1970-01-01 00:00Z"),
            ),
            (
                serde_json::json!("recorded without parseable time"),
                Some("recorded without parseable time"),
            ),
            (serde_json::json!(true), None),
        ] {
            record.extra = BTreeMap::from([("ts".to_owned(), value)]);
            assert_eq!(record_timestamp(&record).as_deref(), expected);
        }
        record.extra.clear();
        assert!(record_timestamp(&record).is_none());
    }

    #[test]
    fn a_missing_transcript_and_unsafe_or_absent_tasks_are_named_as_such() {
        let rich = rich_entry(&"a".repeat(40));
        let empty_substrate = MemorySubstrate::local();
        let missing_transcript = transcript_segment(&empty_substrate, &rich);
        assert!(item_text(&missing_transcript.items[0]).contains("compact and redacted"));
        assert!(
            item_text(&task_item(&empty_substrate, &rich, "../unsafe")).contains("safe artifact")
        );
        assert!(
            item_text(&task_item(&empty_substrate, &rich, "task-missing")).contains("unavailable")
        );
    }

    #[test]
    fn a_recorded_task_lists_its_tokens_and_nested_records() {
        let rich = entry(&"e".repeat(64), &"d".repeat(40));
        let artifacts = MemorySubstrate::local()
            .with_artifact(
                &rich.checkpoint_id,
                "tasks/task-rich/metadata.json",
                br#"{"tool_use_id":"task-rich","agent_id":"codex","started_at_ms":0,"input_tokens":4,"output_tokens":2,"compact_transcript_start":0}"#,
            )
            .with_artifact(
                &rich.checkpoint_id,
                "tasks/task-rich/transcript.compact.jsonl",
                br#"{"type":"assistant","content":"nested result"}
{"type":"system","content":{"ignored":true}}
"#,
            );
        let task = task_item(&artifacts, &rich, "task-rich");
        assert!(item_text(&task).contains("4 input"));
        assert!(item_text(&task).contains("nested result"));
    }

    #[test]
    fn compact_boundary_uses_physical_lines_even_when_a_prior_record_is_corrupt() {
        let mut checkpoint = entry("checkpoint", &"a".repeat(40));
        checkpoint.metadata.compact_transcript_start = Some(2);
        let substrate = MemorySubstrate::local()
            .with_artifact(
                "checkpoint",
                "metadata.json",
                b"{\"compact_transcript_start\":2}",
            )
            .with_artifact(
                "checkpoint",
                "transcript.compact.jsonl",
                b"{\"type\":\"user\",\"content\":\"before\"}\ncorrupt\n{\"type\":\"assistant\",\"content\":\"after\"}\n",
            );
        assert_eq!(
            transcript_excerpt(&substrate, "checkpoint").as_deref(),
            Some("after")
        );
        let segment = transcript_segment(&substrate, &checkpoint);
        let before = segment
            .items
            .iter()
            .find(|item| item.id == "transcript:0")
            .unwrap();
        let after = segment
            .items
            .iter()
            .find(|item| item.id == "transcript:1")
            .unwrap();
        assert!(before.full_session_only);
        assert!(!after.full_session_only);
        assert!(segment
            .items
            .iter()
            .any(|item| item.title == "transcript record skipped"));
    }

    #[test]
    fn configured_artifact_names_cannot_escape_the_checkpoint_subtree() {
        let mut checkpoint = entry("checkpoint", &"a".repeat(40));
        checkpoint.metadata.compact_transcript = Some("../raw-shadow.jsonl".to_owned());
        checkpoint.metadata.compact_transcript_start = Some(0);
        let substrate = MemorySubstrate::local()
            .with_artifact(
                "checkpoint",
                "metadata.json",
                b"{\"compact_transcript\":\"../raw-shadow.jsonl\",\"compact_transcript_start\":0}",
            )
            .with_artifact(
                "checkpoint",
                "../raw-shadow.jsonl",
                b"{\"type\":\"user\",\"content\":\"must-not-render\"}\n",
            );
        assert!(transcript_excerpt(&substrate, "checkpoint").is_none());
        let segment = transcript_segment(&substrate, &checkpoint);
        assert!(!segment.items.iter().any(|item| {
            item.body.iter().any(|line| {
                line.spans
                    .iter()
                    .any(|span| span.text.contains("must-not-render"))
            })
        }));
        assert!(segment.items.iter().any(|item| {
            item.summary.iter().any(|line| {
                line.spans
                    .iter()
                    .any(|span| span.text.contains("not a safe leaf"))
            })
        }));
    }
}
