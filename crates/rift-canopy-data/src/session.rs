//! The live-session signal: a **local, lenient, total** reader over
//! the capture hooks' per-session cache, `<sessions>/*.json`, where
//! `<sessions>` is the member's sessions directory in the family's run state
//! (`member_sessions` resolves it, and reads the git dir's old tree only for
//! a member not moved yet).
//!
//! The writer is `rift-shadow-hooks` (`session_cache.rs`, SCHEMA 1: `schema`,
//! `session_id`, `state`, `agent`, `current_turn_ordinal`, `turn_open`,
//! `compaction_boundary`, `updated_at_ms`). The live read **mirrors** the
//! contract locally rather than parsing the writer's type, so it can be
//! lenient where the writer is strict: unknown fields are
//! ignored, unknown states degrade to "not live", and only the two fields the
//! signal needs (`state`, `updated_at_ms`) are required. Parsing is **total**:
//! corrupt bytes yield [`SessionsRead::Corrupt`] (a distinct error status),
//! never a panic and never a silent "quiet"; files over `MAX_CACHE_BYTES`
//! are treated as corrupt rather than parsed.
//!
//! [`sessions_detail`] is the single active-session cache pass. It returns the
//! badge signal plus every live native identity so the atomic System feed can
//! resolve scope without selecting a misleading fallback.
//!
//! # The family read and the worktree discriminator
//!
//! Session caches stay **per member** by design (family identity: they are a
//! session's private working state), so a question about a repository FAMILY —
//! "is anything running anywhere in it?" — has to be asked of every member's
//! sessions directory, which is [`family_sessions`]. That read must credit each session
//! **once**, or the family row would double-count every cache that a member and
//! the shared registry can both see, and a member row could be credited with a
//! session driven in a sibling.
//!
//! The rule is the writer's own discriminator: a cache carries the canonical
//! work tree it was observed in, and where that names a member of this family
//! **that** member gets it, whatever registry the file happens to sit in. A
//! cache with no claim (every file written before the discriminator existed) belongs to the
//! registry it was found in, which is where it has always been usable. It is
//! the rule `bins/rift`'s `family_journal` applies to the same files; the two
//! are separate spellings because `bins/rift` is a binary this crate cannot
//! import. `family_journal` is the one reader here that parses the writer's
//! own type, through `rift-shadow-hooks`, because the integration-health
//! detectors take that type.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use rift_canopy::{LiveSessions, SessionsRead};
use rift_git::{FamilyMember, FamilyState, MintPolicy};
use rift_shadow_hooks::run_state::{self, RunRoot};
use rift_shadow_hooks::session_cache::{read_cache, work_tree_of, SessionCache};

/// A cache file larger than this is treated as corrupt (the writer emits
/// hundreds of bytes; anything huge is not ours).
const MAX_CACHE_BYTES: u64 = 64 * 1024;

/// The staleness window: a session whose cache has not been touched for this
/// long is treated as crash-stale and ignored (the cache is explicitly a
/// cache — a crash leaves the file behind, and the hooks reconcile on the
/// next event). Thirty minutes keeps a genuinely idle-but-open agent session
/// visible (hooks refresh the file on every event) while bounding how long a
/// crashed one can masquerade as live. The window was chosen deliberately and
/// revisited against real sessions before it shipped.
pub const ACTIVE_WINDOW_MS: i64 = 30 * 60 * 1000;

/// The cache states that count as live — mirrored from the writer's
/// `live_sessions` filter.
const LIVE_STATES: &[&str] = &["active", "compacting", "ending"];

/// The one-pass detail read over the session caches: the badge signal
/// ([`SessionsRead`], unchanged contract) plus the id of the **newest live**
/// session — greatest `updated_at_ms`, ties broken toward the greater id so
/// the pick is deterministic. The id anchors the session-scoped panel
/// metrics; the badge also feeds the status truth table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionsDetail {
    /// The active-sessions badge signal.
    pub read: SessionsRead,
    /// The newest live session's id. `None` when nothing is live, on a
    /// corrupt read, or when the winning cache carries no id (leniency: an
    /// id-less cache still counts toward the badge but cannot anchor).
    pub newest_live: Option<String>,
    /// Every live cache that carries both identity halves, sorted
    /// deterministically by update time then identity.
    pub identities: Vec<LiveSessionIdentity>,
    /// Live caches that count toward the badge but cannot be resolved because
    /// either the native session id or adapter name is absent.
    pub unresolved: u16,
}

/// Identity facts retained from one live top-level capture-session cache.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct LiveSessionIdentity {
    /// Adapter name (`claude-code`, ...).
    pub agent: String,
    /// Adapter-native session id.
    pub native_session_id: String,
    /// Cache update time in epoch milliseconds.
    pub updated_at_ms: i64,
}

/// **The** single active-sessions query: distinct live sessions for the repo
/// whose git dir is `git_dir`, judged at `now_ms` (injected — no wall clock).
///
/// A session counts iff its cache file parses, its `state` is live
/// (`active`/`compacting`/`ending`), and its `updated_at_ms` is within
/// [`ACTIVE_WINDOW_MS`] of `now_ms`. A missing sessions directory is
/// `Ok(None)`; any unparseable cache file is [`SessionsRead::Corrupt`] (the
/// distinct error status — never silently "quiet"). `since_ms` is the
/// earliest live session's last update, so "running since" reflects the
/// oldest current activity.
#[must_use]
pub fn active_sessions(git_dir: &Path, now_ms: i64) -> SessionsRead {
    sessions_detail(git_dir, now_ms).read
}

/// The same single pass as [`active_sessions`], also surfacing the newest
/// live session's id (see [`SessionsDetail`]). One `read_dir` walk serves
/// both consumers, so the badge and the metrics anchor can never disagree
/// about which sessions are live.
#[must_use]
pub fn sessions_detail(git_dir: &Path, now_ms: i64) -> SessionsDetail {
    let sessions = match work_tree_of(git_dir) {
        Some(work_tree) => member_sessions(git_dir, &work_tree),
        None => RunRoot::Sessions.legacy_under(git_dir),
    };
    match read_live_caches(&sessions, now_ms) {
        DirRead::Corrupt => CORRUPT_DETAIL,
        DirRead::Ok(caches) => finish(&caches),
    }
}

/// The sessions directory of the member checked out at `work_tree`, whose
/// git dir is `git_dir`: in its family's run state under this machine's cache
/// root once the member was moved there, in the git dir before.
///
/// One tree per member, never both: this reader moves nothing, so until the
/// `rift` binary has moved a member, the git dir is where its hooks write.
/// No recorded family (outside a repository, or before `rift init`) leaves
/// the git dir as the member's only tree.
pub(crate) fn member_sessions(git_dir: &Path, work_tree: &Path) -> PathBuf {
    let cache_root = rift_store::resolve_cache_root(|key| std::env::var(key).ok());
    member_sessions_under(&cache_root, git_dir, work_tree)
}

/// [`member_sessions`] under an explicit cache root.
fn member_sessions_under(cache_root: &Path, git_dir: &Path, work_tree: &Path) -> PathBuf {
    let Ok((_, FamilyState::Minted(family))) = rift_git::family_of(work_tree, MintPolicy::Never)
    else {
        return RunRoot::Sessions.legacy_under(git_dir);
    };
    let family_dir = rift_store::projection_dir_under(cache_root, &family.family_id);
    let member = run_state::member_dir(&family_dir, work_tree);
    run_state::dir_for_read(RunRoot::Sessions, &member, git_dir)
}

/// A family's members with the vantage's own place in the list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FamilyRoster {
    /// The main work tree first, then each linked worktree by administrative
    /// name — with the vantage itself guaranteed present.
    pub members: Vec<FamilyMember>,
    /// The index of the vantage in [`Self::members`]; always in range.
    pub vantage: usize,
}

/// Every member of the family `paths` belongs to, and where `paths` itself sits
/// in that list.
///
/// **Never fewer members than the caller can see.** A family whose members
/// cannot be enumerated at all — an unreadable common dir, an administrative
/// entry with no `gitdir` file — still yields the vantage's own row rather than
/// an empty list, because "this checkout has no sessions" and "the family could
/// not be read" are different answers and only one of them is true. The same
/// insurance `bins/rift`'s `family_journal` carries, for the same reason.
///
/// The self-comparison canonicalises both sides: `discover` keeps the spelling
/// its upward walk found, `family_members` canonicalises, and a checkout
/// reached through a symlink therefore names one directory in two ways — the
/// defect that made a vantage report its own queue as a sibling's.
#[must_use]
pub fn roster(paths: &rift_git::RepoPaths) -> FamilyRoster {
    let members = rift_git::family_members(paths);
    let here = canonical_or_lexical(&paths.git_dir);
    if let Some(vantage) = members
        .iter()
        .position(|m| canonical_or_lexical(&m.git_dir) == here)
    {
        return FamilyRoster { members, vantage };
    }
    let mut members = members;
    members.insert(
        0,
        FamilyMember {
            name: paths.member_name(),
            work_tree: paths.work_tree.clone(),
            git_dir: paths.git_dir.clone(),
            is_main: !paths.is_linked(),
            prunable: false,
        },
    );
    FamilyRoster {
        members,
        vantage: 0,
    }
}

/// The live sessions of a whole repository family, credited one per member.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FamilySessions {
    /// One read per member, in the order of the `members` slice given.
    pub per_member: Vec<SessionsDetail>,
    /// Every member's live sessions, each counted exactly once — the family
    /// row's aggregate (the family view).
    pub family: SessionsDetail,
}

/// Read every member's session registry once and credit each live session to
/// exactly one member (the discriminator, the module doc's rule), yielding both the
/// per-member reads and the family aggregate.
///
/// **Corruption belongs to the registry it was found in.** A file that cannot
/// be parsed carries no claim by definition, so it is credited exactly like a
/// claimless one: to the member whose directory holds it. That member reads
/// [`SessionsRead::Corrupt`], and so does the family — "a live session I cannot
/// count" must never render as "quiet" at either scope, and the whole point of
/// the distinct error status is that it is loud.
///
/// Deterministic and total: an unreadable member yields `Corrupt`, a member
/// with no registry yields `Ok(None)`, and no input can panic.
#[must_use]
pub fn family_sessions(members: &[FamilyMember], now_ms: i64) -> FamilySessions {
    let mut credited: Vec<Vec<LiveCache>> = vec![Vec::new(); members.len()];
    let mut corrupt = vec![false; members.len()];
    // One credit per session, family-wide: a cache visible from two registries
    // is one session, and the first member to reach it decides nothing — the
    // claim does.
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for (index, member) in members.iter().enumerate() {
        match read_live_caches(&member_sessions(&member.git_dir, &member.work_tree), now_ms) {
            DirRead::Corrupt => {
                if let Some(flag) = corrupt.get_mut(index) {
                    *flag = true;
                }
            }
            DirRead::Ok(caches) => {
                for cache in caches {
                    if !seen.insert(cache.key.clone()) {
                        continue;
                    }
                    let owner = claimed_member(members, cache.claim.as_deref()).unwrap_or(index);
                    if let Some(bucket) = credited.get_mut(owner) {
                        bucket.push(cache);
                    }
                }
            }
        }
    }
    let per_member: Vec<SessionsDetail> = credited
        .iter()
        .zip(&corrupt)
        .map(|(caches, &bad)| if bad { CORRUPT_DETAIL } else { finish(caches) })
        .collect();
    let family = if corrupt.iter().any(|&bad| bad) {
        CORRUPT_DETAIL
    } else {
        let all: Vec<LiveCache> = credited.into_iter().flatten().collect();
        finish(&all)
    };
    FamilySessions { per_member, family }
}

/// Every session cache the family's members hold, each session once: the
/// journal the health detectors read, live or not.
///
/// The same files `bins/rift`'s `family_journal` reads for the command line,
/// through the writer's own reader: unlike the live read above, which mirrors
/// the two fields it needs, this returns the writer's type, because the
/// integration-health detectors take it.
///
/// An unreadable cache is skipped rather than failing the read: the live
/// session reader above already reports it as a corrupt cache, which outranks
/// every finding this feeds.
pub(crate) fn family_journal(members: &[FamilyMember]) -> Vec<SessionCache> {
    let mut seen = BTreeSet::new();
    let mut sessions = Vec::new();
    for member in members {
        let dir = member_sessions(&member.git_dir, &member.work_tree);
        for path in run_state::list_entries(&dir) {
            let Some(name) = path.file_name() else {
                continue;
            };
            let Some(stem) = name
                .to_string_lossy()
                .strip_suffix(".json")
                .map(str::to_owned)
            else {
                continue;
            };
            let Ok(Some(cache)) = read_cache(&dir, &stem) else {
                continue;
            };
            if seen.insert(cache.session_id.clone()) {
                sessions.push(cache);
            }
        }
    }
    sessions
}

/// The index of the member a cache's claim names, or `None` when it claims
/// nothing (every file written before the discriminator existed) or names a tree outside this
/// family — in both cases the cache stays with the registry it was found in.
fn claimed_member(members: &[FamilyMember], claim: Option<&str>) -> Option<usize> {
    let claimed = canonical_or_lexical(Path::new(claim?));
    members
        .iter()
        .position(|m| canonical_or_lexical(&m.work_tree) == claimed)
}

/// The canonical path where it resolves, the given one where it does not.
/// Both sides of every comparison here go through it: a claim written through
/// a symlinked path, or before the checkout moved, still has to match the tree
/// it names, and a `prunable` member's gone checkout still has to be nameable.
/// One rule, kept in `rift-git` beside the family plane that states it.
use rift_git::family::canonical_or_lexical;

/// The reading of ONE registry directory: its live caches, or the fact that the
/// directory could not be read as one.
enum DirRead {
    Ok(Vec<LiveCache>),
    Corrupt,
}

/// One live, fresh cache file, retained for crediting.
#[derive(Clone)]
struct LiveCache {
    /// The dedup key, family-wide: the session id where the cache carries one,
    /// else the file's own path. An id-less cache cannot be recognised in a
    /// second registry, so it is keyed by something that can never collide
    /// rather than silently deduplicated against a stranger.
    key: String,
    updated_at_ms: i64,
    /// Adapter name; `None` under the lenient contract.
    agent: Option<String>,
    /// Adapter-native session id; `None` under the lenient contract.
    session_id: Option<String>,
    /// The worktree discriminator: the work tree this session claims.
    claim: Option<String>,
}

/// The detail a corrupt read yields at every scope — a distinct error, never a
/// silent "quiet".
const CORRUPT_DETAIL: SessionsDetail = SessionsDetail {
    read: SessionsRead::Corrupt,
    newest_live: None,
    identities: Vec::new(),
    unresolved: 0,
};

/// Walk one registry directory and keep every live, fresh cache.
///
/// A missing directory is an empty read; a directory that exists but cannot be
/// listed, a file that cannot be read, one over [`MAX_CACHE_BYTES`] and one
/// whose bytes are not a cache at all are all [`DirRead::Corrupt`].
fn read_live_caches(sessions: &Path, now_ms: i64) -> DirRead {
    let Ok(paths) = run_state::list_checked(sessions) else {
        return DirRead::Corrupt;
    };
    let mut out = Vec::new();
    // The dual read: an unmoved member's caches are read from the old tree
    // until the new tree has its own.
    for path in paths {
        if path.extension().is_none_or(|e| e != "json") {
            continue; // tmp files / strays are not cache entries
        }
        if std::fs::metadata(&path).is_ok_and(|m| m.len() > MAX_CACHE_BYTES) {
            return DirRead::Corrupt; // size cap: not plausibly ours
        }
        let Ok(bytes) = std::fs::read(&path) else {
            return DirRead::Corrupt; // exists but unreadable
        };
        let Some(cache) = parse_cache(&bytes) else {
            return DirRead::Corrupt;
        };
        let fresh = now_ms.saturating_sub(cache.updated_at_ms) < ACTIVE_WINDOW_MS
            && cache.updated_at_ms <= now_ms.saturating_add(ACTIVE_WINDOW_MS);
        if !(cache.live && fresh) {
            continue;
        }
        let key = cache
            .session_id
            .clone()
            .unwrap_or_else(|| path.to_string_lossy().into_owned());
        out.push(LiveCache {
            key,
            updated_at_ms: cache.updated_at_ms,
            agent: cache.agent,
            session_id: cache.session_id,
            claim: cache.work_tree,
        });
    }
    DirRead::Ok(out)
}

/// Fold a set of live caches into the badge signal, the metrics anchor and the
/// resolved identities. The one place those three are derived, so a per-member
/// read and the family aggregate can never disagree about how they are counted.
fn finish(caches: &[LiveCache]) -> SessionsDetail {
    if caches.is_empty() {
        return SessionsDetail {
            read: SessionsRead::Ok(None),
            newest_live: None,
            identities: Vec::new(),
            unresolved: 0,
        };
    }
    let mut live: u16 = 0;
    let mut since_ms = i64::MAX;
    let mut newest: Option<(i64, String)> = None;
    let mut identities = Vec::new();
    let mut unresolved = 0u16;
    for cache in caches {
        live = live.saturating_add(1);
        since_ms = since_ms.min(cache.updated_at_ms);
        if let (Some(id), Some(agent)) = (cache.session_id.clone(), cache.agent.clone()) {
            let candidate = (cache.updated_at_ms, id.clone());
            if newest.as_ref().is_none_or(|best| candidate > *best) {
                newest = Some(candidate);
            }
            identities.push(LiveSessionIdentity {
                agent,
                native_session_id: id,
                updated_at_ms: cache.updated_at_ms,
            });
        } else {
            unresolved = unresolved.saturating_add(1);
        }
    }
    identities.sort_by(|a, b| {
        (a.updated_at_ms, &a.agent, &a.native_session_id).cmp(&(
            b.updated_at_ms,
            &b.agent,
            &b.native_session_id,
        ))
    });
    SessionsDetail {
        read: SessionsRead::Ok(Some(LiveSessions {
            active_sessions: live,
            since_ms,
        })),
        newest_live: newest.map(|(_, id)| id),
        identities,
        unresolved,
    }
}

/// The facts the signal needs from one cache file.
struct CacheFacts {
    live: bool,
    updated_at_ms: i64,
    /// The cache's `session_id`, when present — the metrics anchor. Optional
    /// by leniency: only `state` + `updated_at_ms` are *required*.
    session_id: Option<String>,
    /// Adapter name, optional under the lenient cache contract.
    agent: Option<String>,
    /// The working tree this session claims — the worktree discriminator.
    /// `None` means UNKNOWN (every cache written before the discriminator), and
    /// unknown is never a claim on anybody.
    work_tree: Option<String>,
}

/// Parse one cache file's bytes, leniently and totally: unknown fields are
/// ignored, an unknown `state` reads as not-live, and `None` means the bytes
/// are not a cache file at all (corrupt). Never panics on any input.
fn parse_cache(bytes: &[u8]) -> Option<CacheFacts> {
    let value: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    let obj = value.as_object()?;
    let state = obj.get("state")?.as_str()?;
    let updated_at_ms = obj.get("updated_at_ms")?.as_i64()?;
    let session_id = obj
        .get("session_id")
        .and_then(|v| v.as_str())
        .map(str::to_owned);
    let agent = obj.get("agent").and_then(|v| v.as_str()).map(str::to_owned);
    let work_tree = obj
        .get("work_tree")
        .and_then(|v| v.as_str())
        .map(str::to_owned);
    Some(CacheFacts {
        live: LIVE_STATES.contains(&state),
        updated_at_ms,
        session_id,
        agent,
        work_tree,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_broken_unmoved_fallback_is_not_a_quiet_empty_registry() {
        let tmp = tempfile::tempdir().unwrap();
        let git_dir = tmp.path().join("repo/.git");
        std::fs::create_dir_all(&git_dir).unwrap();
        let member = tmp.path().join("cache/run/member");
        run_state::record_work_tree(&member, &git_dir).unwrap();
        run_state::record_git_dir(&member, &git_dir).unwrap();
        let sessions = run_state::RunRoot::Sessions.under(&member);
        std::fs::create_dir_all(&sessions).unwrap();
        let legacy = run_state::RunRoot::Sessions.legacy_under(&git_dir);
        std::fs::write(&legacy, b"a file cannot be a session registry").unwrap();
        assert!(!run_state::is_moved(&member));
        assert_eq!(
            run_state::read_dirs(&sessions),
            vec![sessions.clone(), legacy]
        );
        assert!(matches!(
            read_live_caches(&sessions, 1_000),
            DirRead::Corrupt
        ));
    }

    use proptest::prelude::*;

    fn write_cache(dir: &Path, name: &str, body: &str) {
        let d = dir.join("rift-sessions");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join(format!("{name}.json")), body).unwrap();
    }

    /// A cache carrying the worktree discriminator: the work tree it claims.
    ///
    /// Serialised, not spliced: a Windows path carries `\`, which a raw JSON
    /// string would read as an escape and the reader would call corrupt.
    fn cache_json_claiming(id: &str, updated_at_ms: i64, work_tree: &Path) -> String {
        serde_json::json!({
            "schema": 1,
            "session_id": id,
            "state": "active",
            "agent": "claude-code",
            "current_turn_ordinal": 1,
            "turn_open": true,
            "compaction_boundary": null,
            "updated_at_ms": updated_at_ms,
            "native_id_map": {},
            "work_tree": work_tree.display().to_string(),
        })
        .to_string()
    }

    /// A family member built directly, so the partition is tested without a
    /// git fixture: `family_sessions` reads directories, never a repository.
    fn member(name: &str, git_dir: &Path, work_tree: &Path, is_main: bool) -> FamilyMember {
        std::fs::create_dir_all(git_dir).unwrap();
        std::fs::create_dir_all(work_tree).unwrap();
        FamilyMember {
            name: name.to_owned(),
            work_tree: work_tree.to_path_buf(),
            git_dir: git_dir.to_path_buf(),
            is_main,
            prunable: false,
        }
    }

    /// A two-member family under one tempdir: main and one linked worktree,
    /// laid out the way git lays them out.
    fn family(root: &Path) -> Vec<FamilyMember> {
        let main_git = root.join("main/.git");
        vec![
            member("main", &main_git, &root.join("main"), true),
            member(
                "wt",
                &main_git.join("worktrees/wt"),
                &root.join("wt"),
                false,
            ),
        ]
    }

    /// A member with a recorded family reads the git dir's old tree until it
    /// is marked moved, and only the new tree after: one tree per read, and a
    /// reader left on the old path would read nothing once the move is done.
    #[test]
    fn a_moved_member_is_read_from_its_run_state_and_never_from_its_git_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let git_dir = rift_git::test_support::fake_repo(&repo).unwrap();
        let paths = rift_git::discover(&repo).unwrap();
        let FamilyState::Minted(family) =
            rift_git::resolve_family(&paths, MintPolicy::Allow).unwrap()
        else {
            panic!("the fixture's family is minted");
        };
        let cache_root = tmp.path().join("cache");
        let legacy = git_dir.join("rift-sessions");
        assert_eq!(member_sessions_under(&cache_root, &git_dir, &repo), legacy);

        let family_dir = rift_store::projection_dir_under(&cache_root, &family.family_id);
        let member = run_state::member_dir(&family_dir, &repo);
        run_state::create_private_dir(&member).unwrap();
        std::fs::write(member.join(run_state::MOVED_MARKER), b"").unwrap();
        let moved = member_sessions_under(&cache_root, &git_dir, &repo);
        assert_eq!(moved, RunRoot::Sessions.under(&member));
        write_cache(&git_dir, "left-behind", &cache_json("active", 900));
        std::fs::create_dir_all(&moved).unwrap();
        std::fs::write(moved.join("s.json"), cache_json("active", 950)).unwrap();
        match read_live_caches(&moved, 1_000) {
            DirRead::Ok(caches) => assert_eq!(caches.len(), 1, "the new tree alone"),
            DirRead::Corrupt => panic!("the new tree reads"),
        }
    }

    fn count(detail: &SessionsDetail) -> u16 {
        match detail.read {
            SessionsRead::Ok(Some(live)) => live.active_sessions,
            SessionsRead::Ok(None) => 0,
            SessionsRead::Corrupt => u16::MAX,
        }
    }

    #[test]
    fn a_session_is_credited_to_the_member_it_claims_not_the_registry_it_sits_in() {
        // The discriminator on a READ. An agent whose process runs in the main checkout
        // registers its session under the shared `.git`, but the cache names the
        // tree it was observed in — so the member gets it, and main does not.
        // Crediting by location instead would put a member's live session on
        // the wrong row and count it twice in the family.
        let tmp = tempfile::tempdir().unwrap();
        let members = family(tmp.path());
        write_cache(
            &members[0].git_dir,
            "claims-wt",
            &cache_json_claiming("s-wt", 900, &members[1].work_tree),
        );
        let read = family_sessions(&members, 1_000);
        assert_eq!(count(&read.per_member[0]), 0, "main did not run it");
        assert_eq!(count(&read.per_member[1]), 1, "the member it names did");
        assert_eq!(
            read.per_member[1].newest_live.as_deref(),
            Some("s-wt"),
            "the identity travels with the credit"
        );
        assert_eq!(count(&read.family), 1, "one session, counted once");
    }

    #[test]
    fn the_family_counts_every_members_live_session_exactly_once() {
        // The family row is the sum over members, and a cache visible from two
        // registries is still ONE session: the same id in both dirs must not
        // become two threads on the badge.
        let tmp = tempfile::tempdir().unwrap();
        let members = family(tmp.path());
        write_cache(&members[0].git_dir, "in-main", &cache_json("active", 800));
        write_cache(
            &members[1].git_dir,
            "in-wt",
            &cache_json_claiming("s-wt", 900, &members[1].work_tree),
        );
        // The same session, seen from the shared registry too.
        write_cache(
            &members[0].git_dir,
            "also-s-wt",
            &cache_json_claiming("s-wt", 900, &members[1].work_tree),
        );
        let read = family_sessions(&members, 1_000);
        assert_eq!(count(&read.family), 2, "two sessions, not three");
        assert_eq!(count(&read.per_member[0]), 1);
        assert_eq!(count(&read.per_member[1]), 1);
        assert_eq!(
            read.family.read,
            SessionsRead::Ok(Some(LiveSessions {
                active_sessions: 2,
                since_ms: 800,
            })),
            "the family's `since` is the earliest live activity anywhere in it"
        );
    }

    #[test]
    fn a_cache_with_no_claim_stays_with_the_registry_it_was_found_in() {
        // Every cache written before the discriminator existed has none, and
        // absence is not a claim: it stays usable exactly where it has always
        // been, and is never adopted by a checkout that merely shares a
        // repository with it.
        let tmp = tempfile::tempdir().unwrap();
        let members = family(tmp.path());
        write_cache(&members[1].git_dir, "legacy", &cache_json("active", 950));
        let read = family_sessions(&members, 1_000);
        assert_eq!(count(&read.per_member[0]), 0);
        assert_eq!(count(&read.per_member[1]), 1);
        // A claim naming a tree outside this family is likewise not a claim on
        // anyone in it.
        let outsider = tmp.path().join("elsewhere");
        std::fs::create_dir_all(&outsider).unwrap();
        write_cache(
            &members[0].git_dir,
            "stranger",
            &cache_json_claiming("s-out", 940, &outsider),
        );
        let read = family_sessions(&members, 1_000);
        assert_eq!(
            count(&read.per_member[0]),
            1,
            "an unrecognised claim keeps the cache where it was found"
        );
        assert_eq!(count(&read.per_member[1]), 1);
    }

    #[test]
    fn a_corrupt_cache_is_loud_at_the_member_and_at_the_family() {
        // "A live session I cannot count" must never render as "quiet" at
        // either scope. Corruption carries no claim, so it belongs to the
        // registry it sits in — and the family says so too, because the sum it
        // would otherwise print is provably incomplete.
        let tmp = tempfile::tempdir().unwrap();
        let members = family(tmp.path());
        write_cache(&members[0].git_dir, "fine", &cache_json("active", 900));
        write_cache(&members[1].git_dir, "bad", "not json at all");
        let read = family_sessions(&members, 1_000);
        assert_eq!(
            read.per_member[0].read,
            SessionsRead::Ok(Some(LiveSessions {
                active_sessions: 1,
                since_ms: 900,
            }))
        );
        assert_eq!(read.per_member[1].read, SessionsRead::Corrupt);
        assert_eq!(read.family.read, SessionsRead::Corrupt);
    }

    #[test]
    fn a_member_with_no_registry_at_all_reads_quiet_never_corrupt() {
        // A worktree nobody has run an agent in has no `rift-sessions` dir.
        // That is an absent fact, not an unreadable one, and an empty family
        // reads quiet rather than erroring.
        let tmp = tempfile::tempdir().unwrap();
        let members = family(tmp.path());
        let read = family_sessions(&members, 1_000);
        assert_eq!(read.per_member[0].read, SessionsRead::Ok(None));
        assert_eq!(read.per_member[1].read, SessionsRead::Ok(None));
        assert_eq!(read.family.read, SessionsRead::Ok(None));
        // And a family with no members at all is quiet, not a panic.
        let empty = family_sessions(&[], 1_000);
        assert!(empty.per_member.is_empty());
        assert_eq!(empty.family.read, SessionsRead::Ok(None));
    }

    fn cache_json(state: &str, updated_at_ms: i64) -> String {
        format!(
            r#"{{"schema":1,"session_id":"s","state":"{state}","agent":"claude-code",
                 "current_turn_ordinal":3,"turn_open":true,"compaction_boundary":null,
                 "updated_at_ms":{updated_at_ms},"native_id_map":{{}}}}"#
        )
    }

    #[test]
    fn no_sessions_dir_reads_quiet() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(active_sessions(tmp.path(), 1_000), SessionsRead::Ok(None));
    }

    #[test]
    fn live_fresh_sessions_are_counted_distinctly() {
        let tmp = tempfile::tempdir().unwrap();
        write_cache(tmp.path(), "a", &cache_json("active", 900));
        write_cache(tmp.path(), "b", &cache_json("compacting", 800));
        write_cache(tmp.path(), "c", &cache_json("ended", 950)); // not live
        assert_eq!(
            active_sessions(tmp.path(), 1_000),
            SessionsRead::Ok(Some(LiveSessions {
                active_sessions: 2,
                since_ms: 800, // the earliest live activity
            }))
        );
    }

    #[test]
    fn crash_stale_sessions_are_ignored() {
        let tmp = tempfile::tempdir().unwrap();
        let now = 100 * ACTIVE_WINDOW_MS;
        write_cache(
            tmp.path(),
            "stale",
            &cache_json("active", now - ACTIVE_WINDOW_MS - 1),
        );
        write_cache(tmp.path(), "fresh", &cache_json("active", now - 5));
        assert_eq!(
            active_sessions(tmp.path(), now),
            SessionsRead::Ok(Some(LiveSessions {
                active_sessions: 1,
                since_ms: now - 5,
            }))
        );
    }

    #[test]
    fn sessions_detail_picks_the_newest_live_id() {
        // The metrics anchor is the live session touched most recently — the
        // one being typed in — not the earliest (`since_ms` keeps that role).
        let tmp = tempfile::tempdir().unwrap();
        write_cache(tmp.path(), "a", &cache_json_id("a-id", "active", 900));
        write_cache(tmp.path(), "b", &cache_json_id("b-id", "compacting", 800));
        write_cache(tmp.path(), "c", &cache_json_id("c-id", "ended", 950)); // not live
        let detail = sessions_detail(tmp.path(), 1_000);
        assert_eq!(
            detail.read,
            SessionsRead::Ok(Some(LiveSessions {
                active_sessions: 2,
                since_ms: 800,
            }))
        );
        assert_eq!(detail.newest_live.as_deref(), Some("a-id"));
        assert_eq!(detail.identities.len(), 2);
        assert_eq!(detail.unresolved, 0);
    }

    #[test]
    fn an_id_less_cache_counts_toward_the_badge_but_never_anchors() {
        // Leniency: only `state` + `updated_at_ms` are required — a cache
        // without a `session_id` still counts as live, it just cannot be the
        // metrics anchor.
        let tmp = tempfile::tempdir().unwrap();
        write_cache(
            tmp.path(),
            "bare",
            r#"{"state":"active","updated_at_ms":990}"#,
        );
        let detail = sessions_detail(tmp.path(), 1_000);
        assert_eq!(
            detail.read,
            SessionsRead::Ok(Some(LiveSessions {
                active_sessions: 1,
                since_ms: 990,
            }))
        );
        assert_eq!(detail.newest_live, None);
        assert!(detail.identities.is_empty());
        assert_eq!(detail.unresolved, 1);
    }

    #[test]
    fn newest_live_ties_break_deterministically() {
        let tmp = tempfile::tempdir().unwrap();
        write_cache(tmp.path(), "x", &cache_json_id("aa", "active", 900));
        write_cache(tmp.path(), "y", &cache_json_id("zz", "active", 900));
        let detail = sessions_detail(tmp.path(), 1_000);
        assert_eq!(
            detail.newest_live.as_deref(),
            Some("zz"),
            "equal freshness breaks to the greater id, deterministically"
        );
    }

    fn cache_json_id(id: &str, state: &str, updated_at_ms: i64) -> String {
        format!(
            r#"{{"schema":1,"session_id":"{id}","state":"{state}","agent":"claude-code",
                 "current_turn_ordinal":3,"turn_open":true,"compaction_boundary":null,
                 "updated_at_ms":{updated_at_ms},"native_id_map":{{}}}}"#
        )
    }

    #[test]
    fn unknown_fields_and_unknown_states_degrade_gracefully() {
        // Leniency (fog: schema drift): extra fields ignored; a new state the
        // writer might grow reads as not-live rather than corrupt.
        let tmp = tempfile::tempdir().unwrap();
        write_cache(
            tmp.path(),
            "future",
            r#"{"schema":2,"session_id":"s","state":"hibernating","updated_at_ms":990,
                "brand_new_field":{"nested":true}}"#,
        );
        assert_eq!(active_sessions(tmp.path(), 1_000), SessionsRead::Ok(None));
    }

    #[test]
    fn corrupt_bytes_are_a_distinct_error_never_quiet() {
        let tmp = tempfile::tempdir().unwrap();
        write_cache(tmp.path(), "bad", "not json at all");
        assert_eq!(active_sessions(tmp.path(), 1_000), SessionsRead::Corrupt);

        // Valid JSON that is not a cache shape is corrupt too.
        let tmp = tempfile::tempdir().unwrap();
        write_cache(tmp.path(), "shape", "[1, 2, 3]");
        assert_eq!(active_sessions(tmp.path(), 1_000), SessionsRead::Corrupt);
    }

    #[test]
    fn oversized_cache_file_is_corrupt() {
        let tmp = tempfile::tempdir().unwrap();
        let huge = format!(
            r#"{{"state":"active","updated_at_ms":1,"pad":"{}"}}"#,
            "x".repeat(usize::try_from(MAX_CACHE_BYTES).unwrap())
        );
        write_cache(tmp.path(), "huge", &huge);
        assert_eq!(active_sessions(tmp.path(), 1_000), SessionsRead::Corrupt);
    }

    #[test]
    fn non_json_extensions_are_ignored() {
        // The writer's own temp files (`.<id>.tmp`) must not read as corrupt.
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path().join("rift-sessions");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join(".s1.tmp"), b"\xff\xfe partial write").unwrap();
        write_cache(tmp.path(), "ok", &cache_json("active", 990));
        assert_eq!(
            active_sessions(tmp.path(), 1_000),
            SessionsRead::Ok(Some(LiveSessions {
                active_sessions: 1,
                since_ms: 990,
            }))
        );
    }

    proptest! {
        /// The parser is total over arbitrary bytes: Some or None, never a panic.
        #[test]
        fn parser_is_total_over_arbitrary_bytes(bytes in proptest::collection::vec(any::<u8>(), 0..2048)) {
            let _ = parse_cache(&bytes);
        }

        /// The full query is total over one arbitrary cache file: it yields a
        /// SessionsRead, never a panic, for any file contents.
        #[test]
        fn query_is_total_over_arbitrary_file(bytes in proptest::collection::vec(any::<u8>(), 0..2048)) {
            let tmp = tempfile::tempdir().unwrap();
            let d = tmp.path().join("rift-sessions");
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("x.json"), &bytes).unwrap();
            let _ = active_sessions(tmp.path(), 1_000);
        }
    }
}
