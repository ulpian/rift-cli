//! Whether Codex will run the hooks Rift wrote into `.codex/hooks.json`,
//! handler by handler, and the one sentence that says what to do when it
//! will not.
//!
//! Codex runs a project hook only while the hash it recorded when the user
//! trusted the hook equals the hash of the hook as it now stands. A handler
//! Rift rewrites — a new field, a new binary path — is therefore skipped
//! until the user trusts it again, and Codex skips it without a message. The
//! comparison is made here, read-only, at the moments a person is looking:
//! every write of the hook file and every status read. Nothing on a hook's
//! decision path calls into this module, and Rift never writes Codex's
//! configuration: trust is the user's to give, in Codex.
//!
//! The comparison is only as good as the reproduced hash, so it is made only
//! for the Codex versions the fixtures pin ([`PROVEN_CODEX_VERSIONS`]). For
//! any other version, or when no Codex session has said which version runs
//! here, every handler is "unverifiable", in those words, never a guess.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::{read_json_object, ClientFamily};

use super::codex_client::observed_codex_client;
use super::codex_identity::{event_key_label, handler_identity_hash, PROVEN_CODEX_VERSIONS};

/// Codex's verdict on one handler Rift wrote, reached the way Codex reaches
/// it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodexHookVerdict {
    /// The recorded hash equals the handler's: Codex runs it.
    Trusted,
    /// A hash is recorded and differs: the handler changed after the user
    /// trusted it, and Codex skips it.
    Modified,
    /// No hash is recorded for the handler: Codex skips it until trusted.
    Untrusted,
    /// Rift cannot reproduce Codex's answer: the Codex version is not one
    /// the hash is proven against, or Codex's record could not be read.
    Unverifiable,
}

/// One handler's verdict, named by the hook-file event it answers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodexHookTrust {
    /// The event key in `.codex/hooks.json` (`SessionStart`, …).
    pub event: String,
    /// What Codex does with the handler.
    pub verdict: CodexHookVerdict,
}

/// Which Codex client ran the sessions in this checkout; the remedy differs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodexSurface {
    /// The command-line interface, or any client that is not the extension.
    Terminal,
    /// The VS Code extension, which also has to reload to pick up trust.
    VsCode,
}

/// The Codex client a session's own record names.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodexClient {
    /// Where the session ran.
    pub surface: CodexSurface,
    /// The Codex version the session ran, when its record carries one.
    pub version: Option<String>,
}

/// Codex's trust in every handler Rift wrote into one `.codex/hooks.json`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodexTrust {
    /// One verdict per Rift handler, in hook-file order.
    pub hooks: Vec<CodexHookTrust>,
    /// The client the newest recorded Codex session names; `None` when no
    /// Codex session has been recorded here.
    pub client: Option<CodexClient>,
}

impl CodexTrust {
    /// The events whose handlers Codex is skipping: modified or untrusted.
    #[must_use]
    pub fn needs_review(&self) -> Vec<&str> {
        self.hooks
            .iter()
            .filter(|hook| {
                matches!(
                    hook.verdict,
                    CodexHookVerdict::Modified | CodexHookVerdict::Untrusted
                )
            })
            .map(|hook| hook.event.as_str())
            .collect()
    }

    /// The row's word: `error` when Codex skips any of Rift's hooks,
    /// `unverifiable` when Rift cannot tell, `trusted` otherwise.
    #[must_use]
    pub fn state(&self) -> &'static str {
        if !self.needs_review().is_empty() {
            return "error";
        }
        if self.count(CodexHookVerdict::Unverifiable) > 0 {
            return "unverifiable";
        }
        "trusted"
    }

    /// The remedy, when Codex is skipping any of Rift's hooks. The one place
    /// the sentence is written; every surface prints this.
    #[must_use]
    pub fn remedy(&self) -> Option<String> {
        crate::health::remedy::codex_skipped_sentence(
            self.count(CodexHookVerdict::Modified),
            self.count(CodexHookVerdict::Untrusted),
            self.surface(),
        )
    }

    /// One line for a status row: the state and, when it is not `trusted`,
    /// which hooks or why.
    #[must_use]
    pub fn summary(&self) -> String {
        let total = self.hooks.len();
        let review = self.needs_review();
        if !review.is_empty() {
            let named: Vec<String> = self
                .hooks
                .iter()
                .filter(|hook| review.contains(&hook.event.as_str()))
                .map(|hook| format!("{} {}", hook.event, verdict_word(hook.verdict)))
                .collect();
            return format!(
                "error — Codex is skipping {} of {total} Rift hooks: {}",
                review.len(),
                named.join(", ")
            );
        }
        if self.count(CodexHookVerdict::Unverifiable) > 0 {
            return format!("unverifiable — {}", self.unverifiable_reason());
        }
        format!("trusted ({total} of {total} hooks)")
    }

    fn unverifiable_reason(&self) -> String {
        match self.client.as_ref().map(|client| client.version.as_deref()) {
            None => "no Codex session has been recorded in this checkout yet".to_owned(),
            Some(None) => "the recorded Codex session names no Codex version".to_owned(),
            Some(Some(version)) if !is_proven(version) => format!(
                "Codex {version} is not a version Rift's hook-trust check is proven against"
            ),
            Some(Some(_)) => "Codex's hook-trust record could not be read".to_owned(),
        }
    }

    fn surface(&self) -> CodexSurface {
        self.client
            .as_ref()
            .map_or(CodexSurface::Terminal, |client| client.surface)
    }

    fn count(&self, verdict: CodexHookVerdict) -> usize {
        self.hooks
            .iter()
            .filter(|hook| hook.verdict == verdict)
            .count()
    }
}

/// Codex's home: `$CODEX_HOME`, else `.codex` in the user's home directory.
/// Read-only, always.
#[must_use]
pub fn codex_home_from_process() -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os("CODEX_HOME") {
        return Some(PathBuf::from(explicit));
    }
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(|home| PathBuf::from(home).join(".codex"))
}

/// Codex's trust in the hooks Rift wrote into `repo`'s `.codex/hooks.json`,
/// judged for the client the checkout's newest recorded Codex session names.
/// `None` when the file holds no Rift handler.
#[must_use]
pub fn codex_trust(repo: &Path, codex_home: Option<&Path>) -> Option<CodexTrust> {
    let hooks_json = repo.join(ClientFamily::Codex.hook_path());
    if !hooks_json.is_file() {
        return None;
    }
    let trust = codex_hook_trust(&hooks_json, codex_home, observed_codex_client(repo));
    if trust.hooks.is_empty() {
        return None;
    }
    Some(trust)
}

/// Codex's trust in every Rift handler of `hooks_json`, for `client`.
///
/// Codex keys a record by the absolute path it resolved, the event label and
/// the group and handler positions, so both spellings of the path are tried:
/// a temporary directory is `/var/…` and `/private/var/…` at once on macOS.
/// No `codex_home` means no read at all, and every handler is unverifiable.
#[must_use]
pub fn codex_hook_trust(
    hooks_json: &Path,
    codex_home: Option<&Path>,
    client: Option<CodexClient>,
) -> CodexTrust {
    let proven = client
        .as_ref()
        .and_then(|client| client.version.as_deref())
        .is_some_and(is_proven);
    let recorded = codex_home.and_then(recorded_trust);
    let mut prefixes = vec![hooks_json.display().to_string()];
    if let Ok(canonical) = std::fs::canonicalize(hooks_json) {
        prefixes.push(canonical.display().to_string());
    }
    let hooks = rift_handlers(hooks_json)
        .into_iter()
        .map(|handler| {
            let verdict = match (&recorded, proven) {
                (Some(recorded), true) => judge(&handler, recorded, &prefixes),
                _ => CodexHookVerdict::Unverifiable,
            };
            CodexHookTrust {
                event: handler.event,
                verdict,
            }
        })
        .collect();
    CodexTrust { hooks, client }
}

/// One handler Rift wrote, with what Codex keys and hashes it by.
struct RiftHandler {
    event: String,
    key_suffix: Option<String>,
    identity: Option<String>,
}

fn judge(
    handler: &RiftHandler,
    recorded: &BTreeMap<String, Option<String>>,
    prefixes: &[String],
) -> CodexHookVerdict {
    let (Some(suffix), Some(identity)) = (&handler.key_suffix, &handler.identity) else {
        return CodexHookVerdict::Unverifiable;
    };
    let trusted = prefixes
        .iter()
        .find_map(|prefix| recorded.get(&format!("{prefix}:{suffix}")));
    match trusted {
        Some(Some(hash)) if hash == identity => CodexHookVerdict::Trusted,
        Some(Some(_)) => CodexHookVerdict::Modified,
        Some(None) | None => CodexHookVerdict::Untrusted,
    }
}

/// Every handler in `hooks_json` whose command runs Rift's Codex hook.
fn rift_handlers(hooks_json: &Path) -> Vec<RiftHandler> {
    let Ok(root) = read_json_object(hooks_json) else {
        return Vec::new();
    };
    let Some(events) = root.get("hooks").and_then(Value::as_object) else {
        return Vec::new();
    };
    let mut handlers = Vec::new();
    for (event, groups) in events {
        let groups = groups.as_array().map(Vec::as_slice).unwrap_or_default();
        for (group_index, group) in groups.iter().enumerate() {
            let Some(group) = group.as_object() else {
                continue;
            };
            collect_group(event, group_index, group, &mut handlers);
        }
    }
    handlers
}

fn collect_group(
    event: &str,
    group_index: usize,
    group: &Map<String, Value>,
    acc: &mut Vec<RiftHandler>,
) {
    let marker = format!("\" hooks {} ", ClientFamily::Codex.token());
    let handlers = group
        .get("hooks")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    for (handler_index, handler) in handlers.iter().enumerate() {
        let Some(handler) = handler.as_object() else {
            continue;
        };
        let command = handler.get("command").and_then(Value::as_str);
        if !command.is_some_and(|command| command.contains(&marker)) {
            continue;
        }
        acc.push(RiftHandler {
            event: event.to_owned(),
            key_suffix: event_key_label(event)
                .map(|label| format!("{label}:{group_index}:{handler_index}")),
            identity: handler_identity_hash(event, group, handler),
        });
    }
}

/// Codex's `[hooks.state]` table: record key to recorded hash. `None` when
/// the configuration exists and cannot be read or parsed; an absent file is
/// an empty record, since Codex has then trusted nothing.
fn recorded_trust(codex_home: &Path) -> Option<BTreeMap<String, Option<String>>> {
    let path = codex_home.join("config.toml");
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Some(BTreeMap::new());
        }
        Err(_) => return None,
    };
    let doc = text.parse::<toml_edit::DocumentMut>().ok()?;
    let state = doc
        .get("hooks")
        .and_then(toml_edit::Item::as_table_like)
        .and_then(|hooks| hooks.get("state"))
        .and_then(toml_edit::Item::as_table_like);
    let Some(state) = state else {
        return Some(BTreeMap::new());
    };
    let recorded = state
        .iter()
        .map(|(key, entry)| {
            let hash = entry
                .as_table_like()
                .and_then(|entry| entry.get("trusted_hash"))
                .and_then(toml_edit::Item::as_str)
                .map(str::to_owned);
            (key.to_owned(), hash)
        })
        .collect();
    Some(recorded)
}

fn is_proven(version: &str) -> bool {
    PROVEN_CODEX_VERSIONS.contains(&version)
}

fn verdict_word(verdict: CodexHookVerdict) -> &'static str {
    match verdict {
        CodexHookVerdict::Trusted => "trusted",
        CodexHookVerdict::Modified => "modified",
        CodexHookVerdict::Untrusted => "untrusted",
        CodexHookVerdict::Unverifiable => "unverifiable",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE_HOOKS: &str = include_str!("../../fixtures/codex-trust/hooks.json");
    const FIXTURE_CONFIG: &str = include_str!("../../fixtures/codex-trust/config.toml");
    const FIXTURE_CONFIG_0157: &str =
        include_str!("../../fixtures/codex-trust/config-0.157.0.toml");
    const FIXTURE_HASHES: &str = include_str!("../../fixtures/codex-trust/expected-hashes.txt");

    /// A checkout holding the fixture hook file, and a Codex home beside it.
    struct Machine {
        _tmp: tempfile::TempDir,
        repo: PathBuf,
        hooks_json: PathBuf,
        home: PathBuf,
    }

    fn machine(hooks: &str) -> Machine {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let hooks_json = repo.join(".codex/hooks.json");
        std::fs::create_dir_all(hooks_json.parent().unwrap()).unwrap();
        std::fs::write(&hooks_json, hooks).unwrap();
        let home = tmp.path().join("codex-home");
        std::fs::create_dir_all(&home).unwrap();
        Machine {
            _tmp: tmp,
            repo,
            hooks_json,
            home,
        }
    }

    /// Write `config` as Codex's configuration, its records keyed by `path`.
    /// A TOML basic string escapes its backslashes, as Codex does when it
    /// records a Windows path.
    fn record(machine: &Machine, config: &str, path: &Path) -> String {
        let key = path.display().to_string().replace('\\', "\\\\");
        let text = config.replace("@HOOKS_JSON@", &key);
        std::fs::write(machine.home.join("config.toml"), &text).unwrap();
        text
    }

    fn client(surface: CodexSurface, version: &str) -> Option<CodexClient> {
        Some(CodexClient {
            surface,
            version: Some(version.to_owned()),
        })
    }

    fn expected_hashes() -> Vec<(String, String)> {
        FIXTURE_HASHES
            .lines()
            .filter(|line| !line.starts_with('#'))
            .filter_map(|line| line.split_once(' '))
            .map(|(event, hash)| (event.to_owned(), hash.to_owned()))
            .collect()
    }

    /// A config recording every handler exactly as it stands.
    fn current_config() -> String {
        let mut config = String::new();
        for (event, hash) in expected_hashes() {
            let label = event_key_label(&event).unwrap();
            config.push_str(&format!(
                "[hooks.state.\"@HOOKS_JSON@:{label}:0:0\"]\ntrusted_hash = \"{hash}\"\n"
            ));
        }
        config
    }

    /// Codex's hash, reproduced for all ten handlers Rift writes, against
    /// the values recorded from Codex itself.
    #[test]
    fn every_rift_handler_hashes_as_the_fixture_records_codex_hashing_it() {
        let machine = machine(FIXTURE_HOOKS);
        let computed: Vec<(String, String)> = rift_handlers(&machine.hooks_json)
            .into_iter()
            .map(|handler| (handler.event, handler.identity.unwrap()))
            .collect();
        assert_eq!(computed, expected_hashes());
        assert_eq!(computed.len(), 10);
    }

    /// The machine this check exists for: two handlers gained
    /// `additionalContextLimit` after they were trusted, Codex dropped both
    /// without a word, and every surface said the integration was fine. The
    /// two recorded hashes are the handlers without the limit — reproducing
    /// them is what proves the reconstruction.
    #[test]
    fn the_two_handlers_that_gained_a_context_limit_read_as_modified() {
        let machine = machine(FIXTURE_HOOKS);
        let config = record(&machine, FIXTURE_CONFIG, &machine.hooks_json);
        for version in PROVEN_CODEX_VERSIONS {
            let trust = codex_hook_trust(
                &machine.hooks_json,
                Some(&machine.home),
                client(CodexSurface::VsCode, version),
            );
            assert_eq!(
                trust.needs_review(),
                ["SessionStart", "UserPromptSubmit"],
                "{version}"
            );
            assert_eq!(trust.count(CodexHookVerdict::Trusted), 8, "{version}");
            assert_eq!(trust.state(), "error");
        }
        let root = read_json_object(&machine.hooks_json).unwrap();
        for (event, label) in [
            ("SessionStart", "session_start"),
            ("UserPromptSubmit", "user_prompt_submit"),
        ] {
            let group = root["hooks"][event][0].as_object().unwrap();
            let mut handler = group["hooks"][0].as_object().unwrap().clone();
            handler.remove("additionalContextLimit");
            let before = handler_identity_hash(event, group, &handler).unwrap();
            assert!(
                config.contains(&format!("{label}:0:0\"]\ntrusted_hash = \"{before}\"")),
                "{event}: the recorded hash is the handler without the limit"
            );
        }
    }

    /// The record Codex 0.157.0 wrote after the user re-trusted both
    /// modified handlers: all ten current. The older versions hash alike, so
    /// the same record judged as one of them is trusted too.
    #[test]
    fn a_codex_0157_record_taken_after_re_trust_reads_all_ten_hooks_trusted() {
        let machine = machine(FIXTURE_HOOKS);
        record(&machine, FIXTURE_CONFIG_0157, &machine.hooks_json);
        for version in ["0.157.0", "0.155.0-alpha.16.3"] {
            let trust = codex_hook_trust(
                &machine.hooks_json,
                Some(&machine.home),
                client(CodexSurface::Terminal, version),
            );
            assert_eq!(trust.state(), "trusted", "{version}");
            assert_eq!(trust.summary(), "trusted (10 of 10 hooks)", "{version}");
            assert_eq!(trust.hooks.len(), 10, "{version}");
            assert!(
                trust
                    .hooks
                    .iter()
                    .all(|hook| hook.verdict == CodexHookVerdict::Trusted),
                "{version}"
            );
        }
    }

    #[test]
    fn editing_a_trusted_handler_flips_it_to_modified() {
        let machine = machine(FIXTURE_HOOKS);
        record(&machine, &current_config(), &machine.hooks_json);
        let judge = || {
            codex_hook_trust(
                &machine.hooks_json,
                Some(&machine.home),
                client(CodexSurface::Terminal, "0.153.1"),
            )
        };
        assert_eq!(judge().state(), "trusted");
        assert_eq!(judge().summary(), "trusted (10 of 10 hooks)");

        let edited = FIXTURE_HOOKS.replacen("\"timeout\": 10", "\"timeout\": 11", 1);
        std::fs::write(&machine.hooks_json, edited).unwrap();
        let trust = judge();
        assert_eq!(trust.needs_review(), ["PostCompact"]);
        assert_eq!(
            trust.summary(),
            "error — Codex is skipping 1 of 10 Rift hooks: PostCompact modified"
        );
    }

    /// Only the versions the fixtures were recorded against are judged; any
    /// other says unverifiable, and says why, rather than guessing.
    #[test]
    fn an_unproven_codex_version_is_unverifiable_and_says_so() {
        let machine = machine(FIXTURE_HOOKS);
        record(&machine, FIXTURE_CONFIG, &machine.hooks_json);
        let trust = codex_hook_trust(
            &machine.hooks_json,
            Some(&machine.home),
            client(CodexSurface::Terminal, "0.158.0"),
        );
        assert!(trust
            .hooks
            .iter()
            .all(|hook| hook.verdict == CodexHookVerdict::Unverifiable));
        assert_eq!(trust.state(), "unverifiable");
        assert_eq!(trust.remedy(), None);
        assert_eq!(
            trust.summary(),
            "unverifiable — Codex 0.158.0 is not a version Rift's hook-trust check is proven against"
        );

        let unseen = codex_hook_trust(&machine.hooks_json, Some(&machine.home), None);
        assert_eq!(
            unseen.summary(),
            "unverifiable — no Codex session has been recorded in this checkout yet"
        );
    }

    #[test]
    fn the_remedy_names_the_terminal_and_adds_vscode_only_for_vscode() {
        let machine = machine(FIXTURE_HOOKS);
        record(&machine, FIXTURE_CONFIG, &machine.hooks_json);
        let remedy = |surface| {
            codex_hook_trust(
                &machine.hooks_json,
                Some(&machine.home),
                client(surface, "0.155.0-alpha.16.3"),
            )
            .remedy()
            .unwrap()
        };
        let terminal = "Codex is skipping 2 of Rift's hooks because they changed after you \
                        approved them, so Rift cannot see your Codex sessions start. In a \
                        terminal in this repository run `codex`, type `/hooks`, and trust the \
                        hooks marked 'Modified since last trusted'.";
        assert_eq!(remedy(CodexSurface::Terminal), terminal);
        assert_eq!(
            remedy(CodexSurface::VsCode),
            format!(
                "{terminal} Then reload the VS Code window and start a new Codex conversation."
            )
        );
    }

    /// Codex's trust is Codex's to give: the record is read under either
    /// spelling of the path (a temp path is `/var/…` and `/private/var/…` on
    /// macOS), a path with no record is untrusted, no Codex home means no read
    /// at all, and the configuration comes back byte-identical.
    #[test]
    fn codex_trust_is_read_from_codex_state_and_never_written() {
        let machine = machine(FIXTURE_HOOKS);
        let canonical = std::fs::canonicalize(&machine.hooks_json).unwrap();
        let proven = || client(CodexSurface::Terminal, "0.153.1");
        for (recorded, label) in [(&machine.hooks_json, "as given"), (&canonical, "canonical")] {
            let bytes = record(&machine, &current_config(), recorded);
            let trust = codex_hook_trust(&machine.hooks_json, Some(&machine.home), proven());
            assert_eq!(trust.state(), "trusted", "{label}");
            assert_eq!(
                std::fs::read_to_string(machine.home.join("config.toml")).unwrap(),
                bytes,
                "{label}: Rift never writes Codex's configuration"
            );
        }
        let elsewhere = machine.repo.join("other/.codex/hooks.json");
        std::fs::create_dir_all(elsewhere.parent().unwrap()).unwrap();
        std::fs::write(&elsewhere, FIXTURE_HOOKS).unwrap();
        let untrusted = codex_hook_trust(&elsewhere, Some(&machine.home), proven());
        assert_eq!(untrusted.needs_review().len(), 10);
        assert!(untrusted
            .remedy()
            .unwrap()
            .contains("you have not approved them yet"));

        let homeless = codex_hook_trust(&machine.hooks_json, None, proven());
        assert_eq!(homeless.state(), "unverifiable");
        assert_eq!(
            codex_trust(&machine.repo.join("nowhere"), Some(&machine.home)),
            None
        );
    }
}
