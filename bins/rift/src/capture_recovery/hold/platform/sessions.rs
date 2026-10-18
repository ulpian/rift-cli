//! The admitted member's CURRENT run-state roots, held by descriptor: read-only
//! evidence derived from the hold's own bound target scope, never from a
//! resolver that could mint, record, adopt or move. The sessions tree and the
//! agent-queue journal are two roots under one marker and one discipline.
use std::ffi::OsStr;
use std::path::Path;

use rift_core::RepoId;
use rift_shadow::hooks::run_state::{RunRoot, MOVED_MARKER};

use super::{open_child_if_present, EvidenceError, EvidenceResult};
use crate::capture_recovery::files::{self, Namespace, PhysicalIdentity};
use crate::capture_recovery::hold::OwnHold;
use crate::capture_recovery::plan::Plan;

/// The marker holds a decimal instant; anything wider is not the marker.
const MAX_MARKER_BYTES: usize = 64;

/// One root bound under the hold: the hold it was derived from, the exact
/// bound member, the checked moved marker and the root opened under it.
pub(super) struct BoundRoot<'h> {
    hold: &'h OwnHold<'h>,
    member: &'h Namespace,
    marker: Marker,
    root: RunRoot,
    dir: Namespace,
}

/// The capability over the sessions tree.
pub(crate) struct ReadySessions<'h>(BoundRoot<'h>);

struct Marker {
    identity: PhysicalIdentity,
    bytes: Vec<u8>,
}

impl<'h> BoundRoot<'h> {
    pub(super) fn admit(hold: &'h OwnHold<'h>, root: RunRoot) -> EvidenceResult<Self> {
        hold.check_current()?;
        let member = hold.evidence.targets.bound_member()?;
        let marker = read_marker(member)?;
        let dir = open_root(member, root)?;
        let bound = Self {
            hold,
            member,
            marker,
            root,
            dir,
        };
        bound.recheck()?;
        Ok(bound)
    }

    /// Every evidence pass rechecks the hold, the member, the marker and the
    /// root directory before trusting a byte read under them.
    pub(super) fn recheck(&self) -> EvidenceResult<()> {
        self.hold.check_current()?;
        let member = self.hold.evidence.targets.bound_member()?;
        if member.path != self.member.path || member.ancestry != self.member.ancestry {
            return Err(EvidenceError);
        }
        self.member.recheck().map_err(|_| EvidenceError)?;
        let marker = read_marker(self.member)?;
        if marker.identity != self.marker.identity || marker.bytes != self.marker.bytes {
            return Err(EvidenceError);
        }
        self.dir.recheck().map_err(|_| EvidenceError)?;
        let now = open_root(self.member, self.root)?;
        if now.path != self.dir.path
            || now.ancestry != self.dir.ancestry
            || now.identity().map_err(|_| EvidenceError)?
                != self.dir.identity().map_err(|_| EvidenceError)?
        {
            return Err(EvidenceError);
        }
        Ok(())
    }

    pub(super) fn dir(&self) -> &Namespace {
        &self.dir
    }

    pub(super) fn plan(&self) -> &Plan {
        &self.hold.evidence.plan
    }

    /// The hold's private operation directory, rechecked with the hold.
    pub(super) fn private(&self) -> EvidenceResult<&Namespace> {
        self.hold.check_current()?;
        Ok(&self.hold.evidence.private)
    }
}

impl<'h> ReadySessions<'h> {
    pub(in crate::capture_recovery::hold) fn admit(hold: &'h OwnHold<'h>) -> EvidenceResult<Self> {
        BoundRoot::admit(hold, RunRoot::Sessions).map(Self)
    }

    pub(crate) fn recheck(&self) -> EvidenceResult<()> {
        self.0.recheck()
    }

    pub(in crate::capture_recovery) fn sessions(&self) -> &Namespace {
        self.0.dir()
    }

    pub(crate) fn repo_id(&self) -> &RepoId {
        self.0.plan().repo_id()
    }

    pub(crate) fn git_dir(&self) -> &Path {
        self.0.plan().git_dir()
    }

    pub(crate) fn work_tree(&self) -> &Path {
        self.0.plan().work_tree()
    }
}

/// The moved marker, a regular file opened without following, read whole.
fn read_marker(member: &Namespace) -> EvidenceResult<Marker> {
    let read = files::read_regular(member, OsStr::new(MOVED_MARKER), MAX_MARKER_BYTES)
        .map_err(|_| EvidenceError)?;
    Ok(Marker {
        identity: files::physical_identity(&read.metadata),
        bytes: read.bytes,
    })
}

/// The root's child of the bound member, present and a directory.
fn open_root(member: &Namespace, root: RunRoot) -> EvidenceResult<Namespace> {
    let path = root.under(&member.path);
    open_child_if_present(member, OsStr::new(root.dir_name()), &path)?.ok_or(EvidenceError)
}
