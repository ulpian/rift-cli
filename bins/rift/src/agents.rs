//! `rift enable/disable --agent <x>`: wire an agent's hooks to
//! `rift hooks <agent> <verb>` and record the enable/disable as an append-only
//! event.
//!
//! Wiring is **config, not code injection**: `enable` writes a rift-owned wiring
//! descriptor under `.rift/agents/<agent>.json` naming the **absolute** `rift`
//! binary and the exact `rift hooks <agent> <verb>` command for each native verb
//! (mirroring the git-hook absolute-path discipline). It is idempotent and
//! reversible (`disable` removes only a fingerprint-matching Rift artifact),
//! and **never** names the upstream backend. The shared native lifecycle is the
//! same one Project Settings invokes.

use rift_core::{AgentId, Result};
use rift_hashchain::leaf_hash;
use rift_shadow::hooks::AdapterRegistry;
use rift_store::{record_agent_enablement, AgentEnablementRow, SqliteStore};

use crate::cli::{exit, Outcome};

/// What one successful enable did, gathered for its report.
struct Enabled<'a> {
    root: &'a std::path::Path,
    agent_token: &'a str,
    verbs: &'a [&'a str],
    notes: &'a rift_agent::CompatNotes,
    rift_bin: &'a std::path::Path,
    path: &'a std::path::Path,
    wired_hash: &'a str,
    changed_files: &'a [std::path::PathBuf],
    members: &'a [MemberOutcome],
    codex_trust: Option<&'a rift_agent_install::native::CodexTrust>,
}

/// Hex content fingerprint over `bytes` (domain-separated leaf hash, hex).
fn fingerprint(bytes: &[u8]) -> String {
    leaf_hash(bytes).to_hex()
}

/// Map an installer failure to its truthful [`rift_core::RiftError`] class.
///
/// Every install error used to become `RiftError::Storage`, so a refusal —
/// an ownership conflict, a scope precondition — surfaced as
/// `storage error: <path>: …` when no storage operation had failed and, for
/// refusals, nothing at that path had ever been written. A refusal is a
/// config fact; only a real filesystem failure is a storage one.
fn install_error(error: rift_agent_install::InstallError) -> rift_core::RiftError {
    match error {
        rift_agent_install::InstallError::Config { .. } => {
            rift_core::RiftError::config(error.to_string())
        }
        rift_agent_install::InstallError::Io { .. } => {
            rift_core::RiftError::storage(error.to_string())
        }
    }
}

/// `rift enable --agent <x>`.
pub(crate) async fn enable(
    store: &SqliteStore,
    root: &std::path::Path,
    agent_token: &str,
    now_ms: i64,
) -> Result<Outcome> {
    let agent = AgentId::new(agent_token);
    if !agent.is_well_formed() {
        return Ok(Outcome::coded(
            exit::USAGE,
            format!("invalid agent token {agent_token:?}"),
            serde_json::json!({ "error": "invalid_agent", "agent": agent_token }),
        ));
    }
    let reg = AdapterRegistry::with_builtin();
    let Some(adapter) = reg.get(&agent) else {
        return Ok(unknown_agent(&reg, agent_token));
    };
    let rift_bin = std::env::current_exe()?;
    let changed_files = wire(root, &rift_bin, agent_token)?;
    let path = root
        .join(".rift")
        .join("agents")
        .join(format!("{agent_token}.json"));
    let wired_hash =
        std::fs::read(&path).map_or_else(|_| fingerprint(b"enabled"), |bytes| fingerprint(&bytes));

    record_event(store, agent_token, "enable", now_ms, &wired_hash).await?;
    let members = fan_out(root, agent_token, true);
    let codex_trust = written_codex_trust(root, agent_token);
    let enabled = Enabled {
        root,
        agent_token,
        verbs: adapter.verbs(),
        notes: &adapter.compat_notes(),
        rift_bin: &rift_bin,
        path: &path,
        wired_hash: &wired_hash,
        changed_files: &changed_files,
        members: &members,
        codex_trust: codex_trust.as_ref(),
    };
    Ok(enabled_outcome(&enabled))
}

/// Codex's trust in the hook file this enable just wrote, compared at the
/// write so a handler Codex will now skip is said here rather than found
/// weeks later in an idle session. Rift cannot re-trust on the user's
/// behalf and never writes Codex's configuration; it can only say so.
pub(crate) fn written_codex_trust(
    root: &std::path::Path,
    agent_token: &str,
) -> Option<rift_agent_install::native::CodexTrust> {
    if agent_token != AgentId::CODEX.as_str() {
        return None;
    }
    let home = rift_agent_install::native::codex_home_from_process();
    rift_agent_install::native::codex_trust(root, home.as_deref())
}

/// The refusal for a well-formed token no built-in adapter answers to.
///
/// A precondition, not a generic failure: the command is well-formed but no
/// built-in adapter exists for this agent (the exit contract).
fn unknown_agent(reg: &AdapterRegistry, agent_token: &str) -> Outcome {
    let known: Vec<String> = reg.agents().iter().map(|a| a.as_str().to_owned()).collect();
    Outcome::coded(
        exit::PRECONDITION,
        format!(
            "no adapter for agent {agent_token:?}; known: {}",
            known.join(", ")
        ),
        serde_json::json!({ "error": "unknown_agent", "known": known }),
    )
}

/// Wire this checkout through the native installer, and install the recall
/// skill beside it when the repository's policy has recall on.
///
/// Returns every file either step changed, sorted and deduplicated, because
/// the two installers can name the same file and the report lists each once.
fn wire(
    root: &std::path::Path,
    rift_bin: &std::path::Path,
    agent_token: &str,
) -> Result<Vec<std::path::PathBuf>> {
    let mut report =
        rift_agent_install::native::enable(root, rift_bin, agent_token).map_err(install_error)?;
    let recall_on = rift_config::EffectiveConfig::load(&root.join(".rift"))
        .is_ok_and(|config| config.recall_enabled());
    if recall_on {
        report.changed_files.extend(
            rift_agent_install::native::install_recall_skill(
                root,
                agent_token,
                &crate::agent_help::skill_body(),
            )
            .map_err(install_error)?
            .changed_files,
        );
        report.changed_files.sort();
        report.changed_files.dedup();
    }
    Ok(report.changed_files)
}

/// The human line and the JSON document for one successful enable.
fn enabled_outcome(enabled: &Enabled<'_>) -> Outcome {
    let agent_token = enabled.agent_token;
    let note_line = if enabled.notes.best_effort {
        format!("\n  note: best-effort capture ({})", enabled.notes.note)
    } else {
        String::new()
    };
    // The native hook file, when this agent has one Rift writes. Named from the
    // installer's table rather than from a `== "codex"` test: now that
    // Claude Code has one as well, and an enable that wires a file without
    // saying so is how D12 stayed invisible for nineteen days.
    let native_hook_line =
        rift_agent_install::native::hook_file(agent_token).map_or_else(String::new, |relative| {
            format!(
                "\n  native hooks: {}",
                enabled.root.join(relative).display()
            )
        });
    let human = format!(
        "enabled {agent_token}: wired {} hook(s) via {}\n  config: {}{native_hook_line}{note_line}{}{}",
        enabled.verbs.len(),
        enabled.rift_bin.display(),
        enabled.path.display(),
        member_lines(enabled.members),
        codex_trust_lines(enabled.codex_trust),
    );
    let json = serde_json::json!({
        "agent": agent_token,
        "wired_hooks": enabled.verbs,
        "config": enabled.path.display().to_string(),
        "wired_hooks_hash": enabled.wired_hash,
        "best_effort": enabled.notes.best_effort,
        "changed_files": enabled.changed_files.iter().map(|path| path.display().to_string()).collect::<Vec<_>>(),
        "members": member_json(enabled.members),
        "codex_trust": enabled.codex_trust,
        "codex_trust_remedy": enabled.codex_trust.and_then(rift_agent_install::native::CodexTrust::remedy),
    });
    Outcome::ok(human, json)
}

/// `outcome` with Codex's per-hook trust appended, for a status read: the
/// line and remedy in the human text, the verdicts and remedy in the JSON.
/// Unchanged when the checkout's Codex hook file holds no Rift hook.
pub(crate) fn with_codex_trust(root: &std::path::Path, mut outcome: Outcome) -> Outcome {
    let Some(trust) = written_codex_trust(root, AgentId::CODEX.as_str()) else {
        return outcome;
    };
    outcome.human.push_str(&codex_trust_lines(Some(&trust)));
    if let Some(json) = outcome.json.as_object_mut() {
        json.insert("codex_trust_remedy".to_owned(), trust.remedy().into());
        json.insert(
            "codex_trust".to_owned(),
            serde_json::to_value(&trust).unwrap_or_default(),
        );
    }
    outcome
}

/// The trust line an enable or a status read prints for Codex, and the
/// remedy under it when Codex will skip any of Rift's hooks.
fn codex_trust_lines(trust: Option<&rift_agent_install::native::CodexTrust>) -> String {
    let Some(trust) = trust else {
        return String::new();
    };
    let mut lines = format!("\n  codex trust: {}", trust.summary());
    if let Some(remedy) = trust.remedy() {
        lines.push_str(&format!("\n  {remedy}"));
    }
    lines
}

/// `rift disable --agent <x>`: remove the wiring descriptor (full round-trip).
pub(crate) async fn disable(
    store: &SqliteStore,
    root: &std::path::Path,
    agent_token: &str,
    now_ms: i64,
) -> Result<Outcome> {
    let mut changed = rift_agent_install::native::disable(root, agent_token)
        .map_err(install_error)?
        .changed_files;
    changed.extend(
        rift_agent_install::native::remove_recall_skill(root, agent_token)
            .map_err(install_error)?
            .changed_files,
    );
    changed.sort();
    changed.dedup();
    let wired_hash = fingerprint(b"disabled");
    record_event(store, agent_token, "disable", now_ms, &wired_hash).await?;
    let members = fan_out(root, agent_token, false);
    let human = if changed.is_empty() {
        format!("{agent_token} was not enabled{}", member_lines(&members))
    } else {
        format!(
            "disabled {agent_token}: removed Rift-managed wiring{}",
            member_lines(&members)
        )
    };
    Ok(Outcome::ok(
        human,
        serde_json::json!({
            "agent": agent_token,
            "disabled": !changed.is_empty(),
            "changed_files": changed.iter().map(|path| path.display().to_string()).collect::<Vec<_>>(),
            "members": member_json(&members),
        }),
    ))
}

/// One other member of the family, and what this enable/disable did to it.
type MemberOutcome = (String, String);

/// Carry this call to every OTHER member of the family.
///
/// `rift enable` records the family's INTENT, and the intent is family-wide:
/// the `agent_enablement` relation is one relation for the whole repository
/// so a member that already exists is wired now rather than at its
/// next `rift worktree adopt`. Without this, "enabled" meant "enabled in the
/// checkout that happened to run the command", which is the shape of the defect
/// this program exists to remove.
///
/// Three rules it does not bend:
///
/// * the binary written into another member is the family's **binary of
///   record**, never this process's `current_exe()` — enabling from a
///   throwaway build must not wire the family to it;
/// * a member that refuses (a conflicted fragment, an unowned descriptor) is
///   **named, not overwritten**, and does not fail the enable that already
///   succeeded here — the local act is recorded and true;
/// * tracked files and git hooks are untouched, exactly as in adoption: this
///   goes through the same installer, so it inherits the same refusals.
///
/// Outside a repository, or with an unresolvable family, there are no other
/// members and there is nothing to say.
fn fan_out(root: &std::path::Path, agent_token: &str, enable: bool) -> Vec<MemberOutcome> {
    let Ok(paths) = rift_git::discover(root) else {
        return Vec::new();
    };
    let Ok(machine) = crate::adopt::machine_for(&paths) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for member in rift_git::family_members(&paths) {
        if member.prunable || crate::adopt::same_tree(&member.work_tree, root) {
            continue;
        }
        let outcome = if enable {
            match rift_agent_install::native::enable_for_adoption(
                &member.work_tree,
                &machine.rift_bin,
                agent_token,
            ) {
                Ok(report) if report.changed_files.is_empty() => "already wired".to_owned(),
                Ok(report) => format!("wired ({} file(s))", report.changed_files.len()),
                Err(error) => format!("refused: {error}"),
            }
        } else {
            match rift_agent_install::native::disable_for_adoption(&member.work_tree, agent_token) {
                Ok(report) if report.changed_files.is_empty() => "was not enabled".to_owned(),
                Ok(_) => "unwired".to_owned(),
                Err(error) => format!("refused: {error}"),
            }
        };
        out.push((member.name, outcome));
    }
    out
}

fn member_lines(members: &[MemberOutcome]) -> String {
    let mut out = String::new();
    for (name, outcome) in members {
        out.push_str(&format!("\n  worktree {name}: {outcome}"));
    }
    out
}

fn member_json(members: &[MemberOutcome]) -> serde_json::Value {
    serde_json::Value::Array(
        members
            .iter()
            .map(|(name, outcome)| serde_json::json!({ "member": name, "outcome": outcome }))
            .collect(),
    )
}

/// Record an enable/disable event (append-only, idempotent on its content address).
async fn record_event(
    store: &SqliteStore,
    agent: &str,
    action: &str,
    changed_at: i64,
    wired_hash: &str,
) -> Result<()> {
    let mut key = Vec::new();
    key.extend_from_slice(agent.as_bytes());
    key.push(0);
    key.extend_from_slice(action.as_bytes());
    key.push(0);
    key.extend_from_slice(changed_at.to_string().as_bytes());
    key.push(0);
    key.extend_from_slice(wired_hash.as_bytes());
    let change_id = fingerprint(&key);
    record_agent_enablement(
        store,
        &AgentEnablementRow {
            change_id,
            agent: agent.to_owned(),
            action: action.to_owned(),
            changed_at,
            wired_hooks_hash: wired_hash.to_owned(),
            schema_version: 1,
        },
    )
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A family fixture with one linked member, and a binary of record that is
    /// nothing like the binary running this test — so "wired with the binary of
    /// record" is a claim the fixture can actually falsify.
    fn family_with_member(tmp: &std::path::Path) -> (std::path::PathBuf, std::path::PathBuf) {
        let main = tmp.join("main");
        std::fs::create_dir_all(&main).unwrap();
        let main_git = rift_git::test_support::fake_repo(&main).unwrap();
        let member = tmp.join("member");
        rift_git::test_support::fake_worktree(&main_git, &member, "member").unwrap();
        rift_shadow::hooks::record_binary_of_record(
            &main_git.join("rift-family"),
            std::path::Path::new("/opt/rift/rift"),
            rift_shadow::hooks::RecordedBy::Init,
        )
        .unwrap();
        (main, member)
    }

    fn projection(path: &std::path::Path) -> SqliteStore {
        rift_store::open_capture_projection(path).unwrap()
    }

    /// ★ `rift enable` records the FAMILY's intent — the
    /// `agent_enablement` relation is one relation for the whole repository —
    /// so a member that already exists is wired by the same call, rather than
    /// waiting for someone to remember `rift worktree adopt` inside it.
    ///
    /// The member is wired with the family's **binary of record**; the checkout
    /// that ran the command is wired with the binary that ran it. Both facts
    /// are asserted, because collapsing them is how a throwaway build ends up
    /// named in every member's descriptor.
    #[test]
    fn enable_in_main_provisions_an_existing_member() {
        let tmp = tempfile::tempdir().unwrap();
        let (main, member) = family_with_member(tmp.path());
        let store = projection(&tmp.path().join("p.db"));
        let runtime = crate::runtime().unwrap();

        let outcome = runtime
            .block_on(enable(&store, &main, "claude-code", 1_700))
            .unwrap();

        assert_eq!(outcome.code, exit::OK, "{}", outcome.human);
        let descriptor =
            std::fs::read_to_string(member.join(".rift/agents/claude-code.json")).unwrap();
        assert!(
            descriptor.contains("/opt/rift/rift"),
            "the member is wired with the binary of record: {descriptor}"
        );
        let fragment = std::fs::read_to_string(member.join(".claude/settings.local.json")).unwrap();
        assert!(
            fragment.contains(r#"/opt/rift/rift\" hooks claude-code"#),
            "and its hook fragment names it too: {fragment}"
        );
        assert!(
            outcome.human.contains("worktree member: wired"),
            "the fan-out is reported, not silent: {}",
            outcome.human
        );
        assert_eq!(outcome.json["members"][0]["member"], "member");

        // Parsed, then compared as a path: JSON spells a Windows path with
        // `\\`, so a substring search over the raw text is platform-naive.
        let local: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(main.join(".rift/agents/claude-code.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            local["rift_bin"].as_str().map(std::path::Path::new),
            Some(std::env::current_exe().unwrap().as_path()),
            "the checkout that ran the command keeps the binary that ran it: {local}"
        );

        // Idempotent across the family: a second enable rewires nothing.
        let again = runtime
            .block_on(enable(&store, &main, "claude-code", 1_800))
            .unwrap();
        assert_eq!(again.json["members"][0]["outcome"], "already wired");
    }

    /// The symmetry that rule requires: `disable` from ANY member removes the
    /// wiring from every member, so an agent cannot stay live in a worktree
    /// nobody is looking at.
    #[test]
    fn disable_in_a_member_unwires_every_member() {
        let tmp = tempfile::tempdir().unwrap();
        let (main, member) = family_with_member(tmp.path());
        let store = projection(&tmp.path().join("p.db"));
        let runtime = crate::runtime().unwrap();
        runtime
            .block_on(enable(&store, &main, "claude-code", 1_700))
            .unwrap();
        assert!(member.join(".rift/agents/claude-code.json").is_file());

        let outcome = runtime
            .block_on(disable(&store, &member, "claude-code", 1_900))
            .unwrap();

        assert_eq!(outcome.code, exit::OK, "{}", outcome.human);
        assert!(
            !member.join(".rift/agents/claude-code.json").exists(),
            "the member the command ran in"
        );
        assert!(
            !main.join(".rift/agents/claude-code.json").exists(),
            "and every other member of the family"
        );
        assert_eq!(outcome.json["members"][0]["outcome"], "unwired");
    }

    /// The error-class boundary: an installer REFUSAL (ownership conflict,
    /// scope precondition) is a config fact and must never claim a storage
    /// failure; a real filesystem failure still does.
    #[test]
    fn install_refusals_surface_as_config_errors_not_storage() {
        let refusal = install_error(rift_agent_install::InstallError::Config {
            path: "/r/.rift/agents/gemini.json".into(),
            reason: "capture for gemini is not supported yet".into(),
        });
        assert!(
            refusal.to_string().starts_with("config error:"),
            "{refusal}"
        );
        let io = install_error(rift_agent_install::InstallError::Io {
            path: "/r/.codex/hooks.json".into(),
            source: std::io::Error::other("disk full"),
        });
        assert!(io.to_string().starts_with("storage error:"), "{io}");
    }
}
