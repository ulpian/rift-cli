//! The one removal under a durable retirement intent. The queued name is
//! first moved beside its pin in the private directory, checked there to be
//! the pinned original by inode and bytes, and only then unlinked; anything
//! else goes back under its name. A check followed by an unlink of the queue
//! name would remove whatever a writer put there in between, and a move
//! leaves a crash point a rerun can finish from the parked name alone.
use std::ffi::OsStr;

use super::{
    classify_checked, physical_identity, present, verify_pin, DeliveryMatch, FrozenSelection,
    LivePin, Scopes,
};
use crate::capture_queue::QueueLock;
use crate::capture_recovery::codec::MAX_BYTES;
use crate::capture_recovery::files::{self, RegularRead};
use crate::capture_recovery::{codec, PinError, PinResult};
use sha2::{Digest as _, Sha256};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Discharge {
    /// The pinned original is no longer queued; its pin still holds its bytes.
    Retired,
    /// Another file is under the original's name and stays there.
    Different,
}

/// Retire one pinned original, or finish a retirement a crash interrupted
/// at `parked`. Rerunning after any step acts only on what is still owed.
pub(crate) fn discharge_original(
    owner: &QueueLock,
    pin: &LivePin,
    parked: &str,
) -> PinResult<Discharge> {
    verify_pin(owner, pin)?;
    let scopes = &pin.scopes;
    if !present(&scopes.private, parked)? {
        let source = scopes.source.path.join(&pin.frozen.basename);
        match classify_checked(owner, &source, pin, &mut |_| {})? {
            DeliveryMatch::Missing => return Ok(Discharge::Retired),
            DeliveryMatch::Different => return Ok(Discharge::Different),
            DeliveryMatch::Unavailable => return Err(PinError),
            DeliveryMatch::OriginalPresent => {}
        }
        scopes
            .source
            .dir
            .rename(&pin.frozen.basename, &scopes.private.dir, parked)
            .map_err(|_| PinError)?;
    }
    settle(owner, pin, parked)
}

/// The parked file is unlinked only when it is the pinned inode with the
/// pinned bytes; otherwise it returns under the original's name.
fn settle(owner: &QueueLock, pin: &LivePin, parked: &str) -> PinResult<Discharge> {
    let scopes = &pin.scopes;
    let raw = files::read_regular(&scopes.private, OsStr::new(parked), MAX_BYTES)?;
    if is_original(&raw, &pin.frozen) {
        scopes
            .private
            .dir
            .remove_file(parked)
            .map_err(|_| PinError)?;
        verify_pin(owner, pin)?;
        return Ok(Discharge::Retired);
    }
    restore(scopes, &pin.frozen.basename, parked, &raw)?;
    Ok(Discharge::Different)
}

fn is_original(raw: &RegularRead, frozen: &FrozenSelection) -> bool {
    physical_identity(&raw.metadata) == frozen.identity
        && raw.bytes == frozen.raw_bytes
        && <[u8; 32]>::from(Sha256::digest(&raw.bytes)) == frozen.raw_sha256
}

/// A hard link never clobbers, so a name a writer took meanwhile is left
/// alone; a link already made by an interrupted restore is the same inode.
fn restore(scopes: &Scopes, name: &OsStr, parked: &str, raw: &RegularRead) -> PinResult<()> {
    codec::basename(name)?;
    if scopes
        .private
        .dir
        .hard_link(parked, &scopes.source.dir, name)
        .is_err()
    {
        let back = scopes
            .source
            .dir
            .symlink_metadata(name)
            .map_err(|_| PinError)?;
        if physical_identity(&back) != physical_identity(&raw.metadata) {
            return Err(PinError);
        }
    }
    scopes.private.dir.remove_file(parked).map_err(|_| PinError)
}
