//! The admitted member's CURRENT sessions tree, held by descriptor: read-only
//! evidence derived from the hold's own bound target scope, never from a
//! resolver that could mint, record, adopt or move.
use std::ffi::OsStr;
use std::path::Path;

use rift_core::RepoId;
use rift_shadow::hooks::run_state::{RunRoot, MOVED_MARKER};

use super::{open_child_if_present, EvidenceError, EvidenceResult};
use crate::capture_recovery::files::{self, Namespace, PhysicalIdentity};
use crate::capture_recovery::hold::OwnHold;

/// The marker holds a decimal instant; anything wider is not the marker.
const MAX_MARKER_BYTES: usize = 64;

/// The capability: the hold it was derived from, the exact bound member, the
/// checked moved marker and the sessions child opened under it.
pub(crate) struct ReadySessions<'h> {
    hold: &'h OwnHold<'h>,
    member: &'h Namespace,
    marker: Marker,
    sessions: Namespace,
}

struct Marker {
    identity: PhysicalIdentity,
    bytes: Vec<u8>,
}

impl<'h> ReadySessions<'h> {
    pub(in crate::capture_recovery::hold) fn admit(hold: &'h OwnHold<'h>) -> EvidenceResult<Self> {
        hold.check_current()?;
        let member = hold.evidence.targets.bound_member()?;
        let marker = read_marker(member)?;
        let sessions = open_sessions(member)?;
        let ready = Self {
            hold,
            member,
            marker,
            sessions,
        };
        ready.recheck()?;
        Ok(ready)
    }

    /// Every evidence pass rechecks the hold, the member, the marker and the
    /// sessions directory before trusting a byte read under them.
    pub(crate) fn recheck(&self) -> EvidenceResult<()> {
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
        self.sessions.recheck().map_err(|_| EvidenceError)?;
        let now = open_sessions(self.member)?;
        if now.path != self.sessions.path
            || now.ancestry != self.sessions.ancestry
            || now.identity().map_err(|_| EvidenceError)?
                != self.sessions.identity().map_err(|_| EvidenceError)?
        {
            return Err(EvidenceError);
        }
        Ok(())
    }

    pub(in crate::capture_recovery) fn sessions(&self) -> &Namespace {
        &self.sessions
    }

    pub(crate) fn repo_id(&self) -> &RepoId {
        self.hold.evidence.plan.repo_id()
    }

    pub(crate) fn git_dir(&self) -> &Path {
        self.hold.evidence.plan.git_dir()
    }

    pub(crate) fn work_tree(&self) -> &Path {
        self.hold.evidence.plan.work_tree()
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

/// The sessions child of the bound member, present and a directory.
fn open_sessions(member: &Namespace) -> EvidenceResult<Namespace> {
    let path = RunRoot::Sessions.under(&member.path);
    open_child_if_present(member, OsStr::new(RunRoot::Sessions.dir_name()), &path)?
        .ok_or(EvidenceError)
}
