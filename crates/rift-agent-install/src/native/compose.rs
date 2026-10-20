//! How Rift's hook entries sit beside everyone else's in one client's hook
//! file, in one closed word. Read-only: the ownership verdict is the one
//! `diagnose_all` already reached, and the only extra fact read here is
//! whether a hook Rift does not own answers the same tool calls Rift's do.

use std::path::Path;

use serde::Serialize;

use super::hooks::{client_hook_path, hook_family};
use super::{NativeHookWiring, NativeIntegrationStatus};
use crate::{group_contains_rift_command, is_tool_event, standard_events, ClientFamily};

/// One client's hook composition, as doctor names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum HookComposition {
    /// Rift's entries are present, owned, and no foreign hook shares a slot.
    Owned,
    /// The client file holds no Rift entry.
    Missing,
    /// A recognisable older Rift bundle that `rift enable` refreshes.
    Stale,
    /// Rift-like entries whose ownership no fingerprint proves: changed by
    /// hand, unreadable, or behind a symlink.
    ForeignModified,
    /// Rift's entries are owned, but a hook Rift does not own answers the
    /// same tool calls in the same event slot.
    Shadowed,
}

impl HookComposition {
    /// The closed word every surface prints.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Owned => "owned",
            Self::Missing => "missing",
            Self::Stale => "stale",
            Self::ForeignModified => "foreign-modified",
            Self::Shadowed => "shadowed",
        }
    }
}

/// The composition of `status`'s hook file in `repo`; `None` for an adapter
/// with no separately managed hook file.
#[must_use]
pub fn hook_composition(repo: &Path, status: &NativeIntegrationStatus) -> Option<HookComposition> {
    let wiring = status.hook_wiring?;
    let family = hook_family(&status.agent)?;
    Some(match wiring {
        NativeHookWiring::Missing => HookComposition::Missing,
        NativeHookWiring::Stale => HookComposition::Stale,
        NativeHookWiring::Conflict | NativeHookWiring::Invalid => HookComposition::ForeignModified,
        NativeHookWiring::Managed if foreign_hook_shares_a_slot(repo, family) => {
            HookComposition::Shadowed
        }
        NativeHookWiring::Managed => HookComposition::Owned,
    })
}

/// Whether, in a tool event Rift hooks, a group Rift does not own can match
/// a call one of Rift's groups matches. Lifecycle events are left out on
/// purpose: two session-start hooks both run and neither drops the other's
/// answer, so a neighbour there is unrelated, not a conflict.
fn foreign_hook_shares_a_slot(repo: &Path, family: ClientFamily) -> bool {
    let Ok(bytes) = std::fs::read(client_hook_path(repo, family)) else {
        return false;
    };
    let Ok(root) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return false;
    };
    standard_events(family)
        .into_iter()
        .filter(|event| is_tool_event(event))
        .filter_map(|event| root.pointer(&format!("/hooks/{event}"))?.as_array())
        .any(|groups| {
            let (ours, theirs): (Vec<_>, Vec<_>) = groups
                .iter()
                .partition(|group| group_contains_rift_command(group, family));
            ours.iter()
                .any(|own| theirs.iter().any(|other| matchers_overlap(own, other)))
        })
}

/// Two groups overlap when either matches every tool or both name the same
/// matcher; anything finer is the client's own language and not claimed.
fn matchers_overlap(a: &serde_json::Value, b: &serde_json::Value) -> bool {
    let matcher = |group: &serde_json::Value| {
        group
            .get("matcher")
            .and_then(serde_json::Value::as_str)
            .filter(|m| !m.is_empty() && *m != "*")
            .map(str::to_owned)
    };
    match (matcher(a), matcher(b)) {
        (Some(left), Some(right)) => left == right,
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use crate::pretty_json;

    use super::super::fixtures::{claude_hook_path, codex_hook_path, row_for};
    use super::super::lifecycle::enable;
    use super::*;

    fn composition(dir: &Path, agent: &str) -> Option<HookComposition> {
        hook_composition(dir, &row_for(dir, agent))
    }

    fn write(path: &Path, value: &serde_json::Value) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, pretty_json(value)).unwrap();
    }

    fn read(path: &Path) -> serde_json::Value {
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
    }

    fn foreign_tool_group(matcher: &str) -> serde_json::Value {
        serde_json::json!({
            "matcher": matcher,
            "hooks": [{"type": "command", "command": "someone-elses-guard"}]
        })
    }

    #[test]
    fn claude_code_hooks_read_missing_owned_shadowed_and_foreign_modified() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            composition(dir.path(), "claude-code"),
            Some(HookComposition::Missing)
        );
        enable(dir.path(), Path::new("/opt/rift"), "claude-code").unwrap();
        assert_eq!(
            composition(dir.path(), "claude-code"),
            Some(HookComposition::Owned)
        );

        let path = claude_hook_path(dir.path());
        let mut settings = read(&path);
        settings["hooks"]["PreToolUse"]
            .as_array_mut()
            .unwrap()
            .push(foreign_tool_group("*"));
        write(&path, &settings);
        assert_eq!(
            composition(dir.path(), "claude-code"),
            Some(HookComposition::Shadowed)
        );

        let edited = pretty_json(&settings);
        let edited =
            String::from_utf8(edited)
                .unwrap()
                .replacen("/opt/rift", "/opt/hand-edited", 1);
        std::fs::write(&path, edited).unwrap();
        let verdict = composition(dir.path(), "claude-code").unwrap();
        assert_eq!(verdict, HookComposition::ForeignModified);
        let sentence = crate::health::hook_composition_sentence("claude-code", verdict).unwrap();
        assert!(
            sentence.contains("`rift enable --agent claude-code`"),
            "{sentence}"
        );
    }

    #[test]
    fn unrelated_codex_groups_are_preserved_and_raise_no_composition_finding() {
        let dir = tempfile::tempdir().unwrap();
        let path = codex_hook_path(dir.path());
        let user = serde_json::json!({
            "hooks": {
                "SessionStart": [{"hooks": [{"type": "command", "command": "user-start"}]}]
            }
        });
        write(&path, &user);
        enable(dir.path(), Path::new("/opt/rift"), "codex").unwrap();
        let installed = read(&path);
        assert!(installed["hooks"]["SessionStart"]
            .as_array()
            .unwrap()
            .contains(&user["hooks"]["SessionStart"][0]));
        assert_eq!(
            composition(dir.path(), "codex"),
            Some(HookComposition::Owned)
        );
        assert_eq!(
            read(&path),
            installed,
            "reading the composition writes nothing"
        );
    }

    #[test]
    fn a_codex_tool_hook_answering_the_same_calls_is_named_shadowed() {
        let dir = tempfile::tempdir().unwrap();
        let path = codex_hook_path(dir.path());
        write(
            &path,
            &serde_json::json!({ "hooks": { "PreToolUse": [foreign_tool_group("*")] } }),
        );
        enable(dir.path(), Path::new("/opt/rift"), "codex").unwrap();
        let verdict = composition(dir.path(), "codex").unwrap();
        assert_eq!(verdict, HookComposition::Shadowed);
        assert_eq!(verdict.as_str(), "shadowed");
        let sentence = crate::health::hook_composition_sentence("codex", verdict).unwrap();
        assert!(
            sentence.contains("`rift enable --agent codex`"),
            "{sentence}"
        );
    }

    #[test]
    fn every_word_but_owned_has_a_sentence_naming_rift_enable() {
        let words = [
            HookComposition::Missing,
            HookComposition::Stale,
            HookComposition::ForeignModified,
            HookComposition::Shadowed,
        ];
        for word in words {
            let sentence = crate::health::hook_composition_sentence("codex", word).unwrap();
            assert!(
                sentence.contains("`rift enable --agent codex`"),
                "{sentence}"
            );
        }
        assert_eq!(
            crate::health::hook_composition_sentence("codex", HookComposition::Owned),
            None
        );
    }

    #[test]
    fn an_adapter_without_a_hook_file_has_no_composition() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(composition(dir.path(), "ollama"), None);
    }
}
