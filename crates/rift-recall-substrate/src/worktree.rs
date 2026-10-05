//! Worktree hashing for the avoidable-context metric (scoped to what the
//! ledger needs).
//!
//! The ledger's `avoidable_context_rate` compares each read's
//! `content_sha256` against the current worktree's hash for that path. The hash
//! here MUST therefore be a **plain SHA-256 of the file bytes** — the same
//! function capture records at read time — not the domain-separated
//! `rift-hashchain` leaf and not blake3. We only hash the paths that were
//! actually read (cheap, and all the metric needs); whole-worktree hashing for
//! the code graph is a later concern.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use sha2::{Digest, Sha256};

/// Plain lowercase-hex SHA-256 of `bytes` (matches read-time `content_sha256`).
#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for b in digest {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// The git blob object id of `bytes`: SHA-1 over the loose-object framing
/// `"blob <len>\0" ‖ bytes` — exactly what `git hash-object` produces.
/// This is git's **default** (SHA-1) object hash; a SHA-256-format repo's oids
/// differ, so a `file:` handle then simply misses (honest absence), rather
/// than serving wrong bytes. The lookup is always self-validated against the
/// requested `content_sha256` before serving.
#[must_use]
pub fn git_blob_oid(bytes: &[u8]) -> String {
    // sha1::Sha1 implements the same `digest::Digest` trait re-exported by sha2,
    // already in scope — no second trait import needed.
    let mut h = sha1::Sha1::new();
    h.update(format!("blob {}\0", bytes.len()).as_bytes());
    h.update(bytes);
    let out = h.finalize();
    let mut s = String::with_capacity(40);
    use core::fmt::Write as _;
    for b in out {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// One worktree file's dual identity: the plain SHA-256 content hash (the
/// validity comparator, what capture records at read time) and the git blob
/// object id (the substrate lookup key). The content→blob-oid map is built from these.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorktreeBlob {
    /// Repo-relative path.
    pub path: String,
    /// Plain SHA-256 of the current bytes.
    pub content_sha256: String,
    /// Git blob object id (SHA-1 framing) of the current bytes.
    pub blob_oid: String,
}

/// The file a caller named, as the checkout holds it now — or the reason it
/// could not be read.
///
/// This is what every answer from a reader bound to a checkout stands on when
/// it says *current* or *unchanged*: a projection records the revisions a
/// checkpoint carried, and an edit made after the last checkpoint is not among
/// them, so the projection's newest revision is a statement about the past.
/// Only the bytes on disk say what the file is now, and when they cannot be
/// read the honest answer is that nobody knows, never the projection's guess.
/// A reader bound to no checkout is refused with [`CurrentRefusal::NoWorktree`],
/// and an answer given without these bytes is the projection's view, which
/// credits nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CurrentFile {
    /// The bytes on disk and their plain SHA-256.
    Present {
        /// Plain SHA-256 of `bytes`, the same function capture records.
        content_sha256: String,
        /// The file's bytes, read once so hashing and use cannot disagree.
        bytes: Vec<u8>,
    },
    /// Why the file as it stands now could not be read.
    Refused(CurrentRefusal),
}

/// Why a path's current bytes could not be read: a closed set, each with the
/// tag an answer carries so a caller knows to fall back to a plain read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CurrentRefusal {
    /// The reader is bound to no checkout.
    NoWorktree,
    /// The path is not a safe repository-relative name.
    UnsafePath,
    /// Nothing is at the path.
    Missing,
    /// The path is a symbolic link; what it points at is not this path.
    Symlink,
    /// The path is a directory or another non-file.
    NotAFile,
    /// The path resolves outside the checkout, through a linked directory.
    OutsideRoot,
    /// The file is larger than the caller will hash.
    Oversize,
    /// The file is there but could not be read.
    Unreadable,
}

impl CurrentRefusal {
    /// The tag an answer carries for this refusal.
    #[must_use]
    pub fn tag(self) -> &'static str {
        match self {
            Self::NoWorktree => "no_worktree",
            Self::UnsafePath => "unsafe_path",
            Self::Missing => "missing",
            Self::Symlink => "symlink",
            Self::NotAFile => "not_a_file",
            Self::OutsideRoot => "outside_root",
            Self::Oversize => "oversize",
            Self::Unreadable => "unreadable",
        }
    }
}

/// Hash the current worktree content for `paths`, returning each existing file's
/// (`content_sha256`, `blob_oid`) — the input to both the avoidable-context
/// metric and the content→blob-oid map. Same skip rules as
/// [`hash_paths`] (missing/unsafe/dir/oversize paths are omitted, never a panic).
#[must_use]
pub fn hash_blobs(repo_root: &Path, paths: &BTreeSet<String>) -> Vec<WorktreeBlob> {
    let mut out = Vec::new();
    for path in paths {
        if let Some((blob, _)) = hash_blob_bytes(repo_root, path) {
            out.push(blob);
        }
    }
    out
}

/// Hash ONE worktree file and hand back the bytes alongside its identities —
/// the single-read form for callers that need the content too (the token
/// estimate), so hashing and consuming can never race the filesystem. Same
/// skip rules as [`hash_paths`]: an unsafe/missing/non-file/oversize path is
/// `None`, never a panic.
#[must_use]
pub fn hash_blob_bytes(repo_root: &Path, path: &str) -> Option<(WorktreeBlob, Vec<u8>)> {
    if !path_is_safe(path) {
        return None;
    }
    let full = repo_root.join(path);
    let meta = std::fs::symlink_metadata(&full).ok()?;
    if !meta.is_file() || meta.len() > crate::parse::MAX_ARTIFACT_BYTES as u64 {
        return None;
    }
    let bytes = std::fs::read(&full).ok()?;
    let blob = WorktreeBlob {
        path: path.to_owned(),
        content_sha256: sha256_hex(&bytes),
        blob_oid: git_blob_oid(&bytes),
    };
    Some((blob, bytes))
}

/// The file at `path` under `repo_root` as it stands now, refused rather than
/// guessed: an unsafe name, nothing there, a symbolic link, a non-file, a path
/// that leaves the checkout through a linked directory, or more than
/// `max_bytes` each answer with the reason.
///
/// Separate from [`hash_blob_bytes`] on purpose: that one answers `None` for
/// every refusal and sits on a hook's path, where the reason is not wanted
/// and an extra `canonicalize` is latency nobody asked for.
#[must_use]
pub fn current_file(repo_root: &Path, path: &str, max_bytes: u64) -> CurrentFile {
    match current_bytes(repo_root, path, max_bytes) {
        Ok(bytes) => CurrentFile::Present {
            content_sha256: sha256_hex(&bytes),
            bytes,
        },
        Err(refusal) => CurrentFile::Refused(refusal),
    }
}

/// A repo-relative path is safe to join under the repo root: non-empty, not
/// absolute, no `..` segment, no NUL.
///
/// Segments are split on both separators: `Path::join` on Windows treats
/// `..\x` as a parent step exactly as it treats `../x`, so a check that only
/// read `/` let the former escape the tree.
fn path_is_safe(p: &str) -> bool {
    !p.is_empty()
        && !p.starts_with('/')
        && !p.starts_with('\\')
        && !p.contains('\0')
        && !p.split(['/', '\\']).any(|seg| seg == "..")
}

/// The `/`-spelled form of an already-repository-relative name: **the one
/// normaliser** every recall path passes through, on the way into a row and on
/// the way into a lookup.
///
/// Purely textual, so it holds wherever the string came from — a Windows
/// `strip_prefix`, a shard archived on a Windows machine and rebuilt on Linux,
/// or a caller who typed `bins\rift\src\hooks.rs` at a prompt. That is the
/// point: "repository-relative paths are spelled with `/` on every platform"
/// has to be a property of the *string*, not of the host, or a projection
/// rebuilt on a second machine disagrees with the one it was copied from.
///
/// Borrowed unchanged when there is nothing to re-spell, which is every path
/// on a healthy tree. The trade this makes explicit: a POSIX file whose name
/// genuinely contains a backslash is re-keyed too. It is a spelling Rift could
/// not serve anyway — the pushed alphabet refuses `\` — and re-keying it keeps
/// rows and lookups agreeing with each other, which is what a caller can act
/// on.
#[must_use]
pub fn slash_spelled(path: &str) -> Cow<'_, str> {
    if path.contains('\\') {
        Cow::Owned(path.replace('\\', "/"))
    } else {
        Cow::Borrowed(path)
    }
}

/// The repository-relative, `/`-spelled name of `path` under `root`: the one
/// spelling every persisted, hashed or served path carries on every platform.
///
/// `path` is stripped of `root` and its remaining components joined with `/`,
/// each component itself [`slash_spelled`], so a path that came from
/// `strip_prefix`, `canonicalize` or a payload spelled with `\` is keyed the
/// way the ledger, the sidecar links and the served envelopes are keyed — on
/// Windows, where `\` is a component separator, and equally on a platform
/// where it is an ordinary byte the shard carried over from another machine.
/// `None` when `path` is not under `root`, is
/// `root` itself, is not UTF-8, or is still absolute once `root` is stripped.
/// An empty `root` relativises nothing and only re-spells a relative path.
#[must_use]
pub fn repo_relative_slash(root: &Path, path: &Path) -> Option<String> {
    use std::path::Component;
    let rel = path.strip_prefix(root).ok()?;
    let mut out = String::new();
    for component in rel.components() {
        let part = match component {
            Component::Normal(part) => part.to_str()?,
            Component::CurDir | Component::ParentDir => component.as_os_str().to_str()?,
            Component::Prefix(_) | Component::RootDir => return None,
        };
        if !out.is_empty() {
            out.push('/');
        }
        out.push_str(&slash_spelled(part));
    }
    (!out.is_empty()).then_some(out)
}

/// Hash the current worktree content for exactly `paths` (repo-relative), keyed
/// by path. A path that is missing, unsafe, a directory, or over the size cap is
/// simply omitted (it then counts as "changed/absent" in the metric — never a
/// panic). The returned map is the `worktree` argument to the ledger metrics.
#[must_use]
pub fn hash_paths(repo_root: &Path, paths: &BTreeSet<String>) -> BTreeMap<String, String> {
    hash_blobs(repo_root, paths)
        .into_iter()
        .map(|b| (b.path, b.content_sha256))
        .collect()
}

/// The checks [`current_file`] makes, in the order that keeps each refusal
/// honest: the name before the disk, the link before its target, and the size
/// before the read.
fn current_bytes(
    repo_root: &Path,
    path: &str,
    max_bytes: u64,
) -> std::result::Result<Vec<u8>, CurrentRefusal> {
    if !path_is_safe(path) {
        return Err(CurrentRefusal::UnsafePath);
    }
    let full = repo_root.join(path);
    let meta = match std::fs::symlink_metadata(&full) {
        Ok(meta) => meta,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Err(CurrentRefusal::Missing)
        }
        Err(_) => return Err(CurrentRefusal::Unreadable),
    };
    if meta.file_type().is_symlink() {
        return Err(CurrentRefusal::Symlink);
    }
    if !meta.is_file() {
        return Err(CurrentRefusal::NotAFile);
    }
    if meta.len() > max_bytes {
        return Err(CurrentRefusal::Oversize);
    }
    let (Ok(root), Ok(resolved)) = (repo_root.canonicalize(), full.canonicalize()) else {
        return Err(CurrentRefusal::Unreadable);
    };
    if !resolved.starts_with(&root) {
        return Err(CurrentRefusal::OutsideRoot);
    }
    let Ok(bytes) = std::fs::read(&full) else {
        return Err(CurrentRefusal::Unreadable);
    };
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > max_bytes {
        return Err(CurrentRefusal::Oversize);
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_matches_known_vector() {
        // SHA-256("abc")
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn hashes_only_requested_existing_paths() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), b"hello").unwrap();
        let paths: BTreeSet<String> = ["a.txt", "missing.txt"]
            .into_iter()
            .map(String::from)
            .collect();
        let map = hash_paths(dir.path(), &paths);
        assert_eq!(map.get("a.txt"), Some(&sha256_hex(b"hello")));
        assert!(!map.contains_key("missing.txt"), "absent paths are omitted");
    }

    #[test]
    fn rejects_unsafe_paths() {
        let dir = tempfile::tempdir().unwrap();
        let paths: BTreeSet<String> = ["../escape", "/abs", ""]
            .into_iter()
            .map(String::from)
            .collect();
        assert!(hash_paths(dir.path(), &paths).is_empty());
    }

    /// A parent step spelled with either separator is refused: on Windows
    /// `Path::join` reads `..\x` exactly as `../x`, so a check that read only
    /// `/` let it out of the tree.
    #[test]
    fn a_parent_step_is_refused_with_either_separator() {
        for unsafe_path in ["../x", "..\\x", "a/../x", "a\\..\\x", "\\abs", ".."] {
            assert!(!path_is_safe(unsafe_path), "{unsafe_path}");
        }
        for safe_path in ["a..b/c", "a/..b", "src/main.rs"] {
            assert!(path_is_safe(safe_path), "{safe_path}");
        }
    }

    /// The one spelling every persisted, hashed or served path carries: under
    /// the root, joined with `/`, whichever separator the OS path used.
    #[test]
    fn a_repository_relative_path_is_spelled_with_slashes() {
        let root = Path::new("root");
        let nested = root.join("src").join("a").join("mod.rs");
        assert_eq!(
            repo_relative_slash(root, &nested).as_deref(),
            Some("src/a/mod.rs")
        );
        assert_eq!(
            repo_relative_slash(root, root),
            None,
            "the root has no name"
        );
        assert_eq!(
            repo_relative_slash(root, Path::new("elsewhere")),
            None,
            "a path outside the root is not relative to it"
        );
        let relative = Path::new("src").join("b.rs");
        assert_eq!(
            repo_relative_slash(Path::new(""), &relative).as_deref(),
            Some("src/b.rs"),
            "an empty root only re-spells"
        );
    }

    /// The Windows arm of the same fact: a backslash spelling, whether from
    /// `strip_prefix` or from a payload, is re-keyed with `/`.
    #[cfg(windows)]
    #[test]
    fn a_backslash_spelling_is_rekeyed_with_slashes() {
        assert_eq!(
            repo_relative_slash(Path::new(r"C:\root"), Path::new(r"C:\root\src\a.rs")).as_deref(),
            Some("src/a.rs")
        );
    }

    /// The one normaliser, stated on its own: `\` is not a separator Rift
    /// keys anything by, whichever host the string reached it on. A shard
    /// archived on Windows and rebuilt on Linux carries `\` as an ordinary
    /// byte, so this must not be a `#[cfg(windows)]` fact.
    #[test]
    fn the_one_normaliser_respells_every_backslash_on_every_platform() {
        assert_eq!(
            slash_spelled(r"bins\rift\src\hooks.rs"),
            "bins/rift/src/hooks.rs"
        );
        assert_eq!(
            slash_spelled("bins/rift/src/hooks.rs"),
            "bins/rift/src/hooks.rs"
        );
        assert_eq!(slash_spelled(r"a\b/c\d"), "a/b/c/d");
        assert_eq!(slash_spelled(""), "");
        // Nothing to re-spell borrows rather than allocates — the common case
        // on a healthy tree, and the reason this sits on the lookup path too.
        assert!(matches!(
            slash_spelled("src/a.rs"),
            std::borrow::Cow::Borrowed(_)
        ));
        // …and it reaches the relativiser, so a relative payload is re-keyed
        // whether or not the host reads `\` as a separator.
        assert_eq!(
            repo_relative_slash(Path::new(""), Path::new(r"src\a.rs")).as_deref(),
            Some("src/a.rs")
        );
    }

    #[test]
    fn git_blob_oid_matches_git_hash_object() {
        // `printf '' | git hash-object --stdin`
        assert_eq!(
            git_blob_oid(b""),
            "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391"
        );
        // `printf 'hello\n' | git hash-object --stdin`
        assert_eq!(
            git_blob_oid(b"hello\n"),
            "ce013625030ba8dba906f756967f9e9ca394464a"
        );
        // the content hash and the blob oid are DISTINCT identities.
        assert_ne!(git_blob_oid(b"hello\n"), sha256_hex(b"hello\n"));
    }

    #[test]
    fn hash_blobs_returns_both_identities() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), b"hello\n").unwrap();
        let paths: BTreeSet<String> = ["a.txt"].into_iter().map(String::from).collect();
        let blobs = hash_blobs(dir.path(), &paths);
        assert_eq!(blobs.len(), 1);
        assert_eq!(blobs[0].content_sha256, sha256_hex(b"hello\n"));
        assert_eq!(blobs[0].blob_oid, git_blob_oid(b"hello\n"));
    }

    /// A readable file inside the checkout answers its bytes and their hash.
    #[test]
    fn a_file_in_the_checkout_answers_its_bytes() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/a.rs"), b"fn a() {}\n").unwrap();
        assert_eq!(
            current_file(dir.path(), "src/a.rs", 1024),
            CurrentFile::Present {
                content_sha256: sha256_hex(b"fn a() {}\n"),
                bytes: b"fn a() {}\n".to_vec(),
            }
        );
    }

    /// Each way the current bytes cannot be read is named, never guessed.
    #[test]
    fn every_unreadable_current_file_names_its_reason() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("big.rs"), vec![b'x'; 64]).unwrap();
        let refused = |path: &str, max: u64| match current_file(dir.path(), path, max) {
            CurrentFile::Refused(why) => Some(why),
            CurrentFile::Present { .. } => None,
        };
        assert_eq!(refused("../a.rs", 1024), Some(CurrentRefusal::UnsafePath));
        assert_eq!(refused("gone.rs", 1024), Some(CurrentRefusal::Missing));
        assert_eq!(refused("src", 1024), Some(CurrentRefusal::NotAFile));
        assert_eq!(refused("big.rs", 63), Some(CurrentRefusal::Oversize));
        assert_eq!(refused("big.rs", 64), None);
        assert_eq!(CurrentRefusal::OutsideRoot.tag(), "outside_root");
    }

    /// A link is not the file it names, and a linked directory cannot carry a
    /// read out of the checkout.
    #[cfg(unix)]
    #[test]
    fn a_link_or_a_path_through_a_linked_directory_is_refused() {
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.rs"), b"s").unwrap();
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("real.rs"), b"r").unwrap();
        std::os::unix::fs::symlink(dir.path().join("real.rs"), dir.path().join("link.rs")).unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("out")).unwrap();
        assert_eq!(
            current_file(dir.path(), "link.rs", 1024),
            CurrentFile::Refused(CurrentRefusal::Symlink)
        );
        assert_eq!(
            current_file(dir.path(), "out/secret.rs", 1024),
            CurrentFile::Refused(CurrentRefusal::OutsideRoot)
        );
    }
}
