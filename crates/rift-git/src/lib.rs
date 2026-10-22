//! Native git plumbing for the native capture path.
//!
//! Three pieces the `NativeBackend` composes:
//!
//! * [`RepoPaths`] + [`discover`] — explicit **worktree / common-dir** handling so
//!   multi-worktree repos capture correctly (objects in the common dir, per-worktree
//!   shadow refs, destructive ops isolated to the intended worktree).
//! * [`NativeGitWrite`] — additive object/ref writes where the **durable** writer
//!   accepts only [`RedactedBytes`](rift_core::RedactedBytes) (the redaction type wall survives into
//!   plumbing; a durable write of raw bytes does not compile) and the local-only writer takes
//!   [`SecretBytes`](rift_core::SecretBytes).
//! * [`git_reset_hard`] / [`git_checkout`] — the two **destructive** working-tree
//!   ops shell out to the user's `git` (mirroring the legacy backend's documented go-git
//!   workaround), via [`run_git`]: an **argv array, no shell**, explicit
//!   `GIT_DIR`/`GIT_WORK_TREE`, with refspecs/oids validated so no argument is
//!   interpretable as a flag — defeating argument injection.
//!
//! This crate is the single home for the LAYOUT contract (ref naming + shard
//! paths). The recall-side substrate reader converges onto it as a follow-up; for
//! now the read side stays in `rift-recall-substrate` (gix) and this crate owns the
//! write side + worktree discovery + the destructive-op shell-out.
#![forbid(unsafe_code)]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]

mod command;
mod status;
pub use status::tracked_changes;
mod discovery;
mod head;
mod plumbing;
mod write;

pub mod blob;
pub mod chain;
pub mod child_path;
pub mod family;
pub mod identity;
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;
#[cfg(feature = "gix")]
pub use blob::GixBlobReader;
pub use blob::{BlobReader, MemoryBlobReader, TreeEntry, TreeEntryKind};
pub use child_path::{child_process_path, display_path};
pub use command::{
    changed_worktree_paths, changed_worktree_paths_outside_rift_state, checkout_argv, git_checkout,
    git_reset_hard, reset_hard_argv, run_git, run_git_capture, run_git_capture_env, run_git_status,
    validate_oid, validate_refspec,
};
pub use discovery::{
    discover, gitdir_text_path, is_gitdir, list_worktrees, read_common_dir, repo_id_for,
    LinkedWorktree, RepoPaths,
};
pub use family::{
    family_members, family_of, remint, resolve_family, Family, FamilyMember, FamilyState,
    MintPolicy, MintedFrom, RemintMode, UnmintedReason, ENV_FAMILY_DIR,
};
pub use head::{head_state, HeadState};
pub use identity::{GitConfigIdentity, ENV_DEVELOPER_OVERRIDE};
pub use plumbing::{
    commit_tree, commit_tree_oid, hash_files_write, hash_object_write, mktree, path_in_commit,
    path_object_oid, resolve_commit, sweep_stale_graft_indexes, update_ref_cas, update_ref_create,
    PlumbingGitWrite,
};
pub use write::{
    GitFileMode, GitIdent, MemoryGitWrite, NativeGitWrite, OrphanCommit, TreeWriteEntry,
};

/// Lowercase hex of a byte slice.
fn to_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}
