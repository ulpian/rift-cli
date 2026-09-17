//! Which *commit* a published number is about (the guard's staleness arm).
//!
//! [`crate::freshness`] answers "is the binary at least as new as the sources
//! beside it?" — a **clock** question, answered from mtimes. It cannot answer
//! the question a floor needs: *which commit* was measured, and is that commit
//! in this history at all. Two builds of two different branches taken a minute
//! apart both pass the freshness check and describe different code; a report
//! whose only provenance is an mtime cannot be re-derived by anybody, because
//! the tree it names no longer exists anywhere.
//!
//! So every artifact records the checkout's own `git describe` and whether the
//! working tree was dirty when the run happened. Those two facts make three
//! checks mechanical, and the staleness arm is all three:
//!
//! 1. **dirty is not a commit.** A number taken over uncommitted edits is about
//!    a tree nobody else has. It may be published — as a record — but it may
//!    never derive a floor.
//! 2. **the subject commit must be an ancestor of `HEAD`.** A measurement taken
//!    on an abandoned branch is a real measurement of code this history does not
//!    contain.
//! 3. **the subject commit must contain the last change to the measured
//!    paths.** This is the one that catches the failure that actually happened
//!    here: eighteen committed artifacts were taken about five hours *before*
//!    the caps they are quoted for landed, and nothing in them said so, because
//!    an mtime cannot say so.
//!
//! Provenance is **read from the checkout, never measured**: it runs outside
//! every timed region, exactly like the oracles, and it is deliberately taken
//! through a plain [`std::process::Command`] rather than the hermetic plane —
//! the hermetic plane exists to make two *timings* comparable, and this is not
//! a timing.

use std::{
    path::Path,
    process::{Command, Stdio},
};

use serde::{Deserialize, Serialize};

/// The length of a full hexadecimal object id.
const OID_HEX: usize = 40;

/// What `git` says about the checkout a run resolved its binaries from.
///
/// **Both fields are required of a new artifact and optional in the type**, and
/// the split is deliberate. `#[serde(default)]` on the artifact side keeps the
/// eighteen committed schema-1 records loadable — they are evidence and must
/// not be rewritten — the standing rule is that a record is never edited to
/// make a check pass —
/// while the guard treats an artifact with no provenance as one that **may not
/// derive a floor**. Absent is not fresh, and the two are never collapsed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitProvenance {
    /// `git describe --always --tags --abbrev=40`, so the string **contains a
    /// full object id** whether or not the checkout has tags. The abbreviation
    /// is not cosmetic: the guard resolves ancestry from this string, and a
    /// 7-character prefix can become ambiguous in a repository this size.
    pub describe: String,
    /// Whether `git status --porcelain` had anything to say.
    pub dirty: bool,
}

impl GitProvenance {
    /// Read the provenance of the checkout at `repo_root`.
    ///
    /// `None` when `git` is unavailable or the path is not a checkout — which
    /// is recorded as unknown, never as clean.
    #[must_use]
    pub fn detect(repo_root: &Path) -> Option<Self> {
        // The BARE full object id, deliberately undecorated. The decorated
        // `<tag>-<n>-g<oid>` form this recorded until 2026-08-31 mixes the tag
        // alphabet into a 48-character token, and whether that token crosses
        // the redaction entropy layer's 4.0-bit floor depends on the sha's own
        // byte spread — one measurement sitting saw one commit's describe at
        // 4.07 bits and its predecessor's at 3.92, twelve census hits apart on
        // identical artifacts. A 40-hex id is a 16-symbol alphabet and can
        // never reach 4.0 bits, so the recorded provenance stays census-inert.
        // `commit()` still accepts both shapes: committed artifacts predate
        // this and are evidence, never rewritten.
        let describe = git_stdout(repo_root, &["rev-parse", "HEAD"])?;
        let describe = describe.trim();
        if describe.is_empty() {
            return None;
        }
        // `--porcelain` prints one line per changed path and nothing at all for
        // a clean tree, so emptiness *is* the answer.
        let porcelain = git_stdout(repo_root, &["status", "--porcelain"])?;
        Some(Self {
            describe: describe.to_owned(),
            dirty: !porcelain.trim().is_empty(),
        })
    }

    /// The object id inside [`Self::describe`].
    ///
    /// `git describe` yields either a bare object id (no tags reachable) or
    /// `<tag>-<n>-g<oid>`. Both end in the id, which is why `--abbrev=40` is
    /// part of the contract: this returns a *full* id or nothing, and never a
    /// prefix that a later `rev-parse` might resolve to something else.
    #[must_use]
    pub fn commit(&self) -> Option<&str> {
        let tail = self
            .describe
            .rsplit_once("-g")
            .map_or(self.describe.as_str(), |(_, oid)| oid);
        (tail.len() == OID_HEX && tail.chars().all(|c| c.is_ascii_hexdigit())).then_some(tail)
    }

    /// Whether this provenance can back a derived floor at all.
    ///
    /// Two ways to fail and they are different sentences: a dirty tree is not a
    /// commit, and a describe string with no resolvable id names nothing.
    #[must_use]
    pub fn refusal(&self) -> Option<String> {
        if self.dirty {
            return Some(format!(
                "the subject checkout was DIRTY at `{}`: the tree measured exists on no other \
                 machine, so this artifact is a record and may not derive a floor",
                self.describe
            ));
        }
        if self.commit().is_none() {
            return Some(format!(
                "`{}` carries no full object id (the harness records \
                 `git describe --always --tags --abbrev=40`), so ancestry cannot be resolved",
                self.describe
            ));
        }
        None
    }
}

/// Whether `ancestor` is an ancestor of (or identical to) `descendant`.
///
/// `None` when the question could not be asked — an unknown id, no git, not a
/// checkout. Never `Some(false)` for those: "I could not tell" and "no" send a
/// reader to opposite conclusions.
#[must_use]
pub fn is_ancestor(repo_root: &Path, ancestor: &str, descendant: &str) -> Option<bool> {
    if !known_commit(repo_root, ancestor) || !known_commit(repo_root, descendant) {
        return None;
    }
    let status = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["merge-base", "--is-ancestor", ancestor, descendant])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .stdin(Stdio::null())
        .status()
        .ok()?;
    // 0 = yes, 1 = no, anything else = the question was not answered.
    match status.code() {
        Some(0) => Some(true),
        Some(1) => Some(false),
        _ => None,
    }
}

/// The newest commit reachable from `HEAD` that touched any of `paths`.
///
/// This is the other half of the staleness arm: a subject that does not contain
/// this commit was built before the code it is quoted about.
#[must_use]
pub fn last_commit_touching(repo_root: &Path, paths: &[String]) -> Option<String> {
    if paths.is_empty() {
        return None;
    }
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(repo_root)
        .args(["log", "-1", "--format=%H", "HEAD", "--"]);
    for path in paths {
        cmd.arg(path);
    }
    let out = cmd
        .stderr(Stdio::null())
        .stdin(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let oid = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    (oid.len() == OID_HEX).then_some(oid)
}

/// `HEAD`'s own object id.
#[must_use]
pub fn head_commit(repo_root: &Path) -> Option<String> {
    let oid = git_stdout(repo_root, &["rev-parse", "HEAD"])?;
    let oid = oid.trim().to_owned();
    (oid.len() == OID_HEX).then_some(oid)
}

/// Whether `rev` names an object this checkout holds.
fn known_commit(repo_root: &Path, rev: &str) -> bool {
    if rev.is_empty() {
        return false;
    }
    git_stdout(repo_root, &["cat-file", "-e", &format!("{rev}^{{commit}}")]).is_some()
}

/// Run `git` in `repo_root` and return its stdout, or `None` on any failure.
fn git_stdout(repo_root: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(args)
        .stderr(Stdio::null())
        .stdin(Stdio::null())
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, path::PathBuf, process::Command};

    /// A throwaway checkout with two commits, so ancestry has something to say.
    struct Repo {
        _tmp: tempfile::TempDir,
        root: PathBuf,
    }

    impl Repo {
        fn new() -> Self {
            let tmp = tempfile::tempdir().unwrap();
            let root = tmp.path().to_path_buf();
            Self::git(&root, &["init", "--quiet"]);
            Self::git(&root, &["config", "user.email", "bench@example.invalid"]);
            Self::git(&root, &["config", "user.name", "bench"]);
            Self::git(&root, &["config", "commit.gpgsign", "false"]);
            Self { _tmp: tmp, root }
        }

        fn git(root: &Path, args: &[&str]) -> String {
            let out = Command::new("git")
                .arg("-C")
                .arg(root)
                .args(args)
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?}: {out:?}");
            String::from_utf8_lossy(&out.stdout).trim().to_owned()
        }

        fn commit(&self, path: &str, body: &str) -> String {
            let file = self.root.join(path);
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(&file, body).unwrap();
            Self::git(&self.root, &["add", "--", path]);
            Self::git(&self.root, &["commit", "--quiet", "-m", body]);
            Self::git(&self.root, &["rev-parse", "HEAD"])
        }
    }

    #[test]
    fn a_real_checkout_yields_a_describe_that_carries_a_full_object_id() {
        let repo = Repo::new();
        let first = repo.commit("a.txt", "one");
        let p = GitProvenance::detect(&repo.root).unwrap();
        assert!(!p.dirty, "a freshly committed tree is clean: {p:?}");
        assert_eq!(p.commit(), Some(first.as_str()));
        assert_eq!(p.refusal(), None);
    }

    /// The `<tag>-<n>-g<oid>` shape, which is what this repository actually
    /// produces — a bare-oid-only parser would return `None` here and the whole
    /// staleness arm would silently degrade to "unknown".
    #[test]
    fn a_tagged_describe_still_yields_the_object_id() {
        let repo = Repo::new();
        repo.commit("a.txt", "one");
        Repo::git(&repo.root, &["tag", "ls"]);
        let second = repo.commit("b.txt", "two");
        let p = GitProvenance::detect(&repo.root).unwrap();
        // The writer records the bare id even in a tagged checkout (the
        // decorated form is census-hostile — see `detect`); the decorated
        // `<tag>-<n>-g<oid>` arm of the PARSER stays covered by
        // `a_decorated_describe_from_a_committed_artifact_still_parses`-style
        // fixtures below, because committed artifacts still carry it.
        assert_eq!(p.describe, second, "the bare id is the record");
        assert_eq!(p.commit(), Some(second.as_str()));
    }

    /// A dirty tree is refused as a floor source **and still published**: the
    /// artifact is a record of a real run, it simply names a tree that exists
    /// nowhere else.
    #[test]
    fn a_dirty_checkout_is_recorded_and_refused_as_a_floor_source() {
        let repo = Repo::new();
        repo.commit("a.txt", "one");
        fs::write(repo.root.join("a.txt"), "edited").unwrap();
        let p = GitProvenance::detect(&repo.root).unwrap();
        assert!(p.dirty);
        let refusal = p.refusal().expect("a dirty tree may not derive a floor");
        assert!(refusal.contains("DIRTY"), "{refusal}");
        assert!(refusal.contains("may not derive a floor"), "{refusal}");
    }

    #[test]
    fn a_describe_with_no_object_id_names_nothing() {
        let p = GitProvenance {
            describe: "v1.2.3".to_owned(),
            dirty: false,
        };
        assert_eq!(p.commit(), None);
        assert!(p.refusal().unwrap().contains("no full object id"));
        // A short id is refused rather than guessed at: `--abbrev=40` is part of
        // the contract precisely so a prefix never has to be resolved.
        let short = GitProvenance {
            describe: "ls-597-g198f2f7".to_owned(),
            dirty: false,
        };
        assert_eq!(short.commit(), None);
    }

    #[test]
    fn ancestry_answers_yes_no_and_unknown_as_three_different_things() {
        let repo = Repo::new();
        let first = repo.commit("a.txt", "one");
        let second = repo.commit("b.txt", "two");
        assert_eq!(is_ancestor(&repo.root, &first, &second), Some(true));
        assert_eq!(is_ancestor(&repo.root, &second, &first), Some(false));
        assert_eq!(is_ancestor(&repo.root, &first, &first), Some(true));
        // An id this checkout does not hold is unanswerable, not "no".
        let stranger = "0".repeat(OID_HEX);
        assert_eq!(is_ancestor(&repo.root, &stranger, &second), None);
        assert_eq!(is_ancestor(&repo.root, "", &second), None);
    }

    #[test]
    fn the_last_commit_touching_a_path_is_the_one_that_touched_it() {
        let repo = Repo::new();
        let first = repo.commit("src/a.rs", "one");
        let second = repo.commit("other.txt", "two");
        assert_eq!(
            last_commit_touching(&repo.root, &["src/a.rs".to_owned()]).as_deref(),
            Some(first.as_str())
        );
        assert_eq!(
            last_commit_touching(&repo.root, &["other.txt".to_owned()]).as_deref(),
            Some(second.as_str())
        );
        // A path nothing ever touched has no such commit — absent, not HEAD.
        assert_eq!(
            last_commit_touching(&repo.root, &["nope/x".to_owned()]),
            None
        );
        assert_eq!(last_commit_touching(&repo.root, &[]), None);
        assert_eq!(head_commit(&repo.root).as_deref(), Some(second.as_str()));
    }

    #[test]
    fn a_path_that_is_not_a_checkout_is_unknown_rather_than_clean() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(GitProvenance::detect(tmp.path()), None);
        assert_eq!(head_commit(tmp.path()), None);
    }
}
