//! What this checkout can say about a path a transcript named.
//!
//! A replay has no file at the instant of the call, so the worktree stands in
//! for one, under the same two refusals the gate applies.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use rift_agent::window_journal::Interval;

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
}

impl FileFacts {
    /// How many lines the file has, the way the gate counts them: a terminated
    /// run and an unterminated tail are each one line, and an empty file has
    /// none.
    pub(super) fn total_lines(&self) -> u32 {
        u32::try_from(self.line_bytes.len()).unwrap_or(u32::MAX)
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

/// The worktree's facts about one path, cached for the whole replay.
pub(super) struct Worktree {
    /// The checkout every relative key is resolved under.
    root: PathBuf,
    /// What each key resolved to, or that it resolved to nothing.
    cache: BTreeMap<String, Option<FileFacts>>,
}

impl Worktree {
    pub(super) fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
            cache: BTreeMap::new(),
        }
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
    if bytes.contains(&0) {
        return None;
    }
    Some(FileFacts {
        line_bytes: bytes
            .split_inclusive(|byte| *byte == b'\n')
            .map(|line| u32::try_from(line.len()).unwrap_or(u32::MAX))
            .collect(),
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
