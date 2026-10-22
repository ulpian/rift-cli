//! What the repository's setup says: a policy key spelled the retired way,
//! another tool's hook on the same calls Rift answers, and the memory chain
//! from checkpoints to the daemon.

use std::path::{Path, PathBuf};

use crate::family;
use crate::launch::home_dir_from;
use crate::projection::runtime;

/// The policy keys this repository still spells the way a past release did.
///
/// A renamed config key is the one deprecation a repository cannot notice for
/// itself: the old spelling parses, hashes identically to the new one and has
/// no runtime effect, so every run looks ordinary right up to the release that
/// stops accepting it and turns a silent no-op into a config that will not
/// load. `rift doctor` is the surface whose whole job is to tell you about your
/// setup, so it is where that sentence belongs — and, like the other legs, it
/// is silent when there is nothing to say: the current spelling and a
/// repository that never named the key at all both print nothing.
///
/// Reads the policy file and nothing else, opens no store, and fixes nothing —
/// the fix is one word in a file Rift does not own, so naming it is the whole
/// of what this can honestly do. A config that will not load says nothing
/// either: every other leg already reports that repository's real problem, and
/// a second complaint about a file that is already broken is noise.
pub(super) fn doctor_retired_config_keys(repo: &Path) -> (String, serde_json::Value) {
    let silent = || {
        (
            String::new(),
            serde_json::json!({ "notice": serde_json::Value::Null }),
        )
    };
    let Ok(cfg) = rift_config::EffectiveConfig::load(&repo.join(".rift")) else {
        return silent();
    };
    let Some(notice) = cfg.retired_feature_key_notice() else {
        return silent();
    };
    (
        format!("\n  retired key:    {notice}"),
        serde_json::json!({ "notice": notice }),
    )
}

/// A project config that is present and not applied: one that does not load,
/// or one Rift refused to read. Every other surface degrades to the defaults
/// quietly, so this is where a developer learns their policy is not running.
/// The load error is never printed: a TOML parse error quotes the line it
/// failed on.
pub(super) fn doctor_config_load(repo: &Path) -> (String, serde_json::Value) {
    let reason = match rift_config::EffectiveConfig::load(&repo.join(".rift")) {
        Err(_) => ".rift/config.toml does not load",
        Ok(config) => match config.policy_source().diagnostic() {
            Some(diagnostic) => diagnostic,
            None => {
                return (
                    String::new(),
                    serde_json::json!({ "notice": serde_json::Value::Null }),
                )
            }
        },
    };
    let notice = format!(
        "{reason}; the declared policy is not applied and raw transcripts are off until it is repaired"
    );
    (
        format!("\n  config:         {notice}"),
        serde_json::json!({ "notice": notice }),
    )
}

/// Where the setup wizard keeps its plan while setup runs, relative to the
/// checkout.
const SETUP_PLAN: &str = ".rift/state/project-setup.json";

/// A setup plan left beside a checkout whose family is set up on this
/// machine: a crash before setup removed it, a failed remove, or a copied
/// checkout.
///
/// The plan decides no status, so nothing else would ever mention it; this
/// leg is where it is named. Silent when there is no plan, and silent for a
/// plan still pending, which the dashboard already shows as setup left to do.
/// Report-only: the plan is in the person's checkout and the leg writes
/// nowhere.
pub(super) fn doctor_stale_setup_plan(repo: &Path) -> (String, serde_json::Value) {
    doctor_stale_setup_plan_under(&family::cache_root(), repo)
}

fn doctor_stale_setup_plan_under(cache_root: &Path, repo: &Path) -> (String, serde_json::Value) {
    use rift_canopy_data::bus::SetupPlanMarker;
    match rift_canopy_data::bus::setup_plan_marker_under(cache_root, repo) {
        SetupPlanMarker::Stale => {
            let remedy = rift_agent_install::health::stale_setup_plan_sentence(SETUP_PLAN);
            (
                format!("\n  setup plan:     stale; {remedy}"),
                serde_json::json!({ "state": "stale", "path": SETUP_PLAN, "remedy": remedy }),
            )
        }
        SetupPlanMarker::Pending => (
            String::new(),
            serde_json::json!({ "state": "pending", "path": SETUP_PLAN, "remedy": null }),
        ),
        SetupPlanMarker::Absent => (
            String::new(),
            serde_json::json!({ "state": "absent", "path": null, "remedy": null }),
        ),
    }
}

/// The two files Rift's own native installer writes and reads a Claude Code
/// hook table into, relative to a repository.
///
/// Spelled here because this leg reads them for a different reason than the
/// installer writes them, and because what it looks for is what somebody
/// *else* put there. A hook configured anywhere this list does not name — a
/// plugin's own table, a machine-wide policy file — is outside what this can
/// see, and the notice says so rather than implying the list is exhaustive.
const CLAUDE_SETTINGS_FILES: [&str; 2] = [".claude/settings.json", ".claude/settings.local.json"];

/// Another `PreToolUse` hook that could answer the same tool calls Rift does.
///
/// One tool call carries at most one amendment: a client applies the last one
/// it is handed, so a sibling hook that amends a `Read` or a `Grep` silently
/// replaces Rift's. That is not a fault in either hook and Rift does not fight
/// over the channel — it verifies its own amendment at the call's result and
/// stands down for that window — but it is the one setup fact that explains
/// why documents are being converted and never substituted, and why searches
/// are never bounded, and a repository cannot notice either for itself.
///
/// The screen errs towards saying something. A matcher's exact language is the
/// client's to define, so anything that is absent, is the catch-all, or names
/// either tool counts — a superset, which over-warns and never under-warns.
/// Rift's own entries are recognized by the command they invoke and never
/// reported. Read-only, fixes nothing, opens no store.
pub(super) fn doctor_sibling_tool_hooks(repo: &Path) -> (String, serde_json::Value) {
    let home = home_dir_from(|key| std::env::var(key).ok()).map(PathBuf::from);
    doctor_sibling_tool_hooks_in(repo, home.as_deref())
}

/// The same leg against an explicit home, which is what a test can drive.
fn doctor_sibling_tool_hooks_in(repo: &Path, home: Option<&Path>) -> (String, serde_json::Value) {
    let mut files: Vec<PathBuf> = CLAUDE_SETTINGS_FILES
        .iter()
        .map(|name| repo.join(name))
        .collect();
    if let Some(home) = home {
        files.push(home.join(CLAUDE_SETTINGS_FILES[0]));
    }
    let found: Vec<serde_json::Value> = files
        .iter()
        .filter_map(|path| {
            let handlers = sibling_tool_handlers(path);
            (handlers > 0).then(
                || serde_json::json!({ "file": path.display().to_string(), "hooks": handlers }),
            )
        })
        .collect();
    if found.is_empty() {
        return (String::new(), serde_json::json!({ "siblings": [] }));
    }
    let mut human = String::from(
        "\n  tool hooks:     another PreToolUse hook can answer the same calls Rift does",
    );
    for entry in &found {
        human.push_str(&format!(
            "\n    {} — {} handler(s)",
            entry["file"].as_str().unwrap_or_default(),
            entry["hooks"]
        ));
    }
    human.push_str(
        "\n    a tool call carries one amendment, so a document substitution or a \
         search's result cap made here may be dropped; `[doc] channel = \"redirect\"` \
         answers a document read without one, and a dropped cap is recorded and never \
         becomes a decline — a content search is answered by no index. Hooks \
         configured elsewhere are not visible to this check.",
    );
    (human, serde_json::json!({ "siblings": found }))
}

/// How many handlers in this settings file's `PreToolUse` table are somebody
/// else's and could match a read or a search.
///
/// Every failure counts none: a file that is absent, unreadable, not JSON, or
/// shaped differently than the table Rift writes says nothing about anyone's
/// hooks, and a notice invented out of one of those would be worse than
/// silence.
fn sibling_tool_handlers(path: &Path) -> usize {
    let Ok(bytes) = std::fs::read(path) else {
        return 0;
    };
    let Ok(settings) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return 0;
    };
    let Some(groups) = settings
        .pointer("/hooks/PreToolUse")
        .and_then(|v| v.as_array())
    else {
        return 0;
    };
    // The invocation Rift's own installer writes for this family. A handler
    // that runs it is Rift's, wherever the entry came from.
    let ours = format!("\" hooks {} ", rift_core::AgentId::CLAUDE_CODE.as_str());
    groups
        .iter()
        .filter(|group| {
            group
                .get("matcher")
                .and_then(serde_json::Value::as_str)
                .is_none_or(|matcher| {
                    matcher.contains('*') || matcher.contains("Read") || matcher.contains("Grep")
                })
        })
        .filter_map(|group| group.get("hooks").and_then(|v| v.as_array()))
        .flatten()
        .filter(|handler| {
            handler
                .get("command")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|command| !command.contains(&ours))
        })
        .count()
}

/// The memory-chain readout: checkpoints found →
/// recall projection ingested → native client integrations provisioned → daemon servable —
/// each broken leg naming its exact next command, so an empty memory panel is
/// a diagnosis rather than a shrug.
///
/// **Three of the four legs are the FAMILY's, and one is this checkout's** —
/// worth stating, because the readout
/// predates the family and a reader would otherwise have to work out which
/// number moves when they change worktree:
///
/// * *checkpoints* come off the substrate, and `refs/heads/rift/checkpoints/v1`
///   lives in the common dir: every member of the family reads the same count,
///   which is why a member reporting `0` was always evidence of a broken
///   identity rather than of an empty repository (RESEARCH §1.4 measured
///   exactly that: 0 from a side worktree while the family's refs held 134);
/// * *recall ingest* opens `recall_db`, resolved through the family manifest,
///   so it is one file for the repository;
/// * *integrations* are per checkout on purpose: hook wiring is written into
///   the working tree a client is launched from (per-worktree state stays per
///   worktree), so `0/9` in a fresh member is a true statement about
///   that member — the `worktree-wiring` leg is what will name it as such;
/// * the *daemon* is the machine's.
pub(super) fn doctor_memory_chain(
    repo: &Path,
    strict_read_only: bool,
) -> (String, serde_json::Value) {
    let mut human = String::from("\n  memory chain:");
    let checkpoints = substrate_checkpoints(repo);
    human.push_str(&checkpoints_line(checkpoints));
    let recall_enabled =
        rift_config::EffectiveConfig::load(&repo.join(".rift")).is_ok_and(|c| c.recall_enabled());
    let ingested = ingested_reads(repo, strict_read_only);
    human.push_str(&recall_ingest_line(recall_enabled, ingested));
    let statuses = rift_agent_install::native::diagnose_all(repo);
    human.push_str(&integrations_lines(&statuses));
    let memd = resolve_memd_binary();
    human.push_str(&daemon_line(memd.as_deref()));
    let json = serde_json::json!({
        "checkpoints": checkpoints,
        "recall_enabled": recall_enabled,
        "ingested_reads": ingested,
        "integrations": crate::recall::integration_rows(repo, &statuses),
        "memd": memd.map(|p| p.display().to_string()),
    });
    (human, json)
}

/// Leg 1: checkpoints on the substrate — zero when it cannot be opened.
fn substrate_checkpoints(repo: &Path) -> usize {
    use rift_recall_substrate::{GixSubstrate, SubstrateReader as _};
    GixSubstrate::open(repo)
        .and_then(|sub| sub.checkpoints())
        .map_or(0, |c| c.len())
}

fn checkpoints_line(checkpoints: usize) -> String {
    if checkpoints == 0 {
        return "\n    checkpoints:    none — commit while a session is live \
                (the post-commit hook archives)"
            .to_owned();
    }
    format!("\n    checkpoints:    {checkpoints} found")
}

/// Leg 2: how many reads the recall projection holds — `None` when there is
/// no projection to ask, or it will not open.
fn ingested_reads(repo: &Path, strict_read_only: bool) -> Option<i64> {
    use rift_store::CanonicalStore as _;
    let db = family::recall_db(repo).ok().filter(|db| db.exists())?;
    let store = if strict_read_only {
        rift_store::SqliteStore::open_read_only(&db).ok()?
    } else {
        rift_store::open_migrated(&db, &rift_store::recall_migrations()).ok()?
    };
    let rt = runtime().ok()?;
    rt.block_on(store.query("SELECT COUNT(*) FROM n_file_read", &[]))
        .ok()
        .and_then(|rows| rows.first().and_then(|r| r.get_i64(0).ok()))
}

fn recall_ingest_line(recall_enabled: bool, ingested: Option<i64>) -> String {
    match (recall_enabled, ingested) {
        (false, _) => "\n    recall ingest:  off — enable it in Project Settings".to_owned(),
        (true, None) => "\n    recall ingest:  not ingested — launch `rift` here, or run \
                         `rift memory cost-hotspots`"
            .to_owned(),
        (true, Some(0)) => "\n    recall ingest:  projection empty — checkpoints carry no read \
                            facts yet; commit with a live session"
            .to_owned(),
        (true, Some(n)) => format!("\n    recall ingest:  {n} read(s) ingested"),
    }
}

/// Leg 3: every adapter in registry order, rendered by the table
/// `rift recall status` prints — wiring, the installer's one availability
/// sentence (`present via …`) and Codex's trust with its remedy — so doctor,
/// that table and Project Settings cannot name different reasons for the same
/// row. An adapter that is merely not wired gets no line beyond its row; an
/// action follows only a file Rift cannot manage as written.
fn integrations_lines(statuses: &[rift_agent_install::native::NativeIntegrationStatus]) -> String {
    let wired = statuses
        .iter()
        .filter(|status| crate::recall::is_wired(status))
        .count();
    let mut table = String::new();
    crate::recall::render_status(&mut table, statuses);
    let actions = statuses
        .iter()
        .filter_map(fault_action)
        .map(|action| format!("  action: {action}"));
    let mut human = format!("\n    integrations:   {wired}/{} wired", statuses.len());
    for line in table.lines().map(str::to_owned).chain(actions) {
        human.push_str("\n    ");
        human.push_str(&line);
    }
    human
}

/// The repair a conflicting or malformed descriptor or hook file calls for.
/// Codex's skipped hooks are not here: their remedy is already printed under
/// the trust line the table ends with.
fn fault_action(status: &rift_agent_install::native::NativeIntegrationStatus) -> Option<String> {
    use rift_agent_install::native::{NativeHookWiring, NativeWiring};

    let name = &status.display_name;
    match (status.wiring, status.hook_wiring) {
        (NativeWiring::Conflict | NativeWiring::Invalid, _) => {
            Some(format!("repair {name} descriptor"))
        }
        (_, Some(NativeHookWiring::Conflict | NativeHookWiring::Invalid)) => {
            Some(format!("repair {name} hook configuration"))
        }
        _ => None,
    }
}

/// Leg 4: the daemon binary.
fn daemon_line(memd: Option<&Path>) -> String {
    match memd {
        Some(p) => format!("\n    daemon:         rift-memd at {}", p.display()),
        None => "\n    daemon:         rift-memd NOT found — build it \
                 (`cargo build -p rift-memd`) or put it on PATH"
            .to_owned(),
    }
}

/// Resolve the rift-memd binary: beside the current executable first (the
/// cargo/dist layout), then `PATH`.
fn resolve_memd_binary() -> Option<PathBuf> {
    let name = if cfg!(windows) {
        "rift-memd.exe"
    } else {
        "rift-memd"
    };
    if let Ok(exe) = std::env::current_exe() {
        // Beside the executable (the dist layout), then one level up (a test
        // harness runs from target/debug/deps/ while memd sits in debug/).
        for dir in exe
            .parent()
            .into_iter()
            .chain(exe.parent().and_then(Path::parent))
        {
            let candidate = dir.join(name);
            if candidate.exists() {
                return Some(candidate);
            }
        }
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join(name))
        .find(|c| c.exists())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A repository whose `.claude/settings.local.json` is `settings`.
    fn claude_settings_repo(tmp: &Path, name: &str, settings: &serde_json::Value) -> PathBuf {
        let repo = tmp.join(name);
        std::fs::create_dir_all(repo.join(".claude")).unwrap();
        std::fs::write(
            repo.join(".claude/settings.local.json"),
            settings.to_string(),
        )
        .unwrap();
        repo
    }

    /// A `PreToolUse` table holding these matcher groups.
    fn pre_tool_use(groups: serde_json::Value) -> serde_json::Value {
        serde_json::json!({ "hooks": { "PreToolUse": groups } })
    }

    /// The handler Rift's own installer writes for this family.
    fn ours() -> serde_json::Value {
        serde_json::json!({
            "type": "command",
            "command": "\"/opt/rift\" hooks claude-code PreToolUse",
        })
    }

    /// Somebody else's handler.
    fn theirs() -> serde_json::Value {
        serde_json::json!({ "type": "command", "command": "my-linter --pre" })
    }

    /// Rift's own entry is recognized by the command it runs and never
    /// reported as a stranger's: its catch-all entry, alone, says nothing.
    #[test]
    fn rifts_own_pre_tool_use_entry_is_not_a_sibling() {
        let tmp = tempfile::tempdir().unwrap();
        let mine = claude_settings_repo(
            tmp.path(),
            "mine",
            &pre_tool_use(serde_json::json!([{ "matcher": "*", "hooks": [ours()] }])),
        );
        let (human, json) = doctor_sibling_tool_hooks_in(&mine, None);
        assert!(human.is_empty(), "{human}");
        assert_eq!(json["siblings"].as_array().map(Vec::len), Some(0));
    }

    /// A stranger's hook on a matcher that names neither tool is not one this
    /// leg can be about.
    #[test]
    fn a_hook_on_a_matcher_naming_neither_tool_is_not_a_finding() {
        let tmp = tempfile::tempdir().unwrap();
        let elsewhere = claude_settings_repo(
            tmp.path(),
            "elsewhere",
            &pre_tool_use(serde_json::json!([{ "matcher": "Bash", "hooks": [theirs()] }])),
        );
        assert!(doctor_sibling_tool_hooks_in(&elsewhere, None).0.is_empty());
    }

    /// The one setup fact that explains a document being converted and never
    /// substituted: somebody else's hook answers the same call, and a client
    /// keeps the last amendment it is handed. It is named once, with the file
    /// to look in and what to do instead — and without claiming to have
    /// looked everywhere.
    #[test]
    fn a_sibling_hook_that_can_answer_a_read_is_named_once_with_its_file() {
        let tmp = tempfile::tempdir().unwrap();
        let shared = claude_settings_repo(
            tmp.path(),
            "shared",
            &pre_tool_use(serde_json::json!([
                { "matcher": "*", "hooks": [ours()] },
                { "matcher": "Read|Grep", "hooks": [theirs()] }
            ])),
        );
        let (human, json) = doctor_sibling_tool_hooks_in(&shared, None);
        assert!(human.contains("tool hooks:"), "{human}");
        assert!(
            human.contains(".claude/settings.local.json"),
            "names the file to look in: {human}"
        );
        assert!(human.contains("redirect"), "names the way out: {human}");
        assert!(
            human.contains("result cap"),
            "and says what else this costs: a search's bound is the other amendment \
             a sibling hook displaces: {human}"
        );
        assert!(
            human.contains("elsewhere are not visible"),
            "and does not claim to have looked everywhere: {human}"
        );
        let siblings = json["siblings"].as_array().expect("one file");
        assert_eq!(siblings.len(), 1, "{json}");
        assert_eq!(siblings[0]["hooks"], 1, "Rift's own is not a stranger");
    }

    /// The operator's own file counts too, and nothing at all is silent.
    #[test]
    fn the_operators_own_settings_file_counts_and_nothing_at_all_is_silent() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        std::fs::write(
            home.join(".claude/settings.json"),
            pre_tool_use(serde_json::json!([
                { "hooks": [{ "type": "command", "command": "watchdog" }] }
            ]))
            .to_string(),
        )
        .unwrap();
        let bare = tmp.path().join("bare");
        std::fs::create_dir_all(&bare).unwrap();
        assert!(doctor_sibling_tool_hooks_in(&bare, None).0.is_empty());
        let (human, json) = doctor_sibling_tool_hooks_in(&bare, Some(&home));
        assert!(human.contains("tool hooks:"), "{human}");
        assert_eq!(json["siblings"].as_array().map(Vec::len), Some(1), "{json}");
    }

    /// Nothing this file says is readable as a hook table: silence, never a
    /// notice invented out of a parse failure.
    #[test]
    fn an_unreadable_hook_table_says_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let broken = claude_settings_repo(
            tmp.path(),
            "broken",
            &serde_json::json!({ "hooks": "not a table" }),
        );
        std::fs::write(broken.join(".claude/settings.json"), "{ not json").unwrap();
        assert!(doctor_sibling_tool_hooks_in(&broken, None).0.is_empty());
    }

    /// A repository still spelling the search feature key the retired way is
    /// told, by name, what to rename it to and in which file — and a
    /// repository using the current spelling, or naming no feature key at all,
    /// hears nothing. The silence half is the load-bearing half: a leg that
    /// prints on every run is a leg readers learn to skip, and this one only
    /// ever has something to say to the repositories that predate the rename.
    #[test]
    fn doctor_asks_for_the_retired_feature_key_rename_and_is_silent_otherwise() {
        let tmp = tempfile::tempdir().unwrap();
        let write = |name: &str, features: &str| {
            let repo = tmp.path().join(name);
            std::fs::create_dir_all(repo.join(".rift")).unwrap();
            std::fs::write(
                repo.join(".rift").join("config.toml"),
                format!("schema_version = 1\n[project]\nname = \"t\"\n{features}"),
            )
            .unwrap();
            repo
        };

        let retired = write("retired", "[features]\nfff = true\n");
        let (human, json) = doctor_retired_config_keys(&retired);
        assert!(human.contains("retired key:"), "{human}");
        assert!(human.contains("fff"), "names the key to rename: {human}");
        assert!(human.contains("search"), "names the new key: {human}");
        assert!(
            human.contains(".rift/config.toml"),
            "names the file to edit: {human}"
        );
        // The JSON carries the same sentence, so `--json` consumers are not a
        // second, drifting copy of the wording.
        let machine = json["notice"].as_str().expect("a notice was printed");
        assert!(human.contains(machine), "{human} :: {machine}");

        for (name, features) in [
            ("current", "[features]\nsearch = true\n"),
            ("recall", "[features]\ntotal_recall = true\n"),
            ("bare", ""),
        ] {
            let repo = write(name, features);
            let (human, json) = doctor_retired_config_keys(&repo);
            assert!(human.is_empty(), "{name} should be silent: {human}");
            assert_eq!(json["notice"], serde_json::Value::Null, "{name}");
        }

        // A policy file that will not load is another leg's finding, not this
        // one's: this leg never speculates about a config it could not read.
        let broken = write("broken", "[features]\nnot_a_key = true\n");
        let (human, json) = doctor_retired_config_keys(&broken);
        assert!(human.is_empty(), "{human}");
        assert_eq!(json["notice"], serde_json::Value::Null);
    }

    /// A config that is present but not applied is named, with what that
    /// costs, and never by quoting the file; a config that loads, or none at
    /// all, says nothing.
    #[test]
    fn doctor_names_a_config_that_does_not_load_or_was_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let repo_with = |name: &str, config: Option<&str>| {
            let repo = tmp.path().join(name);
            std::fs::create_dir_all(repo.join(".rift")).unwrap();
            if let Some(config) = config {
                std::fs::write(repo.join(".rift/config.toml"), config).unwrap();
            }
            repo
        };
        let broken = repo_with(
            "broken",
            Some("schema_version = 99\nsecret = \"hunter2\"\n"),
        );
        let (human, json) = doctor_config_load(&broken);
        assert!(human.contains(".rift/config.toml does not load"), "{human}");
        assert!(human.contains("raw transcripts are off"), "{human}");
        assert!(!human.contains("hunter2"), "never quotes the file: {human}");
        assert!(human.contains(json["notice"].as_str().unwrap()), "{human}");

        let refused = repo_with("refused", None);
        std::fs::create_dir(refused.join(".rift/config.toml")).unwrap();
        let (human, _) = doctor_config_load(&refused);
        assert!(human.contains("not a regular file"), "{human}");
        assert!(human.contains("raw transcripts are off"), "{human}");

        for repo in [
            repo_with(
                "valid",
                Some("schema_version = 1\n[project]\nname = \"t\"\n"),
            ),
            repo_with("absent", None),
        ] {
            let (human, json) = doctor_config_load(&repo);
            assert_eq!(human, "");
            assert_eq!(json["notice"], serde_json::Value::Null);
        }
    }

    /// Only a file Rift cannot manage as written earns an action line; an
    /// adapter that is merely not wired says nothing beyond its row.
    #[test]
    fn only_a_conflicting_or_malformed_file_earns_an_action() {
        use rift_agent_install::native::{
            NativeDetection, NativeHookWiring, NativeIntegrationStatus, NativeWiring,
        };
        let status = |wiring, hook_wiring| NativeIntegrationStatus {
            agent: "cursor".to_owned(),
            display_name: "Cursor".to_owned(),
            detection: NativeDetection::NotDetected,
            wiring,
            executable: None,
            installations: Vec::new(),
            hook_wiring,
            codex_trust: None,
        };
        assert_eq!(fault_action(&status(NativeWiring::Missing, None)), None);
        assert_eq!(
            fault_action(&status(
                NativeWiring::Managed,
                Some(NativeHookWiring::Stale)
            )),
            None
        );
        assert_eq!(
            fault_action(&status(NativeWiring::Conflict, None)).as_deref(),
            Some("repair Cursor descriptor")
        );
        assert_eq!(
            fault_action(&status(
                NativeWiring::Managed,
                Some(NativeHookWiring::Invalid)
            ))
            .as_deref(),
            Some("repair Cursor hook configuration")
        );
    }

    /// A plan beside a family set up here is named with its remedy; the same
    /// plan beside an unprovisioned family is pending and stays silent.
    #[test]
    fn a_setup_plan_left_beside_a_set_up_family_is_named_with_its_remedy() {
        let cache = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        rift_git::test_support::fake_repo(repo.path()).unwrap();
        std::fs::create_dir_all(repo.path().join(".rift/state")).unwrap();
        std::fs::write(
            repo.path().join(SETUP_PLAN),
            r#"{"schema_version": 1, "integrations": []}"#,
        )
        .unwrap();
        let (human, json) = doctor_stale_setup_plan_under(cache.path(), repo.path());
        assert!(human.is_empty(), "{human}");
        assert_eq!(json["state"], "pending");

        let (_, rift_git::FamilyState::Minted(family)) =
            rift_git::family_of(repo.path(), rift_git::MintPolicy::Allow).unwrap()
        else {
            unreachable!("a fresh repository mints its family");
        };
        let db = rift_store::projection_db_for_family_under(cache.path(), &family);
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        std::fs::write(&db, b"").unwrap();
        rift_shadow::hooks::record_binary_of_record(
            &rift_git::family::family_dir_under(|k| std::env::var(k).ok(), &family.common_dir),
            &std::env::current_exe().unwrap(),
            rift_shadow::hooks::RecordedBy::Init,
        )
        .unwrap();
        let (human, json) = doctor_stale_setup_plan_under(cache.path(), repo.path());
        let remedy = rift_agent_install::health::stale_setup_plan_sentence(SETUP_PLAN);
        assert!(human.contains("setup plan:     stale"), "{human}");
        assert!(human.contains(&remedy), "{human}");
        assert_eq!(json["state"], "stale");
        assert_eq!(json["remedy"], remedy.as_str());
        assert!(
            repo.path().join(SETUP_PLAN).is_file(),
            "the report removes nothing"
        );
    }
}
