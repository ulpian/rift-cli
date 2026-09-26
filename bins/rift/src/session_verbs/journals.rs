//! The family-wide journal reader: every member of a repository family and
//! the sessions its gitdir is credited with.
//!
//! Session caches stay per gitdir by design, so a family-wide question has to
//! be asked of every member — and a session is credited once, to the member
//! it names.

use std::path::{Path, PathBuf};

/// Every session the journal knows in one worktree — including `ended` ones,
/// which `live_sessions` deliberately filters and which are exactly what a
/// picker must show (a stopped session is the thing you resume).
pub(super) fn journal_sessions(
    git_dir: &Path,
) -> Vec<rift_shadow::hooks::session_cache::SessionCache> {
    use rift_shadow::hooks::session_cache::read_cache;
    let dir = git_dir.join("rift-sessions");
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let name = entry.file_name();
        let Some(stem) = name
            .to_string_lossy()
            .strip_suffix(".json")
            .map(str::to_owned)
        else {
            continue;
        };
        if let Ok(Some(cache)) = read_cache(git_dir, &stem) {
            out.push(cache);
        }
    }
    out
}

/// One family member and the sessions its journal is credited with.
pub(crate) struct MemberJournal {
    /// The member: the main work tree first, then each linked worktree by
    /// administrative name.
    pub(crate) member: rift_git::FamilyMember,
    /// The caches credited to this member, deduplicated family-wide.
    pub(crate) sessions: Vec<rift_shadow::hooks::session_cache::SessionCache>,
}

/// Every member of the family `paths` belongs to, with the sessions its
/// journal holds — the one enumeration `session list`, the resume picker and
/// `rift doctor`'s session sweep share.
///
/// **Main is always in it.** Session caches stay per gitdir by design — they
/// are a session's private working state, scoped to the checkout that produced
/// it — so a family-wide question has to be asked of every member's
/// gitdir. The readers here asked `repo.work_tree` + `list_worktrees`, which
/// from a linked member enumerates that member and its siblings and omits the
/// main work tree — the member that holds nearly every session in a real
/// repository. `family_members` proves main rather than assuming it
/// (never the lexical `common_dir.parent()`), so it is in the list
/// from any vantage.
///
/// **A session is credited once, to the member it names.** A cache
/// carries the canonical work tree it was observed in; where it names a member
/// of this family, that member gets it, whatever registry the file happens to
/// sit in — a session driven in worktree A is never credited to worktree B. A
/// cache with no claim (every file written before the claim existed) belongs to the
/// registry it was found in, which is where it has always been usable.
///
/// **Total, and never fewer members than the caller can see.** A family whose
/// members cannot be enumerated at all — an unreadable common dir, an
/// administrative entry with no `gitdir` file — still yields the vantage's own
/// journal rather than an empty list, because "this checkout has no sessions"
/// and "the family could not be read" are different answers and only one of
/// them is true.
pub(crate) fn family_journal(paths: &rift_git::RepoPaths) -> Vec<MemberJournal> {
    let members = with_vantage(paths, rift_git::family_members(paths));
    let mut out: Vec<MemberJournal> = members
        .iter()
        .cloned()
        .map(|member| MemberJournal {
            member,
            sessions: Vec::new(),
        })
        .collect();
    let mut seen = std::collections::BTreeSet::new();
    for (index, m) in members.iter().enumerate() {
        for s in journal_sessions(&m.git_dir) {
            if !seen.insert(s.session_id.clone()) {
                continue; // one credit per session, family-wide
            }
            let owner = claimed_member(&members, s.work_tree.as_deref()).unwrap_or(index);
            if let Some(journal) = out.get_mut(owner) {
                journal.sessions.push(s);
            }
        }
    }
    out
}

/// `members`, with the vantage itself prepended when the enumeration did not
/// find it. Cheap insurance for a family that cannot be read: the reader must
/// never return less than the single-checkout reader it replaced.
fn with_vantage(
    paths: &rift_git::RepoPaths,
    mut members: Vec<rift_git::FamilyMember>,
) -> Vec<rift_git::FamilyMember> {
    let here = canonical_or_lexical(&paths.git_dir);
    if members
        .iter()
        .any(|m| canonical_or_lexical(&m.git_dir) == here)
    {
        return members;
    }
    members.insert(
        0,
        rift_git::FamilyMember {
            name: paths.work_tree.file_name().map_or_else(
                || paths.work_tree.to_string_lossy().into_owned(),
                |n| n.to_string_lossy().into_owned(),
            ),
            work_tree: paths.work_tree.clone(),
            git_dir: paths.git_dir.clone(),
            is_main: paths.git_dir == paths.common_dir,
            prunable: false,
        },
    );
    members
}

/// The index of the member a cache's `work_tree` claims, or `None` when it
/// claims nothing (files written before the claim existed) or names a tree
/// outside this family.
fn claimed_member(members: &[rift_git::FamilyMember], claim: Option<&str>) -> Option<usize> {
    let claimed = canonical_or_lexical(Path::new(claim?));
    members
        .iter()
        .position(|m| canonical_or_lexical(&m.work_tree) == claimed)
}

/// The canonical path where it resolves, the given one where it does not —
/// both sides of every comparison here go through it, because a claim written
/// through a symlinked path still has to match the tree it names, and a gone
/// checkout still has to be nameable.
fn canonical_or_lexical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Plant a session cache file directly under `git_dir` — the shape the
    /// hook writes, and (with an explicit `work_tree`) the shape only a
    /// hand-written file has, which is exactly what the credit discriminator
    /// exists to adjudicate.
    fn plant(git_dir: &Path, id: &str, state: &str, work_tree: Option<&Path>, updated_at_ms: i64) {
        let dir = git_dir.join("rift-sessions");
        std::fs::create_dir_all(&dir).unwrap();
        let cache = rift_shadow::hooks::session_cache::SessionCache {
            schema: rift_shadow::hooks::session_cache::SCHEMA,
            session_id: id.to_owned(),
            state: state.to_owned(),
            agent: "claude-code".to_owned(),
            current_turn_ordinal: 1,
            turn_open: false,
            compaction_boundary: None,
            updated_at_ms,
            native_id_map: std::collections::BTreeMap::new(),
            transcript_path: None,
            work_tree: work_tree.map(|w| w.display().to_string()),
            // A planted cache has observed no worktree: the memo is UNKNOWN,
            // which is what a cache file written before worktree provenance
            // existed reads as too.
            last_worktree: None,
            native_turn: None,
        };
        std::fs::write(
            dir.join(format!("{id}.json")),
            serde_json::to_vec(&cache).unwrap(),
        )
        .unwrap();
    }

    /// A family, hermetically: a main work tree and one linked member, in the
    /// exact file shapes `git worktree add` writes.
    fn family(root: &Path) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
        let main = root.join("main");
        std::fs::create_dir_all(&main).unwrap();
        let main_git = rift_git::test_support::fake_repo(&main).unwrap();
        let member = root.join("member");
        let admin = rift_git::test_support::fake_worktree(&main_git, &member, "member").unwrap();
        (main, main_git, member, admin)
    }

    /// **From a member, the main work tree's journal is in the list.** The
    /// readers enumerated `repo.work_tree` + `list_worktrees`, which from a
    /// linked worktree is that worktree and its siblings — never main, where
    /// nearly every session of a real repository lives.
    #[test]
    fn the_family_journal_reads_every_member_main_included() {
        let tmp = tempfile::tempdir().unwrap();
        let (_main, main_git, member, admin) = family(tmp.path());
        plant(&main_git, "on-main", "ended", None, 10);
        plant(&admin, "on-member", "ended", None, 20);

        // The vantage is the MEMBER: the case that was broken.
        let paths = rift_git::discover(&member).unwrap();
        let journal = family_journal(&paths);

        assert_eq!(journal.len(), 2, "main and the member");
        assert!(journal[0].member.is_main, "main is first");
        assert_eq!(
            journal[0]
                .sessions
                .iter()
                .map(|s| s.session_id.as_str())
                .collect::<Vec<_>>(),
            vec!["on-main"],
            "main's own journal, read from the member"
        );
        assert_eq!(
            journal[1]
                .sessions
                .iter()
                .map(|s| s.session_id.as_str())
                .collect::<Vec<_>>(),
            vec!["on-member"]
        );
    }

    /// **The cache's own claim decides the credit, not the directory.** A cache sitting in
    /// the shared registry that NAMES a member is that member's; one that
    /// names nothing belongs to the registry it was found in. Neither is ever
    /// credited to two members, and a session driven in one worktree is never
    /// attributed to another.
    #[test]
    fn a_cache_is_credited_to_the_member_it_names_not_to_the_registry_it_sits_in() {
        let tmp = tempfile::tempdir().unwrap();
        let (main, main_git, member, _admin) = family(tmp.path());
        plant(&main_git, "claims-the-member", "ended", Some(&member), 30);
        plant(&main_git, "claims-nothing", "ended", None, 20);
        plant(
            &main_git,
            "claims-elsewhere",
            "ended",
            Some(Path::new("/nowhere")),
            10,
        );

        let paths = rift_git::discover(&main).unwrap();
        let journal = family_journal(&paths);

        let ids = |i: usize| {
            journal[i]
                .sessions
                .iter()
                .map(|s| s.session_id.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            ids(0),
            vec!["claims-elsewhere".to_owned(), "claims-nothing".to_owned()],
            "a claim on no member of this family falls back to the registry's owner"
        );
        assert_eq!(ids(1), vec!["claims-the-member".to_owned()]);
    }

    /// A family the enumeration cannot see all of still answers for the
    /// vantage. The member's administrative `gitdir` file is unreadable, so
    /// `family_members` skips the entry (a row without a path would be an
    /// invented one) — the reader must still be that member's own journal
    /// plus what it can enumerate, never an empty list.
    #[test]
    fn a_family_that_cannot_be_enumerated_still_reads_the_vantages_own_journal() {
        let tmp = tempfile::tempdir().unwrap();
        let (_main, main_git, member, admin) = family(tmp.path());
        plant(&main_git, "on-main", "ended", None, 10);
        plant(&admin, "on-member", "ended", None, 20);
        std::fs::remove_file(admin.join("gitdir")).unwrap();

        let paths = rift_git::discover(&member).unwrap();
        let journal = family_journal(&paths);

        assert_eq!(journal.len(), 2, "the vantage, and the main it could prove");
        assert!(!journal[0].member.is_main, "the vantage is prepended");
        assert_eq!(journal[0].member.git_dir, paths.git_dir);
        assert_eq!(
            journal[0]
                .sessions
                .iter()
                .map(|s| s.session_id.as_str())
                .collect::<Vec<_>>(),
            vec!["on-member"]
        );
        assert_eq!(journal[1].sessions.len(), 1, "and main is still read");
    }
}
