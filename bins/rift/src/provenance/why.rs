//! `rift why`: one line's whole story, and the reading of a shard that makes
//! the prompt half of it possible.
//!
//! Split from the blame document because the two answer different questions —
//! one line in depth against a whole file in breadth — and carry separate,
//! separately versioned `--json` contracts.

use std::collections::HashMap;

use rift_core::{Result, RiftError};
use rift_store::capture::{checkpoint_usage_windows, checkpoint_window_tokens, WindowScope};
use rift_store::{quote_usage_costs, CanonicalStore, CostQuoteScope, SqliteStore, Value};

use super::blame::{blamed_spans, parse_line_range, LineRange};
use super::render::answer;
use super::resolve::{commit_claims, resolve, CheckpointFacts, Reads, Resolved, Tag};
use super::safe_text::{safe_excerpt, safe_path, short, Label};
use super::worktree::UNKNOWN_WORKTREE;
use crate::cli::{exit, Outcome};

/// Envelope version for `rift why --json`. Bump on any breaking shape change.
const WHY_SCHEMA_VERSION: u32 = 1;

/// Split `PATH[:LINE]` (and reconcile the `--line` flag): the tail after the
/// LAST colon is a line spec only when it matches the line grammar, so
/// `C:\repo\a.rs` and a path that merely contains colons stay paths. A range
/// in either position is refused with the sentence the legacy backend used, because `why`
/// answers for one line.
pub(super) fn parse_why_spec(spec: &str, flag: Option<&str>) -> Result<(String, Option<usize>)> {
    let single = |s: &str| -> Result<usize> {
        let r = parse_line_range(s)?;
        if r.start != r.end {
            return Err(RiftError::validity(
                "why explains a single line; use rift blame for a range",
            ));
        }
        Ok(r.start)
    };
    let (path, colon_line) = match spec.rsplit_once(':') {
        Some((head, tail))
            if !head.is_empty()
                && tail.bytes().any(|b| b.is_ascii_digit())
                && tail.bytes().all(|b| b.is_ascii_digit() || b == b'-') =>
        {
            // Line-LIKE but malformed (`:0`, `:3-`, `:1-2-3`, a range) errors
            // here rather than being blamed as a path that cannot exist.
            (head.to_owned(), Some(single(tail)?))
        }
        _ => (spec.to_owned(), None),
    };
    let flag_line = flag.map(single).transpose()?;
    if colon_line.is_some() && flag_line.is_some() {
        return Err(RiftError::validity(
            "give the line once: PATH:LINE or --line N, not both",
        ));
    }
    Ok((path, colon_line.or(flag_line)))
}

/// Best-effort prompt text out of one archived transcript record. The record
/// shapes across captured dialects: `message` as a string, `message.content`
/// as a string or as `[{type:"text",text}]` blocks, or `payload.text`. When no
/// string is found the raw record line stands in (it is already redacted), and
/// the caller says which happened.
fn extract_prompt_text(v: &serde_json::Value) -> Option<String> {
    if let Some(s) = v.get("message").and_then(serde_json::Value::as_str) {
        return Some(s.to_owned());
    }
    if let Some(c) = v.pointer("/message/content") {
        if let Some(s) = c.as_str() {
            return Some(s.to_owned());
        }
        if let Some(items) = c.as_array() {
            let text: Vec<&str> = items
                .iter()
                .filter_map(|i| i.get("text").and_then(serde_json::Value::as_str))
                .collect();
            if !text.is_empty() {
                return Some(text.join("\n"));
            }
        }
    }
    v.pointer("/payload/text")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
}

/// The redacted prompt as read from the chosen checkpoint's own shard.
pub(crate) struct PromptView {
    /// Extracted (or raw-record) prompt text — already-redacted stored bytes.
    pub(crate) text: String,
    /// True when a text field was extracted; false when the raw record stands in.
    pub(crate) extracted: bool,
    /// 0-based non-empty line index into the shard transcript.
    pub(crate) line: usize,
    /// SHA-256 of the line's bytes — the same address `digest.intent.json`
    /// records, so the claim is checkable against the shard.
    pub(crate) sha256: String,
}

impl PromptView {
    /// The one JSON shape a prompt has, wherever it is served.
    pub(crate) fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "text": self.text,
            "extracted": self.extracted,
            "line": self.line,
            "sha256": self.sha256,
        })
    }

    /// A view for renderer tests: fixed address, extracted text.
    #[cfg(test)]
    pub(crate) fn test_view(text: &str) -> Self {
        Self {
            text: text.to_owned(),
            extracted: true,
            line: 0,
            sha256: "00".repeat(32),
        }
    }
}

/// What the shard answered when asked for its redacted prompt. Three shapes,
/// because they render differently: only [`Self::NoTranscript`] may say
/// "pre-transcript era" — a transcript-carrying shard whose records hold no
/// prompt is a different (and differently-worded) absence. Both absences
/// serialize as `null` on the locked `--json` contracts.
pub(crate) enum ShardPrompt {
    /// The prompt, located and served from the shard's own transcript.
    Found(PromptView),
    /// A transcript artifact exists, but no prompt record was located in it.
    NoPromptRecord,
    /// The shard archives no transcript artifact at all, or the shard/walker
    /// is unavailable.
    NoTranscript,
}

impl ShardPrompt {
    /// The served view, when one was found — the single JSON projection point.
    pub(crate) fn view(&self) -> Option<&PromptView> {
        match self {
            Self::Found(p) => Some(p),
            Self::NoPromptRecord | Self::NoTranscript => None,
        }
    }
}

/// Read the redacted prompt for `checkpoint_id` from its shard: prefer the
/// intent digest's locator, fall back to the first `"type":"user"` record for
/// pre-digest shards.
///
/// Transcript resolution covers both writer eras: `transcript.jsonl` is the
/// redacted full transcript a **canon-3 native shard** archives (the same
/// bytes `digest.intent.json`'s `prompt_line` indexes); `full.jsonl` /
/// `sessions/0/full.jsonl` are the legacy-dialect names, kept for older
/// shards. `transcript.compact.jsonl` is deliberately not a prompt source:
/// the intent locator indexes the full transcript, and the pair always
/// travels together.
pub(crate) fn prompt_from_shard(root: &std::path::Path, checkpoint_id: &str) -> ShardPrompt {
    let Ok(walker) = rift_recall_substrate::ShardWalker::open(root) else {
        return ShardPrompt::NoTranscript;
    };
    let transcript = ["transcript.jsonl", "full.jsonl", "sessions/0/full.jsonl"]
        .into_iter()
        .find_map(|name| walker.read_artifact_any(checkpoint_id, name).ok().flatten());
    let Some(transcript) = transcript else {
        return ShardPrompt::NoTranscript;
    };
    let lines: Vec<&[u8]> = transcript
        .split(|b| *b == b'\n')
        .filter(|l| !l.is_empty())
        .collect();
    let located = walker
        .read_artifact_any(checkpoint_id, "digest.intent.json")
        .ok()
        .flatten()
        .and_then(|d| {
            let v: serde_json::Value = serde_json::from_slice(&d).ok()?;
            usize::try_from(v.pointer("/intent/prompt_line")?.as_u64()?).ok()
        });
    let line_idx = located.or_else(|| {
        lines.iter().position(|l| {
            serde_json::from_slice::<serde_json::Value>(l)
                .is_ok_and(|v| v.get("type").and_then(serde_json::Value::as_str) == Some("user"))
        })
    });
    let Some(line_idx) = line_idx else {
        return ShardPrompt::NoPromptRecord;
    };
    let Some(bytes) = lines.get(line_idx) else {
        return ShardPrompt::NoPromptRecord;
    };
    let record = serde_json::from_slice::<serde_json::Value>(bytes).ok();
    let (text, extracted) = match record.as_ref().and_then(extract_prompt_text) {
        Some(t) => (t, true),
        None => (String::from_utf8_lossy(bytes).into_owned(), false),
    };
    ShardPrompt::Found(PromptView {
        text,
        extracted,
        line: line_idx,
        sha256: rift_shadow::sha256_hex(&[bytes]),
    })
}

/// Recorded token totals and their current catalog quote, over one session's
/// calls: all of them for [`session_cost`], one checkpoint's usage window for
/// [`checkpoint_cost`].
#[derive(Clone, Debug, ::serde::Serialize)]
pub(crate) struct CostView {
    pub(crate) session_id: String,
    pub(crate) uncached_input: i64,
    pub(crate) cached_input: i64,
    pub(crate) cache_creation: i64,
    pub(crate) output: i64,
    /// Current quoted micro-USD; absent when no call has complete prices.
    pub(crate) amount_micros: Option<i64>,
}

/// Sum one session's recorded token classes and cost. `Ok(None)` when the
/// session has no recorded model calls at all.
pub(crate) async fn session_cost(
    store: &SqliteStore,
    session_id: &str,
) -> Result<Option<CostView>> {
    let rows = store
        .query(
            "SELECT COUNT(*), COALESCE(SUM(tu.uncached_input),0), \
             COALESCE(SUM(tu.cached_input),0), COALESCE(SUM(tu.cache_creation),0), \
             COALESCE(SUM(tu.output),0) \
             FROM token_usage tu \
             JOIN model_call mc ON mc.call_id = tu.call_id \
             JOIN turn t ON t.turn_id = mc.turn_id \
             WHERE t.session_id = ?",
            &[Value::text(session_id.to_owned())],
        )
        .await?;
    let Some(r) = rows.first() else {
        return Ok(None);
    };
    if r.get_i64(0)? == 0 {
        return Ok(None);
    }
    let mut view = CostView {
        session_id: session_id.to_owned(),
        uncached_input: r.get_i64(1)?,
        cached_input: r.get_i64(2)?,
        cache_creation: r.get_i64(3)?,
        output: r.get_i64(4)?,
        amount_micros: None,
    };
    view.amount_micros = rift_store::quote_usage_costs(
        store,
        rift_store::CostQuoteScope::Session(session_id),
        i64::MAX,
    )
    .await?
    .get(session_id)
    .copied();
    Ok(Some(view))
}

/// What the model calls a checkpoint froze cost: its first session's calls
/// since that session's previous checkpoint, up to this one — the store's usage
/// window, read through the same join the pull verbs' provenance reads, so
/// `why` and a delta never disagree about a checkpoint's figure. `Ok(None)` when
/// the checkpoint has no window (undated, or no session) or the window holds no
/// settled call.
///
/// A session total under a checkpoint's name would read as what that change
/// cost; [`session_cost`] stays for the callers that name a session.
pub(super) async fn checkpoint_cost(
    store: &SqliteStore,
    checkpoint_id: &str,
) -> Result<Option<CostView>> {
    let scope = WindowScope::Checkpoint(checkpoint_id);
    let Some(window) = checkpoint_usage_windows(store, scope)
        .await?
        .into_iter()
        .next()
    else {
        return Ok(None);
    };
    let Some(tokens) = checkpoint_window_tokens(store, scope)
        .await?
        .remove(checkpoint_id)
    else {
        return Ok(None);
    };
    let quotes =
        quote_usage_costs(store, CostQuoteScope::CheckpointWindows(scope), i64::MAX).await?;
    Ok(Some(CostView {
        session_id: window.session_id,
        uncached_input: tokens.uncached_input,
        cached_input: tokens.cached_input,
        cache_creation: tokens.cache_creation,
        output: tokens.output,
        amount_micros: quotes.get(checkpoint_id).copied(),
    }))
}

/// `rift why <file>[:LINE]`: one line's full story — attribution, agent,
/// model, checkpoint, session, commit, the REDACTED stored prompt, the
/// four-class cost of the calls the selected checkpoint froze, and the
/// policy-gated handle to ladder deeper. Reads only: stored `RedactedBytes` artifacts from the shard
/// (the redaction gate ran at capture; nothing is sealed here) and projection
/// rows.
pub(crate) async fn why(
    store: &SqliteStore,
    root: &std::path::Path,
    spec: &str,
    line_flag: Option<&str>,
) -> Result<Outcome> {
    let (file, line) = parse_why_spec(spec, line_flag)?;
    if file.trim().is_empty() {
        return Err(RiftError::validity("why needs a path"));
    }
    let repo = rift_git::discover(root)?;
    let asked = line.map(|n| LineRange { start: n, end: n });
    let Some(all_spans) = blamed_spans(&repo, &file, asked)? else {
        return Ok(untracked_answer(&file, line));
    };
    let res = resolve(store, all_spans, |oid| commit_claims(&repo, oid)).await?;
    let Some(row) = answering_row(&res.rows, line) else {
        return Ok(Outcome::coded(
            exit::PRECONDITION,
            format!("{} has no blameable lines", safe_path(&file)),
            serde_json::json!({ "error": "not_answerable", "file": file, "line": line }),
        ));
    };

    let checkpoint = row.selection.as_ref().and_then(|sel| {
        res.facts.get(&sel.checkpoint_id).map(|f| {
            serde_json::json!({
                "id": sel.checkpoint_id,
                "created_at_ms": f.created_at_ms,
                "files_touched": f.files_touched.len(),
                "strategy": f.strategy,
                "worktree": f.worktree,
            })
        })
    });
    let prompt = row
        .selection
        .as_ref()
        .map(|sel| prompt_from_shard(root, &sel.checkpoint_id));
    let cost = cost_answer(store, row, &res.facts).await?;
    let file_handle = std::fs::read(root.join(&file))
        .ok()
        .map(|bytes| format!("file:{file}@sha256:{}", rift_shadow::sha256_hex(&[&bytes])));
    Ok(render_why(&WhyView {
        file: &file,
        line,
        row,
        checkpoint,
        prompt,
        cost,
        file_handle,
        reads: res.reads,
    }))
}

/// git's refusal, worded: an untracked path, or a line past the end of the
/// file. Both candidate causes are named, because `-L` past the end fails the
/// same way an untracked path does and the verb cannot tell them apart.
fn untracked_answer(file: &str, line: Option<usize>) -> Outcome {
    let what = match line {
        Some(n) => format!(
            "cannot answer for {} line {n}: path untracked or line past end of file",
            safe_path(file)
        ),
        None => format!(
            "cannot answer for {}: not a tracked path in this worktree",
            safe_path(file)
        ),
    };
    Outcome::coded(
        exit::PRECONDITION,
        what,
        serde_json::json!({
            "error": "not_answerable",
            "file": file,
            "line": line,
        }),
    )
}

/// The row that answers: the asked line's span, or — for a whole-file question
/// — the dominant one: most evidence-backed lines, then most lines, then
/// earliest position. Deterministic, and the basis is printed, not implied.
fn answering_row(rows: &[Resolved], line: Option<usize>) -> Option<&Resolved> {
    match line {
        Some(n) => rows.iter().find(|r| r.span.start <= n && n <= r.span.end),
        None => rows.iter().max_by_key(|r| {
            let width = r.span.end - r.span.start + 1;
            let evidence = r.selection.as_ref().is_some_and(|s| s.basis.is_evidence());
            (
                usize::from(evidence) * width,
                usize::from(r.selection.is_some()) * width,
                std::cmp::Reverse(r.span.start),
            )
        }),
    }
}

/// What the cost paragraph answers, decided where the checkpoint's recorded
/// time is known, so the renderer stays a pure function.
enum CostAnswer {
    /// No selected checkpoint with a session: the paragraph is silent.
    Silent,
    /// The selected checkpoint records no time, so no window frames its calls
    /// and no figure is known — never a zero.
    Undated,
    /// The selected checkpoint's usage window, and what its calls cost —
    /// `None` when the window holds no settled call.
    Window {
        checkpoint_id: String,
        cost: Option<CostView>,
    },
}

/// The cost answer for the answering row: the selected checkpoint's usage
/// window when the checkpoint is dated and archives a session.
async fn cost_answer(
    store: &SqliteStore,
    row: &Resolved,
    facts: &HashMap<String, CheckpointFacts>,
) -> Result<CostAnswer> {
    let Some(selection) = row.selection.as_ref() else {
        return Ok(CostAnswer::Silent);
    };
    if row.sessions.is_empty() {
        return Ok(CostAnswer::Silent);
    }
    let checkpoint_id = selection.checkpoint_id.clone();
    let dated = facts.get(&checkpoint_id).and_then(|f| f.created_at_ms);
    if dated.is_none() {
        return Ok(CostAnswer::Undated);
    }
    let cost = checkpoint_cost(store, &checkpoint_id).await?;
    Ok(CostAnswer::Window {
        checkpoint_id,
        cost,
    })
}

/// Everything [`render_why`] needs, resolved — so the rendering is a pure
/// function a test can drive without a store, a repository, or a shard.
struct WhyView<'a> {
    file: &'a str,
    line: Option<usize>,
    row: &'a Resolved,
    checkpoint: Option<serde_json::Value>,
    prompt: Option<ShardPrompt>,
    cost: CostAnswer,
    file_handle: Option<String>,
    reads: Reads,
}

/// The human and JSON halves of one `why` answer. Every transcript- or
/// adapter-derived string goes through the existing neutralizers: [`Label`]
/// (agent/model), [`safe_path`] (the path), [`safe_excerpt`] (the prompt
/// block).
fn render_why(v: &WhyView<'_>) -> Outcome {
    let WhyView {
        file,
        line,
        row,
        checkpoint,
        prompt,
        cost,
        file_handle,
        reads,
    } = v;
    let (line, row) = (*line, *row);
    let tag = Tag::of(row.counts.as_ref());
    let mut human = match line {
        Some(n) => format!("rift why {}:{n}\n", safe_path(file)),
        None => format!("rift why {}\n", safe_path(file)),
    };
    human.push_str(&format!(
        "  {:>11}  {}  {}{}\n",
        format!("{}-{}", row.span.start, row.span.end),
        short(&row.span.commit),
        answer(row, tag),
        import_note_of(checkpoint.as_ref()),
    ));
    // Worktree provenance on the one-line verb: the answering checkpoint's shard
    // member, `unknown` when it carries none — never `main`.
    human.push_str(&format!(
        "  archived_in: {}\n",
        archived_in_of(checkpoint.as_ref())
    ));
    human.push_str(&prompt_block(prompt.as_ref()));
    human.push_str(&cost_block(cost));
    if let Some(h) = &file_handle {
        human.push_str(&format!(
            "  ladder deeper (policy-gated): rift recall retrieve-exact \"{h}\" --reason <why>\n"
        ));
    }

    let json = serde_json::json!({
        "schema_version": WHY_SCHEMA_VERSION,
        "file": file,
        "line": line,
        "span": {
            "start": row.span.start,
            "end": row.span.end,
            "commit": row.span.commit,
            "path": row.span.path,
        },
        "tag": tag,
        "selection": row.selection,
        "attribution": row.counts,
        "sessions": row.sessions,
        "checkpoint": checkpoint,
        "prompt": prompt.as_ref().and_then(ShardPrompt::view).map(PromptView::to_json),
        "cost": served_cost(cost),
        "handles": { "file": file_handle },
        "reads": reads,
    });
    Outcome::ok(human.trim_end().to_owned(), json)
}

/// The prompt paragraph, bounded to eight lines of 600 escaped characters.
///
/// The three shapes are worded apart on purpose: only [`ShardPrompt::NoTranscript`]
/// may say "pre-transcript era", because a transcript-carrying shard that
/// holds no prompt record is a different absence and saying otherwise would
/// describe the archive wrongly.
fn prompt_block(prompt: Option<&ShardPrompt>) -> String {
    match prompt {
        Some(ShardPrompt::Found(p)) => {
            let mut out = format!(
                "  prompt (redacted, line {} of the checkpoint transcript{}):\n",
                p.line,
                if p.extracted { "" } else { ", raw record" }
            );
            for l in safe_excerpt(&p.text, 600).lines().take(8) {
                out.push_str(&format!("    | {l}\n"));
            }
            out
        }
        Some(ShardPrompt::NoPromptRecord) => {
            "  prompt: the archived transcript holds no prompt record for this checkpoint\n"
                .to_owned()
        }
        Some(ShardPrompt::NoTranscript) => {
            "  prompt: not archived for this checkpoint (pre-transcript era, or shard unavailable)\n"
                .to_owned()
        }
        None => String::new(),
    }
}

/// The cost paragraph, named after the checkpoint whose window it reads.
///
/// The three silences are worded apart: a row with no checkpoint session has
/// nothing to say; an undated checkpoint has no window, which is unknown, not
/// zero; a dated window with no settled call is a fact worth printing.
fn cost_block(answer: &CostAnswer) -> String {
    let (checkpoint_id, c) = match answer {
        CostAnswer::Silent => return String::new(),
        CostAnswer::Undated => {
            return "  cost: unknown, the checkpoint records no time to bound its model calls\n"
                .to_owned()
        }
        CostAnswer::Window { cost: None, .. } => {
            return "  cost: no model calls recorded in this checkpoint's window\n".to_owned()
        }
        CostAnswer::Window {
            checkpoint_id,
            cost: Some(c),
        } => (checkpoint_id, c),
    };
    format!(
        "  cost (checkpoint {}, session {}): in {} / cached {} / cache-w {} / out {} tokens{}\n",
        short(checkpoint_id),
        Label::of(&c.session_id),
        c.uncached_input,
        c.cached_input,
        c.cache_creation,
        c.output,
        c.amount_micros.map_or(String::new(), |m| format!(
            "  (${}.{:06})",
            m / 1_000_000,
            m.rem_euclid(1_000_000)
        )),
    )
}

/// The `--json` cost object: the window's figures, or `null` for every
/// silence — the same key and shape a whole-session figure had.
fn served_cost(answer: &CostAnswer) -> Option<&CostView> {
    match answer {
        CostAnswer::Window { cost, .. } => cost.as_ref(),
        CostAnswer::Silent | CostAnswer::Undated => None,
    }
}

/// The `archived_in` value a rendered checkpoint object carries, as a human
/// line prints it: the member's name, or `unknown`.
///
/// Reads the document rather than the facts map, because the document is what
/// the caller already resolved and two readers of the same fact are two places
/// for it to disagree.
fn archived_in_of(checkpoint: Option<&serde_json::Value>) -> String {
    checkpoint
        .and_then(|c| c.get("worktree"))
        .and_then(serde_json::Value::as_str)
        .map_or_else(|| UNKNOWN_WORKTREE.to_owned(), str::to_owned)
}

/// The imported-transcript note over the `why` document's checkpoint object,
/// which carries the same `strategy` the JSON serves — one source for both
/// halves, read the same way [`archived_in_of`] reads the member.
fn import_note_of(checkpoint: Option<&serde_json::Value>) -> &'static str {
    let imported = checkpoint
        .and_then(|c| c.get("strategy"))
        .and_then(serde_json::Value::as_str)
        == Some("import");
    if imported {
        " (imported transcript, read-only)"
    } else {
        ""
    }
}

#[cfg(test)]
mod tests {
    use rift_shadow::attribution::provenance::Selection;
    use rift_shadow::attribution::SelectionBasis;

    use super::*;
    use crate::provenance::blame::Span;

    /// `why`'s spec grammar: the LAST colon carries the line only when its
    /// tail is line-LIKE; a range is refused with the single-line sentence in
    /// both positions; the line may be given once.
    #[test]
    fn the_why_spec_takes_one_line_from_the_last_colon_or_the_flag() {
        let ok = |s: &str, f: Option<&str>| parse_why_spec(s, f).unwrap();
        assert_eq!(ok("src/a.rs:42", None), ("src/a.rs".into(), Some(42)));
        assert_eq!(ok("src/a.rs", None), ("src/a.rs".into(), None));
        assert_eq!(ok("src/a.rs", Some("7")), ("src/a.rs".into(), Some(7)));
        assert_eq!(ok("a:b:9", None), ("a:b".into(), Some(9)));
        // Colon tails that are not line-like stay part of the path.
        assert_eq!(ok("C:\\repo\\a.rs", None), ("C:\\repo\\a.rs".into(), None));
        assert_eq!(ok("a.rs:-", None), ("a.rs:-".into(), None));
        // Ranges: refused with the single-line sentence, both spellings.
        for (s, f) in [("a.rs:2-5", None), ("a.rs", Some("2-5"))] {
            let e = parse_why_spec(s, f).unwrap_err().to_string();
            assert!(e.contains("single line"), "{e}");
        }
        // Line-like but malformed is an error, not a path that cannot exist.
        assert!(parse_why_spec("a.rs:0", None).is_err());
        assert!(parse_why_spec("a.rs:3-", None).is_err());
        assert!(parse_why_spec("a.rs:1-2-3", None).is_err());
        // Given twice is refused.
        assert!(parse_why_spec("a.rs:2", Some("3")).is_err());
    }

    /// Prompt text extraction over the shapes captured dialects actually
    /// write; a shape with no string field falls back to the raw record.
    #[test]
    fn prompt_text_is_extracted_across_dialect_shapes() {
        let s = |j: serde_json::Value| extract_prompt_text(&j);
        assert_eq!(
            s(serde_json::json!({"type":"user","message":"hi"})).as_deref(),
            Some("hi")
        );
        assert_eq!(
            s(serde_json::json!({"message":{"content":"hey"}})).as_deref(),
            Some("hey")
        );
        assert_eq!(
            s(serde_json::json!({"message":{"content":[
                {"type":"text","text":"a"},{"type":"text","text":"b"}]}}))
            .as_deref(),
            Some("a\nb")
        );
        assert_eq!(
            s(serde_json::json!({"payload":{"text":"p"}})).as_deref(),
            Some("p")
        );
        assert_eq!(s(serde_json::json!({"type":"user","message":[1]})), None);
    }

    /// Rw2's misspeak, pinned at the renderer: a shard that DOES carry a
    /// transcript but holds no prompt record must not be described as
    /// "pre-transcript era" — that sentence is reserved for the shard that
    /// archives no transcript at all. Both absences serialize as `null`, so
    /// the locked `--json` contracts do not move.
    #[test]
    fn the_two_prompt_absences_render_distinct_lines() {
        let row = Resolved {
            span: Span {
                start: 1,
                end: 1,
                commit: "c".repeat(40),
                path: "a.rs".into(),
            },
            selection: Some(Selection {
                checkpoint_id: "ab".repeat(32),
                basis: SelectionBasis::FileTouched,
                considered: 1,
                resolved: true,
            }),
            counts: None,
            sessions: vec![],
        };
        let view = |prompt: ShardPrompt| WhyView {
            file: "a.rs",
            line: Some(1),
            row: &row,
            checkpoint: None,
            prompt: Some(prompt),
            cost: CostAnswer::Silent,
            file_handle: None,
            reads: Reads::default(),
        };
        let with_transcript = render_why(&view(ShardPrompt::NoPromptRecord));
        assert!(
            with_transcript
                .human
                .contains("the archived transcript holds no prompt record"),
            "{}",
            with_transcript.human
        );
        assert!(
            !with_transcript.human.contains("pre-transcript era"),
            "a transcript-carrying shard must not be called pre-transcript: {}",
            with_transcript.human
        );
        assert_eq!(with_transcript.json["prompt"], serde_json::Value::Null);

        let without = render_why(&view(ShardPrompt::NoTranscript));
        assert!(
            without
                .human
                .contains("pre-transcript era, or shard unavailable"),
            "{}",
            without.human
        );
        assert_eq!(without.json["prompt"], serde_json::Value::Null);
    }

    /// One settled call of session `s`, alone in turn `ordinal`, priced at one
    /// micro-USD per token by [`sibling_store`]'s catalog.
    async fn sibling_call(store: &SqliteStore, ordinal: i64, at: i64, input: i64) {
        let turn_id = format!("turn-{ordinal}");
        let turn = rift_store::TurnRow {
            turn_id: turn_id.clone(),
            session_id: "s".into(),
            ordinal,
            started_at: at,
            schema_version: 1,
        };
        rift_store::record_turn(store, &turn).await.unwrap();
        let call = rift_store::ModelCallRow {
            call_id: format!("call-{ordinal}"),
            turn_id,
            model: "m".into(),
            started_at: at,
            schema_version: 1,
        };
        rift_store::record_model_call(store, &call).await.unwrap();
        let usage = rift_store::TokenUsageRow {
            call_id: call.call_id,
            uncached_input: input,
            cached_input: 0,
            cache_creation: 0,
            output: 0,
            counter_method: "provider_reported".into(),
            schema_version: 1,
        };
        rift_store::record_token_usage(store, &usage).await.unwrap();
    }

    /// Session `s` checkpointed twice, at 100 (`ck-1`) and 200 (`ck-2`), with
    /// a call in each window.
    async fn sibling_store() -> SqliteStore {
        let store = SqliteStore::open_in_memory().unwrap();
        store.create_canonical_schema().unwrap();
        for class in ["uncached_input", "cached_input", "cache_creation", "output"] {
            let price = rift_store::PriceTableRow {
                price_id: class.into(),
                model: "m".into(),
                token_class: class.into(),
                unit_price_micros: 1_000_000,
                currency: "USD".into(),
                valid_from: 0,
                valid_to: None,
                recorded_at: 0,
                supersedes_id: None,
                schema_version: 1,
            };
            rift_store::record_price(&store, &price).await.unwrap();
        }
        sibling_call(&store, 1, 50, 1_000).await;
        sibling_call(&store, 2, 150, 20).await;
        for (ck, at) in [("ck-1", 100), ("ck-2", 200)] {
            let columns = [
                "checkpoint_id",
                "strategy",
                "created_at",
                "merkle_root",
                "schema_version",
            ];
            let row = [
                Value::text(ck.to_owned()),
                Value::text("manual-commit".to_owned()),
                Value::Int(at),
                Value::text("00".to_owned()),
                Value::Int(1),
            ];
            store.insert("checkpoint", &columns, &row).await.unwrap();
            let sic = rift_store::SessionInCheckpointRow {
                checkpoint_id: ck.into(),
                session_id: "s".into(),
                ordinal: 0,
                schema_version: 1,
            };
            rift_store::record_session_in_checkpoint(&store, &sic)
                .await
                .unwrap();
        }
        store
    }

    #[tokio::test]
    async fn why_over_a_checkpoint_with_an_earlier_sibling_prints_only_its_own_window() {
        let store = sibling_store().await;
        let row = Resolved {
            span: Span {
                start: 1,
                end: 1,
                commit: "c".repeat(40),
                path: "a.rs".into(),
            },
            selection: Some(Selection {
                checkpoint_id: "ck-2".into(),
                basis: SelectionBasis::FileTouched,
                considered: 1,
                resolved: true,
            }),
            counts: None,
            sessions: vec![crate::provenance::resolve::SessionRef {
                session_id: "s".into(),
                agent: Label::of("claude-code"),
                model: Label::of("m"),
            }],
        };
        let mut facts = HashMap::new();
        let dated = CheckpointFacts {
            created_at_ms: Some(200),
            ..CheckpointFacts::default()
        };
        facts.insert("ck-2".to_owned(), dated);
        let cost = cost_answer(&store, &row, &facts).await.unwrap();
        let out = render_why(&WhyView {
            file: "a.rs",
            line: Some(1),
            row: &row,
            checkpoint: None,
            prompt: None,
            cost,
            file_handle: None,
            reads: Reads::default(),
        });
        assert!(
            out.human
                .contains("cost (checkpoint ck-2, session s): in 20 / cached 0"),
            "{}",
            out.human
        );
        assert_eq!(out.json["cost"]["uncached_input"], 20, "{}", out.json);
        assert_eq!(out.json["cost"]["amount_micros"], 20, "{}", out.json);
    }
}
