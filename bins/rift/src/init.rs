//! `rift init` / `rift setup`, and the one shared initialization Project
//! Settings runs too.
//!
//! Register the family, scaffold `.rift/`, open the projection, install the
//! git hooks, wire the native capture integrations, adopt a linked member,
//! and provision Recall — every step reported to a progress sink as it
//! lands, so the dashboard's setup dialog and the CLI's report are one
//! sequence of facts.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use rift_canopy::{PersistOp, SetupProgress, SetupStep};
use rift_registry::{default_registry_path, Added, ProjectRegistry};

use crate::cli::{exit, InitArgs, Outcome};
use crate::projection::{open_projection, project_name, runtime, worktree_root};
use crate::{adopt, agents, family, features, hooks};

/// A default `.rift/config.toml` (secure defaults apply for absent keys).
const DEFAULT_CONFIG: &str = "# Rift project configuration (.rift/config.toml).\n\
                              # Secure defaults apply when keys are absent.\n";

/// `rift init` / `rift setup`: register the cwd, scaffold `.rift/`, init the
/// projection, install git hooks (if a repo), and provision the backend.
pub(crate) fn cmd_init(root: &Path, args: &InitArgs) -> rift_core::Result<Outcome> {
    cmd_init_with_integrations(root, args, None, &mut |_| {})
}

/// The first line of a report, trimmed — the sentence a progress dialog
/// shows for a step whose full report is the CLI's.
fn first_line(text: &str) -> String {
    text.lines().next().unwrap_or_default().trim().to_owned()
}

/// The git-hooks step as the progress dialog reports it, read off the line
/// `init` prints: a refusal or a skip is said as one; healthy hooks left
/// alone, and hooks installed or re-pointed, landed.
fn hooks_progress(line: &str) -> rift_canopy::SetupProgress {
    let detail = first_line(line);
    let detail = detail.trim_start_matches("git hooks: ").to_owned();
    if detail.starts_with("skipped") || detail.starts_with("left alone —") {
        rift_canopy::SetupProgress::skipped(rift_canopy::SetupStep::GitHooks, detail)
    } else {
        rift_canopy::SetupProgress::done(rift_canopy::SetupStep::GitHooks, detail)
    }
}

/// One agent's wiring as the dialog reports it: `enable`'s first line
/// without the `enabled <agent>: ` preamble and the binary path — the row
/// already names the agent, and the path is `doctor`'s to show.
fn wired_detail(agent: &str, human: &str) -> String {
    let line = first_line(human);
    let line = line
        .strip_prefix(&format!("enabled {agent}: "))
        .unwrap_or(&line);
    line.split(" via ").next().unwrap_or(line).to_owned()
}

/// The refusal `init` owes an explicit selection naming agents this alpha
/// cannot capture — `None` when there is nothing to refuse.
///
/// It is a *precondition* failure, not a storage error: the command is
/// well-formed and the repository is fine; the environment is not in the state
/// the command needs. And it names **every** offender, because the caller that
/// made this necessary discovered them one run at a time.
///
/// The auto-detect path passes `None` and is never policed here — detection is
/// filtered by [`select_integrations`] instead, which leaves the coming-soon
/// agents alone rather than failing over them.
fn refuse_unsupported_selection(
    selected_integrations: Option<&std::collections::BTreeSet<String>>,
) -> Option<Outcome> {
    let selected = selected_integrations?;
    let refused =
        rift_agent_install::native::unsupported_alpha_agents(selected.iter().map(String::as_str));
    if refused.is_empty() {
        return None;
    }
    Some(Outcome::coded(
        exit::PRECONDITION,
        format!(
            "capture for {} is not supported yet — this alpha captures {}. \
             Nothing was initialized; re-run without {}",
            refused.join(", "),
            rift_agent_install::native::ALPHA_AGENTS.join(", "),
            if refused.len() == 1 { "it" } else { "them" },
        ),
        serde_json::json!({
            "error": "unsupported_agents",
            "unsupported": refused,
            "supported": rift_agent_install::native::ALPHA_AGENTS,
        }),
    ))
}

/// Split the adapters `init` would act on into the ones it wires (by token,
/// with the display name a dialog shows) and the ones it names as coming
/// soon (by display name).
///
/// **Detection is not eligibility.** `init` selected every
/// *detected* adapter and then let `enable` refuse the unsupported ones one at a
/// time, so `rift init` exited 1 on any machine with a coming-soon agent
/// installed. Filtering them here is only half the answer; the other half is
/// that they are returned rather than dropped, so the caller can say so.
fn select_integrations(
    repo: &Path,
    selected_integrations: Option<&std::collections::BTreeSet<String>>,
) -> (Vec<(String, String)>, Vec<String>) {
    let mut wire = Vec::new();
    let mut coming_soon = Vec::new();
    for row in rift_agent_install::native::diagnose_all(repo) {
        // `detected()` is the one predicate: an agent installed only as an
        // IDE extension is wired here exactly as a `PATH` command is, and
        // exactly as Canopy's pre-tick would have offered it.
        let chosen = selected_integrations
            .map_or_else(|| row.detected(), |selected| selected.contains(&row.agent))
            && !row.enabled();
        if !chosen {
            continue;
        }
        if rift_agent_install::native::is_alpha_supported(&row.agent) {
            wire.push((row.agent, row.display_name));
        } else {
            coming_soon.push(row.display_name);
        }
    }
    (wire, coming_soon)
}

/// Where `init`'s registry row goes, and which checkout it is standing in.
///
/// **`init` inside a linked worktree registers the family ROOT, not the
/// worktree.** A member is a second path to one repository, and a row
/// of its own was the duplicate project the family model removes; the member is
/// adopted instead, once the projection `init` creates can say what the family
/// has enabled.
struct InitTarget {
    /// The path that gets the family's one registry row.
    registered: PathBuf,
    /// That row's display name.
    name: String,
    /// The dedup key the registries are given. `None` outside a repository, and
    /// wherever the family has no recorded identity — an unknown id is never a
    /// match, so such a row dedups by path alone.
    family_id: Option<String>,
    /// The family's main work tree, where it is provable and present.
    root: Option<PathBuf>,
    /// This checkout's discovery, when it is inside a repository.
    paths: Option<rift_git::RepoPaths>,
    /// Whether this checkout is a linked member — the arm that adopts.
    member: bool,
}

impl InitTarget {
    /// Resolve the target for a canonical checkout path.
    ///
    /// The fallbacks are deliberate. No repository, or a family whose main work
    /// tree is unprovable from here, leaves `canon` as the path to register:
    /// there is no other path to name, and registering nothing would make the
    /// repository invisible in the dashboard rather than deduplicated.
    fn of(canon: &Path) -> Self {
        let paths = rift_git::discover(canon).ok();
        let member = paths.as_ref().is_some_and(rift_git::RepoPaths::is_linked);
        let family = family::try_at(canon);
        let root = family
            .as_ref()
            .and_then(|resolved| resolved.main_work_tree.clone())
            .filter(|main| main.is_dir());
        let registered = if member {
            root.clone().unwrap_or_else(|| canon.to_path_buf())
        } else {
            canon.to_path_buf()
        };
        Self {
            name: project_name(&registered),
            registered,
            family_id: family.map(|resolved| resolved.family_id.as_str().to_owned()),
            root,
            paths,
            member,
        }
    }
}

/// Shared native initialization. The CLI uses detected adapters as defaults;
/// Project Settings supplies the staged explicit selection. Every step is
/// reported to `progress` as it lands — the dashboard's setup dialog reads
/// them; the CLI passes a sink that drops them and prints the report.
///
/// An unsupported selection is refused HERE: before the registry row, before
/// the `.rift/` scaffold, before the git hooks. It used to be discovered
/// inside the wiring loop, which returned on the first refusal with the git
/// hooks already installed — leaving a half-initialised repository behind for
/// `rift doctor` to read as real.
///
/// Non-zero only at the END, and only for something the operator can act on
/// (a fragment Rift does not own, an unadopted member). Everything this
/// command could do, it did.
pub(crate) fn cmd_init_with_integrations(
    root: &Path,
    args: &InitArgs,
    selected_integrations: Option<&BTreeSet<String>>,
    progress: &mut dyn FnMut(SetupProgress),
) -> rift_core::Result<Outcome> {
    if let Some(refusal) = refuse_unsupported_selection(selected_integrations) {
        return Ok(refusal);
    }
    let canon = worktree_root(root).canonicalize()?;
    let target = InitTarget::of(&canon);
    let (added, reg_path) = register_family(&target)?;
    let (cfg, scaffolded) = scaffold_config(&canon)?;
    let projection = projection_progress(&canon);

    let mut human = init_registry_lines(&added, &target.name, &target.registered, &reg_path);
    progress(SetupProgress::done(SetupStep::Registry, first_line(&human)));
    human.push_str(&format!("config: {}\n", cfg.display()));
    progress(SetupProgress::done(
        SetupStep::Configuration,
        if scaffolded {
            ".rift/config.toml written"
        } else {
            "kept as the repository committed it"
        },
    ));
    progress(projection);

    let hooks_line = init_git_hooks(&canon);
    progress(hooks_progress(&hooks_line));
    human.push_str(&hooks_line);

    let (integration_lines, mut refused) =
        init_wire_integrations(&canon, selected_integrations, progress)?;
    human.push_str(&integration_lines);

    if let (true, Some(paths)) = (target.member, target.paths.as_ref()) {
        let (line, refusal) = adopt_member_step(paths, &canon, target.root.as_deref(), progress);
        human.push_str(&line);
        refused.extend(refusal);
    }

    let (recall_line, recall_json) = init_recall(&canon, args, progress)?;
    human.push_str(&recall_line);

    let json = serde_json::json!({
        "registered": target.name,
        "registered_path": target.registered.display().to_string(),
        "newly_registered": added == Added::Registered,
        "adopted_worktree": target.member,
        "refused": refused,
        "config": cfg.display().to_string(),
        "recall": recall_json,
    });
    let code = if refused.is_empty() {
        exit::OK
    } else {
        exit::PRECONDITION
    };
    Ok(Outcome::coded(code, human.trim_end().to_owned(), json))
}

/// Register into the global project registry (shared with the dashboard),
/// deduplicated by the family id the target resolved — the registry crate
/// stays free of git and never derives one itself.
fn register_family(target: &InitTarget) -> rift_core::Result<(Added, PathBuf)> {
    let reg_path = default_registry_path();
    let mut reg = ProjectRegistry::load(&reg_path)?;
    let added = reg.add_with_family(
        target.name.clone(),
        target.registered.clone(),
        target.family_id.as_deref(),
    );
    reg.save(&reg_path)?;
    Ok((added, reg_path))
}

/// Scaffold `.rift/`, returning the config path and whether this call wrote
/// it. A config the repository committed is kept as it came: the defaults
/// are written only where nothing was.
fn scaffold_config(canon: &Path) -> rift_core::Result<(PathBuf, bool)> {
    let rift_dir = canon.join(".rift");
    std::fs::create_dir_all(&rift_dir)?;
    let cfg = rift_dir.join("config.toml");
    let scaffolded = !cfg.exists();
    if scaffolded {
        std::fs::write(&cfg, DEFAULT_CONFIG)?;
    }
    Ok((cfg, scaffolded))
}

/// Best-effort projection init, as the dialog hears it: a failure is SAID
/// and is not fatal here — the open inside a real repository, when the
/// integrations are wired, is where a store failure stays an error.
fn projection_progress(canon: &Path) -> SetupProgress {
    match open_projection(canon) {
        Ok(_) => SetupProgress::done(
            SetupStep::Projection,
            family::projection_db(canon).map_or_else(
                |_| "opened".to_owned(),
                |db| rift_git::display_path(&db).display().to_string(),
            ),
        ),
        Err(error) => SetupProgress::failed(SetupStep::Projection, error.to_string()),
    }
}

/// The member's adoption, reported: the line it contributes and the refusal
/// it adds when the adoption was not clean. It runs after the integrations
/// are wired, so the family's projection exists and its `agent_enablement`
/// can answer; a refusal joins the report rather than aborting it.
fn adopt_member_step(
    paths: &rift_git::RepoPaths,
    canon: &Path,
    root: Option<&Path>,
    progress: &mut dyn FnMut(SetupProgress),
) -> (String, Option<String>) {
    let (line, clean) = init_adopt_member(paths, canon, root);
    if clean {
        progress(SetupProgress::done(SetupStep::Worktree, first_line(&line)));
        return (line, None);
    }
    progress(SetupProgress::failed(
        SetupStep::Worktree,
        first_line(&line),
    ));
    let refusal = format!("worktree {}", paths.member_name());
    (line, Some(refusal))
}

/// Wire the native capture integrations `init` selected — every detected
/// (or explicitly selected) supported agent — reporting each to the dialog
/// and returning the lines `init` prints plus the refusals it exits non-zero
/// for. The same registry-driven lifecycle `rift enable --agent` and Project
/// Settings use; absent agents remain available later.
///
/// A coming-soon agent is detected, left alone, and SAID — a user with Ollama
/// on their PATH is told why Rift did not wire it, rather than watching
/// `init` fail or seeing nothing at all (the same silence a half-wired
/// install produces). The descriptor rows an integration writes land in the
/// family's projection, so with no family there is nothing to write them to,
/// and minting a fresh one keyed by this directory's path is the defect the
/// family model removed: that case is said and skipped, exactly like the git
/// hooks. The check is separate from the open below it so a real store
/// failure inside a real repository stays fatal.
fn init_wire_integrations(
    canon: &Path,
    selected_integrations: Option<&BTreeSet<String>>,
    progress: &mut dyn FnMut(SetupProgress),
) -> rift_core::Result<(String, Vec<String>)> {
    let (integrations, coming_soon) = select_integrations(canon, selected_integrations);
    let mut human = coming_soon_lines(&coming_soon, progress);
    if integrations.is_empty() {
        human.push_str("native integrations: no new selected agents\n");
        return Ok((human, Vec::new()));
    }
    if let Err(e) = family::resolve_at(canon) {
        human.push_str(&format!("native integrations: skipped ({e})\n"));
        for (_, name) in &integrations {
            progress(SetupProgress::skipped(
                SetupStep::Agent(name.clone()),
                format!("skipped ({e})"),
            ));
        }
        return Ok((human, Vec::new()));
    }
    let store = open_projection(canon)?;
    let rt = runtime()?;
    let mut wired = 0_usize;
    let mut refused: Vec<String> = Vec::new();
    for (agent, name) in &integrations {
        match wire_agent(&store, &rt, canon, agent, name, progress)? {
            Wiring::Wired => wired += 1,
            Wiring::Refused(refusal) => refused.push(refusal),
        }
    }
    human.push_str(&format!("enabled {wired} native integration(s)\n"));
    for refusal in &refused {
        human.push_str(&format!("not wired — {refusal}\n"));
    }
    Ok((human, refused))
}

/// The coming-soon line, and one skipped row per agent it names — or nothing.
fn coming_soon_lines(coming_soon: &[String], progress: &mut dyn FnMut(SetupProgress)) -> String {
    if coming_soon.is_empty() {
        return String::new();
    }
    for name in coming_soon {
        progress(SetupProgress::skipped(
            SetupStep::Agent(name.clone()),
            "coming soon — not captured by this alpha",
        ));
    }
    format!(
        "coming soon: {} (detected, not wired — this alpha captures {})\n",
        coming_soon.join(", "),
        rift_agent_install::native::ALPHA_AGENTS.join(", ")
    )
}

/// What wiring one agent came to.
enum Wiring {
    Wired,
    /// The refusal, as `init` prints it.
    Refused(String),
}

/// Wire one agent, reported to the dialog. **A refusal is reported, never an
/// abort.** The loop used to return on the first agent that refused — after
/// the registry row, the scaffold, the projection and the git hooks were
/// already in place — leaving a half-initialised repository for `rift doctor`
/// to read as real, and never reaching the Recall step. The other agents are
/// wired, every refusal is printed, and `init` exits non-zero at the END so a
/// script still knows.
///
/// A CONFLICT — a native file Rift does not own, or one a person edited —
/// arrives as `Config`, and it is a fact about one agent's file, never a
/// reason to stop provisioning the repository. An `Io` failure is the
/// machine, and stays fatal: swallowing it would report an install that did
/// not happen.
fn wire_agent(
    store: &rift_store::SqliteStore,
    rt: &tokio::runtime::Runtime,
    canon: &Path,
    agent: &str,
    name: &str,
    progress: &mut dyn FnMut(SetupProgress),
) -> rift_core::Result<Wiring> {
    progress(SetupProgress::running(
        SetupStep::Agent(name.to_owned()),
        "wiring",
    ));
    match rt.block_on(agents::enable(store, canon, agent, hooks::now_ms())) {
        Ok(outcome) if outcome.code == exit::OK => {
            progress(SetupProgress::done(
                SetupStep::Agent(name.to_owned()),
                wired_detail(agent, &outcome.human),
            ));
            Ok(Wiring::Wired)
        }
        Ok(outcome) => {
            progress(SetupProgress::failed(
                SetupStep::Agent(name.to_owned()),
                first_line(&outcome.human),
            ));
            Ok(Wiring::Refused(format!(
                "{agent}: {}",
                outcome.human.trim()
            )))
        }
        Err(rift_core::RiftError::Config(reason)) => {
            progress(SetupProgress::failed(
                SetupStep::Agent(name.to_owned()),
                reason.clone(),
            ));
            Ok(Wiring::Refused(format!("{agent}: {reason}")))
        }
        Err(fault) => Err(fault),
    }
}

/// `init`'s Recall step: the line it prints and the `--json` document.
/// Reported to the dialog only when this call performs it — the settings
/// surface passes `--no-recall` and provisions Recall itself, with its own
/// report, after the policy it decides is written.
fn init_recall(
    canon: &Path,
    args: &InitArgs,
    progress: &mut dyn FnMut(rift_canopy::SetupProgress),
) -> rift_core::Result<(String, serde_json::Value)> {
    if args.no_recall {
        return Ok((
            "recall: skipped (--no-recall)\n".to_owned(),
            serde_json::json!({ "skipped": true }),
        ));
    }
    progress(SetupProgress::running(
        SetupStep::Recall,
        "skills and initial ingest",
    ));
    let report = match features::recall_install(canon) {
        Ok(report) => report,
        Err(reason) => {
            progress(SetupProgress::failed(SetupStep::Recall, reason.clone()));
            return Err(rift_core::RiftError::storage(reason));
        }
    };
    progress(SetupProgress::done(
        SetupStep::Recall,
        format!(
            "skills for {} agent(s), initial ingest ran",
            report.integrations.len()
        ),
    ));
    let line = format!(
        "recall: configured {} native integrations ({} files changed)\n",
        report.integrations.len(),
        report.changed_files.len()
    );
    Ok((
        line,
        serde_json::to_value(report).unwrap_or_else(|_| serde_json::json!({})),
    ))
}

/// `init` run inside a linked worktree: adopt the member, as the line it
/// contributes to the report plus whether anything was refused.
///
/// Best-effort by contract. Adoption's own rules apply unchanged — the family's
/// binary of record, a read-only projection open, a tracked file verified and
/// never written — and everything it cannot do it records in the member's
/// adoption marker, which `rift worktree adopt` and `rift doctor --fix` clear.
fn init_adopt_member(
    paths: &rift_git::RepoPaths,
    canon: &Path,
    root: Option<&Path>,
) -> (String, bool) {
    let member = paths.member_name();
    let of_family = root.map_or_else(
        || "(root unresolved)".to_owned(),
        |root| root.display().to_string(),
    );
    let Ok(machine) = adopt::machine_for(paths) else {
        return (
            format!("worktree {member}: not adopted (this binary cannot read its own path)\n"),
            false,
        );
    };
    let intent = family::projection_db(canon).map_or_else(
        |error| adopt::Intent::Unknown(error.to_string()),
        |db| adopt::enabled_agents(&db),
    );
    let report = adopt::adopt_member(canon, &machine, &intent, hooks::now_ms());
    (
        format!(
            "adopted worktree {member} of family {of_family} — {}\n",
            report.summary()
        ),
        report.is_clean(),
    )
}

/// What `init` says about the two registries, and the dashboard row it adds to
/// the second of them.
///
/// The dashboard's append-only store is the source of truth (the TOML registry
/// is kept for the one-time legacy import); registering into it is best-effort,
/// because a UI-store hiccup must not fail the initialization of a repository.
/// A **refusal** from it is the one-row-per-family rule rather than a failure — this
/// family already has its row — and it is now said out loud: `init` used to
/// discard the result whole, so a registration that never happened read exactly
/// like one that did.
fn init_registry_lines(added: &Added, name: &str, registered: &Path, reg_path: &Path) -> String {
    let mut human = match added {
        Added::Registered => format!(
            "registered {name} at {}\n",
            rift_git::display_path(registered).display()
        ),
        Added::AlreadyPresent => {
            format!("{name} is already registered ({})\n", registered.display())
        }
        Added::FamilyRegisteredAs(row) => format!(
            "{name} belongs to a repository that is already registered ({})\n",
            row.display()
        ),
    };
    let cache_root = rift_store::resolve_cache_root(|k| std::env::var(k).ok());
    if let Ok(cstore) = rift_canopy_data::CanopyStore::open(&cache_root) {
        if let Err(error) = cstore.apply(
            &PersistOp::AddProject {
                path: registered.to_string_lossy().into_owned(),
            },
            hooks::now_ms(),
        ) {
            human.push_str(&format!("dashboard: {error}\n"));
        }
    }
    human.push_str(&format!("registry: {}\n", reg_path.display()));
    human
}

/// `init`'s hook install, as the lines it contributes to the report.
///
/// Hooks live in the COMMON dir: `<root>/.git` is a directory only in the
/// main worktree — in a linked one it is a file naming the shared repository,
/// and testing it with `is_dir()` reported a real repository as "not a git
/// repo" and installed nothing. **`init` never re-points a family that is
/// already working.** Hooks are shared through the common dir, so before this
/// policy an `init` in any member re-pointed every member's hooks at whatever
/// binary that process happened to be — the side effect that left this
/// repository's six hooks dangling at a deleted `/tmp` binary for four days.
///
/// **Best-effort, and now honestly so**: reading this process's own path used
/// to be fatal to `init` (`current_exe()?`), directly under a comment promising
/// it never would be. Nothing here can fail the initialization of a repository;
/// what it cannot do, it says.
fn init_git_hooks(canon: &Path) -> String {
    let Ok(git_dir) = rift_git::discover(canon).map(|paths| paths.common_dir) else {
        return "git hooks: skipped (not a git repo)\n".to_owned();
    };
    let Ok(rift_bin) = std::env::current_exe() else {
        return "git hooks: skipped (this binary cannot read its own path)\n".to_owned();
    };
    match hooks::install_family_hooks(
        &git_dir,
        canon,
        None,
        rift_bin,
        rift_shadow::hooks::HookInstallPolicy::KeepIfHealthy {
            allow_transient: false,
        },
        rift_shadow::hooks::RecordedBy::Init,
    ) {
        Ok(report) => {
            let mut lines = hooks::report_lines(&report).join("\n");
            lines.push('\n');
            lines
        }
        Err(error) => format!("git hooks: skipped ({error})\n"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The refusal `init` owes an unsupported explicit selection: a
    /// PRECONDITION, naming every offender, and silent when there is nothing to
    /// refuse.
    ///
    /// `FAILURE` would be the wrong code and was the shipped one — the wiring
    /// loop raised a *storage* error, so `rift init` reported a repository
    /// problem for what is a selection problem.
    #[test]
    fn an_unsupported_selection_is_a_precondition_naming_every_offender() {
        let selection: std::collections::BTreeSet<String> = ["cursor", "codex", "ollama"]
            .iter()
            .map(|s| (*s).to_owned())
            .collect();
        let refusal = refuse_unsupported_selection(Some(&selection))
            .expect("a selection holding two coming-soon agents is refused");
        assert_eq!(refusal.code, exit::PRECONDITION);
        assert!(refusal.human.contains("cursor"), "{}", refusal.human);
        assert!(
            refusal.human.contains("ollama"),
            "every offender, not the first: {}",
            refusal.human
        );
        assert_eq!(
            refusal.json["unsupported"],
            serde_json::json!(["cursor", "ollama"]),
            "the machine surface carries the same list the sentence does"
        );

        let supported: std::collections::BTreeSet<String> =
            rift_agent_install::native::ALPHA_AGENTS
                .iter()
                .map(|s| (*s).to_owned())
                .collect();
        assert!(refuse_unsupported_selection(Some(&supported)).is_none());
        // The auto-detect path has no explicit selection and is never refused —
        // if it were, `rift init` would fail on exactly the machines this fixed.
        assert!(refuse_unsupported_selection(None).is_none());
    }

    /// The dialog's lines are shaped from the CLI's own report lines: the
    /// hooks step reads a skip or a refusal as skipped and everything else as
    /// landed, and an agent's line drops the preamble and the binary path the
    /// row does not need.
    #[test]
    fn setup_progress_is_shaped_from_the_report_lines() {
        use rift_canopy::{SetupStep, SetupStepStatus};
        let landed =
            hooks_progress("installed 7 git hooks in /r/.git/hooks (binary of record: /b)\n");
        assert_eq!(landed.step, SetupStep::GitHooks);
        assert_eq!(landed.status, SetupStepStatus::Done);
        assert!(
            landed.detail.starts_with("installed 7 git hooks"),
            "{}",
            landed.detail
        );
        let healthy =
            hooks_progress("git hooks: 7 already healthy, left alone (binary of record: /b)\n");
        assert_eq!(healthy.status, SetupStepStatus::Done);
        assert_eq!(
            healthy.detail,
            "7 already healthy, left alone (binary of record: /b)"
        );
        for skipped in [
            "git hooks: skipped (not a git repo)\n",
            "git hooks: left alone — refused: /tmp binary\n",
        ] {
            let report = hooks_progress(skipped);
            assert_eq!(report.status, SetupStepStatus::Skipped, "{skipped}");
            assert!(
                !report.detail.starts_with("git hooks:"),
                "{}",
                report.detail
            );
        }
        assert_eq!(
            wired_detail(
                "codex",
                "enabled codex: wired 10 hook(s) via /r/target/release/rift\n  config: x"
            ),
            "wired 10 hook(s)"
        );
        assert_eq!(wired_detail("codex", "something else\n"), "something else");
        assert_eq!(first_line("  a\nb\n"), "a");
        assert_eq!(first_line(""), "");
    }
}
