//! The out-of-tree transient session cache.
//!
//! A small JSON file at `<git_dir>/rift-sessions/<session_id>.json` holds the
//! session's **current** phase so Canopy and the strategy layer can read it
//! without a DB round-trip. It is a *cache*: the append-only `session_transition`
//! rows are the source of truth (a crash leaves a stale file, reconciled on the
//! next event). It lives under the git dir — outside the working tree, so it never
//! shows up as a dirty file — and carries **no raw content**: only ids, the phase,
//! ordinals, and the native→Rift id map.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use rift_core::{Result, RiftError};
use serde::{Deserialize, Serialize};

use crate::session_id::is_safe_session_id;

/// Directory (under the git dir) holding per-session cache files.
const SESSIONS_DIR: &str = "rift-sessions";
/// Cache file schema version.
pub const SCHEMA: u32 = 1;

/// The transient per-session state. Structural only — no transcript/tool
/// BYTES: ids, the phase, ordinals, the id map, and a *path* to where the
/// agent keeps its own transcript (a filename is an address, not content).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionCache {
    /// Contract schema version.
    pub schema: u32,
    /// The session id.
    pub session_id: String,
    /// The current phase (`SessionState::as_str`).
    pub state: String,
    /// The agent driving the session.
    pub agent: String,
    /// The current turn ordinal.
    pub current_turn_ordinal: u32,
    /// Whether the current turn is still open (a prompt/turn-start has been
    /// seen and no turn-end has closed it). Drives native turn-ordinal
    /// derivation; absent in cache files written before the field existed
    /// (reads as closed).
    #[serde(default)]
    pub turn_open: bool,
    /// The compaction boundary ordinal, if mid-compaction.
    pub compaction_boundary: Option<u32>,
    /// Transaction-time of the last update (epoch ms).
    pub updated_at_ms: i64,
    /// Native id → Rift id reconciliation map (ids only).
    #[serde(default)]
    pub native_id_map: BTreeMap<String, String>,
    /// Where the agent keeps this session's transcript JSONL, as last reported
    /// on a hook payload (`transcript_path`) — the POST-COMMIT archive has no
    /// hook payload, and this is how it finds the transcript to condense
    /// A PATH only, never bytes; last
    /// report wins because agents relocate transcripts across resumes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript_path: Option<String>,
    /// The canonical working tree this session was observed in. Absent in
    /// every cache file written before worktrees became family members.
    ///
    /// **This is a DISCRIMINATOR, not a convenience.** Session files live under
    /// the git dir the agent's hook resolved, which for a linked worktree is
    /// that worktree's admin dir and for the main one is the shared `.git` —
    /// so a reader that consults the shared registry to find sessions its own
    /// admin dir does not hold must be able to tell whose they are. Without it
    /// a session driven in worktree A could be credited with a commit made in
    /// worktree B, and the archive would attribute one developer's work to
    /// another's session.
    ///
    /// `None` means UNKNOWN, and unknown never matches a linked worktree —
    /// see [`live_sessions_for`]. It is filled by [`write_cache`] from the git
    /// dir it is already given, so no caller can forget it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_tree: Option<String>,
    /// The LAST worktree state this session was observed in — the cheap
    /// comparison that keeps the observation writer off the hot path.
    ///
    /// **A cache of a durable fact, never the fact.** The durable record is the
    /// append-only `session_worktree` relation, whose rows are addressed by the
    /// observed state itself; this field exists so a hook that observes the
    /// SAME state as the last one skips the insert instead of paying a database
    /// round trip on every event. Losing it costs one redundant `INSERT OR
    /// IGNORE` — which is a no-op — and never a row, so a deleted cache, a
    /// crash mid-write and a cache file written before worktree provenance
    /// existed are all simply "unknown, look again".
    ///
    /// Written by the agent-hook path only, after the ingest has already
    /// rewritten this file, so the two writes are ordered by the one hook that
    /// runs them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_worktree: Option<WorktreeObservation>,
}

/// One observed worktree state, as the hook path last saw it.
///
/// Carries the whole observation rather than a bare `(token, branch, head)`
/// triple: `subagent_id` is part of the observation's identity — a subagent's
/// hook in the same checkout is a DIFFERENT row under the same session — so a
/// comparison that left it out would skip the very rows worktree-isolated
/// subagents exist to produce. `name`/`linked` ride along because the writer
/// needs them anyway and a partial cache invites a second read.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeObservation {
    /// `RepoPaths::worktree_token()` — 12 hex.
    pub token: String,
    /// The linked worktree's administrative name, or `main`.
    pub name: String,
    /// Whether the observed checkout is a linked worktree.
    pub linked: bool,
    /// `HEAD`'s symbolic name; `None` when detached or unreadable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// `HEAD`'s oid; `None` when unborn or unresolvable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head: Option<String>,
    /// The subagent session id whose hook observed this, when one did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagent: Option<String>,
}

/// Resolve a worktree root from a hook's `--git-dir`, handling linked worktrees.
///
/// For a linked worktree git sets `GIT_DIR=<repo>/.git/worktrees/<name>`, which
/// holds a `gitdir` file pointing at the worktree's `.git` file — its parent is
/// the worktree root. (Naively taking `git_dir.parent()` would walk into
/// `.git/worktrees` and `discover` up to the *main* worktree, capturing the
/// wrong tree.) For the main worktree there is no `gitdir` file and the parent
/// is correct.
///
/// **This is the ONE implementation.** `bins/rift/src/hooks.rs` carried a byte-
/// identical copy until the worktree discriminator needed the same resolution:
/// two copies of a path rule with nothing reddening if they drifted, and they
/// had already drifted once (this one canonicalised, that one did not) before
/// the duplication was removed. Its four hook call sites and its test now come
/// here.
///
/// A linked worktree's admin dir (`<common>/.git/worktrees/<name>`) carries a
/// `gitdir` file naming that worktree's `.git` FILE, whose parent is the tree;
/// a main `.git` has the tree as its own parent. Total either way, and `None`
/// only when neither shape resolves — a bare relative `--git-dir .git` has an
/// empty parent, which means "the cwd", and this cannot know the cwd.
#[must_use]
pub fn work_tree_of(git_dir: &Path) -> Option<PathBuf> {
    let raw = if let Ok(s) = std::fs::read_to_string(git_dir.join("gitdir")) {
        Path::new(s.trim()).parent().map(Path::to_path_buf)
    } else {
        match git_dir.parent() {
            Some(p) if !p.as_os_str().is_empty() => Some(p.to_path_buf()),
            _ => None,
        }
    }?;
    // Returned RAW, not canonicalised. The equality this feeds does need
    // canonical paths — the two sides arrive by different routes (a hook's
    // `--git-dir`, a discovery walk, a symlinked checkout) and `/tmp/x` and
    // `/private/tmp/x` are the same tree — but canonicalising HERE would change
    // what four `bins/rift` hook call sites resolve a work tree to, which is a
    // path-semantics change and not a timing one. So the normalisation lives at
    // the comparison in [`live_sessions_for`], and this function stays byte-for-
    // byte the resolution the hooks have always done.
    Some(raw)
}

impl SessionCache {
    /// A fresh cache for a session entering `state`.
    #[must_use]
    pub fn new(
        session_id: impl Into<String>,
        agent: impl Into<String>,
        state: &str,
        now_ms: i64,
    ) -> Self {
        Self {
            schema: SCHEMA,
            session_id: session_id.into(),
            state: state.to_owned(),
            agent: agent.into(),
            current_turn_ordinal: 0,
            turn_open: false,
            compaction_boundary: None,
            updated_at_ms: now_ms,
            native_id_map: BTreeMap::new(),
            transcript_path: None,
            // Filled by `write_cache` from the git dir the write is addressed
            // to, so a constructor cannot get it wrong by omission.
            work_tree: None,
            // Unknown until the first observation, and unknown is the honest
            // answer for a session that has not been seen anywhere yet.
            last_worktree: None,
        }
    }
}

/// The archive's transcript progress for one session: how many non-empty RAW
/// transcript lines the last archived checkpoint covered — exactly the value
/// the next checkpoint passes as its compaction `start_line`
/// (`CompactTranscript::raw_lines`, whose doc explains why this count must
/// come from the compactor's own iterator and never be re-counted).
///
/// **Worker-owned, deliberately not a `SessionCache` field.** The cache is
/// written by the hook path on every event (last-write-wins, whole file); the
/// archive worker runs detached and later. One more writer of the same file
/// would race the hooks and quietly drop whichever side lost, so the worker
/// records its progress in its own sidecar (`<session>.transcript`, which
/// [`live_sessions`] never even enumerates — it strips `.json` only).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscriptProgress {
    /// Contract schema version.
    pub schema: u32,
    /// The session id.
    pub session_id: String,
    /// Non-empty raw transcript lines covered by the last archive.
    pub raw_lines: u64,
    /// Transaction-time of the last update (epoch ms).
    pub updated_at_ms: i64,
}

/// A session's sidecar path under [`sessions_dir`]: `<session_id>.<suffix>`.
///
/// Every sidecar shares the one unsafe-id guard: a session id is agent-written
/// text, and this is the only place it becomes part of a filename beside
/// [`cache_path`], so the two cannot disagree about what stays inside the
/// directory.
///
/// # Errors
/// [`RiftError::Validity`] if `session_id` is not usable as a path component.
fn sidecar_path(git_dir: &Path, session_id: &str, suffix: &str) -> Result<PathBuf> {
    if !is_safe_session_id(session_id) {
        return Err(RiftError::validity(format!(
            "{suffix} sidecar: unsafe session id"
        )));
    }
    Ok(sessions_dir(git_dir).join(format!("{session_id}.{suffix}")))
}

/// Atomically write a sidecar (temp file + rename).
fn write_sidecar(
    git_dir: &Path,
    path: &Path,
    session_id: &str,
    suffix: &str,
    json: &[u8],
) -> Result<()> {
    let dir = sessions_dir(git_dir);
    std::fs::create_dir_all(&dir)?;
    let tmp = dir.join(format!(".{session_id}.{suffix}.tmp"));
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// The transcript-progress sidecar path for `session_id`.
///
/// # Errors
/// [`RiftError::Validity`] if `session_id` is not usable as a path component.
fn transcript_progress_path(git_dir: &Path, session_id: &str) -> Result<PathBuf> {
    sidecar_path(git_dir, session_id, "transcript")
}

/// The **hook-owned** usage-progress sidecar for one session
/// (`<session>.usage`): how far into the agent's transcript the closed
/// turns' spend has been settled (the deferred-settle contract).
///
/// A `Stop` hook records nothing about spend — the agent is still flushing
/// the transcript when it fires — and the *next* hook for the session (or the
/// doctor sweep) settles the turn that has closed by then, reading forward
/// from `byte_offset`. Two consequences the reader should hold:
///
/// * **Hook-owned, not a [`SessionCache`] field and not
///   [`TranscriptProgress`].** The cache is rewritten whole on every hook and
///   the transcript sidecar belongs to the detached archive worker, whose
///   documented two-writer race this file must not join. The hook path is the
///   only writer here, so successive settles are ordered by the hooks that ran
///   them.
/// * **A late start never back-fills.** The first settle for a session seeds
///   `byte_offset` at the transcript's *current* length, so a session enabled
///   or attached mid-life records the turns after that point and never
///   re-attributes earlier spend. `settled_turn` is the last turn ordinal whose
///   spend landed; a settle for a turn at or below it is a no-op, which is what
///   makes redelivery, a double settle and the sweep idempotent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageProgress {
    /// Contract schema version.
    pub schema: u32,
    /// The session id.
    pub session_id: String,
    /// The transcript byte offset the next window starts at.
    pub byte_offset: u64,
    /// The last turn ordinal whose spend was settled (0 = none yet).
    #[serde(default)]
    pub settled_turn: u32,
    /// Transaction-time of the last update (epoch ms).
    pub updated_at_ms: i64,
}

/// The usage-progress sidecar path for `session_id`.
///
/// # Errors
/// [`RiftError::Validity`] if `session_id` is not usable as a path component.
fn usage_progress_path(git_dir: &Path, session_id: &str) -> Result<PathBuf> {
    sidecar_path(git_dir, session_id, "usage")
}

/// Atomically record the hook path's usage progress.
///
/// # Errors
/// [`RiftError::Io`] on a filesystem failure, [`RiftError::Validity`] on an
/// unserializable value or unsafe session id.
pub fn write_usage_progress(git_dir: &Path, p: &UsageProgress) -> Result<()> {
    let path = usage_progress_path(git_dir, &p.session_id)?;
    let json = serde_json::to_vec_pretty(p)
        .map_err(|e| RiftError::validity(format!("usage progress serialize: {e}")))?;
    write_sidecar(git_dir, &path, &p.session_id, "usage", &json)
}

/// Read the usage progress for `session_id`, `None` before the first settle.
///
/// # Errors
/// [`RiftError::Io`] on a filesystem failure, [`RiftError::Validity`] on a
/// malformed file or unsafe session id.
pub fn read_usage_progress(git_dir: &Path, session_id: &str) -> Result<Option<UsageProgress>> {
    match std::fs::read(usage_progress_path(git_dir, session_id)?) {
        Ok(bytes) => {
            let p = serde_json::from_slice(&bytes)
                .map_err(|e| RiftError::validity(format!("usage progress parse: {e}")))?;
            Ok(Some(p))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Atomically record the archive worker's transcript progress.
///
/// # Errors
/// [`RiftError::Io`] on a filesystem failure, [`RiftError::Validity`] on an
/// unserializable value or unsafe session id.
pub fn write_transcript_progress(git_dir: &Path, p: &TranscriptProgress) -> Result<()> {
    let path = transcript_progress_path(git_dir, &p.session_id)?;
    let json = serde_json::to_vec_pretty(p)
        .map_err(|e| RiftError::validity(format!("transcript progress serialize: {e}")))?;
    write_sidecar(git_dir, &path, &p.session_id, "transcript", &json)
}

/// Read the worker's transcript progress for `session_id`, `None` before the
/// first transcript-carrying archive.
///
/// # Errors
/// [`RiftError::Io`] on a filesystem failure, [`RiftError::Validity`] on a
/// malformed file or unsafe session id.
pub fn read_transcript_progress(
    git_dir: &Path,
    session_id: &str,
) -> Result<Option<TranscriptProgress>> {
    match std::fs::read(transcript_progress_path(git_dir, session_id)?) {
        Ok(bytes) => {
            let p = serde_json::from_slice(&bytes)
                .map_err(|e| RiftError::validity(format!("transcript progress parse: {e}")))?;
            Ok(Some(p))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// How long a session whose cache still says *live* may go untouched before a
/// sweep is allowed to finalize it (24 h).
///
/// A running session touches this file on every prompt, tool call and turn end,
/// so a full day of silence means the agent is gone — the process was killed,
/// the machine rebooted, the terminal was closed — and the cache is the only
/// thing still claiming otherwise. The window is deliberately generous because
/// the two errors are not symmetric: finalizing a crashed session late costs
/// nothing but a stale row in `rift doctor`, while finalizing a session the
/// developer is about to resume moves it to `ending` in the middle of the work
/// — and `Ending` has no edge back, so the phase reads `ending` for the rest of
/// that session's life. Nothing is lost when that happens: the resumed agent's
/// hooks are captured in full and re-stamp the last-touch time, so the session
/// is no longer idle, and the first sweep after the developer stops condenses it
/// (the sweep retries a `RequestCondense` left outstanding — see
/// `sweep_stale_sessions`). The cost is a phase that under-describes a live
/// session, which is why the window is a day and not an hour. Narrowing it is a
/// policy decision, not a sweep detail.
///
/// The legacy backend has no equivalent: its `IDLE` phase is the gap *between turns*, not a
/// timeout, so this threshold is Rift's own and is stated rather than ported.
pub const IDLE_TIMEOUT_MS: i64 = 24 * 60 * 60 * 1000;

/// Whether `cache` has been untouched for longer than `timeout_ms` as of
/// `now_ms` — the sweep's whole decision, kept pure and clock-free.
///
/// A cache stamped in the future (a clock step, or a file copied from another
/// machine) reads as age zero rather than as a huge negative, so a backwards
/// clock can never finalize a live session.
#[must_use]
pub fn is_idle_past(cache: &SessionCache, now_ms: i64, timeout_ms: i64) -> bool {
    now_ms.saturating_sub(cache.updated_at_ms) > timeout_ms
}

/// The directory holding session cache files for `git_dir`.
#[must_use]
pub fn sessions_dir(git_dir: &Path) -> PathBuf {
    git_dir.join(SESSIONS_DIR)
}

/// The cache file path for `session_id`.
///
/// The id comes from a hook payload the agent wrote, so it is checked here as
/// well as at the wire ([`is_safe_session_id`]): this function is the only
/// place a session id becomes a filename, which makes it the one place that can
/// guarantee the path stays inside [`sessions_dir`].
///
/// # Errors
/// [`RiftError::Validity`] if `session_id` is not usable as a path component
/// (empty, over-long, or carrying path punctuation such as `../`).
pub fn cache_path(git_dir: &Path, session_id: &str) -> Result<PathBuf> {
    if !is_safe_session_id(session_id) {
        return Err(RiftError::validity("session cache: unsafe session id"));
    }
    Ok(sessions_dir(git_dir).join(format!("{session_id}.json")))
}

/// Atomically write the cache (temp file + rename) so a reader never sees a torn
/// write.
///
/// # Errors
/// [`RiftError::Io`] on a filesystem failure, [`RiftError::Validity`] if the
/// value cannot be serialized or its session id is not filename-safe.
pub fn write_cache(git_dir: &Path, c: &SessionCache) -> Result<()> {
    // The discriminator is derived HERE, from the git dir this write is already
    // addressed to, rather than being passed in by each caller. Three
    // production writers reach this function and a fourth is the attach verb;
    // asking each to remember a field is how the last two defects in this area
    // were written. A caller that already knows better may set it, and this
    // never overwrites what it finds.
    let c = &{
        let mut owned = c.clone();
        if owned.work_tree.is_none() {
            owned.work_tree = work_tree_of(git_dir).map(|p| p.to_string_lossy().into_owned());
        }
        owned
    };
    let path = cache_path(git_dir, &c.session_id)?;
    let dir = sessions_dir(git_dir);
    std::fs::create_dir_all(&dir)?;
    let json = serde_json::to_vec_pretty(c)
        .map_err(|e| RiftError::validity(format!("session cache serialize: {e}")))?;
    let tmp = dir.join(format!(".{}.tmp", c.session_id));
    std::fs::write(&tmp, &json)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Read the cache for `session_id`, or `None` if there is no file yet.
///
/// # Errors
/// [`RiftError::Io`] on a filesystem failure, [`RiftError::Validity`] if the file
/// is not valid cache JSON or the session id is not filename-safe. An unsafe id
/// is an error rather than "no session": the caller must defer the hook, not
/// carry on and open a second session under a name it could not address.
pub fn read_cache(git_dir: &Path, session_id: &str) -> Result<Option<SessionCache>> {
    match std::fs::read(cache_path(git_dir, session_id)?) {
        Ok(bytes) => {
            let c = serde_json::from_slice(&bytes)
                .map_err(|e| RiftError::validity(format!("session cache parse: {e}")))?;
            Ok(Some(c))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// The live sessions THIS working tree may act on, consulting both its own
/// registry and the repository's shared one.
///
/// **The defect, measured 2026-08-18 — and READ THE NEXT PARAGRAPH, because
/// this function does not close it.** A session file lives under the git dir
/// the agent's hook resolved. An agent whose process runs in one checkout
/// registers its session there, while commits made in a *different* checkout of
/// the same repository fire their hooks against that checkout's admin dir —
/// which holds no sessions at all. Neither side ever has both halves of a
/// condensation, so the post-commit hook takes its no-session branch, persists
/// the diff facts and archives nothing. On this repository's five linked
/// worktrees that was **0 sessions, 0 checkpoints, 0 verification_runs** against
/// 63 file revisions and 471 changed hunks.
///
/// **WHAT THIS ACTUALLY DOES: it makes the misattribution IMPOSSIBLE rather
/// than silent. It does NOT restore capture, and the shared-registry branch
/// below is unreachable from any file a production writer emits.** Planted both
/// ways on the shipped binary: a real `SessionStart` + `UserPromptSubmit` with
/// cwd in the main checkout stamps the cache `work_tree = <main>`, because
/// [`work_tree_of`] on a main `.git` returns its parent — and the shared-registry
/// branch here fires only for a LINKED worktree, whose path is never the main
/// one. So that session is correctly refused (`archived=0` committing in a
/// linked worktree; `archived=1` in the main checkout, the control), and the
/// branch only ever admits a hand-written file claiming a linked worktree,
/// which nothing in production writes. **The gap is structural, not a property
/// of how this repository is developed: it applies to every user with git
/// worktrees, which is the population family-scoped sessions exist for.** It
/// is carried as a known, planted and open gap, not as a closed item.
///
/// **The discriminator is what the change is FOR.** Reading the shared registry
/// without one would let a session driven in worktree A be credited with a
/// commit made in worktree B — one developer's work attributed to another's
/// session, which is worse than capturing nothing. So:
///
/// * this worktree's OWN registry is read as before, unfiltered — those
///   sessions are already scoped to it by their location;
/// * the SHARED registry is read only when this is a linked worktree, and an
///   entry from it is kept only when its `work_tree` canonicalises equal to
///   this one;
/// * **`work_tree: None` never matches.** Every cache file written before
///   the discriminator existed has none, and absence is not a claim — an old
///   session can still be used where it always was (its own registry) and can
///   never be adopted by a checkout that merely shares a repository with it.
///
/// # Errors
/// [`rift_core::RiftError`] if a registry directory cannot be read.
pub fn live_sessions_for(
    git_dir: &Path,
    common_dir: &Path,
    work_tree: &Path,
) -> Result<Vec<SessionCache>> {
    let mut out = live_sessions(git_dir)?;
    // The main working tree's own registry IS the shared one; reading it twice
    // would double every session it holds.
    if git_dir == common_dir {
        return Ok(out);
    }
    let mine = std::fs::canonicalize(work_tree).unwrap_or_else(|_| work_tree.to_path_buf());
    let known: Vec<String> = out.iter().map(|c| c.session_id.clone()).collect();
    let before = out.len();
    for cache in live_sessions(common_dir)? {
        // Canonicalise BOTH sides at the comparison, not at the stamp: a claim
        // written before a checkout was moved, or through a symlinked path,
        // still has to match the tree it names.
        let claims_mine = cache.work_tree.as_ref().is_some_and(|w| {
            let claimed = Path::new(w);
            std::fs::canonicalize(claimed).unwrap_or_else(|_| claimed.to_path_buf())
                == mine.as_path()
        });
        if claims_mine && !known.contains(&cache.session_id) {
            out.push(cache);
        }
    }
    // Appending breaks [`live_sessions`]'s documented "most-recently-updated
    // first" order, and three of the four production readers take `.next()` off
    // this list as THE session to archive against — so an out-of-order tail
    // would silently change which session a commit is attributed to. Latent
    // today only because the branch above is unreachable from production files;
    // fixing it now rather than leaving a trap for whoever makes it reachable.
    if out.len() != before {
        out.sort_by(|a, b| {
            b.updated_at_ms
                .cmp(&a.updated_at_ms)
                .then_with(|| a.session_id.cmp(&b.session_id))
        });
    }
    Ok(out)
}

/// Read every session cache under ONE `git_dir` whose phase is **live**
/// (active, compacting, or ending). A missing/empty sessions dir yields an
/// empty list; an unreadable individual file is skipped (never fatal — capture
/// must not abort a git op).
///
/// This reads a single registry and applies no ownership rule. Callers deciding
/// which sessions a WORKING TREE may act on want [`live_sessions_for`], which
/// consults the shared registry too and enforces the worktree discriminator.
///
/// # Errors
/// [`RiftError::Io`] only if the sessions directory exists but cannot be listed.
pub fn live_sessions(git_dir: &Path) -> Result<Vec<SessionCache>> {
    const LIVE: &[&str] = &["active", "compacting", "ending"];
    let dir = sessions_dir(git_dir);
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(stem) = name
            .to_string_lossy()
            .strip_suffix(".json")
            .map(str::to_owned)
        else {
            continue; // skip temp files / non-cache entries
        };
        if let Ok(Some(cache)) = read_cache(git_dir, &stem) {
            if LIVE.contains(&cache.state.as_str()) {
                out.push(cache);
            }
        }
    }
    // Deterministic order (most-recently-updated first), so a single archive
    // target is stable across runs.
    out.sort_by(|a, b| {
        b.updated_at_ms
            .cmp(&a.updated_at_ms)
            .then(a.session_id.cmp(&b.session_id))
    });
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let mut c = SessionCache::new("01JSESSION", "claude-code", "active", 42);
        c.current_turn_ordinal = 7;
        c.native_id_map.insert("native-1".into(), "01JRIFT".into());
        write_cache(tmp.path(), &c).unwrap();
        let read = read_cache(tmp.path(), "01JSESSION").unwrap().unwrap();

        // The discriminator is derived by `write_cache` from the git dir it is
        // addressed to, so what comes back is the record PLUS the
        // worktree claim — that difference is the feature, and asserting it here
        // is what pins the derivation happening at all.
        assert_eq!(
            read.work_tree,
            work_tree_of(tmp.path()).map(|p| p.to_string_lossy().into_owned()),
            "write_cache stamps the working tree the session was observed in"
        );
        assert!(read.work_tree.is_some(), "and it resolved to something");
        assert_eq!(
            SessionCache {
                work_tree: None,
                ..read
            },
            c,
            "every other field round-trips byte-identically"
        );
    }

    /// `write_cache` never OVERWRITES a claim it finds, so the derivation is
    /// structural against omission and conventional against commission: a
    /// caller that sets the field wins. That arm had no test, and the field is
    /// `pub`, so this pins the one property that keeps a wrong claim from being
    /// laundered into a right-looking one — what is written is what was set.
    #[test]
    fn write_cache_keeps_a_claim_the_caller_already_made() {
        let tmp = tempfile::tempdir().unwrap();
        let mut c = SessionCache::new("01JCLAIM", "claude-code", "active", 1);
        c.work_tree = Some("/somewhere/else".to_owned());
        write_cache(tmp.path(), &c).unwrap();
        let read = read_cache(tmp.path(), "01JCLAIM").unwrap().unwrap();
        assert_eq!(
            read.work_tree.as_deref(),
            Some("/somewhere/else"),
            "an explicit claim survives; the derivation only FILLS, never corrects"
        );
    }

    /// **Absence is not a claim**: a cache file written before the
    /// discriminator existed deserializes with `work_tree: None`, and
    /// [`live_sessions_for`] must never let a linked worktree adopt it. Pinned
    /// here at the type level and end-to-end in
    /// `hooks_worktree_e2e::a_worktree_adopts_only_the_shared_session_that_names_it`.
    #[test]
    fn a_pre_ruling_cache_file_deserializes_with_no_claim() {
        let pre_ruling = br#"{"schema":1,"session_id":"old","state":"active",
            "agent":"claude-code","current_turn_ordinal":1,"turn_open":false,
            "compaction_boundary":null,"updated_at_ms":9,"native_id_map":{}}"#;
        let c: SessionCache = serde_json::from_slice(pre_ruling).unwrap();
        assert_eq!(
            c.work_tree, None,
            "no field means no claim, never a default"
        );
    }

    #[test]
    fn missing_cache_is_none() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(read_cache(tmp.path(), "nope").unwrap().is_none());
    }

    /// The sink guard: even handed an id from somewhere other than the
    /// wire, the cache cannot address a path outside its own directory.
    #[test]
    fn an_unsafe_session_id_never_becomes_a_path() {
        let tmp = tempfile::tempdir().unwrap();
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        let git_dir = outside.join("git");
        let escape = "../../evil";

        assert!(cache_path(&git_dir, escape).is_err());
        assert!(read_cache(&git_dir, escape).is_err());
        let hostile = SessionCache::new(escape, "claude-code", "active", 1);
        assert!(write_cache(&git_dir, &hostile).is_err());
        assert!(
            !outside.join("evil.json").exists() && !tmp.path().join("evil.json").exists(),
            "nothing may be written outside the sessions directory"
        );

        // A safe id stays one component inside the sessions directory.
        let path = cache_path(&git_dir, "01JSESSION").unwrap();
        assert_eq!(path.parent(), Some(sessions_dir(&git_dir).as_path()));
    }

    #[test]
    fn overwrite_is_atomic_and_latest_wins() {
        let tmp = tempfile::tempdir().unwrap();
        let c1 = SessionCache::new("s", "claude-code", "active", 1);
        write_cache(tmp.path(), &c1).unwrap();
        let c2 = SessionCache::new("s", "claude-code", "ending", 2);
        write_cache(tmp.path(), &c2).unwrap();
        assert_eq!(
            read_cache(tmp.path(), "s").unwrap().unwrap().state,
            "ending"
        );
        // no leftover temp file
        let leftovers: Vec<_> = std::fs::read_dir(sessions_dir(tmp.path()))
            .unwrap()
            .filter_map(std::result::Result::ok)
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "temp file must be renamed away");
    }

    #[test]
    fn live_sessions_filters_by_phase_and_orders_recent_first() {
        let tmp = tempfile::tempdir().unwrap();
        let gd = tmp.path();
        assert!(live_sessions(gd).unwrap().is_empty(), "no dir → empty");
        write_cache(gd, &SessionCache::new("s-idle", "claude-code", "idle", 1)).unwrap();
        write_cache(gd, &SessionCache::new("s-ended", "claude-code", "ended", 2)).unwrap();
        write_cache(
            gd,
            &SessionCache::new("s-active", "claude-code", "active", 5),
        )
        .unwrap();
        write_cache(
            gd,
            &SessionCache::new("s-ending", "claude-code", "ending", 9),
        )
        .unwrap();
        let live = live_sessions(gd).unwrap();
        let ids: Vec<&str> = live.iter().map(|c| c.session_id.as_str()).collect();
        // only active+ending, most-recent first
        assert_eq!(ids, vec!["s-ending", "s-active"]);
    }

    #[test]
    fn idleness_is_measured_from_the_last_touch_and_never_from_a_backwards_clock() {
        let fresh = SessionCache::new("s", "claude-code", "active", 1_000);
        // Exactly at the threshold is not yet past it.
        assert!(!is_idle_past(
            &fresh,
            1_000 + IDLE_TIMEOUT_MS,
            IDLE_TIMEOUT_MS
        ));
        assert!(is_idle_past(
            &fresh,
            1_000 + IDLE_TIMEOUT_MS + 1,
            IDLE_TIMEOUT_MS
        ));
        // A cache stamped in the future (clock step, or copied from another
        // machine) reads as age zero — never as a huge age that would finalize
        // a session that is very much alive.
        let future = SessionCache::new("s", "claude-code", "active", i64::MAX);
        assert!(!is_idle_past(&future, 0, IDLE_TIMEOUT_MS));
        assert!(!is_idle_past(&future, 0, 0));
    }

    #[test]
    fn cache_carries_no_raw_content_fields() {
        // Guard: the struct is structural-only. If someone adds a `payload`/`text`
        // field, this test (and review) should force a rethink.
        //
        // `transcript` left this list DELIBERATELY (2c): the cache now carries
        // `transcript_path` — an ADDRESS, which is structural the same way the
        // session id is — and a substring guard cannot tell a path field from
        // a bytes field. What it can still catch is a field whose NAME says it
        // holds content, so those stay; the path field is asserted below to
        // carry exactly the path it was given and nothing longer.
        let mut c = SessionCache::new("s", "claude-code", "active", 0);
        let json = serde_json::to_string(&c).unwrap();
        for forbidden in ["payload", "prompt", "content", "text"] {
            assert!(!json.contains(forbidden), "cache must stay structural-only");
        }
        c.transcript_path = Some("/tmp/agent/sess.jsonl".into());
        let json = serde_json::to_string(&c).unwrap();
        assert!(
            json.contains("\"transcript_path\":\"/tmp/agent/sess.jsonl\""),
            "the path rides as a plain string address: {json}"
        );
    }

    #[test]
    fn a_pre_2c_cache_file_still_parses_and_a_none_path_serializes_invisibly() {
        // Forward-compat both ways: an old file (no transcript_path) reads as
        // None, and a None path writes no field — so pre-2c cache bytes and
        // post-2c cache bytes are identical until a path is actually seen.
        let old = r#"{"schema":1,"session_id":"s","state":"active","agent":"claude-code",
                      "current_turn_ordinal":0,"turn_open":false,"compaction_boundary":null,
                      "updated_at_ms":0,"native_id_map":{}}"#;
        let parsed: SessionCache = serde_json::from_str(old).unwrap();
        assert_eq!(parsed.transcript_path, None);
        let json =
            serde_json::to_string(&SessionCache::new("s", "claude-code", "active", 0)).unwrap();
        assert!(!json.contains("transcript_path"), "None writes no field");
    }

    #[test]
    fn transcript_progress_roundtrips_and_stays_out_of_live_sessions() {
        let tmp = tempfile::tempdir().unwrap();
        let gd = tmp.path();
        assert!(read_transcript_progress(gd, "s1").unwrap().is_none());
        let p = TranscriptProgress {
            schema: SCHEMA,
            session_id: "s1".into(),
            raw_lines: 42,
            updated_at_ms: 7,
        };
        write_transcript_progress(gd, &p).unwrap();
        assert_eq!(read_transcript_progress(gd, "s1").unwrap().unwrap(), p);

        // The sidecar must be invisible to the session enumeration: it is not
        // a session, and a worker-owned file must never make one appear live.
        write_cache(gd, &SessionCache::new("s1", "claude-code", "active", 1)).unwrap();
        let live = live_sessions(gd).unwrap();
        assert_eq!(live.len(), 1, "one session, not one per sidecar");

        // And the unsafe-id guard holds for the sidecar too.
        assert!(read_transcript_progress(gd, "../../evil").is_err());
        let hostile = TranscriptProgress {
            schema: SCHEMA,
            session_id: "../../evil".into(),
            raw_lines: 0,
            updated_at_ms: 0,
        };
        assert!(write_transcript_progress(gd, &hostile).is_err());
    }

    /// The hook-owned usage sidecar mirrors the worker's transcript sidecar:
    /// it round-trips, it is invisible to the session enumeration, the
    /// unsafe-id guard holds for it, and a file written before `settled_turn`
    /// existed still parses (the field defaults to "nothing settled yet").
    #[test]
    fn usage_progress_roundtrips_and_stays_out_of_live_sessions() {
        let tmp = tempfile::tempdir().unwrap();
        let gd = tmp.path();
        assert!(read_usage_progress(gd, "s1").unwrap().is_none());
        let p = UsageProgress {
            schema: SCHEMA,
            session_id: "s1".into(),
            byte_offset: 4096,
            settled_turn: 3,
            updated_at_ms: 7,
        };
        write_usage_progress(gd, &p).unwrap();
        assert_eq!(read_usage_progress(gd, "s1").unwrap().unwrap(), p);
        assert!(gd.join("rift-sessions").join("s1.usage").is_file());

        write_cache(gd, &SessionCache::new("s1", "claude-code", "active", 1)).unwrap();
        write_transcript_progress(
            gd,
            &TranscriptProgress {
                schema: SCHEMA,
                session_id: "s1".into(),
                raw_lines: 1,
                updated_at_ms: 1,
            },
        )
        .unwrap();
        let live = live_sessions(gd).unwrap();
        assert_eq!(live.len(), 1, "one session, not one per sidecar");

        assert!(read_usage_progress(gd, "../../evil").is_err());
        let hostile = UsageProgress {
            session_id: "../../evil".into(),
            ..p.clone()
        };
        assert!(write_usage_progress(gd, &hostile).is_err());

        let old = r#"{"schema":1,"session_id":"s1","byte_offset":12,"updated_at_ms":3}"#;
        let parsed: UsageProgress = serde_json::from_str(old).unwrap();
        assert_eq!(
            parsed.settled_turn, 0,
            "a pre-field file reads as nothing settled"
        );
        assert_eq!(parsed.byte_offset, 12);
    }
}
