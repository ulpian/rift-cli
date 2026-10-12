//! `rift checkpoint explain [ID|SHA] [--full|--short]` — the native render of
//! one checkpoint from its own shard + the projection.
//!
//! **No `--generate`**: Rift executes no model, so the
//! intent/outcome slots are filled mechanically by the condense-time digest
//! set — `digest.intent.json` locates the redacted prompt, `digest.failures`/
//! `digest.log`/`digest.shape` carry the mechanical outcome — rather than by
//! an LLM summary that would couple a provenance verb to a cloud.
//!
//! Target resolution mirrors the legacy backend's `explain` (consulted read-only at the
//! pinned tag and cited as `upstream: explain.go`): a checkpoint id prefix wins, a
//! commit-ish falls back to the `Rift-Checkpoint` trailer walk, and no target
//! means `HEAD`. A multi-trailer (squash) commit resolves ALL claims and picks
//! the latest by `created_at` — the item-4 rule; the legacy backend's `candidates[0]`
//! names opposite commits in local vs forge squashes, which is the measured
//! reason ordinal order cannot be trusted.
//!
//! Serving discipline: everything shard-read is **stored redacted bytes**
//! (the gate ran at capture; nothing is sealed here), and every
//! transcript-derived string reaches the terminal only through the existing
//! neutralizers (`safe_excerpt`, `Label`, `safe_path` equivalents in
//! `provenance`). `--json` carries no transcript bytes at all — agents ladder
//! through policy-gated handles, not through this envelope.

use rift_core::{Result, RiftError};
use rift_recall_substrate::{
    build_checkpoint_catalogue, resolve_explicit_target, GixSubstrate, ResolutionKind, ShardWalker,
};
use rift_store::{CanonicalStore, SqliteStore, Value};

use crate::cli::{exit, Outcome};
use crate::provenance::{
    checkpoint_worktree, prompt_from_shard, safe_excerpt, session_cost, worktree_label, CostView,
    Label, PromptView, ShardPrompt,
};

/// Envelope version for `rift checkpoint explain --json`.
const SCHEMA_VERSION: u32 = 1;

/// How much of the story to render.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Depth {
    /// Header + sessions + digest summary only.
    Short,
    /// Everything but the transcript.
    Normal,
    /// Everything, plus the compact transcript.
    Full,
}

impl Depth {
    /// From the two flags; both at once is a refusal.
    pub(crate) fn of(full: bool, short: bool) -> Result<Self> {
        match (full, short) {
            (true, true) => Err(RiftError::validity("--full and --short exclude each other")),
            (true, false) => Ok(Self::Full),
            (false, true) => Ok(Self::Short),
            (false, false) => Ok(Self::Normal),
        }
    }
}

/// Whether `s` could be a checkpoint id (prefix): lowercase hex, 7..=64.
#[cfg(test)]
fn id_like(s: &str) -> bool {
    (7..=64).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Latest-by-`created_at` among the ids a commit claims (the item-4 squash
/// rule). Ids the projection does not know sort last; ordinal breaks ties.
async fn latest_claimed(store: &SqliteStore, ids: &[String]) -> Result<Option<String>> {
    let mut best: Option<(Option<i64>, std::cmp::Reverse<usize>, String)> = None;
    for (ordinal, id) in ids.iter().enumerate() {
        let created = store
            .query_opt_i64(
                "SELECT created_at FROM checkpoint WHERE checkpoint_id = ?",
                &[Value::text(id.clone())],
            )
            .await?;
        let key = (created, std::cmp::Reverse(ordinal), id.clone());
        if best.as_ref().is_none_or(|b| key > *b) {
            best = Some(key);
        }
    }
    Ok(best.map(|(_, _, id)| id))
}

/// Resolve `[ID|SHA]` to one checkpoint id, or the refusal that says why not.
async fn resolve_target(
    store: &SqliteStore,
    root: &std::path::Path,
    target: Option<&str>,
) -> Result<std::result::Result<String, Outcome>> {
    let not_found = |what: String, t: &str| {
        Outcome::coded(
            exit::PRECONDITION,
            what,
            serde_json::json!({ "error": "no_checkpoint", "target": t }),
        )
    };
    let reader = GixSubstrate::open(root)?;
    let catalogue = build_checkpoint_catalogue(&reader)?;
    // Explain deliberately retains its established no-target = HEAD behavior;
    // the viewer passes no target differently and selects catalogue newest.
    let requested = target.unwrap_or("HEAD");
    let resolved = resolve_explicit_target(&reader, &catalogue, requested)?;
    match resolved.kind {
        ResolutionKind::ExactCheckpoint | ResolutionKind::CheckpointPrefix => Ok(Ok(resolved
            .candidates
            .first()
            .map(|candidate| candidate.checkpoint_id.clone())
            .ok_or_else(|| RiftError::validity("checkpoint resolver returned no candidate"))?)),
        ResolutionKind::AmbiguousCheckpointPrefix => Ok(Err(Outcome::coded(
            exit::PRECONDITION,
            format!("checkpoint prefix {requested} is ambiguous"),
            serde_json::json!({
                "error": "ambiguous_prefix",
                "target": requested,
                "candidates": resolved.candidates.iter().map(|c| &c.checkpoint_id).collect::<Vec<_>>(),
                "truncated": resolved.truncated,
            }),
        ))),
        ResolutionKind::GitCommit => {
            let ids: Vec<String> = resolved
                .candidates
                .iter()
                .map(|candidate| candidate.checkpoint_id.clone())
                .collect();
            if let Some(id) = latest_claimed(store, &ids).await? {
                Ok(Ok(id))
            } else {
                let oid = resolved.commit_oid.unwrap_or_default();
                let message = if target.is_none() {
                    format!("HEAD ({}) claims no checkpoint", &oid[..12.min(oid.len())])
                } else {
                    format!("commit {} claims no checkpoint", &oid[..12.min(oid.len())])
                };
                Ok(Err(not_found(message, requested)))
            }
        }
        ResolutionKind::Missing => {
            let message = if target.is_none() {
                "no HEAD checkpoint to explain".to_owned()
            } else {
                format!("{requested}: neither a known checkpoint id nor a commit")
            };
            Ok(Err(not_found(message, requested)))
        }
    }
}

/// One digest family, parsed when present.
fn digest(walker: &ShardWalker, id: &str, name: &str) -> Option<serde_json::Value> {
    let bytes = walker.read_artifact_any(id, name).ok().flatten()?;
    serde_json::from_slice(&bytes).ok()
}

/// `rift checkpoint explain`.
pub(crate) async fn checkpoint_explain(
    store: &SqliteStore,
    root: &std::path::Path,
    target: Option<&str>,
    depth: Depth,
) -> Result<Outcome> {
    let id = match resolve_target(store, root, target).await? {
        Ok(id) => id,
        Err(refusal) => return Ok(refusal),
    };
    let walker = ShardWalker::open(root).ok();
    let meta: Option<serde_json::Value> = walker
        .as_ref()
        .and_then(|w| w.read_artifact_any(&id, "metadata.json").ok().flatten())
        .and_then(|b| serde_json::from_slice(&b).ok());
    let created_at = recorded_created_at(store, &id).await?;
    let sessions = session_rows(store, &id).await?;
    let worktree = checkpoint_worktree(store, &id).await;
    let digests = digests_of(walker.as_ref(), &id);
    let prompt = match depth {
        Depth::Short => None,
        _ => Some(prompt_from_shard(root, &id)),
    };
    let attribution = attribution_rows(store, &id).await?;
    let costs = session_costs(store, &sessions).await?;
    let transcript = match depth {
        Depth::Full => walker.as_ref().and_then(|w| transcript_of(w, &id)),
        _ => None,
    };
    let subagents = walker
        .as_ref()
        .map(|w| subagents_of(w, &id, meta.as_ref()))
        .unwrap_or_default();
    Ok(render(&View {
        id,
        depth,
        meta,
        created_at,
        sessions,
        digests,
        prompt,
        attribution,
        costs,
        transcript,
        worktree,
        subagents,
    }))
}

/// The checkpoint's recorded creation time, `None` when none was recorded.
///
/// The pre-fix sentinel (`-1`) is "no recorded time", classified here exactly
/// as `provenance::checkpoint_facts` classifies it — the JSON answers null and
/// the human header answers `?`, never an instant that does not exist.
async fn recorded_created_at(store: &SqliteStore, id: &str) -> Result<Option<i64>> {
    Ok(store
        .query_opt_i64(
            "SELECT created_at FROM checkpoint WHERE checkpoint_id = ?",
            &[Value::text(id.to_owned())],
        )
        .await?
        .filter(|&ms| ms != rift_shadow::CREATED_AT_UNKNOWN))
}

/// Every digest family in its fixed order, each parsed when the shard is here;
/// without a shard walker every family is listed as absent rather than
/// dropped, so the view always names all four.
fn digests_of(
    walker: Option<&ShardWalker>,
    id: &str,
) -> Vec<(&'static str, Option<serde_json::Value>)> {
    walker.map_or_else(
        || {
            vec![
                ("digest.failures.json", None),
                ("digest.intent.json", None),
                ("digest.log.json", None),
                ("digest.shape.json", None),
            ]
        },
        |w| {
            [
                "digest.failures.json",
                "digest.intent.json",
                "digest.log.json",
                "digest.shape.json",
            ]
            .into_iter()
            .map(|n| (n, digest(w, id, n)))
            .collect()
        },
    )
}

/// The cost of the first four sessions the checkpoint recorded, skipping any
/// session without a cost row.
async fn session_costs(store: &SqliteStore, sessions: &[SessionRow]) -> Result<Vec<CostView>> {
    let mut costs = Vec::new();
    for s in sessions.iter().take(4) {
        if let Some(c) = session_cost(store, &s.session_id).await? {
            costs.push(c);
        }
    }
    Ok(costs)
}

/// The shard's transcript for `--full`: the compact form when the shard has
/// one, the full log otherwise.
fn transcript_of(walker: &ShardWalker, id: &str) -> Option<(&'static str, Vec<u8>)> {
    walker
        .read_artifact_any(id, "transcript.compact.jsonl")
        .ok()
        .flatten()
        .map(|b| ("transcript.compact.jsonl", b))
        .or_else(|| {
            walker
                .read_artifact_any(id, "full.jsonl")
                .ok()
                .flatten()
                .map(|b| ("full.jsonl", b))
        })
}

/// One subagent transcript the checkpoint sealed, as its
/// `tasks/<tool-use-id>/metadata.json` records it.
struct SubagentRow {
    tool_use_id: Label,
    agent_id: Label,
    subagent_type: Option<Label>,
    /// The sealed `transcript.jsonl`'s size; `None` when none was sealed.
    bytes: Option<u64>,
    /// Why no transcript was sealed (`missing`, `unreadable`, `unsealed`).
    absent: Option<Label>,
}

/// The subagents the shard's own task list names, each read from its subtree's
/// metadata. A task listed with no readable metadata is still listed, by name.
fn subagents_of(
    walker: &ShardWalker,
    id: &str,
    meta: Option<&serde_json::Value>,
) -> Vec<SubagentRow> {
    let listed = meta
        .and_then(|m| m.get("tasks"))
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str);
    listed
        .map(|tool_use_id| {
            let doc: Option<serde_json::Value> = walker
                .read_artifact_any(id, &format!("tasks/{tool_use_id}/metadata.json"))
                .ok()
                .flatten()
                .and_then(|b| serde_json::from_slice(&b).ok());
            let text = |k: &str| {
                doc.as_ref()
                    .and_then(|d| d.get(k))
                    .and_then(serde_json::Value::as_str)
                    .map(Label::of)
            };
            SubagentRow {
                tool_use_id: Label::of(tool_use_id),
                agent_id: text("agent_id").unwrap_or_else(|| Label::of("?")),
                subagent_type: text("subagent_type"),
                bytes: doc
                    .as_ref()
                    .and_then(|d| d.get("bytes"))
                    .and_then(serde_json::Value::as_u64),
                absent: text("task_transcript"),
            }
        })
        .collect()
}

/// One session the checkpoint recorded, label-filtered for the terminal.
struct SessionRow {
    session_id: String,
    agent: Label,
    model: Label,
}

async fn session_rows(store: &SqliteStore, id: &str) -> Result<Vec<SessionRow>> {
    let rows = store
        .query(
            "SELECT sic.session_id, COALESCE(s.agent, 'unknown'), COALESCE(s.model_primary, 'unknown') \
             FROM session_in_checkpoint sic \
             LEFT JOIN session s ON s.session_id = sic.session_id \
             WHERE sic.checkpoint_id = ? ORDER BY sic.ordinal",
            &[Value::text(id.to_owned())],
        )
        .await?;
    rows.iter()
        .map(|r| {
            Ok(SessionRow {
                session_id: r.get_text(0)?,
                agent: Label::of(&r.get_text(1)?),
                model: Label::of(&r.get_text(2)?),
            })
        })
        .collect()
}

/// Per-file attribution rows for the checkpoint (path, agent, human, conf).
async fn attribution_rows(store: &SqliteStore, id: &str) -> Result<Vec<(String, i64, i64, i64)>> {
    let rows = store
        .query(
            "SELECT fr.path, a.agent_added, a.human_added, a.confidence_milli \
             FROM attribution a \
             JOIN file_revision fr ON fr.file_revision_id = a.file_revision_id \
             WHERE a.checkpoint_id = ? ORDER BY fr.path",
            &[Value::text(id.to_owned())],
        )
        .await?;
    rows.iter()
        .map(|r| Ok((r.get_text(0)?, r.get_i64(1)?, r.get_i64(2)?, r.get_i64(3)?)))
        .collect()
}

/// Everything [`render`] needs, resolved.
struct View {
    id: String,
    depth: Depth,
    meta: Option<serde_json::Value>,
    created_at: Option<i64>,
    sessions: Vec<SessionRow>,
    digests: Vec<(&'static str, Option<serde_json::Value>)>,
    prompt: Option<ShardPrompt>,
    attribution: Vec<(String, i64, i64, i64)>,
    costs: Vec<CostView>,
    transcript: Option<(&'static str, Vec<u8>)>,
    /// The member the shard was archived in, from the
    /// PROJECTION rather than from `meta` — `metadata.json` carries the member
    /// too, but only when the shard is present locally, and a verb that
    /// answered `unknown` merely because the shard is elsewhere would be
    /// reporting its own reach as a fact about the archive.
    worktree: Option<Label>,
    /// The subagent transcripts the shard sealed.
    subagents: Vec<SubagentRow>,
}

/// The first 12 characters of an id.
fn short(id: &str) -> String {
    id.chars().take(12).collect()
}

fn render(v: &View) -> Outcome {
    let mut human = format!("checkpoint {}\n", v.id);
    header_lines(v, &mut human);
    for s in &v.sessions {
        human.push_str(&format!(
            "  session {}  {}/{}\n",
            Label::of(&s.session_id),
            s.agent,
            s.model
        ));
    }
    digest_summary_line(v, &mut human);
    if v.depth != Depth::Short {
        body_lines(v, &mut human);
        subagent_lines(v, &mut human);
    }
    if let Some((name, bytes)) = &v.transcript {
        human.push_str(&format!("  transcript ({name}):\n"));
        for line in bytes.split(|b| *b == b'\n').filter(|l| !l.is_empty()) {
            let text = String::from_utf8_lossy(line);
            human.push_str(&format!("    | {}\n", safe_excerpt(&text, 200)));
        }
    }
    let json = serde_json::json!({
        "schema_version": SCHEMA_VERSION,
        "checkpoint_id": v.id,
        "created_at_ms": v.created_at,
        // A top-level fact, not a re-read of `metadata`. `null` is
        // UNKNOWN — the human header spells it, and it is never `main`.
        "worktree": v.worktree,
        "metadata": v.meta,
        "sessions": v.sessions.iter().map(|s| serde_json::json!({
            "session_id": s.session_id, "agent": s.agent, "model": s.model,
        })).collect::<Vec<_>>(),
        "digests": v.digests.iter().map(|(n, d)| serde_json::json!({
            "name": n, "document": d,
        })).collect::<Vec<_>>(),
        "prompt": v.prompt.as_ref().and_then(ShardPrompt::view).map(PromptView::to_json),
        "attribution": v.attribution.iter().map(|(p, a, h, c)| serde_json::json!({
            "path": p, "agent_added": a, "human_added": h, "confidence_milli": c,
        })).collect::<Vec<_>>(),
        "costs": v.costs,
        "transcript_rendered": v.transcript.is_some(),
    });
    Outcome::ok(
        human.trim_end().to_owned(),
        with_subagents(json, &v.subagents),
    )
}

/// The metadata header: anchor, strategy, canon, creation time.
fn header_lines(v: &View, human: &mut String) {
    let m = |k: &str| {
        v.meta
            .as_ref()
            .and_then(|m| m.get(k))
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
    };
    let canon = v
        .meta
        .as_ref()
        .and_then(|m| m.get("canon_version"))
        .and_then(serde_json::Value::as_u64);
    human.push_str(&format!(
        "  archived_in: {}\n",
        worktree_label(v.worktree.as_ref())
    ));
    human.push_str(&format!(
        "  anchored: {}  strategy: {}  canon: {}  created_at_ms: {}\n",
        m("commit").as_deref().map_or_else(
            || "(shard not present locally)".to_owned(),
            |c| short(&Label::of(c).to_string())
        ),
        Label::of(&m("strategy").unwrap_or_else(|| "?".into())),
        canon.map_or_else(|| "?".into(), |c| c.to_string()),
        v.created_at.map_or_else(|| "?".into(), |t| t.to_string()),
    ));
}

/// One line summarizing which digest families the shard carries.
fn digest_summary_line(v: &View, human: &mut String) {
    let carried: Vec<&str> = v
        .digests
        .iter()
        .filter_map(|(n, d)| d.is_some().then_some(*n))
        .collect();
    human.push_str(&format!(
        "  digests: {}\n",
        if carried.is_empty() {
            "(none archived)".to_owned()
        } else {
            carried.join(" · ")
        }
    ));
}

/// The normal-depth body: prompt, files read, attribution, costs.
fn body_lines(v: &View, human: &mut String) {
    if let Some(p) = v.prompt.as_ref().and_then(ShardPrompt::view) {
        human.push_str(&format!(
            "  prompt (redacted, transcript line {}{}):\n",
            p.line,
            if p.extracted { "" } else { ", raw record" }
        ));
        for l in safe_excerpt(&p.text, 600).lines().take(8) {
            human.push_str(&format!("    | {l}\n"));
        }
    }
    let files = v
        .digests
        .iter()
        .find(|(n, _)| *n == "digest.shape.json")
        .and_then(|(_, d)| d.as_ref())
        .and_then(|d| d.get("files"))
        .and_then(serde_json::Value::as_array);
    if let Some(files) = files {
        human.push_str(&format!("  files read: {}\n", files.len()));
        for f in files.iter().take(12) {
            if let Some(p) = f.get("path").and_then(serde_json::Value::as_str) {
                human.push_str(&format!("    {}\n", Label::of(p)));
            }
        }
    }
    for (path, agent, humans, conf) in &v.attribution {
        human.push_str(&format!(
            "  attribution {}  agent={agent} human={humans} conf={}.{:03}\n",
            Label::of(path),
            conf / 1000,
            conf.rem_euclid(1000),
        ));
    }
    for c in &v.costs {
        human.push_str(&format!(
            "  tokens (session {}): in {} / cached {} / cache-w {} / out {}{}\n",
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
        ));
    }
}

/// The document with its `subagents` member, present only when the shard
/// sealed one: a checkpoint without subagents renders the document it always
/// did, byte for byte.
fn with_subagents(mut json: serde_json::Value, subagents: &[SubagentRow]) -> serde_json::Value {
    if subagents.is_empty() {
        return json;
    }
    let rows: Vec<serde_json::Value> = subagents
        .iter()
        .map(|s| {
            serde_json::json!({
                "tool_use_id": s.tool_use_id, "agent_id": s.agent_id,
                "subagent_type": s.subagent_type, "bytes": s.bytes,
                "task_transcript": s.absent,
            })
        })
        .collect();
    if let Some(obj) = json.as_object_mut() {
        obj.insert("subagents".into(), rows.into());
    }
    json
}

/// One line per sealed subagent: its spawn, agent, type and size, or why no
/// transcript was sealed.
fn subagent_lines(v: &View, human: &mut String) {
    for s in &v.subagents {
        let size = match (s.bytes, &s.absent) {
            (Some(n), _) => format!("{n} bytes"),
            (None, Some(why)) => format!("transcript {why}"),
            (None, None) => "no transcript".to_owned(),
        };
        human.push_str(&format!(
            "  subagent {}  agent {}  {}  {size}\n",
            s.tool_use_id,
            s.agent_id,
            s.subagent_type
                .as_ref()
                .map_or_else(|| "?".to_owned(), ToString::to_string),
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn depth_flags_exclude_each_other() {
        assert_eq!(Depth::of(false, false).unwrap(), Depth::Normal);
        assert_eq!(Depth::of(true, false).unwrap(), Depth::Full);
        assert_eq!(Depth::of(false, true).unwrap(), Depth::Short);
        assert!(Depth::of(true, true).is_err());
    }

    #[test]
    fn id_like_is_lowercase_hex_prefix_length() {
        assert!(id_like("abc1234"));
        assert!(id_like(&"a".repeat(64)));
        assert!(!id_like("abc123"), "6 is below the prefix floor");
        assert!(!id_like(&"a".repeat(65)));
        assert!(!id_like("ABC1234"), "uppercase addresses nothing");
        assert!(!id_like("abc123g"));
    }

    /// The renderer never leaks a control byte from shard-derived strings, and
    /// `--short` really is shorter: no prompt block, no attribution lines.
    #[test]
    fn short_omits_the_body_and_hostile_meta_cannot_reach_the_terminal_raw() {
        let hostile = "evil\u{1b}[31m\nrow";
        let v = View {
            id: "ab".repeat(32),
            depth: Depth::Short,
            meta: Some(serde_json::json!({
                "commit": hostile, "strategy": hostile, "canon_version": 3,
            })),
            created_at: Some(5),
            sessions: vec![SessionRow {
                session_id: "s1".into(),
                agent: Label::of(hostile),
                model: Label::of("m"),
            }],
            worktree: Some(Label::of(hostile)),
            digests: vec![("digest.intent.json", Some(serde_json::json!({})))],
            prompt: Some(ShardPrompt::Found(PromptView::test_view(
                "secret prompt text",
            ))),
            attribution: vec![("a.rs".into(), 1, 2, 900)],
            costs: vec![],
            transcript: None,
            subagents: vec![],
        };
        let out = render(&v);
        assert!(
            !out.human.contains('\u{1b}'),
            "ESC reached the terminal: {:?}",
            out.human
        );
        assert!(
            !out.human.contains("secret prompt text"),
            "--short must omit the prompt"
        );
        assert!(!out.human.contains("attribution"), "--short omits the body");

        let normal = render(&View {
            depth: Depth::Normal,
            ..v
        });
        assert!(normal.human.contains("secret prompt text"));
        assert!(normal.human.contains("attribution"));
        assert_eq!(normal.json["transcript_rendered"], false);
    }

    /// The subagents a shard sealed are listed with their sizes, a lost one by
    /// why, and a hostile type cannot reach the terminal raw; a checkpoint with
    /// none renders no member at all.
    #[test]
    fn explain_lists_each_sealed_subagent_with_its_size() {
        let v = View {
            id: "ab".repeat(32),
            depth: Depth::Normal,
            meta: None,
            created_at: None,
            sessions: vec![],
            worktree: None,
            digests: vec![],
            prompt: None,
            attribution: vec![],
            costs: vec![],
            transcript: None,
            subagents: vec![
                SubagentRow {
                    tool_use_id: Label::of("toolu_a"),
                    agent_id: Label::of("a1"),
                    subagent_type: Some(Label::of("Ex\u{1b}[31mplore")),
                    bytes: Some(1234),
                    absent: None,
                },
                SubagentRow {
                    tool_use_id: Label::of("a2"),
                    agent_id: Label::of("a2"),
                    subagent_type: None,
                    bytes: None,
                    absent: Some(Label::of("missing")),
                },
            ],
        };
        let out = render(&v);
        assert!(
            out.human.contains("subagent toolu_a  agent a1"),
            "{}",
            out.human
        );
        assert!(out.human.contains("1234 bytes") && out.human.contains("transcript missing"));
        assert!(!out.human.contains('\u{1b}'));
        assert_eq!(out.json["subagents"][0]["bytes"], 1234);
        let none = render(&View {
            subagents: vec![],
            ..v
        });
        assert!(none.json.get("subagents").is_none());
    }

    /// Worktree provenance at this verb's renderer: the member rides the document as
    /// a top-level fact and the header prints it as `archived_in`, while a
    /// checkpoint the projection has no row for reads `unknown` — never `main`,
    /// and never the shard's own `metadata.worktree` read back as a substitute.
    #[test]
    fn the_explain_header_names_the_member_or_says_it_is_unknown() {
        let base = View {
            id: "ab".repeat(32),
            depth: Depth::Normal,
            meta: Some(serde_json::json!({ "strategy": "manual-commit" })),
            created_at: Some(5),
            sessions: vec![],
            worktree: Some(Label::of("swt-d")),
            digests: vec![],
            prompt: None,
            attribution: vec![],
            costs: vec![],
            transcript: None,
            subagents: vec![],
        };
        let named = render(&base);
        assert!(
            named.human.contains("archived_in: swt-d"),
            "{}",
            named.human
        );
        assert_eq!(named.json["worktree"], "swt-d");

        let unknown = render(&View {
            worktree: None,
            ..base
        });
        assert!(
            unknown.human.contains("archived_in: unknown"),
            "{}",
            unknown.human
        );
        assert!(
            !unknown.human.contains("archived_in: main"),
            "absence is not the main work tree (D6): {}",
            unknown.human
        );
        assert_eq!(unknown.json["worktree"], serde_json::Value::Null);
    }
}
