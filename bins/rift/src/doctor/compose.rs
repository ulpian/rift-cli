//! Per-client hook composition: for each enabled agent with a hook file, one
//! closed word for how Rift's entries sit beside everyone else's.
//!
//! The words and the ownership verdict come from
//! `rift_agent_install::native::hook_composition`, which reads the existing
//! fingerprints and never rewrites. `--fix` re-wires only an entry that is
//! missing or still Rift's own and unmodified, through the same enable path
//! `rift enable` takes; a hand edit or a foreign neighbour is named and left.

use std::path::Path;

use rift_agent_install::native::{self, HookComposition, NativeWiring};

/// The doctor leg. The repair binary is the family's binary of record, read
/// only when `--fix` asks for a repair.
pub(super) fn doctor_hook_composition(repo: &Path, fix: bool) -> (String, serde_json::Value) {
    let rift_bin = fix
        .then(|| rift_git::discover(repo).ok())
        .flatten()
        .and_then(|paths| crate::adopt::machine_for(&paths).ok())
        .map(|machine| machine.rift_bin);
    hook_composition_with(repo, rift_bin.as_deref())
}

/// The leg with the repair binary injected; `None` reports only.
fn hook_composition_with(repo: &Path, rift_bin: Option<&Path>) -> (String, serde_json::Value) {
    let mut human = String::new();
    let mut found = Vec::new();
    // Enabled by its descriptor, not by `enabled()`, which also asks for owned
    // hooks and so would hide exactly the agents this leg exists to name.
    let rows = native::diagnose_all(repo);
    for row in rows
        .iter()
        .filter(|row| row.wiring == NativeWiring::Managed)
    {
        let Some(before) = native::hook_composition(repo, row) else {
            continue;
        };
        let rewired = rewire(repo, rift_bin, &row.agent, before);
        let after = rewired
            .as_ref()
            .and_then(|_| {
                native::diagnose_all(repo)
                    .into_iter()
                    .find(|r| r.agent == row.agent)
            })
            .and_then(|r| native::hook_composition(repo, &r));
        human.push_str(&composition_line(
            &row.agent,
            before,
            rewired.as_ref(),
            after,
        ));
        found.push(serde_json::json!({
            "agent": row.agent,
            "composition": before.as_str(),
            "rewirable": before.rewirable(),
            "rewired": rewired.as_ref().map(Result::is_ok),
            "error": rewired.and_then(Result::err),
            "after": after.map(HookComposition::as_str),
        }));
    }
    (human, serde_json::json!({ "agents": found }))
}

/// Re-wire through the enable path when there is a binary to wire and the
/// entry is Rift's to rewrite; `None` when nothing was attempted.
fn rewire(
    repo: &Path,
    rift_bin: Option<&Path>,
    agent: &str,
    composition: HookComposition,
) -> Option<Result<(), String>> {
    let rift_bin = rift_bin.filter(|_| composition.rewirable())?;
    Some(
        native::enable(repo, rift_bin, agent)
            .map(|_| ())
            .map_err(|e| e.to_string()),
    )
}

/// The human line for one agent.
fn composition_line(
    agent: &str,
    before: HookComposition,
    rewired: Option<&Result<(), String>>,
    after: Option<HookComposition>,
) -> String {
    let outcome = match (rewired, after) {
        (Some(Ok(())), Some(now)) => format!("; re-wired, now {}", now.as_str()),
        (Some(Ok(())), None) => "; re-wired".to_owned(),
        (Some(Err(e)), _) => format!("; re-wire refused: {e}"),
        (None, _) if before.rewirable() => "; `rift doctor --fix` re-wires it".to_owned(),
        (None, _) if before == HookComposition::Owned => String::new(),
        (None, _) => "; left for you to settle, Rift rewrites no entry it cannot prove".to_owned(),
    };
    format!("\n  hook composition: {agent} {}{outcome}", before.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn codex_hooks(dir: &Path) -> std::path::PathBuf {
        dir.join(".codex/hooks.json")
    }

    #[test]
    fn a_missing_hook_entry_is_named_and_only_fix_rewires_it() {
        let dir = tempfile::tempdir().unwrap();
        native::enable(dir.path(), Path::new("/opt/rift"), "codex").unwrap();
        std::fs::remove_file(codex_hooks(dir.path())).unwrap();

        let (human, json) = hook_composition_with(dir.path(), None);
        assert!(human.contains("codex missing"), "{human}");
        assert_eq!(json["agents"][0]["rewired"], serde_json::Value::Null);
        assert!(!codex_hooks(dir.path()).exists(), "a report writes nothing");

        let (human, json) = hook_composition_with(dir.path(), Some(Path::new("/opt/rift")));
        assert!(human.contains("re-wired, now owned"), "{human}");
        assert_eq!(json["agents"][0]["after"], "owned");
    }

    #[test]
    fn a_shadowed_hook_is_named_and_never_rewritten_by_fix() {
        let dir = tempfile::tempdir().unwrap();
        let path = codex_hooks(dir.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let theirs = serde_json::json!({ "hooks": { "PreToolUse": [{
            "matcher": "*",
            "hooks": [{"type": "command", "command": "someone-elses-guard"}]
        }] } });
        std::fs::write(&path, theirs.to_string()).unwrap();
        native::enable(dir.path(), Path::new("/opt/rift"), "codex").unwrap();
        let before = std::fs::read(&path).unwrap();

        let (human, json) = hook_composition_with(dir.path(), Some(Path::new("/opt/rift")));
        assert!(human.contains("codex shadowed; left for you"), "{human}");
        assert_eq!(json["agents"][0]["rewired"], serde_json::Value::Null);
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }
}
