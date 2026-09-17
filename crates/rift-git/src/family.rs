//! The repository **family**: one identity for a repository and every linked
//! worktree of it.
//!
//! Rift used to key its two rebuildable stores by the canonical path of a
//! checkout. That was right while one path meant one repository, and wrong the
//! moment `git worktree add` gave the *same* repository a second path: the same
//! objects, the same checkpoint refs, the same hooks — but a second, empty
//! projection and a second, empty recall store.
//!
//! The unit of identity here is therefore the **common dir**, and the identity
//! is **minted once and read thereafter**:
//!
//! * [`resolve_family`] reads `<common_dir>/rift-family/family.json`. Present
//!   and parseable → [`FamilyState::Minted`]. Absent, under
//!   [`MintPolicy::Allow`] → it mints. Absent, under [`MintPolicy::Never`] →
//!   [`FamilyState::Unminted`]. Unparseable → [`FamilyState::Corrupt`], which
//!   is a `rift doctor` finding and never an auto-repair.
//! * Minting **adopts** the main work tree's historical ids rather than
//!   deriving new ones, so every projection and recall database already on
//!   disk is adopted in place: no row moves, no checksum changes, no rebuild.
//! * The main work tree is **proven, never guessed** (D2 rules a/b/c below). A
//!   vantage that cannot prove it mints **nothing** — the alternative is a
//!   `post-checkout` inside a brand-new member minting an identity over
//!   `…/.git/worktrees` and orphaning the real projection forever.
//! * After the mint the manifest is rewritten only by [`remint`], which appends
//!   its record to `family.history.jsonl` first. Nothing else writes it.
//!
//! Because the manifest travels with `.git`, a repository that **moves** keeps
//! its identity — under the old path rule a move orphaned the projection.
//!
//! **Canonicalization contract.** Every identity in Rift derives from
//! [`std::fs::canonicalize`] output bytes; the hashes themselves live in
//! [`rift_core::id::legacy`] and take an already-canonical path, so a path that
//! skipped that step cannot reach them by accident.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use rift_core::{RepoId, Result, RiftError};
use serde::{Deserialize, Serialize};

use crate::{discover, RepoPaths};

/// The manifest directory inside the common dir. Named `rift-family` rather
/// than `rift` so the `state-roots` gate leg, which enumerates `rift-*`
/// directory literals, sees it.
const FAMILY_DIR: &str = "rift-family";

/// The minted identity record.
const MANIFEST_FILE: &str = "family.json";

/// The append-only record of every mint and remint.
const HISTORY_FILE: &str = "family.history.jsonl";

/// The manifest's `schema_version`. A file that carries any other value is
/// [`FamilyState::Corrupt`]: this reader will not guess at a shape it does not
/// know.
const SCHEMA_VERSION: u32 = 1;

/// Relocates the manifest root, mirroring `RIFT_CACHE_DIR`. Set, the manifest
/// lands under `<override>/<key>/` instead of inside the real `.git`, which is
/// what lets a hermetic run mint without touching a repository it does not own.
pub const ENV_FAMILY_DIR: &str = "RIFT_FAMILY_DIR";

/// Domain byte for the **directory name** a `RIFT_FAMILY_DIR` override keys a
/// family's manifests by. A name, not an identity: it never keys a store, and
/// it is domained away from both id spaces so it cannot be mistaken for one.
const FAMILY_ROOT_DOMAIN: u8 = 0x03;

/// How many times a losing minter re-reads before giving up.
const MINT_ATTEMPTS: u8 = 3;

/// Where a family's ids came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MintedFrom {
    /// The main work tree's historical ids, adopted in place (the common case).
    AdoptedMainWorktree,
    /// Derived under this ADR's own domains, because the family has no work
    /// tree to adopt from.
    Fresh,
    /// Written by an explicit [`remint`], which also recorded why.
    Remint,
}

impl MintedFrom {
    /// The wire spelling, for a report line that should not re-invent it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AdoptedMainWorktree => "adopted-main-worktree",
            Self::Fresh => "fresh",
            Self::Remint => "remint",
        }
    }
}

impl core::fmt::Display for MintedFrom {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A repository family: the common dir, the ids it minted, and where they came
/// from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Family {
    /// The shared object/ref store — **canonical**, never the lexical
    /// `…/worktrees/x/../..` form discovery hands out.
    pub common_dir: PathBuf,
    /// The main work tree, canonical. `None` for a family with no checkout.
    pub main_work_tree: Option<PathBuf>,
    /// Keys `projection.db`.
    pub family_id: RepoId,
    /// Keys `recall.db`.
    pub recall_id: RepoId,
    /// Where the record was read from.
    pub manifest_path: PathBuf,
    /// How the ids were arrived at.
    pub minted_from: MintedFrom,
}

impl Family {
    /// The append-only record of this family's mints and remints, beside the
    /// manifest. It may not exist yet on a family minted by an older binary.
    #[must_use]
    pub fn history_path(&self) -> PathBuf {
        self.manifest_path.with_file_name(HISTORY_FILE)
    }
}

/// The family's ids, as the shared contract `rift-store`'s path layer
/// consumes. `rift-store` cannot name [`Family`] — it does not depend on
/// this crate, and the workspace graph stays acyclic — so the two crates meet
/// at [`rift_core::FamilyIds`], which only this type implements.
impl rift_core::FamilyIds for Family {
    fn family_id(&self) -> &RepoId {
        &self.family_id
    }

    fn recall_id(&self) -> &RepoId {
        &self.recall_id
    }
}

/// Whether a caller is allowed to create an identity it does not find.
///
/// Product paths that own the repository they run in (hooks, verbs, Canopy
/// writes) pass [`MintPolicy::Allow`]; library code and tests pass
/// [`MintPolicy::Never`] so a read can never have a write as a side effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MintPolicy {
    /// Mint when the manifest is absent and the main work tree is provable.
    Allow,
    /// Never write; report [`UnmintedReason::MintNotAllowed`] instead.
    Never,
}

/// Why a family has no identity yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnmintedReason {
    /// No manifest, and the caller passed [`MintPolicy::Never`].
    MintNotAllowed,
    /// No manifest, minting was allowed, and none of the D2 rules could prove
    /// the main work tree from this vantage. Nothing was written. The first
    /// `rift init` or verb run from the main work tree mints.
    MainUnprovable,
}

impl UnmintedReason {
    /// The wire spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MintNotAllowed => "mint-not-allowed",
            Self::MainUnprovable => "main-unprovable",
        }
    }
}

impl core::fmt::Display for UnmintedReason {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What [`resolve_family`] found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FamilyState {
    /// An identity, read from the manifest (possibly minted by this call).
    Minted(Family),
    /// No identity, and none was written.
    Unminted(UnmintedReason),
    /// A manifest exists but this reader cannot trust it. A doctor finding;
    /// hooks degrade to a no-op and it is never repaired on their behalf. The
    /// path is the manifest's, so the report can name it.
    Corrupt(PathBuf),
}

/// One member of a family: the main work tree, or a linked worktree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FamilyMember {
    /// A linked worktree's administrative name (`worktrees/<name>`), or the
    /// main work tree's final path component. Use [`FamilyMember::is_main`],
    /// never the name, to tell the main work tree apart: a linked worktree may
    /// legitimately be called anything.
    pub name: String,
    /// The checkout directory. Canonical where it exists; a `prunable`
    /// member's recorded path, verbatim, where it does not.
    pub work_tree: PathBuf,
    /// This member's gitdir: the common dir for the main work tree,
    /// `<common_dir>/worktrees/<name>` for a linked one.
    pub git_dir: PathBuf,
    /// Whether this is the family's main work tree.
    pub is_main: bool,
    /// The administrative entry survives but its checkout is gone — a
    /// `git worktree prune` candidate, reported rather than removed.
    pub prunable: bool,
}

// --- the manifest on disk ---------------------------------------------------

/// The manifest record, as written. Machine-local, never tracked, and never
/// carrying a timestamp: it is a pure function of the resolved main work tree,
/// so two mints of the same family agree byte for byte. When it happened is
/// `family.history.jsonl`'s job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    schema_version: u32,
    family_id: String,
    recall_id: String,
    common_dir: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    main_work_tree: Option<String>,
    minted_from: MintedFrom,
}

/// One line of `family.history.jsonl`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct HistoryRecord {
    schema_version: u32,
    /// `mint` or `remint`.
    event: String,
    /// Epoch milliseconds. Audit context only — nothing reads it back.
    at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
    family: Manifest,
}

impl Manifest {
    /// Rebuild the public shape. Invalid slugs make the record untrustworthy,
    /// which the caller turns into [`FamilyState::Corrupt`].
    fn into_family(self, manifest_path: PathBuf) -> Option<Family> {
        if self.schema_version != SCHEMA_VERSION {
            return None;
        }
        Some(Family {
            common_dir: PathBuf::from(self.common_dir),
            main_work_tree: self.main_work_tree.map(PathBuf::from),
            family_id: RepoId::new(self.family_id).ok()?,
            recall_id: RepoId::new(self.recall_id).ok()?,
            manifest_path,
            minted_from: self.minted_from,
        })
    }

    fn of(family: &Family) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            family_id: family.family_id.as_str().to_owned(),
            recall_id: family.recall_id.as_str().to_owned(),
            common_dir: family.common_dir.to_string_lossy().into_owned(),
            main_work_tree: family
                .main_work_tree
                .as_ref()
                .map(|p| p.to_string_lossy().into_owned()),
            minted_from: family.minted_from,
        }
    }
}

// --- where the manifest lives -----------------------------------------------

/// The manifest root for a **canonical** common dir, resolving
/// [`ENV_FAMILY_DIR`] from an injected environment lookup.
///
/// Unset, the manifest lives in the common dir itself, which is what makes it
/// travel with the repository. Set, it lives under the override, in a
/// directory named by a hash of the canonical common dir — so one override
/// root can hold many families without collision, and a hermetic run never
/// writes into a real `.git`.
#[must_use]
pub fn family_dir_under(
    env: impl Fn(&str) -> Option<String>,
    canonical_common_dir: &Path,
) -> PathBuf {
    env(ENV_FAMILY_DIR).map_or_else(
        || canonical_common_dir.join(FAMILY_DIR),
        |root| {
            PathBuf::from(root).join(rift_core::id::legacy::bare_slug(
                FAMILY_ROOT_DOMAIN,
                canonical_common_dir,
            ))
        },
    )
}

// --- main-worktree resolution -----------------------------------------------

/// What the resolution rules could prove about a family's main work tree.
#[derive(Debug, Clone, PartialEq, Eq)]
enum MainResolution {
    /// A rule proved this canonical work tree.
    WorkTree(PathBuf),
    /// `core.bare = true`: the family has no checkout to adopt.
    Bare,
    /// No rule applied from this vantage. Nothing may be minted.
    Unprovable,
}

/// Rule (a) — **the vantage**. `paths` itself describes a main work tree when
/// its gitdir *is* the common dir and a `.git` entry sits beside the work tree:
/// a `.git` directory (an ordinary main work tree) or a `.git` file whose
/// gitdir carries no `commondir` (a `--separate-git-dir` main, or a submodule —
/// a submodule is its own repository and so its own family).
fn vantage_is_main(paths: &RepoPaths, canonical_common_dir: &Path) -> bool {
    let Ok(git_dir) = std::fs::canonicalize(&paths.git_dir) else {
        return false;
    };
    if git_dir != canonical_common_dir {
        return false;
    }
    let dot_git = paths.work_tree.join(".git");
    if dot_git.is_file() {
        return true;
    }
    dot_git.is_dir()
        && std::fs::canonicalize(&dot_git).is_ok_and(|resolved| resolved == canonical_common_dir)
}

/// Rule (b) — `<common_dir>/config`. `core.bare = true` means no work tree;
/// `core.worktree` names one, relative to the common dir.
fn config_says_main(canonical_common_dir: &Path) -> Option<MainResolution> {
    let text = std::fs::read_to_string(canonical_common_dir.join("config")).ok()?;
    let core = CoreSection::parse(&text);
    if core.bare == Some(true) {
        return Some(MainResolution::Bare);
    }
    let declared = core.worktree?;
    let joined = canonical_common_dir.join(declared);
    std::fs::canonicalize(joined)
        .ok()
        .map(MainResolution::WorkTree)
}

/// Rule (c) — **the parent**. `canonical(common_dir).parent()` is the main work
/// tree only if its own `.git` resolves back to this common dir. The lexical
/// parent that `sibling_projections` used resolves
/// `…/.git/worktrees/canopy/../..` to `…/.git/worktrees`, which is the bug this
/// check exists to refuse.
fn parent_is_main(canonical_common_dir: &Path) -> Option<PathBuf> {
    let parent = canonical_common_dir.parent()?;
    let resolved = std::fs::canonicalize(parent.join(".git")).ok()?;
    (resolved == canonical_common_dir).then(|| parent.to_path_buf())
}

/// Resolve the main work tree by the D2 rules, in order. **Pure**: it reads the
/// filesystem and never writes.
fn resolve_main(paths: &RepoPaths, canonical_common_dir: &Path) -> MainResolution {
    if vantage_is_main(paths, canonical_common_dir) {
        if let Ok(work_tree) = std::fs::canonicalize(&paths.work_tree) {
            return MainResolution::WorkTree(work_tree);
        }
    }
    if let Some(found) = config_says_main(canonical_common_dir) {
        return found;
    }
    parent_is_main(canonical_common_dir)
        .map_or(MainResolution::Unprovable, MainResolution::WorkTree)
}

/// The two `[core]` keys rule (b) needs, read without spawning `git`.
///
/// A hook pays this read on every invocation, and the standing budget is 100 ms
/// of git latency with no daemon to amortise it — a process spawn per hook is not
/// affordable. Only `bare` and `worktree` are understood; anything else in the
/// file is skipped rather than misread.
#[derive(Debug, Default, PartialEq, Eq)]
struct CoreSection {
    bare: Option<bool>,
    worktree: Option<String>,
}

impl CoreSection {
    fn parse(text: &str) -> Self {
        let mut out = Self::default();
        let mut in_core = false;
        for raw in text.lines() {
            let line = raw.trim();
            if let Some(rest) = line.strip_prefix('[') {
                // `[core]` and `[core "sub"]`; a subsection is not `[core]`.
                let head = rest.split(']').next().unwrap_or("").trim();
                in_core = head.eq_ignore_ascii_case("core");
                continue;
            }
            if !in_core || line.is_empty() || line.starts_with('#') || line.starts_with(';') {
                continue;
            }
            let (key, value) = match line.split_once('=') {
                Some((k, v)) => (k.trim(), Some(strip_value(v))),
                // A bare key is the boolean true, which is how `bare` is often
                // written.
                None => (line, None),
            };
            if key.eq_ignore_ascii_case("bare") {
                out.bare = Some(value.as_deref().is_none_or(parse_bool));
            } else if key.eq_ignore_ascii_case("worktree") {
                out.worktree = value.filter(|v| !v.is_empty());
            }
        }
        out
    }
}

/// Strip an inline comment outside quotes, then the surrounding quotes.
fn strip_value(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut quoted = false;
    let mut escaped = false;
    for c in raw.chars() {
        if escaped {
            out.push(c);
            escaped = false;
            continue;
        }
        match c {
            '\\' if quoted => escaped = true,
            '"' => quoted = !quoted,
            '#' | ';' if !quoted => break,
            _ => out.push(c),
        }
    }
    out.trim().to_owned()
}

/// git's boolean spellings. Anything else is `false`, which for `core.bare`
/// means "keep looking", never "guess a work tree".
fn parse_bool(value: &str) -> bool {
    matches!(
        value.to_ascii_lowercase().as_str(),
        "true" | "yes" | "on" | "1"
    )
}

// --- resolution + mint ------------------------------------------------------

/// Resolve the family of a discovered worktree, minting one under
/// [`MintPolicy::Allow`] when none exists and the main work tree is provable.
///
/// # Errors
/// [`RiftError::Io`] if the common dir cannot be canonicalized, or if the
/// manifest exists but cannot be read (an unreadable file is a machine fault,
/// not a corrupt record), or if a permitted mint cannot be written.
pub fn resolve_family(paths: &RepoPaths, policy: MintPolicy) -> Result<FamilyState> {
    resolve_family_under(|k| std::env::var(k).ok(), paths, policy)
}

/// [`resolve_family`] with the environment injected — the seam
/// [`ENV_FAMILY_DIR`] is exercised through, so a test never has to mutate the
/// process environment its neighbours share.
///
/// # Errors
/// As [`resolve_family`].
pub fn resolve_family_under(
    env: impl Fn(&str) -> Option<String>,
    paths: &RepoPaths,
    policy: MintPolicy,
) -> Result<FamilyState> {
    let common_dir = std::fs::canonicalize(&paths.common_dir)?;
    let dir = family_dir_under(env, &common_dir);
    let manifest_path = dir.join(MANIFEST_FILE);

    match read_manifest(&manifest_path)? {
        Some(state) => Ok(state),
        None if policy == MintPolicy::Never => {
            Ok(FamilyState::Unminted(UnmintedReason::MintNotAllowed))
        }
        None => mint(paths, &common_dir, &dir, &manifest_path),
    }
}

/// Discover from `start`, then resolve its family.
///
/// # Errors
/// [`RiftError::NotFound`] when `start` is outside any repository; otherwise as
/// [`resolve_family`].
pub fn family_of(start: &Path, policy: MintPolicy) -> Result<(RepoPaths, FamilyState)> {
    let paths = discover(start)?;
    let state = resolve_family(&paths, policy)?;
    Ok((paths, state))
}

/// `Ok(None)` when the manifest is absent; `Ok(Some(Corrupt))` when it exists
/// but cannot be trusted; `Err` when the machine cannot read it at all.
fn read_manifest(manifest_path: &Path) -> Result<Option<FamilyState>> {
    let text = match std::fs::read_to_string(manifest_path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(RiftError::from(e)),
    };
    let parsed = serde_json::from_str::<Manifest>(&text)
        .ok()
        .and_then(|m| m.into_family(manifest_path.to_path_buf()));
    Ok(Some(parsed.map_or_else(
        || FamilyState::Corrupt(manifest_path.to_path_buf()),
        FamilyState::Minted,
    )))
}

/// Mint an identity for `common_dir`, or report why none was written.
fn mint(
    paths: &RepoPaths,
    common_dir: &Path,
    dir: &Path,
    manifest_path: &Path,
) -> Result<FamilyState> {
    let (main_work_tree, family_id, recall_id, minted_from) = match resolve_main(paths, common_dir)
    {
        MainResolution::Unprovable => {
            return Ok(FamilyState::Unminted(UnmintedReason::MainUnprovable))
        }
        MainResolution::WorkTree(main) => {
            let ids = adopted_ids(&main)?;
            (Some(main), ids.0, ids.1, MintedFrom::AdoptedMainWorktree)
        }
        MainResolution::Bare => {
            let ids = fresh_ids(common_dir)?;
            (None, ids.0, ids.1, MintedFrom::Fresh)
        }
    };
    let family = Family {
        common_dir: common_dir.to_path_buf(),
        main_work_tree,
        family_id,
        recall_id,
        manifest_path: manifest_path.to_path_buf(),
        minted_from,
    };
    write_first(dir, manifest_path, &family)
}

/// The main work tree's **historical** ids, byte for byte: adoption, not
/// migration. These are the ids `rift_git::repo_id_for` and
/// `rift_store::recall_repo_id` already mint for that path, so the projection
/// and recall databases on disk are adopted in place.
fn adopted_ids(canonical_main: &Path) -> Result<(RepoId, RepoId)> {
    Ok((
        RepoId::new(rift_core::id::legacy::capture_slug(canonical_main))?,
        RepoId::new(rift_core::id::legacy::recall_slug(canonical_main))?,
    ))
}

/// Ids for a family with no checkout to adopt: the common dir under this ADR's
/// own domains, so they can never collide with a checkout's ids.
fn fresh_ids(canonical_common_dir: &Path) -> Result<(RepoId, RepoId)> {
    use rift_core::id::legacy::{bare_slug, BARE_CAPTURE_DOMAIN, BARE_RECALL_DOMAIN};
    Ok((
        RepoId::new(bare_slug(BARE_CAPTURE_DOMAIN, canonical_common_dir))?,
        RepoId::new(bare_slug(BARE_RECALL_DOMAIN, canonical_common_dir))?,
    ))
}

/// Write the record with `create_new` semantics a concurrent minter cannot lose
/// data to: a complete temporary file is fsynced, then **hard-linked** into
/// place. `hard_link` refuses to clobber, so the final name only ever names a
/// complete file and a reader can never see a torn one. The loser unlinks its
/// temporary and re-reads the winner's record.
fn write_first(dir: &Path, manifest_path: &Path, family: &Family) -> Result<FamilyState> {
    let record = Manifest::of(family);
    let bytes = serde_json::to_vec_pretty(&record)
        .map_err(|e| RiftError::validity(format!("serialize family manifest: {e}")))?;
    for _ in 0..MINT_ATTEMPTS {
        std::fs::create_dir_all(dir)?;
        let tmp = dir.join(tmp_name());
        write_synced(&tmp, &bytes)?;
        match std::fs::hard_link(&tmp, manifest_path) {
            Ok(()) => {
                let _ = std::fs::remove_file(&tmp);
                append_history(dir, "mint", None, &record)?;
                return Ok(FamilyState::Minted(family.clone()));
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                let _ = std::fs::remove_file(&tmp);
                if let Some(state) = read_manifest(manifest_path)? {
                    return Ok(state);
                }
            }
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                return Err(RiftError::from(e));
            }
        }
    }
    Err(RiftError::validity(
        "family manifest could not be minted after three attempts",
    ))
}

/// `family.json.tmp-<pid>`, plus a per-process counter so two
/// threads of one process can never share a temporary name and truncate each
/// other's half-written record.
fn tmp_name() -> String {
    use core::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    format!(
        "{MANIFEST_FILE}.tmp-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

/// Write and fsync, so the bytes are durable before anything links to them.
fn write_synced(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = std::fs::File::create(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

/// Append one line to `family.history.jsonl`. Append-only, like everything else
/// that records what Rift did.
fn append_history(
    dir: &Path,
    event: &str,
    reason: Option<String>,
    record: &Manifest,
) -> Result<()> {
    let entry = HistoryRecord {
        schema_version: SCHEMA_VERSION,
        event: event.to_owned(),
        at_ms: rift_core::Timestamp::now().as_millis(),
        reason,
        family: record.clone(),
    };
    let line = serde_json::to_string(&entry)
        .map_err(|e| RiftError::validity(format!("serialize family history: {e}")))?;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(HISTORY_FILE))?;
    file.write_all(line.as_bytes())?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    Ok(())
}

// --- remint (the only post-mint writer) -------------------------------------

/// Which rule a [`remint`] re-derives the ids by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemintMode {
    /// Re-adopt the resolved main work tree's historical ids.
    AdoptMain,
    /// Derive fresh ids from the common dir.
    Fresh,
}

/// Replace a family's manifest, explicitly and on the record.
///
/// This is the **only** writer after the first mint: nothing remints as a side
/// effect, `rift doctor --fix` prints this command rather than running it, and
/// the record of every previous identity survives in `family.history.jsonl`.
///
/// # Errors
/// [`RiftError::Validity`] when [`RemintMode::AdoptMain`] is asked for and no
/// D2 rule can prove the main work tree from this vantage; [`RiftError::Io`] on
/// a filesystem failure.
pub fn remint(paths: &RepoPaths, mode: RemintMode, reason: &str) -> Result<Family> {
    remint_under(|k| std::env::var(k).ok(), paths, mode, reason)
}

/// [`remint`] with the environment injected.
///
/// # Errors
/// As [`remint`].
pub fn remint_under(
    env: impl Fn(&str) -> Option<String>,
    paths: &RepoPaths,
    mode: RemintMode,
    reason: &str,
) -> Result<Family> {
    let common_dir = std::fs::canonicalize(&paths.common_dir)?;
    let dir = family_dir_under(env, &common_dir);
    let manifest_path = dir.join(MANIFEST_FILE);

    let (main_work_tree, family_id, recall_id) = match mode {
        RemintMode::AdoptMain => match resolve_main(paths, &common_dir) {
            MainResolution::WorkTree(main) => {
                let ids = adopted_ids(&main)?;
                (Some(main), ids.0, ids.1)
            }
            MainResolution::Bare | MainResolution::Unprovable => {
                return Err(RiftError::validity(
                    "remint --adopt-main: the main work tree is not provable from here; \
                     run it from the main work tree, or use --fresh",
                ))
            }
        },
        RemintMode::Fresh => {
            let ids = fresh_ids(&common_dir)?;
            (None, ids.0, ids.1)
        }
    };
    let family = Family {
        common_dir,
        main_work_tree,
        family_id,
        recall_id,
        manifest_path: manifest_path.clone(),
        minted_from: MintedFrom::Remint,
    };
    let record = Manifest::of(&family);
    std::fs::create_dir_all(&dir)?;
    // History first: a crash between the two leaves a record of an identity
    // that was intended, which is recoverable. The reverse leaves a live
    // identity nothing recorded.
    append_history(&dir, "remint", Some(reason.to_owned()), &record)?;
    let bytes = serde_json::to_vec_pretty(&record)
        .map_err(|e| RiftError::validity(format!("serialize family manifest: {e}")))?;
    let tmp = dir.join(tmp_name());
    write_synced(&tmp, &bytes)?;
    std::fs::rename(&tmp, &manifest_path)?;
    Ok(family)
}

// --- members ----------------------------------------------------------------

/// Every member of the family `paths` belongs to: the main work tree first,
/// then each linked worktree by administrative name.
///
/// Deduplicated by canonical path, so a linked worktree that happens to point
/// at the main checkout is listed once. A member whose administrative entry
/// survives but whose checkout is gone is listed with `prunable` set — reported,
/// never removed, because deciding to prune is the operator's.
#[must_use]
pub fn family_members(paths: &RepoPaths) -> Vec<FamilyMember> {
    let Ok(common_dir) = std::fs::canonicalize(&paths.common_dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    if let MainResolution::WorkTree(main) = resolve_main(paths, &common_dir) {
        out.push(FamilyMember {
            name: member_name(&main),
            work_tree: main,
            git_dir: common_dir.clone(),
            is_main: true,
            prunable: false,
        });
    }
    out.extend(linked_members(&common_dir));

    let mut seen = std::collections::BTreeSet::new();
    out.retain(|m| seen.insert(canonical_or_lexical(&m.work_tree)));
    out
}

/// The administrative entries under `<common_dir>/worktrees`, in name order.
fn linked_members(common_dir: &Path) -> Vec<FamilyMember> {
    let Ok(entries) = std::fs::read_dir(common_dir.join("worktrees")) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let admin = entry.path();
        if !admin.is_dir() {
            continue;
        }
        let Some(name) = admin.file_name().map(|n| n.to_string_lossy().into_owned()) else {
            continue;
        };
        // The `gitdir` file holds the absolute path of the member's `.git`
        // FILE; its parent is the checkout. Unreadable means the entry names
        // no path at all, and a row without a path would be an invented one.
        let Ok(text) = std::fs::read_to_string(admin.join("gitdir")) else {
            continue;
        };
        let Some(work_tree) = crate::gitdir_text_path(&text)
            .parent()
            .map(Path::to_path_buf)
        else {
            continue;
        };
        let prunable = !work_tree.is_dir();
        out.push(FamilyMember {
            name,
            work_tree: if prunable {
                work_tree
            } else {
                canonical_or_lexical(&work_tree)
            },
            git_dir: admin,
            is_main: false,
            prunable,
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// The canonical path where it resolves, the given one where it does not — a
/// gone checkout still has to be nameable.
#[must_use]
pub fn canonical_or_lexical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// A member's display name: its final path component, or the whole path when it
/// has none.
fn member_name(work_tree: &Path) -> String {
    work_tree.file_name().map_or_else(
        || work_tree.to_string_lossy().into_owned(),
        |n| n.to_string_lossy().into_owned(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{fake_gitdir, fake_repo, fake_worktree};

    /// No `RIFT_FAMILY_DIR`: the manifest lands inside the fixture's own `.git`,
    /// which is what makes these tests hermetic without an override.
    fn no_env(_: &str) -> Option<String> {
        None
    }

    /// A main work tree at `<root>/main`, discovered.
    fn main_repo(root: &Path) -> RepoPaths {
        fake_repo(&root.join("main")).unwrap();
        discover(&root.join("main")).unwrap()
    }

    /// A linked member of `main`, discovered from inside itself.
    fn member(root: &Path, main: &RepoPaths, name: &str) -> RepoPaths {
        let work_tree = root.join(name);
        fake_worktree(&main.common_dir, &work_tree, name).unwrap();
        discover(&work_tree).unwrap()
    }

    fn minted(state: FamilyState) -> Family {
        match state {
            FamilyState::Minted(f) => f,
            other => panic!("expected a minted family, got {other:?}"),
        }
    }

    fn history_lines(family: &Family) -> Vec<String> {
        std::fs::read_to_string(family.history_path())
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn tmp_files(dir: &Path) -> Vec<String> {
        std::fs::read_dir(dir)
            .map(|entries| {
                entries
                    .flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .filter(|n| n.contains(".tmp-"))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The whole point: a second path to one repository is a **member** of it,
    /// not a second repository. The member reads the identity the main work
    /// tree minted — same ids, same manifest — without minting anything of its
    /// own.
    #[test]
    fn a_linked_worktree_resolves_to_its_family() {
        let tmp = tempfile::tempdir().unwrap();
        let main = main_repo(tmp.path());
        let from_main = minted(resolve_family_under(no_env, &main, MintPolicy::Allow).unwrap());

        let wt = member(tmp.path(), &main, "feature");
        let from_member = minted(resolve_family_under(no_env, &wt, MintPolicy::Never).unwrap());

        assert_eq!(from_member.family_id, from_main.family_id);
        assert_eq!(from_member.recall_id, from_main.recall_id);
        assert_eq!(from_member.common_dir, from_main.common_dir);
        assert_eq!(from_member.manifest_path, from_main.manifest_path);
        assert_eq!(from_member.main_work_tree, from_main.main_work_tree);
        assert_eq!(
            history_lines(&from_main).len(),
            1,
            "the member minted nothing"
        );
    }

    /// Adoption, not migration. The minted ids are the bytes the main work
    /// tree's own derivations produce, so `~/.cache/rift/<id>/projection.db`
    /// and `recall.db` are adopted in place: no row moves and nothing rebuilds.
    #[test]
    fn mint_adopts_the_main_worktrees_legacy_ids_byte_for_byte() {
        let tmp = tempfile::tempdir().unwrap();
        let main = main_repo(tmp.path());
        let family = minted(resolve_family_under(no_env, &main, MintPolicy::Allow).unwrap());

        let canonical = std::fs::canonicalize(&main.work_tree).unwrap();
        assert_eq!(
            family.family_id,
            crate::repo_id_for(&main.work_tree).unwrap(),
            "the capture id is the one this checkout already keys its projection by"
        );
        assert_eq!(
            family.recall_id.as_str(),
            rift_core::id::legacy::recall_slug(&canonical),
            "the recall id is the one rift-store already keys recall.db by"
        );
        assert_eq!(family.minted_from, MintedFrom::AdoptedMainWorktree);
        assert_eq!(family.main_work_tree, Some(canonical));
    }

    /// Rule (c). From inside a member, the vantage proves nothing and there is
    /// no `core.worktree`, so the main work tree is the common dir's parent —
    /// accepted only because that parent's own `.git` resolves back to this
    /// common dir. Without that check the lexical parent of
    /// `…/.git/worktrees/x/../..` is `…/.git/worktrees`, and minting over it
    /// would orphan the real projection.
    #[test]
    fn a_mint_from_a_linked_member_proves_main_by_its_parent_dot_git() {
        let tmp = tempfile::tempdir().unwrap();
        let main = main_repo(tmp.path());
        let wt = member(tmp.path(), &main, "feature");

        let family = minted(resolve_family_under(no_env, &wt, MintPolicy::Allow).unwrap());
        assert_eq!(
            family.main_work_tree,
            Some(std::fs::canonicalize(&main.work_tree).unwrap()),
            "the member proved main by its parent's .git, not by its own path"
        );
        assert_eq!(
            family.family_id,
            crate::repo_id_for(&main.work_tree).unwrap()
        );
        assert_eq!(family.minted_from, MintedFrom::AdoptedMainWorktree);
    }

    /// A vantage that cannot prove the main work tree writes **nothing**. The
    /// cost of waiting is one deferred mint; the cost of guessing is an
    /// identity nobody can find their data under.
    #[test]
    fn a_mint_from_an_unprovable_vantage_writes_nothing_and_reads_unminted() {
        let tmp = tempfile::tempdir().unwrap();
        // A common dir that is not named `.git` and is not its parent's `.git`,
        // with no `core.worktree`: no rule can reach a work tree from a member
        // of it.
        let common = fake_gitdir(&tmp.path().join("gitstore").join("main.git")).unwrap();
        let work_tree = tmp.path().join("wt");
        fake_worktree(&common, &work_tree, "wt").unwrap();

        let paths = discover(&work_tree).unwrap();
        let state = resolve_family_under(no_env, &paths, MintPolicy::Allow).unwrap();

        assert_eq!(state, FamilyState::Unminted(UnmintedReason::MainUnprovable));
        assert!(
            !common.join(FAMILY_DIR).exists(),
            "an unprovable vantage left a manifest directory behind"
        );
    }

    /// The loser of a mint race adopts the winner's record rather than its own,
    /// and takes its temporary file with it. Driven deterministically: the
    /// winner's file is planted between the read and the link, which is the
    /// only window the race has and the one a two-process test can only reach
    /// by luck.
    #[test]
    fn a_second_minter_loses_the_race_and_reads_the_winner() {
        let tmp = tempfile::tempdir().unwrap();
        let main = main_repo(tmp.path());
        let winner = minted(resolve_family_under(no_env, &main, MintPolicy::Allow).unwrap());

        let dir = winner.manifest_path.parent().unwrap().to_path_buf();
        let ours = Family {
            family_id: RepoId::new("0000000000000000000000000000dead").unwrap(),
            ..winner.clone()
        };
        let state = write_first(&dir, &winner.manifest_path, &ours).unwrap();

        assert_eq!(
            minted(state).family_id,
            winner.family_id,
            "the loser adopted the winner's identity, not its own"
        );
        assert!(
            tmp_files(&dir).is_empty(),
            "the loser's temporary file is gone"
        );
        assert_eq!(
            history_lines(&winner).len(),
            1,
            "only the winner recorded a mint"
        );
    }

    /// Two threads minting one family agree on one identity. The outcome is
    /// deterministic even though which thread wins is not.
    #[test]
    fn two_threads_minting_one_family_agree_on_one_identity() {
        let tmp = tempfile::tempdir().unwrap();
        let main = main_repo(tmp.path());
        let a = main.clone();
        let b = main.clone();
        let left = std::thread::spawn(move || resolve_family_under(no_env, &a, MintPolicy::Allow));
        let right = std::thread::spawn(move || resolve_family_under(no_env, &b, MintPolicy::Allow));
        let left = minted(left.join().unwrap().unwrap());
        let right = minted(right.join().unwrap().unwrap());

        assert_eq!(left.family_id, right.family_id);
        assert_eq!(left.recall_id, right.recall_id);
        assert!(tmp_files(left.manifest_path.parent().unwrap()).is_empty());
    }

    /// A manifest this reader cannot trust is a finding, never a repair. The
    /// hook path degrades to writing nothing at all — including not "fixing"
    /// the file, because a torn record may be the only trace of an identity
    /// somebody's data is still filed under.
    #[test]
    fn a_torn_manifest_reads_corrupt_and_a_hook_exits_zero_writing_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let main = main_repo(tmp.path());
        let family = minted(resolve_family_under(no_env, &main, MintPolicy::Allow).unwrap());

        let torn = {
            let whole = std::fs::read_to_string(&family.manifest_path).unwrap();
            whole.get(..whole.len() / 2).unwrap_or_default().to_owned()
        };
        std::fs::write(&family.manifest_path, &torn).unwrap();

        let state = resolve_family_under(no_env, &main, MintPolicy::Allow).unwrap();
        assert_eq!(state, FamilyState::Corrupt(family.manifest_path.clone()));
        assert_eq!(
            std::fs::read_to_string(&family.manifest_path).unwrap(),
            torn,
            "a corrupt manifest is left exactly as found"
        );
        assert_eq!(history_lines(&family).len(), 1, "nothing was recorded");
    }

    /// A schema version this reader does not know is corrupt for the same
    /// reason: guessing at an unknown shape is how an identity gets silently
    /// replaced.
    #[test]
    fn an_unknown_schema_version_reads_corrupt() {
        let tmp = tempfile::tempdir().unwrap();
        let main = main_repo(tmp.path());
        let family = minted(resolve_family_under(no_env, &main, MintPolicy::Allow).unwrap());
        let bumped = std::fs::read_to_string(&family.manifest_path)
            .unwrap()
            .replace("\"schema_version\": 1", "\"schema_version\": 2");
        std::fs::write(&family.manifest_path, bumped).unwrap();

        assert_eq!(
            resolve_family_under(no_env, &main, MintPolicy::Never).unwrap(),
            FamilyState::Corrupt(family.manifest_path)
        );
    }

    /// A family with no checkout has nothing to adopt, so it derives its ids
    /// from the common dir under domains of its own — never reusing a
    /// checkout's domain over a different kind of path.
    #[test]
    fn a_bare_common_dir_mints_fresh_ids() {
        use rift_core::id::legacy::{bare_slug, BARE_CAPTURE_DOMAIN, BARE_RECALL_DOMAIN};
        let tmp = tempfile::tempdir().unwrap();
        let bare = fake_gitdir(&tmp.path().join("repo.git")).unwrap();
        std::fs::write(bare.join("config"), "[core]\n\tbare = true\n").unwrap();
        let paths = RepoPaths {
            git_dir: bare.clone(),
            common_dir: bare.clone(),
            work_tree: bare.clone(),
            worktree_hash: [0u8; 6],
        };

        let family = minted(resolve_family_under(no_env, &paths, MintPolicy::Allow).unwrap());
        let canonical = std::fs::canonicalize(&bare).unwrap();
        assert_eq!(family.minted_from, MintedFrom::Fresh);
        assert_eq!(family.main_work_tree, None);
        assert_eq!(
            family.family_id.as_str(),
            bare_slug(BARE_CAPTURE_DOMAIN, &canonical)
        );
        assert_eq!(
            family.recall_id.as_str(),
            bare_slug(BARE_RECALL_DOMAIN, &canonical)
        );
    }

    /// Two shapes that look like members and are not: a `--separate-git-dir`
    /// main and a submodule each have a `.git` file with no `commondir`, and a
    /// submodule is its own repository. Each is its own family, with its own
    /// ids.
    #[test]
    fn a_separate_git_dir_main_and_a_submodule_each_mint_their_own_family() {
        let tmp = tempfile::tempdir().unwrap();

        let app = tmp.path().join("app");
        let elsewhere = fake_gitdir(&tmp.path().join("gitdirs").join("app")).unwrap();
        std::fs::create_dir_all(&app).unwrap();
        std::fs::write(
            app.join(".git"),
            format!("gitdir: {}\n", elsewhere.display()),
        )
        .unwrap();

        let parent = tmp.path().join("parent");
        fake_repo(&parent).unwrap();
        let sub = parent.join("sub");
        let modules = fake_gitdir(&parent.join(".git").join("modules").join("sub")).unwrap();
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join(".git"), format!("gitdir: {}\n", modules.display())).unwrap();

        let separate = minted(
            resolve_family_under(no_env, &discover(&app).unwrap(), MintPolicy::Allow).unwrap(),
        );
        let submodule = minted(
            resolve_family_under(no_env, &discover(&sub).unwrap(), MintPolicy::Allow).unwrap(),
        );
        let outer = minted(
            resolve_family_under(no_env, &discover(&parent).unwrap(), MintPolicy::Allow).unwrap(),
        );

        assert_eq!(
            separate.main_work_tree,
            Some(std::fs::canonicalize(&app).unwrap())
        );
        assert_eq!(
            submodule.main_work_tree,
            Some(std::fs::canonicalize(&sub).unwrap())
        );
        assert_ne!(
            submodule.family_id, outer.family_id,
            "a submodule is not its parent"
        );
        assert_ne!(separate.family_id, submodule.family_id);
    }

    /// The manifest travels with `.git`, so a repository that moves keeps its
    /// identity. Under the path rule it lost it — the projection stayed behind
    /// under the old path's slug and the new path opened an empty one.
    #[test]
    fn a_moved_repository_keeps_its_ids() {
        let tmp = tempfile::tempdir().unwrap();
        let before = minted(
            resolve_family_under(no_env, &main_repo(tmp.path()), MintPolicy::Allow).unwrap(),
        );

        let moved = tmp.path().join("moved");
        std::fs::rename(tmp.path().join("main"), &moved).unwrap();
        let after = minted(
            resolve_family_under(no_env, &discover(&moved).unwrap(), MintPolicy::Never).unwrap(),
        );

        assert_eq!(after.family_id, before.family_id);
        assert_eq!(after.recall_id, before.recall_id);
        assert_ne!(
            after.family_id,
            crate::repo_id_for(&moved).unwrap(),
            "the id is read, not re-derived — that is what surviving a move means"
        );
    }

    /// `RIFT_FAMILY_DIR` relocates the manifest root, so a hermetic run mints
    /// without writing one byte into a `.git` it does not own.
    #[test]
    fn rift_family_dir_keeps_a_hermetic_run_out_of_the_real_git_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let main = main_repo(tmp.path());
        let root_path = root.path().to_string_lossy().into_owned();
        let env = |k: &str| (k == ENV_FAMILY_DIR).then(|| root_path.clone());

        let family = minted(resolve_family_under(env, &main, MintPolicy::Allow).unwrap());

        assert!(
            family.manifest_path.starts_with(root.path()),
            "the manifest landed under the override: {}",
            family.manifest_path.display()
        );
        assert!(
            !main.common_dir.join(FAMILY_DIR).exists(),
            "the real .git was written to anyway"
        );
        // Keyed by the common dir, so one override root holds many families.
        let other = main_repo(&tmp.path().join("second"));
        let second = minted(resolve_family_under(env, &other, MintPolicy::Allow).unwrap());
        assert_ne!(second.manifest_path, family.manifest_path);
    }

    /// `MintPolicy::Never` is a read: it reports the absence rather than
    /// filling it, so library code and tests cannot write an identity into a
    /// repository they were only looking at.
    #[test]
    fn a_read_only_policy_reports_the_absence_instead_of_filling_it() {
        let tmp = tempfile::tempdir().unwrap();
        let main = main_repo(tmp.path());
        assert_eq!(
            resolve_family_under(no_env, &main, MintPolicy::Never).unwrap(),
            FamilyState::Unminted(UnmintedReason::MintNotAllowed)
        );
        assert!(!main.common_dir.join(FAMILY_DIR).exists());
    }

    /// The canonicalization contract, asserted rather than assumed: the ids
    /// come from `canonicalize` output bytes, never from the path the caller
    /// happened to type. A lexically different route to the same directory
    /// mints the same identity.
    #[test]
    fn every_identity_derives_from_canonicalize_output() {
        let tmp = tempfile::tempdir().unwrap();
        let main = main_repo(tmp.path());
        std::fs::create_dir_all(main.work_tree.join("sub")).unwrap();
        let roundabout = main.work_tree.join("sub").join("..");

        let straight = minted(resolve_family_under(no_env, &main, MintPolicy::Allow).unwrap());
        let scenic = minted(
            resolve_family_under(no_env, &discover(&roundabout).unwrap(), MintPolicy::Never)
                .unwrap(),
        );

        assert_eq!(scenic.family_id, straight.family_id);
        assert_eq!(
            straight.family_id.as_str(),
            rift_core::id::legacy::capture_slug(&std::fs::canonicalize(&main.work_tree).unwrap())
        );
        assert_ne!(
            straight.family_id.as_str(),
            rift_core::id::legacy::capture_slug(&roundabout),
            "a lexical path must not be able to mint an identity of its own"
        );
    }

    /// The two `[core]` keys rule (b) reads, without spawning `git`: a bare
    /// flag, quoted and commented values, and a subsection that is not `core`.
    #[test]
    fn the_core_config_reader_understands_only_what_it_claims_to() {
        let core = CoreSection::parse(
            "[core]\n\tbare\n\tworktree = \"/a b/c\" # trailing\n[core \"sub\"]\n\tbare = false\n",
        );
        assert_eq!(core.bare, Some(true), "a bare key is the boolean true");
        assert_eq!(core.worktree.as_deref(), Some("/a b/c"));

        let off = CoreSection::parse("[core]\nbare = false\n");
        assert_eq!(off.bare, Some(false));
        assert_eq!(off.worktree, None);

        let elsewhere = CoreSection::parse("[remote \"origin\"]\n\tworktree = /nope\n");
        assert_eq!(elsewhere, CoreSection::default());
    }

    /// `core.worktree` names the main work tree relative to the common dir,
    /// which is rule (b) — reached before the parent rule, so a repository
    /// that says where its checkout is is believed.
    #[test]
    fn core_worktree_names_the_main_work_tree() {
        let tmp = tempfile::tempdir().unwrap();
        let store = fake_gitdir(&tmp.path().join("store").join("git")).unwrap();
        let checkout = tmp.path().join("checkout");
        std::fs::create_dir_all(&checkout).unwrap();
        std::fs::write(
            store.join("config"),
            "[core]\n\tworktree = ../../checkout\n",
        )
        .unwrap();
        let paths = RepoPaths {
            git_dir: store.clone(),
            common_dir: store,
            work_tree: tmp.path().join("nowhere"),
            worktree_hash: [0u8; 6],
        };

        let family = minted(resolve_family_under(no_env, &paths, MintPolicy::Allow).unwrap());
        assert_eq!(
            family.main_work_tree,
            Some(std::fs::canonicalize(&checkout).unwrap())
        );
    }

    /// After the first mint, nothing rewrites the manifest as a side effect —
    /// resolving again reads it. `remint` is the one writer, and it records
    /// what it did before it does it.
    #[test]
    fn remint_appends_history_and_is_the_only_post_mint_writer() {
        let tmp = tempfile::tempdir().unwrap();
        let main = main_repo(tmp.path());
        let family = minted(resolve_family_under(no_env, &main, MintPolicy::Allow).unwrap());
        let bytes = std::fs::read(&family.manifest_path).unwrap();

        for _ in 0..3 {
            let _ = resolve_family_under(no_env, &main, MintPolicy::Allow).unwrap();
        }
        assert_eq!(std::fs::read(&family.manifest_path).unwrap(), bytes);
        assert_eq!(history_lines(&family).len(), 1);

        let after = remint_under(no_env, &main, RemintMode::Fresh, "operator asked").unwrap();
        assert_ne!(after.family_id, family.family_id);
        assert_eq!(after.minted_from, MintedFrom::Remint);
        let lines = history_lines(&family);
        assert_eq!(lines.len(), 2, "the remint appended, it did not replace");
        assert!(lines[0].contains("\"event\":\"mint\""));
        assert!(lines[1].contains("\"event\":\"remint\""));
        assert!(lines[1].contains("operator asked"));
        assert!(tmp_files(after.manifest_path.parent().unwrap()).is_empty());
    }

    /// `remint --adopt-main` refuses from a vantage that cannot prove the main
    /// work tree, rather than reaching for the fresh rule on the operator's
    /// behalf.
    #[test]
    fn remint_adopt_main_refuses_an_unprovable_vantage() {
        let tmp = tempfile::tempdir().unwrap();
        let common = fake_gitdir(&tmp.path().join("gitstore").join("main.git")).unwrap();
        let work_tree = tmp.path().join("wt");
        fake_worktree(&common, &work_tree, "wt").unwrap();
        let paths = discover(&work_tree).unwrap();

        assert!(remint_under(no_env, &paths, RemintMode::AdoptMain, "try").is_err());
        assert!(!common.join(FAMILY_DIR).join(MANIFEST_FILE).exists());
    }

    /// The family's members: the main work tree first, then each linked
    /// worktree by name. A member whose checkout is gone is still listed —
    /// reported as prunable, because removing it is the operator's call.
    #[test]
    fn family_members_include_main_and_mark_prunable() {
        let tmp = tempfile::tempdir().unwrap();
        let main = main_repo(tmp.path());
        member(tmp.path(), &main, "beta");
        member(tmp.path(), &main, "alpha");
        let doomed = member(tmp.path(), &main, "gone");
        std::fs::remove_dir_all(&doomed.work_tree).unwrap();

        let members = family_members(&main);
        let names: Vec<&str> = members.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, vec!["main", "alpha", "beta", "gone"]);
        assert!(members[0].is_main, "the main work tree leads the list");
        assert_eq!(
            members[0].git_dir,
            std::fs::canonicalize(&main.common_dir).unwrap()
        );
        assert!(members[1..].iter().all(|m| !m.is_main));
        assert!(
            members[1..3].iter().all(|m| !m.prunable),
            "a live member is not a prune candidate"
        );
        assert!(
            members[3].prunable,
            "a member whose checkout is gone is prunable"
        );
        assert!(
            members[3].work_tree.ends_with("gone"),
            "and is still nameable"
        );
    }

    /// Enumerated from a member's vantage, the family has the same membership:
    /// the list is a property of the family, not of where you stand in it.
    #[test]
    fn members_are_the_same_seen_from_any_member() {
        let tmp = tempfile::tempdir().unwrap();
        let main = main_repo(tmp.path());
        let wt = member(tmp.path(), &main, "feature");
        assert_eq!(family_members(&wt), family_members(&main));
    }
}
