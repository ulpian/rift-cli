//! The two directories: the family's own copy of every answer, and one
//! member's view of them.
//!
//! Separate from the rest because it is the only half that writes: everything
//! here is a filesystem operation on one of those two directories, so a crash
//! leaves either the old answer or the new one and never a torn one.

use std::path::{Path, PathBuf};

use rift_core::Result;

use super::sweep::{older_than, sidecars, sweep_tmps, sweep_windows};

/// The family's own directory inside the git common dir — the same one the
/// family manifest lives in, because they answer the same question: what does
/// this repository share between its checkouts?
const FAMILY_DIR: &str = "rift-family";

/// The sidecar subdirectory, under the family directory and under a member's
/// run state alike.
const DOC_DIR: &str = "doc";

/// The extension a converted document is written under.
pub(super) const SIDECAR_EXT: &str = "md";

/// The marker written beside a key that must not be converted again: a scanned
/// PDF, an encrypted or malformed container. Its content is a one-word reason
/// for whoever reads the directory; nothing parses it back.
const SKIP_EXT: &str = "skip";

/// The family's sidecars and one member's view of them.
///
/// Two paths and no state: every operation is a filesystem operation, so a
/// crash leaves either the old answer or the new one and never a torn one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DocStore {
    family: PathBuf,
    member: PathBuf,
}

impl DocStore {
    /// The store as `work_tree` sees it, given the family's common directory.
    ///
    /// `common_dir` is the family's — canonical, shared by every member — so
    /// two members of one family name the same file, and a member that is
    /// removed tomorrow still contributes to it today.
    pub(crate) fn for_member(common_dir: &Path, work_tree: &Path) -> Self {
        Self {
            family: common_dir.join(FAMILY_DIR).join(DOC_DIR),
            member: work_tree.join(".rift/state/doc"),
        }
    }

    /// Where the family keeps `key`'s answer.
    pub(crate) fn family_path(&self, key: &str) -> PathBuf {
        self.family.join(format!("{key}.{SIDECAR_EXT}"))
    }

    /// Where this member sees it.
    pub(crate) fn member_path(&self, key: &str) -> PathBuf {
        self.member.join(format!("{key}.{SIDECAR_EXT}"))
    }

    /// The marker that says `key` was looked at and is not to be looked at
    /// again.
    fn skip_path(&self, key: &str) -> PathBuf {
        self.family.join(format!("{key}.{SKIP_EXT}"))
    }

    /// Whether the family already holds this answer — one `stat`, no writes.
    ///
    /// This is the whole of what a decision on a hook path may do: ask, and
    /// hand over what is already there. Converting is the worker's.
    pub(crate) fn holds(&self, key: &str) -> bool {
        self.family_path(key).is_file()
    }

    /// Whether this key was decided against: a document whose conversion is
    /// not worth substituting (a scanned PDF) or cannot be produced at all (an
    /// encrypted or malformed container). Checked before a conversion is
    /// attempted, so a document that refused once refuses cheaply, for every
    /// member of the family.
    pub(crate) fn is_skipped(&self, key: &str) -> bool {
        self.skip_path(key).exists()
    }

    /// Record `key`'s answer in the family store and link it into this member.
    ///
    /// Atomic by rename inside the family directory, so a reader sees either
    /// the whole sidecar or no sidecar. Recording an answer that is already
    /// there is not a rewrite: the key is the hash of what this would have
    /// written.
    ///
    /// Written only where a conversion happened, so a build without the
    /// document seam reads and sweeps this store without ever writing to it.
    ///
    /// # Errors
    /// [`rift_core::RiftError::Io`] if the family directory cannot be written.
    #[cfg(feature = "doc")]
    pub(crate) fn put(&self, key: &str, text: &str) -> Result<PathBuf> {
        let path = self.family_path(key);
        if !path.is_file() {
            std::fs::create_dir_all(&self.family)?;
            // Named for the writing process, so two workers converting one
            // document at the same instant cannot tear each other's temp
            // file. Both then rename onto the same name with the same bytes,
            // and the loser's rename is a no-op.
            let tmp = self
                .family
                .join(format!("{key}.{SIDECAR_EXT}.tmp{}", std::process::id()));
            std::fs::write(&tmp, text.as_bytes())?;
            restrict(&tmp);
            std::fs::rename(&tmp, &path)?;
        }
        self.link(key)
    }

    /// Say that `key` is not to be attempted again, and why in one word.
    ///
    /// Written only for a decision about the *document* — no text layer,
    /// encrypted, not the format its name claims. A helper that was missing or
    /// a deadline that was missed says nothing about the document and leaves
    /// no marker, so the next pass tries again. Like [`Self::put`], a decision
    /// about a document is something only a converting build reaches.
    #[cfg(feature = "doc")]
    pub(crate) fn mark_skipped(&self, key: &str, reason: &str) {
        if std::fs::create_dir_all(&self.family).is_ok() {
            let _ = std::fs::write(self.skip_path(key), reason.as_bytes());
        }
    }

    /// Link the family's answer into this member, copying where the filesystem
    /// will not link (a different device, or one with no hardlinks at all).
    ///
    /// # Errors
    /// [`rift_core::RiftError::Io`] if neither a link nor a copy could be made.
    pub(crate) fn link(&self, key: &str) -> Result<PathBuf> {
        let source = self.family_path(key);
        let member = self.member_path(key);
        if member.is_file() {
            return Ok(member);
        }
        std::fs::create_dir_all(&self.member)?;
        match std::fs::hard_link(&source, &member) {
            Ok(()) => Ok(member),
            // Somebody linked it between the two lines above. The name is the
            // hash of the content, so whoever won wrote the same bytes.
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(member),
            Err(_) => {
                std::fs::copy(&source, &member)?;
                Ok(member)
            }
        }
    }

    /// What this member and its family are holding, for `rift doctor`.
    pub(crate) fn survey(&self) -> Survey {
        let mut survey = Survey::default();
        for (_, path) in sidecars(&self.family) {
            survey.sidecars += 1;
            survey.bytes += std::fs::metadata(&path).map_or(0, |m| m.len());
        }
        for (key, _) in sidecars(&self.member) {
            survey.member_links += 1;
            if !self.holds(&key) {
                survey.orphan_links += 1;
            }
        }
        survey
    }

    /// Collect what has expired and what has been orphaned, and say how much.
    ///
    /// * A family sidecar untouched for longer than the TTL goes, with this
    ///   member's view of it. Losing one costs a conversion and never an
    ///   answer: the document is still in the checkout and still hashes to the
    ///   same key.
    /// * A member view with no family answer behind it is an **orphan** — the
    ///   remains of an expired sidecar, or of a family store somebody removed
    ///   — and it goes whatever its age, because nothing will ever refresh it
    ///   and an agent must not be pointed at it.
    /// * A temp file past the TTL is the corpse of a write that died before
    ///   its rename.
    ///
    /// Total and quiet, like every other sweep in this tool: a name this
    /// module did not write is left alone however it got there, a file whose
    /// age cannot be read is kept (removing on an unknown age would collect a
    /// sidecar written a second ago), and a removal that fails is not counted.
    pub(crate) fn sweep(&self, now_ms: i64, ttl_ms: i64, dry_run: bool) -> SweepReport {
        let cutoff = now_ms.saturating_sub(ttl_ms);
        let mut report = SweepReport::default();
        for (key, path) in sidecars(&self.family) {
            if !older_than(&path, cutoff) {
                continue;
            }
            report.expired += 1;
            if !dry_run {
                let _ = std::fs::remove_file(&path);
                let _ = std::fs::remove_file(self.skip_path(&key));
                let _ = std::fs::remove_file(self.member_path(&key));
            }
        }
        for (key, path) in sidecars(&self.member) {
            if self.holds(&key) {
                continue;
            }
            report.orphan_links += 1;
            if !dry_run {
                let _ = std::fs::remove_file(&path);
            }
        }
        report.swept_tmps = sweep_tmps(&self.family, cutoff, dry_run);
        report.swept_windows = sweep_windows(&self.member, cutoff, dry_run);
        report
    }
}

/// What a member and its family hold, for `rift doctor`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Survey {
    /// Sidecars in the family store.
    pub(crate) sidecars: usize,
    /// Bytes those sidecars occupy.
    pub(crate) bytes: u64,
    /// Views this member holds.
    pub(crate) member_links: usize,
    /// Views whose family answer is gone.
    pub(crate) orphan_links: usize,
}

/// What one sweep removed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct SweepReport {
    /// Family sidecars past the TTL.
    pub(crate) expired: usize,
    /// Member views with no family answer behind them.
    pub(crate) orphan_links: usize,
    /// Temp files left by a write that died before its rename.
    pub(crate) swept_tmps: usize,
    /// Window directories whose conversation ended longer ago than the TTL.
    pub(crate) swept_windows: usize,
}

/// Owner-only permissions on a sidecar (unix): it is a re-encoding of a file
/// in the checkout, and it inherits that file's audience rather than the
/// umask's.
///
/// Both callers — the sidecar a conversion writes and the marker a window
/// creates — belong to the document seam.
#[cfg(feature = "doc")]
pub(super) fn restrict(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    let _ = path;
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "doc")]
    use super::*;
    #[cfg(feature = "doc")]
    use crate::doc_store::key::sidecar_key;
    #[cfg(feature = "doc")]
    use crate::doc_store::tests::{family, store_for, Fixture, SHA_A, SHA_B};
    #[cfg(feature = "doc")]
    use crate::doc_store::window::DocWindow;
    #[cfg(feature = "doc")]
    use rift_recall_policy::DocPolicy;

    /// ★ The load-bearing property of the store: a document converted while
    /// working in one member of the family is **linked** into the next member
    /// that wants it, never converted again — and the two members are looking
    /// at one file rather than two copies that can drift.
    #[cfg(feature = "doc")]
    #[test]
    fn a_sidecar_converted_in_one_member_is_hardlinked_not_reconverted_in_another() {
        let f = family();
        let doc = DocPolicy::default();
        let key = sidecar_key(SHA_A, &doc).unwrap();
        let main = store_for(&f, &f.main);
        let linked = store_for(&f, &f.linked);

        // Neither member holds it, so neither can hand it over: a lookup is a
        // question, never a conversion.
        assert!(!main.holds(&key));
        assert!(!linked.member_path(&key).exists());

        let converted = "<!-- converted by rift -->\n\n# Spec\n";
        let in_main = main.put(&key, converted).unwrap();
        assert!(in_main.starts_with(&f.main), "{}", in_main.display());

        // The second member finds the answer already made and takes a view of
        // it. Nothing was converted here: the store was never asked to.
        assert!(
            linked.holds(&key),
            "one store, asked from the other checkout"
        );
        let in_linked = linked.link(&key).unwrap();
        assert!(in_linked.starts_with(&f.linked));
        assert_eq!(std::fs::read_to_string(&in_linked).unwrap(), converted);

        // One file, three names: the family's, and both members' views.
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            let ino = |p: &Path| std::fs::metadata(p).unwrap().ino();
            let family_copy = main.family_path(&key);
            assert_eq!(ino(&family_copy), ino(&in_main));
            assert_eq!(
                ino(&family_copy),
                ino(&in_linked),
                "a member's view is a hardlink to the family's one copy"
            );
            assert_eq!(std::fs::metadata(&family_copy).unwrap().nlink(), 3);
        }

        // And the family store is one directory under the common dir both
        // members share — not one per checkout.
        assert_eq!(main.family_path(&key), linked.family_path(&key));
        assert!(main.family_path(&key).starts_with(&f.common_dir));
    }

    /// A temporary worktree comes and goes; what it converted stays with the
    /// family. Removing the member removes only its view — the family's one
    /// copy is untouched and the next member still finds it.
    #[cfg(feature = "doc")]
    #[test]
    fn a_removed_worktree_leaves_the_family_store_intact() {
        let f = family();
        let doc = DocPolicy::default();
        let key = sidecar_key(SHA_A, &doc).unwrap();
        let linked = store_for(&f, &f.linked);
        let converted = "<!-- converted by rift -->\n\n# Spec\n";
        linked.put(&key, converted).unwrap();
        let family_copy = linked.family_path(&key);
        assert!(family_copy.exists());

        std::fs::remove_dir_all(&f.linked).unwrap();
        assert!(!f.linked.exists());

        assert!(
            family_copy.exists(),
            "the family's copy outlives the member"
        );
        assert_eq!(std::fs::read_to_string(&family_copy).unwrap(), converted);
        let main = store_for(&f, &f.main);
        assert!(main.holds(&key), "another member still finds the answer");
        let in_main = main.link(&key).unwrap();
        assert_eq!(std::fs::read_to_string(in_main).unwrap(), converted);
    }

    /// A document that must not be converted again says so once, in the
    /// family, so every member of the family stops asking.
    #[cfg(feature = "doc")]
    #[test]
    fn a_decision_against_a_document_is_recorded_for_the_whole_family() {
        let f = family();
        let key = sidecar_key(SHA_A, &DocPolicy::default()).unwrap();
        let main = store_for(&f, &f.main);
        assert!(!main.is_skipped(&key));
        main.mark_skipped(&key, "thin");
        assert!(store_for(&f, &f.linked).is_skipped(&key));
        assert!(!main.holds(&key), "a marker is not an answer");
    }

    /// Everything one sweep has to tell apart, in one family store: a fresh
    /// sidecar and a stale one, a decision recorded about the stale one, a
    /// member view whose family answer somebody removed by hand, the corpse of
    /// a write that died before its rename, and a file this store never wrote.
    #[cfg(feature = "doc")]
    struct Seeded {
        f: Fixture,
        store: DocStore,
        fresh: String,
        stale: String,
        orphan: PathBuf,
        tmp: PathBuf,
        stranger: PathBuf,
    }

    /// Far enough ahead of any file's real modification time that the sweep's
    /// cutoff, and not a clock this test controls, decides what is old: the
    /// sweep takes `now` from its caller precisely so a test never has to wait
    /// or backdate.
    #[cfg(feature = "doc")]
    const LATER: i64 = 4_102_444_800_000;

    #[cfg(feature = "doc")]
    fn seeded() -> Seeded {
        let f = family();
        let doc = DocPolicy::default();
        let store = store_for(&f, &f.main);
        let fresh = sidecar_key(SHA_A, &doc).unwrap();
        let stale = sidecar_key(SHA_B, &doc).unwrap();
        store.put(&fresh, "# fresh\n").unwrap();
        store.put(&stale, "# stale\n").unwrap();
        store.mark_skipped(&stale, "thin");
        let orphan =
            store.member_path("cc11bb22cc33dd44ee55ff66007788990011223344556677889900aabbccddee");
        std::fs::write(&orphan, "# orphan\n").unwrap();
        let tmp = store.family_path(&fresh).with_extension("md.tmp999");
        std::fs::write(&tmp, "half a conversion").unwrap();
        let stranger = store.family_path(&fresh).with_file_name("notes.md");
        std::fs::write(&stranger, "someone else's file").unwrap();
        Seeded {
            f,
            store,
            fresh,
            stale,
            orphan,
            tmp,
            stranger,
        }
    }

    /// What `rift doctor` counts, before anything is collected.
    #[cfg(feature = "doc")]
    #[test]
    fn the_doctor_counts_the_family_sidecars_and_this_members_views() {
        let s = seeded();
        assert_eq!(
            s.store.survey(),
            Survey {
                sidecars: 2,
                bytes: 16,
                member_links: 3,
                orphan_links: 1,
            },
            "the doctor's four numbers, before anything is collected"
        );
    }

    /// A dry run is a count and nothing else.
    #[cfg(feature = "doc")]
    #[test]
    fn a_dry_run_counts_what_a_sweep_would_collect_and_removes_nothing() {
        let s = seeded();
        let dry = s.store.sweep(LATER, 0, true);
        assert_eq!(
            (dry.expired, dry.orphan_links, dry.swept_tmps),
            (2, 1, 1),
            "a dry run counts everything and removes nothing"
        );
        assert!(s.store.holds(&s.fresh) && s.store.holds(&s.stale) && s.orphan.exists());
    }

    /// An expired answer must not leave a link an agent can still be pointed
    /// at, so a member view with nothing behind it goes whatever its age —
    /// while inside the TTL nothing else does.
    #[cfg(feature = "doc")]
    #[test]
    fn inside_the_ttl_only_a_view_with_nothing_behind_it_goes() {
        let s = seeded();
        let inside = s.store.sweep(LATER, i64::MAX / 2, false);
        assert_eq!(
            (inside.expired, inside.swept_tmps),
            (0, 0),
            "inside the TTL nothing expires"
        );
        assert_eq!(inside.orphan_links, 1, "an orphan has no TTL to be inside");
        assert!(!s.orphan.exists());
        assert!(s.store.holds(&s.fresh) && s.store.holds(&s.stale));
    }

    /// `rift clean`'s document leg past the TTL: a sidecar nobody has used
    /// since goes, and this member's view of it, the decision recorded about
    /// it and the corpse of a torn write go with it.
    #[cfg(feature = "doc")]
    #[test]
    fn past_the_ttl_a_sidecar_its_view_its_decision_and_its_temp_file_all_go() {
        let s = seeded();
        let past = s.store.sweep(LATER, 0, false);
        assert_eq!((past.expired, past.swept_tmps), (2, 1));
        assert!(!s.store.holds(&s.fresh) && !s.store.holds(&s.stale));
        assert!(!s.tmp.exists());
        assert!(
            !s.store.member_path(&s.fresh).exists(),
            "a member's view goes with the answer behind it"
        );
        assert!(
            !s.store.is_skipped(&s.stale),
            "and so does a decision about a document nobody keeps"
        );
        assert!(
            s.stranger.exists(),
            "a file this store did not write is left"
        );
        assert_eq!(s.store.survey(), Survey::default());
    }

    /// What one conversation was told about documents goes with the rest of
    /// its run state: the window's markers are only ever created, so the
    /// directory's own time is the last thing that window was told.
    #[cfg(feature = "doc")]
    #[test]
    fn a_window_is_forgotten_only_once_its_conversation_is_past_the_ttl() {
        let s = seeded();
        let window = DocWindow::at(&s.f.main, "s-1", None);
        window.arm_rewrite("docs/plan.docx", SHA_B, SHA_A);
        assert!(window.flip_to_redirect());
        assert!(window.is_redirecting());
        assert_eq!(s.store.sweep(LATER, i64::MAX / 2, false).swept_windows, 0);
        assert!(
            window.is_redirecting(),
            "inside the TTL nothing is forgotten"
        );
        assert_eq!(
            s.store.sweep(LATER, 0, true).swept_windows,
            1,
            "a dry run counts it"
        );
        assert!(window.is_redirecting(), "and removes nothing");

        let past = s.store.sweep(LATER, 0, false);
        assert_eq!(past.swept_windows, 1, "an ended conversation is collected");
        assert!(!window.is_redirecting());
        assert_eq!(
            window.take_armed("docs/plan.docx"),
            None,
            "and so is what it was told"
        );
    }
}
