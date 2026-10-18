//! One private record published no-clobber: created 0600 and never followed,
//! written whole, synced with its directory and read back equal. An existing
//! record holding exactly these bytes is the resume input; anything else there
//! refuses and is never overwritten. The intent and the cut share this one
//! discipline so neither can drift from the other.
use std::io::{Read as _, Write as _};

use cap_fs_ext::{FollowSymlinks, OpenOptionsFollowExt as _};
use cap_std::fs::{OpenOptions, OpenOptionsExt as _};

use super::files::Namespace;

/// Whether this attempt wrote the record or found its exact bytes already there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Publication {
    Fresh,
    Resumed,
}

/// The two durability points a test observes between the write and the return.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Step {
    Written,
    Synced,
}

/// Why a record was not published, as a static class the caller names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RecordError {
    /// The private directory no longer proves itself.
    Unavailable,
    /// Something else is already there under this name, or the record read
    /// back is not a record at all.
    Conflict,
    /// The create, write, sync or read-back failed.
    Failed,
    /// The observer refused to continue.
    Resource,
}

/// Create the record no-clobber, 0600, write it whole, sync it and its
/// directory and read it back; an existing record is compared, never replaced.
pub(super) fn publish_private_record(
    private: &Namespace,
    name: &str,
    cap: usize,
    bytes: &[u8],
    observe: &mut impl FnMut(Step) -> Result<(), ()>,
) -> Result<Publication, RecordError> {
    private.recheck().map_err(|_| RecordError::Unavailable)?;
    let mut options = OpenOptions::new();
    options
        .write(true)
        .create_new(true)
        .follow(FollowSymlinks::No)
        .mode(0o600);
    let mut file = match private.dir.open_with(name, &options) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            return resume_private_record(private, name, cap, bytes);
        }
        Err(_) => return Err(RecordError::Failed),
    };
    file.write_all(bytes).map_err(|_| RecordError::Failed)?;
    observe(Step::Written).map_err(|()| RecordError::Resource)?;
    file.sync_all().map_err(|_| RecordError::Failed)?;
    private
        .dir
        .try_clone()
        .map_err(|_| RecordError::Failed)?
        .into_std_file()
        .sync_all()
        .map_err(|_| RecordError::Failed)?;
    observe(Step::Synced).map_err(|()| RecordError::Resource)?;
    if read_private_record(private, name, cap)? != bytes {
        return Err(RecordError::Failed);
    }
    Ok(Publication::Fresh)
}

fn resume_private_record(
    private: &Namespace,
    name: &str,
    cap: usize,
    bytes: &[u8],
) -> Result<Publication, RecordError> {
    if read_private_record(private, name, cap)? != bytes {
        return Err(RecordError::Conflict);
    }
    Ok(Publication::Resumed)
}

/// The record read whole under its cap plus one, descriptor-relative and
/// without following: a symlink, a directory or an oversize file is not it.
pub(super) fn read_private_record(
    private: &Namespace,
    name: &str,
    cap: usize,
) -> Result<Vec<u8>, RecordError> {
    private.recheck().map_err(|_| RecordError::Unavailable)?;
    let before = private
        .dir
        .symlink_metadata(name)
        .map_err(|_| RecordError::Conflict)?;
    if !before.is_file() || before.len() > cap as u64 {
        return Err(RecordError::Conflict);
    }
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No);
    let file = private
        .dir
        .open_with(name, &options)
        .map_err(|_| RecordError::Conflict)?;
    let mut bytes = Vec::new();
    (&file)
        .take(cap as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| RecordError::Conflict)?;
    if bytes.len() > cap {
        return Err(RecordError::Conflict);
    }
    Ok(bytes)
}
