//! Per-client hook composition: for each enabled agent with a hook file, one
//! closed word for how Rift's entries sit beside everyone else's.
//!
//! The words and the ownership verdict come from
//! `rift_agent_install::native::hook_composition`, which reads the existing
//! fingerprints and never rewrites. This leg only reports, under `--fix` too:
//! `rift enable` is the one command that rewrites an agent's hook file, so each
//! word's remedy sentence names it instead of doctor acting.

use std::path::Path;

use rift_agent_install::health::hook_composition_sentence;
use rift_agent_install::native::{self, NativeWiring};

/// The doctor leg: one word and its remedy sentence per enabled agent.
pub(super) fn doctor_hook_composition(repo: &Path) -> (String, serde_json::Value) {
    let mut human = String::new();
    let mut found = Vec::new();
    // Enabled by its descriptor, not by `enabled()`, which also asks for owned
    // hooks and so would hide exactly the agents this leg exists to name.
    let rows = native::diagnose_all(repo);
    for row in rows
        .iter()
        .filter(|row| row.wiring == NativeWiring::Managed)
    {
        let Some(composition) = native::hook_composition(repo, row) else {
            continue;
        };
        let remedy = hook_composition_sentence(&row.agent, composition);
        human.push_str(&composition_line(
            &row.agent,
            composition.as_str(),
            remedy.as_deref(),
        ));
        found.push(serde_json::json!({
            "agent": row.agent,
            "composition": composition.as_str(),
            "remedy": remedy,
        }));
    }
    (human, serde_json::json!({ "agents": found }))
}

/// The human line for one agent.
fn composition_line(agent: &str, word: &str, remedy: Option<&str>) -> String {
    match remedy {
        Some(remedy) => format!("\n  hook composition: {agent} {word}; {remedy}"),
        None => format!("\n  hook composition: {agent} {word}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn codex_hooks(dir: &Path) -> std::path::PathBuf {
        dir.join(".codex/hooks.json")
    }

    #[test]
    fn a_missing_hook_entry_is_named_with_rift_enable_and_never_rewired() {
        let dir = tempfile::tempdir().unwrap();
        native::enable(dir.path(), Path::new("/opt/rift"), "codex").unwrap();
        std::fs::remove_file(codex_hooks(dir.path())).unwrap();

        let (human, json) = doctor_hook_composition(dir.path());
        assert!(human.contains("codex missing"), "{human}");
        assert!(human.contains("`rift enable --agent codex`"), "{human}");
        assert_eq!(json["agents"][0]["composition"], "missing");
        assert!(json["agents"][0]["remedy"]
            .as_str()
            .unwrap()
            .contains("`rift enable --agent codex`"));
        assert!(
            !codex_hooks(dir.path()).exists(),
            "doctor writes no hook file"
        );
    }

    #[test]
    fn a_shadowed_hook_is_named_and_left_untouched() {
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

        let (human, json) = doctor_hook_composition(dir.path());
        assert!(human.contains("codex shadowed;"), "{human}");
        assert!(human.contains("`rift enable --agent codex`"), "{human}");
        assert_eq!(json["agents"][0]["composition"], "shadowed");
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }
}
