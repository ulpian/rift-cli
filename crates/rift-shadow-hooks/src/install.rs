//! Git-hook installation (GUI-client safe, never-clobber).
//!
//! Installs the five hooks of [`HOOK_NAMES`] by **absolute** path, so a desktop
//! GUI with a sparse `PATH` still finds Rift. On Unix, an unoccupied hook is a
//! direct symlink to the binary: this avoids paying a fresh shell-script launch
//! on every commit. A hook that must preserve a pre-existing user hook remains a
//! tiny POSIX-`sh` shim which runs that hook first. Managed-link sidecars let a
//! later install distinguish Rift's link from a user's without following it.
//!
//! These functions only write files; the never-block discipline (a panicking
//! capture must still let the commit succeed) lives at the hook *entrypoint* in
//! the bin, which the shim execs.
//!
//! # The hooks are the family's, and so is the binary they point at
//!
//! Git resolves `hooks/<name>` through the **common dir**, so one install
//! covers every worktree of a repository — which also means *any* member's
//! `rift init` re-pointed *every* member's hooks at whatever binary that
//! process happened to be. On this machine that side effect pointed all six
//! hooks at a deleted `/tmp/rift-measure-run.*` on 2026-08-26 21:27; git skips a
//! dangling hook **silently**, and capture stopped for four days without one
//! error line.
//!
//! Two rules close it, and both live here:
//!
//! * [`HookInstallPolicy::KeepIfHealthy`] — the default for every install that
//!   is a *side effect* of something else (`rift init`, Project Settings,
//!   adoption). A healthy family keeps its hooks and its recorded binary; only
//!   a dangling link or a vanished binary re-points anything, and that repair
//!   is reported, never silent.
//! * The **binary of record** ([`BinaryOfRecord`], `<family_dir>/binary.json`):
//!   which binary this family's hooks point at, when it was recorded, and by
//!   what act — with the superseded records kept behind it. "Whichever process
//!   ran last" becomes a fact with a history.
//!
//! A binary under a temporary root, a `deps/` or `build/` test binary, or a binary
//! inside a **linked member's** work tree is [transient](is_transient): the
//! family may not be pointed at it without `--allow-transient`, because all
//! three vanish while the family lives on.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use rift_core::{Result, RiftError};
use serde::{Deserialize, Serialize};

/// Marker line identifying a shim Rift owns (so re-install overwrites our shim but
/// never a user's hook).
const MARKER: &str = "# Rift-managed hook";
/// Suffix for a chained-aside user hook.
const CHAINED_SUFFIX: &str = ".rift-chained";
/// Sidecar that records the exact target of a Rift-managed direct hook link.
///
/// Written only where links are (Unix), and read everywhere: a sidecar is never
/// a hook entry, and a host that could not name it would sweep one.
const LINK_MARKER_SUFFIX: &str = ".rift-managed-link";

/// The git hooks Rift installs: the four capture hooks, and `post-checkout`.
///
/// The list is the whole dispatch table. A direct Unix link is routed by its
/// `argv[0]` against these names (`bins/rift/src/hooks/git.rs`), and a Windows
/// shim is written per name, so an entry here **is** an installed, routed hook
/// and nothing else changes — which is also why a name with no capture body
/// does not belong in it. Growing it has one deliberate consequence: a family
/// installed while the list was shorter is missing a hook, the health probe
/// behind [`HookInstallPolicy::KeepIfHealthy`] reports it unhealthy, and the
/// next such install writes all of them and records the repair.
pub const HOOK_NAMES: &[&str] = &[
    "prepare-commit-msg",
    "commit-msg",
    "post-commit",
    "pre-push",
    // The only moment git tells Rift a worktree was born, and it tells it from
    // inside the new tree.
    "post-checkout",
];

/// Where + how to install. `rift_bin` must be an **absolute** path.
#[derive(Clone, Debug)]
pub struct HookInstall {
    /// The repository's git dir (passed to each shim as `--git-dir`).
    pub git_dir: PathBuf,
    /// The absolute path to the `rift` binary the shim execs.
    pub rift_bin: PathBuf,
    /// The hooks directory **git itself will run** — resolved by the caller
    /// through `git rev-parse --git-path hooks`, so `core.hooksPath` and
    /// `extensions.worktreeConfig` cannot leave `<git_dir>/hooks` reading
    /// healthy while git runs somewhere else. `None` falls back to
    /// `<git_dir>/hooks`, which is only right when git says so.
    pub hooks_dir: Option<PathBuf>,
    /// The family these hooks belong to: where its `binary.json` lives, and
    /// which checkouts a binary may not be reached through.
    pub family: FamilySites,
    /// Which act is installing — the `recorded_by` of the **first** record this
    /// family gets. A repair records [`RecordedBy::SelfHeal`] and an explicit
    /// replacement [`RecordedBy::Repoint`] regardless of what a caller says.
    pub recorded_by: RecordedBy,
}

impl HookInstall {
    /// The resolved hooks directory.
    #[must_use]
    pub fn resolved_hooks_dir(&self) -> PathBuf {
        self.hooks_dir
            .clone()
            .unwrap_or_else(|| self.git_dir.join("hooks"))
    }
}

/// The family a hook install belongs to, as this module must see it.
///
/// `rift-shadow-hooks` cannot name `rift_git::Family` — it does not depend on
/// that crate and the workspace graph stays acyclic — so the caller resolves
/// the family and hands over the two facts an install needs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FamilySites {
    /// The family's manifest directory, `<common_dir>/rift-family` (or the
    /// `RIFT_FAMILY_DIR` override's), where `binary.json` sits beside
    /// `family.json`.
    pub dir: PathBuf,
    /// Every **linked** member's work tree, canonical. The main work tree is
    /// deliberately absent: its `target/` holds the developer's own build of
    /// Rift and is allowed,
    /// while a member is removable and takes any binary inside it away.
    pub linked_work_trees: Vec<PathBuf>,
}

impl FamilySites {
    /// A family with no linked members — the whole repository is its main work
    /// tree.
    #[must_use]
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            linked_work_trees: Vec::new(),
        }
    }
}

/// What an install is allowed to do to a family that already has hooks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HookInstallPolicy {
    /// Leave a healthy family alone and report the binary of record; re-point
    /// only what is broken. Every install that is a side effect of another
    /// command uses this.
    KeepIfHealthy {
        /// Point the repair at a [transient](is_transient) binary anyway.
        allow_transient: bool,
    },
    /// Replace every managed hook with `cfg.rift_bin`, healthy or not. Only
    /// `rift hooks install --repoint` asks for this.
    Repoint {
        /// Point the family at a [transient](is_transient) binary anyway.
        allow_transient: bool,
    },
}

impl HookInstallPolicy {
    /// Whether this policy waives the transient screen.
    #[must_use]
    pub const fn allows_transient(self) -> bool {
        match self {
            Self::KeepIfHealthy { allow_transient } | Self::Repoint { allow_transient } => {
                allow_transient
            }
        }
    }
}

impl Default for HookInstallPolicy {
    fn default() -> Self {
        Self::KeepIfHealthy {
            allow_transient: false,
        }
    }
}

/// What one install did, hook by hook.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HookInstallReport {
    /// The hooks directory git will actually run.
    pub hooks_dir: PathBuf,
    /// The family's binary of record after this call. `None` only when there is
    /// none to report: a refusal, or a family whose hooks are shims and which
    /// has never recorded one.
    pub binary: Option<PathBuf>,
    /// Hooks left exactly as they were, because the family is healthy.
    pub kept: Vec<PathBuf>,
    /// Hooks written because they were missing or dangling.
    pub healed: Vec<PathBuf>,
    /// Hooks written because an explicit [`HookInstallPolicy::Repoint`] said to.
    pub repointed: Vec<PathBuf>,
    /// Entries removed because Rift wrote them under a name it no longer
    /// installs. Swept under every policy, a refusal included: the name left
    /// the dispatch table, so git may not go on running the entry.
    pub removed: Vec<PathBuf>,
    /// The screen refused the binary; **no hook was written**, and the live
    /// hooks are exactly as they were found.
    pub refused: Option<RefusedBinary>,
}

impl HookInstallReport {
    /// The hooks this call wrote (empty when it kept or refused).
    #[must_use]
    pub fn written(&self) -> &[PathBuf] {
        if self.repointed.is_empty() {
            &self.healed
        } else {
            &self.repointed
        }
    }
}

/// Why a binary may not become a family's binary of record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransientReason {
    /// Under a temporary root — `$TMPDIR`, `/tmp`, `/private/tmp`,
    /// `/var/folders`, `/private/var/folders`.
    TempRoot,
    /// A `debug/deps/` or `debug/build/` (likewise `release/`) build
    /// artifact: a test or bench binary reached through `current_exe()`,
    /// rebuilt under a new hash on the next `cargo test`.
    BuildArtifact,
    /// Inside a **linked member's** work tree, which `git worktree remove`
    /// deletes while the family lives on.
    LinkedMember,
}

impl TransientReason {
    /// The wire spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::TempRoot => "temp-root",
            Self::BuildArtifact => "build-artifact",
            Self::LinkedMember => "linked-member",
        }
    }

    /// Why a person should care, in one clause.
    #[must_use]
    pub const fn reason(self) -> &'static str {
        match self {
            Self::TempRoot => "a temporary root the system empties",
            Self::BuildArtifact => "a build artifact the next `cargo test` replaces",
            Self::LinkedMember => "inside a linked worktree, which `git worktree remove` deletes",
        }
    }
}

impl core::fmt::Display for TransientReason {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A binary the transient screen refused, and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RefusedBinary {
    /// The path as the caller offered it.
    pub path: PathBuf,
    /// Which rule refused it.
    pub reason: TransientReason,
}

impl core::fmt::Display for RefusedBinary {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "{} is {} ({}); the hooks were left alone — pass --allow-transient to point at it anyway",
            self.path.display(),
            self.reason,
            self.reason.reason()
        )
    }
}

// --- the binary of record ---------------------------------------------------

/// `binary.json`, beside the family manifest.
const BINARY_FILE: &str = "binary.json";

/// How many superseded records `binary.json` keeps. Old enough to answer "what
/// was it pointing at before?" through a week of builds, small enough that the
/// file a hook may one day read stays one page.
const HISTORY_CAP: usize = 16;

/// The record's `schema_version`. A file carrying any other value is not read.
const BINARY_SCHEMA_VERSION: u32 = 1;

/// Which act recorded a binary of record.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RecordedBy {
    /// `rift init` (and Project Settings, which is the same provisioning act
    /// through a different surface).
    Init,
    /// An explicit `rift hooks install`.
    HooksInstall,
    /// A [`HookInstallPolicy::KeepIfHealthy`] install that found a dangling
    /// link or a vanished binary and repaired it.
    SelfHeal,
    /// An explicit `rift hooks install --repoint`.
    Repoint,
}

impl RecordedBy {
    /// The wire spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Init => "init",
            Self::HooksInstall => "hooks-install",
            Self::SelfHeal => "self-heal",
            Self::Repoint => "repoint",
        }
    }
}

impl core::fmt::Display for RecordedBy {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One superseded binary of record.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BinaryRecord {
    /// The absolute path that was of record.
    pub rift_bin: String,
    /// When it was recorded (epoch milliseconds). Audit context only — nothing
    /// reads it back, exactly like `family.history.jsonl`'s `at_ms`.
    pub recorded_at_ms: i64,
    /// The act that recorded it.
    pub recorded_by: RecordedBy,
}

/// The family's binary of record: `<family_dir>/binary.json`.
///
/// Machine-local, never tracked, never synced — it names a path on **this**
/// machine. Written with tmp+rename so a reader sees either the old record or
/// the new one, and never half of either.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BinaryOfRecord {
    /// The record's schema version — **1**, and a file carrying any other
    /// value is not read.
    pub schema_version: u32,
    /// The absolute path the family's hooks point at.
    pub rift_bin: String,
    /// When it was recorded (epoch milliseconds).
    pub recorded_at_ms: i64,
    /// The act that recorded it.
    pub recorded_by: RecordedBy,
    /// Previous records, oldest first; the oldest are dropped past 16.
    #[serde(default)]
    pub history: Vec<BinaryRecord>,
}

impl BinaryOfRecord {
    /// The recorded path.
    #[must_use]
    pub fn path(&self) -> &Path {
        Path::new(&self.rift_bin)
    }

    /// Whether the recorded binary is still an executable file.
    #[must_use]
    pub fn is_live(&self) -> bool {
        is_executable_file(self.path())
    }

    fn superseded(&self) -> BinaryRecord {
        BinaryRecord {
            rift_bin: self.rift_bin.clone(),
            recorded_at_ms: self.recorded_at_ms,
            recorded_by: self.recorded_by,
        }
    }
}

/// Read a family's binary of record.
///
/// `None` covers every "there is nothing to honour here": no file, an
/// unreadable one, a shape this version does not know. A garbled record is not
/// an install failure — the hooks matter more than the note about them — so the
/// next record replaces it, and `rift doctor`'s `binary-of-record` leg is where
/// the file is named.
#[must_use]
pub fn read_binary_of_record(family_dir: &Path) -> Option<BinaryOfRecord> {
    let bytes = std::fs::read(family_dir.join(BINARY_FILE)).ok()?;
    let record: BinaryOfRecord = serde_json::from_slice(&bytes).ok()?;
    (record.schema_version == BINARY_SCHEMA_VERSION).then_some(record)
}

/// The path `binary.json` is read from and written to.
#[must_use]
pub fn binary_of_record_path(family_dir: &Path) -> PathBuf {
    family_dir.join(BINARY_FILE)
}

/// Record `rift_bin` as the family's binary of record, pushing the record it
/// supersedes onto the history.
///
/// Idempotent by construction: recording the path that is already of record
/// changes nothing and grows no history, so a `rift init` run twice leaves one
/// record, not two.
///
/// # Errors
/// [`RiftError::Io`] on a filesystem failure, [`RiftError::Validity`] if the
/// record cannot be serialized.
pub fn record_binary_of_record(
    family_dir: &Path,
    rift_bin: &Path,
    recorded_by: RecordedBy,
) -> Result<BinaryOfRecord> {
    let previous = read_binary_of_record(family_dir);
    let rift_bin = rift_bin.to_string_lossy().into_owned();
    if let Some(current) = previous.as_ref().filter(|r| r.rift_bin == rift_bin) {
        return Ok(current.clone());
    }
    let mut history = previous
        .as_ref()
        .map(|r| r.history.clone())
        .unwrap_or_default();
    if let Some(current) = previous.as_ref() {
        history.push(current.superseded());
    }
    // Oldest first, oldest dropped: the cap bounds the file, and what it drops
    // is the record furthest from anything anyone is still debugging.
    if history.len() > HISTORY_CAP {
        history.drain(..history.len() - HISTORY_CAP);
    }
    let record = BinaryOfRecord {
        schema_version: BINARY_SCHEMA_VERSION,
        rift_bin,
        recorded_at_ms: rift_core::Timestamp::now().as_millis(),
        recorded_by,
        history,
    };
    let bytes = serde_json::to_vec_pretty(&record)
        .map_err(|e| RiftError::validity(format!("serialize binary of record: {e}")))?;
    std::fs::create_dir_all(family_dir)?;
    write_atomic(&binary_of_record_path(family_dir), &bytes)?;
    Ok(record)
}

/// `binary.json.tmp-<pid>-<n>`, in the destination directory so the rename is
/// atomic, with a per-process counter so two threads cannot share a name.
fn tmp_name() -> String {
    use core::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    format!(
        "{BINARY_FILE}.tmp-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

/// Write, fsync, rename — so a concurrent reader sees one whole record.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let Some(dir) = path.parent() else {
        return Err(RiftError::validity("binary of record has no directory"));
    };
    let tmp = dir.join(tmp_name());
    let write = (|| -> std::io::Result<()> {
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()
    })();
    if let Err(e) = write {
        let _ = std::fs::remove_file(&tmp);
        return Err(RiftError::from(e));
    }
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(RiftError::from(e));
    }
    Ok(())
}

// --- the transient screen ---------------------------------------------------

/// The temporary roots a family's binary may not live under.
///
/// `/var/folders` **and** `/private/var/folders` are both listed on purpose:
/// macOS's `$TMPDIR` is reached through a `/var` symlink, so the same file has
/// two spellings and only one of them survives canonicalization. The screen
/// runs against both the path as offered and its canonical form, so a binary
/// that exists is caught by its real location and one that does not is still
/// caught by its name.
fn temp_roots() -> Vec<PathBuf> {
    let mut roots = vec![
        PathBuf::from("/tmp"),
        PathBuf::from("/private/tmp"),
        PathBuf::from("/var/folders"),
        PathBuf::from("/private/var/folders"),
    ];
    let tmp = std::env::temp_dir();
    if let Ok(canonical) = tmp.canonicalize() {
        roots.push(canonical);
    }
    roots.push(tmp);
    roots
}

/// Whether `path` sits under a `<profile>/deps/` or `<profile>/build/`
/// directory.
///
/// The `target` component is deliberately not required: `CARGO_TARGET_DIR`
/// renames it, and what makes the path transient is the directory itself,
/// whose contents carry a content hash that changes on every rebuild.
/// `deps/` is where cargo has always put test binaries; `build/` was always
/// build-script territory and is ALSO where the 1.100-era build-dir layout
/// runs test binaries from (`<profile>/build/<name>/<hash>/out/<name>-<hash>`)
/// — found live on 2026-09-01, when a doctor --fix under the coverage harness
/// pointed a sandbox family at exactly that shape and
/// `a_doctor_fix_refuses_to_point_the_family_at_a_transient_binary` went red.
fn is_build_artifact(path: &Path) -> bool {
    let names: Vec<&std::ffi::OsStr> = path.iter().collect();
    names.windows(2).any(|pair| {
        pair.first()
            .is_some_and(|p| *p == "debug" || *p == "release")
            && pair.get(1).is_some_and(|p| *p == "deps" || *p == "build")
    })
}

/// Whether a family may be pointed at `path` — and if not, why.
///
/// All three refusals are the same shape: a path that outlives the *decision*
/// to install by less than the family does. The screen runs over the
/// **canonicalized** path as well as the offered one, because a binary reached
/// through `/tmp` (a symlink to `/private/tmp` on macOS) is the same file
/// either way, and the session cache already records that equivalence.
///
/// `None` means "not transient", which is the only answer that lets an install
/// through without `--allow-transient`.
#[must_use]
pub fn is_transient(path: &Path, family: &FamilySites) -> Option<TransientReason> {
    let canonical = path.canonicalize().ok();
    let mut spellings: Vec<&Path> = vec![path];
    if let Some(canonical) = canonical.as_deref() {
        spellings.push(canonical);
    }
    for spelling in spellings {
        if temp_roots().iter().any(|root| spelling.starts_with(root)) {
            return Some(TransientReason::TempRoot);
        }
        if is_build_artifact(spelling) {
            return Some(TransientReason::BuildArtifact);
        }
        for member in &family.linked_work_trees {
            if spelling.starts_with(member) {
                return Some(TransientReason::LinkedMember);
            }
            if member
                .canonicalize()
                .is_ok_and(|member| spelling.starts_with(member))
            {
                return Some(TransientReason::LinkedMember);
            }
        }
    }
    None
}

/// Whether `path` is a file this machine can execute — what a hook target has
/// to be for git to run it, and the health question `binary.json` answers.
#[must_use]
pub fn is_executable_file(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
    }
    // Windows has no execute bit; git runs a hook through its bundled `sh`.
    #[cfg(not(unix))]
    {
        metadata.is_file()
    }
}

/// The shim text for one hook. It chains a pre-existing user hook (now `<name>{CHAINED_SUFFIX}`)
/// first, preserving its exit status, then execs `rift hooks git <name>`.
///
/// **The hook directory comes from parameter expansion, never a command
/// substitution.** `$(dirname "$0")` forks a shell and execs `/usr/bin/dirname`
/// on every hook, and a `git commit` runs three of these shims, so the
/// repository spent that three times over on a 100 ms git-latency budget to
/// compute a string. `${{0%/*}}` is the same answer with no process.
///
/// Certified cost, idle machine, release build, 40 iterations per timing: the
/// installed shim **19.34 ms → 15.78 ms**, i.e. **3.56 ms per hook and 10.7 ms
/// per commit**. (An earlier figure of 4.1 ms/hook circulated from a run taken
/// under load average 6–14; it is quoted here only so the two are not mistaken
/// for a discrepancy. Prefer the idle number — the loaded one measures the
/// machine.)
///
/// The two spellings differ on one input, and the cheap one is also the safer:
/// when `$0` carries no `/`, `dirname` answers `.` and the chain probe resolves
/// against the **process working directory** — an attacker-writable worktree
/// could put an executable there — while `${{0%/*}}` leaves `$0` whole, so the
/// probe names `<hook>/<hook>.rift-chained`, a path under a regular file, which
/// can never be executable. It fails closed instead of resolving elsewhere.
#[must_use]
pub fn shim_text(rift_bin: &Path, name: &str) -> String {
    let bin = rift_bin.display();
    format!(
        "#!/bin/sh\n\
         {MARKER} — do not edit.\n\
         chained=\"${{0%/*}}/{name}{CHAINED_SUFFIX}\"\n\
         if [ -x \"$chained\" ]; then \"$chained\" \"$@\" || exit $?; fi\n\
         exec \"{bin}\" hooks git {name} --git-dir \"${{GIT_DIR:-.git}}\" -- \"$@\"\n"
    )
}

/// The sidecar beside a hook entry — path math only, so every host can name
/// the file a Unix install wrote.
fn link_marker(path: &Path) -> PathBuf {
    let mut marker = path.as_os_str().to_os_string();
    marker.push(LINK_MARKER_SUFFIX);
    PathBuf::from(marker)
}

#[cfg(unix)]
fn direct_link_marker_bytes(target: &Path) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt as _;

    // Keep the path's exact platform bytes. The record is compared as an opaque
    // ownership fingerprint; it is never parsed back into a path.
    let mut record = Vec::with_capacity(MARKER.len() + 1 + target.as_os_str().as_bytes().len());
    record.extend_from_slice(MARKER.as_bytes());
    record.push(0);
    record.extend_from_slice(target.as_os_str().as_bytes());
    record
}

/// Whether a file at `path` is a Rift-managed shim or direct hook link.
///
/// Nothing here follows the link, so **a managed link whose target is gone is
/// still managed**. That has to be true for a dangling hook to
/// be healed in place: the alternative is what happened on this machine on
/// 2026-08-30, where six dead links were chained aside as if they were user
/// hooks — leaving six dead `*.rift-chained` links and a shim on every commit,
/// for hooks that had been Rift's all along.
///
/// The sidecar marker is the ownership proof, and it did not exist before
/// `fd5d716b` (2026-08-26 21:37) — ten minutes *after* the install that pointed
/// this repository's hooks at a `/tmp` binary. So the second arm exists: a
/// **dangling** symlink to a file named `rift`, with no marker beside it, is
/// one of ours from before the marker. It is claimed only while it is dangling;
/// a link that still resolves can still run, and a hook that runs is never
/// taken from its owner.
fn is_rift_managed(path: &Path) -> bool {
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return false;
    };
    #[cfg(unix)]
    if metadata.file_type().is_symlink() {
        let Ok(target) = std::fs::read_link(path) else {
            return false;
        };
        if std::fs::read(link_marker(path))
            .is_ok_and(|record| record == direct_link_marker_bytes(&target))
        {
            return true;
        }
        return !link_target_exists(path, &target)
            && target.file_name().is_some_and(|name| name == "rift")
            && !link_marker(path).exists();
    }
    metadata.file_type().is_file()
        && std::fs::read_to_string(path).is_ok_and(|s| s.contains(MARKER))
}

/// Whether the entry at a hook path is one Rift installed — the question
/// `rift doctor` has to answer before it decides a repository is *broken*
/// rather than simply not set up.
///
/// A repository with no Rift hooks is not a finding: `rift init` is what
/// installs them, and a diagnosis that installed them as a side effect would be
/// the very class of side effect the family-owned hooks exist to remove.
#[must_use]
pub fn is_managed_hook(path: &Path) -> bool {
    is_rift_managed(path)
}

/// One hook entry, seen without following it.
#[derive(Clone, Debug, PartialEq, Eq)]
enum HookEntry {
    /// Nothing at that path, or something unreadable.
    Missing,
    /// Someone else's hook. Rift chains it aside; it never counts as healthy.
    Foreign,
    /// A Rift-managed direct symlink, and the target it names — which may be
    /// gone (that is the dangling case, and the whole reason for the policy).
    ManagedLink(PathBuf),
    /// A Rift-managed shim: the chained-user-hook shape, and what the
    /// installer writes on every non-Unix host. Its target is inside the
    /// script, so the family's record is the only thing that can answer for it.
    ManagedShim,
}

/// Classify one hook path.
///
/// The link arm is not platform-gated: the installer writes links only on
/// Unix, but *reading* one is portable (`symlink_metadata` and `read_link`
/// answer on every host), and a gate here made a Windows build declare the
/// variant dead while a Windows checkout of a Unix-installed family would have
/// mis-read its links as shims.
fn hook_entry(path: &Path) -> HookEntry {
    if !exists_without_following(path).unwrap_or(false) {
        return HookEntry::Missing;
    }
    if !is_rift_managed(path) {
        return HookEntry::Foreign;
    }
    if std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) {
        return std::fs::read_link(path).map_or(HookEntry::Missing, HookEntry::ManagedLink);
    }
    HookEntry::ManagedShim
}

fn exists_without_following(path: &Path) -> std::io::Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

/// Whether the link at `link` resolves — a relative target is resolved against
/// the link's own directory, the way the kernel does it, never against the
/// process working directory.
fn link_target_exists(link: &Path, target: &Path) -> bool {
    if target.is_absolute() {
        return target.exists();
    }
    link.parent()
        .map_or_else(|| target.to_path_buf(), |dir| dir.join(target))
        .exists()
}

/// Install every hook in [`HOOK_NAMES`] under `policy`, and record the family's
/// binary of record.
///
/// The policy decides one question — *may this call re-point a family that is
/// already working?* — and everything else follows from the answer:
///
/// * healthy family + [`KeepIfHealthy`](HookInstallPolicy::KeepIfHealthy) →
///   every hook is `kept`, nothing is written, and the recorded binary is
///   reported. A family that is healthy but has never recorded one **adopts**
///   what its links already name: that is recording reality, not choosing it.
/// * anything dangling, missing, foreign, or a record whose binary is gone →
///   the hooks are written to `cfg.rift_bin` and reported as `healed`, with a
///   [`RecordedBy::SelfHeal`] record when there was a record to supersede.
/// * [`Repoint`](HookInstallPolicy::Repoint) → written unconditionally and
///   reported as `repointed`.
///
/// A [transient](is_transient) `cfg.rift_bin` is **refused before anything is
/// written**: the report carries the refusal and the hooks are exactly as they
/// were found. That is the one screen standing between a benchmark rig's
/// `/tmp` binary and four days of silently skipped hooks.
///
/// # Errors
/// [`RiftError::Io`] on a filesystem failure —
/// including the never-clobber refusal when a user hook cannot be chained aside
/// because its `.rift-chained` name is taken.
pub fn install_hooks_with(
    cfg: &HookInstall,
    policy: HookInstallPolicy,
) -> Result<HookInstallReport> {
    let dir = cfg.resolved_hooks_dir();
    let paths: Vec<PathBuf> = HOOK_NAMES.iter().map(|name| dir.join(name)).collect();
    let record = read_binary_of_record(&cfg.family.dir);
    // Before the health check, because a healthy family returns below without
    // looking at the directory again — and a retired entry is exactly what a
    // healthy family is still carrying.
    let removed = sweep_retired_hooks(&dir)?;

    if matches!(policy, HookInstallPolicy::KeepIfHealthy { .. }) {
        if let Some(live) = healthy_binary(&paths, record.as_ref()) {
            // The record follows the hooks, never the other way round: a family
            // whose hooks all run is healthy, and a note that disagrees with it
            // is corrected here. Recording the path already of record writes
            // nothing, so a re-run of `rift init` leaves one record, not two.
            let of_record = record_binary_of_record(&cfg.family.dir, &live, cfg.recorded_by)?;
            return Ok(HookInstallReport {
                hooks_dir: dir,
                binary: Some(PathBuf::from(of_record.rift_bin)),
                kept: paths,
                removed,
                ..HookInstallReport::default()
            });
        }
    }

    if !policy.allows_transient() {
        if let Some(reason) = is_transient(&cfg.rift_bin, &cfg.family) {
            return Ok(HookInstallReport {
                hooks_dir: dir,
                binary: record.map(|record| PathBuf::from(record.rift_bin)),
                removed,
                refused: Some(RefusedBinary {
                    path: cfg.rift_bin.clone(),
                    reason,
                }),
                ..HookInstallReport::default()
            });
        }
    }

    let written = write_hooks(cfg, &dir)?;
    let recorded_by = match policy {
        HookInstallPolicy::Repoint { .. } => RecordedBy::Repoint,
        HookInstallPolicy::KeepIfHealthy { .. } if record.is_some() => RecordedBy::SelfHeal,
        HookInstallPolicy::KeepIfHealthy { .. } => cfg.recorded_by,
    };
    let of_record = record_binary_of_record(&cfg.family.dir, &cfg.rift_bin, recorded_by)?;
    let mut report = HookInstallReport {
        hooks_dir: dir,
        binary: Some(PathBuf::from(of_record.rift_bin)),
        removed,
        ..HookInstallReport::default()
    };
    if matches!(policy, HookInstallPolicy::Repoint { .. }) {
        report.repointed = written;
    } else {
        report.healed = written;
    }
    Ok(report)
}

/// Remove the Rift-managed entries under names [`HOOK_NAMES`] no longer
/// carries, and report what was removed.
///
/// A family provisioned while the list was longer still has an entry under
/// every name that has since left it, and git goes on running those: it never
/// hears that a name left the dispatch table, so a merge and every step of a
/// rebase run a link whose binary answers with a usage error. The name left
/// the list, so the entry leaves the directory.
///
/// Only the entry, and only ours: ownership is the same question
/// [`is_rift_managed`] answers for a live name, so a hook somebody else put
/// there under a retired name is left exactly where it is. Nothing here
/// follows a link — the link is what is removed, never what it names.
///
/// # Errors
/// [`RiftError::Io`] on a filesystem failure.
fn sweep_retired_hooks(dir: &Path) -> Result<Vec<PathBuf>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    // Read the whole directory before touching any of it: removing entries
    // while the handle walks them is where a listing starts skipping things.
    let mut retired: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| is_retired_hook(path))
        .collect();
    retired.sort();
    for path in &retired {
        remove_retired_hook(path)?;
    }
    Ok(retired)
}

/// Whether the entry at `path` is one of Rift's, under a name Rift no longer
/// installs — what an install removes and what `rift doctor` reports.
///
/// The same ownership question [`is_managed_hook`] answers for a live name,
/// asked of a name that is not in [`HOOK_NAMES`] at all. Public because the
/// diagnosis and the repair must agree on the answer, and a second reading of
/// it in the bin is a second answer waiting to happen.
#[must_use]
pub fn is_retired_hook(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    // Neither suffix names a hook entry: a chained hook is the user's file,
    // which git never runs by that name, and a sidecar goes with its link.
    if HOOK_NAMES.contains(&name)
        || name.ends_with(CHAINED_SUFFIX)
        || name.ends_with(LINK_MARKER_SUFFIX)
    {
        return false;
    }
    is_rift_managed(path)
}

/// Remove one retired entry and its sidecar, and put back the user hook the
/// install that wrote it chained aside.
///
/// The shim that ran that hook is going; leaving it behind the suffix would end
/// a hook the user has always had, and end it the way this whole module exists
/// to prevent — silently.
fn remove_retired_hook(path: &Path) -> Result<()> {
    std::fs::remove_file(path)?;
    let marker = link_marker(path);
    if exists_without_following(&marker)? {
        std::fs::remove_file(&marker)?;
    }
    let mut chained = path.as_os_str().to_os_string();
    chained.push(CHAINED_SUFFIX);
    let chained = PathBuf::from(chained);
    if exists_without_following(&chained)? {
        std::fs::rename(&chained, path)?;
    }
    Ok(())
}

/// The binary a family's hooks are all pointed at, when every one of them is in
/// working order — `None` the moment any is not.
///
/// Healthy means: every hook in [`HOOK_NAMES`] is Rift-managed, every direct
/// link resolves to an executable file, and a recorded binary (if there is one)
/// is still executable. A shim carries its target inside the script, so a
/// shim-only family with no record has no answer here and re-installs.
fn healthy_binary(paths: &[PathBuf], record: Option<&BinaryOfRecord>) -> Option<PathBuf> {
    let mut observed = None;
    let mut has_shim = false;
    for path in paths {
        match hook_entry(path) {
            HookEntry::ManagedLink(target) => {
                if !link_target_exists(path, &target) || !is_executable_file(&target) {
                    return None;
                }
                observed.get_or_insert(target);
            }
            HookEntry::ManagedShim => has_shim = true,
            HookEntry::Missing | HookEntry::Foreign => return None,
        }
    }
    if has_shim {
        // A shim carries its binary inside the script, so nothing can be
        // observed from outside it and only the record can answer. A family of
        // shims that never recorded one has no answer, and re-installs.
        return record
            .filter(|record| record.is_live())
            .map(|record| record.path().to_path_buf());
    }
    observed
}

/// Write every hook, chaining any pre-existing user hook aside. Returns the
/// installed hook paths.
fn write_hooks(cfg: &HookInstall, dir: &Path) -> Result<Vec<PathBuf>> {
    std::fs::create_dir_all(dir)?;
    let mut written = Vec::with_capacity(HOOK_NAMES.len());
    for name in HOOK_NAMES {
        let target = dir.join(name);
        let chained = dir.join(format!("{name}{CHAINED_SUFFIX}"));
        // Chain a pre-existing, non-Rift hook aside (once) so we never clobber it.
        if exists_without_following(&target)? && !is_rift_managed(&target) {
            if exists_without_following(&chained)? {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    format!(
                        "refusing to overwrite user hook {} while {} already exists",
                        target.display(),
                        chained.display()
                    ),
                )
                .into());
            }
            std::fs::rename(&target, &chained)?;
        }

        if exists_without_following(&target)? {
            std::fs::remove_file(&target)?;
        }

        if exists_without_following(&chained)? {
            // The shell is retained only where it has real work to do: run the
            // user's hook before Rift while preserving that hook's exit status.
            std::fs::write(&target, shim_text(&cfg.rift_bin, name))?;
            set_executable(&target)?;
            #[cfg(unix)]
            {
                let marker = link_marker(&target);
                if exists_without_following(&marker)? {
                    std::fs::remove_file(marker)?;
                }
            }
        } else {
            #[cfg(unix)]
            {
                std::os::unix::fs::symlink(&cfg.rift_bin, &target)?;
                std::fs::write(
                    link_marker(&target),
                    direct_link_marker_bytes(&cfg.rift_bin),
                )?;
            }
            #[cfg(not(unix))]
            {
                std::fs::write(&target, shim_text(&cfg.rift_bin, name))?;
                set_executable(&target)?;
            }
        }
        written.push(target);
    }
    Ok(written)
}

#[cfg(unix)]
fn set_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))?;
    Ok(())
}
#[cfg(not(unix))]
fn set_executable(_path: &Path) -> Result<()> {
    // Windows git runs hooks via its bundled `sh`; the execute bit is not used.
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(dir: &Path, hooks: Option<PathBuf>) -> HookInstall {
        HookInstall {
            git_dir: dir.join(".git"),
            rift_bin: PathBuf::from("/usr/local/bin/rift"),
            hooks_dir: hooks,
            family: FamilySites::new(dir.join(".git").join("rift-family")),
            recorded_by: RecordedBy::HooksInstall,
        }
    }

    /// The unconditional replacement the never-clobber tests below are about:
    /// they pin how a hook is WRITTEN, which is one question below the policy.
    fn repoint() -> HookInstallPolicy {
        HookInstallPolicy::Repoint {
            allow_transient: true,
        }
    }

    #[test]
    fn install_uses_absolute_binary_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let report = install_hooks_with(&cfg(tmp.path(), None), repoint()).unwrap();
        let written = report.repointed;
        assert_eq!(written.len(), HOOK_NAMES.len());
        for (path, name) in written.iter().zip(HOOK_NAMES) {
            assert_eq!(
                path.file_name().and_then(|value| value.to_str()),
                Some(*name)
            );
            #[cfg(unix)]
            {
                assert_eq!(
                    std::fs::read_link(path).unwrap(),
                    Path::new("/usr/local/bin/rift")
                );
                assert_eq!(
                    std::fs::read(link_marker(path)).unwrap(),
                    direct_link_marker_bytes(Path::new("/usr/local/bin/rift"))
                );
            }
            #[cfg(not(unix))]
            {
                let text = std::fs::read_to_string(path).unwrap();
                assert!(text.contains("/usr/local/bin/rift"), "absolute path");
                assert!(!text.contains("exec \"rift\""), "never a bare name");
                assert!(text.contains(&format!("hooks git {name} ")));
            }
        }
    }

    #[test]
    fn install_chains_existing_hook() {
        let tmp = tempfile::tempdir().unwrap();
        let hooks = tmp.path().join(".git").join("hooks");
        std::fs::create_dir_all(&hooks).unwrap();
        let user = hooks.join("post-commit");
        std::fs::write(&user, "#!/bin/sh\necho user hook\n").unwrap();

        install_hooks_with(&cfg(tmp.path(), None), repoint()).unwrap();

        // the user's hook is preserved aside, not clobbered
        let chained = hooks.join("post-commit.rift-chained");
        assert!(chained.exists(), "user hook chained aside");
        assert!(std::fs::read_to_string(&chained)
            .unwrap()
            .contains("echo user hook"));
        // the new shim references the chained hook
        let shim = std::fs::read_to_string(hooks.join("post-commit")).unwrap();
        assert!(shim.contains("post-commit.rift-chained"));
        assert!(shim.contains(MARKER));
    }

    #[test]
    fn reinstall_is_idempotent_and_does_not_double_chain() {
        let tmp = tempfile::tempdir().unwrap();
        let hooks = tmp.path().join(".git").join("hooks");
        std::fs::create_dir_all(&hooks).unwrap();
        std::fs::write(hooks.join("post-commit"), "#!/bin/sh\necho user\n").unwrap();

        install_hooks_with(&cfg(tmp.path(), None), repoint()).unwrap();
        let chained_once = std::fs::read_to_string(hooks.join("post-commit.rift-chained")).unwrap();
        // second install must NOT chain our own shim over the user's chained hook
        install_hooks_with(&cfg(tmp.path(), None), repoint()).unwrap();
        let chained_twice =
            std::fs::read_to_string(hooks.join("post-commit.rift-chained")).unwrap();
        assert_eq!(
            chained_once, chained_twice,
            "user hook preserved, not re-chained"
        );
        assert!(chained_twice.contains("echo user"));
    }

    #[test]
    fn respects_custom_hooks_path() {
        let tmp = tempfile::tempdir().unwrap();
        let custom = tmp.path().join("my-hooks");
        install_hooks_with(&cfg(tmp.path(), Some(custom.clone())), repoint()).unwrap();
        assert!(std::fs::symlink_metadata(custom.join("pre-push")).is_ok());
        assert!(std::fs::symlink_metadata(tmp.path().join(".git/hooks/pre-push")).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn reinstall_updates_a_managed_direct_link_without_chaining_it() {
        let tmp = tempfile::tempdir().unwrap();
        let mut first = cfg(tmp.path(), None);
        first.rift_bin = PathBuf::from("/first/rift");
        install_hooks_with(&first, repoint()).unwrap();

        let mut second = cfg(tmp.path(), None);
        second.rift_bin = PathBuf::from("/second/rift");
        install_hooks_with(&second, repoint()).unwrap();

        let hooks = tmp.path().join(".git/hooks");
        assert_eq!(
            std::fs::read_link(hooks.join("post-commit")).unwrap(),
            Path::new("/second/rift")
        );
        assert!(std::fs::symlink_metadata(hooks.join("post-commit.rift-chained")).is_err());
    }

    #[test]
    fn no_shim_forks_a_process_to_compute_its_own_directory() {
        // A command substitution in the shim is a fork+exec on the hook path,
        // three times per commit. Backticks count as one too, so both
        // spellings are screened rather than the one that happens to be here.
        for name in HOOK_NAMES {
            let text = shim_text(Path::new("/abs/rift"), name);
            assert!(!text.contains("$("), "{name}: command substitution: {text}");
            assert!(!text.contains('`'), "{name}: backtick substitution: {text}");
            assert!(text.contains("${0%/*}"), "{name}: {text}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn the_shim_runs_the_chained_hook_and_keeps_its_exit_code() {
        // The parameter-expansion rewrite changes how the shim resolves the
        // chained hook's path, so the guarantee is asserted by RUNNING it, not
        // by reading the text. The chained hook exits 3 so the shim returns
        // before the final `exec` (whose binary does not exist here) — which
        // also pins the "preserve the user hook's exit status" contract.
        use std::os::unix::fs::PermissionsExt as _;
        let tmp = tempfile::tempdir().unwrap();
        let hooks = tmp.path().join(".git").join("hooks");
        std::fs::create_dir_all(&hooks).unwrap();
        let user = hooks.join("post-commit");
        let marker = tmp.path().join("ran");
        std::fs::write(
            &user,
            format!("#!/bin/sh\ntouch '{}'\nexit 3\n", marker.display()),
        )
        .unwrap();
        std::fs::set_permissions(&user, std::fs::Permissions::from_mode(0o755)).unwrap();

        install_hooks_with(&cfg(tmp.path(), None), repoint()).unwrap();

        let out = std::process::Command::new(hooks.join("post-commit"))
            .current_dir(tmp.path())
            .output()
            .unwrap();
        assert!(marker.exists(), "the chained hook ran");
        assert_eq!(out.status.code(), Some(3), "its exit status is preserved");
    }

    // --- the policy matrix and the binary of record -------------------------

    /// An executable file standing in for a built `rift`.
    fn fake_binary(dir: &Path, name: &str) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        path
    }

    fn keep(allow_transient: bool) -> HookInstallPolicy {
        HookInstallPolicy::KeepIfHealthy { allow_transient }
    }

    /// Every fixture binary here lives in a temp dir, which is exactly what the
    /// screen refuses — so a fixture that means to install one says so.
    fn install_at(dir: &Path, bin: &Path, policy: HookInstallPolicy) -> HookInstallReport {
        let mut cfg = cfg(dir, None);
        cfg.rift_bin = bin.to_path_buf();
        install_hooks_with(&cfg, policy).unwrap()
    }

    #[test]
    fn the_default_policy_keeps_a_healthy_family_and_screens_transient_paths() {
        assert_eq!(
            HookInstallPolicy::default(),
            HookInstallPolicy::KeepIfHealthy {
                allow_transient: false
            }
        );
        assert!(!HookInstallPolicy::default().allows_transient());
    }

    /// The measured failure mode: `rift init` in a second worktree
    /// re-pointed the whole family at that process's binary. It may not.
    #[test]
    fn healthy_hooks_are_kept_across_an_init_from_another_worktree() {
        let tmp = tempfile::tempdir().unwrap();
        let installed = fake_binary(&tmp.path().join("main/target/debug"), "rift");
        install_at(tmp.path(), &installed, repoint());

        // A second worktree runs its own binary and installs by side effect.
        let other = fake_binary(&tmp.path().join("member/target/debug"), "rift");
        let report = install_at(tmp.path(), &other, keep(false));

        assert_eq!(report.kept.len(), HOOK_NAMES.len(), "every hook kept");
        assert!(report.healed.is_empty() && report.repointed.is_empty());
        assert_eq!(report.binary.as_deref(), Some(installed.as_path()));
        #[cfg(unix)]
        for name in HOOK_NAMES {
            assert_eq!(
                std::fs::read_link(tmp.path().join(".git/hooks").join(name)).unwrap(),
                installed,
                "{name} still points at the family's binary"
            );
        }
        let record = read_binary_of_record(&tmp.path().join(".git/rift-family")).unwrap();
        assert_eq!(record.path(), installed);
        assert!(record.history.is_empty(), "keeping supersedes nothing");
    }

    /// ★ `post-checkout` is installed by this chokepoint and nothing else,
    /// and a family installed while the list was shorter is repaired.
    ///
    /// `post-checkout` is the only moment git tells Rift a worktree was born,
    /// and it is reached the way every other one is: a name in [`HOOK_NAMES`],
    /// written here and routed by `argv[0]` in the bin. So the first half of
    /// this test is that the name is in the list and the entry lands.
    ///
    /// The second half is the consequence nobody may discover by surprise. A
    /// family installed before the name joined the list is missing a hook —
    /// [`healthy_binary`] must call that *unhealthy* so the next
    /// `KeepIfHealthy` install writes it and records the repair. A list that
    /// changed while the health probe kept saying "fine" would install the
    /// hook nowhere and report nothing, which is the silence this whole design
    /// exists to prevent.
    #[test]
    fn the_worktree_hook_is_installed_and_a_family_missing_it_is_not_healthy() {
        assert!(
            HOOK_NAMES.contains(&"post-checkout"),
            "this hook is dispatched by its presence in this list alone"
        );
        let tmp = tempfile::tempdir().unwrap();
        let bin = fake_binary(&tmp.path().join("live"), "rift");
        install_at(tmp.path(), &bin, repoint());
        let hooks = tmp.path().join(".git/hooks");
        let adoption = hooks.join("post-checkout");
        assert!(exists_without_following(&adoption).unwrap(), "installed");

        // The shape an installation from before the name joined is in.
        std::fs::remove_file(&adoption).unwrap();
        #[cfg(unix)]
        std::fs::remove_file(link_marker(&adoption)).unwrap();

        let report = install_at(tmp.path(), &bin, keep(true));

        assert!(
            report.kept.is_empty(),
            "a family missing a hook is not healthy: {report:?}"
        );
        assert_eq!(report.healed.len(), HOOK_NAMES.len(), "{report:?}");
        assert!(exists_without_following(&adoption).unwrap(), "healed");
        assert!(
            std::fs::symlink_metadata(hooks.join("post-checkout.rift-chained")).is_err(),
            "a hook Rift added to its own list is never chained aside as a user's"
        );
    }

    /// D13. A managed link whose target is gone is still ours: it is healed in
    /// place, never renamed aside as if a user had put it there.
    ///
    /// Portable on purpose, because the claim is: on Unix the dangling entry is
    /// the link itself, on Windows it is the record the shim's binary left
    /// behind — and neither may produce a `.rift-chained` name.
    #[test]
    fn a_dangling_managed_link_is_healed_not_chained_aside() {
        let tmp = tempfile::tempdir().unwrap();
        let first = fake_binary(&tmp.path().join("gone"), "rift");
        install_at(tmp.path(), &first, repoint());
        std::fs::remove_file(&first).unwrap();

        let second = fake_binary(&tmp.path().join("live"), "rift");
        let report = install_at(tmp.path(), &second, keep(true));

        assert_eq!(report.healed.len(), HOOK_NAMES.len());
        assert!(report.kept.is_empty());
        let hooks = tmp.path().join(".git/hooks");
        for name in HOOK_NAMES {
            #[cfg(unix)]
            assert_eq!(std::fs::read_link(hooks.join(name)).unwrap(), second);
            assert!(
                std::fs::symlink_metadata(hooks.join(format!("{name}{CHAINED_SUFFIX}"))).is_err(),
                "{name}: a dead link of ours is never chained aside"
            );
        }
    }

    /// The 2026-08-30 shape exactly: the link-target sidecar landed in
    /// `fd5d716b`, ten minutes AFTER the install that pointed this repository's
    /// hooks at a `/tmp` binary — so the dangling links left by that install
    /// had no marker, and the repair chained six dead links aside. A dangling
    /// symlink to a file named `rift` is ours even with no marker beside it.
    #[cfg(unix)]
    #[test]
    fn an_unmarked_dangling_link_from_before_the_marker_is_healed_not_chained_aside() {
        let tmp = tempfile::tempdir().unwrap();
        let hooks = tmp.path().join(".git/hooks");
        std::fs::create_dir_all(&hooks).unwrap();
        let deleted = tmp.path().join("rift-measure-run/rift");
        std::os::unix::fs::symlink(&deleted, hooks.join("post-commit")).unwrap();
        assert!(is_rift_managed(&hooks.join("post-commit")), "ours");

        let live = fake_binary(&tmp.path().join("live"), "rift");
        let report = install_at(tmp.path(), &live, keep(true));

        assert_eq!(report.healed.len(), HOOK_NAMES.len());
        assert_eq!(std::fs::read_link(hooks.join("post-commit")).unwrap(), live);
        assert!(
            std::fs::symlink_metadata(hooks.join("post-commit.rift-chained")).is_err(),
            "no dead .rift-chained link is left behind"
        );
    }

    /// A user's hook that can still RUN is still theirs — the never-clobber
    /// rule is untouched by D13, and this is the line between the two.
    #[cfg(unix)]
    #[test]
    fn a_live_unmarked_link_is_a_user_hook_and_is_chained_aside() {
        let tmp = tempfile::tempdir().unwrap();
        let hooks = tmp.path().join(".git/hooks");
        std::fs::create_dir_all(&hooks).unwrap();
        let theirs = fake_binary(&tmp.path().join("theirs"), "rift");
        std::os::unix::fs::symlink(&theirs, hooks.join("post-commit")).unwrap();

        let live = fake_binary(&tmp.path().join("live"), "rift");
        install_at(tmp.path(), &live, keep(true));

        assert_eq!(
            std::fs::read_link(hooks.join("post-commit.rift-chained")).unwrap(),
            theirs,
            "a hook that can run is preserved"
        );
    }

    /// The D13 second arm is **narrow on purpose**, and until this test nothing
    /// held its edges. Every symlink fixture in the tree named its target
    /// `rift`, so `&& target.file_name() == "rift"` could be deleted and the
    /// whole suite stayed green while the claim widened from "one of ours from
    /// before the marker" to *every dangling symlink in the hooks directory*.
    /// A user's dead link to their own tool is still theirs.
    #[cfg(unix)]
    #[test]
    fn a_dangling_link_to_a_target_not_named_rift_is_a_user_hook() {
        let tmp = tempfile::tempdir().unwrap();
        let hooks = tmp.path().join(".git/hooks");
        std::fs::create_dir_all(&hooks).unwrap();
        let theirs = tmp.path().join("their-tool/lefthook");
        std::os::unix::fs::symlink(&theirs, hooks.join("post-commit")).unwrap();
        assert!(
            !theirs.exists(),
            "dangling — the arm's other condition holds"
        );
        assert_eq!(hook_entry(&hooks.join("post-commit")), HookEntry::Foreign);

        let live = fake_binary(&tmp.path().join("live"), "rift");
        install_at(tmp.path(), &live, keep(true));

        assert_eq!(
            std::fs::read_link(hooks.join("post-commit.rift-chained")).unwrap(),
            theirs,
            "a dangling hook that was never ours is chained aside, not seized"
        );
    }

    /// The arm's other condition, `!link_marker(path).exists()`. A marker beside
    /// a link is the ownership proof, and one that names a DIFFERENT target
    /// proves the opposite: the link was replaced after Rift recorded it. The
    /// pre-marker inference is for links with no marker at all — where the file
    /// Rift would have written is absent, not contradicted.
    #[cfg(unix)]
    #[test]
    fn a_dangling_rift_link_contradicted_by_its_marker_is_a_user_hook() {
        let tmp = tempfile::tempdir().unwrap();
        let hooks = tmp.path().join(".git/hooks");
        std::fs::create_dir_all(&hooks).unwrap();
        let theirs = tmp.path().join("their-tool/rift");
        let hook = hooks.join("post-commit");
        std::os::unix::fs::symlink(&theirs, &hook).unwrap();
        std::fs::write(
            link_marker(&hook),
            direct_link_marker_bytes(Path::new("/opt/rift/rift")),
        )
        .unwrap();
        assert_eq!(hook_entry(&hook), HookEntry::Foreign);

        let live = fake_binary(&tmp.path().join("live"), "rift");
        install_at(tmp.path(), &live, keep(true));

        assert_eq!(
            std::fs::read_link(hooks.join("post-commit.rift-chained")).unwrap(),
            theirs,
            "a link its own marker disowns is chained aside"
        );
    }

    /// The repair is a record, not a rumour: `binary.json` gains a `self-heal`
    /// entry naming the new binary, with the dead one behind it.
    #[test]
    fn a_dangling_hook_self_heals_and_records_it() {
        let tmp = tempfile::tempdir().unwrap();
        let first = fake_binary(&tmp.path().join("gone"), "rift");
        install_at(tmp.path(), &first, repoint());
        std::fs::remove_file(&first).unwrap();

        let second = fake_binary(&tmp.path().join("live"), "rift");
        let report = install_at(tmp.path(), &second, keep(true));

        assert_eq!(report.binary.as_deref(), Some(second.as_path()));
        let record = read_binary_of_record(&tmp.path().join(".git/rift-family")).unwrap();
        assert_eq!(record.path(), second);
        assert_eq!(record.recorded_by, RecordedBy::SelfHeal);
        assert_eq!(record.schema_version, 1);
        assert_eq!(
            record.history.last().map(|r| r.rift_bin.as_str()),
            Some(first.to_string_lossy().as_ref()),
            "the binary that died is kept behind the one that replaced it"
        );
    }

    /// An explicit re-point at a path that outlives the decision
    /// by less than the family does is refused, and refusing writes NOTHING.
    #[test]
    fn a_transient_repoint_is_refused_without_allow_transient() {
        let tmp = tempfile::tempdir().unwrap();
        let bin = fake_binary(&tmp.path().join("rig"), "rift");
        let report = install_at(
            tmp.path(),
            &bin,
            HookInstallPolicy::Repoint {
                allow_transient: false,
            },
        );

        let refused = report.refused.expect("a temp-root binary is refused");
        assert_eq!(refused.reason, TransientReason::TempRoot);
        assert_eq!(refused.path, bin);
        assert!(report.repointed.is_empty() && report.healed.is_empty());
        assert!(
            std::fs::symlink_metadata(tmp.path().join(".git/hooks/post-commit")).is_err(),
            "a refusal writes no hook"
        );
        assert!(
            read_binary_of_record(&tmp.path().join(".git/rift-family")).is_none(),
            "a refusal records nothing"
        );
        // The same rig, said out loud, is allowed.
        assert!(install_at(
            tmp.path(),
            &bin,
            HookInstallPolicy::Repoint {
                allow_transient: true
            }
        )
        .refused
        .is_none());
    }

    /// The screen guards the SELF-HEAL target too: a dangling
    /// family is not repaired by pointing it at something that will dangle next.
    #[cfg(unix)]
    #[test]
    fn a_transient_self_heal_is_refused_and_leaves_the_hooks_as_found() {
        let tmp = tempfile::tempdir().unwrap();
        let first = fake_binary(&tmp.path().join("gone"), "rift");
        install_at(tmp.path(), &first, repoint());
        std::fs::remove_file(&first).unwrap();

        let second = fake_binary(&tmp.path().join("live"), "rift");
        let report = install_at(tmp.path(), &second, keep(false));

        assert_eq!(
            report.refused.map(|r| r.reason),
            Some(TransientReason::TempRoot)
        );
        assert_eq!(
            std::fs::read_link(tmp.path().join(".git/hooks/post-commit")).unwrap(),
            first,
            "the hooks are exactly as they were found"
        );
    }

    #[test]
    fn repoint_replaces_and_appends_history() {
        let tmp = tempfile::tempdir().unwrap();
        let family = tmp.path().join(".git/rift-family");
        install_at(tmp.path(), Path::new("/opt/rift-a"), repoint());
        let report = install_at(tmp.path(), Path::new("/opt/rift-b"), repoint());

        assert_eq!(report.repointed.len(), HOOK_NAMES.len());
        assert!(report.kept.is_empty());
        #[cfg(unix)]
        assert_eq!(
            std::fs::read_link(tmp.path().join(".git/hooks/pre-push")).unwrap(),
            Path::new("/opt/rift-b")
        );
        let record = read_binary_of_record(&family).unwrap();
        assert_eq!(record.rift_bin, "/opt/rift-b");
        assert_eq!(record.recorded_by, RecordedBy::Repoint);
        assert_eq!(
            record
                .history
                .iter()
                .map(|r| r.rift_bin.as_str())
                .collect::<Vec<_>>(),
            vec!["/opt/rift-a"]
        );

        // Re-pointing at the path already of record writes no second record.
        install_at(tmp.path(), Path::new("/opt/rift-b"), repoint());
        assert_eq!(read_binary_of_record(&family).unwrap().history.len(), 1);
    }

    #[test]
    fn binary_json_history_is_capped() {
        let tmp = tempfile::tempdir().unwrap();
        let family = tmp.path().join("rift-family");
        for i in 0..20 {
            record_binary_of_record(
                &family,
                &PathBuf::from(format!("/opt/rift-{i}")),
                RecordedBy::Repoint,
            )
            .unwrap();
        }
        let record = read_binary_of_record(&family).unwrap();
        assert_eq!(record.rift_bin, "/opt/rift-19");
        assert_eq!(record.history.len(), 16, "capped at 16");
        assert_eq!(
            record.history.first().map(|r| r.rift_bin.as_str()),
            Some("/opt/rift-3"),
            "the oldest are the ones dropped"
        );
        assert_eq!(
            record.history.last().map(|r| r.rift_bin.as_str()),
            Some("/opt/rift-18"),
        );
        // No temporary file survives a completed write.
        let strays: Vec<_> = std::fs::read_dir(&family)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| name != "binary.json")
            .collect();
        assert!(
            strays.is_empty(),
            "tmp+rename leaves nothing behind: {strays:?}"
        );
    }

    #[test]
    fn a_garbled_record_is_not_honoured_and_is_replaced() {
        let tmp = tempfile::tempdir().unwrap();
        let family = tmp.path().join("rift-family");
        std::fs::create_dir_all(&family).unwrap();
        std::fs::write(binary_of_record_path(&family), b"{not json").unwrap();
        assert!(read_binary_of_record(&family).is_none());
        record_binary_of_record(&family, Path::new("/opt/rift"), RecordedBy::Init).unwrap();
        assert_eq!(
            read_binary_of_record(&family).unwrap().rift_bin,
            "/opt/rift"
        );

        // A shape from a future schema is not guessed at either.
        std::fs::write(
            binary_of_record_path(&family),
            br#"{"schema_version":2,"rift_bin":"/opt/x","recorded_at_ms":0,"recorded_by":"init"}"#,
        )
        .unwrap();
        assert!(read_binary_of_record(&family).is_none());
    }

    #[test]
    fn is_transient_names_the_three_shapes_and_allows_the_main_worktree() {
        let member = PathBuf::from("/Users/dev/rift-worktrees/swt-b");
        let family = FamilySites {
            dir: PathBuf::from("/Users/dev/repo/.git/rift-family"),
            linked_work_trees: vec![member.clone()],
        };
        let cases = [
            ("/tmp/rift-measure-run.42/rift", TransientReason::TempRoot),
            ("/private/tmp/rift", TransientReason::TempRoot),
            ("/var/folders/x9/T/rift", TransientReason::TempRoot),
            ("/private/var/folders/x9/T/rift", TransientReason::TempRoot),
            (
                "/Users/dev/repo/target/debug/deps/rift-3f2a",
                TransientReason::BuildArtifact,
            ),
            (
                "/Users/dev/repo/target/release/deps/rift-3f2a",
                TransientReason::BuildArtifact,
            ),
            // The 1.100-era build-dir layout: test binaries run from
            // `<profile>/build/<name>/<hash>/out/<name>-<hash>` — the shape a
            // doctor --fix under the coverage harness pointed a sandbox
            // family at before `build` joined the screen.
            (
                "/Users/dev/repo/target/llvm-cov-target/debug/build/rift/dd1d/out/rift-dd1d",
                TransientReason::BuildArtifact,
            ),
            (
                "/Users/dev/repo/target/debug/build/rift-abc123/out/rift",
                TransientReason::BuildArtifact,
            ),
            (
                "/Users/dev/rift-worktrees/swt-b/target/debug/rift",
                TransientReason::LinkedMember,
            ),
        ];
        for (path, reason) in cases {
            assert_eq!(
                is_transient(Path::new(path), &family),
                Some(reason),
                "{path}"
            );
        }
        // The developer's own build: the MAIN work tree's `target/`, which is
        // not a member and is deliberately not screened.
        assert_eq!(
            is_transient(Path::new("/Users/dev/repo/target/debug/rift"), &family),
            None
        );
        assert_eq!(
            is_transient(Path::new("/usr/local/bin/rift"), &family),
            None
        );
        // A member that is not this family's is not this family's problem.
        assert_eq!(
            is_transient(
                Path::new("/Users/dev/other/target/debug/rift"),
                &FamilySites::new(family.dir.clone())
            ),
            None
        );
    }

    #[test]
    fn the_screen_reads_a_temp_path_through_its_canonical_form() {
        // `/tmp` is a symlink to `/private/tmp` on macOS: the same file has two
        // spellings and the screen has to refuse both.
        let tmp = tempfile::tempdir().unwrap();
        let bin = fake_binary(tmp.path(), "rift");
        let family = FamilySites::new(tmp.path().join("rift-family"));
        assert_eq!(
            is_transient(&bin, &family),
            Some(TransientReason::TempRoot),
            "{}",
            bin.display()
        );
        assert_eq!(
            is_transient(&bin.canonicalize().unwrap(), &family),
            Some(TransientReason::TempRoot)
        );
    }

    #[test]
    fn hooks_name_no_upstream_brand() {
        // Shares the capture layer's forbidden-token set; no upstream brand
        // may appear.
        for name in HOOK_NAMES {
            let text = shim_text(Path::new("/abs/rift"), name).to_ascii_lowercase();
            assert!(
                !text.contains(concat!("ent", "ire")),
                "{name} shim leaks brand"
            );
            assert!(
                !text.contains(concat!("ent", "ireio")),
                "{name} shim leaks brand"
            );
        }
    }

    // --- the names that left the list ---------------------------------------

    /// The shape a family provisioned while [`HOOK_NAMES`] was longer still
    /// carries: a Rift-managed entry under a name the binary no longer routes.
    fn retired_managed_hook(hooks: &Path, name: &str, bin: &Path) {
        std::fs::create_dir_all(hooks).unwrap();
        let path = hooks.join(name);
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(bin, &path).unwrap();
            std::fs::write(link_marker(&path), direct_link_marker_bytes(bin)).unwrap();
        }
        #[cfg(not(unix))]
        {
            std::fs::write(&path, shim_text(bin, name)).unwrap();
            set_executable(&path).unwrap();
        }
    }

    /// ★ A healthy family is swept before it is kept.
    ///
    /// Every entry under a retired name still runs — git does not know the name
    /// left the dispatch table, so it runs the link on every merge and every
    /// rebase step, and the binary answers a name it cannot route with a usage
    /// error. The early return for a healthy family is the one path that would
    /// never look at the directory again, so the sweep runs before it.
    #[test]
    fn a_healthy_family_loses_the_entries_for_names_rift_no_longer_installs() {
        let tmp = tempfile::tempdir().unwrap();
        let bin = fake_binary(&tmp.path().join("live"), "rift");
        install_at(tmp.path(), &bin, repoint());
        let hooks = tmp.path().join(".git/hooks");
        retired_managed_hook(&hooks, "post-merge", &bin);
        retired_managed_hook(&hooks, "post-rewrite", &bin);

        let report = install_at(tmp.path(), &bin, keep(true));

        assert_eq!(report.kept.len(), HOOK_NAMES.len(), "{report:?}");
        assert_eq!(
            report.removed,
            vec![hooks.join("post-merge"), hooks.join("post-rewrite")]
        );
        for name in ["post-merge", "post-rewrite"] {
            let path = hooks.join(name);
            assert!(!exists_without_following(&path).unwrap(), "{name} is gone");
            #[cfg(unix)]
            assert!(
                !exists_without_following(&link_marker(&path)).unwrap(),
                "{name} took its marker with it"
            );
        }
    }

    /// The sweep is not the health probe's: an explicit re-point removes the
    /// retired entries too, and reports them beside what it wrote.
    #[test]
    fn a_repoint_removes_the_entries_for_names_rift_no_longer_installs() {
        let tmp = tempfile::tempdir().unwrap();
        let bin = fake_binary(&tmp.path().join("live"), "rift");
        let hooks = tmp.path().join(".git/hooks");
        retired_managed_hook(&hooks, "post-merge", &bin);

        let report = install_at(tmp.path(), &bin, repoint());

        assert_eq!(report.repointed.len(), HOOK_NAMES.len());
        assert_eq!(report.removed, vec![hooks.join("post-merge")]);
        assert!(!exists_without_following(&hooks.join("post-merge")).unwrap());
    }

    /// The never-clobber rule reaches the retired names as well: a hook under
    /// one of them that Rift did not write is the user's, a `.sample` is git's,
    /// and neither is Rift's to remove.
    #[test]
    fn a_hook_rift_did_not_write_survives_the_sweep_of_a_retired_name() {
        let tmp = tempfile::tempdir().unwrap();
        let hooks = tmp.path().join(".git/hooks");
        std::fs::create_dir_all(&hooks).unwrap();
        std::fs::write(hooks.join("post-merge"), "#!/bin/sh\necho theirs\n").unwrap();
        std::fs::write(hooks.join("pre-rebase.sample"), "#!/bin/sh\nexit 0\n").unwrap();

        let bin = fake_binary(&tmp.path().join("live"), "rift");
        let report = install_at(tmp.path(), &bin, repoint());

        assert!(report.removed.is_empty(), "{report:?}");
        assert!(std::fs::read_to_string(hooks.join("post-merge"))
            .unwrap()
            .contains("echo theirs"));
        assert!(hooks.join("pre-rebase.sample").exists());
    }

    /// A link a marker disowns is a user hook under a retired name exactly as
    /// it is under a live one — the ownership question has one answer here.
    #[cfg(unix)]
    #[test]
    fn a_retired_link_contradicted_by_its_marker_is_a_user_hook_and_stays() {
        let tmp = tempfile::tempdir().unwrap();
        let hooks = tmp.path().join(".git/hooks");
        std::fs::create_dir_all(&hooks).unwrap();
        let theirs = tmp.path().join("their-tool/rift");
        let hook = hooks.join("post-merge");
        std::os::unix::fs::symlink(&theirs, &hook).unwrap();
        std::fs::write(
            link_marker(&hook),
            direct_link_marker_bytes(Path::new("/opt/rift/rift")),
        )
        .unwrap();

        let bin = fake_binary(&tmp.path().join("live"), "rift");
        let report = install_at(tmp.path(), &bin, repoint());

        assert!(report.removed.is_empty(), "{report:?}");
        assert_eq!(std::fs::read_link(&hook).unwrap(), theirs);
    }

    /// The user's hook goes back where it was. An install that met a
    /// `post-merge` of theirs chained it aside and ran it from the shim; with
    /// the shim gone, leaving it behind the suffix would end a hook they have
    /// always had, and end it silently.
    #[test]
    fn a_user_hook_chained_aside_under_a_retired_name_is_put_back() {
        let tmp = tempfile::tempdir().unwrap();
        let hooks = tmp.path().join(".git/hooks");
        std::fs::create_dir_all(&hooks).unwrap();
        std::fs::write(
            hooks.join(format!("post-merge{CHAINED_SUFFIX}")),
            "#!/bin/sh\necho theirs\n",
        )
        .unwrap();
        let bin = fake_binary(&tmp.path().join("live"), "rift");
        retired_managed_hook(&hooks, "post-merge", &bin);

        let report = install_at(tmp.path(), &bin, repoint());

        assert_eq!(report.removed, vec![hooks.join("post-merge")]);
        assert!(std::fs::read_to_string(hooks.join("post-merge"))
            .unwrap()
            .contains("echo theirs"));
        assert!(
            !exists_without_following(&hooks.join(format!("post-merge{CHAINED_SUFFIX}"))).unwrap(),
            "the suffix is not left holding a hook that runs again"
        );
    }

    /// A chained-aside user hook under a name Rift still installs belongs to
    /// the shim that runs it, and the sweep passes it by.
    #[cfg(unix)]
    #[test]
    fn a_chained_user_hook_under_a_live_name_is_not_swept() {
        let tmp = tempfile::tempdir().unwrap();
        let hooks = tmp.path().join(".git/hooks");
        std::fs::create_dir_all(&hooks).unwrap();
        // A dangling link to a file named `rift` is claimed as ours when it
        // stands at a hook's own name; behind the chained suffix it is the
        // user's, whatever it points at.
        let chained = hooks.join(format!("post-commit{CHAINED_SUFFIX}"));
        std::os::unix::fs::symlink(tmp.path().join("gone/rift"), &chained).unwrap();

        let bin = fake_binary(&tmp.path().join("live"), "rift");
        let report = install_at(tmp.path(), &bin, repoint());

        assert!(report.removed.is_empty(), "{report:?}");
        assert!(exists_without_following(&chained).unwrap());
    }
}
