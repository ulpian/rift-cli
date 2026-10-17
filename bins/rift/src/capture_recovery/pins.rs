//! Live physical preservation and read-only matching; no unlink authority.

#[cfg(unix)]
mod platform {
    use std::ffi::{OsStr, OsString};
    #[cfg(test)]
    use std::io::Seek as _;
    use std::path::{Path, PathBuf};

    use cap_std::fs::{File, Metadata, PermissionsExt as _};
    use sha2::{Digest as _, Sha256};

    use crate::capture_queue::QueueLock;
    use crate::capture_recovery::codec::{self, Decoded, MAX_BYTES};
    use crate::capture_recovery::files::{
        self, clean_absolute, physical_identity, DirectoryIdentity, Namespace, PhysicalIdentity,
    };
    use crate::capture_recovery::{PinError, PinResult};

    #[derive(Clone)]
    pub(crate) struct FrozenSelection {
        owner_git_dir: PathBuf,
        source_parent: PathBuf,
        source_parent_identity: DirectoryIdentity,
        source_ancestry: Vec<DirectoryIdentity>,
        private_parent: PathBuf,
        private_parent_identity: DirectoryIdentity,
        private_ancestry: Vec<DirectoryIdentity>,
        basename: OsString,
        pin_basename: OsString,
        identity: PhysicalIdentity,
        byte_len: u64,
        raw_bytes: Vec<u8>,
        raw_sha256: [u8; 32],
        decoded: Decoded,
    }

    pub(crate) struct LivePin {
        frozen: FrozenSelection,
        source: Namespace,
        private: Namespace,
        original_fd: Option<File>,
        pin_fd: File,
        excluded: Vec<Namespace>,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(crate) enum DeliveryMatch {
        OriginalPresent,
        Missing,
        Different,
        Unavailable,
    }

    struct SourceScope {
        namespace: Namespace,
        excluded: Vec<Namespace>,
    }

    struct Sample {
        file: File,
        metadata: Metadata,
        bytes: Vec<u8>,
        decoded: Decoded,
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Phase {
        BeforeOpen,
        AfterOpen,
        BeforeBoundedRead,
        BeforeLink,
        AfterLink,
        BeforeClassifySource,
    }

    impl LivePin {
        pub(crate) fn selection(&self) -> &FrozenSelection {
            &self.frozen
        }

        /// Physical identity is evidence for later checks, never deletion permission.
        pub(crate) fn original_identity(&self) -> (u64, u64, u32) {
            let identity = self.frozen.identity;
            (identity.dev, identity.ino, identity.permission_bits)
        }
    }

    fn map_read_phase(phase: files::ReadPhase) -> Phase {
        match phase {
            files::ReadPhase::BeforeOpen => Phase::BeforeOpen,
            files::ReadPhase::AfterOpen => Phase::AfterOpen,
            files::ReadPhase::BeforeBoundedRead => Phase::BeforeBoundedRead,
        }
    }

    fn read_fd(file: &File) -> PinResult<Vec<u8>> {
        files::read_fd(file, MAX_BYTES)
    }

    #[cfg(test)]
    fn read_fd_with(file: &File, phase: &mut impl FnMut(Phase)) -> PinResult<Vec<u8>> {
        files::read_fd_with(file, MAX_BYTES, &mut |point| phase(map_read_phase(point)))
    }

    fn read_regular(namespace: &Namespace, name: &OsStr, decode_name: &OsStr) -> PinResult<Sample> {
        read_regular_with(namespace, name, decode_name, &mut |_| {})
    }

    fn read_regular_with(
        namespace: &Namespace,
        name: &OsStr,
        decode_name: &OsStr,
        phase: &mut impl FnMut(Phase),
    ) -> PinResult<Sample> {
        let raw = files::read_regular_with(namespace, name, MAX_BYTES, &mut |point| {
            phase(map_read_phase(point));
        })?;
        let decoded = codec::decode(decode_name, &raw.bytes)?;
        Ok(Sample {
            file: raw.file,
            metadata: raw.metadata,
            bytes: raw.bytes,
            decoded,
        })
    }

    fn source_scope(owner: &QueueLock, source: &Path) -> PinResult<SourceScope> {
        owner.check_current().map_err(|_| PinError)?;
        clean_absolute(source)?;
        codec::basename(source.file_name().ok_or(PinError)?)?;
        let repo = rift_git::discover(owner.git_dir()).map_err(|_| PinError)?;
        if !owner.matches_git_dir(&repo.git_dir) {
            return Err(PinError);
        }
        let rift_git::FamilyState::Minted(family) =
            rift_git::resolve_family(&repo, rift_git::MintPolicy::Never).map_err(|_| PinError)?
        else {
            return Err(PinError);
        };
        let cache = crate::family::cache_root()
            .canonicalize()
            .map_err(|_| PinError)?;
        let family_dir = rift_store::projection_dir_under(&cache, &family.family_id);
        let work_tree = repo.work_tree.canonicalize().map_err(|_| PinError)?;
        let member = rift_shadow::hooks::run_state::member_dir(&family_dir, &work_tree);
        let expected = rift_shadow::hooks::run_state::RunRoot::AgentQueue.under(&member);
        let namespace = Namespace::open(source.parent().ok_or(PinError)?)?;
        let actual = Namespace::open(&expected)?;
        if namespace.identity()? != actual.identity()? {
            return Err(PinError);
        }
        let mut excluded = Vec::new();
        for path in [
            &work_tree,
            member.parent().ok_or(PinError)?,
            owner.git_dir(),
        ] {
            excluded.push(Namespace::open(path)?);
        }
        Ok(SourceScope {
            namespace,
            excluded,
        })
    }

    fn private_namespace(path: &Path, excluded: &[Namespace]) -> PinResult<Namespace> {
        let namespace = Namespace::open(path)?;
        check_exclusions(&namespace, excluded)?;
        if namespace
            .dir
            .dir_metadata()
            .map_err(|_| PinError)?
            .permissions()
            .mode()
            & 0o7777
            != 0o700
        {
            return Err(PinError);
        }
        Ok(namespace)
    }

    fn check_exclusions(namespace: &Namespace, excluded: &[Namespace]) -> PinResult<()> {
        for root in excluded {
            root.recheck()?;
            if namespace.ancestry.contains(&root.identity()?) {
                return Err(PinError);
            }
        }
        Ok(())
    }

    fn freeze(
        owner: &QueueLock,
        source: &Namespace,
        private: &Namespace,
        name: &OsStr,
        pin_name: &OsStr,
        sample: Sample,
    ) -> PinResult<(FrozenSelection, File)> {
        let frozen = FrozenSelection {
            owner_git_dir: owner.git_dir().to_owned(),
            source_parent: source.path.clone(),
            source_parent_identity: source.identity()?,
            source_ancestry: source.ancestry.clone(),
            private_parent: private.path.clone(),
            private_parent_identity: private.identity()?,
            private_ancestry: private.ancestry.clone(),
            basename: name.to_owned(),
            pin_basename: pin_name.to_owned(),
            identity: physical_identity(&sample.metadata),
            byte_len: sample.metadata.len(),
            raw_sha256: Sha256::digest(&sample.bytes).into(),
            raw_bytes: sample.bytes,
            decoded: sample.decoded,
        };
        Ok((frozen, sample.file))
    }

    fn sample_matches(sample: &Sample, frozen: &FrozenSelection) -> bool {
        physical_identity(&sample.metadata) == frozen.identity
            && sample.metadata.len() == frozen.byte_len
            && sample.bytes == frozen.raw_bytes
            && <[u8; 32]>::from(Sha256::digest(&sample.bytes)) == frozen.raw_sha256
            && sample.decoded == frozen.decoded
    }

    pub(crate) fn preserve(
        owner: &QueueLock,
        source: &Path,
        private_dir: &Path,
        pin_name: &OsStr,
    ) -> PinResult<LivePin> {
        preserve_with(owner, source, private_dir, pin_name, &mut |_| {})
    }

    fn preserve_with(
        owner: &QueueLock,
        source: &Path,
        private_dir: &Path,
        pin_name: &OsStr,
        phase: &mut impl FnMut(Phase),
    ) -> PinResult<LivePin> {
        codec::basename(pin_name)?;
        let scope = source_scope(owner, source)?;
        let private = private_namespace(private_dir, &scope.excluded)?;
        let name = source.file_name().ok_or(PinError)?;
        if scope.namespace.identity()? == private.identity()? {
            return Err(PinError);
        }
        let sample = read_regular(&scope.namespace, name, name)?;
        let (frozen, original_fd) =
            freeze(owner, &scope.namespace, &private, name, pin_name, sample)?;
        phase(Phase::BeforeLink);
        owner.check_current().map_err(|_| PinError)?;
        scope.namespace.recheck()?;
        private.recheck()?;
        scope
            .namespace
            .dir
            .hard_link(name, &private.dir, pin_name)
            .map_err(|_| PinError)?;
        phase(Phase::AfterLink);
        let pin = read_regular(&private, pin_name, name)?;
        let current = read_regular(&scope.namespace, name, name)?;
        if !sample_matches(&pin, &frozen)
            || !sample_matches(&current, &frozen)
            || read_fd(&original_fd)? != frozen.raw_bytes
        {
            return Err(PinError);
        }
        original_fd.sync_all().map_err(|_| PinError)?;
        private
            .dir
            .try_clone()
            .map_err(|_| PinError)?
            .into_std_file()
            .sync_all()
            .map_err(|_| PinError)?;
        let live = LivePin {
            frozen,
            source: scope.namespace,
            private,
            original_fd: Some(original_fd),
            pin_fd: pin.file,
            excluded: scope.excluded,
        };
        verify_pin(owner, &live)?;
        if !sample_matches(&read_regular(&live.source, name, name)?, &live.frozen) {
            return Err(PinError);
        }
        Ok(live)
    }

    pub(crate) fn reopen(
        owner: &QueueLock,
        source: &Path,
        private_dir: &Path,
        pin_name: &OsStr,
        expected: &FrozenSelection,
    ) -> PinResult<LivePin> {
        codec::basename(pin_name)?;
        let scope = source_scope(owner, source)?;
        let private = private_namespace(private_dir, &scope.excluded)?;
        if scope.namespace.path != expected.source_parent
            || private.path != expected.private_parent
            || scope.namespace.ancestry != expected.source_ancestry
            || private.ancestry != expected.private_ancestry
            || owner.git_dir() != expected.owner_git_dir
            || source.file_name() != Some(expected.basename.as_os_str())
            || pin_name != expected.pin_basename
            || scope.namespace.identity()? != expected.source_parent_identity
            || private.identity()? != expected.private_parent_identity
        {
            return Err(PinError);
        }
        let pin = read_regular(&private, pin_name, &expected.basename)?;
        if !sample_matches(&pin, expected) {
            return Err(PinError);
        }
        let frozen = expected.clone();
        let live = LivePin {
            frozen,
            source: scope.namespace,
            private,
            original_fd: None,
            pin_fd: pin.file,
            excluded: scope.excluded,
        };
        verify_pin(owner, &live)?;
        Ok(live)
    }

    fn verify_pin(owner: &QueueLock, pin: &LivePin) -> PinResult<()> {
        owner.check_current().map_err(|_| PinError)?;
        if pin.source.path != pin.frozen.source_parent
            || pin.private.path != pin.frozen.private_parent
            || pin.source.ancestry != pin.frozen.source_ancestry
            || pin.private.ancestry != pin.frozen.private_ancestry
            || owner.git_dir() != pin.frozen.owner_git_dir
            || pin.private.identity()? != pin.frozen.private_parent_identity
            || pin.source.identity()? != pin.frozen.source_parent_identity
        {
            return Err(PinError);
        }
        if pin
            .private
            .dir
            .dir_metadata()
            .map_err(|_| PinError)?
            .permissions()
            .mode()
            & 0o7777
            != 0o700
        {
            return Err(PinError);
        }
        pin.private.recheck()?;
        pin.source.recheck()?;
        check_exclusions(&pin.private, &pin.excluded)?;
        let sample = read_regular(&pin.private, &pin.frozen.pin_basename, &pin.frozen.basename)?;
        if !sample_matches(&sample, &pin.frozen)
            || physical_identity(&pin.pin_fd.metadata().map_err(|_| PinError)?)
                != pin.frozen.identity
            || read_fd(&pin.pin_fd)? != pin.frozen.raw_bytes
        {
            return Err(PinError);
        }
        if let Some(original) = &pin.original_fd {
            if physical_identity(&original.metadata().map_err(|_| PinError)?) != pin.frozen.identity
                || read_fd(original)? != pin.frozen.raw_bytes
            {
                return Err(PinError);
            }
        }
        Ok(())
    }

    pub(crate) fn classify(owner: &QueueLock, source: &Path, pin: &LivePin) -> DeliveryMatch {
        classify_with(owner, source, pin, &mut |_| {})
    }

    fn classify_with(
        owner: &QueueLock,
        source: &Path,
        pin: &LivePin,
        phase: &mut impl FnMut(Phase),
    ) -> DeliveryMatch {
        classify_checked(owner, source, pin, phase).unwrap_or(DeliveryMatch::Unavailable)
    }

    fn classify_checked(
        owner: &QueueLock,
        source: &Path,
        pin: &LivePin,
        phase: &mut impl FnMut(Phase),
    ) -> PinResult<DeliveryMatch> {
        verify_pin(owner, pin)?;
        let scope = source_scope(owner, source)?;
        check_exclusions(&pin.private, &scope.excluded)?;
        if scope.namespace.path != pin.frozen.source_parent
            || scope.namespace.ancestry != pin.frozen.source_ancestry
            || scope.namespace.identity()? != pin.frozen.source_parent_identity
            || source.file_name() != Some(pin.frozen.basename.as_os_str())
        {
            return Err(PinError);
        }
        phase(Phase::BeforeClassifySource);
        let outcome = match scope.namespace.dir.symlink_metadata(&pin.frozen.basename) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => DeliveryMatch::Missing,
            Err(_) => return Err(PinError),
            Ok(_) => {
                let sample =
                    read_regular(&scope.namespace, &pin.frozen.basename, &pin.frozen.basename)?;
                if sample_matches(&sample, &pin.frozen) {
                    DeliveryMatch::OriginalPresent
                } else {
                    DeliveryMatch::Different
                }
            }
        };
        scope.namespace.recheck()?;
        if outcome == DeliveryMatch::Missing
            && !matches!(scope.namespace.dir.symlink_metadata(&pin.frozen.basename), Err(error) if error.kind() == std::io::ErrorKind::NotFound)
        {
            return Err(PinError);
        }
        verify_pin(owner, pin)?;
        Ok(outcome)
    }

    #[cfg(test)]
    mod tests;
}

#[cfg(not(unix))]
mod platform {
    use crate::capture_queue::QueueLock;
    use crate::capture_recovery::{PinError, PinResult};
    use std::ffi::OsStr;
    use std::path::Path;

    pub(crate) struct FrozenSelection;
    pub(crate) struct LivePin;
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(crate) enum DeliveryMatch {
        OriginalPresent,
        Missing,
        Different,
        Unavailable,
    }

    pub(crate) fn preserve(_: &QueueLock, _: &Path, _: &Path, _: &OsStr) -> PinResult<LivePin> {
        Err(PinError)
    }
    pub(crate) fn reopen(
        _: &QueueLock,
        _: &Path,
        _: &Path,
        _: &OsStr,
        _: &FrozenSelection,
    ) -> PinResult<LivePin> {
        Err(PinError)
    }
    pub(crate) fn classify(_: &QueueLock, _: &Path, _: &LivePin) -> DeliveryMatch {
        DeliveryMatch::Unavailable
    }
}

// Only the physical-redelivery regression tests import these until the
// controller lands.
#[cfg_attr(
    not(test),
    expect(
        unused_imports,
        reason = "called by the CFX-4 recovery controller (eligibility → selection → retirement), not yet wired"
    )
)]
pub(crate) use platform::{classify, preserve, DeliveryMatch, LivePin};
