//! The walk that produces the index, and the cheap probe that says whether it
//! has to run again.
//!
//! Both use the same rules, because an index built over one set of paths and
//! invalidated against another would be fresh and wrong at the same time:
//!
//! - **What git ignores, Rift ignores.** `target/`, `node_modules/` and every
//!   other line of `.gitignore` are excluded, as are `.git/info/exclude` and
//!   `.ignore` files. Ignore files *above* the work tree are not consulted and
//!   neither is the developer's global one: an index that differed between two
//!   machines with the same checkout would rank the same query differently on
//!   each.
//! - **What Rift's policy calls sensitive is never indexed.** Not filtered at
//!   the answer, not ranked last — never in the list. A path that is not in the
//!   index cannot be named by a ranked answer, cannot be grepped by the content
//!   primitive, and cannot leak through a caller that forgot to re-check.
//! - **Rift's own run state is not searchable.** `.git/` and `.rift/state/` are
//!   pruned: they hold window journals, sidecars and this index, none of which
//!   is a file anybody meant to search for.
//! - **Symbolic links are not followed** — a link out of the work tree is not
//!   part of it, and a link into it is already indexed under its real name.

use std::path::Path;

use ignore::WalkBuilder;

use crate::index::{FileEntry, IndexKey, PathIndex, MAX_PATHS};
use crate::SearchError;

/// How large a walk may get before it is abandoned.
///
/// A repository past the cap gets *no* index rather than a partial one. A
/// partial list would answer "these are the paths" and be wrong about it — the
/// total a listing answer quotes would be a number Rift made up — and the
/// caller's fallback (run the tool, ask again later) is exactly right for a
/// tree that size.
pub const WALK_PATH_CAP: usize = MAX_PATHS;

/// What one walk found.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Walked {
    /// Every indexable path, unsorted.
    pub entries: Vec<FileEntry>,
    /// The newest modification time of any directory entered, in milliseconds.
    pub newest_dir_mtime_ms: u64,
    /// How many directories that was, which is what the probe costs.
    pub directories: usize,
}

/// Walk `root` and collect the paths an index is made of.
///
/// Never fails: a directory that cannot be read, an entry whose metadata is
/// gone by the time it is asked for, a name that is not UTF-8 — each is one
/// path missing from the list, which costs a ranking and can never produce a
/// path that is not there.
#[must_use]
pub fn walk(root: &Path, sensitive: &[String]) -> Walked {
    walk_within(root, sensitive, None).unwrap_or_default()
}

/// Walk `root`, giving up if `deadline` passes.
///
/// `None` means the deadline passed: the walk is abandoned rather than
/// finished and thrown away, because the caller waiting on it has a budget and
/// the detached worker — which has none — is about to do the same work
/// properly.
///
/// The clock is read once every few hundred entries rather than on each one. The check is what makes the budget real, and reading a clock is
/// not free; a repository large enough for the deadline to matter has orders of
/// magnitude more entries than the interval.
#[must_use]
pub fn walk_within(
    root: &Path,
    sensitive: &[String],
    deadline: Option<std::time::Instant>,
) -> Option<Walked> {
    let mut out = Walked::default();
    let mut seen: usize = 0;
    for entry in walker(root).build().flatten() {
        seen += 1;
        if seen.is_multiple_of(DEADLINE_CHECK_EVERY)
            && deadline.is_some_and(|deadline| std::time::Instant::now() > deadline)
        {
            return None;
        }
        let Some(rel) = relative(root, entry.path()) else {
            continue;
        };
        if entry.file_type().is_some_and(|t| t.is_dir()) {
            out.directories += 1;
            out.newest_dir_mtime_ms = out.newest_dir_mtime_ms.max(dir_mtime_ms(&entry));
            continue;
        }
        let mtime_ms = entry.metadata().ok().map_or(0, |m| mtime_ms(&m));
        if rel.is_empty() || is_sensitive(&rel, sensitive) {
            continue;
        }
        out.entries.push(FileEntry {
            path: rel,
            mtime_ms,
        });
        if out.entries.len() > WALK_PATH_CAP {
            // One past the cap is enough to refuse: the caller is told the
            // tree is too large, and nothing is spent walking the rest of it.
            break;
        }
    }
    Some(out)
}

/// How many entries pass between two readings of the clock.
const DEADLINE_CHECK_EVERY: usize = 512;

/// The state a fresh index would have to have been taken in.
///
/// The directory half of the key without the file half of the walk: the same
/// traversal, statting only what it enters. This is what a cold caller runs
/// before it trusts an index file.
///
/// It takes no sensitive globs because they cannot change its answer: they
/// exclude *files*, and a directory is entered — and its modification time
/// read — either way.
#[must_use]
pub fn current_key(root: &Path, head_oid: &str) -> IndexKey {
    current_key_within(root, head_oid, None).unwrap_or_else(|| IndexKey {
        head_oid: head_oid.to_owned(),
        newest_dir_mtime_ms: 0,
    })
}

/// The same probe, abandoned if `deadline` passes.
///
/// `None` means the probe itself ran out of time, which is a real outcome
/// rather than a theoretical one: on a tree big enough for the walk to be
/// unaffordable, *deciding whether the saved index is stale* is unaffordable
/// too, and a caller that spent its whole budget finding that out would have
/// spent it for nothing.
#[must_use]
pub fn current_key_within(
    root: &Path,
    head_oid: &str,
    deadline: Option<std::time::Instant>,
) -> Option<IndexKey> {
    let mut newest = 0;
    let mut seen: usize = 0;
    for entry in walker(root).build().flatten() {
        if !entry.file_type().is_some_and(|t| t.is_dir()) {
            continue;
        }
        seen += 1;
        if seen.is_multiple_of(DEADLINE_CHECK_EVERY)
            && deadline.is_some_and(|deadline| std::time::Instant::now() > deadline)
        {
            return None;
        }
        if relative(root, entry.path()).is_none() {
            continue;
        }
        newest = newest.max(dir_mtime_ms(&entry));
    }
    Some(IndexKey {
        head_oid: head_oid.to_owned(),
        newest_dir_mtime_ms: newest,
    })
}

/// Walk `root` and build the index it stands for.
///
/// # Errors
///
/// [`SearchError::TooManyPaths`] when the tree is over [`WALK_PATH_CAP`].
pub fn build_index(
    root: &Path,
    head_oid: &str,
    sensitive: &[String],
) -> Result<PathIndex, SearchError> {
    let walked = walk(root, sensitive);
    let key = IndexKey {
        head_oid: head_oid.to_owned(),
        newest_dir_mtime_ms: walked.newest_dir_mtime_ms,
    };
    PathIndex::new(key, walked.entries)
}

/// The one walker configuration, shared by the walk and the probe.
fn walker(root: &Path) -> WalkBuilder {
    let mut builder = WalkBuilder::new(root);
    builder
        // Dotfiles are files: `.github/workflows/ci.yml` is a path an agent
        // looks for by name.
        .hidden(false)
        .parents(false)
        .git_global(false)
        .git_ignore(true)
        .git_exclude(true)
        .ignore(true)
        // The ignore rules apply whether or not this directory is a checkout,
        // so a temporary tree behaves like the repository it stands in for.
        .require_git(false)
        .follow_links(false)
        .filter_entry(|entry| {
            let name = entry.file_name();
            name != ".git" && !(name == "state" && is_rift_state(entry.path()))
        });
    builder
}

/// Whether this `state` directory is Rift's own, rather than a `state`
/// directory belonging to the project.
fn is_rift_state(path: &Path) -> bool {
    path.parent()
        .and_then(Path::file_name)
        .is_some_and(|parent| parent == ".rift")
}

/// The path relative to the work tree, spelled with `/` on every platform.
///
/// `None` for anything that cannot be spelled that way — a path outside the
/// root, or a name that is not UTF-8 — because a path Rift cannot write into a
/// window journal row or a tool call is not a path it can offer as an answer.
fn relative(root: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(root).ok()?;
    let mut out = String::new();
    for component in rel.components() {
        let part = component.as_os_str().to_str()?;
        if !out.is_empty() {
            out.push('/');
        }
        out.push_str(part);
    }
    Some(out)
}

/// Whether the repository's policy calls this path sensitive.
fn is_sensitive(rel: &str, sensitive: &[String]) -> bool {
    sensitive
        .iter()
        .any(|glob| rift_config::glob_match(glob, rel))
}

/// A directory's modification time, read from the directory itself.
///
/// Never from the walker's cached entry: on Windows `walkdir` hands back the
/// metadata it captured while listing the *parent*, and NTFS refreshes the
/// duplicate timestamps kept in that listing lazily, when a handle to the
/// directory is closed. The walk's own enumeration of a directory is what
/// refreshes its entry, so a probe that read the cached value after a walk
/// would key the same tree twice differently and judge every fresh index
/// stale. A fresh `symlink_metadata` reads the directory's own record on
/// every platform; on Unix the two agree, so this is behaviour-neutral there.
/// File times still come from the entry: they feed frecency, not the key.
fn dir_mtime_ms(entry: &ignore::DirEntry) -> u64 {
    std::fs::symlink_metadata(entry.path())
        .ok()
        .map_or(0, |m| mtime_ms(&m))
}

/// A modification time in milliseconds, or 0 where the filesystem has none.
///
/// Zero is the coldest a file can be, which is the right way for this to fail:
/// a file whose age is unknown is ranked as though it had never been touched,
/// never as though it had just been.
fn mtime_ms(meta: &std::fs::Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, rel: &str, body: &str) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    fn paths(walked: &Walked) -> Vec<String> {
        let mut out: Vec<String> = walked.entries.iter().map(|e| e.path.clone()).collect();
        out.sort();
        out
    }

    /// The walk is the repository's own idea of what is in it: `.gitignore`
    /// decides, dotfiles are files, and Rift's own run state is not a place to
    /// search.
    #[test]
    fn the_walk_indexes_what_git_would_and_nothing_rift_owns() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, ".gitignore", "target\n");
        write(root, "src/main.rs", "fn main() {}\n");
        write(root, "target/debug/rift", "binary");
        write(root, ".github/workflows/ci.yml", "on: push\n");
        write(root, ".rift/config.toml", "schema_version = 1\n");
        write(root, ".rift/state/reads/abc.jsonl", "{}\n");
        write(root, ".git/HEAD", "ref: refs/heads/main\n");
        write(root, "state/keep.txt", "a project's own state directory");

        assert_eq!(
            paths(&walk(root, &[])),
            vec![
                ".github/workflows/ci.yml".to_owned(),
                ".gitignore".to_owned(),
                ".rift/config.toml".to_owned(),
                "src/main.rs".to_owned(),
                "state/keep.txt".to_owned(),
            ]
        );
    }

    /// A sensitive path is not in the index at all. Everything downstream —
    /// the ranked answer, the content primitive, whatever a later caller
    /// builds — reads the index, so this is the one place the exclusion has to
    /// hold, and it holds for the file rather than for the query.
    #[test]
    fn a_sensitive_path_is_never_indexed() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "src/main.rs", "fn main() {}\n");
        write(root, "secrets/prod.env", "TOKEN=hunter2\n");
        write(root, "deploy/id_rsa", "-----BEGIN\n");
        write(root, "notes/keys.md", "not a key\n");

        let sensitive = vec!["secrets/**".to_owned(), "**/id_rsa".to_owned()];
        assert_eq!(
            paths(&walk(root, &sensitive)),
            vec!["notes/keys.md".to_owned(), "src/main.rs".to_owned()]
        );
    }

    /// The probe and the walk agree about the key, and the key moves when a
    /// path appears, disappears or moves — not when a file's bytes change.
    #[test]
    fn the_index_is_rebuilt_when_head_or_mtime_moves() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "src/main.rs", "fn main() {}\n");
        let head = "a".repeat(40);

        let index = build_index(root, &head, &[]).unwrap();
        assert!(index.is_fresh_for(&current_key(root, &head)));

        // A checkout: the same files, a different commit.
        assert!(!index.is_fresh_for(&current_key(root, &"b".repeat(40))));

        // A new file: the directory that holds it is newer than the index.
        std::thread::sleep(std::time::Duration::from_millis(20));
        write(root, "src/lib.rs", "pub fn f() {}\n");
        let after_add = current_key(root, &head);
        assert!(
            !index.is_fresh_for(&after_add),
            "a new path left the index fresh"
        );

        let rebuilt = build_index(root, &head, &[]).unwrap();
        assert!(rebuilt.is_fresh_for(&after_add));
        assert_eq!(rebuilt.entries().len(), 2);

        // An edit in place changes no path, so the list is still the list.
        std::thread::sleep(std::time::Duration::from_millis(20));
        write(root, "src/lib.rs", "pub fn f() { g() }\n");
        assert!(
            rebuilt.is_fresh_for(&current_key(root, &head)),
            "editing a file's bytes invalidated a list of paths"
        );
    }

    /// The probe visits the same tree the walk does, so the two cannot
    /// disagree about which directories the key is made of.
    #[test]
    fn the_probe_reads_the_same_tree_the_walk_does() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, ".gitignore", "target\n");
        write(root, "src/main.rs", "fn main() {}\n");
        write(root, "target/debug/deps/x.o", "object");
        let walked = walk(root, &[]);
        assert_eq!(
            current_key(root, "").newest_dir_mtime_ms,
            walked.newest_dir_mtime_ms
        );
        assert_eq!(walked.directories, 2, "the root and src, never target");
    }
}
