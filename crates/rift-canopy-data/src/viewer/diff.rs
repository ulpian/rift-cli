//! The recorded-hunk diff: revisions and hunk ranges read from the
//! projection, paired with the anchor commit's blobs under the serve policy,
//! and rendered with bounded context and semantic highlighting. Ranges are
//! never recomputed; a range the blobs cannot hold is metadata only.

use std::collections::BTreeMap;
use std::path::Path;

use rift_canopy::{SyntaxClass, ViewerLine, ViewerLineKind, ViewerSpan, ViewerTimelineItem};
use rift_config::{EffectiveConfig, PolicySource};
use rift_core::Result;
use rift_git::{BlobReader, GixBlobReader};
use rift_recall_policy::{load_serve_policy, PolicyEngine, ServePolicySource};
use rift_recall_substrate::CheckpointCatalogueEntry;
use rift_shadow_attribution::MAX_DIFF_BYTES;
use rift_store::{CanonicalStore, Row, SqliteStore, Value};

use super::open_existing_read_only;
use super::text::{bounded, warning_item};

use crate::store::runtime;

const DEFAULT_DIFF_CONTEXT: usize = 3;
const EXPANDED_DIFF_CONTEXT: usize = 24;

#[derive(Clone)]
struct RecordedHunk {
    id: String,
    old_start: usize,
    old_len: usize,
    new_start: usize,
    new_len: usize,
}

#[derive(Default)]
struct RecordedFile {
    revision_id: String,
    path: String,
    new_blob_oid: String,
    hunks: Vec<RecordedHunk>,
}

struct DiffPolicy {
    config: Option<EffectiveConfig>,
    serve: rift_recall_policy::ServePolicy,
    fail_closed_reason: Option<&'static str>,
}

/// Both sides of one file as logical lines, with the highlight spans each
/// side parsed to when its language is known.
struct DiffSides<'a> {
    old: &'a [&'a str],
    new: &'a [&'a str],
    old_highlights: Option<&'a [Vec<ViewerSpan>]>,
    new_highlights: Option<&'a [Vec<ViewerSpan>]>,
}

impl DiffPolicy {
    fn load(project: &Path) -> Self {
        let rift_dir = project.join(".rift");
        let config = EffectiveConfig::load(&rift_dir).ok();
        let serve = load_serve_policy(&rift_dir);
        let config_refused = config
            .as_ref()
            .is_some_and(|config| matches!(config.policy_source(), PolicySource::Refused(_)));
        let fail_closed_reason = if config.is_none() {
            Some("invalid configuration")
        } else if config_refused {
            Some("refused configuration")
        } else {
            match serve.source {
                ServePolicySource::Invalid => Some("invalid Recall serve policy"),
                ServePolicySource::Refused => Some("refused Recall serve policy"),
                ServePolicySource::Declared | ServePolicySource::Default => None,
            }
        };
        Self {
            config,
            serve: serve.policy,
            fail_closed_reason,
        }
    }

    fn permits(&self, path: &str) -> std::result::Result<(), String> {
        if let Some(reason) = self.fail_closed_reason {
            return Err(format!("{reason}; all source code is metadata-only"));
        }
        let Some(config) = self.config.as_ref() else {
            return Err("configuration unavailable; all source code is metadata-only".to_owned());
        };
        let engine = PolicyEngine::new(config).with_serve_policy(self.serve.clone());
        if engine.is_denied_path(path) {
            return Err("denied path; ranges and counts only".to_owned());
        }
        if engine.exact_source_required(path) {
            return Err("sensitive path; ranges and counts only".to_owned());
        }
        Ok(())
    }
}

impl DiffSides<'_> {
    /// A context line is shown from the new side: both sides hold it, and
    /// the new side is the one the reader is being brought to.
    fn context(&self, old_index: usize, new_index: usize) -> ViewerLine {
        source_line(
            ViewerLineKind::Context,
            Some(old_index + 1),
            Some(new_index + 1),
            self.new.get(new_index).copied().unwrap_or(""),
            self.new_highlights.and_then(|lines| lines.get(new_index)),
        )
    }

    fn removal(&self, index: usize) -> ViewerLine {
        source_line(
            ViewerLineKind::Removal,
            Some(index + 1),
            None,
            self.old.get(index).copied().unwrap_or(""),
            self.old_highlights.and_then(|lines| lines.get(index)),
        )
    }

    fn addition(&self, index: usize) -> ViewerLine {
        source_line(
            ViewerLineKind::Addition,
            None,
            Some(index + 1),
            self.new.get(index).copied().unwrap_or(""),
            self.new_highlights.and_then(|lines| lines.get(index)),
        )
    }
}

pub(super) fn diff_items(
    project: &Path,
    projection_path: &Path,
    entry: &CheckpointCatalogueEntry,
) -> Result<Vec<ViewerTimelineItem>> {
    let Some(anchor) = entry.anchor_commit.as_deref() else {
        return Ok(vec![warning_item(
            "diff-anchor-unavailable",
            "diff metadata only",
            "anchor commit unavailable; recorded ranges cannot be paired with blobs",
        )]);
    };
    let Some(store) = open_existing_read_only(projection_path) else {
        return Ok(vec![warning_item(
            "diff-projection-unavailable",
            "diff metadata unavailable",
            "capture projection is absent or unreadable; no database was created",
        )]);
    };
    let files = recorded_files(&store, &entry.checkpoint_id, anchor)?;
    if files.is_empty() {
        return Ok(Vec::new());
    }

    let blob_reader = GixBlobReader::open(project)?;
    let new_tree = tree_blobs(&blob_reader, anchor)?;
    let old_tree = match blob_reader.parent_of(anchor)? {
        Some(parent) => tree_blobs(&blob_reader, &parent)?,
        None => BTreeMap::new(),
    };
    let policy = DiffPolicy::load(project);
    Ok(files
        .into_values()
        .map(|file| render_file_diff(&blob_reader, &old_tree, &new_tree, &policy, file))
        .collect())
}

/// The files the checkpoint's attribution names, then every hunk recorded
/// against the anchor commit — a file the hunks name but attribution did
/// not is still a recorded change and joins the map.
fn recorded_files(
    store: &SqliteStore,
    checkpoint_id: &str,
    anchor: &str,
) -> Result<BTreeMap<String, RecordedFile>> {
    let rt = runtime()?;
    let mut files = BTreeMap::<String, RecordedFile>::new();
    if let Ok(rows) = rt.block_on(store.query(
        "SELECT DISTINCT fr.file_revision_id, fr.path, fr.blob_oid \
         FROM attribution a JOIN file_revision fr \
           ON fr.file_revision_id = a.file_revision_id \
         WHERE a.checkpoint_id = ?1 ORDER BY fr.path, fr.file_revision_id",
        &[Value::text(checkpoint_id.to_owned())],
    )) {
        for row in rows {
            let (Ok(revision_id), Ok(path), Ok(blob_oid)) =
                (row.get_text(0), row.get_text(1), row.get_text(2))
            else {
                continue;
            };
            files.insert(
                revision_id.clone(),
                RecordedFile {
                    revision_id,
                    path,
                    new_blob_oid: blob_oid,
                    hunks: Vec::new(),
                },
            );
        }
    }
    let rows = rt.block_on(store.query(
        "SELECT h.hunk_id, h.file_revision_id, h.old_start, h.old_len, \
                h.new_start, h.new_len, fr.path, fr.blob_oid \
         FROM changed_hunk h JOIN file_revision fr \
           ON fr.file_revision_id = h.file_revision_id \
         WHERE h.commit_id = ?1 \
         ORDER BY fr.path, h.old_start, h.new_start, h.hunk_id",
        &[Value::text(anchor.to_owned())],
    ))?;
    for row in rows {
        push_recorded_hunk(&mut files, &row);
    }
    Ok(files)
}

/// A hunk row with a negative range is skipped rather than clamped: the
/// projection recorded it, and the renderer must not invent a different one.
fn push_recorded_hunk(files: &mut BTreeMap<String, RecordedFile>, row: &Row) {
    let (
        Ok(id),
        Ok(revision_id),
        Ok(old_start),
        Ok(old_len),
        Ok(new_start),
        Ok(new_len),
        Ok(path),
        Ok(blob_oid),
    ) = (
        row.get_text(0),
        row.get_text(1),
        row.get_i64(2),
        row.get_i64(3),
        row.get_i64(4),
        row.get_i64(5),
        row.get_text(6),
        row.get_text(7),
    )
    else {
        return;
    };
    let Some(hunk) = checked_hunk(id, old_start, old_len, new_start, new_len) else {
        return;
    };
    files
        .entry(revision_id.clone())
        .or_insert_with(|| RecordedFile {
            revision_id,
            path,
            new_blob_oid: blob_oid,
            hunks: Vec::new(),
        })
        .hunks
        .push(hunk);
}

fn checked_hunk(
    id: String,
    old_start: i64,
    old_len: i64,
    new_start: i64,
    new_len: i64,
) -> Option<RecordedHunk> {
    Some(RecordedHunk {
        id,
        old_start: usize::try_from(old_start).ok()?,
        old_len: usize::try_from(old_len).ok()?,
        new_start: usize::try_from(new_start).ok()?,
        new_len: usize::try_from(new_len).ok()?,
    })
}

fn tree_blobs(reader: &GixBlobReader, commit: &str) -> Result<BTreeMap<String, String>> {
    Ok(reader
        .tree_entries_at(commit)?
        .into_iter()
        .map(|entry| (entry.path, entry.blob_oid))
        .collect())
}

fn render_file_diff(
    reader: &impl BlobReader,
    old_tree: &BTreeMap<String, String>,
    new_tree: &BTreeMap<String, String>,
    policy: &DiffPolicy,
    mut file: RecordedFile,
) -> ViewerTimelineItem {
    file.hunks
        .sort_by_key(|hunk| (hunk.old_start, hunk.new_start, hunk.id.clone()));
    let removed = file.hunks.iter().map(|hunk| hunk.old_len).sum::<usize>();
    let added = file.hunks.iter().map(|hunk| hunk.new_len).sum::<usize>();
    let summary = vec![ViewerLine::plain(
        ViewerLineKind::DiffFile,
        format!("{}  +{added} -{removed}", bounded(&file.path)),
    )];
    if let Err(reason) = policy.permits(&file.path) {
        return metadata_only_diff(file, summary, &reason);
    }
    if file.hunks.is_empty() {
        return metadata_only_diff(
            file,
            summary,
            "changed file has no recorded textual hunk ranges (binary, oversized, or unavailable)",
        );
    }
    let (old_text, new_text) = match readable_sides(reader, old_tree, new_tree, &file) {
        Ok(sides) => sides,
        Err(reason) => return metadata_only_diff(file, summary, reason),
    };
    let old_lines = logical_lines(&old_text);
    let new_lines = logical_lines(&new_text);
    if !ranges_fit(&file.hunks, old_lines.len(), new_lines.len()) {
        return metadata_only_diff(
            file,
            summary,
            "recorded hunk range is outside the readable blobs",
        );
    }
    let old_highlights = highlight_file(&file.path, &old_text);
    let new_highlights = highlight_file(&file.path, &new_text);
    let sides = DiffSides {
        old: &old_lines,
        new: &new_lines,
        old_highlights: old_highlights.as_deref(),
        new_highlights: new_highlights.as_deref(),
    };
    ViewerTimelineItem {
        id: format!("diff:{}", file.revision_id),
        title: file.path,
        timestamp: None,
        summary,
        body: render_recorded_hunks(&file.hunks, &sides, DEFAULT_DIFF_CONTEXT),
        expanded_body: Some(render_recorded_hunks(
            &file.hunks,
            &sides,
            EXPANDED_DIFF_CONTEXT,
        )),
        collapsed: false,
        full_session_only: false,
        tone: rift_canopy::ViewerItemTone::Neutral,
    }
}

/// Both blobs as text, or the reason the file stays metadata-only. The
/// checks run in the order a reader would want to hear them: a missing side,
/// then the size limit, then binary, then non-UTF-8.
fn readable_sides(
    reader: &impl BlobReader,
    old_tree: &BTreeMap<String, String>,
    new_tree: &BTreeMap<String, String>,
    file: &RecordedFile,
) -> std::result::Result<(String, String), &'static str> {
    let old_bytes = match old_tree.get(&file.path) {
        Some(oid) => match reader.read_blob(oid) {
            Ok(Some(bytes)) => bytes,
            Ok(None) | Err(_) => return Err("old-side blob missing or unreadable"),
        },
        // A path absent from the first parent's tree is a factual added file
        // (including every path in a root commit), so its old side is empty.
        None => Vec::new(),
    };
    let new_oid = new_tree.get(&file.path).unwrap_or(&file.new_blob_oid);
    let Some(new_bytes) = reader.read_blob(new_oid).ok().flatten() else {
        return Err("new-side blob missing or unreadable");
    };
    if old_bytes.len() > MAX_DIFF_BYTES || new_bytes.len() > MAX_DIFF_BYTES {
        return Err("blob exceeds the existing 5 MiB diff limit");
    }
    if old_bytes.contains(&0) || new_bytes.contains(&0) {
        return Err("binary blob; ranges and counts only");
    }
    let (Ok(old_text), Ok(new_text)) = (String::from_utf8(old_bytes), String::from_utf8(new_bytes))
    else {
        return Err("non-text blob; ranges and counts only");
    };
    Ok((old_text, new_text))
}

fn metadata_only_diff(
    file: RecordedFile,
    summary: Vec<ViewerLine>,
    reason: &str,
) -> ViewerTimelineItem {
    ViewerTimelineItem {
        id: format!("diff:{}", file.revision_id),
        title: file.path,
        timestamp: None,
        summary,
        body: vec![ViewerLine::plain(ViewerLineKind::Warning, reason)],
        expanded_body: None,
        collapsed: false,
        full_session_only: false,
        tone: rift_canopy::ViewerItemTone::Neutral,
    }
}

fn logical_lines(text: &str) -> Vec<&str> {
    text.split_terminator('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .collect()
}

fn ranges_fit(hunks: &[RecordedHunk], old_len: usize, new_len: usize) -> bool {
    hunks.iter().all(|hunk| {
        hunk.old_start.saturating_add(hunk.old_len) <= old_len
            && hunk.new_start.saturating_add(hunk.new_len) <= new_len
    })
}

/// Every recorded hunk with up to `context` unchanged lines around it; the
/// unchanged lines between two hunks are counted as a gap rather than
/// shown, and context never overlaps what the previous hunk already showed.
fn render_recorded_hunks(
    hunks: &[RecordedHunk],
    sides: &DiffSides<'_>,
    context: usize,
) -> Vec<ViewerLine> {
    let mut out = Vec::new();
    let mut previous_old_end = 0usize;
    let mut previous_new_end = 0usize;
    for (hunk_index, hunk) in hunks.iter().enumerate() {
        let old_before = hunk.old_start.saturating_sub(context);
        let new_before = hunk.new_start.saturating_sub(context);
        let omitted = old_before
            .saturating_sub(previous_old_end)
            .min(new_before.saturating_sub(previous_new_end));
        if omitted > 0 {
            out.push(ViewerLine::plain(
                ViewerLineKind::Gap,
                format!("{omitted} unchanged lines"),
            ));
        }
        out.push(hunk_header(hunk));
        let before_count = hunk
            .old_start
            .saturating_sub(old_before.max(previous_old_end))
            .min(
                hunk.new_start
                    .saturating_sub(new_before.max(previous_new_end)),
            );
        for offset in 0..before_count {
            let old_index = hunk.old_start - before_count + offset;
            let new_index = hunk.new_start - before_count + offset;
            out.push(sides.context(old_index, new_index));
        }
        out.extend((0..hunk.old_len).map(|offset| sides.removal(hunk.old_start + offset)));
        out.extend((0..hunk.new_len).map(|offset| sides.addition(hunk.new_start + offset)));
        let old_end = hunk.old_start + hunk.old_len;
        let new_end = hunk.new_start + hunk.new_len;
        let next = hunks.get(hunk_index.saturating_add(1));
        let next_old = next.map_or(sides.old.len(), |candidate| candidate.old_start);
        let next_new = next.map_or(sides.new.len(), |candidate| candidate.new_start);
        let after_count = context
            .min(next_old.saturating_sub(old_end))
            .min(next_new.saturating_sub(new_end));
        for offset in 0..after_count {
            out.push(sides.context(old_end + offset, new_end + offset));
        }
        previous_old_end = old_end.saturating_add(after_count);
        previous_new_end = new_end.saturating_add(after_count);
    }
    out
}

fn hunk_header(hunk: &RecordedHunk) -> ViewerLine {
    ViewerLine::plain(
        ViewerLineKind::DiffHunk,
        format!(
            "@@ -{},{} +{},{} @@",
            hunk.old_start.saturating_add(1),
            hunk.old_len,
            hunk.new_start.saturating_add(1),
            hunk.new_len
        ),
    )
}

fn source_line(
    kind: ViewerLineKind,
    old_line: Option<usize>,
    new_line: Option<usize>,
    text: &str,
    spans: Option<&Vec<ViewerSpan>>,
) -> ViewerLine {
    ViewerLine {
        old_line: old_line.and_then(|line| u32::try_from(line).ok()),
        new_line: new_line.and_then(|line| u32::try_from(line).ok()),
        kind,
        spans: spans.cloned().unwrap_or_else(|| {
            vec![ViewerSpan {
                text: bounded(text),
                class: SyntaxClass::Plain,
            }]
        }),
    }
}

fn highlight_file(path: &str, text: &str) -> Option<Vec<Vec<ViewerSpan>>> {
    use syntect::easy::ScopeRegionIterator;
    use syntect::parsing::{ParseState, ScopeStack, SyntaxSet};

    let syntaxes = SyntaxSet::load_defaults_newlines();
    // An unrecognised extension is a plain-text fallback, not a successful
    // highlight. Returning `None` keeps that distinction explicit and lets the
    // common source-line builder apply the same sanitized fallback used for a
    // parser error.
    let syntax = syntaxes.find_syntax_for_file(path).ok().flatten()?;
    let mut parse_state = ParseState::new(syntax);
    let mut stack = ScopeStack::new();
    let mut output = Vec::new();
    for raw_line in text.split_inclusive('\n') {
        let display = raw_line.strip_suffix('\n').unwrap_or(raw_line);
        let display = display.strip_suffix('\r').unwrap_or(display);
        if raw_line.len() > 32 * 1024 {
            output.push(vec![ViewerSpan {
                text: bounded(display),
                class: SyntaxClass::Plain,
            }]);
            continue;
        }
        let operations = parse_state.parse_line(raw_line, &syntaxes).ok()?;
        let mut spans: Vec<ViewerSpan> = Vec::new();
        for (region, operation) in ScopeRegionIterator::new(&operations, raw_line) {
            stack.apply(operation).ok()?;
            let region = region.strip_suffix('\n').unwrap_or(region);
            let region = region.strip_suffix('\r').unwrap_or(region);
            if region.is_empty() {
                continue;
            }
            let class = classify_scope(&stack);
            if let Some(previous) = spans.last_mut() {
                if previous.class == class {
                    previous.text.push_str(region);
                    continue;
                }
            }
            spans.push(ViewerSpan {
                text: bounded(region),
                class,
            });
        }
        if spans.is_empty() {
            spans.push(ViewerSpan {
                text: bounded(display),
                class: SyntaxClass::Plain,
            });
        }
        output.push(spans);
    }
    if text.is_empty() || text.ends_with('\n') {
        output.truncate(logical_lines(text).len());
    }
    Some(output)
}

fn classify_scope(stack: &syntect::parsing::ScopeStack) -> SyntaxClass {
    for scope in stack.scopes.iter().rev() {
        let scope = scope.build_string();
        if scope.contains("comment") {
            return SyntaxClass::Comment;
        }
        if scope.contains("string") {
            return SyntaxClass::String;
        }
        if scope.contains("numeric") || scope.contains("constant") {
            return SyntaxClass::Number;
        }
        if scope.contains("entity.name.function")
            || scope.contains("support.function")
            || scope.contains("meta.function-call")
        {
            return SyntaxClass::Function;
        }
        if scope.contains("entity.name.type")
            || scope.contains("support.type")
            || scope.contains("storage.type")
        {
            return SyntaxClass::Type;
        }
        if scope.contains("keyword.operator") || scope.contains("punctuation") {
            return SyntaxClass::Operator;
        }
        if scope.contains("keyword") || scope.contains("storage.modifier") {
            return SyntaxClass::Keyword;
        }
    }
    SyntaxClass::Plain
}

#[cfg(test)]
mod tests {
    use rift_git::MemoryBlobReader;

    use super::super::fixtures::item_text;
    use super::*;

    fn permissive_diff_policy() -> DiffPolicy {
        DiffPolicy {
            config: Some(EffectiveConfig::secure_default().unwrap()),
            serve: rift_recall_policy::ServePolicy::default(),
            fail_closed_reason: None,
        }
    }

    fn one_hunk_file(path: &str) -> RecordedFile {
        RecordedFile {
            revision_id: "revision".to_owned(),
            path: path.to_owned(),
            new_blob_oid: "new".to_owned(),
            hunks: vec![RecordedHunk {
                id: "hunk".to_owned(),
                old_start: 0,
                old_len: 1,
                new_start: 0,
                new_len: 1,
            }],
        }
    }

    /// A reader holding a one-line old and new blob, plus a blob that is
    /// not UTF-8.
    fn three_blob_reader() -> MemoryBlobReader {
        MemoryBlobReader::new()
            .with_blob("old", b"old\n")
            .with_blob("new", b"new\n")
            .with_blob("invalid", &[0xff])
    }

    fn lib_rs_tree(oid: &str) -> BTreeMap<String, String> {
        BTreeMap::from([("src/lib.rs".to_owned(), oid.to_owned())])
    }

    #[test]
    fn recorded_hunks_keep_three_context_lines_and_explicit_gaps() {
        let old = (0..18).map(|n| format!("old-{n}")).collect::<Vec<_>>();
        let new = (0..18).map(|n| format!("new-{n}")).collect::<Vec<_>>();
        let old_refs = old.iter().map(String::as_str).collect::<Vec<_>>();
        let new_refs = new.iter().map(String::as_str).collect::<Vec<_>>();
        let hunks = [
            RecordedHunk {
                id: "h1".to_owned(),
                old_start: 5,
                old_len: 1,
                new_start: 5,
                new_len: 1,
            },
            RecordedHunk {
                id: "h2".to_owned(),
                old_start: 14,
                old_len: 1,
                new_start: 14,
                new_len: 1,
            },
        ];
        let sides = DiffSides {
            old: &old_refs,
            new: &new_refs,
            old_highlights: None,
            new_highlights: None,
        };
        let lines = render_recorded_hunks(&hunks, &sides, 3);
        assert_eq!(lines[0].kind, ViewerLineKind::Gap);
        assert_eq!(lines[0].spans[0].text, "2 unchanged lines");
        assert!(lines
            .iter()
            .any(|line| { line.kind == ViewerLineKind::Removal && line.old_line == Some(6) }));
        assert!(lines
            .iter()
            .any(|line| { line.kind == ViewerLineKind::Addition && line.new_line == Some(15) }));
        let first_hunk = lines
            .iter()
            .position(|line| line.kind == ViewerLineKind::DiffHunk)
            .unwrap();
        assert_eq!(
            lines[first_hunk + 1..first_hunk + 4]
                .iter()
                .filter(|line| line.kind == ViewerLineKind::Context)
                .count(),
            3
        );
    }

    #[test]
    fn recorded_ranges_outside_blobs_fail_closed() {
        let hunk = RecordedHunk {
            id: "h".to_owned(),
            old_start: 8,
            old_len: 2,
            new_start: 0,
            new_len: 1,
        };
        assert!(!ranges_fit(&[hunk], 9, 1));
    }

    #[test]
    fn a_hunk_row_with_a_negative_range_is_refused() {
        assert!(checked_hunk("ok".to_owned(), 0, 0, 0, 0).is_some());
        for values in [(-1, 0, 0, 0), (0, -1, 0, 0), (0, 0, -1, 0), (0, 0, 0, -1)] {
            assert!(
                checked_hunk("bad".to_owned(), values.0, values.1, values.2, values.3).is_none()
            );
        }
    }

    #[test]
    fn unreadable_old_blob_is_metadata_only_not_invented_as_a_root_addition() {
        let reader = MemoryBlobReader::new().with_blob("new", b"new\n");
        let old_tree = BTreeMap::from([("src/lib.rs".to_owned(), "missing-old".to_owned())]);
        let new_tree = BTreeMap::from([("src/lib.rs".to_owned(), "new".to_owned())]);
        let item = render_file_diff(
            &reader,
            &old_tree,
            &new_tree,
            &permissive_diff_policy(),
            one_hunk_file("src/lib.rs"),
        );
        assert!(item_text(&item).contains("old-side blob missing or unreadable"));
        assert!(!item
            .body
            .iter()
            .any(|line| line.kind == ViewerLineKind::Addition));
    }

    #[test]
    fn binary_oversized_sensitive_and_fail_closed_diffs_keep_metadata() {
        let maps = || {
            (
                BTreeMap::from([("src/lib.rs".to_owned(), "old".to_owned())]),
                BTreeMap::from([("src/lib.rs".to_owned(), "new".to_owned())]),
            )
        };
        let (old_tree, new_tree) = maps();
        let binary = MemoryBlobReader::new()
            .with_blob("old", b"old\0")
            .with_blob("new", b"new\0");
        let item = render_file_diff(
            &binary,
            &old_tree,
            &new_tree,
            &permissive_diff_policy(),
            one_hunk_file("src/lib.rs"),
        );
        assert!(item_text(&item).contains("binary blob"));

        let oversized = MemoryBlobReader::new()
            .with_blob("old", b"old\n")
            .with_blob("new", &vec![b'x'; MAX_DIFF_BYTES + 1]);
        let item = render_file_diff(
            &oversized,
            &old_tree,
            &new_tree,
            &permissive_diff_policy(),
            one_hunk_file("src/lib.rs"),
        );
        assert!(item_text(&item).contains("exceeds the existing"));

        let item = render_file_diff(
            &MemoryBlobReader::new(),
            &BTreeMap::new(),
            &BTreeMap::new(),
            &permissive_diff_policy(),
            one_hunk_file("project/.env"),
        );
        assert!(item_text(&item).contains("sensitive path"));

        let fail_closed = DiffPolicy {
            config: Some(EffectiveConfig::secure_default().unwrap()),
            serve: rift_recall_policy::ServePolicy::default(),
            fail_closed_reason: Some("invalid configuration"),
        };
        let item = render_file_diff(
            &MemoryBlobReader::new(),
            &BTreeMap::new(),
            &BTreeMap::new(),
            &fail_closed,
            one_hunk_file("src/lib.rs"),
        );
        assert!(item_text(&item).contains("all source code is metadata-only"));
    }

    #[test]
    fn a_file_without_recorded_hunks_says_so_instead_of_diffing() {
        let empty_hunks = RecordedFile {
            revision_id: "empty".to_owned(),
            path: "src/lib.rs".to_owned(),
            new_blob_oid: "new".to_owned(),
            hunks: Vec::new(),
        };
        assert!(item_text(&render_file_diff(
            &three_blob_reader(),
            &BTreeMap::new(),
            &BTreeMap::new(),
            &permissive_diff_policy(),
            empty_hunks,
        ))
        .contains("no recorded textual hunk"));
    }

    #[test]
    fn readable_sides_render_an_expanded_body_and_the_rest_name_their_reason() {
        let reader = three_blob_reader();
        let valid = render_file_diff(
            &reader,
            &lib_rs_tree("old"),
            &lib_rs_tree("new"),
            &permissive_diff_policy(),
            one_hunk_file("src/lib.rs"),
        );
        assert!(valid.expanded_body.is_some());

        let invalid = render_file_diff(
            &reader,
            &BTreeMap::new(),
            &lib_rs_tree("invalid"),
            &permissive_diff_policy(),
            one_hunk_file("src/lib.rs"),
        );
        assert!(item_text(&invalid).contains("non-text blob"));

        let missing_new = render_file_diff(
            &MemoryBlobReader::new(),
            &BTreeMap::new(),
            &BTreeMap::new(),
            &permissive_diff_policy(),
            one_hunk_file("src/lib.rs"),
        );
        assert!(item_text(&missing_new).contains("new-side blob missing"));
    }

    #[test]
    fn a_policy_without_configuration_permits_nothing() {
        let no_config = DiffPolicy {
            config: None,
            serve: rift_recall_policy::ServePolicy::default(),
            fail_closed_reason: None,
        };
        assert!(no_config.permits("src/lib.rs").is_err());
    }

    #[test]
    fn an_added_file_diffs_against_an_empty_old_side() {
        let reader = MemoryBlobReader::new().with_blob("added", b"added\n");
        let added = RecordedFile {
            revision_id: "added".to_owned(),
            path: "src/added.rs".to_owned(),
            new_blob_oid: "added".to_owned(),
            hunks: vec![RecordedHunk {
                id: "addition".to_owned(),
                old_start: 0,
                old_len: 0,
                new_start: 0,
                new_len: 1,
            }],
        };
        assert!(render_file_diff(
            &reader,
            &BTreeMap::new(),
            &BTreeMap::new(),
            &permissive_diff_policy(),
            added,
        )
        .expanded_body
        .is_some());
    }

    #[test]
    fn syntax_highlighting_is_semantic_and_has_plain_fallback() {
        let highlighted = highlight_file("main.rs", "fn main() { let n = 3; }\n").unwrap();
        assert!(highlighted.iter().flatten().any(|span| {
            matches!(
                span.class,
                SyntaxClass::Keyword | SyntaxClass::Function | SyntaxClass::Number
            )
        }));
        assert!(highlight_file("asset.unknown-rift-language", "plain\n").is_none());
    }

    #[test]
    fn every_syntax_class_is_reached_by_a_rust_source_line() {
        let highlighted = highlight_file(
            "matrix.rs",
            "// comment\nstruct Kind(u32);\nfn call() { if true { return; } let text = \"value\"; let n = 3 + 4; }\n",
        )
        .unwrap();
        let classes = highlighted
            .iter()
            .flatten()
            .map(|span| span.class)
            .collect::<Vec<_>>();
        for class in [
            SyntaxClass::Comment,
            SyntaxClass::String,
            SyntaxClass::Number,
            SyntaxClass::Type,
            SyntaxClass::Keyword,
            SyntaxClass::Operator,
        ] {
            assert!(
                classes.contains(&class),
                "missing class {class:?}: {classes:?}"
            );
        }
    }

    #[test]
    fn supplied_spans_are_kept_and_line_endings_are_normalised() {
        let supplied = vec![ViewerSpan {
            text: "styled".to_owned(),
            class: SyntaxClass::Function,
        }];
        assert_eq!(
            source_line(
                ViewerLineKind::Text,
                Some(1),
                Some(2),
                "plain",
                Some(&supplied)
            )
            .spans,
            supplied
        );
        assert_eq!(logical_lines("a\r\nb\n"), ["a", "b"]);
    }
}
