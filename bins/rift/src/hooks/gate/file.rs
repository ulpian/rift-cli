//! The bytes a call names: one spelling of a path, one hash of the file at
//! it, and the request resolved against both.
//!
//! Separate because every refusal in here is a read let through with nothing
//! recorded, and keeping them together is what keeps the record from ever
//! claiming something about bytes the agent was never handed.

use std::path::{Path, PathBuf};

/// One file's bytes as the gate needs them: which revision, how long, and how
/// far a run of lines reaches into it.
pub(in crate::hooks) struct GateFile {
    /// This checkout's own spelling of the path.
    pub(in crate::hooks) rel_path: String,
    /// The revision the bytes hash to now.
    pub(in crate::hooks) hash: rift_core::Sha256Id,
    /// The same bytes as git names them — the source the worker falls back
    /// to when the working file has moved on by the time it counts.
    pub(super) blob_oid: String,
    /// How many lines the file has.
    pub(in crate::hooks) total_lines: u32,
    /// The bytes, kept only long enough to measure a slice of them.
    pub(in crate::hooks) bytes: Vec<u8>,
}

impl GateFile {
    /// The bytes as text, or nothing when they are not text this can search.
    ///
    /// An encoding no one here can read is not a reason to claim anything: the
    /// stretch an agent wrote is then looked for in an empty haystack, is not
    /// found, and the change is recorded as one whose places do not follow
    /// from its payload.
    pub(in crate::hooks) fn text(&self) -> &str {
        std::str::from_utf8(&self.bytes).unwrap_or_default()
    }

    /// How many bytes the lines `lo..=hi` amount to.
    pub(super) fn slice_bytes(&self, run: rift_agent::window_journal::Interval) -> usize {
        self.bytes
            .split_inclusive(|byte| *byte == b'\n')
            .skip(run.lo as usize - 1)
            .take((run.hi - run.lo) as usize + 1)
            .map(<[u8]>::len)
            .sum()
    }
}

/// The file a request names and the request resolved against it, or `None`
/// where this checkout has nothing to say about either.
///
/// Two refusals in one place because they are one question — *which lines of
/// which revision is this call about* — and every way of failing to answer it
/// leaves the call alone and records nothing.
pub(super) fn gate_resolve(
    work_tree: &Path,
    request: rift_agent::ReadRequest,
    cwd: Option<&Path>,
    effective: &rift_recall_policy::Effective,
) -> Option<(GateFile, rift_agent::window_journal::ResolvedRead)> {
    let file = gate_file(
        work_tree,
        &request.path,
        cwd,
        effective.max_inline_hash_bytes,
    )?;
    let read = rift_agent::window_journal::ResolvedRead::new(
        &rift_agent::ReadRequest {
            path: file.rel_path.clone(),
            ..request
        },
        file.hash,
        file.total_lines,
    )?;
    Some((file, read))
}

/// Resolve a payload path to bytes of **this** checkout, or `None`.
///
/// Every refusal here is a read let through and nothing recorded, and each one
/// closes a way the record could claim something about bytes the agent was
/// never handed:
///
/// * a path this checkout has no name for ([`gate_rel_path`]), or one naming
///   something that is not a file, belongs to no checkout this journal
///   describes;
/// * a file past `max_bytes` — the repository's own inline-hash cap — is not
///   hashed, because the budget is the agent's and a saving is not worth
///   spending it;
/// * bytes holding a zero are not lines, and a range of them is not a range
///   of anything.
pub(in crate::hooks) fn gate_file(
    work_tree: &Path,
    payload_path: &str,
    cwd: Option<&Path>,
    max_bytes: u64,
) -> Option<GateFile> {
    let root = work_tree.canonicalize().ok()?;
    let rel_path = gate_rel_path(&root, payload_path, cwd)?;
    let canonical = root.join(&rel_path);
    let meta = std::fs::metadata(&canonical).ok()?;
    if !meta.is_file() || meta.len() > max_bytes {
        return None;
    }
    // The hasher the ingest already uses on this path, over the same bytes it
    // reads once: one read, one hash, no second visit to the filesystem.
    let (blob, bytes) = rift_recall_substrate::worktree::hash_blob_bytes(&root, &rel_path)?;
    if bytes.contains(&0) {
        return None;
    }
    // Lines as the file has them: a terminated run and an unterminated tail
    // are each one line, and an empty file has none.
    let total_lines = bytes.split_inclusive(|byte| *byte == b'\n').count();
    Some(GateFile {
        hash: rift_core::Sha256Id::parse_hex(&blob.content_sha256).ok()?,
        blob_oid: blob.blob_oid,
        total_lines: u32::try_from(total_lines).ok()?,
        rel_path,
        bytes,
    })
}

/// This checkout's own spelling of a path a payload named, or `None` when the
/// payload named something this checkout has no name for.
///
/// One spelling, in one place, because coverage is keyed by this string: two
/// routes to one file — a symlinked prefix, a relative name resolved against
/// some other directory — must not become two claims about two files.
///
/// A relative name is resolved against the working directory the **payload
/// itself states**, and against nothing else. That is what a shell read is
/// usually written as (`sed -n '1,40p' src/lib.rs`), so refusing one outright
/// dropped a whole class of read the recogniser had already understood. The
/// hook process's own cwd is deliberately not a fallback: a hook runs wherever
/// the client launched it, which is not a statement about where the agent
/// stood, and a guess here is a claim about bytes some other file holds. No
/// stated directory, or one that is itself relative, and the read is let
/// through with nothing recorded.
///
/// A file that does not exist yet is still named: a whole-file write says
/// beforehand which bytes it is about to put somewhere, and the somewhere is a
/// directory that does exist. The parent is canonicalized and the name put
/// back on, so a write that creates a file is keyed exactly as the read that
/// follows it will be.
pub(in crate::hooks) fn gate_rel_path(
    root: &Path,
    payload_path: &str,
    cwd: Option<&Path>,
) -> Option<String> {
    let named = Path::new(payload_path);
    let resolved;
    let named = if named.is_absolute() {
        named
    } else {
        resolved = cwd.filter(|dir| dir.is_absolute())?.join(named);
        resolved.as_path()
    };
    let canonical = match named.canonicalize() {
        Ok(here) => here,
        Err(_) => named
            .parent()?
            .canonicalize()
            .ok()?
            .join(named.file_name()?),
    };
    rift_recall_substrate::worktree::repo_relative_slash(root, &canonical)
}

/// The directory this payload says the agent was standing in, when it says one
/// in the only field that means that.
///
/// Only [`rift_agent::CwdSource::PayloadCwd`] answers. Antigravity sends no
/// working directory at all and its adapter fills the fact from the IDE's
/// workspace root under its own source token; a workspace root is where a
/// project lives, not where a command ran, so resolving a relative name
/// against it would be inventing a path. That family's own shell arguments
/// carry a `Cwd`, and its adapter has already resolved them by the time a
/// request reaches here.
pub(crate) fn payload_cwd(facts: &rift_agent::HookFacts) -> Option<PathBuf> {
    let stated = facts.cwd.as_ref()?;
    (stated.source == rift_agent::CwdSource::PayloadCwd).then(|| PathBuf::from(&stated.path))
}
