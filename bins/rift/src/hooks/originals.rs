//! Spooling the original of a tool result a hook is about to rewrite.
//!
//! Q72 grants a hook two ways to deliver a Bash result other than the one the
//! tool produced (`held_output_substitution`, `folded_output`), on one
//! condition: the original is kept. [`spool_original`] is the keeping, and the
//! runtime half of the condition — a caller delivers its rewrite only on `Ok`;
//! on any `Err` it delivers the original unchanged. The archive seals every
//! spooled record beside the transcript holding the call
//! (`hooks/archive/originals.rs`); nothing else reads the spool.
//!
//! **Where.** `<common_dir>/rift-family/spool/<session>/<tool-use-id>.original`
//! — the family's common dir, never a member's git dir, so a subagent in a
//! temporary worktree spools where its parent session's archive looks. One
//! file per call: a record is up to ~30 KB, past the single-`write` atomicity
//! an append file would rely on, and parallel calls never contend.
//!
//! **What.** The record of [`rift_shadow::backend::SpoolHeader`]: a header and
//! the original's canonical record ([`original_record_json`]) through the
//! redaction gate with the repository's packs. The writer takes
//! [`RedactedBytes`], so raw tool output never reaches the disk; the directory
//! is `0o700` and the file `0o600` — the basis snapshot's class, redacted text
//! at rest, never served.
//!
//! **Never twice.** One entry per call: the file is published by a hard link
//! from a private temporary, which fails when the name exists, so a second
//! call is refused by name (`already_spooled`) and the first is never
//! overwritten. No link is followed anywhere on the path.
//!
//! **The budget.** The hook path opens no store, no object database and no
//! tokenizer: the paths come from the caller's own discovery, the packs from
//! the repository's files. An original over [`MAX_REWRITE_CHARS`] is refused
//! before any work, and a spool that has used [`SPOOL_BUDGET`] by the time the
//! bytes are ready refuses rather than writes, so the rewrite never costs the
//! hook its latency.
//!
//! **Lifetime.** A record stays until it is sealed AND the session's final
//! archive has landed, because every checkpoint re-seals the whole session.
//! Deletion at the final archive is the session lifecycle's (S8 L-4) and is
//! not implemented here; `rift doctor` names a spool whose session has no
//! live state ([`spooled_sessions`]).

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rift_core::{RedactedBytes, RepoId, SecretBytes};
use rift_shadow::backend::rewrite::{
    original_record_json, ranges_fit, LineRange, ResultText, RewriteForm,
};
use rift_shadow::backend::SpoolHeader;

use super::family_state::family_state_dir;
use super::subagent_claims::is_safe_id;

/// The most characters an original's stdout and stderr may hold together:
/// the client's own inline threshold, past which it spills the result to a
/// file (CF-13) rather than deliver it inline. Measured: at this size the
/// spool's p95 stays under 40 ms in a fresh process.
pub(crate) const MAX_REWRITE_CHARS: usize = 30_000;

/// What one spool may spend before it refuses: the hook's 100 ms less the
/// gate's own decision and the rewrite's rendering.
pub(in crate::hooks) const SPOOL_BUDGET: Duration = Duration::from_millis(60);

/// The spool's directory under the family's state directory.
const SPOOL_DIR: &str = "spool";

/// The suffix of one spooled record.
pub(in crate::hooks) const SPOOL_SUFFIX: &str = ".original";

/// The result the client would have recorded, before the rewrite. The streams
/// stay separate, and separate from the interrupted flag, as the client
/// records them.
pub(crate) struct ToolOriginal {
    pub(crate) stdout: SecretBytes,
    pub(crate) stderr: SecretBytes,
    pub(crate) interrupted: bool,
}

/// Where a rewrite happens and what it replaces.
pub(crate) struct RewriteSite<'a> {
    /// The hook's own discovery; the common dir keys the spool.
    pub(crate) repo: &'a rift_git::RepoPaths,
    /// The session id — the parent's for a subagent.
    pub(crate) session_id: &'a str,
    /// The subagent whose window rewrote the result, `None` for the session's.
    pub(crate) agent_id: Option<&'a str>,
    /// The window journal key of that window (64 lowercase hex), so the
    /// sealed record joins the journal's rewrite row.
    pub(crate) window_key: &'a str,
    /// The repository family the session belongs to.
    pub(crate) family_key: &'a RepoId,
    /// The rewritten call.
    pub(crate) tool_use_id: &'a str,
    /// The form the rewrite is delivered in.
    pub(crate) form: RewriteForm,
    /// The original's lines the rewrite replaces, each naming its stream.
    pub(crate) replaced: &'a [LineRange],
}

/// What a spool kept.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SpoolReceipt {
    /// The SHA-256 (lowercase hex) of the spooled body: the redaction gate's
    /// output over the canonical JSON `{"interrupted","stderr","stdout"}` of
    /// the original, keys sorted, one line ([`original_record_json`]). The
    /// sealed `tool-originals/<id>.json` is the archive's second pass over
    /// these bytes, so the manifest's `sha256` is this value whenever that
    /// pass leaves them unchanged (it is idempotent under unchanged packs).
    pub(crate) sha256: String,
    /// The spooled body's length in bytes.
    pub(crate) bytes: u64,
}

/// Why an original was not spooled. Closed; each has a stable word.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SpoolRefusal {
    /// stdout and stderr together exceed [`MAX_REWRITE_CHARS`].
    TooLarge,
    /// This call already has a spooled original; it is never overwritten.
    AlreadySpooled,
    /// An id cannot safely name a directory, a file or a journal row.
    UnsafeId,
    /// A directory on the spool's path is a link or not a directory.
    UnsafePath,
    /// A stream is not text, or the ranges do not fit the original.
    Malformed,
    /// The redaction gate refused the original.
    Unsealed,
    /// The file system refused the write.
    Io,
    /// The spool used its budget before the bytes were ready.
    OverBudget,
}

impl SpoolRefusal {
    /// The refusal's stable word.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "a refusal is not yet named in any report")
    )]
    pub(crate) const fn word(self) -> &'static str {
        match self {
            Self::TooLarge => "too_large",
            Self::AlreadySpooled => "already_spooled",
            Self::UnsafeId => "unsafe_id",
            Self::UnsafePath => "unsafe_path",
            Self::Malformed => "malformed",
            Self::Unsealed => "unsealed",
            Self::Io => "io",
            Self::OverBudget => "over_budget",
        }
    }
}

/// One session's spooled records, as `rift doctor` reports them.
pub(crate) struct SpooledSession {
    pub(crate) session_id: String,
    pub(crate) records: usize,
}

/// Redact the original with the repository's packs and spool it for the
/// archive. `Ok`: the caller may deliver its rewrite. `Err`: it must deliver
/// the original unchanged.
///
/// # Errors
/// A [`SpoolRefusal`]; nothing is left on disk for any of them.
#[must_use = "a rewrite without a spooled original is forbidden (Q72)"]
pub(crate) fn spool_original(
    site: &RewriteSite<'_>,
    original: ToolOriginal,
) -> Result<SpoolReceipt, SpoolRefusal> {
    let started = Instant::now();
    spool_within(site, &original, &|| started.elapsed(), SPOOL_BUDGET)
}

/// [`spool_original`] with a clock that never advances: what tests spool
/// through, so no verdict of theirs depends on the wall clock.
#[cfg(test)]
pub(in crate::hooks) fn spool_untimed(
    site: &RewriteSite<'_>,
    original: &ToolOriginal,
) -> Result<SpoolReceipt, SpoolRefusal> {
    spool_within(site, original, &|| Duration::ZERO, SPOOL_BUDGET)
}

/// [`spool_original`] against an injected clock and budget.
fn spool_within(
    site: &RewriteSite<'_>,
    original: &ToolOriginal,
    elapsed: &dyn Fn() -> Duration,
    budget: Duration,
) -> Result<SpoolReceipt, SpoolRefusal> {
    screen_ids(site)?;
    let text = original_text(original)?;
    if !ranges_fit(text, site.replaced) {
        return Err(SpoolRefusal::Malformed);
    }
    let dir = ensure_spool_dir(&site.repo.common_dir, site.session_id)?;
    let path = dir.join(format!("{}{SPOOL_SUFFIX}", site.tool_use_id));
    if std::fs::symlink_metadata(&path).is_ok() {
        return Err(SpoolRefusal::AlreadySpooled);
    }
    let body = redact_original(&site.repo.work_tree, text, original.interrupted)?;
    if elapsed() > budget {
        return Err(SpoolRefusal::OverBudget);
    }
    let receipt = SpoolReceipt {
        sha256: body.redacted_sha256().to_hex(),
        bytes: u64::try_from(body.as_slice().len()).unwrap_or(u64::MAX),
    };
    let header = SpoolHeader {
        tool_use_id: site.tool_use_id.to_owned(),
        agent_id: site.agent_id.map(str::to_owned),
        window_key: site.window_key.to_owned(),
        family_key: site.family_key.as_str().to_owned(),
        form: site.form,
        replaced: site.replaced.to_vec(),
        sha256: receipt.sha256.clone(),
        bytes: receipt.bytes,
    };
    publish(&dir, &path, &header.record(&body))?;
    Ok(receipt)
}

/// Every id the spool names a path or a row with must be one it can.
fn screen_ids(site: &RewriteSite<'_>) -> Result<(), SpoolRefusal> {
    let window = site.window_key.len() == 64
        && site
            .window_key
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    let safe = rift_shadow::hooks::is_safe_session_id(site.session_id)
        && is_safe_id(site.tool_use_id)
        && site.agent_id.is_none_or(is_safe_id)
        && window;
    if safe {
        Ok(())
    } else {
        Err(SpoolRefusal::UnsafeId)
    }
}

/// The original's streams as text, refused past the ceiling before anything
/// else reads them.
fn original_text(original: &ToolOriginal) -> Result<ResultText<'_>, SpoolRefusal> {
    // Four bytes is the widest a character can be: past this, no reading of
    // the bytes can bring the count back under the ceiling.
    let ceiling_bytes = MAX_REWRITE_CHARS.saturating_mul(4);
    if original.stdout.len().saturating_add(original.stderr.len()) > ceiling_bytes {
        return Err(SpoolRefusal::TooLarge);
    }
    let (Ok(stdout), Ok(stderr)) = (
        std::str::from_utf8(original.stdout.as_slice()),
        std::str::from_utf8(original.stderr.as_slice()),
    ) else {
        return Err(SpoolRefusal::Malformed);
    };
    if stdout.chars().count() + stderr.chars().count() > MAX_REWRITE_CHARS {
        return Err(SpoolRefusal::TooLarge);
    }
    Ok(ResultText { stdout, stderr })
}

/// The original's canonical record through the gate, with the repository's
/// own packs — the same the archive seals the transcript with.
fn redact_original(
    work_tree: &Path,
    text: ResultText<'_>,
    interrupted: bool,
) -> Result<RedactedBytes, SpoolRefusal> {
    let raw = SecretBytes::new(original_record_json(text.stdout, text.stderr, interrupted));
    let (packs, _notes) = crate::redactor_packs::capture_rule_packs(work_tree);
    rift_shadow::redact::redact_with(&raw, &packs)
        .map_err(|_| SpoolRefusal::Unsealed)
        .and_then(|out| {
            rift_shadow::backend::rewrite::parse_original_record(out.redacted.as_slice())
                .filter(|(_, _, actual)| *actual == interrupted)
                .map(|_| out.redacted)
                .ok_or(SpoolRefusal::Unsealed)
        })
}

/// The session's spool directory, created owner-only where missing, every
/// component inspected without following a link.
fn ensure_spool_dir(common_dir: &Path, session_id: &str) -> Result<PathBuf, SpoolRefusal> {
    let family = family_state_dir(common_dir);
    let spool = family.join(SPOOL_DIR);
    let session = spool.join(session_id);
    for dir in [&family, &spool, &session] {
        ensure_private_dir(dir)?;
    }
    Ok(session)
}

/// One directory that must be a real directory, created `0o700` if absent.
fn ensure_private_dir(dir: &Path) -> Result<(), SpoolRefusal> {
    match std::fs::symlink_metadata(dir) {
        Ok(meta) if meta.file_type().is_dir() => return Ok(()),
        Ok(_) => return Err(SpoolRefusal::UnsafePath),
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(SpoolRefusal::Io),
        Err(_) => {}
    }
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    match builder.create(dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(_) => return Err(SpoolRefusal::Io),
    }
    // A racing creator may have put anything there: inspect what exists now.
    match std::fs::symlink_metadata(dir) {
        Ok(meta) if meta.file_type().is_dir() => Ok(()),
        Ok(_) => Err(SpoolRefusal::UnsafePath),
        Err(_) => Err(SpoolRefusal::Io),
    }
}

/// Write the record to a private temporary and publish it under its name by
/// a hard link, which refuses an existing name rather than replace it.
fn publish(dir: &Path, path: &Path, record: &[u8]) -> Result<(), SpoolRefusal> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let temp = dir.join(format!(".{name}.{}.tmp", std::process::id()));
    let written = write_private(&temp, record);
    let published = written.and_then(|()| {
        std::fs::hard_link(&temp, path).map_err(|e| match e.kind() {
            std::io::ErrorKind::AlreadyExists => SpoolRefusal::AlreadySpooled,
            _ => SpoolRefusal::Io,
        })
    });
    let _ = std::fs::remove_file(&temp);
    published
}

/// A new owner-only file holding `bytes`; never opened through a link, never
/// over an existing file.
fn write_private(path: &Path, bytes: &[u8]) -> Result<(), SpoolRefusal> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(path).map_err(|_| SpoolRefusal::Io)?;
    file.write_all(bytes).map_err(|_| SpoolRefusal::Io)
}

/// The spool directory of `session_id` in the family whose common dir is
/// `common_dir`, whether or not it exists.
pub(in crate::hooks) fn session_spool_dir(common_dir: &Path, session_id: &str) -> PathBuf {
    family_state_dir(common_dir)
        .join(SPOOL_DIR)
        .join(session_id)
}

/// Every session with a spool directory, and how many records it holds. A
/// finding source for `rift doctor`; nothing is removed here.
pub(crate) fn spooled_sessions(common_dir: &Path) -> Vec<SpooledSession> {
    let root = family_state_dir(common_dir).join(SPOOL_DIR);
    let Ok(entries) = std::fs::read_dir(&root) else {
        return Vec::new();
    };
    let mut out: Vec<SpooledSession> = entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter_map(|e| {
            let session_id = e.file_name().to_str()?.to_owned();
            let records = std::fs::read_dir(e.path()).map_or(0, |files| {
                files
                    .flatten()
                    .filter(|f| f.file_name().to_string_lossy().ends_with(SPOOL_SUFFIX))
                    .count()
            });
            Some(SpooledSession {
                session_id,
                records,
            })
        })
        .collect();
    out.sort_by(|a, b| a.session_id.cmp(&b.session_id));
    out
}

#[cfg(test)]
mod tests;
