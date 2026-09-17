//! The family's Markdown sidecars: one converted document, shared by every
//! member of the repository family.
//!
//! An agent that opens `spec.docx` is handed Markdown instead of zip bytes.
//! Producing that Markdown costs a process and a parse, so it is done **once
//! per document version per family** and read thereafter. "Once per family" is
//! the whole point: a linked worktree is a member of its repository's family,
//! subagents run in temporary worktrees, and a cache keyed by a checkout path
//! would convert the same file again in every one of them and lose the lot
//! when the worktree is removed.
//!
//! So the store is two directories:
//!
//! * `<common_dir>/rift-family/doc/<key>.md` — the family's own copy, beside
//!   the family manifest, in the one directory every member already shares.
//!   Written temp-file-then-rename, so a reader never sees half a conversion.
//! * `<work_tree>/.rift/state/doc/<key>.md` — this member's **view** of it, a
//!   hardlink (a copy where the filesystem refuses one). A member's view is
//!   what an agent's tool call can name — a path inside the checkout it is
//!   working in — and it costs one inode. Delete the worktree and the view
//!   goes with it; the family's copy is untouched, and the next member to want
//!   that document links it instead of converting it.
//!
//! **The key is the identity of the answer, not of the file.** It is
//! `sha256(source bytes) ‖ engine version ‖ policy version`, hashed once into a
//! path-safe name: change a byte of the document, upgrade the converter, or
//! change a cap that changes the Markdown, and the key changes with it. That is
//! what makes a stale sidecar unreachable rather than wrong — the same rule the
//! recall plane's summaries live by. Nothing is overwritten in place; a new
//! answer is a new name.
//!
//! **Why a sidecar is durable text that needs no redaction gate.** Every byte
//! here is derived from a file in the repository's own work tree — bytes the
//! developer wrote or committed, which git already stores in the clear — and
//! from nothing else. No transcript, no tool output and no agent-authored
//! payload reaches this module: a converter is handed file bytes and the
//! file's extension, and its answer is written under the hash of those bytes.
//! Rift's redaction gate exists for what a session *produces* on its way into
//! the durable capture record; a sidecar is a local re-encoding of a file that
//! is already in the checkout, kept beside that checkout, owner-readable, and
//! collected like every other transient. The one class that must never be
//! re-encoded is the one the repository declared sensitive, and the caller
//! that walks documents into this store skips those paths before it reads a
//! byte.
//!
//! Nothing here opens a store or initialises a tokenizer, and the conversion
//! half is never reached from a decision: a hook may ask whether an answer
//! exists and hand over the one that does, and everything that costs more than
//! a `stat` belongs to the detached worker.

use std::path::{Path, PathBuf};

use rift_core::Result;
use rift_recall_policy::{DocPolicy, SERVE_POLICY_VERSION};

/// The family's own directory inside the git common dir — the same one the
/// family manifest lives in, because they answer the same question: what does
/// this repository share between its checkouts?
const FAMILY_DIR: &str = "rift-family";

/// The sidecar subdirectory, under the family directory and under a member's
/// run state alike.
const DOC_DIR: &str = "doc";

/// The extension a converted document is written under.
const SIDECAR_EXT: &str = "md";

/// The marker written beside a key that must not be converted again: a scanned
/// PDF, an encrypted or malformed container. Its content is a one-word reason
/// for whoever reads the directory; nothing parses it back.
const SKIP_EXT: &str = "skip";

/// The document converter this store's keys are the answers of.
///
/// It is stated here rather than read from the converter because the crate
/// that owns this number is optional in this binary (`--no-default-features`
/// drops the seam and the engine together) while a sidecar key must be
/// computable in every build. A test binds this to the workspace's pin, so
/// the two cannot drift apart silently: an engine upgrade that left this
/// behind would serve every existing sidecar as though the new engine had
/// produced it; where the seam is linked the test holds the two equal too.
pub(crate) const ENGINE_VERSION: &str = "0.2.4";

/// The document extensions this binary treats as documents.
///
/// Spelled here for the same reason [`ENGINE_VERSION`] is: the crate that owns
/// the list is optional in this binary (`--no-default-features` drops the
/// seam), while the verb that asks "is this a document?" must answer in every
/// build. A test holds the two lists equal wherever the seam is linked, so
/// they cannot drift in silence.
pub(crate) const DOCUMENT_EXTENSIONS: [&str; 14] = [
    "csv", "doc", "docm", "docx", "epub", "odp", "ods", "odt", "pdf", "ppt", "pptx", "rtf", "xls",
    "xlsx",
];

/// Whether `ext` names a document Rift converts. A leading `.` is accepted and
/// the comparison is case-insensitive: callers get their extension from a path,
/// and paths carry both spellings.
pub(crate) fn is_document_extension(ext: &str) -> bool {
    let ext = ext.strip_prefix('.').unwrap_or(ext);
    DOCUMENT_EXTENSIONS
        .iter()
        .any(|known| ext.eq_ignore_ascii_case(known))
}

/// The family's sidecars and one member's view of them.
///
/// Two paths and no state: every operation is a filesystem operation, so a
/// crash leaves either the old answer or the new one and never a torn one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DocStore {
    family: PathBuf,
    member: PathBuf,
}

impl DocStore {
    /// The store as `work_tree` sees it, given the family's common directory.
    ///
    /// `common_dir` is the family's — canonical, shared by every member — so
    /// two members of one family name the same file, and a member that is
    /// removed tomorrow still contributes to it today.
    pub(crate) fn for_member(common_dir: &Path, work_tree: &Path) -> Self {
        Self {
            family: common_dir.join(FAMILY_DIR).join(DOC_DIR),
            member: work_tree.join(".rift/state/doc"),
        }
    }

    /// Where the family keeps `key`'s answer.
    pub(crate) fn family_path(&self, key: &str) -> PathBuf {
        self.family.join(format!("{key}.{SIDECAR_EXT}"))
    }

    /// Where this member sees it.
    pub(crate) fn member_path(&self, key: &str) -> PathBuf {
        self.member.join(format!("{key}.{SIDECAR_EXT}"))
    }

    /// The marker that says `key` was looked at and is not to be looked at
    /// again.
    fn skip_path(&self, key: &str) -> PathBuf {
        self.family.join(format!("{key}.{SKIP_EXT}"))
    }

    /// Whether the family already holds this answer — one `stat`, no writes.
    ///
    /// This is the whole of what a decision on a hook path may do: ask, and
    /// hand over what is already there. Converting is the worker's.
    pub(crate) fn holds(&self, key: &str) -> bool {
        self.family_path(key).is_file()
    }

    /// Whether this key was decided against: a document whose conversion is
    /// not worth substituting (a scanned PDF) or cannot be produced at all (an
    /// encrypted or malformed container). Checked before a conversion is
    /// attempted, so a document that refused once refuses cheaply, for every
    /// member of the family.
    pub(crate) fn is_skipped(&self, key: &str) -> bool {
        self.skip_path(key).exists()
    }

    /// Record `key`'s answer in the family store and link it into this member.
    ///
    /// Atomic by rename inside the family directory, so a reader sees either
    /// the whole sidecar or no sidecar. Recording an answer that is already
    /// there is not a rewrite: the key is the hash of what this would have
    /// written.
    ///
    /// Written only where a conversion happened, so a build without the
    /// document seam reads and sweeps this store without ever writing to it.
    ///
    /// # Errors
    /// [`rift_core::RiftError::Io`] if the family directory cannot be written.
    #[cfg(feature = "doc")]
    pub(crate) fn put(&self, key: &str, text: &str) -> Result<PathBuf> {
        let path = self.family_path(key);
        if !path.is_file() {
            std::fs::create_dir_all(&self.family)?;
            // Named for the writing process, so two workers converting one
            // document at the same instant cannot tear each other's temp
            // file. Both then rename onto the same name with the same bytes,
            // and the loser's rename is a no-op.
            let tmp = self
                .family
                .join(format!("{key}.{SIDECAR_EXT}.tmp{}", std::process::id()));
            std::fs::write(&tmp, text.as_bytes())?;
            restrict(&tmp);
            std::fs::rename(&tmp, &path)?;
        }
        self.link(key)
    }

    /// Say that `key` is not to be attempted again, and why in one word.
    ///
    /// Written only for a decision about the *document* — no text layer,
    /// encrypted, not the format its name claims. A helper that was missing or
    /// a deadline that was missed says nothing about the document and leaves
    /// no marker, so the next pass tries again. Like [`Self::put`], a decision
    /// about a document is something only a converting build reaches.
    #[cfg(feature = "doc")]
    pub(crate) fn mark_skipped(&self, key: &str, reason: &str) {
        if std::fs::create_dir_all(&self.family).is_ok() {
            let _ = std::fs::write(self.skip_path(key), reason.as_bytes());
        }
    }

    /// Link the family's answer into this member, copying where the filesystem
    /// will not link (a different device, or one with no hardlinks at all).
    ///
    /// # Errors
    /// [`rift_core::RiftError::Io`] if neither a link nor a copy could be made.
    pub(crate) fn link(&self, key: &str) -> Result<PathBuf> {
        let source = self.family_path(key);
        let member = self.member_path(key);
        if member.is_file() {
            return Ok(member);
        }
        std::fs::create_dir_all(&self.member)?;
        match std::fs::hard_link(&source, &member) {
            Ok(()) => Ok(member),
            // Somebody linked it between the two lines above. The name is the
            // hash of the content, so whoever won wrote the same bytes.
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(member),
            Err(_) => {
                std::fs::copy(&source, &member)?;
                Ok(member)
            }
        }
    }

    /// What this member and its family are holding, for `rift doctor`.
    pub(crate) fn survey(&self) -> Survey {
        let mut survey = Survey::default();
        for (_, path) in sidecars(&self.family) {
            survey.sidecars += 1;
            survey.bytes += std::fs::metadata(&path).map_or(0, |m| m.len());
        }
        for (key, _) in sidecars(&self.member) {
            survey.member_links += 1;
            if !self.holds(&key) {
                survey.orphan_links += 1;
            }
        }
        survey
    }

    /// Collect what has expired and what has been orphaned, and say how much.
    ///
    /// * A family sidecar untouched for longer than the TTL goes, with this
    ///   member's view of it. Losing one costs a conversion and never an
    ///   answer: the document is still in the checkout and still hashes to the
    ///   same key.
    /// * A member view with no family answer behind it is an **orphan** — the
    ///   remains of an expired sidecar, or of a family store somebody removed
    ///   — and it goes whatever its age, because nothing will ever refresh it
    ///   and an agent must not be pointed at it.
    /// * A temp file past the TTL is the corpse of a write that died before
    ///   its rename.
    ///
    /// Total and quiet, like every other sweep in this tool: a name this
    /// module did not write is left alone however it got there, a file whose
    /// age cannot be read is kept (removing on an unknown age would collect a
    /// sidecar written a second ago), and a removal that fails is not counted.
    pub(crate) fn sweep(&self, now_ms: i64, ttl_ms: i64, dry_run: bool) -> SweepReport {
        let cutoff = now_ms.saturating_sub(ttl_ms);
        let mut report = SweepReport::default();
        for (key, path) in sidecars(&self.family) {
            if !older_than(&path, cutoff) {
                continue;
            }
            report.expired += 1;
            if !dry_run {
                let _ = std::fs::remove_file(&path);
                let _ = std::fs::remove_file(self.skip_path(&key));
                let _ = std::fs::remove_file(self.member_path(&key));
            }
        }
        for (key, path) in sidecars(&self.member) {
            if self.holds(&key) {
                continue;
            }
            report.orphan_links += 1;
            if !dry_run {
                let _ = std::fs::remove_file(&path);
            }
        }
        report.swept_tmps = sweep_tmps(&self.family, cutoff, dry_run);
        report.swept_windows = sweep_windows(&self.member, cutoff, dry_run);
        report
    }
}

// ------------------------------------------- one context window's decisions ---

/// The directory a window's document decisions are remembered in, under this
/// member's own run state.
const WINDOW_DIR: &str = "w";

/// The marker that says this window is answered by the redirect rather than by
/// the substitution.
#[cfg(feature = "doc")]
const REDIRECT_MARKER: &str = "redirect";

/// What one **context window** has already been told about documents.
///
/// Three facts, and each is a file whose *existence* is the fact, created with
/// `create_new` so that the process which created it is the one that acted.
/// That is not a stylistic choice: two of the three are "has this happened
/// yet", a client may run its hooks in parallel, and a read-modify-write of a
/// shared record would let two hooks both conclude they were first — which for
/// the redirect means declining the same read twice, and a decline is allowed
/// exactly once per key precisely so that the repeated call is the escape
/// hatch its own sentence promises.
///
/// Per window and not per session: a subagent runs under its parent's session
/// id with its own agent id and holds different bytes, so it is told
/// separately — the same key the read ledger is filed under, computed by the
/// same function.
///
/// Local run state. Losing the directory costs one extra redirect or one extra
/// substitution and can never cost an answer.
///
/// A window remembers what the document gate said, so it exists where the
/// gate does: a build without the document seam answers no document read and
/// has nothing to remember. What outlives the feature is the sweep below,
/// which removes the directories a converting build left behind.
#[cfg(feature = "doc")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DocWindow {
    dir: PathBuf,
}

/// One amendment this window made, as its result finds it again: the document
/// the agent asked for, and the identity of the bytes the amendment was made
/// about.
#[cfg(feature = "doc")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ArmedRewrite {
    /// This checkout's own spelling of the document.
    pub(crate) rel_path: String,
    /// The bytes it had when the substitution was decided — the identity the
    /// declaration is made under, and the one a worker re-reads it by.
    pub(crate) content_sha256: String,
}

#[cfg(feature = "doc")]
impl DocWindow {
    /// The window a hook belongs to, as this member sees it.
    pub(crate) fn at(work_tree: &Path, session_id: &str, agent_id: Option<&str>) -> Self {
        Self {
            dir: work_tree
                .join(".rift/state/doc")
                .join(WINDOW_DIR)
                .join(crate::read_ledger::key(session_id, agent_id)),
        }
    }

    /// Whether this window has been flipped to the redirect.
    pub(crate) fn is_redirecting(&self) -> bool {
        self.dir.join(REDIRECT_MARKER).exists()
    }

    /// Flip it, and say whether **this** call is the one that did.
    ///
    /// The caller records a dropped substitution once, not once per hook that
    /// notices the same drop.
    pub(crate) fn flip_to_redirect(&self) -> bool {
        self.create_marker(REDIRECT_MARKER, b"dropped")
    }

    /// Take the redirect for `key`, and say whether this call may make it.
    ///
    /// `false` means some earlier call in this window already named that
    /// sidecar, so the read runs: **one decline per key**, and the repeat is
    /// the escape hatch the frame itself promises.
    pub(crate) fn redirect_once(&self, key: &str) -> bool {
        is_sha256(key) && self.create_marker(&format!("r-{key}"), b"")
    }

    /// Remember that this window's read of `rel_path` was answered with the
    /// sidecar `key` names, so the result of that read can be checked against
    /// it — from whichever side the result arrives on.
    ///
    /// **Two markers for one amendment**, because a result names one of two
    /// paths and each of them has to find the fact from its own name: the
    /// sidecar, which means the amendment survived to the tool and the read
    /// the agent wrote never happened, or the document, which means a sibling
    /// hook's amendment was the one the client kept. Each marker names the
    /// other, so whichever is taken takes its twin with it: one amendment is
    /// answered once, and no later result can answer it a second time.
    ///
    /// The names are digests and keys — never a path — and the bodies carry
    /// the identity of the bytes the amendment was made about, so the result's
    /// own hook hashes nothing and the accounting row it journals does not
    /// have to re-read the file to say what it is about.
    pub(crate) fn arm_rewrite(&self, rel_path: &str, key: &str, content_sha256: &str) {
        if !is_sha256(key) || !is_sha256(content_sha256) || rel_path.is_empty() {
            return;
        }
        let _ = self.create_marker(
            &Self::armed(rel_path),
            format!("{content_sha256}\n{key}").as_bytes(),
        );
        let _ = self.create_marker(
            &Self::armed_sidecar(key),
            format!("{content_sha256}\n{rel_path}").as_bytes(),
        );
    }

    /// Take the arming for `rel_path` — the **dropped** side — and hand back
    /// the identity of the bytes it was made about.
    ///
    /// Consumed, not read: a substitution that was dropped is recorded once,
    /// and the window is flipped once. Its twin goes with it, so the sidecar
    /// this window was pointed at can no longer be presented as a substitution
    /// that worked.
    pub(crate) fn take_armed(&self, rel_path: &str) -> Option<String> {
        let (sha, key) = self.take_marker(&Self::armed(rel_path))?;
        let _ = std::fs::remove_file(self.dir.join(Self::armed_sidecar(&key)));
        is_sha256(&sha).then_some(sha)
    }

    /// Take the arming the sidecar for `key` was made under — the **verified**
    /// side — and hand back the document it stood in for.
    ///
    /// `None` for a sidecar this window never armed: an agent reading Rift's
    /// own Markdown on its own account is not a substitution, and only a read
    /// Rift amended may be claimed.
    pub(crate) fn take_armed_sidecar(&self, key: &str) -> Option<ArmedRewrite> {
        if !is_sha256(key) {
            return None;
        }
        let (content_sha256, rel_path) = self.take_marker(&Self::armed_sidecar(key))?;
        let _ = std::fs::remove_file(self.dir.join(Self::armed(&rel_path)));
        (is_sha256(&content_sha256) && !rel_path.is_empty()).then_some(ArmedRewrite {
            rel_path,
            content_sha256,
        })
    }

    /// Read one marker's two fields and remove it, so the fact it carries is
    /// answered by exactly one caller.
    ///
    /// The identity comes first because it is a fixed shape; the whole of what
    /// follows the first newline is the other field, so a path that carries a
    /// newline of its own survives the round trip.
    fn take_marker(&self, name: &str) -> Option<(String, String)> {
        let path = self.dir.join(name);
        let body = std::fs::read_to_string(&path).ok()?;
        std::fs::remove_file(&path).ok()?;
        let (head, tail) = body.split_once('\n')?;
        Some((head.to_owned(), tail.to_owned()))
    }

    /// The marker name for one path.
    fn armed(rel_path: &str) -> String {
        format!(
            "p-{}",
            rift_hashchain::leaf_hash(rel_path.as_bytes()).to_hex()
        )
    }

    /// The marker name for the sidecar one amendment pointed at.
    fn armed_sidecar(key: &str) -> String {
        format!("s-{key}")
    }

    /// Create one marker, and say whether this call created it.
    fn create_marker(&self, name: &str, body: &[u8]) -> bool {
        if std::fs::create_dir_all(&self.dir).is_err() {
            return false;
        }
        let path = self.dir.join(name);
        let Ok(mut file) = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        else {
            return false;
        };
        use std::io::Write as _;
        let _ = file.write_all(body);
        restrict(&path);
        true
    }
}

/// Remove the window directories of sessions that ended longer ago than the
/// TTL, and say how many went.
///
/// A window's markers are the memory of what one conversation was told, and a
/// conversation that has been over for a month is not going to be told
/// anything more. Directories only, named the way [`crate::read_ledger::key`]
/// names one, and a name this module did not write is left alone.
fn sweep_windows(dir: &Path, cutoff_ms: i64, dry_run: bool) -> usize {
    let Ok(entries) = std::fs::read_dir(dir.join(WINDOW_DIR)) else {
        return 0;
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        let named = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(is_sha256);
        if !named || !path.is_dir() || !older_than(&path, cutoff_ms) {
            continue;
        }
        removed += 1;
        if !dry_run {
            let _ = std::fs::remove_dir_all(&path);
        }
    }
    removed
}

/// What a member and its family hold, for `rift doctor`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Survey {
    /// Sidecars in the family store.
    pub(crate) sidecars: usize,
    /// Bytes those sidecars occupy.
    pub(crate) bytes: u64,
    /// Views this member holds.
    pub(crate) member_links: usize,
    /// Views whose family answer is gone.
    pub(crate) orphan_links: usize,
}

/// What one sweep removed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct SweepReport {
    /// Family sidecars past the TTL.
    pub(crate) expired: usize,
    /// Member views with no family answer behind them.
    pub(crate) orphan_links: usize,
    /// Temp files left by a write that died before its rename.
    pub(crate) swept_tmps: usize,
    /// Window directories whose conversation ended longer ago than the TTL.
    pub(crate) swept_windows: usize,
}

/// The `(key, path)` of every sidecar in `dir` — and only the sidecars: a name
/// this module did not mint is not one, whatever its extension.
fn sidecars(dir: &Path) -> Vec<(String, PathBuf)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            let name = path.file_name()?.to_str()?;
            let key = name.strip_suffix(&format!(".{SIDECAR_EXT}"))?;
            is_sha256(key).then(|| (key.to_owned(), path.clone()))
        })
        .collect()
}

/// Whether `path` was last modified before `cutoff` (epoch ms). A time the
/// filesystem will not report is **not** old.
fn older_than(path: &Path, cutoff_ms: i64) -> bool {
    let Ok(modified) = std::fs::metadata(path).and_then(|m| m.modified()) else {
        return false;
    };
    let Ok(since) = modified.duration_since(std::time::UNIX_EPOCH) else {
        return false;
    };
    i64::try_from(since.as_millis()).unwrap_or(i64::MAX) < cutoff_ms
}

/// Remove the temp files of writes that died before their rename.
fn sweep_tmps(dir: &Path, cutoff_ms: i64, dry_run: bool) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let marker = format!(".{SIDECAR_EXT}.tmp");
    let mut removed = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        let is_tmp = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.contains(&marker));
        if !is_tmp || !older_than(&path, cutoff_ms) {
            continue;
        }
        removed += 1;
        if !dry_run {
            let _ = std::fs::remove_file(&path);
        }
    }
    removed
}

/// The sidecar key for bytes whose SHA-256 is `content_sha256`, under `doc`.
///
/// `None` for anything that is not a 64-character lowercase hex hash: the key
/// becomes a file name, and a name is never built out of a string this program
/// has not checked.
///
/// The three inputs are the three things that decide what the Markdown says —
/// the document, the converter, and the caps the conversion ran under —
/// separated by a byte that occurs in none of them, so no two of them can
/// spell a third.
pub(crate) fn sidecar_key(content_sha256: &str, doc: &DocPolicy) -> Option<String> {
    if !is_sha256(content_sha256) {
        return None;
    }
    Some(rift_shadow::sha256_hex(&[
        content_sha256.as_bytes(),
        b"\0",
        ENGINE_VERSION.as_bytes(),
        b"\0",
        policy_version(doc).as_bytes(),
    ]))
}

/// The sidecar key a path names, when the path is this member's view of one.
///
/// The one question a read's *result* has to answer: did the tool run with the
/// path Rift substituted, or with the document the agent wrote? A view is a
/// key and an extension directly under this member's document state, so a name
/// with a separator left in it — a window's own markers live one level down —
/// is not one, and neither is anything whose stem is not a key this module
/// would have written.
///
/// Asked by the gate that made the substitution, and by nothing else.
#[cfg(feature = "doc")]
pub(crate) fn member_sidecar_key(rel_path: &str) -> Option<&str> {
    // The path is spelled here the way every other body that builds this
    // directory spells it, rather than shared through a constant: the
    // repository's own guard on family-keyed caches finds these sites by
    // reading the literal, and a constant would hide them from it.
    let name = rel_path
        .strip_prefix(".rift/state/doc")
        .and_then(|rest| rest.strip_prefix('/'))?;
    let (key, ext) = name.rsplit_once('.')?;
    (ext == SIDECAR_EXT && !key.contains('/') && is_sha256(key)).then_some(key)
}

/// The policy half of a key: the semantics version, plus every cap that
/// changes the bytes of a conversion.
///
/// Deliberately not the whole `[doc]` table. The mode, the channel, the
/// deadline and the TTL decide whether and when a document is converted and
/// how long its answer is kept — none of them changes what the answer says —
/// so including them would throw away every sidecar in the family the first
/// time somebody moved the repository from `observe` to `enforce`.
fn policy_version(doc: &DocPolicy) -> String {
    format!(
        "{SERVE_POLICY_VERSION}:{}:{}:{}:{}",
        doc.formats.as_str(),
        doc.table_rows,
        doc.max_input_bytes,
        doc.pdf_min_chars_per_page,
    )
}

/// Whether `s` is 64 lowercase hex characters — the one shape that becomes a
/// path component here.
fn is_sha256(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Owner-only permissions on a sidecar (unix): it is a re-encoding of a file
/// in the checkout, and it inherits that file's audience rather than the
/// umask's.
///
/// Both callers — the sidecar a conversion writes and the marker a window
/// creates — belong to the document seam.
#[cfg(feature = "doc")]
fn restrict(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    let _ = path;
}

// ------------------------------------------------------------ conversion ---

/// What became of one document.
#[cfg(feature = "doc")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Conversion {
    /// The family already held the answer; this member now has a view of it.
    Linked(PathBuf),
    /// Converted here, and linked into this member.
    Converted(PathBuf),
    /// Decided against, once and for the whole family: the reason is a
    /// property of the document, so nothing will convert it later either.
    Refused(&'static str),
    /// Not now: no helper, a deadline, a fault that says nothing about the
    /// document. The next pass tries again.
    Deferred(&'static str),
}

/// The engine classifications that are facts about the **document** rather
/// than about the run, and therefore worth remembering.
///
/// The rest — a helper that is not installed, a deadline, an unwinding parser,
/// a machine out of memory — say only that this attempt failed, and a marker
/// written for one of those would refuse a perfectly convertible document for
/// as long as the family kept it.
#[cfg(feature = "doc")]
const DOCUMENT_FAULTS: [&str; 5] = [
    "encrypted",
    "malformed",
    "missingPart",
    "needsOcr",
    "unsupported",
];

/// The caps `doc` puts on a conversion.
#[cfg(feature = "doc")]
pub(crate) fn caps(doc: &DocPolicy) -> rift_recall_doc::ConvertCaps {
    rift_recall_doc::ConvertCaps {
        table_rows: usize::try_from(doc.table_rows).unwrap_or(rift_recall_doc::DEFAULT_TABLE_ROWS),
        max_input_bytes: doc.max_input_bytes,
        pdf_min_chars_per_page: doc.pdf_min_chars_per_page,
    }
}

/// Convert the document at `path` into the family store, or say why not.
///
/// `expected_sha256` is the identity a caller recorded earlier (a journaled
/// job names the bytes it meant); a file that has moved on since is left
/// alone rather than converted under a stale name.
///
/// Never converts what the family already holds, never converts what the
/// family has already decided against, and never reads a file larger than the
/// policy's cap. Every failure is a value: this runs in the detached worker,
/// where the honest outcomes are "there is an answer now" and "there is not".
#[cfg(feature = "doc")]
pub(crate) fn convert_into_store(
    store: &DocStore,
    converter: &dyn rift_recall_doc::DocConverter,
    path: &Path,
    doc: &DocPolicy,
    expected_sha256: Option<&str>,
) -> Conversion {
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
        return Conversion::Refused("no-extension");
    };
    if !rift_recall_doc::is_supported_extension(ext) {
        return Conversion::Refused("unsupported-extension");
    }
    // The size is read before the bytes are: a document over the cap is never
    // converted, and refusing it must not cost reading it.
    let Ok(meta) = std::fs::metadata(path) else {
        return Conversion::Deferred("unreadable");
    };
    if meta.len() > doc.max_input_bytes {
        return Conversion::Refused("too-large");
    }
    let Ok(bytes) = std::fs::read(path) else {
        return Conversion::Deferred("unreadable");
    };
    let sha = rift_shadow::sha256_hex(&[&bytes]);
    if expected_sha256.is_some_and(|expected| expected != sha) {
        return Conversion::Deferred("moved-on");
    }
    let Some(key) = sidecar_key(&sha, doc) else {
        return Conversion::Deferred("unreadable");
    };
    if store.holds(&key) {
        return match store.link(&key) {
            Ok(member) => Conversion::Linked(member),
            Err(_) => Conversion::Deferred("link"),
        };
    }
    if store.is_skipped(&key) {
        return Conversion::Refused("already-decided");
    }
    let caps = caps(doc);
    match converter.convert(&bytes, ext, &caps) {
        Ok(markdown) if markdown.is_thin(&caps) => {
            // A page image is not text, and Markdown that says so is worse
            // than the page the agent asked for. Recorded so the family stops
            // paying a parse to learn it again.
            store.mark_skipped(&key, "thin");
            Conversion::Refused("thin")
        }
        Ok(markdown) => {
            let basename = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("(unnamed)");
            let text = rift_recall_doc::sidecar_text(&markdown, basename, &sha);
            match store.put(&key, &text) {
                Ok(member) => Conversion::Converted(member),
                Err(_) => Conversion::Deferred("write"),
            }
        }
        Err(rift_recall_doc::DocError::Engine { code }) if DOCUMENT_FAULTS.contains(&code) => {
            store.mark_skipped(&key, code);
            Conversion::Refused(code)
        }
        Err(rift_recall_doc::DocError::Engine { code }) => Conversion::Deferred(code),
        Err(rift_recall_doc::DocError::TooLarge { .. }) => Conversion::Refused("too-large"),
        Err(rift_recall_doc::DocError::UnsupportedExtension(_)) => {
            Conversion::Refused("unsupported-extension")
        }
    }
}

// ------------------------------------------------------- the proactive pass ---

/// Documents examined in one proactive pass.
///
/// A pass runs in the detached worker, where the session that asked for it has
/// already been answered, so the bound is not a latency budget: it is the
/// promise that a pass is a bounded amount of work on a machine somebody is
/// using, whatever the repository turns out to hold.
#[cfg(feature = "doc")]
pub(crate) const SWEEP_LIMIT: usize = 200;

/// What one pass did, for the log line that is the only thing watching it.
#[cfg(feature = "doc")]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct PassReport {
    /// Documents whose bytes were read and keyed.
    pub(crate) examined: usize,
    /// Documents converted here.
    pub(crate) converted: usize,
    /// Documents the family already held, linked into this member.
    pub(crate) linked: usize,
    /// Documents decided against, now or earlier.
    pub(crate) refused: usize,
    /// Documents whose conversion is still owed.
    pub(crate) deferred: usize,
}

/// The documents this work tree tracks, in git's order.
///
/// **Tracked**, not "found by walking": the repository's index is already the
/// statement of what belongs to this project, so a build directory full of
/// generated PDFs, an ignored vendor tree and a stray download are outside the
/// pass by construction rather than by a list of exclusions somebody has to
/// maintain. It also costs one `git` child process instead of a directory
/// walk, and it is the member's own index, so each member sweeps what it has
/// checked out.
///
/// The pathspecs are the converter's extension table, so a format Rift does
/// not convert is never even listed.
#[cfg(feature = "doc")]
fn tracked_documents(repo: &rift_git::RepoPaths) -> Vec<String> {
    let mut args = vec!["ls-files".to_owned(), "-z".to_owned(), "--".to_owned()];
    args.extend(
        rift_recall_doc::SUPPORTED_EXTENSIONS
            .iter()
            .map(|ext| format!("*.{ext}")),
    );
    let Ok((true, out)) = rift_git::run_git_status(repo, &args, &[], &[]) else {
        return Vec::new();
    };
    String::from_utf8_lossy(&out)
        .split('\0')
        .filter(|path| !path.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

/// Convert this work tree's tracked documents into the family store.
#[cfg(feature = "doc")]
pub(crate) fn convert_tracked_documents(
    repo: &rift_git::RepoPaths,
    store: &DocStore,
    converter: &dyn rift_recall_doc::DocConverter,
    config: &rift_config::EffectiveConfig,
    policy: &rift_recall_policy::ServePolicy,
    limit: usize,
) -> PassReport {
    convert_documents(
        &tracked_documents(repo),
        &repo.work_tree,
        store,
        converter,
        config,
        policy,
        limit,
    )
}

/// The pass itself, over a list of repo-relative paths.
///
/// Stops after `limit` documents have been examined, in the order it was
/// given. A repository with more documents than that converts a prefix per
/// pass and reaches the rest through the on-demand job — a pass that walked
/// everything would be an unbounded read of the whole repository every time a
/// session started.
///
/// The two refusals are the repository's own and are made before a byte is
/// read: a path it declared sensitive, and a path its serving policy never
/// serves. Neither counts against the limit — refusing a name is not work.
#[cfg(feature = "doc")]
fn convert_documents(
    paths: &[String],
    work_tree: &Path,
    store: &DocStore,
    converter: &dyn rift_recall_doc::DocConverter,
    config: &rift_config::EffectiveConfig,
    policy: &rift_recall_policy::ServePolicy,
    limit: usize,
) -> PassReport {
    let mut report = PassReport::default();
    for rel in paths {
        if report.examined >= limit {
            break;
        }
        if !is_convertible_path(config, policy, rel) {
            continue;
        }
        let Some(path) = document_path(work_tree, rel) else {
            continue;
        };
        report.examined += 1;
        match convert_into_store(store, converter, &path, &policy.doc, None) {
            Conversion::Converted(_) => report.converted += 1,
            Conversion::Linked(_) => report.linked += 1,
            Conversion::Refused(_) => report.refused += 1,
            Conversion::Deferred(_) => report.deferred += 1,
        }
    }
    report
}

/// The absolute path of a repo-relative document, or `None` when the name does
/// not stay inside the checkout.
///
/// A path can arrive from an agent's tool call by way of a journaled job, so
/// it is normalized through the same lexical rule the serving policy uses —
/// `..` resolved against earlier segments, an escape refused — before it is
/// joined to anything.
#[cfg(feature = "doc")]
pub(crate) fn document_path(work_tree: &Path, rel_path: &str) -> Option<PathBuf> {
    let rel = rift_recall_policy::normalize_rel(rel_path)?;
    (!rel.is_empty()).then(|| work_tree.join(rel))
}

/// Whether a document may be converted at all.
///
/// Two refusals, both the repository's own: a path it declared sensitive, and
/// a path its serving policy never serves. A sidecar is durable text derived
/// from the file, so a file that must not be served must not be re-encoded
/// into a second copy beside the checkout either.
#[cfg(feature = "doc")]
pub(crate) fn is_convertible_path(
    config: &rift_config::EffectiveConfig,
    policy: &rift_recall_policy::ServePolicy,
    rel_path: &str,
) -> bool {
    !config.is_sensitive_path(rel_path)
        && !policy
            .denied_paths
            .iter()
            .any(|glob| rift_config::glob_match(glob, rel_path))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHA_A: &str = "aa11bb22cc33dd44ee55ff66007788990011223344556677889900aabbccddee";
    const SHA_B: &str = "bb11bb22cc33dd44ee55ff66007788990011223344556677889900aabbccddee";

    /// A family with two members, built out of the same files `git init` and
    /// `git worktree add` write, so the store under test is keyed by a real
    /// common directory and the member views are real checkouts.
    ///
    /// Every test that needs one is a test about a conversion, so the fixture
    /// belongs to the same build the conversions do.
    #[cfg(feature = "doc")]
    struct Fixture {
        _tmp: tempfile::TempDir,
        common_dir: PathBuf,
        main: PathBuf,
        linked: PathBuf,
    }

    #[cfg(feature = "doc")]
    fn family() -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let main = tmp.path().join("main");
        std::fs::create_dir_all(&main).unwrap();
        let git_dir = rift_git::test_support::fake_repo(&main).unwrap();
        let linked = tmp.path().join("linked");
        rift_git::test_support::fake_worktree(&git_dir, &linked, "linked").unwrap();
        Fixture {
            _tmp: tmp,
            common_dir: git_dir,
            main,
            linked,
        }
    }

    #[cfg(feature = "doc")]
    fn store_for(f: &Fixture, member: &Path) -> DocStore {
        DocStore::for_member(&f.common_dir, member)
    }

    /// The key is the identity of the ANSWER: the document, the engine that
    /// read it, and the caps it read it under. With two of the three fixed, a
    /// change in the third is a different sidecar — which is what makes a
    /// stale one unreachable rather than wrong.
    #[test]
    fn a_key_changes_with_the_document_the_engine_or_a_cap_that_changes_the_bytes() {
        let doc = DocPolicy::default();
        let base = sidecar_key(SHA_A, &doc).unwrap();
        assert!(is_sha256(&base), "the key is a path-safe name: {base}");
        assert_ne!(base, sidecar_key(SHA_B, &doc).unwrap(), "another document");

        let capped = DocPolicy {
            table_rows: 20,
            ..doc
        };
        assert_ne!(base, sidecar_key(SHA_A, &capped).unwrap(), "another cap");

        // The levers that decide WHETHER to convert, not WHAT a conversion
        // says, leave every sidecar in the family reachable.
        let observing = DocPolicy {
            mode: rift_recall_policy::Mode::Observe,
            channel: rift_recall_policy::DocChannel::Redirect,
            convert_budget_ms: 1,
            ttl_days: 1,
            ..doc
        };
        assert_eq!(base, sidecar_key(SHA_A, &observing).unwrap());
    }

    /// A name is never built out of a string this program has not checked.
    #[test]
    fn a_hash_that_is_not_one_is_not_a_key() {
        let doc = DocPolicy::default();
        for candidate in ["", "../evil", &SHA_A.to_uppercase(), &SHA_A[..63]] {
            assert!(
                sidecar_key(candidate, &doc).is_none(),
                "`{candidate}` must not become a file name"
            );
        }
    }

    /// ★ The load-bearing property of the store: a document converted while
    /// working in one member of the family is **linked** into the next member
    /// that wants it, never converted again — and the two members are looking
    /// at one file rather than two copies that can drift.
    #[cfg(feature = "doc")]
    #[test]
    fn a_sidecar_converted_in_one_member_is_hardlinked_not_reconverted_in_another() {
        let f = family();
        let doc = DocPolicy::default();
        let key = sidecar_key(SHA_A, &doc).unwrap();
        let main = store_for(&f, &f.main);
        let linked = store_for(&f, &f.linked);

        // Neither member holds it, so neither can hand it over: a lookup is a
        // question, never a conversion.
        assert!(!main.holds(&key));
        assert!(!linked.member_path(&key).exists());

        let converted = "<!-- converted by rift -->\n\n# Spec\n";
        let in_main = main.put(&key, converted).unwrap();
        assert!(in_main.starts_with(&f.main), "{}", in_main.display());

        // The second member finds the answer already made and takes a view of
        // it. Nothing was converted here: the store was never asked to.
        assert!(
            linked.holds(&key),
            "one store, asked from the other checkout"
        );
        let in_linked = linked.link(&key).unwrap();
        assert!(in_linked.starts_with(&f.linked));
        assert_eq!(std::fs::read_to_string(&in_linked).unwrap(), converted);

        // One file, three names: the family's, and both members' views.
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            let ino = |p: &Path| std::fs::metadata(p).unwrap().ino();
            let family_copy = main.family_path(&key);
            assert_eq!(ino(&family_copy), ino(&in_main));
            assert_eq!(
                ino(&family_copy),
                ino(&in_linked),
                "a member's view is a hardlink to the family's one copy"
            );
            assert_eq!(std::fs::metadata(&family_copy).unwrap().nlink(), 3);
        }

        // And the family store is one directory under the common dir both
        // members share — not one per checkout.
        assert_eq!(main.family_path(&key), linked.family_path(&key));
        assert!(main.family_path(&key).starts_with(&f.common_dir));
    }

    /// A temporary worktree comes and goes; what it converted stays with the
    /// family. Removing the member removes only its view — the family's one
    /// copy is untouched and the next member still finds it.
    #[cfg(feature = "doc")]
    #[test]
    fn a_removed_worktree_leaves_the_family_store_intact() {
        let f = family();
        let doc = DocPolicy::default();
        let key = sidecar_key(SHA_A, &doc).unwrap();
        let linked = store_for(&f, &f.linked);
        let converted = "<!-- converted by rift -->\n\n# Spec\n";
        linked.put(&key, converted).unwrap();
        let family_copy = linked.family_path(&key);
        assert!(family_copy.exists());

        std::fs::remove_dir_all(&f.linked).unwrap();
        assert!(!f.linked.exists());

        assert!(
            family_copy.exists(),
            "the family's copy outlives the member"
        );
        assert_eq!(std::fs::read_to_string(&family_copy).unwrap(), converted);
        let main = store_for(&f, &f.main);
        assert!(main.holds(&key), "another member still finds the answer");
        let in_main = main.link(&key).unwrap();
        assert_eq!(std::fs::read_to_string(in_main).unwrap(), converted);
    }

    /// A document that must not be converted again says so once, in the
    /// family, so every member of the family stops asking.
    #[cfg(feature = "doc")]
    #[test]
    fn a_decision_against_a_document_is_recorded_for_the_whole_family() {
        let f = family();
        let key = sidecar_key(SHA_A, &DocPolicy::default()).unwrap();
        let main = store_for(&f, &f.main);
        assert!(!main.is_skipped(&key));
        main.mark_skipped(&key, "thin");
        assert!(store_for(&f, &f.linked).is_skipped(&key));
        assert!(!main.holds(&key), "a marker is not an answer");
    }

    /// `rift clean`'s two document legs, at the level they are decided: a
    /// sidecar nobody has used since the TTL goes, and a member view with
    /// nothing behind it goes whatever its age — an expired answer must not
    /// leave a link an agent can still be pointed at.
    #[cfg(feature = "doc")]
    #[test]
    fn clean_removes_expired_sidecars_and_orphan_links() {
        // Far enough ahead of any file's real modification time that the
        // sweep's cutoff, and not a clock this test controls, decides what is
        // old: the sweep takes `now` from its caller precisely so a test never
        // has to wait or backdate.
        const LATER: i64 = 4_102_444_800_000;
        let f = family();
        let doc = DocPolicy::default();
        let store = store_for(&f, &f.main);
        let fresh = sidecar_key(SHA_A, &doc).unwrap();
        let stale = sidecar_key(SHA_B, &doc).unwrap();
        store.put(&fresh, "# fresh\n").unwrap();
        store.put(&stale, "# stale\n").unwrap();
        store.mark_skipped(&stale, "thin");
        // A view whose family answer somebody removed by hand, a temp file
        // from a write that died before its rename, and a file this store did
        // not write at all.
        let orphan =
            store.member_path("cc11bb22cc33dd44ee55ff66007788990011223344556677889900aabbccddee");
        std::fs::write(&orphan, "# orphan\n").unwrap();
        let tmp = store.family_path(&fresh).with_extension("md.tmp999");
        std::fs::write(&tmp, "half a conversion").unwrap();
        let stranger = store.family_path(&fresh).with_file_name("notes.md");
        std::fs::write(&stranger, "someone else's file").unwrap();

        assert_eq!(
            store.survey(),
            Survey {
                sidecars: 2,
                bytes: 16,
                member_links: 3,
                orphan_links: 1,
            },
            "the doctor's four numbers, before anything is collected"
        );

        let dry = store.sweep(LATER, 0, true);
        assert_eq!(
            (dry.expired, dry.orphan_links, dry.swept_tmps),
            (2, 1, 1),
            "a dry run counts everything and removes nothing"
        );
        assert!(store.holds(&fresh) && store.holds(&stale) && orphan.exists());

        let inside = store.sweep(LATER, i64::MAX / 2, false);
        assert_eq!(
            (inside.expired, inside.swept_tmps),
            (0, 0),
            "inside the TTL nothing expires"
        );
        assert_eq!(inside.orphan_links, 1, "an orphan has no TTL to be inside");
        assert!(!orphan.exists());
        assert!(store.holds(&fresh) && store.holds(&stale));

        // What one conversation was told about documents goes with the rest of
        // its run state: the window's markers are only ever created, so the
        // directory's own time is the last thing that window was told.
        let window = DocWindow::at(&f.main, "s-1", None);
        window.arm_rewrite("docs/plan.docx", SHA_B, SHA_A);
        assert!(window.flip_to_redirect());
        assert!(window.is_redirecting());
        assert_eq!(store.sweep(LATER, i64::MAX / 2, false).swept_windows, 0);
        assert!(
            window.is_redirecting(),
            "inside the TTL nothing is forgotten"
        );
        assert_eq!(
            store.sweep(LATER, 0, true).swept_windows,
            1,
            "a dry run counts it"
        );
        assert!(window.is_redirecting(), "and removes nothing");

        let past = store.sweep(LATER, 0, false);
        assert_eq!(past.swept_windows, 1, "an ended conversation is collected");
        assert!(!window.is_redirecting());
        assert_eq!(
            window.take_armed("docs/plan.docx"),
            None,
            "and so is what it was told"
        );
        assert_eq!((past.expired, past.swept_tmps), (2, 1));
        assert!(!store.holds(&fresh) && !store.holds(&stale));
        assert!(!tmp.exists());
        assert!(
            !store.member_path(&fresh).exists(),
            "a member's view goes with the answer behind it"
        );
        assert!(
            !store.is_skipped(&stale),
            "and so does a decision about a document nobody keeps"
        );
        assert!(stranger.exists(), "a file this store did not write is left");
        assert_eq!(store.survey(), Survey::default());
    }

    /// One amendment, two names, and exactly one claim.
    ///
    /// A read's result names either the sidecar the amendment pointed at or
    /// the document the agent wrote, so the arming has to be findable from
    /// both — and taking it from either side has to take it from the other,
    /// because the two names are one amendment and a saving claimed twice for
    /// one read is a saving Rift did not make.
    #[cfg(feature = "doc")]
    #[test]
    fn one_amendment_is_answered_from_either_side_and_only_once() {
        let f = family();
        let key = sidecar_key(SHA_A, &DocPolicy::default()).unwrap();
        let window = DocWindow::at(&f.main, "s-1", None);

        // A view of this member's store is recognised as one; a window's own
        // markers, a name that is not a key, and a document are not.
        assert_eq!(
            member_sidecar_key(&format!(".rift/state/doc/{key}.md")),
            Some(key.as_str())
        );
        for other in [
            format!(".rift/state/doc/{WINDOW_DIR}/abc/{key}.md"),
            ".rift/state/doc/notes.md".to_owned(),
            format!("{key}.md"),
            format!(".rift/state/doc/{key}.txt"),
        ] {
            assert_eq!(member_sidecar_key(&other), None, "{other}");
        }

        // The verified side: the result named the sidecar, so the document it
        // stood in for and the bytes it was made about come back — once.
        window.arm_rewrite("docs/plan.docx", &key, SHA_A);
        assert_eq!(
            window.take_armed_sidecar(&key),
            Some(ArmedRewrite {
                rel_path: "docs/plan.docx".to_owned(),
                content_sha256: SHA_A.to_owned(),
            })
        );
        assert_eq!(window.take_armed_sidecar(&key), None, "claimed once");
        assert_eq!(
            window.take_armed("docs/plan.docx"),
            None,
            "and the same amendment cannot then be reported as dropped"
        );

        // The dropped side, of a second amendment: the result named the
        // document, and the sidecar this window was pointed at can no longer
        // be presented as a substitution that happened.
        window.arm_rewrite("docs/plan.docx", &key, SHA_A);
        assert_eq!(window.take_armed("docs/plan.docx"), Some(SHA_A.to_owned()));
        assert_eq!(window.take_armed_sidecar(&key), None);

        // Neither side answers about an amendment nobody made.
        assert_eq!(
            window.take_armed_sidecar(&sidecar_key(SHA_B, &DocPolicy::default()).unwrap()),
            None
        );
        assert_eq!(window.take_armed("docs/other.docx"), None);
    }

    /// What counts as a document is asked in every build, and answered from a
    /// list this binary keeps itself — the crate that owns it is optional here.
    /// Where the seam IS linked, the two lists are held equal.
    #[test]
    fn the_document_extensions_are_the_ones_the_converter_knows() {
        #[cfg(feature = "doc")]
        assert_eq!(
            DOCUMENT_EXTENSIONS.to_vec(),
            rift_recall_doc::SUPPORTED_EXTENSIONS.to_vec(),
            "the binary's own spelling of the document list drifted from the \
             crate that converts them"
        );
        assert!(is_document_extension(".DOCX") && is_document_extension("pdf"));
        assert!(!is_document_extension("rs") && !is_document_extension("md"));
    }

    /// The engine that produced a sidecar is half of its key, and a build
    /// without the seam cannot ask the engine. So the constant is bound to
    /// the workspace's pin: an upgrade that left this behind would hand every
    /// agent the old engine's answers under the new engine's name.
    #[test]
    fn the_engine_version_matches_the_workspace_pin() {
        #[cfg(feature = "doc")]
        assert_eq!(
            ENGINE_VERSION,
            rift_recall_doc::ENGINE_VERSION,
            "this binary's spelling of the engine version must equal the crate's"
        );
        let manifest =
            std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.toml"))
                .unwrap();
        let pin = format!("anydoc = \"={ENGINE_VERSION}\"");
        assert!(
            manifest.contains(&pin),
            "the workspace pins a different anydoc than ENGINE_VERSION names ({pin})"
        );
    }

    #[cfg(feature = "doc")]
    mod conversion {
        use super::*;
        use rift_recall_doc::{ConvertCaps, DocConverter, DocError, Markdown};

        /// A converter that counts, so "converted once for the family" is an
        /// observation rather than a hope.
        struct Counting {
            calls: std::cell::Cell<usize>,
            answer: std::result::Result<Markdown, DocError>,
        }

        impl Counting {
            fn ok(text: &str) -> Self {
                Self {
                    calls: std::cell::Cell::new(0),
                    answer: Ok(Markdown {
                        text: text.to_owned(),
                        ..Markdown::default()
                    }),
                }
            }
            fn err(answer: DocError) -> Self {
                Self {
                    calls: std::cell::Cell::new(0),
                    answer: Err(answer),
                }
            }
        }

        impl DocConverter for Counting {
            fn convert(
                &self,
                _bytes: &[u8],
                _ext: &str,
                _caps: &ConvertCaps,
            ) -> std::result::Result<Markdown, DocError> {
                self.calls.set(self.calls.get() + 1);
                self.answer.clone()
            }
        }

        fn document(dir: &Path, name: &str, bytes: &str) -> PathBuf {
            let path = dir.join(name);
            std::fs::write(&path, bytes).unwrap();
            path
        }

        /// ★ The same claim as the store test, end to end through the
        /// conversion path: the second member's pass links what the first
        /// member's pass converted, and the converter is not called again.
        #[test]
        fn a_document_is_converted_once_for_the_family_and_linked_everywhere_else() {
            let f = family();
            let doc = DocPolicy::default();
            let converter = Counting::ok("# Costs\n");
            let in_main = document(&f.main, "costs.xlsx", "book");
            let in_linked = document(&f.linked, "costs.xlsx", "book");

            let first =
                convert_into_store(&store_for(&f, &f.main), &converter, &in_main, &doc, None);
            let Conversion::Converted(main_side) = first else {
                panic!("the first pass converts: {first:?}");
            };
            assert_eq!(converter.calls.get(), 1);
            assert!(std::fs::read_to_string(&main_side)
                .unwrap()
                .contains("# Costs"));

            let second = convert_into_store(
                &store_for(&f, &f.linked),
                &converter,
                &in_linked,
                &doc,
                None,
            );
            let Conversion::Linked(linked_side) = second else {
                panic!("the second member links what the family already has: {second:?}");
            };
            assert_eq!(
                converter.calls.get(),
                1,
                "the same bytes are never converted twice for one family"
            );
            assert!(linked_side.starts_with(&f.linked));
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt as _;
                assert_eq!(
                    std::fs::metadata(&main_side).unwrap().ino(),
                    std::fs::metadata(&linked_side).unwrap().ino()
                );
            }
        }

        /// The cheap refusals are made before anything is parsed, and a
        /// journaled job's bytes are checked against the identity it named.
        #[test]
        fn what_is_never_converted_is_refused_before_the_parser() {
            let f = family();
            let doc = DocPolicy {
                max_input_bytes: 4,
                ..DocPolicy::default()
            };
            let converter = Counting::ok("# nope\n");
            let store = store_for(&f, &f.main);

            let source = document(&f.main, "notes.md", "plain markdown");
            assert_eq!(
                convert_into_store(&store, &converter, &source, &doc, None),
                Conversion::Refused("unsupported-extension")
            );
            let big = document(&f.main, "big.docx", "far more than four bytes");
            assert_eq!(
                convert_into_store(&store, &converter, &big, &doc, None),
                Conversion::Refused("too-large")
            );
            let small = document(&f.main, "s.docx", "ab");
            assert_eq!(
                convert_into_store(&store, &converter, &small, &doc, Some(SHA_A)),
                Conversion::Deferred("moved-on"),
                "a job names the bytes it meant; a file that moved on is left alone"
            );
            assert_eq!(converter.calls.get(), 0, "nothing was parsed");
        }

        /// A fault of the document is remembered for the family; a fault of
        /// the run is not, so the next pass tries again.
        #[test]
        fn only_a_fault_of_the_document_is_remembered() {
            let f = family();
            let doc = DocPolicy::default();
            let store = store_for(&f, &f.main);
            let sealed = document(&f.main, "sealed.docx", "locked");
            let encrypted = Counting::err(DocError::Engine { code: "encrypted" });
            assert_eq!(
                convert_into_store(&store, &encrypted, &sealed, &doc, None),
                Conversion::Refused("encrypted")
            );
            // A second pass costs nothing at all: the family has decided.
            assert_eq!(
                convert_into_store(&store, &encrypted, &sealed, &doc, None),
                Conversion::Refused("already-decided")
            );
            assert_eq!(encrypted.calls.get(), 1);

            let missing = document(&f.main, "other.docx", "fine");
            let no_helper = Counting::err(DocError::Engine {
                code: "helperMissing",
            });
            assert_eq!(
                convert_into_store(&store, &no_helper, &missing, &doc, None),
                Conversion::Deferred("helperMissing")
            );
            assert_eq!(
                convert_into_store(&store, &no_helper, &missing, &doc, None),
                Conversion::Deferred("helperMissing"),
                "a run that failed says nothing about the document"
            );
            assert_eq!(no_helper.calls.get(), 2);
        }

        /// A scanned page is not text: the conversion is thin, nothing is
        /// stored to substitute for the page the agent asked for, and the
        /// family remembers so it does not pay the parse again.
        #[test]
        fn a_scanned_document_is_decided_against_rather_than_stored() {
            let f = family();
            let doc = DocPolicy::default();
            let store = store_for(&f, &f.main);
            let scanned = document(&f.main, "scan.pdf", "%PDF");
            let converter = Counting {
                calls: std::cell::Cell::new(0),
                answer: Ok(Markdown {
                    text: "  \n".to_owned(),
                    pages: Some(4),
                    thin_pages: vec![1, 2, 3, 4],
                    ..Markdown::default()
                }),
            };
            assert_eq!(
                convert_into_store(&store, &converter, &scanned, &doc, None),
                Conversion::Refused("thin")
            );
            let key = sidecar_key(&rift_shadow::sha256_hex(&[b"%PDF"]), &doc).unwrap();
            assert!(!store.holds(&key), "nothing thin is ever stored");
            assert!(store.is_skipped(&key));
        }

        /// The proactive pass: bounded, progressive, and refusing what the
        /// repository said it would refuse — before it reads a byte of it.
        #[test]
        fn a_pass_is_bounded_and_never_reads_what_the_repository_withholds() {
            let f = family();
            std::fs::create_dir_all(f.main.join(".rift")).unwrap();
            std::fs::write(
                f.main.join(".rift/config.toml"),
                "schema_version = 1\n\n[project]\nname = \"docs\"\n\n[paths]\n\
                 sensitive_paths = [\"board/**\"]\n",
            )
            .unwrap();
            let config = rift_config::EffectiveConfig::load(&f.main.join(".rift")).unwrap();
            let policy = rift_recall_policy::ServePolicy {
                denied_paths: vec!["private/**".to_owned()],
                ..rift_recall_policy::ServePolicy::default()
            };
            let store = store_for(&f, &f.main);
            let converter = Counting::ok("# converted\n");
            for rel in ["a.docx", "b.docx"] {
                document(&f.main, rel, rel);
            }
            for dir in ["board", "private"] {
                std::fs::create_dir_all(f.main.join(dir)).unwrap();
                document(&f.main, &format!("{dir}/secret.docx"), "confidential");
            }
            let paths: Vec<String> = [
                "a.docx",
                "b.docx",
                "board/secret.docx",
                "private/secret.docx",
            ]
            .iter()
            .map(|p| (*p).to_owned())
            .collect();

            let first = convert_documents(&paths, &f.main, &store, &converter, &config, &policy, 1);
            assert_eq!(
                first,
                PassReport {
                    examined: 1,
                    converted: 1,
                    ..PassReport::default()
                },
                "the bound is a promise about how much work one pass is"
            );

            let second =
                convert_documents(&paths, &f.main, &store, &converter, &config, &policy, 9);
            assert_eq!(
                second,
                PassReport {
                    examined: 2,
                    converted: 1,
                    linked: 1,
                    ..PassReport::default()
                },
                "what the family already holds is linked, not converted again"
            );
            assert_eq!(
                converter.calls.get(),
                2,
                "a sensitive or never-served document is refused before it is read"
            );
            let withheld =
                sidecar_key(&rift_shadow::sha256_hex(&[b"confidential"]), &policy.doc).unwrap();
            assert!(!store.holds(&withheld));
        }
    }
}
