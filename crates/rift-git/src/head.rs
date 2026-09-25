//! `HEAD` read from the files git writes, without spawning git.

use crate::{validate_oid, validate_refspec, RepoPaths};

/// What `HEAD` said in one worktree at one moment: the branch it points at and
/// the commit it resolves to. Either may be absent, and absence is a FACT here,
/// never an empty string — a detached head has no branch, an unborn branch has
/// no commit, and an unreadable gitdir has neither (the schema stores both
/// columns NULLABLE for exactly these three shapes).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HeadState {
    /// `HEAD`'s symbolic name with `refs/heads/` stripped — the short branch
    /// name `git rev-parse --abbrev-ref HEAD` prints. `None` when detached or
    /// unreadable.
    pub branch: Option<String>,
    /// The object id `HEAD` resolves to. `None` on an unborn branch (the ref
    /// file does not exist yet) or when the ref cannot be resolved from the
    /// files this reader is allowed to touch.
    pub oid: Option<String>,
}

/// Read `HEAD` **without running git** — `paths.git_dir/HEAD`, then the loose
/// ref, then `packed-refs`.
///
/// **Why not `git rev-parse`.** This runs on the agent-hook path, once per
/// hook, inside the hard 100 ms git-latency budget with no daemon to hide
/// behind. A process spawn costs milliseconds; three `read_to_string` calls
/// cost microseconds, and the files are the same ones git itself would read.
/// The trade is that this reader understands the two ref stores git writes
/// (loose files and `packed-refs`) and not the `reftable` backend; a repository
/// git has migrated to `reftable` reports `oid: None`, which is UNKNOWN and
/// therefore correct rather than wrong.
///
/// A linked worktree's per-worktree `HEAD` lives in its own admin dir, which is
/// what `paths.git_dir` already is — so a member's branch is its own, never the
/// main work tree's. The loose ref is looked up in the worktree's gitdir FIRST
/// (git keeps per-worktree refs such as `refs/bisect/*` there) and then in the
/// common store, which is where `refs/heads/*` actually lives.
///
/// Total: every failure is `None`. Nothing here can fail a hook.
#[must_use]
pub fn head_state(paths: &RepoPaths) -> HeadState {
    let Ok(text) = std::fs::read_to_string(paths.git_dir.join("HEAD")) else {
        return HeadState::default();
    };
    let text = text.trim();
    let Some(refname) = text.strip_prefix("ref:") else {
        // Detached: `HEAD` holds the oid itself. Screened rather than trusted —
        // this file is ordinary bytes on disk and the value lands in an
        // append-only row.
        return HeadState {
            branch: None,
            oid: validate_oid(text).is_ok().then(|| text.to_owned()),
        };
    };
    let refname = refname.trim();
    if !is_safe_refname(refname) {
        return HeadState::default();
    }
    let branch = refname.strip_prefix("refs/heads/").unwrap_or(refname);
    HeadState {
        branch: (!branch.is_empty()).then(|| branch.to_owned()),
        oid: resolve_loose_or_packed(paths, refname),
    }
}

/// Whether a ref name read out of `HEAD` may be used as a path suffix under a
/// gitdir: `refs/`-rooted, with no component that escapes it.
///
/// `HEAD` is a file anyone with write access to the checkout can edit, and this
/// function is the difference between reading `<git_dir>/refs/heads/x` and
/// reading `<git_dir>/../../../../etc/passwd`. The screen is positive (an
/// allow-list of shapes) rather than a blacklist of `..`, because a blacklist
/// of path traversal is the one screen this codebase has already had to widen
/// twice.
fn is_safe_refname(refname: &str) -> bool {
    refname.starts_with("refs/")
        && refname.len() <= 255
        && validate_refspec(refname).is_ok()
        && refname
            .split('/')
            .all(|c| !c.is_empty() && c != "." && c != ".." && !c.contains('\\'))
}

/// The oid a screened ref resolves to: the worktree's own loose ref, then the
/// common store's, then `packed-refs`. `None` when the ref exists nowhere (an
/// unborn branch) or holds something that is not an object id.
fn resolve_loose_or_packed(paths: &RepoPaths, refname: &str) -> Option<String> {
    for base in [&paths.git_dir, &paths.common_dir] {
        if let Ok(text) = std::fs::read_to_string(base.join(refname)) {
            let value = text.trim();
            if validate_oid(value).is_ok() {
                return Some(value.to_owned());
            }
            // A symref file (`ref: refs/heads/other`) is NOT followed: one
            // level of indirection is all `HEAD` itself is allowed, and a
            // reader that chases a chain of them is a reader that can loop.
            return None;
        }
    }
    let packed = std::fs::read_to_string(paths.common_dir.join("packed-refs")).ok()?;
    packed
        .lines()
        .filter(|l| !l.starts_with('#') && !l.starts_with('^'))
        .find_map(|l| {
            let (oid, name) = l.split_once(' ')?;
            (name.trim() == refname && validate_oid(oid).is_ok()).then(|| oid.to_owned())
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discover;

    /// `HEAD` read from the files, not from a process — the three shapes the
    /// hook path meets, plus the two it must refuse (the NULLABLE
    /// `branch`/`head_oid` columns).
    #[test]
    fn head_state_reads_the_branch_and_the_oid_without_running_git() {
        let tmp = tempfile::tempdir().unwrap();
        let git_dir = crate::test_support::fake_repo(tmp.path()).unwrap();
        let paths = discover(tmp.path()).unwrap();

        // Unborn: a branch with no commit yet. The branch is KNOWN, the oid is
        // not — and "not yet" is absence, never the empty string.
        let unborn = head_state(&paths);
        assert_eq!(unborn.branch.as_deref(), Some("main"));
        assert_eq!(unborn.oid, None);

        // Loose ref in the common store: both known.
        let oid = "a".repeat(40);
        std::fs::create_dir_all(git_dir.join("refs/heads")).unwrap();
        std::fs::write(git_dir.join("refs/heads/main"), format!("{oid}\n")).unwrap();
        assert_eq!(
            head_state(&paths),
            HeadState {
                branch: Some("main".to_owned()),
                oid: Some(oid.clone())
            }
        );

        // Packed refs: the loose file is gone, the value is not.
        std::fs::remove_file(git_dir.join("refs/heads/main")).unwrap();
        std::fs::write(
            git_dir.join("packed-refs"),
            format!(
                "# pack-refs with: peeled\n{oid} refs/heads/main\n^{}\n",
                "b".repeat(40)
            ),
        )
        .unwrap();
        assert_eq!(head_state(&paths).oid.as_deref(), Some(oid.as_str()));

        // Detached: no branch, the oid straight out of HEAD.
        std::fs::write(git_dir.join("HEAD"), format!("{oid}\n")).unwrap();
        let detached = head_state(&paths);
        assert_eq!(detached.branch, None, "a detached HEAD has no branch");
        assert_eq!(detached.oid.as_deref(), Some(oid.as_str()));

        // Garbage is UNKNOWN, not a value: neither a branch nor an oid is
        // invented out of a HEAD that says neither.
        std::fs::write(git_dir.join("HEAD"), "not a ref and not an oid\n").unwrap();
        assert_eq!(head_state(&paths), HeadState::default());
    }

    /// `HEAD` is a file anyone with write access to a checkout can edit, and
    /// its contents become a path this reader joins onto a gitdir. A ref name
    /// that would escape the gitdir reads as UNKNOWN rather than as whatever
    /// file it pointed at.
    #[test]
    fn a_head_that_escapes_the_gitdir_is_refused_rather_than_followed() {
        let tmp = tempfile::tempdir().unwrap();
        let git_dir = crate::test_support::fake_repo(tmp.path()).unwrap();
        let paths = discover(tmp.path()).unwrap();
        std::fs::write(tmp.path().join("secret"), "a".repeat(40)).unwrap();

        for hostile in [
            "ref: refs/../../secret",
            "ref: ../secret",
            "ref: refs/heads/a b",
            "ref: refs/heads/;rm",
        ] {
            std::fs::write(git_dir.join("HEAD"), format!("{hostile}\n")).unwrap();
            assert_eq!(
                head_state(&paths),
                HeadState::default(),
                "{hostile} must not resolve"
            );
        }
        assert!(!is_safe_refname("refs/heads/../../x"));
        assert!(is_safe_refname("refs/heads/feature/one"));
    }

    /// A linked member's `HEAD` is its OWN — the whole reason a worktree label
    /// is worth recording. The admin dir holds the member's `HEAD`; the branch
    /// it names resolves through the common store, which is where
    /// `refs/heads/*` lives for every member.
    #[test]
    fn a_linked_member_reads_its_own_head_not_the_main_worktrees() {
        let tmp = tempfile::tempdir().unwrap();
        let main_git = crate::test_support::fake_repo(&tmp.path().join("main")).unwrap();
        let member_tree = tmp.path().join("member");
        let admin = crate::test_support::fake_worktree(&main_git, &member_tree, "member").unwrap();
        std::fs::write(admin.join("HEAD"), "ref: refs/heads/lane\n").unwrap();
        let oid = "c".repeat(40);
        std::fs::create_dir_all(main_git.join("refs/heads")).unwrap();
        std::fs::write(main_git.join("refs/heads/lane"), format!("{oid}\n")).unwrap();

        let member = discover(&member_tree).unwrap();
        assert_eq!(
            head_state(&member),
            HeadState {
                branch: Some("lane".to_owned()),
                oid: Some(oid)
            }
        );
        assert_eq!(
            head_state(&discover(&tmp.path().join("main")).unwrap())
                .branch
                .as_deref(),
            Some("main"),
            "the main work tree is still on its own branch"
        );
    }
}
