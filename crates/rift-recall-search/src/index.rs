//! The path index and the key that says whether it is still true.
//!
//! The index is the list of paths this checkout holds, with the modification
//! time each had when the list was taken. It is a cache and never a record:
//! the only question asked of it is "is this still the file list", and the
//! only two answers are *yes* and *rebuild*.
//!
//! ## What the key is, and why it is not the newest file mtime
//!
//! An index of **paths** stops being true when a path appears, disappears or
//! moves. `HEAD` moving is one way that happens — a checkout rewrites the
//! tracked set — and the other is the developer or an agent creating, deleting
//! or renaming a file. Both change the modification time of the *directory*
//! that holds the entry, on every filesystem this ships to, and neither is
//! hidden by an editor writing a file in place.
//!
//! The newest *file* mtime would also move on a plain content edit, which
//! changes no path at all: an index thrown away on every save is an index
//! nobody has. It would also be ruinous to check. Freshness is asked on the
//! cold path, where the budget is milliseconds; statting every file to answer
//! it costs what rebuilding costs, so the answer would arrive after the work it
//! was meant to save. Statting directories is a walk of the tree's skeleton,
//! and there are two orders of magnitude fewer of them.
//!
//! So the key is `(HEAD oid, newest directory mtime)`, and what it is deaf to
//! is stated rather than hidden: a file whose bytes changed keeps its recorded
//! mtime until the next rebuild, so its recency contribution ages by at most
//! the gap between rebuilds — session start and commit — while its *presence*
//! in the list stays exactly right.
//!
//! ## The file
//!
//! ```text
//! .rift/state/search/index.bin
//! ```
//!
//! A fixed little-endian layout rather than a serialisation format: this file
//! is written by one program, read by the same program and thrown away when it
//! disagrees, so a format would buy nothing and cost a dependency in a binary
//! that is measured in kilobytes. Every read is bounds-checked and every
//! malformed byte ends as [`PathIndex::decode`] returning `None`, which the
//! callers treat exactly as they treat a missing file.

use std::path::Path;

use crate::SearchError;

/// The layout this build writes and the only one it reads.
///
/// A version that does not match is not upgraded in place: the index is
/// rebuilt, because it is derivable in milliseconds and a migration for a
/// cache is a liability with no upside.
pub const INDEX_VERSION: u16 = 1;

/// The first eight bytes of every index file.
const MAGIC: &[u8; 8] = b"RIFTSRC1";

/// The most paths this build will index or accept from a file.
///
/// A ceiling on memory rather than an opinion about repositories: at the cap
/// the decoded index is a few tens of megabytes, and a repository past it is
/// one whose file list was never going to be an answer an agent could read.
pub const MAX_PATHS: usize = 500_000;

/// The most bytes an index file is read at all.
///
/// The decoder bounds every count it reads, but the file is read whole
/// before it is decoded, and it sits where the process a hook gates can
/// write it. Past this size the file is no index — the same answer a damaged
/// one gets — and the read that would have paid for it never starts.
pub const MAX_INDEX_BYTES: u64 = 64 * 1_024 * 1_024;

/// The longest path this build will index or accept from a file.
const MAX_PATH_BYTES: usize = 4_096;

/// The longest object id this build will accept as part of a key.
const MAX_OID_BYTES: usize = 64;

/// What the index was taken against.
///
/// Two facts, compared as a pair: an index is fresh when both still hold, and
/// stale — which means *absent* — the moment either moves.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IndexKey {
    /// `HEAD`'s object id at the time of the walk, or the empty string on a
    /// repository with no commits. Compared as text, never parsed.
    pub head_oid: String,
    /// The newest modification time of any directory the walk entered, in
    /// milliseconds. See the module documentation for why this is directories.
    pub newest_dir_mtime_ms: u64,
}

/// One path the checkout holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileEntry {
    /// The path relative to the work tree, in the same spelling the window
    /// journal keys coverage by, with `/` separators on every platform.
    pub path: String,
    /// Its modification time in milliseconds at the time of the walk.
    pub mtime_ms: u64,
}

/// The list of paths a checkout held, and what it was taken against.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PathIndex {
    key: IndexKey,
    entries: Vec<FileEntry>,
}

impl PathIndex {
    /// Build an index from a walk's entries.
    ///
    /// The entries are sorted by path and de-duplicated here rather than at the
    /// callers, because *every* consumer of this list depends on the order:
    /// ranking breaks ties by position, so two runs that disagreed about the
    /// order of two equally-scored paths would rank them differently.
    ///
    /// # Errors
    ///
    /// [`SearchError::TooManyPaths`] when the walk offered more than
    /// [`MAX_PATHS`] entries; the caller's walk is expected to stop at the cap
    /// rather than reach this.
    pub fn new(key: IndexKey, mut entries: Vec<FileEntry>) -> Result<Self, SearchError> {
        if entries.len() > MAX_PATHS {
            return Err(SearchError::TooManyPaths {
                found: entries.len(),
                cap: MAX_PATHS,
            });
        }
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        entries.dedup_by(|a, b| a.path == b.path);
        Ok(Self { key, entries })
    }

    /// What this index was taken against.
    #[must_use]
    pub fn key(&self) -> &IndexKey {
        &self.key
    }

    /// The paths, in the one order every reader sees them in.
    #[must_use]
    pub fn entries(&self) -> &[FileEntry] {
        &self.entries
    }

    /// Whether this path is one the checkout held when the index was taken.
    ///
    /// A membership test rather than a scan, because the entries are sorted:
    /// this is how a caller checks that a path it was handed — by a model, by
    /// a ranked answer it is re-using — is one this crate is allowed to open.
    #[must_use]
    pub fn contains(&self, path: &str) -> bool {
        self.entries
            .binary_search_by(|entry| entry.path.as_str().cmp(path))
            .is_ok()
    }

    /// Whether this index still answers for a checkout in the given state.
    #[must_use]
    pub fn is_fresh_for(&self, key: &IndexKey) -> bool {
        self.key == *key
    }

    /// The index as bytes.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(64 + self.entries.len() * 48);
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&INDEX_VERSION.to_le_bytes());
        let oid = self.key.head_oid.as_bytes();
        out.extend_from_slice(&(oid.len() as u16).to_le_bytes());
        out.extend_from_slice(oid);
        out.extend_from_slice(&self.key.newest_dir_mtime_ms.to_le_bytes());
        out.extend_from_slice(&(self.entries.len() as u32).to_le_bytes());
        for entry in &self.entries {
            let path = entry.path.as_bytes();
            out.extend_from_slice(&(path.len() as u16).to_le_bytes());
            out.extend_from_slice(path);
            out.extend_from_slice(&entry.mtime_ms.to_le_bytes());
        }
        out
    }

    /// The index those bytes stand for, or `None` for anything else.
    ///
    /// "Anything else" is deliberately every failure at once: a truncated
    /// file, a file from another version, a length that promises more bytes
    /// than the file has, a path that is not UTF-8, trailing bytes after the
    /// last entry. Each of them means the same thing to a caller — walk
    /// instead — so none of them is worth a variant, and returning `None` for
    /// all of them is what makes a hostile file a rebuild rather than a panic.
    #[must_use]
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        let mut cursor = Cursor::new(bytes);
        if cursor.take(MAGIC.len())? != MAGIC.as_slice() {
            return None;
        }
        if cursor.u16()? != INDEX_VERSION {
            return None;
        }
        let oid_len = usize::from(cursor.u16()?);
        if oid_len > MAX_OID_BYTES {
            return None;
        }
        let head_oid = std::str::from_utf8(cursor.take(oid_len)?).ok()?.to_owned();
        let newest_dir_mtime_ms = cursor.u64()?;
        let count = cursor.u32()? as usize;
        if count > MAX_PATHS {
            return None;
        }
        let mut entries = Vec::with_capacity(count.min(4_096));
        for _ in 0..count {
            let path_len = usize::from(cursor.u16()?);
            if path_len == 0 || path_len > MAX_PATH_BYTES {
                return None;
            }
            let path = std::str::from_utf8(cursor.take(path_len)?).ok()?.to_owned();
            entries.push(FileEntry {
                path,
                mtime_ms: cursor.u64()?,
            });
        }
        if !cursor.at_end() {
            return None;
        }
        // Sorted on the way in, so a file that arrives unsorted — hand-written,
        // or written by a build whose ordering differed — is rebuilt rather
        // than trusted: order is part of what the ranking pins.
        if entries.windows(2).any(|pair| match pair {
            [a, b] => a.path >= b.path,
            _ => false,
        }) {
            return None;
        }
        Some(Self {
            key: IndexKey {
                head_oid,
                newest_dir_mtime_ms,
            },
            entries,
        })
    }

    /// The key the saved index at `path` was taken against, read from its
    /// header alone.
    ///
    /// The whole point is what it does **not** do: it decodes no entries, so a
    /// caller with a hook's budget can learn what tree state this checkout was
    /// last indexed at for one small read instead of a walk of the tree's
    /// skeleton. It is the identity a deferred measurement is keyed by — the
    /// tree the search it measures actually ran against — and a caller that
    /// gets `None` has no key rather than a guessed one.
    ///
    /// Every failure is `None`, for the reason [`Self::decode`] gives.
    #[must_use]
    pub fn saved_key(path: &Path) -> Option<IndexKey> {
        // The header is eight bytes of magic, two of version, two of oid
        // length, the oid, and eight of mtime — never more than this, whatever
        // the file claims about its entries.
        const HEADER_BYTES: usize = MAGIC.len() + 2 + 2 + MAX_OID_BYTES + 8;
        use std::io::Read as _;
        let mut head = Vec::with_capacity(HEADER_BYTES);
        std::fs::File::open(path)
            .ok()?
            .take(HEADER_BYTES as u64)
            .read_to_end(&mut head)
            .ok()?;
        let mut cursor = Cursor::new(&head);
        if cursor.take(MAGIC.len())? != MAGIC.as_slice() || cursor.u16()? != INDEX_VERSION {
            return None;
        }
        let oid_len = usize::from(cursor.u16()?);
        if oid_len > MAX_OID_BYTES {
            return None;
        }
        let head_oid = std::str::from_utf8(cursor.take(oid_len)?).ok()?.to_owned();
        Some(IndexKey {
            head_oid,
            newest_dir_mtime_ms: cursor.u64()?,
        })
    }

    /// Read the index at `path`, if it is there and still answers for `key`.
    ///
    /// A missing file, an unreadable one, a corrupt one and a stale one are one
    /// answer: `None`.
    #[must_use]
    pub fn load(path: &Path, key: &IndexKey) -> Option<Self> {
        if std::fs::metadata(path).ok()?.len() > MAX_INDEX_BYTES {
            return None;
        }
        let bytes = std::fs::read(path).ok()?;
        let index = Self::decode(&bytes)?;
        index.is_fresh_for(key).then_some(index)
    }

    /// Write the index to `path`, through a temporary name in the same
    /// directory so a reader never opens a half-written one.
    ///
    /// # Errors
    ///
    /// [`SearchError::Write`] for any I/O failure, which every caller treats as
    /// "the index was not saved" — the work it stood for is still valid in
    /// memory, and the next rebuild will try again.
    pub fn save(&self, path: &Path) -> Result<(), SearchError> {
        let fail = || SearchError::Write {
            path: path.display().to_string(),
        };
        let dir = path.parent().ok_or_else(fail)?;
        std::fs::create_dir_all(dir).map_err(|_| fail())?;
        let tmp = dir.join("index.bin.tmp");
        std::fs::write(&tmp, self.encode()).map_err(|_| fail())?;
        std::fs::rename(&tmp, path).map_err(|_| {
            let _ = std::fs::remove_file(&tmp);
            fail()
        })
    }
}

/// A bounds-checked reader over the index file's bytes.
struct Cursor<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Cursor<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, at: 0 }
    }

    fn take(&mut self, len: usize) -> Option<&'a [u8]> {
        let end = self.at.checked_add(len)?;
        let slice = self.bytes.get(self.at..end)?;
        self.at = end;
        Some(slice)
    }

    fn u16(&mut self) -> Option<u16> {
        Some(u16::from_le_bytes(<[u8; 2]>::try_from(self.take(2)?).ok()?))
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(<[u8; 4]>::try_from(self.take(4)?).ok()?))
    }

    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(<[u8; 8]>::try_from(self.take(8)?).ok()?))
    }

    const fn at_end(&self) -> bool {
        self.at == self.bytes.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str, mtime_ms: u64) -> FileEntry {
        FileEntry {
            path: path.to_owned(),
            mtime_ms,
        }
    }

    fn key() -> IndexKey {
        IndexKey {
            head_oid: "a".repeat(40),
            newest_dir_mtime_ms: 1_757_000_000_000,
        }
    }

    fn index() -> PathIndex {
        PathIndex::new(
            key(),
            vec![
                entry("src/main.rs", 12),
                entry("Cargo.toml", 7),
                entry("docs/guide/00-start-here.md", 9),
            ],
        )
        .unwrap()
    }

    /// The header read is the whole-file read's answer, without the entries:
    /// what a caller with a hook's budget may learn about this checkout's tree
    /// state. A file that is not an index has no key, whatever it is.
    #[test]
    fn the_saved_key_is_read_from_the_header_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("index.bin");
        index().save(&path).unwrap();
        assert_eq!(PathIndex::saved_key(&path), Some(key()));
        assert_eq!(
            PathIndex::saved_key(&path).as_ref(),
            PathIndex::decode(&std::fs::read(&path).unwrap())
                .as_ref()
                .map(PathIndex::key),
            "the cheap read and the whole read agree"
        );
        std::fs::write(&path, b"not an index at all").unwrap();
        assert_eq!(PathIndex::saved_key(&path), None);
        assert_eq!(PathIndex::saved_key(&tmp.path().join("absent.bin")), None);
    }

    /// The list a walk hands over is stored in one order, whatever order it
    /// arrived in, because ranking breaks ties by position: two runs that
    /// disagreed about the order of two equally-scored paths would answer the
    /// same query differently.
    #[test]
    fn entries_are_stored_sorted_and_deduplicated() {
        let index = PathIndex::new(
            key(),
            vec![entry("b", 2), entry("a", 1), entry("b", 3), entry("a", 4)],
        )
        .unwrap();
        let paths: Vec<&str> = index.entries().iter().map(|e| e.path.as_str()).collect();
        assert_eq!(paths, vec!["a", "b"]);
    }

    /// The bytes round-trip exactly, key included.
    #[test]
    fn an_index_survives_a_write_and_a_read() {
        let original = index();
        let decoded = PathIndex::decode(&original.encode()).expect("round trip");
        assert_eq!(decoded, original);
        assert_eq!(decoded.key(), &key());
    }

    /// Every way the file can be wrong is the same answer: no index. None of
    /// them is a panic, and none of them is a partly-decoded list.
    /// The read is bounded before it starts: a file past the size bound is
    /// refused unread, the same answer a damaged one gets, so a writable
    /// index path cannot make the hook pay for bytes it will never decode.
    #[test]
    fn an_index_past_the_size_bound_is_refused_unread() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("index.bin");
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(MAX_INDEX_BYTES + 1).unwrap();
        drop(file);
        let key = IndexKey {
            head_oid: "0".repeat(40),
            newest_dir_mtime_ms: 0,
        };
        assert!(PathIndex::load(&path, &key).is_none());
    }

    #[test]
    fn a_damaged_index_is_no_index_at_all() {
        let good = index().encode();
        assert!(PathIndex::decode(&[]).is_none(), "empty file");
        assert!(PathIndex::decode(b"not an index at all").is_none(), "magic");
        for cut in 0..good.len() {
            assert!(
                PathIndex::decode(&good[..cut]).is_none(),
                "a file cut at {cut} bytes decoded"
            );
        }
        let mut trailing = good.clone();
        trailing.push(0);
        assert!(PathIndex::decode(&trailing).is_none(), "trailing bytes");
        let mut wrong_version = good.clone();
        wrong_version[8] = 9;
        assert!(PathIndex::decode(&wrong_version).is_none(), "version");
        let mut huge_count = good.clone();
        // The four count bytes sit after the magic, the version, the oid
        // length, the oid itself and the mtime.
        let count_at = 8 + 2 + 2 + 40 + 8;
        huge_count[count_at..count_at + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(PathIndex::decode(&huge_count).is_none(), "count");
    }

    /// A file whose entries are not in the one order this crate writes is
    /// rebuilt rather than trusted.
    #[test]
    fn an_index_whose_order_was_tampered_with_is_refused() {
        let unsorted = PathIndex {
            key: key(),
            entries: vec![entry("b", 1), entry("a", 1)],
        };
        assert!(PathIndex::decode(&unsorted.encode()).is_none());
    }

    /// The key is the whole freshness question: either half moving is a
    /// rebuild, and a caller asking with the wrong key gets nothing back even
    /// though the file is perfectly readable.
    #[test]
    fn an_index_answers_only_for_the_state_it_was_taken_in() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("search").join("index.bin");
        index().save(&path).unwrap();

        assert_eq!(PathIndex::load(&path, &key()), Some(index()));
        let moved_head = IndexKey {
            head_oid: "b".repeat(40),
            ..key()
        };
        assert_eq!(PathIndex::load(&path, &moved_head), None, "HEAD moved");
        let moved_mtime = IndexKey {
            newest_dir_mtime_ms: key().newest_dir_mtime_ms + 1,
            ..key()
        };
        assert_eq!(PathIndex::load(&path, &moved_mtime), None, "a path moved");
        assert_eq!(
            PathIndex::load(&dir.path().join("absent.bin"), &key()),
            None,
            "no index"
        );
    }

    /// A save leaves the directory holding the index and nothing else: the
    /// temporary name is renamed into place, never left behind for a reader to
    /// find.
    #[test]
    fn a_saved_index_leaves_no_partial_file_behind() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("search").join("index.bin");
        index().save(&path).unwrap();
        index().save(&path).unwrap();
        let names: Vec<String> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["index.bin".to_owned()]);
    }

    /// More paths than the cap is refused rather than written: the number is a
    /// ceiling on what a decode may allocate, so the encoder must not be able
    /// to produce a file the decoder would reject.
    #[test]
    fn an_index_over_the_path_cap_is_refused() {
        let too_many = (0..=MAX_PATHS).map(|i| entry(&i.to_string(), 0)).collect();
        assert_eq!(
            PathIndex::new(key(), too_many),
            Err(SearchError::TooManyPaths {
                found: MAX_PATHS + 1,
                cap: MAX_PATHS,
            })
        );
    }
}
