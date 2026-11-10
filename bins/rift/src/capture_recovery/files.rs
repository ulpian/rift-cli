//! Shared descriptor-relative bounded reads; caller owns record policy.
use super::codec::{self, MAX_BYTES};
use super::{PinError, PinResult};
use cap_fs_ext::{
    DirExt as _, FollowSymlinks, MetadataExt as _, OpenOptionsFollowExt as _,
    OpenOptionsSyncExt as _,
};
use cap_std::fs::{Dir, File, Metadata, OpenOptions, PermissionsExt as _};
use std::ffi::{OsStr, OsString};
use std::io::{Read as _, Seek as _, SeekFrom};
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Component, Path, PathBuf};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct DirectoryIdentity {
    pub(super) dev: u64,
    pub(super) ino: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct PhysicalIdentity {
    pub(super) dev: u64,
    pub(super) ino: u64,
    pub(super) permission_bits: u32,
}

pub(super) struct Namespace {
    pub(super) path: PathBuf,
    pub(super) dir: Dir,
    pub(super) ancestry: Vec<DirectoryIdentity>,
    retained_ancestors: Vec<Dir>,
}

pub(super) struct RegularRead {
    pub(super) file: File,
    pub(super) metadata: Metadata,
    pub(super) bytes: Vec<u8>,
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ReadPhase {
    BeforeOpen,
    AfterOpen,
    BeforeBoundedRead,
}
impl Namespace {
    pub(super) fn open(path: &Path) -> PinResult<Self> {
        clean_absolute(path)?;
        let mut dir =
            Dir::open_ambient_dir("/", cap_std::ambient_authority()).map_err(|_| PinError)?;
        let mut ancestry = vec![directory_identity(
            &dir.dir_metadata().map_err(|_| PinError)?,
        )];
        let mut retained_ancestors = vec![dir.try_clone().map_err(|_| PinError)?];
        for component in path.components() {
            if let Component::Normal(name) = component {
                if ancestry.len() > 128 {
                    return Err(PinError);
                }
                dir = dir.open_dir_nofollow(name).map_err(|_| PinError)?;
                ancestry.push(directory_identity(
                    &dir.dir_metadata().map_err(|_| PinError)?,
                ));
                retained_ancestors.push(dir.try_clone().map_err(|_| PinError)?);
            }
        }
        Ok(Self {
            path: path.to_owned(),
            dir,
            ancestry,
            retained_ancestors,
        })
    }

    pub(super) fn identity(&self) -> PinResult<DirectoryIdentity> {
        Ok(directory_identity(
            &self.dir.dir_metadata().map_err(|_| PinError)?,
        ))
    }

    pub(super) fn recheck(&self) -> PinResult<()> {
        let held: Vec<_> = self
            .retained_ancestors
            .iter()
            .map(|dir| {
                dir.dir_metadata()
                    .map(|metadata| directory_identity(&metadata))
                    .map_err(|_| PinError)
            })
            .collect::<PinResult<_>>()?;
        let now = Self::open(&self.path)?;
        if held != self.ancestry
            || self.ancestry != now.ancestry
            || self.identity()? != now.identity()?
        {
            return Err(PinError);
        }
        Ok(())
    }

    /// Flush the directory's entries to disk. The held `dir` cannot do it:
    /// on Linux cap-std opens directories `O_PATH`, and `fsync` on an
    /// `O_PATH` descriptor fails with `EBADF`. A readable descriptor of the
    /// same directory is opened through the held one and must carry its
    /// identity, so the flush never lands on a substituted directory.
    pub(super) fn sync(&self) -> PinResult<()> {
        let mut options = OpenOptions::new();
        options.read(true).follow(FollowSymlinks::No);
        let readable = self.dir.open_with(".", &options).map_err(|_| PinError)?;
        let metadata = readable.metadata().map_err(|_| PinError)?;
        if !metadata.is_dir() || directory_identity(&metadata) != self.identity()? {
            return Err(PinError);
        }
        readable.sync_all().map_err(|_| PinError)
    }
}

pub(super) fn clean_absolute(path: &Path) -> PinResult<()> {
    let bytes = path.as_os_str().as_bytes();
    if bytes == b"/" {
        return Ok(());
    }
    if bytes.first() != Some(&b'/')
        || bytes
            .get(1..)
            .ok_or(PinError)?
            .split(|b| *b == b'/')
            .any(|part| part.is_empty() || part == b"." || part == b"..")
    {
        return Err(PinError);
    }
    Ok(())
}

pub(super) fn directory_identity(metadata: &Metadata) -> DirectoryIdentity {
    DirectoryIdentity {
        dev: metadata.dev(),
        ino: metadata.ino(),
    }
}

pub(super) fn physical_identity(metadata: &Metadata) -> PhysicalIdentity {
    PhysicalIdentity {
        dev: metadata.dev(),
        ino: metadata.ino(),
        permission_bits: metadata.permissions().mode() & 0o7777,
    }
}

pub(super) fn same_sample(a: &Metadata, b: &Metadata) -> PinResult<bool> {
    Ok(a.is_file()
        && b.is_file()
        && physical_identity(a) == physical_identity(b)
        && a.len() == b.len()
        && a.modified().map_err(|_| PinError)? == b.modified().map_err(|_| PinError)?)
}

pub(super) fn read_fd(file: &File, limit: usize) -> PinResult<Vec<u8>> {
    read_fd_with(file, limit, &mut |_| {})
}

pub(super) fn read_fd_with(
    file: &File,
    limit: usize,
    phase: &mut impl FnMut(ReadPhase),
) -> PinResult<Vec<u8>> {
    if limit > MAX_BYTES {
        return Err(PinError);
    }
    let before = file.metadata().map_err(|_| PinError)?;
    if !before.is_file() || before.len() > limit as u64 {
        return Err(PinError);
    }
    phase(ReadPhase::BeforeBoundedRead);
    let mut view = file;
    view.seek(SeekFrom::Start(0)).map_err(|_| PinError)?;
    let mut bytes = Vec::new();
    view.take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| PinError)?;
    if bytes.len() > limit || !same_sample(&before, &file.metadata().map_err(|_| PinError)?)? {
        return Err(PinError);
    }
    Ok(bytes)
}

pub(super) fn read_regular(
    namespace: &Namespace,
    name: &OsStr,
    limit: usize,
) -> PinResult<RegularRead> {
    read_regular_with(namespace, name, limit, &mut |_| {})
}

pub(super) fn read_regular_with(
    namespace: &Namespace,
    name: &OsStr,
    limit: usize,
    phase: &mut impl FnMut(ReadPhase),
) -> PinResult<RegularRead> {
    if limit > MAX_BYTES {
        return Err(PinError);
    }
    codec::basename(name)?;
    namespace.recheck()?;
    let before = namespace.dir.symlink_metadata(name).map_err(|_| PinError)?;
    if !before.is_file() || before.len() > limit as u64 {
        return Err(PinError);
    }
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No).nonblock(true);
    phase(ReadPhase::BeforeOpen);
    let file = namespace
        .dir
        .open_with(name, &options)
        .map_err(|_| PinError)?;
    phase(ReadPhase::AfterOpen);
    if !same_sample(&before, &file.metadata().map_err(|_| PinError)?)? {
        return Err(PinError);
    }
    let bytes = read_fd(&file, limit)?;
    let after = namespace.dir.symlink_metadata(name).map_err(|_| PinError)?;
    if !same_sample(&before, &after)?
        || !same_sample(&after, &file.metadata().map_err(|_| PinError)?)?
    {
        return Err(PinError);
    }
    namespace.recheck()?;
    Ok(RegularRead {
        file,
        metadata: after,
        bytes,
    })
}

/// The type of one direct entry, taken without following.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum EntryKind {
    Regular,
    Directory,
    Symlink,
    Other,
}

/// The direct entries of the namespace's own directory, descriptor-relative,
/// each with its type taken without following; the iterator's own failure is
/// yielded as an item, never swallowed. No recursion, no path.
pub(super) fn read_dir(
    namespace: &Namespace,
) -> PinResult<impl Iterator<Item = PinResult<(OsString, EntryKind)>>> {
    namespace.recheck()?;
    let entries = namespace.dir.entries().map_err(|_| PinError)?;
    Ok(entries.map(|entry| {
        let entry = entry.map_err(|_| PinError)?;
        let kind = entry.file_type().map_err(|_| PinError)?;
        let kind = if kind.is_symlink() {
            EntryKind::Symlink
        } else if kind.is_dir() {
            EntryKind::Directory
        } else if kind.is_file() {
            EntryKind::Regular
        } else {
            EntryKind::Other
        };
        Ok((entry.file_name(), kind))
    }))
}

#[cfg(test)]
mod tests;
