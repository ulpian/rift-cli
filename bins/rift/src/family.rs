//! `rift family` — the repository family's minted identity.
//!
//! A family's ids are minted once, into `<common_dir>/rift-family/family.json`,
//! and read thereafter: that is what lets a repository move without orphaning
//! its projection, and what stops a linked worktree from becoming a second
//! repository. Everything else in Rift reads that record.
//!
//! This module is the **only** surface that writes it after the first mint, and
//! only when asked in so many words: `--yes`, plus the rule to re-derive by.
//! `rift doctor --fix` prints the command instead of running it, because a
//! recorded identity that can change as a side effect is not a record.

pub(crate) mod run_state;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use rift_core::{Result, RiftError};
use rift_git::{Family, FamilyState, MintPolicy, RemintMode};

use crate::cli::{exit, FamilyCmd, Outcome, RemintArgs};

// --- the product path's identity chokepoint --------------------------------

/// Resolved families, memoized for the life of this process.
///
/// A single agent hook resolves the same family half a dozen times — the
/// ingest, the injector's three preconditions, the audit sink — and the
/// manifest cannot change underneath a process that lives ten milliseconds.
/// This is what keeps the family lookup at *one* small JSON read per hook rather
/// than one per call site, which is the budget the `hook-prompt-submit` row
/// measures.
///
/// **Only successes are memoized.** A `None` — outside a repository, an
/// unprovable main work tree, a corrupt manifest — is re-resolved every time,
/// so a long-lived reader (Canopy) picks a family up the moment the first
/// `rift init` mints it. The one staleness left is a [`remint`] from *another*
/// process while this one runs; `remint` clears the memo it can see, and a
/// remint is an explicit, historied repair whose whole point is that the reader
/// is told about it.
fn memo() -> &'static Mutex<BTreeMap<PathBuf, Family>> {
    static MEMO: OnceLock<Mutex<BTreeMap<PathBuf, Family>>> = OnceLock::new();
    MEMO.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// The family `root` belongs to, minting when this vantage can prove the main
/// work tree ([`MintPolicy::Allow`] — `root` is a repository this product owns).
///
/// This is the one resolution every product storage path in this binary goes
/// through. The failure arms are the ADR's, spelled as the refusal the user
/// needs rather than as a fresh per-path database:
///
/// * outside any repository → [`RiftError::NotFound`]; there is no store here,
///   and minting one under whatever directory the shell was standing in is the
///   phantom-identity defect the family model retired.
/// * no manifest and the main work tree unprovable from here → the same, with
///   the command that fixes it. A `post-checkout` inside a brand-new linked
///   worktree must not mint `repo_id_for("<repo>/.git/worktrees")` and orphan
///   the real projection forever.
/// * a manifest this reader cannot trust → [`RiftError::Validity`], naming the
///   file and `rift family remint`. Never repaired on the caller's behalf.
///
/// # Errors
/// The three arms above, and any I/O the family layer could not complete.
pub(crate) fn resolve_at(root: &Path) -> Result<Family> {
    if let Some(found) = memo().lock().ok().and_then(|m| m.get(root).cloned()) {
        return Ok(found);
    }
    let resolved = resolve_uncached(root)?;
    if let Ok(mut m) = memo().lock() {
        m.insert(root.to_path_buf(), resolved.clone());
    }
    Ok(resolved)
}

fn resolve_uncached(root: &Path) -> Result<Family> {
    let Ok(paths) = rift_git::discover(root) else {
        return Err(RiftError::NotFound(format!(
            "not inside a git repository: {} — Rift records nothing outside one",
            root.display()
        )));
    };
    match rift_git::resolve_family(&paths, MintPolicy::Allow)? {
        FamilyState::Minted(family) => Ok(family),
        FamilyState::Unminted(reason) => Err(RiftError::NotFound(format!(
            "{} has no recorded Rift identity yet ({reason}) — run `rift init` from \
             the main work tree of this repository, which is the vantage that can \
             prove it",
            root.display()
        ))),
        FamilyState::Corrupt(manifest) => {
            Err(RiftError::Validity(corrupt_manifest_refusal(&manifest)))
        }
    }
}

/// The refusal a manifest this reader cannot trust earns, spelled once so the
/// product path's error and the doctor's identity leg say the same sentence.
pub(crate) fn corrupt_manifest_refusal(manifest: &Path) -> String {
    format!(
        "the family manifest at {} cannot be read; nothing was repaired. Run \
         `rift family remint --adopt-main --yes` from the main work tree to \
         record the identity again",
        manifest.display()
    )
}

/// The family for a path that must never fail loudly — the agent-hook path.
///
/// `None` is not an error: a hook that cannot resolve an identity records
/// **nothing** and still exits 0 (a hook never blocks an agent or git —
/// ever). The caller is where "record nothing" can be spelled.
pub(crate) fn try_at(root: &Path) -> Option<Family> {
    resolve_at(root).ok()
}

/// This family's capture projection (`projection.db`) — the file every member
/// of the repository shares.
///
/// # Errors
/// Propagates [`resolve_at`].
pub(crate) fn projection_db(root: &Path) -> Result<PathBuf> {
    Ok(projection_db_of(&resolve_at(root)?))
}

/// The family's own directory under [`cache_root`], where its capture
/// projection and its hook-timing ring live.
pub(crate) fn family_dir_of(family: &Family) -> PathBuf {
    rift_store::projection_dir_under(&cache_root(), &family.family_id)
}

/// The family directory of the repository at `root` without minting: `None`
/// outside a repository or before the family has an identity. For the hook
/// paths and read-only reports that must never create one.
pub(crate) fn family_dir_here(root: &Path) -> Option<PathBuf> {
    family_dir_of_paths(&rift_git::discover(root).ok()?)
}

/// [`family_dir_here`] for a repository already discovered: one manifest
/// read, never a mint.
pub(crate) fn family_dir_of_paths(paths: &rift_git::RepoPaths) -> Option<PathBuf> {
    match rift_git::resolve_family(paths, MintPolicy::Never) {
        Ok(FamilyState::Minted(family)) => Some(family_dir_of(&family)),
        _ => None,
    }
}

/// The capture projection of a family already resolved, under [`cache_root`].
pub(crate) fn projection_db_of(family: &Family) -> PathBuf {
    rift_store::projection_db_for_family_under(&cache_root(), family)
}

/// Whether the repository at `root` is set up **on this machine**: its
/// family's capture projection exists under this machine's state root.
///
/// This is the one definition of "initialized" the CLI reads. It used to be
/// "`.rift/` is a directory", and `.rift/config.toml` is committed
/// configuration that travels with a clone — so a fresh checkout on a second
/// machine read as initialized, every repo-scoped command ran against a
/// projection that did not exist, and the setup that would have wired this
/// machine's agents never ran. Never mints: a read-only question must not
/// create the identity it asks about.
pub(crate) fn provisioned_here(root: &Path) -> bool {
    matches!(
        rift_git::family_of(root, MintPolicy::Never),
        Ok((_, FamilyState::Minted(family))) if projection_db_of(&family).exists()
    )
}

/// This family's recall projection (`recall.db`), keyed by the family's recall
/// id — a second id space from [`projection_db`]'s, as it has always been.
///
/// # Errors
/// Propagates [`resolve_at`].
pub(crate) fn recall_db(root: &Path) -> Result<PathBuf> {
    Ok(recall_db_of(&resolve_at(root)?))
}

/// The recall projection of a family already resolved, under [`cache_root`].
pub(crate) fn recall_db_of(family: &Family) -> PathBuf {
    rift_store::recall_db_for_family_under(&cache_root(), family)
}

/// The machine's cache root: the one place this binary resolves it.
///
/// Every state path the binary derives — the family's projections, the
/// machine-id cache, the install salt's file, the Canopy store — starts here,
/// and a `ci-meta` screen holds every other spelling out of this crate's
/// production code. That is what makes the test build's root below total: an
/// in-process test reaches the machine's cache only through this function,
/// however many calls lie between the test and the store it opens.
///
/// A release build resolves exactly as `rift_store::resolve_cache_root` reads
/// the process environment: `RIFT_CACHE_DIR`, then `XDG_CACHE_HOME`, then the
/// platform default.
#[cfg(not(test))]
pub(crate) fn cache_root() -> PathBuf {
    cache_root_from(|key| std::env::var(key).ok())
}

/// The test build's cache root: one directory per test process, created once
/// under the system temp dir and never the developer's real cache.
///
/// Tests run as threads of one process, so a per-test root would need the
/// root threaded through every production path a test reaches; one root per
/// process keeps the production signatures as they are. Tests stay apart
/// because every store under it is keyed by a family minted in the test's own
/// temp repository. Setting `RIFT_CACHE_DIR` from a test instead would race
/// every other thread reading the environment. `cfg(test)` is set only when
/// this crate's own unit tests are compiled, so no build of the `rift` binary
/// — release, debug, or the one integration tests spawn — contains this body.
#[cfg(test)]
pub(crate) fn cache_root() -> PathBuf {
    static ROOT: OnceLock<PathBuf> = OnceLock::new();
    ROOT.get_or_init(|| {
        tempfile::Builder::new()
            .prefix("rift-test-cache-")
            .tempdir_in(std::env::temp_dir())
            .expect("the test process's cache root is created under the system temp dir")
            .keep()
    })
    .clone()
}

/// [`cache_root`]'s release resolution over an injected environment lookup —
/// the seam its resolution order is tested through.
fn cache_root_from(env: impl Fn(&str) -> Option<String>) -> PathBuf {
    rift_store::resolve_cache_root(env)
}

/// Why this process has nowhere to keep its machine state, or `None` when it
/// has: the environment names neither a cache root nor a config root (cron,
/// `env -i`, a service with no `HOME`). `main` asks before anything runs, so a
/// command refuses with this sentence and a hook exits quietly, instead of
/// projections, the registry and the install salt landing in the working
/// directory.
#[cfg(not(test))]
pub(crate) fn state_roots_unset() -> Option<&'static str> {
    state_roots_unset_from(|key| std::env::var(key).ok())
}

/// The test build always has its per-process root (see [`cache_root`]).
#[cfg(test)]
pub(crate) fn state_roots_unset() -> Option<&'static str> {
    None
}

/// [`state_roots_unset`] over an injected environment lookup.
fn state_roots_unset_from(env: impl Fn(&str) -> Option<String>) -> Option<&'static str> {
    if rift_store::configured_cache_root(&env).is_none() {
        return Some(rift_store::CACHE_ROOT_UNSET);
    }
    if rift_registry::configured_config_root(&env).is_none() {
        return Some(rift_registry::CONFIG_ROOT_UNSET);
    }
    None
}

/// Forget every memoized resolution of one family: the entries whose
/// manifest is `reminted`'s. Called by [`remint`], the only writer of a
/// manifest after the first mint, so a process that reminted does not go on
/// answering with the ids it just replaced.
///
/// One family, not the whole memo. The memo is process-global and the test
/// runner is one process: a remint of some other repository (a parallel test's
/// temp root) must not forget a family this caller is still holding, which
/// under `--all-features` it did, once in every few runs. Compared by the
/// canonical manifest path — every entry was minted from the same common dir
/// and the remint just wrote the file — with the spelled path as the fallback
/// for a manifest that is gone.
fn forget_family(reminted: &Family) {
    let same_manifest = |path: &Path| {
        rift_git::family::canonical_or_lexical(path)
            == rift_git::family::canonical_or_lexical(&reminted.manifest_path)
    };
    if let Ok(mut m) = memo().lock() {
        m.retain(|_, family| !same_manifest(&family.manifest_path));
    }
}

/// Run a `rift family` subcommand from `root`.
///
/// # Errors
/// Propagates the family layer's failures (an unreadable common dir, a
/// manifest that cannot be written).
pub(crate) fn run(cmd: &FamilyCmd, root: &Path) -> Result<Outcome> {
    match cmd {
        FamilyCmd::Remint(args) => remint(args, root),
    }
}

/// The mode the flags name, or the usage refusal that says which is missing.
fn mode_of(args: &RemintArgs) -> core::result::Result<RemintMode, Outcome> {
    match (args.adopt_main, args.fresh) {
        (true, false) => Ok(RemintMode::AdoptMain),
        (false, true) => Ok(RemintMode::Fresh),
        // clap rejects both together; neither is the case left to answer for.
        _ => Err(usage(
            "rift family remint needs one rule: --adopt-main (re-adopt the main \
             work tree's ids) or --fresh (derive them from the common dir)",
        )),
    }
}

fn usage(message: &str) -> Outcome {
    Outcome::coded(
        exit::USAGE,
        message.to_owned(),
        serde_json::json!({ "ok": false, "error": message }),
    )
}

/// `rift family remint (--adopt-main | --fresh) --yes`.
fn remint(args: &RemintArgs, root: &Path) -> Result<Outcome> {
    let mode = match mode_of(args) {
        Ok(m) => m,
        Err(refusal) => return Ok(refusal),
    };
    if !args.yes {
        return Ok(usage(
            "rift family remint rewrites this repository's recorded identity, and \
             the data filed under the previous ids is not moved. Re-run with --yes.",
        ));
    }
    let Ok(paths) = rift_git::discover(root) else {
        return Ok(not_a_repository(root));
    };

    // What the record said before, so the report can name what is being
    // replaced rather than only what replaces it.
    let before = match rift_git::resolve_family(&paths, MintPolicy::Never)? {
        FamilyState::Minted(f) => Some(f),
        FamilyState::Unminted(_) | FamilyState::Corrupt(_) => None,
    };
    let reason = args.reason.as_deref().unwrap_or("rift family remint");
    let after = rift_git::remint(&paths, mode, reason)
        .map_err(|e| RiftError::Other(format!("remint refused: {e}")))?;
    // The record just changed; anything this process memoized is now the
    // previous identity.
    forget_family(&after);
    Ok(reminted(before, &after, reason))
}

/// The precondition refusal for a `root` outside any git repository.
fn not_a_repository(root: &Path) -> Outcome {
    let message = format!(
        "not inside a git repository: {} — a family identity belongs to a \
         repository, so there is nothing here to remint",
        root.display()
    );
    Outcome::coded(
        exit::PRECONDITION,
        message.clone(),
        serde_json::json!({ "ok": false, "error": message }),
    )
}

/// The report of one remint: what the record said before, what it says now,
/// and — when the ids moved — that the data filed under the old ones stays
/// where it is.
fn reminted(before: Option<Family>, after: &Family, reason: &str) -> Outcome {
    let changed = before
        .as_ref()
        .is_none_or(|b| b.family_id != after.family_id || b.recall_id != after.recall_id);
    let mut human = String::new();
    match &before {
        Some(b) => human.push_str(&format!(
            "was  projection {}  recall {}  ({})\n",
            b.family_id, b.recall_id, b.minted_from
        )),
        None => human.push_str("was  no readable record\n"),
    }
    human.push_str(&format!(
        "now  projection {}  recall {}  ({})\n",
        after.family_id, after.recall_id, after.minted_from
    ));
    human.push_str(&format!("     {}\n", after.manifest_path.display()));
    human.push_str(&format!("     {}\n", after.history_path().display()));
    if changed {
        human.push_str(
            "\nThe ids changed. Whatever is filed under the previous ids stays where \
             it is — nothing was moved or deleted. `rift doctor` lists it.\n",
        );
    }

    Outcome::ok(
        human,
        serde_json::json!({
            "ok": true,
            "changed": changed,
            "reason": reason,
            "was": before.map(|b| serde_json::json!({
                "family_id": b.family_id.as_str(),
                "recall_id": b.recall_id.as_str(),
                "minted_from": b.minted_from.as_str(),
            })),
            "now": {
                "family_id": after.family_id.as_str(),
                "recall_id": after.recall_id.as_str(),
                "minted_from": after.minted_from.as_str(),
                "common_dir": after.common_dir.to_string_lossy(),
                "main_work_tree": after.main_work_tree
                    .as_ref().map(|p| p.to_string_lossy().into_owned()),
            },
            "manifest": after.manifest_path.to_string_lossy(),
            "history": after.history_path().to_string_lossy(),
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use rift_git::test_support::fake_repo;

    fn args(adopt_main: bool, fresh: bool, yes: bool) -> RemintArgs {
        RemintArgs {
            adopt_main,
            fresh,
            yes,
            reason: Some("a test asked".to_owned()),
        }
    }

    /// The three refusal arms of the product-path resolution, each in the
    /// vocabulary of what is missing — and never a fresh per-path database.
    #[test]
    fn the_product_resolution_refuses_in_the_vocabulary_of_what_is_missing() {
        // (a) outside any repository.
        let outside = tempfile::tempdir().unwrap();
        let e = resolve_at(outside.path()).unwrap_err().to_string();
        assert!(e.contains("not inside a git repository"), "{e}");
        assert!(!outside.path().join(".git").exists(), "and minted nothing");

        // (b) a member whose main work tree no vantage here can prove: the
        // common dir's parent holds no `.git`, so rule (c) refuses rather than
        // minting over `…/store` and orphaning the real projection.
        let tmp = tempfile::tempdir().unwrap();
        let common = tmp.path().join("store").join("repo.git");
        rift_git::test_support::fake_gitdir(&common).unwrap();
        let member = tmp.path().join("member");
        rift_git::test_support::fake_worktree(&common, &member, "m").unwrap();
        let e = resolve_at(&member).unwrap_err().to_string();
        assert!(e.contains("no recorded Rift identity yet"), "{e}");
        assert!(e.contains("rift init"), "the refusal names the fix: {e}");
        assert!(
            !common.join("rift-family").exists(),
            "an unprovable vantage writes nothing"
        );

        // (c) a manifest this reader will not guess at.
        let torn = tempfile::tempdir().unwrap();
        let git = fake_repo(torn.path()).unwrap();
        std::fs::create_dir_all(git.join("rift-family")).unwrap();
        std::fs::write(git.join("rift-family").join("family.json"), "{not json").unwrap();
        let e = resolve_at(torn.path()).unwrap_err().to_string();
        assert!(e.contains("cannot be read"), "{e}");
        assert!(e.contains("rift family remint"), "{e}");
    }

    /// The memo is what keeps the family lookup at one manifest read per hook.
    /// Stated where it can fail: the record is deleted between two
    /// resolutions, and the second neither re-reads it nor re-creates it —
    /// until `remint` says the recorded identity has changed.
    #[test]
    fn a_resolved_family_is_memoized_until_a_remint_says_otherwise() {
        let tmp = tempfile::tempdir().unwrap();
        let git = fake_repo(tmp.path()).unwrap();
        let first = resolve_at(tmp.path()).unwrap();
        let manifest = git.join("rift-family").join("family.json");
        assert!(manifest.is_file(), "the first resolution minted");

        std::fs::remove_file(&manifest).unwrap();
        let second = resolve_at(tmp.path()).unwrap();
        assert_eq!(first, second, "the second resolution is the memo's");
        assert!(
            !manifest.exists(),
            "and it neither read the file nor minted a new one"
        );

        forget_family(&first);
        let third = resolve_at(tmp.path()).unwrap();
        assert!(manifest.is_file(), "a forgotten family is resolved again");
        assert_eq!(
            (third.family_id, third.recall_id),
            (first.family_id, first.recall_id),
            "and adoption is deterministic, so the ids come back the same"
        );
    }

    /// A remint of some other repository forgets nothing of this one. The
    /// memo is process-global and the test runner is one process, so a
    /// whole-memo clear made a parallel test's remint re-mint the manifest
    /// this test had just deleted — once in every few `--all-features` runs.
    #[test]
    fn a_remint_elsewhere_leaves_this_familys_memo_alone() {
        let mine = tempfile::tempdir().unwrap();
        let git = fake_repo(mine.path()).unwrap();
        let first = resolve_at(mine.path()).unwrap();
        let manifest = git.join("rift-family").join("family.json");
        std::fs::remove_file(&manifest).unwrap();

        let other = tempfile::tempdir().unwrap();
        fake_repo(other.path()).unwrap();
        let reminted = run(&FamilyCmd::Remint(args(true, false, true)), other.path()).unwrap();
        assert_eq!(reminted.code, 0, "{}", reminted.human);

        let second = resolve_at(mine.path()).unwrap();
        assert_eq!(first, second, "the other repository's remint is not ours");
        assert!(
            !manifest.exists(),
            "and nothing re-minted the manifest this test deleted"
        );
    }

    /// The two refusals, before anything is written: a rule is required, and so
    /// is saying yes to it. Neither reaches the filesystem.
    #[test]
    fn remint_refuses_without_a_rule_and_without_yes() {
        let tmp = tempfile::tempdir().unwrap();
        fake_repo(tmp.path()).unwrap();

        let no_rule = run(&FamilyCmd::Remint(args(false, false, true)), tmp.path()).unwrap();
        assert_eq!(no_rule.code, exit::USAGE);
        let no_yes = run(&FamilyCmd::Remint(args(false, true, false)), tmp.path()).unwrap();
        assert_eq!(no_yes.code, exit::USAGE);
        assert!(
            !tmp.path().join(".git").join("rift-family").exists(),
            "a refusal wrote nothing"
        );
    }

    /// Outside a repository there is no family to repair, and the command says
    /// so rather than minting one where it stands.
    #[test]
    fn remint_outside_a_repository_is_a_precondition_refusal() {
        let tmp = tempfile::tempdir().unwrap();
        let out = run(&FamilyCmd::Remint(args(true, false, true)), tmp.path()).unwrap();
        assert_eq!(out.code, exit::PRECONDITION);
        assert!(
            !tmp.path().join(".git").exists(),
            "and created no repository"
        );
    }

    /// The verb is the only post-mint writer, and it reports what it replaced.
    /// `--fresh` from a repository whose main work tree is right there changes
    /// the ids, so the report has to say the previous ones' data stayed behind.
    #[test]
    fn remint_reports_the_identity_it_replaced() {
        let tmp = tempfile::tempdir().unwrap();
        fake_repo(tmp.path()).unwrap();
        let paths = rift_git::discover(tmp.path()).unwrap();
        let before = match rift_git::resolve_family(&paths, MintPolicy::Allow).unwrap() {
            FamilyState::Minted(f) => f,
            other => panic!("expected a mint, got {other:?}"),
        };

        let out = run(&FamilyCmd::Remint(args(false, true, true)), tmp.path()).unwrap();

        assert_eq!(out.code, exit::OK);
        assert_eq!(out.json["changed"], serde_json::json!(true));
        assert_eq!(out.json["reason"], serde_json::json!("a test asked"));
        assert_eq!(
            out.json["was"]["family_id"],
            serde_json::json!(before.family_id.as_str())
        );
        assert_ne!(out.json["now"]["family_id"], out.json["was"]["family_id"]);
        assert_eq!(out.json["now"]["minted_from"], serde_json::json!("remint"));
        assert!(out.human.contains("The ids changed."));

        let history = std::fs::read_to_string(before.history_path()).unwrap();
        assert_eq!(history.lines().count(), 2, "the mint and the remint");
        assert!(history.contains("a test asked"));
    }

    /// `--adopt-main` re-derives the ids the main work tree already owns, so
    /// reminting a repository that is its own main is a no-op in every way but
    /// the history line it leaves.
    #[test]
    fn remint_adopt_main_re_derives_the_same_ids() {
        let tmp = tempfile::tempdir().unwrap();
        fake_repo(tmp.path()).unwrap();
        let paths = rift_git::discover(tmp.path()).unwrap();
        let before = match rift_git::resolve_family(&paths, MintPolicy::Allow).unwrap() {
            FamilyState::Minted(f) => f,
            other => panic!("expected a mint, got {other:?}"),
        };

        let out = run(&FamilyCmd::Remint(args(true, false, true)), tmp.path()).unwrap();

        assert_eq!(out.code, exit::OK);
        assert_eq!(out.json["changed"], serde_json::json!(false));
        assert_eq!(
            out.json["now"]["family_id"],
            serde_json::json!(before.family_id.as_str())
        );
        assert!(!out.human.contains("The ids changed."));
    }

    /// The release resolution keeps the documented order: the explicit
    /// override, then the XDG cache home, then the platform default.
    #[test]
    fn the_release_cache_root_reads_the_override_then_xdg_then_the_platform_default() {
        let table = |pairs: &'static [(&'static str, &'static str)]| {
            move |key: &str| {
                pairs
                    .iter()
                    .find(|(k, _)| *k == key)
                    .map(|(_, v)| (*v).to_owned())
            }
        };
        static ALL: [(&str, &str); 4] = [
            ("RIFT_CACHE_DIR", "/override"),
            ("XDG_CACHE_HOME", "/xdg"),
            ("HOME", "/home/u"),
            ("LOCALAPPDATA", "/local"),
        ];
        assert_eq!(cache_root_from(table(&ALL)), PathBuf::from("/override"));
        assert_eq!(cache_root_from(table(&ALL[1..])), PathBuf::from("/xdg"));
        let platform = if cfg!(windows) {
            PathBuf::from("/local")
        } else {
            PathBuf::from("/home/u/.cache")
        };
        assert_eq!(cache_root_from(table(&ALL[2..])), platform);
    }

    /// With no `HOME` and no override, the process refuses rather than keep
    /// its state in the working directory; an override alone is not enough,
    /// because the registry lives under the config root.
    #[test]
    fn an_environment_without_home_or_an_override_names_why_it_refuses() {
        let none = |_: &str| None::<String>;
        assert_eq!(
            state_roots_unset_from(none),
            Some(rift_store::CACHE_ROOT_UNSET)
        );
        let cache_only = |key: &str| (key == "RIFT_CACHE_DIR").then(|| "/c".to_owned());
        assert_eq!(
            state_roots_unset_from(cache_only),
            Some(rift_registry::CONFIG_ROOT_UNSET)
        );
        let both = |key: &str| {
            matches!(key, "RIFT_CACHE_DIR" | "RIFT_CONFIG_DIR").then(|| "/c".to_owned())
        };
        assert_eq!(state_roots_unset_from(both), None);
    }

    /// An in-process test never reaches the developer's cache: every store
    /// path this crate derives sits under one temp root per test process.
    #[test]
    fn a_test_process_derives_every_store_under_its_own_temp_root() {
        let root = cache_root();
        assert!(root.starts_with(std::env::temp_dir()), "{}", root.display());
        assert!(root.is_dir());
        assert_eq!(cache_root(), root, "one root for the whole process");

        let tmp = tempfile::tempdir().unwrap();
        fake_repo(tmp.path()).unwrap();
        assert!(projection_db(tmp.path()).unwrap().starts_with(&root));
        assert!(recall_db(tmp.path()).unwrap().starts_with(&root));
    }
}
