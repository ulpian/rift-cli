//! What this checkout can say about a path a transcript named.
//!
//! A replay has no file at the instant of the call, so the worktree stands in
//! for one, under the same two refusals the gate applies — or, given the
//! repository's history, the commit each window began at does, with the
//! worktree standing in only for a path that commit does not have.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use rift_agent::window_journal::Interval;

use super::first_contact::{is_first_contact_size, outline_facts, OutlineFacts};
use super::history::{Blob, History};

/// The largest file the gate will hash inline, and so the largest file a
/// decision is ever reached about. Mirrors the shipped default.
pub(super) const MAX_INLINE_HASH_BYTES: u64 = 1024 * 1024;

/// Whether a spelling names a root of its own rather than a name under some
/// working directory.
fn is_rooted(path: &str) -> bool {
    if path.starts_with('/') {
        return true;
    }
    let mut chars = path.chars();
    matches!(
        (chars.next(), chars.next(), chars.next()),
        (Some(drive), Some(':'), Some('/')) if drive.is_ascii_alphabetic()
    )
}

/// The one spelling coverage is keyed by, or `None` when the path is outside
/// the working directory the record states.
///
/// Separators are folded to one form and a drive letter's case is folded with
/// them, because a client spells the same file two ways in one session and two
/// spellings of one file must not become two claims about two files. Everything
/// after that is left verbatim: folding a name's case would merge two files on
/// a platform that keeps them apart.
pub(super) fn path_key(raw: &str, cwd: Option<&str>) -> Option<String> {
    let fold = |s: &str| {
        let mut out = s.replace('\\', "/");
        if is_rooted(&out) && !out.starts_with('/') {
            out.replace_range(0..1, &out[0..1].to_ascii_lowercase());
        }
        out
    };
    let path = fold(raw);
    let Some(cwd) = cwd.map(fold) else {
        return (!is_rooted(&path)).then_some(path);
    };
    let cwd = cwd.trim_end_matches('/').to_owned();
    if !is_rooted(&path) {
        return Some(path);
    }
    let rest = path.strip_prefix(&cwd)?;
    let rest = rest.strip_prefix('/')?;
    (!rest.is_empty()).then(|| rest.to_owned())
}

/// The bytes of one file, reduced to what a decision needs.
#[derive(Clone, Debug)]
pub(super) struct FileFacts {
    /// The byte length of each line, the newline included.
    line_bytes: Vec<u32>,
    /// The outline the worker would bind at a read of the file.
    outline: Option<std::sync::Arc<OutlineFacts>>,
    /// Whether the file is large enough for the first-contact lever.
    large: bool,
}

impl FileFacts {
    /// How many lines the file has, the way the gate counts them: a terminated
    /// run and an unterminated tail are each one line, and an empty file has
    /// none.
    pub(super) fn total_lines(&self) -> u32 {
        u32::try_from(self.line_bytes.len()).unwrap_or(u32::MAX)
    }

    /// How many bytes the file holds.
    pub(super) fn total_bytes(&self) -> u64 {
        self.line_bytes.iter().map(|n| u64::from(*n)).sum()
    }

    /// The outline the worker would bind, where the file is large enough for
    /// the first-contact lever to consider it.
    pub(super) fn outline(&self) -> Option<&OutlineFacts> {
        if !self.large {
            return None;
        }
        self.outline.as_deref()
    }

    /// The outline the worker binds at any read of the file, whatever its
    /// size: the one a decline resting on another window's read may carry.
    pub(super) fn outline_bound_at_any_read(&self) -> Option<&std::sync::Arc<OutlineFacts>> {
        self.outline.as_ref()
    }

    /// How many bytes the lines `lo..=hi` amount to.
    pub(super) fn slice_bytes(&self, run: Interval) -> usize {
        let lo = (run.lo as usize).saturating_sub(1);
        let hi = run.hi as usize;
        self.line_bytes
            .iter()
            .skip(lo)
            .take(hi.saturating_sub(lo))
            .map(|n| *n as usize)
            .sum()
    }
}

/// Where a run's path facts came from, and how often the history had to
/// hand a path back to the worktree.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FactsSource {
    /// Whether the facts came from each window's commit rather than from the
    /// worktree.
    pub history: bool,
    /// Distinct `(commit, path)` pairs a commit's tree answered.
    pub resolved_at_a_commit: u64,
    /// Distinct `(commit, path)` pairs absent at the commit and present in
    /// the worktree, which the worktree answered.
    pub fell_back_to_the_worktree: u64,
    /// Windows with no commit at or before their first event, whose facts all
    /// came from the worktree.
    pub windows_without_a_commit: u64,
}

/// The worktree's facts about one path, cached for the whole replay.
pub(super) struct Worktree {
    /// The checkout every relative key is resolved under.
    root: PathBuf,
    /// What each key resolved to, or that it resolved to nothing.
    cache: BTreeMap<String, Option<FileFacts>>,
    /// The repository's history, when the run was given one.
    history: Option<History>,
    /// What each `(commit, key)` resolved to.
    at_commit: BTreeMap<(usize, String), Option<FileFacts>>,
    /// Where the facts came from, counted as they are resolved.
    source: FactsSource,
}

impl Worktree {
    pub(super) fn new(root: &Path, history: Option<History>) -> Self {
        Self {
            root: root.to_path_buf(),
            cache: BTreeMap::new(),
            source: FactsSource {
                history: history.is_some(),
                ..FactsSource::default()
            },
            history,
            at_commit: BTreeMap::new(),
        }
    }

    /// The commit a window that began at `stamp` is replayed against, or
    /// `None` without a history or without a commit that early — and in the
    /// second case the window is counted.
    pub(super) fn commit_for(&mut self, stamp: Option<&str>) -> Option<usize> {
        let history = self.history.as_ref()?;
        let commit = stamp
            .and_then(super::history::unix_seconds)
            .and_then(|at| history.commit_at(at));
        if commit.is_none() {
            self.source.windows_without_a_commit =
                self.source.windows_without_a_commit.saturating_add(1);
        }
        commit
    }

    /// Where this run's facts came from.
    ///
    /// # Errors
    /// If the history's batch reader failed part-way, which would leave every
    /// later path absent and the figures quietly wrong.
    pub(super) fn source(&self) -> Result<FactsSource, String> {
        match self.history.as_ref().and_then(History::failure) {
            Some(failure) => Err(failure.to_owned()),
            None => Ok(self.source),
        }
    }

    /// The file a key names as a window replayed against `commit` sees it:
    /// that commit's tree, or the worktree where the commit has no such path
    /// and the worktree does, or where there is no commit at all.
    pub(super) fn facts_at(&mut self, commit: Option<usize>, key: &str) -> Option<&FileFacts> {
        let Some(commit) = commit else {
            return self.facts(key);
        };
        let slot = (commit, key.to_owned());
        if !self.at_commit.contains_key(&slot) {
            let found = self.resolve_at(commit, key);
            self.at_commit.insert(slot.clone(), found);
        }
        self.at_commit.get(&slot).and_then(Option::as_ref)
    }

    /// Resolve one key at one commit, falling back to the worktree only where
    /// the commit has nothing at the path and the worktree has a file there.
    fn resolve_at(&mut self, commit: usize, key: &str) -> Option<FileFacts> {
        let blob = self.history.as_mut()?.blob(commit, key);
        match blob {
            Blob::Present(bytes) => {
                self.source.resolved_at_a_commit =
                    self.source.resolved_at_a_commit.saturating_add(1);
                facts_of(&bytes, key)
            }
            Blob::Absent => {
                let now = self.facts(key).cloned();
                if now.is_some() {
                    self.source.fell_back_to_the_worktree =
                        self.source.fell_back_to_the_worktree.saturating_add(1);
                }
                now
            }
        }
    }

    /// The bytes a key names as a window replayed against `commit` sees
    /// them, with the same fallback and refusals as [`Self::facts_at`].
    pub(super) fn bytes_at(&mut self, commit: Option<usize>, key: &str) -> Option<Vec<u8>> {
        let at_commit = match (commit, self.history.as_mut()) {
            (Some(commit), Some(history)) => match history.blob(commit, key) {
                Blob::Present(bytes) => Some(bytes),
                Blob::Absent => None,
            },
            _ => None,
        };
        let bytes = match at_commit {
            Some(bytes) => bytes,
            None => read_bytes(&self.root, key)?,
        };
        facts_of(&bytes, key).map(|_| bytes)
    }

    /// The file a key names, or `None` when this checkout has nothing to say
    /// about it — no such file, too large to hash inline, or not text.
    pub(super) fn facts(&mut self, key: &str) -> Option<&FileFacts> {
        if !self.cache.contains_key(key) {
            self.cache
                .insert(key.to_owned(), read_facts(&self.root, key));
        }
        self.cache.get(key).and_then(Option::as_ref)
    }
}

/// Read one file's facts, applying the same two refusals the gate applies.
fn read_facts(root: &Path, key: &str) -> Option<FileFacts> {
    let path = if is_rooted(key) {
        PathBuf::from(key)
    } else {
        root.join(key)
    };
    let meta = std::fs::metadata(&path).ok()?;
    if !meta.is_file() || meta.len() > MAX_INLINE_HASH_BYTES {
        return None;
    }
    let bytes = std::fs::read(&path).ok()?;
    facts_of(&bytes, key)
}

/// One file's bytes under the checkout, within the inline-hash cap.
fn read_bytes(root: &Path, key: &str) -> Option<Vec<u8>> {
    let path = if is_rooted(key) {
        PathBuf::from(key)
    } else {
        root.join(key)
    };
    let meta = std::fs::metadata(&path).ok()?;
    if !meta.is_file() || meta.len() > MAX_INLINE_HASH_BYTES {
        return None;
    }
    std::fs::read(&path).ok()
}

/// One file's facts from its bytes, refusing what the gate refuses: a file too
/// large to hash inline, or one that is not text.
fn facts_of(bytes: &[u8], key: &str) -> Option<FileFacts> {
    let too_large = u64::try_from(bytes.len()).map_or(true, |n| n > MAX_INLINE_HASH_BYTES);
    if too_large || bytes.contains(&0) {
        return None;
    }
    Some(FileFacts {
        line_bytes: bytes
            .split_inclusive(|byte| *byte == b'\n')
            .map(|line| u32::try_from(line.len()).unwrap_or(u32::MAX))
            .collect(),
        outline: outline_facts(key, bytes),
        large: is_first_contact_size(bytes),
    })
}

#[cfg(test)]
mod tests {
    use super::path_key;

    #[test]
    fn a_path_outside_the_working_directory_has_no_key() {
        assert_eq!(
            path_key("C:\\work\\repo\\src\\lib.rs", Some("c:\\work\\repo")),
            Some("src/lib.rs".to_owned())
        );
        assert_eq!(
            path_key("src/lib.rs", Some("/repo")),
            Some("src/lib.rs".to_owned())
        );
        assert_eq!(path_key("/elsewhere/x.txt", Some("/repo")), None);
        assert_eq!(path_key("/repo", Some("/repo")), None);
    }
}
