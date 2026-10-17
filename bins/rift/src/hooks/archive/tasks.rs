//! Collecting and sealing a session's subagent transcripts into its
//! checkpoint, one `tasks/<tool-use-id>/` subtree each.
//!
//! Claude Code keeps every subagent's conversation in its own file,
//! `<dir of the session transcript>/<session id>/subagents/agent-<id>.jsonl`.
//! The subtree is part of the checkpoint shard, never a ledger of its own
//! (Q37): it is sealed through the same redaction gate as the main transcript,
//! bound by the same Merkle root, and re-derived from the files at every
//! archive, so an unchanged session re-archives to the same id.
//!
//! **Where the files come from.** Two sources, unioned by agent id: the
//! listing of the session's `subagents/` directory, and the claim each
//! `SubagentStop` left (`super::super::subagent_claims`), whose stated address
//! wins. The listing alone finds a subagent whose stop hook never fired.
//!
//! **Which spawn a file belongs to.** The tool call that dispatched the
//! subagent is found in the transcripts themselves: a result whose
//! `toolUseResult.agentId` names it, or, for a background launch, a result
//! whose text says `agentId: <id>`. A nested subagent's spawn is in its
//! parent subagent's file, so every collected file is scanned, not only the
//! session's. A file with no spawn found is sealed under its agent id and
//! marked unmatched.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use rift_shadow::backend::{
    ArchivedTask, ArchivedTranscript, ArchivedWorktreeObservation, TaskProvenance,
    TaskTranscriptState, TaskWorktree,
};
use rift_shadow::redact::RulePackSet;

use super::super::subagent_claims::{is_safe_id, read_claims};
use super::originals::Spool;
use super::spills;

/// What one archive collects a session's subagent transcripts from.
pub(super) struct TaskSources<'a> {
    /// The member's git dir, where the session cache and claims live.
    pub(super) git_dir: &'a Path,
    /// The parent session's native id.
    pub(super) session_id: &'a str,
    /// The redactor packs the main transcript is sealed with.
    pub(super) packs: &'a RulePackSet,
    /// The session's worktree observations, subagents' included.
    pub(super) observations: &'a [ArchivedWorktreeObservation],
    /// The session transcript's address, as the session cache remembers it:
    /// the root its subagents' files and every spill are found under.
    pub(super) main: Option<PathBuf>,
    /// The session's spooled originals, sealed beside the subagent
    /// transcripts that record their calls; `None` seals none.
    pub(super) spool: Option<&'a Spool>,
}

/// One subagent file the archive will try to seal.
struct Candidate {
    agent_id: String,
    path: PathBuf,
    /// The type the stop hook named, used when no spawn names one.
    claimed_type: Option<String>,
    /// The file's bytes, or why there are none.
    read: Result<Vec<u8>, TaskTranscriptState>,
}

/// The spawns the transcripts record.
#[derive(Default)]
struct SpawnIndex {
    /// Agent id → the tool-use id whose structured `toolUseResult.agentId`
    /// names it: the client's own field, which outranks any text.
    by_agent: BTreeMap<String, String>,
    /// Agent id → the tool-use id whose result text says `agentId: <id>`.
    /// Tool output is untrusted text, so this is consulted only when no
    /// structured result names the agent, whatever the line order.
    by_text: BTreeMap<String, String>,
    /// Tool-use id → the subagent type that call asked for.
    types: BTreeMap<String, String>,
}

impl<'a> TaskSources<'a> {
    /// The sources of one archive of `session_id`, its observations read off
    /// the session facts the archive already gathered.
    pub(super) fn of(
        git_dir: &'a Path,
        session_id: &'a str,
        packs: &'a RulePackSet,
        facts: &'a rift_shadow::SessionFacts,
    ) -> Self {
        Self {
            git_dir,
            session_id,
            packs,
            observations: &facts.worktree_observations,
            main: main_transcript_path(git_dir, session_id),
            spool: None,
        }
    }
}

impl SpawnIndex {
    /// Index every spawn and spawn result one transcript records. Only lines
    /// that can carry one are parsed.
    fn scan(&mut self, bytes: &[u8]) {
        for line in bytes.split(|b| *b == b'\n') {
            if find(line, b"agentId").is_none() && find(line, b"subagent_type").is_none() {
                continue;
            }
            if let Ok(serde_json::Value::Object(record)) = serde_json::from_slice(line) {
                self.record(&record);
            }
        }
    }

    /// The call that spawned `agent`: a structured match before a text one.
    fn spawn_of(&self, agent: &str) -> Option<&String> {
        self.by_agent.get(agent).or_else(|| self.by_text.get(agent))
    }

    /// One record's spawn calls and spawn results.
    fn record(&mut self, record: &serde_json::Map<String, serde_json::Value>) {
        let structured = record
            .get("toolUseResult")
            .and_then(|r| r.get("agentId"))
            .and_then(serde_json::Value::as_str);
        let blocks = record
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(serde_json::Value::as_array);
        for block in blocks.into_iter().flatten() {
            let text = |key: &str| block.get(key).and_then(serde_json::Value::as_str);
            match text("type") {
                Some("tool_use") => {
                    let kind = block.get("input").and_then(|i| i.get("subagent_type"));
                    if let (Some(id), Some(kind)) = (text("id"), kind.and_then(|k| k.as_str())) {
                        self.types.insert(id.to_owned(), kind.to_owned());
                    }
                }
                Some("tool_result") => {
                    let Some(call) = text("tool_use_id") else {
                        continue;
                    };
                    if let Some(agent) = structured {
                        self.by_agent
                            .entry(agent.to_owned())
                            .or_insert_with(|| call.to_owned());
                    }
                    for agent in agent_ids_in(block.get("content")) {
                        self.by_text.entry(agent).or_insert_with(|| call.to_owned());
                    }
                }
                _ => {}
            }
        }
    }
}

/// Every subagent transcript of the session, sealed as tasks.
pub(super) fn session_tasks(src: &TaskSources<'_>) -> Vec<ArchivedTask> {
    let main = src.main.as_deref();
    let candidates = candidates(src, main);
    if candidates.is_empty() {
        return Vec::new();
    }
    let mut index = SpawnIndex::default();
    if let Some(bytes) = main.and_then(|m| std::fs::read(m).ok()) {
        index.scan(&bytes);
    }
    for c in &candidates {
        if let Ok(bytes) = &c.read {
            index.scan(bytes);
        }
    }
    let mut named = BTreeSet::new();
    candidates
        .into_iter()
        .filter_map(|c| seal_task(src, &index, c, &mut named))
        .collect()
}

/// The session transcript's address, as the session cache remembers it.
fn main_transcript_path(git_dir: &Path, session_id: &str) -> Option<PathBuf> {
    rift_shadow::hooks::read_cache(&crate::family::run_state::sessions_dir(git_dir), session_id)
        .ok()
        .flatten()?
        .transcript_path
        .map(PathBuf::from)
}

/// The listing and the claims, unioned by agent id, each file read once.
fn candidates(src: &TaskSources<'_>, main: Option<&Path>) -> Vec<Candidate> {
    let mut found: BTreeMap<String, (PathBuf, Option<String>)> = BTreeMap::new();
    if let Some(dir) = main.and_then(|m| subagents_dir(m, src.session_id)) {
        for (agent_id, path) in list_subagent_files(&dir) {
            found.insert(agent_id, (path, None));
        }
    }
    for (agent_id, claim) in read_claims(src.git_dir, src.session_id) {
        found.insert(
            agent_id,
            (PathBuf::from(claim.transcript_path), claim.agent_type),
        );
    }
    found
        .into_iter()
        .map(|(agent_id, (path, claimed_type))| Candidate {
            read: read_transcript(&path),
            agent_id,
            path,
            claimed_type,
        })
        .collect()
}

/// `<dir of the session transcript>/<session id>/subagents`.
pub(crate) fn subagents_dir(main: &Path, session_id: &str) -> Option<PathBuf> {
    Some(main.parent()?.join(session_id).join("subagents"))
}

/// The `agent-<id>.jsonl` files under `dir`, by agent id. Any other entry,
/// and an id that could not name a subtree, is left out.
pub(crate) fn list_subagent_files(dir: &Path) -> Vec<(String, PathBuf)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            let id = name.strip_prefix("agent-")?.strip_suffix(".jsonl")?;
            is_safe_id(id).then(|| (id.to_owned(), entry.path()))
        })
        .collect()
}

/// A subagent file's bytes, or the closed word for why there are none.
///
/// Only a regular file is read, and a symlink is never followed: both sources
/// are addresses an agent wrote, and a link would let one seal a file outside
/// the client's own directory into the shard. The file opened is checked to be
/// the one inspected, so a link swapped in between is refused too.
fn read_transcript(path: &Path) -> Result<Vec<u8>, TaskTranscriptState> {
    use std::io::Read as _;
    let inspected = std::fs::symlink_metadata(path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => TaskTranscriptState::Missing,
        _ => TaskTranscriptState::Unreadable,
    })?;
    if !inspected.file_type().is_file() {
        return Err(TaskTranscriptState::Unreadable);
    }
    let mut file = std::fs::File::open(path).map_err(|_| TaskTranscriptState::Unreadable)?;
    let opened = file
        .metadata()
        .map_err(|_| TaskTranscriptState::Unreadable)?;
    if !same_file(&inspected, &opened) {
        return Err(TaskTranscriptState::Unreadable);
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|_| TaskTranscriptState::Unreadable)?;
    Ok(bytes)
}

/// Whether two metadata reads describe one file.
#[cfg(unix)]
pub(super) fn same_file(a: &std::fs::Metadata, b: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    a.dev() == b.dev() && a.ino() == b.ino()
}

/// Off unix there is no inode to compare; the opened handle must at least be
/// a regular file.
#[cfg(not(unix))]
pub(super) fn same_file(_a: &std::fs::Metadata, b: &std::fs::Metadata) -> bool {
    b.file_type().is_file()
}

/// Seal one candidate under its spawn's tool-use id, or under its agent id
/// when no spawn was found or the spawn's id is already taken. `None` only
/// when both names are taken, which two files of one session cannot produce
/// unless they claim one agent id twice.
fn seal_task(
    src: &TaskSources<'_>,
    index: &SpawnIndex,
    c: Candidate,
    named: &mut BTreeSet<String>,
) -> Option<ArchivedTask> {
    let spawn = index
        .spawn_of(&c.agent_id)
        .filter(|t| is_safe_id(t) && !named.contains(*t));
    let name = spawn.unwrap_or(&c.agent_id).clone();
    if !named.insert(name.clone()) {
        tracing::warn!(agent = %c.agent_id, "subagent transcript named twice; not archived");
        return None;
    }
    let subagent_type = spawn
        .and_then(|t| index.types.get(t))
        .cloned()
        .or(c.claimed_type);
    let (transcript, state, (started, ended)) = match c.read {
        Ok(bytes) => {
            let instants = time_span(&bytes);
            match seal_through_gate(src, bytes, &c.path) {
                Some(t) => (Some(t), TaskTranscriptState::Sealed, instants),
                None => (None, TaskTranscriptState::Unsealed, instants),
            }
        }
        Err(state) => (None, state, (0, 0)),
    };
    Some(ArchivedTask {
        tool_use_id: name,
        provenance: Some(TaskProvenance {
            subagent_type,
            parent_session: src.session_id.to_owned(),
            worktree: worktree_of(src.observations, &c.agent_id),
            spawn_matched: spawn.is_some(),
            transcript: state,
        }),
        agent_id: c.agent_id,
        started_at_ms: started,
        ended_at_ms: ended,
        input_tokens: None,
        output_tokens: None,
        transcript,
    })
}

/// The file through the same gate as the main transcript, whole: a task's
/// pair is re-derived at every archive, so its compaction starts at line 0.
/// A refusal would be said and recorded as `unsealed`, never dropped in
/// silence; the gate refuses no input since a line it cannot settle is sealed
/// flat or replaced by a placeholder record, so the word is unreachable here.
/// The spills it names are sealed beside it; they live in the parent
/// session's directory, so they are contained by the parent's address. So
/// are the spooled originals of the calls it records.
fn seal_through_gate(
    src: &TaskSources<'_>,
    bytes: Vec<u8>,
    path: &Path,
) -> Option<ArchivedTranscript> {
    let raw = rift_core::SecretBytes::new(bytes);
    match rift_shadow::archived_transcript(&raw, 0, src.packs) {
        Ok((mut pair, _)) => {
            if let Some(main) = &src.main {
                pair.spills =
                    spills::of_transcript(main, src.session_id, src.packs, raw.as_slice());
            }
            if let Some(spool) = src.spool {
                pair.originals = spool.claimed_by(raw.as_slice(), pair.full.as_slice());
            }
            Some(pair)
        }
        Err(e) => {
            tracing::warn!(error = %e, path = %path.display(), "subagent transcript not sealed");
            None
        }
    }
}

/// The earliest and latest `timestamp` the file's records carry, epoch ms;
/// `(0, 0)` when none parses.
fn time_span(bytes: &[u8]) -> (i64, i64) {
    const KEY: &[u8] = br#""timestamp":""#;
    let mut span: Option<(i64, i64)> = None;
    let mut rest = bytes;
    while let Some(at) = find(rest, KEY) {
        rest = rest.get(at + KEY.len()..).unwrap_or_default();
        let end = rest.iter().position(|b| *b == b'"').unwrap_or(rest.len());
        let ms = std::str::from_utf8(rest.get(..end).unwrap_or_default())
            .ok()
            .and_then(rift_recall_substrate::parse_rfc3339_ms);
        if let Some(ms) = ms {
            span = Some(span.map_or((ms, ms), |(lo, hi)| (lo.min(ms), hi.max(ms))));
        }
    }
    span.unwrap_or((0, 0))
}

/// The first offset of `needle` in `haystack`.
fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Where the subagent ran: the earliest observation its own hook recorded.
fn worktree_of(
    observations: &[ArchivedWorktreeObservation],
    agent_id: &str,
) -> Option<TaskWorktree> {
    observations
        .iter()
        .filter(|o| o.subagent.as_deref() == Some(agent_id))
        .min_by(|a, b| (a.at, &a.token).cmp(&(b.at, &b.token)))
        .map(|o| TaskWorktree {
            token: o.token.clone(),
            name: o.name.clone(),
            linked: o.linked,
        })
}

/// The agent ids a result's text names as `agentId: <id>`.
fn agent_ids_in(content: Option<&serde_json::Value>) -> Vec<String> {
    const MARK: &str = "agentId: ";
    let texts: Vec<&str> = match content {
        Some(serde_json::Value::String(s)) => vec![s.as_str()],
        Some(serde_json::Value::Array(blocks)) => blocks
            .iter()
            .filter_map(|b| b.get("text").and_then(serde_json::Value::as_str))
            .collect(),
        _ => Vec::new(),
    };
    let mut ids = Vec::new();
    for text in texts {
        for (at, _) in text.match_indices(MARK) {
            let tail = text.get(at + MARK.len()..).unwrap_or_default();
            let end = tail
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'))
                .unwrap_or(tail.len());
            let id = tail.get(..end).unwrap_or_default();
            if is_safe_id(id) {
                ids.push(id.to_owned());
            }
        }
    }
    ids
}

#[cfg(test)]
mod tests;
