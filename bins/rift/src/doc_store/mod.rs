//! The family's Markdown sidecars: one converted document, shared by every
//! member of the repository family.
//!
//! An agent that opens `spec.docx` is handed Markdown instead of zip bytes.
//! Producing that Markdown costs a process and a parse, so it is done **once
//! per document version per family** and read thereafter. "Once per family" is
//! the whole point: a linked worktree is a member of its repository's family,
//! subagents run in temporary worktrees, and a cache keyed by a checkout path
//! would convert the same file again in every one of them and lose the lot
//! when the worktree is removed.
//!
//! So the store is two directories:
//!
//! * `<common_dir>/rift-family/doc/<key>.md` — the family's own copy, beside
//!   the family manifest, in the one directory every member already shares.
//!   Written temp-file-then-rename, so a reader never sees half a conversion.
//! * `<work_tree>/.rift/state/doc/<key>.md` — this member's **view** of it, a
//!   hardlink (a copy where the filesystem refuses one). A member's view is
//!   what an agent's tool call can name — a path inside the checkout it is
//!   working in — and it costs one inode. Delete the worktree and the view
//!   goes with it; the family's copy is untouched, and the next member to want
//!   that document links it instead of converting it.
//!
//! **The key is the identity of the answer, not of the file.** It is
//! `sha256(source bytes) ‖ engine version ‖ policy version`, hashed once into a
//! path-safe name: change a byte of the document, upgrade the converter, or
//! change a cap that changes the Markdown, and the key changes with it. That is
//! what makes a stale sidecar unreachable rather than wrong — the same rule the
//! recall plane's summaries live by. Nothing is overwritten in place; a new
//! answer is a new name.
//!
//! **Why a sidecar is durable text that needs no redaction gate.** Every byte
//! here is derived from a file in the repository's own work tree — bytes the
//! developer wrote or committed, which git already stores in the clear — and
//! from nothing else. No transcript, no tool output and no agent-authored
//! payload reaches this module: a converter is handed file bytes and the
//! file's extension, and its answer is written under the hash of those bytes.
//! Rift's redaction gate exists for what a session *produces* on its way into
//! the durable capture record; a sidecar is a local re-encoding of a file that
//! is already in the checkout, kept beside that checkout, owner-readable, and
//! collected like every other transient. The one class that must never be
//! re-encoded is the one the repository declared sensitive, and the caller
//! that walks documents into this store skips those paths before it reads a
//! byte.
//!
//! Nothing here opens a store or initialises a tokenizer, and the conversion
//! half is never reached from a decision: a hook may ask whether an answer
//! exists and hand over the one that does, and everything that costs more than
//! a `stat` belongs to the detached worker.
//!
//! The store is read and written by halves that have little to say to each
//! other, so each is its own module: `store` holds the two directories and
//! what a sweep does to them, `key` derives the identity of an answer,
//! `window` remembers what one conversation was told, `sweep` reads the
//! directories a collection walks, and `convert` fills the store — the half
//! no decision reaches.

mod convert;
mod key;
mod store;
mod sweep;
mod window;

#[cfg(feature = "doc")]
pub(crate) use convert::{
    convert_into_store, convert_tracked_documents, document_path, is_convertible_path, Conversion,
    SWEEP_LIMIT,
};
#[cfg(feature = "doc")]
pub(crate) use key::member_sidecar_key;
pub(crate) use key::{is_document_extension, sidecar_key, DOCUMENT_EXTENSIONS};
pub(crate) use store::{DocStore, SweepReport};
#[cfg(feature = "doc")]
pub(crate) use window::DocWindow;

#[cfg(test)]
mod tests {
    #[cfg(feature = "doc")]
    use std::path::{Path, PathBuf};

    #[cfg(feature = "doc")]
    use super::*;

    pub(super) const SHA_A: &str =
        "aa11bb22cc33dd44ee55ff66007788990011223344556677889900aabbccddee";
    pub(super) const SHA_B: &str =
        "bb11bb22cc33dd44ee55ff66007788990011223344556677889900aabbccddee";

    /// A family with two members, built out of the same files `git init` and
    /// `git worktree add` write, so the store under test is keyed by a real
    /// common directory and the member views are real checkouts.
    ///
    /// It lives here rather than beside any one of them because every module
    /// of this store tests against the same family. Every test that needs one
    /// is a test about a conversion, so the fixture belongs to the same build
    /// the conversions do.
    #[cfg(feature = "doc")]
    pub(super) struct Fixture {
        _tmp: tempfile::TempDir,
        pub(super) common_dir: PathBuf,
        pub(super) main: PathBuf,
        pub(super) linked: PathBuf,
    }

    #[cfg(feature = "doc")]
    pub(super) fn family() -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let main = tmp.path().join("main");
        std::fs::create_dir_all(&main).unwrap();
        let git_dir = rift_git::test_support::fake_repo(&main).unwrap();
        let linked = tmp.path().join("linked");
        rift_git::test_support::fake_worktree(&git_dir, &linked, "linked").unwrap();
        Fixture {
            _tmp: tmp,
            common_dir: git_dir,
            main,
            linked,
        }
    }

    #[cfg(feature = "doc")]
    pub(super) fn store_for(f: &Fixture, member: &Path) -> DocStore {
        DocStore::for_member(&f.common_dir, member)
    }
}
