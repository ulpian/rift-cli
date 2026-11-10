//! End-to-end: linked worktrees as family members through the real binary —
//! one projection for the family, session rows keyed by the family id, the
//! audit sink resolving a subdirectory to its family, and a worktree adopting
//! only the shared session that names it.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use std::path::{Path, PathBuf};

mod common;
#[allow(dead_code)] // The hooks suites share these builders; each file uses a subset.
#[path = "common/hooks_fixture.rs"]
mod hooks_fixture;
use hooks_fixture::{
    git_in, json_str, now_ms, run_rift_capture, run_rift_in, run_rift_stdout,
    seed_live_session_repo, seed_session, sync_env,
};

/// The captured hook receipts in the **family's** projection, resolved from
/// `member` exactly as the product resolves it.
///
/// [`rift_git::MintPolicy::Never`]: the test reads the identity the product
/// minted, and would rather see an empty list than mint one of its own and
/// assert against a store nothing wrote to.
async fn family_hook_events(cache: &Path, member: &Path) -> Vec<String> {
    let (_, state) = rift_git::family_of(member, rift_git::MintPolicy::Never).unwrap();
    let rift_git::FamilyState::Minted(family) = state else {
        return Vec::new();
    };
    let db = rift_store::projection_db_for_family_under(cache, &family);
    if !db.exists() {
        return Vec::new();
    }
    let store = rift_store::open_capture_projection(db).unwrap();
    let rows = rift_store::CanonicalStore::query(
        &store,
        "SELECT session_id FROM agent_hook_event ORDER BY session_id",
        &[],
    )
    .await
    .unwrap();
    rows.iter().map(|r| r.get_text(0).unwrap()).collect()
}

/// How many archive decisions the **family's** projection holds, read from
/// `member`'s vantage. Zero when nothing has minted the family or the store yet.
fn archived_in_family(cache: &Path, member: &Path) -> i64 {
    let Ok((_, rift_git::FamilyState::Minted(family))) =
        rift_git::family_of(member, rift_git::MintPolicy::Never)
    else {
        return 0;
    };
    let db = rift_store::projection_db_for_family_under(cache, &family);
    if !db.exists() {
        return 0;
    }
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    rt.block_on(async {
        let store = rift_store::open_capture_projection(db).unwrap();
        rift_store::CanonicalStore::query_opt_i64(
            &store,
            "SELECT COUNT(*) FROM archive_decision WHERE archived = 1",
            &[],
        )
        .await
        .unwrap()
        .unwrap_or(0)
    })
}

/// The projection a checkout would have owned under the retired path rule —
/// the per-path derivation the family model replaced. Used only to assert that nothing
/// creates one any more.
fn per_path_projection(cache: &Path, work_tree: &Path) -> std::path::PathBuf {
    rift_store::projection_db_under(cache, &rift_git::repo_id_for(work_tree).unwrap())
}

/// The soak union counts the family's projection **once**, whatever the
/// worktree count.
///
/// `doctor`'s soak reads "the repository's evidence, not this checkout's", which
/// before the family mint meant unioning one projection per working tree. Members now
/// resolve one file, so a union that still deduped by the member's own path
/// would open that file once per worktree and multiply every count in the soak
/// by the number of checkouts that happen to exist. Stated through the real
/// binary, from the member — the vantage whose lexical parent used to resolve to
/// `…/.git/worktrees`.
#[test]
fn the_soak_union_counts_one_family_projection_once() {
    let tmp = tempfile::tempdir().unwrap();
    let main = seed_family_main(tmp.path());
    for name in ["wt1", "wt2"] {
        add_linked_worktree(&main, &tmp.path().join(name), name);
    }

    let cache = tmp.path().join("cache");
    let env = sync_env(&cache);
    // A hook creates the family's projection; the members then have one to find.
    session_start_in(&main, "01SOAKSESSION", &env);

    for vantage in [&main, &tmp.path().join("wt1")] {
        let (code, out) = run_rift_stdout(vantage, &["--json", "doctor"], &env);
        assert!(code == 0 || code == 1, "doctor answers: {out}");
        let doc: serde_json::Value = serde_json::from_str(&out).expect("doctor --json");
        assert_eq!(
            doc["soak"]["projections"], 1,
            "three worktrees, one family projection, unioned once: {out}"
        );
    }
}

/// **`session.repo_id` is the family id**, for every new row,
/// whichever member the session was worked in.
///
/// The value and the file are two separate decisions, and getting only one of
/// them right is worse than getting neither: rows written under a member's own
/// path hash into the family's projection are invisible to every reader that
/// scopes by `repo_id`, which is all of them. So the assertion is stated
/// against the id the member's path *would* have produced, not merely against
/// "some id".
///
/// Historical rows keep whatever they were written with. Both values name the
/// same database after adoption, so no reader changes meaning — which is why
/// this needs no migration.
#[tokio::test]
async fn session_rows_carry_the_family_id() {
    let tmp = tempfile::tempdir().unwrap();
    let main = seed_family_main(tmp.path());
    let linked = tmp.path().join("linked");
    add_linked_worktree(&main, &linked, "wt");

    let cache = tmp.path().join("cache");
    let env = sync_env(&cache);
    session_start_in(&linked, "01MEMBERSESSION", &env);

    let (_, state) = rift_git::family_of(&linked, rift_git::MintPolicy::Never).unwrap();
    let rift_git::FamilyState::Minted(family) = state else {
        panic!("the hook minted the family it captured into");
    };
    let store = rift_store::open_capture_projection(rift_store::projection_db_for_family_under(
        &cache, &family,
    ))
    .unwrap();
    let rows =
        rift_store::CanonicalStore::query(&store, "SELECT DISTINCT repo_id FROM session", &[])
            .await
            .unwrap();
    let ids: Vec<String> = rows.iter().map(|r| r.get_text(0).unwrap()).collect();
    assert_eq!(
        ids,
        vec![family.family_id.as_str().to_owned()],
        "every session row carries the family id"
    );
    assert_ne!(
        family.family_id.as_str(),
        rift_git::repo_id_for(&linked).unwrap().as_str(),
        "the member's own path hashes to something else — which is what makes \
         the assertion above worth making"
    );
}

/// **The serve-audit sink writes the family's `recall.db`** — from a
/// subdirectory, and from a linked member — never a store keyed by whatever
/// directory the hook happened to be standing in.
///
/// `rift hooks memd <verb>` used to resolve its store from the raw process
/// cwd. A serve audited from `<repo>/crates/x` therefore landed in a
/// `recall.db` keyed by `<repo>/crates/x`, while `rift recall <verb>` served
/// out of the one keyed by `<repo>` — writer and reader on different files,
/// which is what stranded the savings panel on "no serves yet" while real
/// serves were being made. The cwd is now resolved to its work tree and that
/// work tree to its family.
#[tokio::test]
async fn the_memd_audit_sink_writes_the_family_recall_db_from_a_subdirectory() {
    let tmp = tempfile::tempdir().unwrap();
    let main = tmp.path().join("main");
    std::fs::create_dir_all(&main).unwrap();
    let main_git = rift_git::test_support::fake_repo(&main).unwrap();
    let deep = main.join("crates").join("x");
    std::fs::create_dir_all(&deep).unwrap();
    let member = tmp.path().join("member");
    rift_git::test_support::fake_worktree(&main_git, &member, "member").unwrap();
    let member_sub = member.join("bins");
    std::fs::create_dir_all(&member_sub).unwrap();

    let cache = tmp.path().join("cache");
    let env = [
        ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
        ("RIFT_NO_KEYCHAIN", "1"),
    ];
    let envelope = |key: &str| {
        format!(
            r#"{{"schema":1,"idempotency_key":"{key}","verb":"memd.known_state","payload":{{"handle":"h","estimated_tokens_returned":7,"estimated_tokens_avoided":11}}}}"#
        )
    };
    for (cwd, key) in [(&deep, "from-subdir"), (&member_sub, "from-member")] {
        assert_eq!(
            run_rift_in(cwd, &["hooks", "memd", "known-state"], &envelope(key), &env),
            0,
            "an audit-write never disturbs the serve path"
        );
    }

    // One store, and it is the family's — which for a main work tree is the
    // file the recall id has always named, adopted in place.
    let family_db =
        rift_store::recall_db_under(&cache, &rift_store::recall_repo_id(&main).unwrap());
    let store = rift_store::open_recall_projection(&family_db).unwrap();
    let rows = rift_store::CanonicalStore::query(
        &store,
        "SELECT audit_id FROM memd_audit_event ORDER BY audit_id",
        &[],
    )
    .await
    .unwrap();
    let got: Vec<String> = rows.iter().map(|r| r.get_text(0).unwrap()).collect();
    assert_eq!(
        got,
        vec!["from-member".to_string(), "from-subdir".to_string()],
        "both serves are audited into the one family store"
    );

    // And no store was keyed by a directory that is not a repository root.
    for stray in [&deep, &member_sub, &member] {
        let per_path =
            rift_store::recall_db_under(&cache, &rift_store::recall_repo_id(stray).unwrap());
        assert!(
            !per_path.exists(),
            "no recall.db may be minted for {}",
            stray.display()
        );
    }
}

/// **The whole point of the family model, through the real binary.** A hook fired in a
/// linked worktree lands in the family's projection — the one the main work
/// tree already owns — and the member gets no second, empty store of its own.
///
/// Two defects meet here. The first silenced capture in every linked worktree
/// of this repository: a linked worktree's `.git` is a FILE
/// (`gitdir: <main>/.git/worktrees/<name>`), so deriving `<cwd>/.git` as a
/// directory made every session read fail `ENOTDIR` — `NotADirectory`, not
/// `NotFound` — and ingest aborted *before* writing its receipt, exiting 0.
/// The second is the one the family identity closes: even once the hook captured, it
/// captured into a projection keyed by the member's own path, so from
/// `~/rift-worktrees/canopy` the binary reported 0 checkpoints while the
/// family's refs held 134.
///
/// The assertion that makes this load-bearing is the *negative* one: no
/// per-path projection exists for the member afterwards. Counting the member's
/// own rows would pass under either rule.
#[tokio::test]
async fn an_agent_hook_fired_in_a_linked_worktree_lands_in_the_family_projection() {
    let tmp = tempfile::tempdir().unwrap();
    let main = seed_family_main(tmp.path());
    let linked = tmp.path().join("linked");
    add_linked_worktree(&main, &linked, "wt");
    assert!(linked.join(".git").is_file(), "a linked .git is a file");

    let cache = tmp.path().join("cache");
    let env = sync_env(&cache);
    session_start_in(&main, "01MAINSESSION", &env);
    session_start_in(&linked, "01LINKEDSESSION", &env);

    // One store, both members: the session worked in the linked worktree and
    // the session worked in main are in the SAME projection, and the family is
    // reachable from either door.
    let from_linked = family_hook_events(&cache, &linked).await;
    let from_main = family_hook_events(&cache, &main).await;
    assert_eq!(
        from_linked,
        vec!["01LINKEDSESSION".to_string(), "01MAINSESSION".to_string()],
        "a hook fired in a linked worktree lands in the family projection, \
         beside the main work tree's"
    );
    assert_eq!(
        from_linked, from_main,
        "either member's vantage resolves the one store"
    );

    // The negative half, and the one that can only pass under the new rule:
    // the member has no projection of its own. Nothing was minted for
    // `~/…/linked`, so nothing can be stranded in it.
    assert!(
        !per_path_projection(&cache, &linked).exists(),
        "a member must not mint a second projection keyed by its own path"
    );
    // And adoption is in place: the family's file is exactly the one the main
    // work tree already owned — no row moved, no rebuild.
    let (_, state) = rift_git::family_of(&linked, rift_git::MintPolicy::Never).unwrap();
    let rift_git::FamilyState::Minted(family) = state else {
        panic!("the hook minted the family it captured into");
    };
    assert_eq!(
        rift_store::projection_db_for_family_under(&cache, &family),
        per_path_projection(&cache, &main),
        "the family adopts the main work tree's historical id, in place"
    );

    // Per-worktree state is the opposite case and stays split: a
    // session lives in one checkout at a time.
    // …its session state lands in its own member's run state.
    let linked_admin = main.join(".git").join("worktrees").join("linked");
    assert!(common::sessions_dir(&linked_admin, &cache)
        .join("01LINKEDSESSION.json")
        .is_file());
    assert!(!common::sessions_dir(&main.join(".git"), &cache)
        .join("01LINKEDSESSION.json")
        .exists());
}

/// A main work tree with one commit, its hooks pointed at an empty directory
/// so a commit of the fixture's own fires nothing.
fn seed_family_main(tmp: &Path) -> PathBuf {
    let main = tmp.join("main");
    let nohooks = tmp.join("nohooks");
    std::fs::create_dir_all(&main).unwrap();
    std::fs::create_dir_all(&nohooks).unwrap();
    git_in(&main, &["init", "-q", "."]);
    git_in(&main, &["config", "user.email", "t@t"]);
    git_in(&main, &["config", "user.name", "T"]);
    git_in(
        &main,
        &["config", "core.hooksPath", nohooks.to_str().unwrap()],
    );
    std::fs::write(main.join("a.txt"), "one\n").unwrap();
    git_in(&main, &["add", "a.txt"]);
    git_in(&main, &["commit", "-q", "-m", "seed"]);
    main
}

/// `git worktree add` from `main`: a linked member at `path` on a new branch.
fn add_linked_worktree(main: &Path, path: &Path, branch: &str) {
    git_in(
        main,
        &[
            "worktree",
            "add",
            "-q",
            path.to_str().unwrap(),
            "-b",
            branch,
        ],
    );
}

/// A `SessionStart` in the raw shape Claude Code sends it, fired from `cwd`:
/// a capture hook never fails the agent.
fn session_start_in(cwd: &Path, session: &str, env: &[(&str, &str)]) {
    let body = format!(r#"{{"session_id":"{session}","hook_event_name":"SessionStart"}}"#);
    assert_eq!(
        run_rift_in(cwd, &["hooks", "claude-code", "SessionStart"], &body, env),
        0,
        "a capture hook never fails the agent"
    );
}

/// **Both halves of the credit rule in one fixture: a linked worktree adopts
/// the session that NAMES it, and never the one that names its neighbour.**
///
/// Measured 2026-08-18, and this is a product defect for any user with git
/// worktrees rather than an artifact of how this repository is developed: an
/// agent registers its session under the git dir its own process resolved,
/// while a commit made in a different checkout of the same repository fires its
/// hooks against that checkout's admin dir — which holds no sessions. Neither
/// side has both halves, so post-commit takes its no-session branch and
/// archives nothing. On this repository's five linked worktrees that was 0
/// sessions, 0 checkpoints and 0 verification runs against 63 file revisions.
///
/// The over-correction is the other half and is why the discriminator exists:
/// reading the shared registry without one would credit a session driven in
/// worktree A with a commit made in worktree B. So the fixture builds TWO
/// linked worktrees off one repository, puts one session in the shared registry
/// claiming worktree A, and commits in BOTH.
///
/// **THE POSITIVE HALF SEEDS A SHAPE NO PRODUCTION WRITER EMITS, and that is
/// stated rather than quietly relied on.** `seed_shared_session` hand-writes a
/// shared-registry entry claiming a LINKED worktree. Nothing in production
/// writes that: `write_cache` stamps the claim from the git dir it is addressed
/// to, so a file in the COMMON registry is always stamped with the MAIN
/// worktree, while `live_sessions_for`'s shared branch fires only for a linked
/// one. Planted both ways on the shipped binary — a real session started in the
/// main checkout gives `archived=0` when committing in a linked worktree and
/// `archived=1` in the main checkout, the control. So this test pins the RULE
/// and not a reachable path; the gap it leaves is enumerated and OPEN.
///
/// It is not weakened or deleted on that account: calibration plant 3 showed it
/// bites — flipping `is_some_and` to `is_none_or` reds its negative half — and a
/// rule that is unreachable today is exactly the rule that must already be right
/// on the day something makes it reachable.
fn seed_shared_session(common_git: &Path, id: &str, work_tree: &Path, updated_at_ms: i64) {
    let sessions = common::seed_sessions_dir(common_git);
    std::fs::create_dir_all(&sessions).unwrap();
    let wt = std::fs::canonicalize(work_tree).unwrap();
    std::fs::write(
        sessions.join(format!("{id}.json")),
        format!(
            r#"{{"schema":1,"session_id":"{id}","state":"active","agent":"claude-code",
                "current_turn_ordinal":1,"turn_open":false,"compaction_boundary":null,
                "updated_at_ms":{updated_at_ms},"native_id_map":{{}},
                "work_tree":{}}}"#,
            json_str(&wt)
        ),
    )
    .unwrap();
}

#[test]
fn a_worktree_adopts_only_the_shared_session_that_names_it() {
    let tmp = tempfile::tempdir().unwrap();
    let main = seed_live_session_repo(tmp.path());
    // The seeder leaves a session of its own in the shared registry; this test
    // is about which registry a LINKED worktree reads, so start it clean.
    let shared = common::seed_sessions_dir(&main.join(".git"));
    std::fs::remove_dir_all(&shared).ok();

    let wt_a = tmp.path().join("wt-a");
    let wt_b = tmp.path().join("wt-b");
    for (path, branch) in [(&wt_a, "side-a"), (&wt_b, "side-b")] {
        add_linked_worktree(&main, path, branch);
    }

    // ONE session, in the SHARED registry, claiming worktree A only.
    let now = now_ms();
    seed_shared_session(&main.join(".git"), "shared-a", &wt_a, now);
    // …and an OLDER session in the same registry, carrying no discriminator
    // at all — every cache file written before the claim existed looks like this. It
    // claims nobody, so nobody may adopt it. Without this entry the
    // "absence is not a claim" rule is unfalsifiable: measured, flipping
    // `is_some_and` to `is_none_or` left the suite green until it was added.
    seed_session(&main, "legacy-nobody", now);

    let cache = tmp.path().join("cache");
    let env = sync_env(&cache);

    // Commit in BOTH worktrees, each firing its own post-commit hook.
    let condensed: Vec<bool> = [&wt_a, &wt_b]
        .into_iter()
        .map(|wt| commit_condenses_in(wt, &cache, &env))
        .collect();

    // A: the session names this worktree, so the commit condenses under it —
    // the capability this rule restores.
    assert!(
        condensed[0],
        "worktree A adopts the shared session that names it and condenses"
    );
    // B: the same session is visible in the same shared registry and names
    // SOMEONE ELSE, so B archives nothing. This is the half that must never
    // regress: it is the difference between capturing work and misattributing
    // it.
    assert!(
        !condensed[1],
        "worktree B must NOT be credited with a session that names worktree A, \
         nor with an older session that names nobody"
    );
}

/// Commit in `wt` and fire its own post-commit hook: whether THIS worktree's
/// hook condensed. The projection is the family's now: both members write to
/// one store, so "did THIS worktree condense" is the DELTA its own hook made,
/// not the store's total. The checkpoint ref would prove even less —
/// `refs/heads/rift/checkpoints/v1` has always been shared, so reading it from
/// B would show A's condensation and the test would pass having demonstrated
/// nothing.
fn commit_condenses_in(wt: &Path, cache: &Path, env: &[(&str, &str)]) -> bool {
    let before = archived_in_family(cache, wt);
    std::fs::write(wt.join("w.txt"), "work").unwrap();
    git_in(wt, &["add", "w.txt"]);
    git_in(wt, &["commit", "-q", "-m", "work"]);
    let git_dir = git_in(wt, &["rev-parse", "--absolute-git-dir"])
        .trim()
        .to_owned();
    let (code, stderr) = run_rift_capture(
        wt,
        &["hooks", "git", "post-commit", "--git-dir", &git_dir],
        Some(""),
        env,
    );
    assert_eq!(code, 0, "the hook never fails the git op: {stderr}");
    archived_in_family(cache, wt) > before
}
