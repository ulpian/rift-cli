//! Descriptor-relative, bounded observations. Every untrusted component is
//! opened without following links; no git writer subsequently reopens a path.

use std::ffi::{OsStr, OsString};
use std::io::{self, Read as _};
use std::path::{Component, Path};

use cap_fs_ext::{
    DirExt as _, FollowSymlinks, MetadataExt as _, OpenOptionsFollowExt as _,
    OpenOptionsSyncExt as _,
};
use cap_std::fs::{Dir, Metadata, OpenOptions};
use rift_shadow_attribution::MAX_DIFF_BYTES;
use rift_store::BoundaryFileKind;

const MAX_COMPONENTS: usize = 128;

pub(super) struct WorkingFile {
    pub bytes: Vec<u8>,
    pub kind: BoundaryFileKind,
}

/// Named phases permit deterministic replacement fixtures without clocks,
/// threads, sleeps or a production-global test hook.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Phase {
    BeforeRoot,
    BeforeOpen,
    BeforeRead,
    AfterRead,
    BeforeMissing,
}

pub(super) fn observe(work_tree: &Path, rel: &str) -> WorkingFile {
    observe_with(work_tree, rel, |_| {})
}

fn observe_with(work_tree: &Path, rel: &str, mut at: impl FnMut(Phase)) -> WorkingFile {
    read_checked(work_tree, rel, &mut at).unwrap_or(WorkingFile {
        bytes: Vec::new(),
        kind: BoundaryFileKind::Unknown,
    })
}

fn read_checked(
    work_tree: &Path,
    rel: &str,
    at: &mut impl FnMut(Phase),
) -> io::Result<WorkingFile> {
    let components = components(rel)?;
    at(Phase::BeforeRoot);
    let root = open_root(work_tree)?;
    let mut parents = vec![(OsString::new(), root)];
    let Some((leaf, ancestry)) = components.split_last() else {
        return Err(refused());
    };
    for component in ancestry {
        let Some((_, parent)) = parents.last() else {
            return Err(refused());
        };
        match parent.open_dir_nofollow(component) {
            Ok(dir) => parents.push((component.clone(), dir)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                at(Phase::BeforeMissing);
                return missing(work_tree, &parents, component);
            }
            Err(error) => return Err(error),
        }
    }
    let Some((_, parent)) = parents.last() else {
        return Err(refused());
    };
    let before = match parent.symlink_metadata(leaf) {
        Ok(meta) => meta,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            at(Phase::BeforeMissing);
            return missing(work_tree, &parents, leaf);
        }
        Err(error) => return Err(error),
    };
    at(Phase::BeforeOpen);
    verify_parents(work_tree, &parents)?;
    let bytes = if before.file_type().is_symlink() {
        link_bytes(parent, leaf, &before)?
    } else {
        regular_bytes(parent, leaf, &before, at)?
    };
    at(Phase::AfterRead);
    verify_parents(work_tree, &parents)?;
    let after = parent.symlink_metadata(leaf)?;
    if !same_observation(&before, &after) {
        return Err(refused());
    }
    Ok(WorkingFile {
        bytes,
        kind: if before.file_type().is_symlink() {
            BoundaryFileKind::Link
        } else {
            BoundaryFileKind::Regular
        },
    })
}

fn components(rel: &str) -> io::Result<Vec<OsString>> {
    let mut out = Vec::new();
    for component in Path::new(rel).components() {
        let Component::Normal(name) = component else {
            return Err(refused());
        };
        if out.len() >= MAX_COMPONENTS || name.eq_ignore_ascii_case(".git") {
            return Err(refused());
        }
        out.push(name.to_owned());
    }
    Ok(out)
}

fn regular_bytes(
    parent: &Dir,
    leaf: &OsStr,
    before: &Metadata,
    at: &mut impl FnMut(Phase),
) -> io::Result<Vec<u8>> {
    if !before.is_file() || before.len() > MAX_DIFF_BYTES as u64 {
        return Err(refused());
    }
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No).nonblock(true);
    let file = parent.open_with(leaf, &options)?;
    let opened = file.metadata()?;
    if !same_observation(before, &opened) {
        return Err(refused());
    }
    at(Phase::BeforeRead);
    if !same_observation(before, &parent.symlink_metadata(leaf)?) {
        return Err(refused());
    }
    let mut bytes = Vec::new();
    (&file)
        .take(MAX_DIFF_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_DIFF_BYTES || !same_observation(before, &file.metadata()?) {
        return Err(refused());
    }
    Ok(bytes)
}

fn link_bytes(parent: &Dir, leaf: &OsStr, before: &Metadata) -> io::Result<Vec<u8>> {
    if before.len() > MAX_DIFF_BYTES as u64 {
        return Err(refused());
    }
    let target = parent.read_link_contents(leaf)?;
    let bytes = target_bytes(target.as_os_str())?;
    if bytes.len() > MAX_DIFF_BYTES {
        return Err(refused());
    }
    Ok(bytes)
}

#[cfg(unix)]
fn target_bytes(target: &OsStr) -> io::Result<Vec<u8>> {
    use std::os::unix::ffi::OsStrExt as _;
    Ok(target.as_bytes().to_vec())
}

#[cfg(not(unix))]
fn target_bytes(target: &OsStr) -> io::Result<Vec<u8>> {
    // Git's Windows link payloads are UTF-8. An unrepresentable target is
    // unknown rather than a lossy spelling that names a different resource.
    target
        .to_str()
        .map(|s| s.as_bytes().to_vec())
        .ok_or_else(refused)
}

fn verify_parents(work_tree: &Path, parents: &[(OsString, Dir)]) -> io::Result<()> {
    let Some((_, root)) = parents.first() else {
        return Err(refused());
    };
    let now = open_root(work_tree)?;
    if !same_identity(&root.dir_metadata()?, &now.dir_metadata()?) {
        return Err(refused());
    }
    for pair in parents.windows(2) {
        let [(.., parent), (name, child)] = pair else {
            return Err(refused());
        };
        let current = parent.open_dir_nofollow(name)?;
        if !same_identity(&child.dir_metadata()?, &current.dir_metadata()?) {
            return Err(refused());
        }
    }
    Ok(())
}

fn same_identity(a: &Metadata, b: &Metadata) -> bool {
    a.dev() == b.dev() && a.ino() == b.ino()
}

fn same_observation(a: &Metadata, b: &Metadata) -> bool {
    same_identity(a, b)
        && a.file_type() == b.file_type()
        && a.len() == b.len()
        && a.modified().ok() == b.modified().ok()
}

fn missing(work_tree: &Path, parents: &[(OsString, Dir)], name: &OsStr) -> io::Result<WorkingFile> {
    verify_parents(work_tree, parents)?;
    let Some((_, parent)) = parents.last() else {
        return Err(refused());
    };
    match parent.symlink_metadata(name) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => (),
        _ => return Err(refused()),
    }
    Ok(WorkingFile {
        bytes: Vec::new(),
        kind: BoundaryFileKind::Absent,
    })
}

/// The configured worktree's parent is ambient authority, like repository
/// discovery. The root itself must be a directory, never a replacement link.
/// OS aliases above it (such as macOS /var -> /private/var) remain usable.
fn open_root(work_tree: &Path) -> io::Result<Dir> {
    let parent = work_tree.parent().ok_or_else(refused)?;
    let name = work_tree.file_name().ok_or_else(refused)?;
    let parent = Dir::open_ambient_dir(parent, cap_std::ambient_authority())?;
    parent.open_dir_nofollow(name)
}

fn refused() -> io::Error {
    io::Error::other("boundary path is not a stable bounded file or link")
}

#[cfg(test)]
#[path = "observation_tests.rs"]
mod tests;
