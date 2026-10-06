//! What the map can say about each file a window held, and the list it says it
//! about.
//!
//! One module because the guard, the entry cap and the verdict are the three
//! things that decide whether a held file becomes an entry, and the
//! announcement is read back out of that same list by position — so the list
//! and the reading of it must not be able to drift apart.

/// The most entries one map will consider naming, before the budget has its
/// say. A session that read six hundred files is a session whose map is a wall
/// of paths; the warm end of the list is where the next read is, and the rest
/// is a count.
const MAX_RECOVERY_ENTRIES: usize = 64;

/// The most files one map asks a verdict about — the cap the diff module
/// declares, applied here because this is the caller it was declared for. Past
/// it an entry is its path, its revision and the lines held — facts that cost
/// nothing to state — and no claim about what the file is now.
const MAX_RECOVERY_VERDICTS: usize = rift_recall_serve::held_diff::MAX_DIFFED_FILES;

/// What the map can say about one file the session held.
enum RecoveryVerdict {
    /// The bytes still hash to the revision the session read.
    Unchanged,
    /// The file changed, in these places in the numbering a reader sees now,
    /// with the shape of the revision it holds now when a valid outline of
    /// that revision is bound.
    ChangedAt {
        sites: Vec<(u32, u32)>,
        shape: Option<rift_recall_serve::OutlineShape>,
    },
    /// Nothing was reachable to compare against, so nothing is claimed.
    Unknown,
}

/// One entry of the map, owned, so the borrowed render type can be built from
/// it in one pass at the end.
///
/// It carries the announcement's own facts — the revision the window read and
/// the runs it holds of that revision — rather than a way back into the held
/// set it was built from. The renderer names entries by their position in
/// *this* list and the guard below drops held files from it, so the two lists
/// have different positions in them; an entry that answers for itself cannot
/// be answered for by the wrong file.
pub(super) struct RecoveryEntry {
    path: String,
    hash: String,
    held: Vec<(u32, u32)>,
    verdict: RecoveryVerdict,
    /// The revision the window read, spelled as the journal writes it.
    revision: rift_core::Sha256Id,
    /// The runs the window holds of that revision, as the journal writes them.
    runs: Vec<rift_agent::window_journal::Interval>,
}

impl RecoveryEntry {
    /// This entry as the renderer takes it: borrowed, in one pass, at the end.
    ///
    /// The renderer names entries by their position in the very list it is
    /// handed, so the conversion preserves order and drops nothing.
    pub(super) fn rendered(&self) -> rift_recall_serve::HeldEntry<'_> {
        rift_recall_serve::HeldEntry {
            path: &self.path,
            hash: &self.hash,
            held: &self.held,
            verdict: match &self.verdict {
                RecoveryVerdict::Unchanged => rift_recall_serve::HeldVerdict::Unchanged,
                RecoveryVerdict::ChangedAt { sites, .. } => {
                    rift_recall_serve::HeldVerdict::ChangedAt(sites)
                }
                RecoveryVerdict::Unknown => rift_recall_serve::HeldVerdict::Unknown,
            },
            shape: match &self.verdict {
                RecoveryVerdict::ChangedAt { shape, .. } => *shape,
                RecoveryVerdict::Unchanged | RecoveryVerdict::Unknown => None,
            },
        }
    }
}

/// The entries a map would name, and the count of held files that did not
/// become one — or `None` when nothing this window held is servable.
///
/// The store and the read-only substrate are opened here, once for the whole
/// boundary, because this is the only step that needs them: a verdict is a
/// comparison against the revision the window read, and that revision is
/// reachable only through the two of them. Neither is available on the gate
/// path and neither is asked for there; this is a turn boundary, where a store
/// is already the norm. An unreachable one costs verdicts, never the map.
pub(super) fn recovery_map_entries(
    repo: &std::path::Path,
    engine: &rift_recall_policy::PolicyEngine<'_>,
    held: &[rift_agent::window_journal::HeldFile],
) -> Option<(Vec<RecoveryEntry>, usize)> {
    let rt = tokio::runtime::Builder::new_current_thread().build().ok()?;
    let basis = crate::family::recall_db(repo)
        .ok()
        .and_then(|db| rift_store::open_migrated(&db, &rift_store::recall_migrations()).ok())
        .zip(rift_recall_substrate::GixSubstrate::open(repo).ok());
    // The ladder's second rung: the revisions a read recorded, kept by the
    // detached worker. A window that read a file and then edited it holds a
    // revision no blob has, which is the case the blob route answers
    // `Unknown` for and this one answers exactly.
    let snapshots = basis_snapshots_at(repo);
    let (entries, omitted) = recovery_entries(
        held,
        &|path| engine.is_denied_path(path),
        &mut |path, hash| {
            recovery_verdict(repo, path, hash, basis.as_ref(), snapshots.as_ref(), &rt)
        },
    );
    if entries.is_empty() {
        // Every file this window held is one this repository does not serve.
        // A frame with a count and no facts in it is spend, so the honest
        // answer is the same silence a session with no held set gets.
        return None;
    }
    Some((entries, omitted))
}

/// The map's entries, in the order the renderer will name them, and the count
/// of held files that did not become one.
///
/// The guard and the entry cap are the only two ways a held file fails to
/// become an entry, and both are applied here — which is the whole point of
/// the split. Everything downstream, the renderer and the announcement alike,
/// speaks about positions in the list this returns, so there is no second
/// index space for a dropped file to shift.
fn recovery_entries(
    held: &[rift_agent::window_journal::HeldFile],
    denied: &dyn Fn(&str) -> bool,
    verdict: &mut dyn FnMut(&str, rift_core::Sha256Id) -> RecoveryVerdict,
) -> (Vec<RecoveryEntry>, usize) {
    let mut entries: Vec<RecoveryEntry> = Vec::new();
    let mut omitted = 0usize;
    for file in held {
        if denied(&file.path) {
            // The same filter every serve applies. A path this repository does
            // not serve is not served by a new renderer either, and the count
            // says something was left out.
            omitted = omitted.saturating_add(1);
            continue;
        }
        if entries.len() >= MAX_RECOVERY_ENTRIES {
            omitted = omitted.saturating_add(1);
            continue;
        }
        let answer = if entries.len() < MAX_RECOVERY_VERDICTS {
            verdict(&file.path, file.hash)
        } else {
            RecoveryVerdict::Unknown
        };
        let runs = file.held.runs().to_vec();
        entries.push(RecoveryEntry {
            path: file.path.clone(),
            hash: file.hash.to_hex(),
            held: runs.iter().map(|run| (run.lo, run.hi)).collect(),
            verdict: answer,
            revision: file.hash,
            runs,
        });
    }
    (entries, omitted)
}

/// What is true of one held file now: unchanged, changed in these places, or
/// nothing anyone can say.
///
/// The two answers are reached by different routes on purpose. **Unchanged** is
/// equality of the revision, which is exact and needs nothing but the bytes in
/// the checkout: the file hashes to what the session read, so what the session
/// held is what a re-read would return, and the re-read can be skipped
/// outright. **Changed at** is the diff module's answer against the revision
/// the session holds, fetched through the blob map — the same computation the
/// manifest and the pull verb give, so a reader is never handed two sets of
/// line numbers for one question.
///
/// Everything else is [`RecoveryVerdict::Unknown`], and that includes a file
/// that changed in more places than an entry may name: a partial list of
/// changed places would tell a reader that everything unnamed is unchanged,
/// which is the one thing this must never say.
fn recovery_verdict(
    repo: &std::path::Path,
    rel_path: &str,
    held: rift_core::Sha256Id,
    basis: Option<&(rift_store::SqliteStore, rift_recall_substrate::GixSubstrate)>,
    snapshots: Option<&rift_recall_serve::basis_store::BasisStore>,
    rt: &tokio::runtime::Runtime,
) -> RecoveryVerdict {
    let Some((blob, bytes)) = rift_recall_substrate::worktree::hash_blob_bytes(repo, rel_path)
    else {
        return RecoveryVerdict::Unknown;
    };
    let held = held.to_hex();
    if blob.content_sha256 == held {
        return RecoveryVerdict::Unchanged;
    }
    let Some((store, substrate)) = basis else {
        return RecoveryVerdict::Unknown;
    };
    let diff = rt.block_on(rift_recall_serve::held_diff::held_diff_against_basis(
        store, substrate, snapshots, &held, &bytes,
    ));
    let rift_recall_serve::held_diff::HeldDiff::Changed { ranges, .. } = diff else {
        return RecoveryVerdict::Unknown;
    };
    // The shape of what the file is now, from a valid outline of exactly
    // these bytes: the store is already open, and nothing is counted here.
    let shape = rt.block_on(rift_recall_serve::current_outline_shape(
        store,
        rel_path,
        &blob.content_sha256,
    ));
    RecoveryVerdict::ChangedAt {
        sites: ranges,
        shape,
    }
}

/// What the map announced, read out of the very list whose positions the
/// renderer handed back.
///
/// A position the renderer did not produce resolves to nothing rather than to
/// a neighbour: the failure direction is an announcement nobody makes.
pub(super) fn recovery_announcements<'a>(
    entries: &'a [RecoveryEntry],
    named: &'a [usize],
) -> impl Iterator<
    Item = (
        &'a str,
        rift_core::Sha256Id,
        &'a [rift_agent::window_journal::Interval],
    ),
> {
    named.iter().filter_map(|at| {
        let entry = entries.get(*at)?;
        Some((entry.path.as_str(), entry.revision, entry.runs.as_slice()))
    })
}

/// This member's view of the family's read-time basis snapshots, when the
/// family has an identity to look it up by.
///
/// `None` is not a failure: a checkout whose family cannot be resolved from
/// here has no shared store, and the basis ladder simply stops at the rung it
/// has always had. Opened where a store is already open — never on the gate
/// path, which is why it is a function the boundary and manifest turns call
/// and nothing a decision reaches.
pub(crate) fn basis_snapshots_at(
    work_tree: &std::path::Path,
) -> Option<rift_recall_serve::basis_store::BasisStore> {
    let family = crate::family::try_at(work_tree)?;
    Some(rift_recall_serve::basis_store::BasisStore::for_family(
        &family.common_dir,
    ))
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::{
        compacted, drop_family_store, evict, read_and_hold, recall_repo, CC, START,
    };
    use super::super::recovery_map;
    use super::*;

    /// An entry claims "unchanged" only when the bytes still hash to what the
    /// session read; a file that moved, or that is not there any more, is
    /// listed with its revision and its lines and **no claim at all**.
    ///
    /// The failure this forbids is the expensive one: a model told a changed
    /// file is unchanged skips the read that would have corrected it.
    #[test]
    fn an_entry_claims_unchanged_only_against_the_held_hash() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = recall_repo(tmp.path());
        let session = "s-verdicts";
        read_and_hold(&repo, session, "still.txt", "one\n", 10);
        read_and_hold(&repo, session, "moved.txt", "one\n", 20);
        read_and_hold(&repo, session, "gone.txt", "one\n", 30);
        std::fs::write(repo.join("moved.txt"), "one\ntwo\n").unwrap();
        std::fs::remove_file(repo.join("gone.txt")).unwrap();
        evict(&repo, session, 40);

        let cfg = rift_config::EffectiveConfig::load(&repo.join(".rift")).unwrap();
        let map = recovery_map(&repo, &cfg, &CC, START, &compacted(session)).expect("a map");
        let line = |path: &str| -> String {
            map.lines()
                .find(|line| line.contains(path))
                .unwrap_or_default()
                .to_owned()
        };
        assert!(
            line("still.txt").contains("unchanged since your read"),
            "{map}"
        );
        assert!(
            !line("moved.txt").contains("unchanged"),
            "the bytes moved: {map}"
        );
        assert!(
            !line("gone.txt").contains("unchanged"),
            "there is nothing to compare against: {map}"
        );
        assert!(
            line("moved.txt").contains("L1-1"),
            "what it held is still stated: {map}"
        );
        drop_family_store(&repo);
    }

    /// Run `git` inside the fixture repository, with the ambient configuration
    /// of whoever is running the suite kept out of it.
    fn git_in(repo: &std::path::Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(repo)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", repo.join("absent-gitconfig"))
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .output()
            .expect("git is on PATH");
        assert!(out.status.success(), "git {args:?}");
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    }

    /// A committed repository whose window read two files, one of which then
    /// moved, and which was then compacted.
    ///
    /// The changed-at label goes through the family store's blob map to the
    /// revision the session read and diffs it — the same computation the
    /// manifest and the pull verb use — so a real repository, a real commit
    /// and a real mapped blob are what this is built on rather than a stub.
    fn a_window_whose_file_moved() -> (tempfile::TempDir, std::path::PathBuf, &'static str) {
        a_window_whose_file_moved_from(
            "moved.txt",
            "alpha\nbravo\ncharlie\n",
            "alpha\nBRAVO\ncharlie\n",
        )
    }

    /// [`a_window_whose_file_moved`], for a file `name` read at `basis` that
    /// then became `moved`.
    fn a_window_whose_file_moved_from(
        name: &str,
        basis: &str,
        moved: &str,
    ) -> (tempfile::TempDir, std::path::PathBuf, &'static str) {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        git_in(&repo, &["init", "-q"]);
        git_in(&repo, &["config", "user.email", "t@example.invalid"]);
        git_in(&repo, &["config", "user.name", "t"]);
        std::fs::create_dir_all(repo.join(".rift")).unwrap();
        std::fs::write(
            repo.join(".rift").join("config.toml"),
            "schema_version = 1\n[project]\nname = \"t\"\n[features]\ntotal_recall = true\n",
        )
        .unwrap();
        std::fs::write(repo.join(name), basis).unwrap();
        std::fs::write(repo.join("still.txt"), "one\n").unwrap();
        git_in(&repo, &["add", "-A"]);
        git_in(&repo, &["commit", "-qm", "seed"]);

        let session = "s-labelled";
        read_and_hold(&repo, session, name, basis, 20);
        read_and_hold(&repo, session, "still.txt", "one\n", 10);

        // The route to the revision a window holds is the blob map the
        // projection writes at ingest; this fixture writes the one row that
        // walk would have written, so the diff has a basis to reach.
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let db = crate::family::recall_db(&repo).unwrap();
        let store = rift_store::open_migrated(&db, &rift_store::recall_migrations()).unwrap();
        rt.block_on(rift_recall_ledger::blob_map::persist_blob_oid_map(
            &store,
            &[rift_recall_substrate::worktree::WorktreeBlob {
                path: name.to_owned(),
                content_sha256: rift_shadow::sha256_hex(&[basis.as_bytes()]),
                blob_oid: git_in(&repo, &["rev-parse", &format!("HEAD:{name}")]),
            }],
            0,
        ))
        .unwrap();
        drop(store);

        // The file moves after the session read it, and then the context is
        // compacted.
        std::fs::write(repo.join(name), moved).unwrap();
        evict(&repo, session, 30);
        (tmp, repo, session)
    }

    /// **Each entry is labelled against the revision the session holds**: the
    /// file that still hashes to it says so, and the file that moved says
    /// *where* it moved, in the numbering a reader is about to see.
    #[test]
    fn entries_are_labelled_unchanged_or_changed_against_the_held_hash() {
        let (_tmp, repo, session) = a_window_whose_file_moved();
        let cfg = rift_config::EffectiveConfig::load(&repo.join(".rift")).unwrap();
        let map = recovery_map(&repo, &cfg, &CC, START, &compacted(session)).expect("a map");
        let line = |path: &str| -> String {
            map.lines()
                .find(|line| line.contains(path))
                .unwrap_or_default()
                .to_owned()
        };
        assert!(
            line("moved.txt").contains("changed at lines 2-2"),
            "the place it moved, in current numbering: {map}"
        );
        assert!(
            line("still.txt").contains("unchanged since your read"),
            "{map}"
        );
        assert!(
            !line("moved.txt").contains("unchanged"),
            "one file, one claim: {map}"
        );
        drop_family_store(&repo);
    }

    /// **A changed file with a valid outline of what it is now is followed by
    /// its shape**, read from the store the map already opened; a file with
    /// no such outline, or one that did not change, gets no shape line.
    #[test]
    fn a_changed_file_with_a_bound_outline_of_its_current_bytes_is_followed_by_its_shape() {
        let moved = "fn alpha() {}\nfn bravo_two() {}\nfn charlie() {}\n";
        let (_tmp, repo, session) = a_window_whose_file_moved_from(
            "moved.rs",
            "fn alpha() {}\nfn bravo() {}\nfn charlie() {}\n",
            moved,
        );
        bind_outline(&repo, "moved.rs", moved);
        let cfg = rift_config::EffectiveConfig::load(&repo.join(".rift")).unwrap();
        let map = recovery_map(&repo, &cfg, &CC, START, &compacted(session)).expect("a map");
        let lines: Vec<&str> = map.lines().collect();
        let at = lines
            .iter()
            .position(|line| line.contains("moved.rs"))
            .expect("the moved file is listed");
        assert!(lines[at].contains("changed at lines 2-2"), "{map}");
        assert_eq!(
            lines[at + 1],
            format!(
                "  shape of the current revision: 3 lines, {} bytes, 3 top-level definitions",
                moved.len()
            ),
            "{map}"
        );
        assert_eq!(
            map.matches("shape of the current revision").count(),
            1,
            "{map}"
        );
        drop_family_store(&repo);
    }

    /// Bind the outline of `text` at `rel` in the family's store, as the
    /// detached worker will.
    fn bind_outline(repo: &std::path::Path, rel: &str, text: &str) {
        let source =
            rift_shadow::redact::redact(&rift_core::SecretBytes::new(text.as_bytes().to_vec()))
                .unwrap()
                .redacted;
        let counts = rift_recall_ledger::validity_store::OutlineCounts::measure(
            rel,
            &source,
            &|t: &str| u32::try_from(t.len()).unwrap(),
            "cl100k_base",
        )
        .unwrap();
        let file = rift_recall_ledger::validity_store::OutlineSource {
            path: rel,
            source: &source,
            policy_version: rift_recall_policy::SERVE_POLICY_VERSION,
            refuses_path: &|_| false,
        };
        let db = crate::family::recall_db(repo).unwrap();
        let store = rift_store::open_migrated(&db, &rift_store::recall_migrations()).unwrap();
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let bound = rt.block_on(rift_recall_ledger::validity_store::put_file_outline(
            &store,
            &rift_store::PlainCipher,
            &file,
            &counts,
        ));
        assert!(bound.unwrap().is_some());
    }

    /// ★ **Every position the renderer hands back names its own entry.** Over
    /// random held sets and random guard masks: whatever the guard drops, an
    /// announcement is the path, the revision and the runs of the entry at
    /// that position in the rendered list — never of the held file that
    /// happened to sit there.
    #[test]
    fn an_announcement_names_the_entry_the_renderer_named() {
        use proptest::prelude::*;

        proptest!(|(mask in proptest::collection::vec(any::<bool>(), 1..12))| {
            let held: Vec<rift_agent::window_journal::HeldFile> = mask
                .iter()
                .enumerate()
                .map(|(at, denied)| {
                    let path = if *denied {
                        format!("private/{at}.txt")
                    } else {
                        format!("src/{at}.rs")
                    };
                    let mut set = rift_agent::window_journal::IntervalSet::new();
                    set.insert(
                        rift_agent::window_journal::Interval::new(1, u32::try_from(at + 1).unwrap())
                            .unwrap(),
                    );
                    rift_agent::window_journal::HeldFile {
                        path,
                        hash: rift_core::Sha256Id::parse_hex(&rift_shadow::sha256_hex(&[
                            format!("{at}").as_bytes(),
                        ]))
                        .unwrap(),
                        held: set,
                        last_read_at_ms: 10,
                    }
                })
                .collect();
            let (entries, omitted) = recovery_entries(
                &held,
                &|path| path.starts_with("private/"),
                &mut |_, _| RecoveryVerdict::Unknown,
            );
            prop_assert_eq!(entries.len() + omitted, held.len());
            for entry in &entries {
                prop_assert!(!entry.path.starts_with("private/"), "{}", entry.path);
            }
            // Every position, in every order, including ones no renderer would
            // produce: each resolves to its own entry or to nothing.
            let named: Vec<usize> = (0..held.len() + 2).rev().collect();
            let announced: Vec<(&str, rift_core::Sha256Id, &[rift_agent::window_journal::Interval])> =
                recovery_announcements(&entries, &named).collect();
            prop_assert_eq!(announced.len(), entries.len());
            for (path, hash, runs) in announced {
                let entry = entries
                    .iter()
                    .find(|entry| entry.path == path)
                    .expect("an announcement names an entry of the rendered list");
                prop_assert_eq!(hash, entry.revision);
                prop_assert_eq!(runs, entry.runs.as_slice());
                let file = held
                    .iter()
                    .find(|file| file.path == path)
                    .expect("and that entry is a file the window held");
                prop_assert_eq!(hash, file.hash);
                prop_assert_eq!(runs, file.held.runs());
            }
        });
    }
}
