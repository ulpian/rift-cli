//! The family's members: directories named `.git` that are not
//! repositories, members whose checkout is gone, members with no agent wiring
//! of their own, and the per-path projections minted before the family
//! identity. Every leg here lists; only the wiring leg repairs, and only
//! under adoption's own rules.

use std::path::Path;

use crate::{adopt, hooks, worktree};

/// How deep below the work-tree root the phantom scan walks. Six levels reaches
/// every phantom this repository has ever grown (`crates/rift-core/src/` is
/// three) without turning `rift doctor` into a whole-tree crawl; the number is
/// a constant rather than a flag because a bound nobody can change is a bound
/// nobody has to reason about.
const PHANTOM_SCAN_DEPTH: usize = 6;

/// Directory names the scan never descends into. `target/` and `node_modules/`
/// are large and hold nobody's `.git`; `.git/` is skipped because a real gitdir
/// legitimately contains `worktrees/<name>` admin dirs, and descending would
/// report the repository's own plumbing as a finding.
const PHANTOM_SCAN_SKIP: &[&str] = &["target", "node_modules", ".git"];

/// One directory named `.git` that is not a gitdir.
struct PhantomGitdir {
    /// The directory itself — what the printed `rm -rf` line names.
    path: std::path::PathBuf,
    /// How many session-cache files it swallowed (`rift-sessions/*.json`).
    /// Non-zero means captured work was written under a second identity.
    sessions: usize,
}

/// Session-cache files under a gitdir — the evidence a phantom actually took
/// capture, rather than merely existing.
fn phantom_session_count(git_dir: &Path) -> usize {
    let Ok(entries) = std::fs::read_dir(git_dir.join("rift-sessions")) else {
        return 0;
    };
    entries
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .count()
}

/// Collect every `<dir>/.git` under `root` that is a directory but not a gitdir,
/// walking at most [`PHANTOM_SCAN_DEPTH`] levels down and never following a
/// symlink. Sorted, so the output is stable across filesystems.
fn scan_phantom_gitdirs(root: &Path) -> Vec<PhantomGitdir> {
    let mut found = Vec::new();
    let mut queue = vec![(root.to_path_buf(), 0_usize)];
    while let Some((dir, depth)) = queue.pop() {
        let dot_git = dir.join(".git");
        if dot_git.is_dir() && !rift_git::is_gitdir(&dot_git) {
            found.push(PhantomGitdir {
                sessions: phantom_session_count(&dot_git),
                path: dot_git,
            });
        }
        if depth == PHANTOM_SCAN_DEPTH {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            // `file_type` here does not follow symlinks, so a link back up the
            // tree is a non-directory and the walk cannot loop.
            if !entry.file_type().is_ok_and(|t| t.is_dir()) {
                continue;
            }
            let name = entry.file_name();
            if PHANTOM_SCAN_SKIP
                .iter()
                .any(|s| name == std::ffi::OsStr::new(s))
            {
                continue;
            }
            queue.push((entry.path(), depth + 1));
        }
    }
    found.sort_by(|a, b| a.path.cmp(&b.path));
    found
}

/// Directories named `.git` that are not repositories.
///
/// A hook fired with a cwd outside any checkout used to answer `<cwd>/.git` and
/// then create it; the next discovery walking up through that directory read it
/// as a repository and minted a second identity for work that belonged to the
/// real one above it. Five existed in this tree at once. The fallback is
/// retired (`hooks::capture_repo`, private), so this leg is the recurrence
/// detector: it says which are still there and how much capture each took.
///
/// **`--fix` never deletes one.** Removing a directory called `.git` is
/// irreversible and the tool cannot tell a phantom it minted from one a person
/// meant to keep (a half-initialised repository, a fixture, a restore in
/// progress); so `--fix` prints the exact commands and the decision stays with
/// the owner — the same rule `capture chain: DIVERGED` follows.
pub(super) fn doctor_phantom_gitdirs(repo: &Path, fix: bool) -> (String, serde_json::Value) {
    let found = scan_phantom_gitdirs(repo);
    if found.is_empty() {
        return (
            String::new(),
            serde_json::json!({ "phantoms": 0, "depth": PHANTOM_SCAN_DEPTH }),
        );
    }
    let mut human = format!(
        "\n  phantom .git:   {} director(ies) named `.git` with no HEAD — not \
         repositories, and rift never deletes one",
        found.len()
    );
    for p in &found {
        human.push_str(&format!(
            "\n    {} ({} session file(s))",
            p.path.display(),
            p.sessions
        ));
    }
    if fix {
        human.push_str("\n    --fix does not remove them; to remove them by hand:");
        for p in &found {
            human.push_str(&format!("\n      rm -rf '{}'", p.path.display()));
        }
    }
    let json = serde_json::json!({
        "phantoms": found.len(),
        "depth": PHANTOM_SCAN_DEPTH,
        "deleted": false,
        "found": found
            .iter()
            .map(|p| serde_json::json!({
                "path": p.path.display().to_string(),
                "sessions": p.sessions,
            }))
            .collect::<Vec<_>>(),
    });
    (human, json)
}

/// Family members whose administrative entry survives but whose checkout is
/// gone — `git worktree prune` candidates.
///
/// Reported, **never pruned**, on the rule the phantom-gitdir leg follows: a
/// removal is irreversible and rift cannot tell a worktree someone deleted from
/// one on a disk that is merely not mounted. They are named here because a
/// prunable member is not inert: its admin dir can still hold a capture queue
/// whose entries now condense commit-only, and the capture-queue leg
/// drains exactly those.
pub(super) fn doctor_prunable_worktrees(repo: &Path) -> (String, serde_json::Value) {
    let Ok(paths) = rift_git::discover(repo) else {
        return (String::new(), serde_json::json!({ "readable": false }));
    };
    let members = rift_git::family_members(&paths);
    let gone: Vec<&rift_git::FamilyMember> = members.iter().filter(|m| m.prunable).collect();
    if gone.is_empty() {
        return (
            String::new(),
            serde_json::json!({ "members": members.len(), "prunable": 0 }),
        );
    }
    let mut human = format!(
        "\n  worktrees:      {} of {} member(s) have no checkout left \
         (rift never prunes one)",
        gone.len(),
        members.len()
    );
    for m in &gone {
        human.push_str(&format!("\n    {} → {}", m.name, m.work_tree.display()));
    }
    human.push_str("\n    to remove the administrative entries: git worktree prune");
    let json = serde_json::json!({
        "members": members.len(),
        "prunable": gone.len(),
        "pruned": false,
        "found": gone
            .iter()
            .map(|m| serde_json::json!({
                "name": m.name,
                "work_tree": m.work_tree.display().to_string(),
                "git_dir": m.git_dir.display().to_string(),
            }))
            .collect::<Vec<_>>(),
    });
    (human, json)
}

/// Which members of this family are wired for the agents the family enabled —
/// and, with `--fix`, wiring the ones that are not.
///
/// A linked worktree is a shadow member: the projection, the recall store and
/// the git hooks are the family's, but the *agent wiring* is per checkout and
/// git-ignored, so a member created by `git worktree add` before the
/// `post-checkout` hook existed — or one whose adoption was refused — runs its
/// agents with no Rift hooks at all. That is silent by construction, which is
/// why it is a doctor leg.
///
/// **Silent when there is nothing to say.** A family of one member is the
/// common case and not a finding; so is a member already carrying
/// `.rift/state/native-integrations.json` with no adoption marker beside it.
///
/// `--fix` adopts, under adoption's own rules: read-only on the projection, the
/// binary of record, no tracked file, no git hook, and a refusal that is
/// reported rather than forced. When the projection cannot be read it says so
/// and wires nothing — a diagnosis that guessed the family's intent would be
/// worse than one that admits it does not know.
pub(super) fn doctor_worktree_wiring(repo: &Path, fix: bool) -> (String, serde_json::Value) {
    let Ok(paths) = rift_git::discover(repo) else {
        return (String::new(), serde_json::json!({ "readable": false }));
    };
    let members = rift_git::family_members(&paths);
    let unwired: Vec<&rift_git::FamilyMember> = members
        .iter()
        .filter(|member| !member.is_main && !member.prunable)
        .filter(|member| {
            !member
                .work_tree
                .join(".rift/state/native-integrations.json")
                .is_file()
                || adopt::marker_reason(&member.work_tree).is_some()
        })
        .collect();
    if unwired.is_empty() {
        return (
            String::new(),
            serde_json::json!({ "members": members.len(), "unwired": 0, "adopted": 0 }),
        );
    }
    let mut human = format!(
        "\n  worktree wiring: {} of {} member(s) carry no agent wiring of their own",
        unwired.len(),
        members.len()
    );
    let mut adopted = 0;
    let mut found = Vec::new();
    let machine = fix.then(|| adopt::machine_for(&paths).ok()).flatten();
    let intent = machine
        .is_some()
        .then(|| {
            crate::family::projection_db(&paths.work_tree).map(|db| adopt::enabled_agents(&db))
        })
        .and_then(std::result::Result::ok);
    for member in &unwired {
        let previous = adopt::marker_reason(&member.work_tree);
        let mut line = format!("\n    {} → {}", member.name, member.work_tree.display());
        if let Some(previous) = &previous {
            line.push_str(&format!(" ({previous})"));
        }
        let mut outcome = None;
        if let (Some(machine), Some(intent)) = (machine.as_ref(), intent.as_ref()) {
            let report = adopt::adopt_member(&member.work_tree, machine, intent, hooks::now_ms());
            if report.is_clean() {
                adopted += 1;
            }
            line.push_str(&format!("\n      {}", report.summary()));
            outcome = Some(report.summary());
        }
        human.push_str(&line);
        found.push(serde_json::json!({
            "name": member.name,
            "work_tree": member.work_tree.display().to_string(),
            "previous_attempt": previous,
            "adopted": outcome,
        }));
    }
    if !fix {
        human.push_str("\n    to wire them: rift worktree adopt <path> (or rift doctor --fix)");
    }
    let json = serde_json::json!({
        "members": members.len(),
        "unwired": unwired.len(),
        "adopted": adopted,
        "found": found,
    });
    (human, json)
}

/// Per-path projections a member minted before the family mint — **listed,
/// never merged, never deleted**.
///
/// Until the family mint, the projection was content-addressed over the
/// *working tree* path, so one repository in seven working trees had seven
/// databases under seven cache keys. Two exist on this machine; the six side
/// worktrees of this repository each had one. Adoption did not move them —
/// nothing in an append-only store can be moved — so they sit under the id the
/// retired rule would still derive for the same member, holding facts nothing
/// reads any more.
///
/// This leg derives that id again, on purpose: it is the one place the legacy
/// per-path derivation is sanctioned in a product path, because a diagnosis of
/// drift has to be able to SEE the old rule.
///
/// **What is NOT claimed.** Projection-only facts an orphan holds — per-turn
/// usage, the serve audit — are not recoverable into the family store, and the
/// line says so rather than implying a merge is pending — the same honesty rule
/// rebuilds follow. Only members the family can still enumerate are checked: a
/// worktree already `git worktree prune`d leaves no path to derive an id from,
/// so its projection is unfindable by construction and is not silently counted
/// as absent.
///
/// **`--fix` never removes one**, on the phantom-gitdir leg's rule: the removal
/// is irreversible, the facts are unrecoverable once gone, and rift cannot tell
/// a stale projection from one an operator is about to salvage. It prints what
/// it found and the exact command; the decision stays with the owner.
pub(super) fn doctor_orphan_projections(repo: &Path) -> (String, serde_json::Value) {
    if rift_git::discover(repo).is_err() || crate::family::projection_db(repo).is_err() {
        // No minted identity yet: every per-path database here is still the
        // one its member legitimately uses, and calling them orphans would be
        // a diagnosis of the family this vantage could not resolve.
        return (String::new(), serde_json::json!({ "readable": false }));
    }
    let orphans = worktree::orphan_projections(repo);
    if orphans.is_empty() {
        return (
            String::new(),
            serde_json::json!({ "orphans": 0, "deleted": false }),
        );
    }
    let mut human = format!(
        "\n  orphan projections: {} per-path projection(s) from before the family \
         mint (rift never deletes one)",
        orphans.len()
    );
    for o in &orphans {
        human.push_str(&format!(
            "\n    {} \u{2192} {} ({} event(s), not recoverable into the family store)",
            o.member,
            o.db.display(),
            o.events
                .map_or_else(|| "unreadable".to_owned(), |n| n.to_string()),
        ));
    }
    human.push_str("\n    --fix does not remove them; the verb that can:");
    human.push_str("\n      rift projects prune --orphan-projections --yes");
    let found: Vec<serde_json::Value> = orphans
        .iter()
        .map(|o| {
            serde_json::json!({
                "member": o.member,
                "path": o.db.display().to_string(),
                "id": o.id,
                "events": o.events,
            })
        })
        .collect();
    (
        human,
        serde_json::json!({ "orphans": found.len(), "deleted": false, "found": found }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A member with no agent wiring of its own is a silent failure — the
    /// agents launched there run with no Rift hooks and nothing says so — so
    /// `rift doctor` names it. **Report-only without `--fix`**: the leg lists,
    /// it does not provision, which is the same rule the phantom-gitdir and
    /// prunable-worktree legs follow.
    #[test]
    fn doctor_names_an_unwired_member_and_wires_nothing_without_fix() {
        let tmp = tempfile::tempdir().unwrap();
        let main = tmp.path().join("main");
        std::fs::create_dir_all(&main).unwrap();
        let main_git = rift_git::test_support::fake_repo(&main).unwrap();
        let member = tmp.path().join("member");
        rift_git::test_support::fake_worktree(&main_git, &member, "member").unwrap();

        let (human, json) = doctor_worktree_wiring(&main, false);

        assert!(human.contains("worktree wiring: 1 of 2"), "{human}");
        assert!(human.contains("member"), "{human}");
        assert!(human.contains("rift worktree adopt"), "{human}");
        assert_eq!(json["unwired"], 1);
        assert_eq!(json["adopted"], 0, "a report is not a repair");
        assert!(
            !member.join(".rift").exists(),
            "the leg wrote nothing into the member"
        );

        // Wired members are not findings: the leg goes quiet.
        std::fs::create_dir_all(member.join(".rift/state")).unwrap();
        std::fs::write(
            member.join(".rift/state/native-integrations.json"),
            "{\"schema_version\":1}",
        )
        .unwrap();
        let (human, json) = doctor_worktree_wiring(&main, false);
        assert_eq!(human, "", "nothing to say, nothing said");
        assert_eq!(json["unwired"], 0);
    }

    /// `--fix` adopts under adoption's own rules, and the first of those is
    /// that the family's intent comes from the projection. With no projection
    /// to read it wires **nothing**, records why in the member's adoption
    /// marker, and leaves the member exactly as it found it — guessing the
    /// enabled set would be worse than admitting it is unknown.
    #[test]
    fn doctor_fix_wires_nothing_when_the_familys_intent_cannot_be_read() {
        let tmp = tempfile::tempdir().unwrap();
        let main = tmp.path().join("main");
        std::fs::create_dir_all(&main).unwrap();
        let main_git = rift_git::test_support::fake_repo(&main).unwrap();
        let member = tmp.path().join("member");
        rift_git::test_support::fake_worktree(&main_git, &member, "member").unwrap();

        let (human, json) = doctor_worktree_wiring(&main, true);

        assert_eq!(json["unwired"], 1);
        assert_eq!(json["adopted"], 0, "{human}");
        assert!(human.contains("adopted nothing"), "{human}");
        assert!(
            !member.join(".rift/agents").exists(),
            "no descriptor is invented for an unknown intent"
        );
        let marker: serde_json::Value = serde_json::from_slice(
            &std::fs::read(member.join(".rift/state/adoption.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(marker["outcome"], "deferred");
    }

    /// The phantom-gitdir fixture. Two rules meet here: every read-only palette
    /// request reaches a real handler against an EXPLICIT repository root, never
    /// process cwd; and repo-scoped legs answer for the DISCOVERED worktree
    /// root, so a subdirectory invocation never reports a false-empty cwd (the
    /// zero-checkpoints incident class, resurfaced as a doctor UX trap).
    ///
    /// The five phantom `.git` directories this repository grew (`docs/`,
    /// `docs/adr/`, `docs/goals/`, `bins/rift/`, `crates/rift-core/src/`),
    /// re-created in a tempdir together with the three shapes the scan must
    /// NOT report: a real gitdir, a linked worktree's `.git` *file*, and a
    /// phantom under `target/`. Returns the repository root.
    fn five_phantoms_fixture(root: &Path) -> std::path::PathBuf {
        let repo = root.join("repo");
        rift_git::test_support::fake_repo(&repo).unwrap();
        for rel in [
            "docs",
            "docs/adr",
            "docs/goals",
            "bins/rift",
            "crates/rift-core/src",
        ] {
            let dir = repo.join(rel);
            std::fs::create_dir_all(&dir).unwrap();
            rift_git::test_support::phantom_gitdir(&dir).unwrap();
        }
        // `docs/` is the one that took capture: two session files.
        let sessions = repo.join("docs").join(".git").join("rift-sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        std::fs::write(sessions.join("01J.json"), "{}").unwrap();
        std::fs::write(sessions.join("01K.json"), "{}").unwrap();
        std::fs::write(sessions.join("notes.txt"), "not a session").unwrap();
        // A real member: its `.git` is a FILE, and the admin dir under the
        // repository's own `.git` is real plumbing, not a finding.
        rift_git::test_support::fake_worktree(&repo.join(".git"), &root.join("member"), "member")
            .unwrap();
        // Skipped by name, however phantom: a build directory is not evidence.
        rift_git::test_support::phantom_gitdir(&repo.join("target").join("debug")).unwrap();
        // Below the depth bound (seven levels down): out of scope by design.
        rift_git::test_support::phantom_gitdir(&repo.join("a/b/c/d/e/f/g")).unwrap();
        repo
    }

    /// The five phantoms' `.git` directories as the doctor spells them, in
    /// the sorted order it lists them. Built component by component: the
    /// doctor spells what `read_dir` walked, and `join("a/b")` keeps the `/`
    /// verbatim on Windows.
    fn phantom_gitdirs(repo: &Path) -> [String; 5] {
        let git = |rel: &str| {
            rel.split('/')
                .fold(repo.to_path_buf(), |at, part| at.join(part))
                .join(".git")
                .display()
                .to_string()
        };
        [
            git("bins/rift"),
            git("crates/rift-core/src"),
            git("docs"),
            git("docs/adr"),
            git("docs/goals"),
        ]
    }

    /// The listing the leg prints for the fixture, byte for byte.
    fn phantom_listing(repo: &Path) -> String {
        let [bins, core, docs, adr, goals] = phantom_gitdirs(repo);
        format!(
            "\n  phantom .git:   5 director(ies) named `.git` with no HEAD — not \
             repositories, and rift never deletes one\
             \n    {bins} (0 session file(s))\
             \n    {core} (0 session file(s))\
             \n    {docs} (2 session file(s))\
             \n    {adr} (0 session file(s))\
             \n    {goals} (0 session file(s))"
        )
    }

    /// The phantom-gitdir doctor leg NAMES every phantom with the capture it
    /// swallowed, sorted, and excludes the real gitdir, its member's `.git`
    /// file, `target/` and the seventh level.
    #[test]
    fn doctor_lists_every_phantom_gitdir_with_the_capture_it_took() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = five_phantoms_fixture(tmp.path());

        let (human, json) = doctor_phantom_gitdirs(&repo, false);

        assert_eq!(
            human,
            phantom_listing(&repo),
            "the five shapes, sorted, with the capture each took"
        );
        assert_eq!(json["phantoms"], 5);
        assert_eq!(json["depth"], 6);
        assert_eq!(json["deleted"], serde_json::json!(false));
        assert_eq!(json["found"][2]["sessions"], 2, "only *.json counts");
    }

    /// `--fix` never removes one: it prints the commands and leaves the
    /// decision with the owner. Byte-locked, because a line a person is
    /// expected to paste into a shell is a contract.
    #[test]
    fn doctor_fix_prints_the_removal_lines_and_removes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = five_phantoms_fixture(tmp.path());
        let listing = phantom_listing(&repo);

        let (fixed, fixed_json) = doctor_phantom_gitdirs(&repo, true);

        assert!(fixed.starts_with(&listing), "the listing is unchanged");
        let [bins, core, docs, adr, goals] = phantom_gitdirs(&repo);
        assert_eq!(
            fixed
                .strip_prefix(&listing)
                .expect("the fix block follows the listing"),
            format!(
                "\n    --fix does not remove them; to remove them by hand:\
                 \n      rm -rf '{bins}'\
                 \n      rm -rf '{core}'\
                 \n      rm -rf '{docs}'\
                 \n      rm -rf '{adr}'\
                 \n      rm -rf '{goals}'"
            )
        );
        assert_eq!(fixed_json["deleted"], serde_json::json!(false));
        for rel in [
            "docs",
            "docs/adr",
            "docs/goals",
            "bins/rift",
            "crates/rift-core/src",
        ] {
            assert!(
                repo.join(rel).join(".git").is_dir(),
                "`--fix` deleted {rel}/.git — it must only ever print"
            );
        }
        assert_eq!(
            std::fs::read_dir(repo.join("docs").join(".git").join("rift-sessions"))
                .unwrap()
                .count(),
            3,
            "nor did it touch what the phantom swallowed"
        );
    }

    /// A clean tree says nothing at all — a leg that prints "none" every run
    /// trains the reader to skip the line that matters.
    #[test]
    fn doctor_is_silent_when_no_phantom_gitdir_exists() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("clean");
        rift_git::test_support::fake_repo(&repo).unwrap();
        std::fs::create_dir_all(repo.join("src").join("deep")).unwrap();
        let (human, json) = doctor_phantom_gitdirs(&repo, true);
        assert!(human.is_empty(), "{human}");
        assert_eq!(json["phantoms"], 0);
    }
}
