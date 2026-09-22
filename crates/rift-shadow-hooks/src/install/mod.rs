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
//!
//! # The modules
//!
//! The act itself is here — [`install_hooks_with`], the policy it answers, the
//! report it returns and the on-disk vocabulary every part of the install
//! shares. The five siblings each own one question the act asks: `record`, what
//! this family's binary of record is and what it was; `screen`, whether a
//! binary is durable enough to point a family at; `entry`, whose hook an entry
//! already is, read without following it; `write`, the bytes an install puts on
//! disk; and `sweep`, the entries left under names the dispatch table no longer
//! carries.

mod entry;
mod record;
mod screen;
mod sweep;
mod write;

use std::path::PathBuf;

use rift_core::Result;

use entry::{hook_entry, link_target_exists, HookEntry};
use sweep::sweep_retired_hooks;
use write::write_hooks;

pub use entry::is_managed_hook;
pub use record::{
    binary_of_record_path, read_binary_of_record, record_binary_of_record, BinaryOfRecord,
    BinaryRecord, RecordedBy,
};
pub use screen::{is_executable_file, is_transient};
pub use sweep::is_retired_hook;
pub use write::shim_text;

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
/// [`RiftError::Io`](rift_core::RiftError::Io) on a filesystem failure —
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

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::entry::exists_without_following;
    #[cfg(unix)]
    use super::entry::link_marker;
    use super::*;

    pub(super) fn cfg(dir: &Path, hooks: Option<PathBuf>) -> HookInstall {
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
    pub(super) fn repoint() -> HookInstallPolicy {
        HookInstallPolicy::Repoint {
            allow_transient: true,
        }
    }

    /// An executable file standing in for a built `rift`.
    pub(super) fn fake_binary(dir: &Path, name: &str) -> PathBuf {
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

    pub(super) fn keep(allow_transient: bool) -> HookInstallPolicy {
        HookInstallPolicy::KeepIfHealthy { allow_transient }
    }

    /// Every fixture binary here lives in a temp dir, which is exactly what the
    /// screen refuses — so a fixture that means to install one says so.
    pub(super) fn install_at(
        dir: &Path,
        bin: &Path,
        policy: HookInstallPolicy,
    ) -> HookInstallReport {
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
}
