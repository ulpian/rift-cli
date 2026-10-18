//! Exact retained Plan/Hold admission; no publication, acquisition or cleanup.
use std::path::Path;

use rift_core::boundary_gap::MemberKey;
use serde::{Deserialize, Serialize};

use super::plan::{self, PlanDigest};
use super::{codec, EvidenceError, EvidenceResult, WireError, WireResult};
use crate::capture_queue::QueueLock;

pub(super) const MAX_HOLD_BYTES: usize = 4096;
const HOLD_NAME: &str = ".rift-capture-recovery.hold";

pub(crate) struct Hold {
    wire: HoldWire,
    member: MemberKey,
    plan_digest: PlanDigest,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct HoldWire {
    schema_version: u32,
    operation_id: String,
    member_key: String,
    plan_sha256: String,
}

#[cfg(unix)]
pub(crate) use platform::queue::ReadyQueue;
#[cfg(unix)]
pub(crate) use platform::sessions::ReadySessions;
/// Non-Unix builds hold no descriptors, so the capability is never admitted.
#[cfg(not(unix))]
pub(crate) struct ReadySessions<'h>(std::marker::PhantomData<&'h ()>);
/// Non-Unix builds hold no descriptors, so the journal is never admitted.
#[cfg(not(unix))]
pub(crate) struct ReadyQueue<'h>(std::marker::PhantomData<&'h ()>);

/// Borrows the actual owner and exact live evidence; it grants only readiness.
pub(crate) struct OwnHold<'a> {
    owner: &'a QueueLock,
    #[cfg(unix)]
    evidence: platform::Evidence,
}

impl Hold {
    pub(crate) fn decode(bytes: &[u8]) -> WireResult<Self> {
        let wire: HoldWire = codec::wire_decode(bytes, MAX_HOLD_BYTES)?;
        if wire.schema_version != 1 {
            return Err(WireError);
        }
        plan::operation(&wire.operation_id)?;
        let member = MemberKey::parse_hex(&wire.member_key).map_err(|_| WireError)?;
        let plan_digest = PlanDigest::parse(&wire.plan_sha256)?;
        Ok(Self {
            wire,
            member,
            plan_digest,
        })
    }
    pub(crate) fn encode(&self) -> WireResult<Vec<u8>> {
        codec::wire_encode(&self.wire, MAX_HOLD_BYTES)
    }
}

impl OwnHold<'_> {
    pub(crate) fn owner(&self) -> &QueueLock {
        self.owner
    }

    /// The member's CURRENT sessions tree as read-only evidence: derived from
    /// the already-bound target scope after a recheck, held by descriptor, and
    /// admitted only behind a regular moved marker in that exact member. It
    /// never resolves, mints, records or moves; a member whose run state is
    /// still in its git dir has no such capability until shared readiness
    /// moves it.
    pub(crate) fn sessions_namespace(&self) -> EvidenceResult<ReadySessions<'_>> {
        self.check_current()?;
        #[cfg(unix)]
        {
            ReadySessions::admit(self)
        }
        #[cfg(not(unix))]
        {
            Err(EvidenceError)
        }
    }

    /// The member's CURRENT agent-queue journal as read-only evidence: the
    /// sibling of [`Self::sessions_namespace`], derived from the same bound
    /// scope behind the same moved marker and rechecked the same way. It lists
    /// and reads; it never publishes, retries, removes or moves.
    pub(crate) fn agent_queue_namespace(&self) -> EvidenceResult<ReadyQueue<'_>> {
        self.check_current()?;
        #[cfg(unix)]
        {
            ReadyQueue::admit(self)
        }
        #[cfg(not(unix))]
        {
            Err(EvidenceError)
        }
    }

    pub(crate) fn check_current(&self) -> EvidenceResult<()> {
        self.owner.check_current().map_err(|_| EvidenceError)?;
        #[cfg(unix)]
        {
            self.evidence.check(self.owner)
        }
        #[cfg(not(unix))]
        {
            Err(EvidenceError)
        }
    }
}

pub(crate) fn admit_own_hold<'a>(
    owner: &'a QueueLock,
    evidence_dir: &Path,
    expected_operation: &str,
) -> EvidenceResult<OwnHold<'a>> {
    owner.check_current().map_err(|_| EvidenceError)?;
    plan::operation(expected_operation).map_err(|_| EvidenceError)?;
    #[cfg(unix)]
    {
        let evidence = platform::Evidence::admit(owner, evidence_dir, expected_operation)?;
        let admitted = OwnHold { owner, evidence };
        admitted.check_current()?;
        Ok(admitted)
    }
    #[cfg(not(unix))]
    {
        let _ = evidence_dir;
        Err(EvidenceError)
    }
}

#[cfg(unix)]
mod platform {
    use super::{
        codec, plan, EvidenceError, EvidenceResult, Hold, Path, QueueLock, HOLD_NAME,
        MAX_HOLD_BYTES,
    };
    use crate::capture_recovery::files::{self, Namespace, PhysicalIdentity, RegularRead};
    use crate::capture_recovery::plan::{Plan, MAX_PLAN_BYTES};
    use cap_fs_ext::DirExt as _;
    use cap_std::fs::PermissionsExt as _;
    use std::cell::OnceCell;
    use std::ffi::{OsStr, OsString};
    use std::path::{Component, PathBuf};

    struct Record {
        read: RegularRead,
        identity: PhysicalIdentity,
    }

    pub(super) struct Evidence {
        plan: Plan,
        private: Namespace,
        git: Namespace,
        work: Namespace,
        targets: TargetScope,
        plan_file: Record,
        hold_file: Record,
    }

    struct TargetDir {
        name: OsString,
        path: PathBuf,
        bound: OnceCell<Namespace>,
    }

    struct TargetScope {
        cache: Namespace,
        expected_member: PathBuf,
        prefixes: [TargetDir; 4],
    }

    impl TargetScope {
        fn admit(cache: Namespace, expected_member: &Path) -> EvidenceResult<Self> {
            let components = expected_member
                .strip_prefix(&cache.path)
                .map_err(|_| EvidenceError)?
                .components()
                .map(|c| match c {
                    Component::Normal(name) => Ok(name.to_owned()),
                    _ => Err(EvidenceError),
                })
                .collect::<EvidenceResult<Vec<_>>>()?;
            if components.len() != 4
                || components.first().is_none_or(|name| name != "rift")
                || components.get(2).is_none_or(|name| name != "run")
            {
                return Err(EvidenceError);
            }
            let mut path = cache.path.clone();
            let mut prefixes = Vec::new();
            for name in components {
                codec::basename(&name).map_err(|_| EvidenceError)?;
                path.push(&name);
                prefixes.push(TargetDir {
                    name,
                    path: path.clone(),
                    bound: OnceCell::new(),
                });
            }
            let scope = Self {
                cache,
                expected_member: expected_member.to_owned(),
                prefixes: prefixes.try_into().map_err(|_| EvidenceError)?,
            };
            scope.check()?;
            Ok(scope)
        }

        fn check(&self) -> EvidenceResult<()> {
            self.cache.recheck().map_err(|_| EvidenceError)?;
            let mut parent = &self.cache;
            for (index, slot) in self.prefixes.iter().enumerate() {
                if let Some(bound) = slot.bound.get() {
                    bound.recheck().map_err(|_| EvidenceError)?;
                }
                let Some(now) = open_child_if_present(parent, &slot.name, &slot.path)? else {
                    if self
                        .prefixes
                        .iter()
                        .skip(index)
                        .any(|entry| entry.bound.get().is_some())
                    {
                        return Err(EvidenceError);
                    }
                    return Ok(());
                };
                match slot.bound.get() {
                    Some(bound) if bound.path != now.path || bound.ancestry != now.ancestry => {
                        return Err(EvidenceError)
                    }
                    Some(_) => (),
                    None => slot.bound.set(now).map_err(|_| EvidenceError)?,
                }
                parent = slot.bound.get().ok_or(EvidenceError)?;
                parent.recheck().map_err(|_| EvidenceError)?;
            }
            Ok(())
        }

        /// The bound member directory itself, once it exists; a scope whose
        /// member is still absent has no sessions to read.
        fn bound_member(&self) -> EvidenceResult<&Namespace> {
            let slot = self.prefixes.get(3).ok_or(EvidenceError)?;
            let bound = slot.bound.get().ok_or(EvidenceError)?;
            if bound.path != self.expected_member {
                return Err(EvidenceError);
            }
            Ok(bound)
        }

        fn exclude_private(&self, private: &Namespace) -> EvidenceResult<()> {
            let run = self.prefixes.get(2).ok_or(EvidenceError)?;
            if private.path.starts_with(&run.path) {
                return Err(EvidenceError);
            }
            if let Some(bound) = run.bound.get() {
                if private
                    .ancestry
                    .contains(&bound.identity().map_err(|_| EvidenceError)?)
                {
                    return Err(EvidenceError);
                }
            }
            Ok(())
        }
    }

    impl Record {
        fn open(namespace: &Namespace, name: &str, limit: usize) -> EvidenceResult<Self> {
            let read = files::read_regular(namespace, OsStr::new(name), limit)
                .map_err(|_| EvidenceError)?;
            let identity = files::physical_identity(&read.metadata);
            if identity.permission_bits != 0o600 {
                return Err(EvidenceError);
            }
            Ok(Self { read, identity })
        }
        fn check(&self, namespace: &Namespace, name: &str, limit: usize) -> EvidenceResult<()> {
            let now = Self::open(namespace, name, limit)?;
            if now.identity != self.identity
                || now.read.bytes != self.read.bytes
                || files::physical_identity(&self.read.file.metadata().map_err(|_| EvidenceError)?)
                    != self.identity
                || files::read_fd(&self.read.file, limit).map_err(|_| EvidenceError)?
                    != self.read.bytes
            {
                return Err(EvidenceError);
            }
            Ok(())
        }
    }

    impl Evidence {
        pub(super) fn admit(owner: &QueueLock, dir: &Path, expected: &str) -> EvidenceResult<Self> {
            let private = Namespace::open(dir).map_err(|_| EvidenceError)?;
            check_private(&private)?;
            let plan_file = Record::open(&private, "plan.json", MAX_PLAN_BYTES)?;
            let plan = Plan::decode(&plan_file.read.bytes).map_err(|_| EvidenceError)?;
            if plan.operation() != expected
                || plan.evidence_dir() != dir
                || plan.encode().map_err(|_| EvidenceError)? != plan_file.read.bytes
            {
                return Err(EvidenceError);
            }
            let (git, work, cache, member) = actual_scope(owner, &plan)?;
            let targets = TargetScope::admit(cache, &member)?;
            let hold_file = Record::open(&git, HOLD_NAME, MAX_HOLD_BYTES)?;
            let hold = Hold::decode(&hold_file.read.bytes).map_err(|_| EvidenceError)?;
            if hold.wire.operation_id != expected
                || hold.member != plan.member()
                || hold.plan_digest != plan.sha256().map_err(|_| EvidenceError)?
                || hold.encode().map_err(|_| EvidenceError)? != hold_file.read.bytes
            {
                return Err(EvidenceError);
            }
            let evidence = Self {
                plan,
                private,
                git,
                work,
                targets,
                plan_file,
                hold_file,
            };
            evidence.check(owner)?;
            Ok(evidence)
        }

        pub(super) fn check(&self, owner: &QueueLock) -> EvidenceResult<()> {
            for ns in [&self.private, &self.git, &self.work] {
                ns.recheck().map_err(|_| EvidenceError)?;
            }
            check_private(&self.private)?;
            let actual = actual_scope(owner, &self.plan)?;
            for (held, now) in [
                (&self.git, &actual.0),
                (&self.work, &actual.1),
                (&self.targets.cache, &actual.2),
            ] {
                if held.path != now.path || held.ancestry != now.ancestry {
                    return Err(EvidenceError);
                }
            }
            for excluded in [&self.git, &self.work] {
                if self
                    .private
                    .ancestry
                    .contains(&excluded.identity().map_err(|_| EvidenceError)?)
                {
                    return Err(EvidenceError);
                }
            }
            if self.targets.expected_member != actual.3 {
                return Err(EvidenceError);
            }
            self.targets.check()?;
            self.targets.exclude_private(&self.private)?;
            self.plan_file
                .check(&self.private, "plan.json", MAX_PLAN_BYTES)?;
            self.hold_file.check(&self.git, HOLD_NAME, MAX_HOLD_BYTES)
        }
    }

    fn open_child_if_present(
        parent: &Namespace,
        name: &OsStr,
        path: &Path,
    ) -> EvidenceResult<Option<Namespace>> {
        open_child_with(parent, name, path, &mut || {})
    }

    fn open_child_with(
        parent: &Namespace,
        name: &OsStr,
        path: &Path,
        before_open: &mut impl FnMut(),
    ) -> EvidenceResult<Option<Namespace>> {
        parent.recheck().map_err(|_| EvidenceError)?;
        let before = match parent.dir.symlink_metadata(name) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                parent.recheck().map_err(|_| EvidenceError)?;
                return match parent.dir.symlink_metadata(name) {
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        parent.recheck().map_err(|_| EvidenceError)?;
                        Ok(None)
                    }
                    _ => Err(EvidenceError),
                };
            }
            Err(_) => return Err(EvidenceError),
        };
        if !before.is_dir() {
            return Err(EvidenceError);
        }
        before_open();
        let opened = parent
            .dir
            .open_dir_nofollow(name)
            .map_err(|_| EvidenceError)?;
        let held = opened.dir_metadata().map_err(|_| EvidenceError)?;
        let current = parent
            .dir
            .symlink_metadata(name)
            .map_err(|_| EvidenceError)?;
        let observed = Namespace::open(path).map_err(|_| EvidenceError)?;
        let identity = files::directory_identity(&before);
        let mut ancestry = parent.ancestry.clone();
        ancestry.push(identity);
        if !held.is_dir()
            || !current.is_dir()
            || files::directory_identity(&held) != identity
            || files::directory_identity(&current) != identity
            || observed.identity().map_err(|_| EvidenceError)? != identity
            || observed.ancestry != ancestry
        {
            return Err(EvidenceError);
        }
        parent.recheck().map_err(|_| EvidenceError)?;
        observed.recheck().map_err(|_| EvidenceError)?;
        Ok(Some(observed))
    }

    fn check_private(namespace: &Namespace) -> EvidenceResult<()> {
        namespace.recheck().map_err(|_| EvidenceError)?;
        if namespace
            .dir
            .dir_metadata()
            .map_err(|_| EvidenceError)?
            .permissions()
            .mode()
            & 0o7777
            != 0o700
        {
            return Err(EvidenceError);
        }
        Ok(())
    }

    fn actual_scope(
        owner: &QueueLock,
        plan: &Plan,
    ) -> EvidenceResult<(Namespace, Namespace, Namespace, PathBuf)> {
        owner.check_current().map_err(|_| EvidenceError)?;
        let repo = rift_git::discover(owner.git_dir()).map_err(|_| EvidenceError)?;
        let git_dir = repo.git_dir.canonicalize().map_err(|_| EvidenceError)?;
        let work_tree = repo.work_tree.canonicalize().map_err(|_| EvidenceError)?;
        if !owner.matches_git_dir(&git_dir)
            || plan.git_dir() != git_dir
            || plan.work_tree() != work_tree
            || !work_tree.is_dir()
        {
            return Err(EvidenceError);
        }
        let rift_git::FamilyState::Minted(family) =
            rift_git::resolve_family(&repo, rift_git::MintPolicy::Never)
                .map_err(|_| EvidenceError)?
        else {
            return Err(EvidenceError);
        };
        if &family.family_id != plan.repo_id()
            || plan::member_key(&family.family_id, git_dir.to_str().ok_or(EvidenceError)?)
                .map_err(|_| EvidenceError)?
                != plan.member()
        {
            return Err(EvidenceError);
        }
        let cache = crate::family::cache_root()
            .canonicalize()
            .map_err(|_| EvidenceError)?;
        let family_dir = rift_store::projection_dir_under(&cache, &family.family_id);
        let member = rift_shadow::hooks::run_state::member_dir(&family_dir, &work_tree);
        Ok((
            Namespace::open(&git_dir).map_err(|_| EvidenceError)?,
            Namespace::open(&work_tree).map_err(|_| EvidenceError)?,
            Namespace::open(&cache).map_err(|_| EvidenceError)?,
            member,
        ))
    }
    pub(super) mod queue;
    pub(super) mod sessions;
    #[cfg(test)]
    mod tests;
}

#[cfg(test)]
mod tests;
