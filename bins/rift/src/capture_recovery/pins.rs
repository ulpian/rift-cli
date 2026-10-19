//! Live physical preservation and read-only matching. The one removal, of a
//! pinned original under a durable retirement intent, lives in `discharge`.

#[cfg(unix)]
mod platform {
    use std::ffi::{OsStr, OsString};
    #[cfg(test)]
    use std::io::Seek as _;
    use std::path::{Path, PathBuf};
    use std::rc::Rc;

    use cap_std::fs::{File, Metadata, PermissionsExt as _};
    use sha2::{Digest as _, Sha256};

    use crate::capture_queue::QueueLock;
    use crate::capture_recovery::codec::{self, Decoded, MAX_BYTES};
    use crate::capture_recovery::files::{
        self, clean_absolute, physical_identity, DirectoryIdentity, Namespace, PhysicalIdentity,
    };
    use crate::capture_recovery::selection::{Expectation, Identity, RetainedOriginal, Scope};
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

    /// The three namespaces every pin is checked against: the queue the
    /// source is named in, the private directory the pin lives in and the
    /// roots a private directory must not sit under. One pin opens its own;
    /// an operation opens them once and shares them across its slots.
    struct Scopes {
        source: Namespace,
        private: Namespace,
        excluded: Vec<Namespace>,
    }

    /// The scopes of one operation, opened once; every slot's pin shares them.
    pub(crate) struct SharedScope(Rc<Scopes>);

    pub(crate) struct LivePin {
        frozen: FrozenSelection,
        scopes: Rc<Scopes>,
        original_fd: Option<File>,
        pin_fd: File,
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

    /// The queue directory the owner's family states for this member and the
    /// roots a private directory must never sit under, resolved once.
    struct Family {
        queue: PathBuf,
        excluded: [PathBuf; 3],
    }

    /// What one read proved about a regular file, whichever handle it came
    /// through: the metadata taken with the bytes, and the bytes decoded.
    struct Facts {
        metadata: Metadata,
        bytes: Vec<u8>,
        decoded: Decoded,
    }

    struct Sample {
        file: File,
        metadata: Metadata,
        bytes: Vec<u8>,
        decoded: Decoded,
    }

    /// Where a frozen selection sits: the owner and the two namespaces, with
    /// the source and pin names under them.
    struct Placement<'a> {
        owner: &'a QueueLock,
        source: &'a Namespace,
        private: &'a Namespace,
        name: &'a OsStr,
        pin_name: &'a OsStr,
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

    impl Sample {
        fn split(self) -> (File, Facts) {
            let facts = Facts {
                metadata: self.metadata,
                bytes: self.bytes,
                decoded: self.decoded,
            };
            (self.file, facts)
        }
    }

    impl SharedScope {
        /// Every slot rechecks the owner, both namespaces, the private mode
        /// and the exclusions before trusting a name under them.
        pub(crate) fn recheck(&self, owner: &QueueLock) -> PinResult<()> {
            owner.check_current().map_err(|_| PinError)?;
            self.0.source.recheck()?;
            self.0.private.recheck()?;
            check_private_mode(&self.0.private)?;
            check_exclusions(&self.0.private, &self.0.excluded)
        }

        /// The scope the intent froze is this scope: same paths, same
        /// directory identities, same ancestry on both sides.
        pub(crate) fn matches_scope(&self, expected: &Scope) -> PinResult<()> {
            let same_ancestry = |held: &[DirectoryIdentity], frozen: &[Identity]| {
                held.len() == frozen.len()
                    && held
                        .iter()
                        .zip(frozen)
                        .all(|(h, f)| (h.dev, h.ino) == (f.dev, f.ino))
            };
            let same_identity = |held: DirectoryIdentity, frozen: Identity| {
                (held.dev, held.ino) == (frozen.dev, frozen.ino)
            };
            if self.0.source.path.as_os_str() != OsStr::new(&expected.source_queue_dir)
                || self.0.private.path.as_os_str() != OsStr::new(&expected.private_dir)
                || !same_identity(self.0.source.identity()?, expected.source_dir_identity)
                || !same_identity(self.0.private.identity()?, expected.private_dir_identity)
                || !same_ancestry(&self.0.source.ancestry, &expected.source_ancestry)
                || !same_ancestry(&self.0.private.ancestry, &expected.private_ancestry)
            {
                return Err(PinError);
            }
            Ok(())
        }

        /// Whether the queue names `name` right now, without following; the
        /// link needs the name, the retained descriptor does not.
        pub(crate) fn source_present(&self, name: &str) -> PinResult<bool> {
            present(&self.0.source, name)
        }

        pub(crate) fn pin_present(&self, name: &str) -> PinResult<bool> {
            present(&self.0.private, name)
        }
    }

    fn present(namespace: &Namespace, name: &str) -> PinResult<bool> {
        codec::basename(OsStr::new(name))?;
        namespace.recheck()?;
        match namespace.dir.symlink_metadata(name) {
            Ok(_) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(_) => Err(PinError),
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

    /// The retained descriptor read whole under the cap and decoded under the
    /// source's name; the descriptor, never the path, is what is sampled.
    fn facts_of_fd(file: &File, decode_name: &OsStr) -> PinResult<Facts> {
        let bytes = read_fd(file)?;
        let metadata = file.metadata().map_err(|_| PinError)?;
        let decoded = codec::decode(decode_name, &bytes)?;
        Ok(Facts {
            metadata,
            bytes,
            decoded,
        })
    }

    fn family_of(owner: &QueueLock) -> PinResult<Family> {
        owner.check_current().map_err(|_| PinError)?;
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
        let queue = rift_shadow::hooks::run_state::RunRoot::AgentQueue.under(&member);
        let run_root = member.parent().ok_or(PinError)?.to_owned();
        Ok(Family {
            queue,
            excluded: [work_tree, run_root, owner.git_dir().to_owned()],
        })
    }

    /// The namespace at `queue` is the family's agent queue, and the roots a
    /// private directory must not sit under are opened beside it.
    fn queue_scope(family: &Family, queue: &Path) -> PinResult<SourceScope> {
        clean_absolute(queue)?;
        let namespace = Namespace::open(queue)?;
        let actual = Namespace::open(&family.queue)?;
        if namespace.identity()? != actual.identity()? {
            return Err(PinError);
        }
        let mut excluded = Vec::with_capacity(family.excluded.len());
        for path in &family.excluded {
            excluded.push(Namespace::open(path)?);
        }
        Ok(SourceScope {
            namespace,
            excluded,
        })
    }

    fn source_scope(owner: &QueueLock, source: &Path) -> PinResult<SourceScope> {
        owner.check_current().map_err(|_| PinError)?;
        clean_absolute(source)?;
        codec::basename(source.file_name().ok_or(PinError)?)?;
        let family = family_of(owner)?;
        queue_scope(&family, source.parent().ok_or(PinError)?)
    }

    /// The operation's scopes, opened once: the queue at `queue_dir`, the
    /// private directory at `private_dir` outside every excluded root and
    /// distinct from the queue.
    pub(crate) fn shared_scope(
        owner: &QueueLock,
        queue_dir: &Path,
        private_dir: &Path,
    ) -> PinResult<SharedScope> {
        let family = family_of(owner)?;
        let scope = queue_scope(&family, queue_dir)?;
        let private = private_namespace(private_dir, &scope.excluded)?;
        if scope.namespace.identity()? == private.identity()? {
            return Err(PinError);
        }
        Ok(SharedScope(Rc::new(Scopes {
            source: scope.namespace,
            private,
            excluded: scope.excluded,
        })))
    }

    fn private_namespace(path: &Path, excluded: &[Namespace]) -> PinResult<Namespace> {
        let namespace = Namespace::open(path)?;
        check_exclusions(&namespace, excluded)?;
        check_private_mode(&namespace)?;
        Ok(namespace)
    }

    fn check_private_mode(namespace: &Namespace) -> PinResult<()> {
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
        Ok(())
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
        let (file, facts) = sample.split();
        let place = Placement {
            owner,
            source,
            private,
            name,
            pin_name,
        };
        Ok((freeze_facts(&place, facts)?, file))
    }

    fn freeze_facts(place: &Placement<'_>, facts: Facts) -> PinResult<FrozenSelection> {
        Ok(FrozenSelection {
            owner_git_dir: place.owner.git_dir().to_owned(),
            source_parent: place.source.path.clone(),
            source_parent_identity: place.source.identity()?,
            source_ancestry: place.source.ancestry.clone(),
            private_parent: place.private.path.clone(),
            private_parent_identity: place.private.identity()?,
            private_ancestry: place.private.ancestry.clone(),
            basename: place.name.to_owned(),
            pin_basename: place.pin_name.to_owned(),
            identity: physical_identity(&facts.metadata),
            byte_len: facts.metadata.len(),
            raw_sha256: Sha256::digest(&facts.bytes).into(),
            raw_bytes: facts.bytes,
            decoded: facts.decoded,
        })
    }

    /// The facts a handle proved are exactly the expectation the intent
    /// froze: identity, mode, length, raw digest, context digest, attempts.
    fn meets(facts: &Facts, expectation: &Expectation) -> PinResult<()> {
        let identity = physical_identity(&facts.metadata);
        let length = u64::try_from(facts.bytes.len()).map_err(|_| PinError)?;
        if !facts.metadata.is_file()
            || (identity.dev, identity.ino, identity.permission_bits)
                != (expectation.dev, expectation.ino, expectation.mode)
            || facts.metadata.len() != expectation.raw_length
            || length != expectation.raw_length
            || format!("{:x}", Sha256::digest(&facts.bytes)) != expectation.ticket_sha256
            || codec::context_sha256(&facts.decoded)? != expectation.context_sha256
            || facts.decoded.attempts != expectation.attempts
        {
            return Err(PinError);
        }
        Ok(())
    }

    /// The frozen selection of an expectation under the shared scopes.
    fn freeze_expected(
        owner: &QueueLock,
        scopes: &Scopes,
        expectation: &Expectation,
        facts: Facts,
    ) -> PinResult<FrozenSelection> {
        meets(&facts, expectation)?;
        let place = Placement {
            owner,
            source: &scopes.source,
            private: &scopes.private,
            name: OsStr::new(&expectation.source_name),
            pin_name: OsStr::new(&expectation.pin_name),
        };
        freeze_facts(&place, facts)
    }

    fn sample_matches(sample: &Sample, frozen: &FrozenSelection) -> bool {
        physical_identity(&sample.metadata) == frozen.identity
            && sample.metadata.len() == frozen.byte_len
            && sample.bytes == frozen.raw_bytes
            && <[u8; 32]>::from(Sha256::digest(&sample.bytes)) == frozen.raw_sha256
            && sample.decoded == frozen.decoded
    }

    /// Reread the RETAINED descriptor — never the path — and freeze it only if
    /// it is still exactly the original the intent expects.
    pub(crate) fn inspect_retained(
        owner: &QueueLock,
        scope: &SharedScope,
        original: &RetainedOriginal,
    ) -> PinResult<FrozenSelection> {
        scope.recheck(owner)?;
        let name = OsStr::new(&original.expectation.source_name);
        codec::basename(name)?;
        codec::basename(OsStr::new(&original.expectation.pin_name))?;
        let facts = facts_of_fd(&original.file, name)?;
        freeze_expected(owner, &scope.0, &original.expectation, facts)
    }

    /// Hard-link the inspected original into the private directory under its
    /// expected pin name and run every post-link check; the original's
    /// descriptor moves into the pin and is released with it.
    pub(crate) fn preserve_selected(
        owner: &QueueLock,
        scope: &SharedScope,
        original: RetainedOriginal,
        frozen: FrozenSelection,
    ) -> PinResult<LivePin> {
        let expectation = &original.expectation;
        if frozen.basename != OsStr::new(&expectation.source_name)
            || frozen.pin_basename != OsStr::new(&expectation.pin_name)
            || (
                frozen.identity.dev,
                frozen.identity.ino,
                frozen.identity.permission_bits,
            ) != (expectation.dev, expectation.ino, expectation.mode)
            || frozen.source_parent != scope.0.source.path
            || frozen.private_parent != scope.0.private.path
        {
            return Err(PinError);
        }
        link_and_validate(owner, &scope.0, frozen, original.file, &mut |_| {})
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
        let scopes = Rc::new(Scopes {
            source: scope.namespace,
            private,
            excluded: scope.excluded,
        });
        link_and_validate(owner, &scopes, frozen, original_fd, phase)
    }

    /// The link and everything after it: the pin, the current source and the
    /// original descriptor must all still be the frozen sample, the original
    /// and the private directory are synced, and the live pin verifies.
    fn link_and_validate(
        owner: &QueueLock,
        scopes: &Rc<Scopes>,
        frozen: FrozenSelection,
        original_fd: File,
        phase: &mut impl FnMut(Phase),
    ) -> PinResult<LivePin> {
        let name = frozen.basename.clone();
        let pin_name = frozen.pin_basename.clone();
        phase(Phase::BeforeLink);
        owner.check_current().map_err(|_| PinError)?;
        scopes.source.recheck()?;
        scopes.private.recheck()?;
        scopes
            .source
            .dir
            .hard_link(&name, &scopes.private.dir, &pin_name)
            .map_err(|_| PinError)?;
        phase(Phase::AfterLink);
        let pin = read_regular(&scopes.private, &pin_name, &name)?;
        let current = read_regular(&scopes.source, &name, &name)?;
        if !sample_matches(&pin, &frozen)
            || !sample_matches(&current, &frozen)
            || read_fd(&original_fd)? != frozen.raw_bytes
        {
            return Err(PinError);
        }
        original_fd.sync_all().map_err(|_| PinError)?;
        scopes
            .private
            .dir
            .try_clone()
            .map_err(|_| PinError)?
            .into_std_file()
            .sync_all()
            .map_err(|_| PinError)?;
        let live = LivePin {
            frozen,
            scopes: Rc::clone(scopes),
            original_fd: Some(original_fd),
            pin_fd: pin.file,
        };
        verify_pin(owner, &live)?;
        if !sample_matches(
            &read_regular(&live.scopes.source, &name, &name)?,
            &live.frozen,
        ) {
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
        let live = LivePin {
            frozen: expected.clone(),
            scopes: Rc::new(Scopes {
                source: scope.namespace,
                private,
                excluded: scope.excluded,
            }),
            original_fd: None,
            pin_fd: pin.file,
        };
        verify_pin(owner, &live)?;
        Ok(live)
    }

    /// An existing pin, read through the private namespace by its expected
    /// name: it must be the expectation on dev/ino/mode/length/digests/
    /// attempts — the pin aliases the original inode, so dev/ino equality is
    /// the physical witness — and its own bytes rebuild the frozen selection.
    pub(crate) fn reopen_expected(
        owner: &QueueLock,
        scope: &SharedScope,
        expectation: &Expectation,
    ) -> PinResult<LivePin> {
        scope.recheck(owner)?;
        let name = OsStr::new(&expectation.source_name);
        let pin_name = OsStr::new(&expectation.pin_name);
        codec::basename(name)?;
        codec::basename(pin_name)?;
        let (file, facts) = read_regular(&scope.0.private, pin_name, name)?.split();
        let frozen = freeze_expected(owner, &scope.0, expectation, facts)?;
        let live = LivePin {
            frozen,
            scopes: Rc::clone(&scope.0),
            original_fd: None,
            pin_fd: file,
        };
        verify_pin(owner, &live)?;
        Ok(live)
    }

    pub(crate) fn verify_pin(owner: &QueueLock, pin: &LivePin) -> PinResult<()> {
        owner.check_current().map_err(|_| PinError)?;
        let scopes = &pin.scopes;
        if scopes.source.path != pin.frozen.source_parent
            || scopes.private.path != pin.frozen.private_parent
            || scopes.source.ancestry != pin.frozen.source_ancestry
            || scopes.private.ancestry != pin.frozen.private_ancestry
            || owner.git_dir() != pin.frozen.owner_git_dir
            || scopes.private.identity()? != pin.frozen.private_parent_identity
            || scopes.source.identity()? != pin.frozen.source_parent_identity
        {
            return Err(PinError);
        }
        check_private_mode(&scopes.private)?;
        scopes.private.recheck()?;
        scopes.source.recheck()?;
        check_exclusions(&scopes.private, &scopes.excluded)?;
        let sample = read_regular(
            &scopes.private,
            &pin.frozen.pin_basename,
            &pin.frozen.basename,
        )?;
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
        check_exclusions(&pin.scopes.private, &scope.excluded)?;
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
            Ok(metadata) => present_match(&scope.namespace, &metadata, &pin.frozen)?,
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

    /// What sits under the original's name. The original was a regular,
    /// capped, decodable ticket, so anything that is not one is certainly a
    /// different file, not an unreadable queue; only a failed read of a
    /// candidate stays unavailable.
    fn present_match(
        namespace: &Namespace,
        metadata: &Metadata,
        frozen: &FrozenSelection,
    ) -> PinResult<DeliveryMatch> {
        if !metadata.is_file() || metadata.len() != frozen.byte_len {
            return Ok(DeliveryMatch::Different);
        }
        let raw = files::read_regular(namespace, &frozen.basename, MAX_BYTES)?;
        let Ok(decoded) = codec::decode(&frozen.basename, &raw.bytes) else {
            return Ok(DeliveryMatch::Different);
        };
        let sample = Sample {
            file: raw.file,
            metadata: raw.metadata,
            bytes: raw.bytes,
            decoded,
        };
        Ok(if sample_matches(&sample, frozen) {
            DeliveryMatch::OriginalPresent
        } else {
            DeliveryMatch::Different
        })
    }

    pub(crate) mod discharge;
    #[cfg(test)]
    mod selected_tests;
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
#[cfg(unix)]
pub(crate) use platform::discharge::{discharge_original, Discharge};
#[cfg_attr(
    not(test),
    expect(
        unused_imports,
        reason = "called by the CFX-4 recovery controller (eligibility → selection → retirement), not yet wired"
    )
)]
pub(crate) use platform::{classify, preserve, DeliveryMatch, LivePin};
#[cfg(unix)]
pub(crate) use platform::{
    inspect_retained, preserve_selected, reopen_expected, shared_scope, verify_pin, SharedScope,
};
