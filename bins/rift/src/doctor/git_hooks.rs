//! Which hooks git will actually run, whether each still points at a binary
//! that exists, and what the family's binary of record says.
//!
//! A dangling hook is git's quietest failure — git skips it and says
//! nothing — so this leg prints its count even when every hook is fine.

use crate::hooks;

/// Which hooks git will actually run, and whether each still points at a binary
/// that exists.
///
/// **A dangling hook is git's quietest failure**: git skips it and says
/// nothing. This repository ran four days that way — no commit after
/// 2026-08-27 02:42 carried its checkpoint trailer, and nothing anywhere
/// reported an error — because an unrelated command re-pointed the family at a
/// `/tmp` binary that was then deleted. This leg is the loud version of that
/// condition, which is why it prints the count even when the answer is "all
/// five are fine".
///
/// The directory is **git's answer**, not our default: with `core.hooksPath`
/// set, `<common_dir>/hooks` can be immaculate and never run.
///
/// `--fix` is the `KeepIfHealthy` self-heal, under the same transient screen as
/// every other install — so a repair that would point the family INTO a linked
/// worktree is refused, and says which vantage can do it. That is the rule this
/// machine has been following by hand since 2026-08-30, now enforced.
pub(super) fn doctor_hooks_target(
    repo: &std::path::Path,
    fix: bool,
) -> (String, serde_json::Value) {
    doctor_hooks_target_with(repo, fix, std::env::current_exe().ok())
}

/// [`doctor_hooks_target`] with the repair binary injected: a test process's own
/// `current_exe()` is a `deps/` build artifact, which the screen refuses on
/// purpose, so the heal is asserted against a path that behaves the way a real
/// installation's does.
fn doctor_hooks_target_with(
    repo: &std::path::Path,
    fix: bool,
    rift_bin: Option<std::path::PathBuf>,
) -> (String, serde_json::Value) {
    // The self-heal order: the record moves to the running binary first, so
    // every repair after it (these hooks, then the members' manifests in the
    // worktree leg) is written from a record that already names it.
    let moved = fix
        .then(|| {
            let running = hooks::RunningBinary {
                path: rift_bin.clone()?,
                screen: rift_shadow::hooks::is_transient,
            };
            hooks::follow_running_binary(repo, &running)
        })
        .flatten();
    let (human, mut json) = hooks_target_leg(repo, fix, rift_bin);
    let Some(line) = moved else {
        return (human, json);
    };
    if let Some(map) = json.as_object_mut() {
        map.insert("record".to_owned(), serde_json::json!(line));
    }
    (format!("\n  record:         {line}{human}"), json)
}

/// The hooks leg proper, once the record has been reconciled.
fn hooks_target_leg(
    repo: &std::path::Path,
    fix: bool,
    rift_bin: Option<std::path::PathBuf>,
) -> (String, serde_json::Value) {
    let Ok(paths) = rift_git::discover(repo) else {
        return (String::new(), serde_json::json!({ "readable": false }));
    };
    let dir = hooks::effective_hooks_dir(repo).unwrap_or_else(|| paths.common_dir.join("hooks"));
    let found = hook_findings(&dir);
    if found.is_empty() {
        return (
            hooks_headline(&dir, &found),
            hooks_report(&dir, &found, 0, None),
        );
    }
    if !rift_has_been_here(&dir, &paths.common_dir, &found) {
        return (
            format!(
                "\n  hooks:          not installed at {} (`rift init` installs them)",
                dir.display()
            ),
            hooks_report(&dir, &HookFindings::default(), 0, None),
        );
    }
    let mut human = hooks_headline(&dir, &found);
    for (name, state) in found.all() {
        human.push_str(&format!("\n    {name}: {state}"));
    }
    if !fix {
        human.push_str(if found.retired.is_empty() {
            "\n    safe --fix re-points them at this binary"
        } else {
            "\n    safe --fix re-points them at this binary and removes what is retired"
        });
        return (human, hooks_report(&dir, &found, 0, None));
    }
    let Some(rift_bin) = rift_bin else {
        human.push_str(
            "\n    --fix cannot read its own path; run \
             `rift hooks install --repoint --binary <path>`",
        );
        return (human, hooks_report(&dir, &found, 0, None));
    };
    let (tail, json) = heal_hooks(repo, &paths.common_dir, &dir, rift_bin, &found);
    human.push_str(&tail);
    (human, json)
}

/// What the hooks leg found: entries that will not run, and entries Rift wrote
/// under a name it no longer installs.
///
/// Two questions with two answers, because they have different repairs — one is
/// re-pointed, the other removed — and the leg reports both in one list.
#[derive(Default)]
struct HookFindings {
    /// `(hook, why it will not run)`.
    broken: Vec<(String, String)>,
    /// `(entry, why it is a finding)`.
    retired: Vec<(String, String)>,
}

impl HookFindings {
    fn is_empty(&self) -> bool {
        self.broken.is_empty() && self.retired.is_empty()
    }

    fn all(&self) -> impl Iterator<Item = &(String, String)> {
        self.broken.iter().chain(self.retired.iter())
    }
}

/// Everything about this hooks directory a person has to know.
fn hook_findings(dir: &std::path::Path) -> HookFindings {
    HookFindings {
        broken: broken_hooks(dir),
        retired: retired_hooks(dir),
    }
}

/// The leg's first line.
///
/// It prints the count even when nothing is wrong: a dangling hook is git's
/// quietest failure, and a leg that only spoke up on failure would be one more
/// silence to interpret.
fn hooks_headline(dir: &std::path::Path, found: &HookFindings) -> String {
    let total = rift_shadow::hooks::HOOK_NAMES.len();
    if !found.broken.is_empty() {
        return format!(
            "\n  hooks:          {} of {total} will not run (git skips a broken hook SILENTLY) at {}",
            found.broken.len(),
            dir.display()
        );
    }
    if found.retired.is_empty() {
        return format!(
            "\n  hooks:          {total} of {total} live at {}",
            dir.display()
        );
    }
    format!(
        "\n  hooks:          {total} of {total} live at {}, and {} Rift no longer installs",
        dir.display(),
        found.retired.len()
    )
}

/// The leg's JSON: the directory, the counts, and every finding by name.
fn hooks_report(
    dir: &std::path::Path,
    found: &HookFindings,
    healed: usize,
    refused: Option<&str>,
) -> serde_json::Value {
    serde_json::json!({
        "hooks_dir": dir.display().to_string(),
        "hooks": rift_shadow::hooks::HOOK_NAMES.len(),
        "broken": found.broken.len(),
        "retired": found.retired.len(),
        "found": found
            .all()
            .map(|(name, state)| serde_json::json!({ "hook": name, "state": state }))
            .collect::<Vec<_>>(),
        "healed": healed,
        "refused": refused,
    })
}

/// Whether Rift has been in this hooks directory at all.
///
/// **A repository with no Rift hooks is not a finding, and `--fix` never
/// installs one.** Provisioning is `rift init`'s act; a diagnosis that wired
/// capture into a checkout nobody asked to wire would be the same class of
/// side effect this whole rule removes. Rift has been here when one of the
/// entries is managed — a dangling managed link included, and an entry
/// under a retired name too — or when the family has recorded a binary.
fn rift_has_been_here(
    dir: &std::path::Path,
    common_dir: &std::path::Path,
    found: &HookFindings,
) -> bool {
    let managed = rift_shadow::hooks::HOOK_NAMES
        .iter()
        .any(|name| rift_shadow::hooks::is_managed_hook(&dir.join(name)));
    managed
        || !found.retired.is_empty()
        || rift_shadow::hooks::read_binary_of_record(&hooks::family_sites(common_dir).dir).is_some()
}

/// The repair: the same keep-if-healthy install every other surface runs, under
/// the same transient screen — so a repair that would point the family INTO a
/// linked worktree is refused, and says which vantage can do it.
fn heal_hooks(
    repo: &std::path::Path,
    common_dir: &std::path::Path,
    dir: &std::path::Path,
    rift_bin: std::path::PathBuf,
    found: &HookFindings,
) -> (String, serde_json::Value) {
    let installed = match hooks::install_family_hooks(
        common_dir,
        repo,
        None,
        rift_bin,
        rift_shadow::hooks::HookInstallPolicy::KeepIfHealthy {
            allow_transient: false,
        },
        rift_shadow::hooks::RecordedBy::SelfHeal,
    ) {
        Ok(installed) => installed,
        Err(error) => {
            return (
                format!("\n    --fix failed ({error})"),
                hooks_report(dir, found, 0, None),
            )
        }
    };
    if let Some(refused) = installed.refused {
        return (
            format!(
                "\n    --fix REFUSED: {refused}\n    re-run it from the family's main work \
                 tree, whose binary outlives every member"
            ),
            hooks_report(dir, found, 0, Some(refused.reason.as_str())),
        );
    }
    let healed = installed.written().len();
    (
        healed_lines(&installed),
        hooks_report(dir, &hook_findings(dir), healed, None),
    )
}

/// What the repair did, in the two sentences it can be: what it re-pointed,
/// and what it removed.
fn healed_lines(installed: &rift_shadow::hooks::HookInstallReport) -> String {
    let mut lines = String::new();
    if !installed.written().is_empty() {
        lines.push_str(&format!(
            "\n    HEALED: {} hook(s) re-pointed at {} and recorded",
            installed.written().len(),
            installed.binary.as_ref().map_or_else(
                || "the running binary".to_owned(),
                |bin| bin.display().to_string()
            )
        ));
    }
    if !installed.removed.is_empty() {
        lines.push_str(&format!(
            "\n    REMOVED: {} retired hook(s): {}",
            installed.removed.len(),
            hooks::hook_names(&installed.removed)
        ));
    }
    if lines.is_empty() {
        lines.push_str("\n    --fix found nothing left to do");
    }
    lines
}

/// Every hook in the directory git runs that cannot execute, and why.
///
/// Deliberately blunt: whatever the entry is — our link, our shim, or a hook
/// somebody else put there — the only question this leg asks is whether git
/// running it would do anything at all.
fn broken_hooks(dir: &std::path::Path) -> Vec<(String, String)> {
    let mut broken = Vec::new();
    for name in rift_shadow::hooks::HOOK_NAMES {
        let path = dir.join(name);
        let Ok(metadata) = std::fs::symlink_metadata(&path) else {
            broken.push(((*name).to_owned(), "missing".to_owned()));
            continue;
        };
        if metadata.file_type().is_symlink() {
            let target = std::fs::read_link(&path).unwrap_or_default();
            if !rift_shadow::hooks::is_executable_file(&path) {
                broken.push((
                    (*name).to_owned(),
                    format!("dangling -> {}", target.display()),
                ));
            }
        } else if !rift_shadow::hooks::is_executable_file(&path) {
            broken.push(((*name).to_owned(), "not executable".to_owned()));
        }
    }
    broken
}

/// Every entry Rift wrote under a name it no longer installs.
///
/// The directory is the source here, never [`rift_shadow::hooks::HOOK_NAMES`]:
/// an entry under a name that left the list is invisible to a scan of the
/// list, and git goes on running it regardless — which is the whole finding.
fn retired_hooks(dir: &std::path::Path) -> Vec<(String, String)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut retired: Vec<(String, String)> = entries
        .flatten()
        .filter(|entry| rift_shadow::hooks::is_retired_hook(&entry.path()))
        .map(|entry| {
            (
                entry.file_name().to_string_lossy().into_owned(),
                "retired (a hook Rift no longer installs)".to_owned(),
            )
        })
        .collect();
    retired.sort();
    retired
}

/// The family's binary of record: what it names, whether that binary is still
/// there, and whether the hooks agree with it, as one closed word.
///
/// Report-only. The record is a note about the hooks, so a note that disagrees
/// with them is corrected by the next install or `--fix`, never by a reader.
pub(super) fn doctor_binary_of_record(repo: &std::path::Path) -> (String, serde_json::Value) {
    let Ok(paths) = rift_git::discover(repo) else {
        return (String::new(), serde_json::json!({ "readable": false }));
    };
    let sites = hooks::family_sites(&paths.common_dir);
    let path = rift_shadow::hooks::binary_of_record_path(&sites.dir);
    let Some(record) = rift_shadow::hooks::read_binary_of_record(&sites.dir) else {
        return (
            format!(
                "\n  binary of record: none yet ({} — the next `rift hooks install` records one)",
                path.display()
            ),
            serde_json::json!({ "recorded": false, "path": path.display().to_string() }),
        );
    };
    let dir = hooks::effective_hooks_dir(repo).unwrap_or_else(|| paths.common_dir.join("hooks"));
    let elsewhere: Vec<String> = rift_shadow::hooks::HOOK_NAMES
        .iter()
        .filter_map(|name| std::fs::read_link(dir.join(name)).ok())
        .filter(|target| target != record.path())
        .map(|target| target.display().to_string())
        .collect();
    let state = record_state(&record, &sites, elsewhere.is_empty());
    let mut human = format!(
        "\n  binary of record: {state}: {} (by {}, {} superseded)",
        record.rift_bin,
        record.recorded_by,
        record.history.len(),
    );
    if state != "agrees" {
        human.push_str(&format!("\n    {}", state_remedy(state)));
    }
    if !elsewhere.is_empty() {
        human.push_str(&format!(
            "\n    {} hook(s) run {}",
            elsewhere.len(),
            elsewhere.first().map_or("?", String::as_str)
        ));
    }
    (
        human,
        serde_json::json!({
            "recorded": true,
            "path": path.display().to_string(),
            "rift_bin": record.rift_bin,
            "recorded_by": record.recorded_by.as_str(),
            "state": state,
            "live": record.is_live(),
            "superseded": record.history.len(),
            "hooks_elsewhere": elsewhere.len(),
        }),
    )
}

/// The record's state in one closed word, worst first: `missing` (the binary
/// is gone), `debug` (a debug-profile build), `throwaway` (a temp, `deps/` or
/// member build), `diverges` (the hooks run another binary), else `agrees`.
///
/// A reader that wants more than the word reads the other fields; the word is
/// closed so a script can switch on it.
fn record_state(
    record: &rift_shadow::hooks::BinaryOfRecord,
    sites: &rift_shadow::hooks::FamilySites,
    hooks_agree: bool,
) -> &'static str {
    if !record.is_live() {
        return "missing";
    }
    if hooks::is_debug_build(record.path()) {
        return "debug";
    }
    if rift_shadow::hooks::is_transient(record.path(), sites).is_some() {
        return "throwaway";
    }
    if hooks_agree {
        "agrees"
    } else {
        "diverges"
    }
}

/// What to run for a record that does not agree.
fn state_remedy(state: &str) -> &'static str {
    match state {
        "diverges" => "`rift doctor --fix` from the binary the family should run moves the record and hooks together",
        _ => "run `rift doctor --fix` or `rift enable` from a release binary; it moves the record and hooks together",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The done-when, as a fixture: `rift doctor` NAMES a dangling
    /// hook — the condition git reports by doing nothing — and `--fix` heals it
    /// and records the binary it healed to.
    ///
    /// `/bin/echo` stands in for a built `rift`: an absolute, executable path
    /// that is not under a temp root, not a `deps/` artifact and not inside a
    /// linked worktree, which is what the screen requires and what a tempdir
    /// can never be. Nothing here runs it.
    #[cfg(unix)]
    #[test]
    fn doctor_names_a_dangling_hook_and_fix_heals_it() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("repo");
        let git_dir = rift_git::test_support::fake_repo(&root).unwrap();
        let hooks_dir = git_dir.join("hooks");
        std::fs::create_dir_all(&hooks_dir).unwrap();
        let deleted = tmp.path().join("rift-measure-run/rift");
        for name in rift_shadow::hooks::HOOK_NAMES {
            std::os::unix::fs::symlink(&deleted, hooks_dir.join(name)).unwrap();
        }
        let stand_in = std::path::PathBuf::from("/bin/echo");
        assert!(rift_shadow::hooks::is_executable_file(&stand_in));

        let (human, json) = doctor_hooks_target_with(&root, false, Some(stand_in.clone()));
        assert!(human.contains("will not run"), "{human}");
        assert!(human.contains("dangling"), "{human}");
        assert_eq!(json["broken"], rift_shadow::hooks::HOOK_NAMES.len());
        assert_eq!(json["healed"], 0, "a report is not a repair");
        let (record_human, record_json) = doctor_binary_of_record(&root);
        assert!(record_human.contains("none yet"), "{record_human}");
        assert_eq!(record_json["recorded"], false);

        let (human, json) = doctor_hooks_target_with(&root, true, Some(stand_in.clone()));
        assert!(human.contains("HEALED"), "{human}");
        assert_eq!(json["healed"], rift_shadow::hooks::HOOK_NAMES.len());
        for name in rift_shadow::hooks::HOOK_NAMES {
            assert_eq!(
                std::fs::read_link(hooks_dir.join(name)).unwrap(),
                stand_in,
                "{name} was re-pointed at a binary that exists"
            );
        }
        assert!(broken_hooks(&hooks_dir).is_empty(), "nothing left broken");

        let (record_human, record_json) = doctor_binary_of_record(&root);
        assert_eq!(record_json["rift_bin"], "/bin/echo");
        assert_eq!(record_json["recorded_by"], "self-heal");
        assert_eq!(record_json["live"], true);
        assert_eq!(record_json["hooks_elsewhere"], 0, "{record_human}");
    }

    /// **A diagnosis never provisions.** A repository with no Rift hooks is not
    /// broken, it is not set up — and `rift doctor --fix` wiring capture into a
    /// checkout nobody asked to wire would be the same class of side effect
    /// this rule exists to remove.
    #[test]
    fn a_doctor_fix_never_installs_hooks_into_a_repository_that_has_none() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("repo");
        let git_dir = rift_git::test_support::fake_repo(&root).unwrap();
        let hooks_dir = git_dir.join("hooks");
        std::fs::create_dir_all(&hooks_dir).unwrap();

        let (human, json) =
            doctor_hooks_target_with(&root, true, Some(std::path::PathBuf::from("/bin/echo")));

        assert!(human.contains("not installed"), "{human}");
        assert_eq!(json["broken"], 0);
        assert_eq!(json["healed"], 0);
        assert_eq!(
            std::fs::read_dir(&hooks_dir).unwrap().count(),
            0,
            "--fix wrote nothing into a repository that never asked for hooks"
        );
    }

    /// The screen the repair itself is held to: a `--fix` whose only candidate
    /// is a build artifact re-points NOTHING and says where the repair belongs.
    /// This is the arm every `cargo test` process takes, and the arm a member's
    /// own `target/debug/rift` takes on this machine.
    #[cfg(unix)]
    #[test]
    fn a_doctor_fix_refuses_to_point_the_family_at_a_transient_binary() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("repo");
        let git_dir = rift_git::test_support::fake_repo(&root).unwrap();
        let hooks_dir = git_dir.join("hooks");
        std::fs::create_dir_all(&hooks_dir).unwrap();
        let deleted = tmp.path().join("gone/rift");
        std::os::unix::fs::symlink(&deleted, hooks_dir.join("post-commit")).unwrap();

        // The test binary's own path: `<target>/debug/deps/rift-<hash>` on
        // the classic layout, `<target>/<...>/debug/build/rift/<hash>/out/
        // rift-<hash>` on the 1.100-era build-dir layout — both screened.
        let (human, json) = doctor_hooks_target_with(&root, true, std::env::current_exe().ok());
        assert!(human.contains("--fix REFUSED"), "{human}");
        assert!(human.contains("main work tree"), "{human}");
        assert_eq!(json["refused"], "build-artifact");
        assert_eq!(
            std::fs::read_link(hooks_dir.join("post-commit")).unwrap(),
            deleted,
            "a refusal changes nothing"
        );
    }

    /// A name that left the dispatch table leaves the directory. The entry is
    /// still a link to the binary, so git still runs it — on every merge and
    /// every step of a rebase — and the binary answers a name it cannot route
    /// with a usage error. The leg names it, and `--fix` is what removes it.
    #[cfg(unix)]
    #[test]
    fn doctor_names_an_entry_rift_no_longer_installs_and_fix_removes_it() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("repo");
        let git_dir = rift_git::test_support::fake_repo(&root).unwrap();
        let stand_in = std::path::PathBuf::from("/bin/echo");
        let hooks_dir = install_five_hooks(&git_dir, &root, &stand_in);
        // The shape a family provisioned before the list shrank carries: a
        // managed entry under a name nothing routes any more.
        let retired = hooks_dir.join("post-merge");
        std::os::unix::fs::symlink(tmp.path().join("gone/rift"), &retired).unwrap();

        let (human, json) = doctor_hooks_target_with(&root, false, Some(stand_in.clone()));
        assert!(
            human.contains("post-merge: retired (a hook Rift no longer installs)"),
            "{human}"
        );
        assert_eq!(json["broken"], 0, "the hooks Rift installs are all fine");
        assert_eq!(json["retired"], 1);
        assert_eq!(json["healed"], 0, "a report is not a repair");
        assert!(std::fs::symlink_metadata(&retired).is_ok());

        let (human, json) = doctor_hooks_target_with(&root, true, Some(stand_in));
        assert!(human.contains("REMOVED: 1"), "{human}");
        assert_eq!(json["retired"], 0, "the second reading is clean");
        assert!(
            std::fs::symlink_metadata(&retired).is_err(),
            "--fix removed it"
        );
    }

    /// The leg reports Rift's entries, not the directory's. A `post-merge` of
    /// the user's own is not a finding and `--fix` never touches it.
    #[test]
    fn a_hook_rift_did_not_write_under_a_retired_name_is_not_a_finding() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("repo");
        let git_dir = rift_git::test_support::fake_repo(&root).unwrap();
        let stand_in = std::path::PathBuf::from("/bin/echo");
        let hooks_dir = install_five_hooks(&git_dir, &root, &stand_in);
        let theirs = hooks_dir.join("post-merge");
        std::fs::write(&theirs, "#!/bin/sh\necho theirs\n").unwrap();

        let (human, json) = doctor_hooks_target_with(&root, true, Some(stand_in));

        assert_eq!(json["broken"], 0);
        assert_eq!(json["retired"], 0);
        assert!(!human.contains("post-merge"), "{human}");
        assert!(std::fs::read_to_string(&theirs)
            .unwrap()
            .contains("echo theirs"));
    }

    /// A record the hooks disagree with is named `diverges`, and the report
    /// changes no file: the record and every hook read the same afterwards.
    #[cfg(unix)]
    #[test]
    fn doctor_names_a_record_the_hooks_diverge_from_and_changes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("repo");
        let git_dir = rift_git::test_support::fake_repo(&root).unwrap();
        let hooks_dir = install_five_hooks(&git_dir, &root, std::path::Path::new("/bin/echo"));
        let first = hooks_dir.join(rift_shadow::hooks::HOOK_NAMES[0]);
        std::fs::remove_file(&first).unwrap();
        std::os::unix::fs::symlink("/bin/cat", &first).unwrap();
        let record_file =
            rift_shadow::hooks::binary_of_record_path(&hooks::family_sites(&git_dir).dir);
        let before = std::fs::read(&record_file).unwrap();

        let (human, json) = doctor_binary_of_record(&root);
        let (_, _) = doctor_hooks_target_with(&root, false, Some("/bin/sh".into()));

        assert_eq!(json["state"], "diverges", "{human}");
        assert!(human.contains("binary of record: diverges"), "{human}");
        assert_eq!(std::fs::read(&record_file).unwrap(), before);
        assert_eq!(
            std::fs::read_link(&first).unwrap(),
            std::path::Path::new("/bin/cat")
        );
    }

    /// A record naming a binary that is gone, or a debug build, gets its own
    /// closed word.
    #[cfg(unix)]
    #[test]
    fn doctor_names_a_missing_or_debug_record_in_one_word() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("repo");
        let git_dir = rift_git::test_support::fake_repo(&root).unwrap();
        let debug = tmp.path().join("checkout/target/debug/rift");
        std::fs::create_dir_all(debug.parent().unwrap()).unwrap();
        std::fs::copy("/bin/echo", &debug).unwrap();
        let gone = tmp.path().join("gone/rift");
        for (bin, word) in [(&debug, "debug"), (&gone, "missing")] {
            let policy = rift_shadow::hooks::HookInstallPolicy::Repoint {
                allow_transient: true,
            };
            let by = rift_shadow::hooks::RecordedBy::HooksInstall;
            hooks::install_family_hooks(&git_dir, &root, None, bin.clone(), policy, by).unwrap();

            let (human, json) = doctor_binary_of_record(&root);

            assert_eq!(json["state"], word, "{human}");
        }
    }

    /// `--fix` from a release binary other than the record's moves the record
    /// first and reports it before anything the hooks leg repaired.
    #[cfg(unix)]
    #[test]
    fn doctor_fix_moves_the_record_before_the_hooks_and_says_so_first() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("repo");
        let git_dir = rift_git::test_support::fake_repo(&root).unwrap();
        let hooks_dir = install_five_hooks(&git_dir, &root, std::path::Path::new("/bin/echo"));

        let (human, json) = doctor_hooks_target_with(&root, true, Some("/bin/cat".into()));

        let record = human
            .find("moved from /bin/echo to /bin/cat")
            .expect(&human);
        let hooks_line = human.find("\n  hooks:").expect(&human);
        assert!(record < hooks_line, "{human}");
        assert!(json["record"].as_str().is_some());
        let (_, record_json) = doctor_binary_of_record(&root);
        assert_eq!(record_json["rift_bin"], "/bin/cat");
        assert_eq!(record_json["superseded"], 1);
        assert_eq!(record_json["state"], "agrees");
        let post_commit = std::fs::read_link(hooks_dir.join("post-commit")).unwrap();
        assert_eq!(post_commit, std::path::Path::new("/bin/cat"));
    }

    /// The five hooks as the installer writes them, so a doctor fixture starts
    /// from a family that is genuinely set up. Returns the hooks directory.
    fn install_five_hooks(
        git_dir: &std::path::Path,
        root: &std::path::Path,
        rift_bin: &std::path::Path,
    ) -> std::path::PathBuf {
        let report = hooks::install_family_hooks(
            git_dir,
            root,
            None,
            rift_bin.to_path_buf(),
            rift_shadow::hooks::HookInstallPolicy::Repoint {
                allow_transient: false,
            },
            rift_shadow::hooks::RecordedBy::HooksInstall,
        )
        .unwrap();
        assert_eq!(report.repointed.len(), rift_shadow::hooks::HOOK_NAMES.len());
        report.hooks_dir
    }
}
