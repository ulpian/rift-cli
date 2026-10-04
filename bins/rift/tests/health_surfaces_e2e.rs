//! End-to-end of integration health on the command line: `rift status`,
//! `rift recall status` and `rift doctor` print the same finding in the same
//! words and carry the same `--json` array, a healthy repository says so on
//! each, and no command, `--fix` included, writes a byte of what the finding
//! was read from.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Output;

mod common;

/// A home and a repository under it, both temporary: the process's `HOME`,
/// Codex's home, Rift's config and cache all point inside `home`, so no read
/// reaches the developer's own agents.
struct Fixture {
    home: tempfile::TempDir,
    repo: tempfile::TempDir,
}

impl Fixture {
    /// A repository set up on this machine with Claude Code, and only Claude
    /// Code, wired to the binary under test.
    fn wired() -> Self {
        let fixture = Self {
            home: tempfile::tempdir().unwrap(),
            repo: tempfile::tempdir().unwrap(),
        };
        rift_git::test_support::fake_repo(fixture.repo.path()).unwrap();
        assert_eq!(
            fixture.rift(&["init", "--no-recall"]).status.code(),
            Some(0)
        );
        let enable = fixture.rift(&["enable", "--agent", "claude-code"]);
        assert_eq!(enable.status.code(), Some(0), "{enable:?}");
        // `init` wires whatever agents the inherited `PATH` shows; the two
        // others are taken back out so the fixture does not depend on the
        // machine it runs on.
        for agent in ["codex", "antigravity"] {
            let disable = fixture.rift(&["disable", "--agent", agent]);
            assert_eq!(disable.status.code(), Some(0), "{disable:?}");
        }
        fixture
    }

    /// `rift <args>`, hermetic.
    fn rift(&self, args: &[&str]) -> Output {
        let home = self.home.path();
        common::rift_cmd(self.repo.path())
            .args(args)
            .env("HOME", home)
            .env("USERPROFILE", home)
            .env("CODEX_HOME", home.join("codex-home"))
            .env("RIFT_CONFIG_DIR", home.join("config"))
            .env("RIFT_CACHE_DIR", home.join("cache"))
            .env("RIFT_HEADLESS", "1")
            .output()
            .expect("spawn rift")
    }

    /// Stdout of a command that must exit 0.
    fn stdout(&self, args: &[&str]) -> String {
        let out = self.rift(args);
        assert_eq!(out.status.code(), Some(0), "{args:?}: {out:?}");
        String::from_utf8(out.stdout).unwrap()
    }

    /// The `--json` document of a command that must exit 0.
    fn json(&self, args: &[&str]) -> serde_json::Value {
        serde_json::from_str(&self.stdout(args)).unwrap()
    }

    /// Claude Code's hook file.
    fn hook_file(&self) -> PathBuf {
        self.repo.path().join(".claude/settings.local.json")
    }

    /// Point every Claude Code hook handler at a binary that does not exist,
    /// rewriting the decoded command strings rather than the JSON text: on
    /// Windows the file holds `"D:\\…\\rift.exe"` escaped, which no search
    /// for a path spelling finds.
    fn break_the_hook_binary(&self) -> String {
        let missing = native_spelling(&self.home.path().join("gone").join("rift"));
        let binary = native_spelling(Path::new(common::rift_bin()));
        let text = std::fs::read_to_string(self.hook_file()).unwrap();
        let mut settings: serde_json::Value = serde_json::from_str(&text).unwrap();
        let commands = handler_commands(&mut settings);
        let handlers = commands.len();
        let mut rewritten = 0;
        for command in commands {
            let Some(retargeted) = retarget_command(command, &binary, &missing) else {
                continue;
            };
            *command = retargeted;
            rewritten += 1;
        }
        assert!(handlers > 0, "{text}");
        assert_eq!(rewritten, handlers, "{binary} in {text}");
        let json = serde_json::to_string_pretty(&settings).unwrap();
        std::fs::write(self.hook_file(), json).unwrap();
        missing
    }

    /// Every file the health read takes as input, with its bytes: the hook
    /// files, the binary of record and Codex's home.
    fn inputs(&self) -> BTreeMap<PathBuf, Vec<u8>> {
        let mut files = BTreeMap::new();
        for dir in [
            self.repo.path().join(".claude"),
            self.home.path().to_path_buf(),
        ] {
            collect(&dir, &mut files);
        }
        files.retain(|path, _| {
            path.starts_with(self.repo.path().join(".claude"))
                || path.starts_with(self.home.path().join("codex-home"))
                || path.file_name().is_some_and(|name| name == "binary.json")
        });
        files
    }
}

/// Every regular file under `dir`, with its bytes.
fn collect(dir: &Path, files: &mut BTreeMap<PathBuf, Vec<u8>>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, files);
        } else if let Ok(bytes) = std::fs::read(&path) {
            files.insert(path, bytes);
        }
    }
}

/// The sentence printed under `headline` in `text`.
fn sentence_under(text: &str, headline: &str) -> String {
    let mut lines = text.lines();
    lines
        .by_ref()
        .find(|line| line.trim_end().ends_with(headline))
        .unwrap_or_else(|| panic!("no {headline:?} in {text}"));
    lines.next().unwrap().trim().to_owned()
}

/// A path as this OS names it to a child process: `display()` with the
/// verbatim prefix dropped. Spelled here, not borrowed from the installer, so
/// the fixture checks what the installer wrote rather than agreeing with it.
fn native_spelling(path: &Path) -> String {
    let spelled = path.display().to_string();
    spelled
        .strip_prefix(r"\\?\")
        .map_or_else(|| spelled.clone(), str::to_owned)
}

/// Every `hooks.<event>[].hooks[].command` string in a Claude Code settings
/// document.
fn handler_commands(settings: &mut serde_json::Value) -> Vec<&mut String> {
    let Some(events) = settings["hooks"].as_object_mut() else {
        return Vec::new();
    };
    events
        .values_mut()
        .filter_map(serde_json::Value::as_array_mut)
        .flatten()
        .filter_map(|group| group["hooks"].as_array_mut())
        .flatten()
        .filter_map(|handler| match &mut handler["command"] {
            serde_json::Value::String(command) => Some(command),
            _ => None,
        })
        .collect()
}

/// `command` with its executable replaced by `missing` when that executable
/// is `binary` — double-quoted, single-quoted or bare, the quoting kept — and
/// `None` when the command runs anything else.
fn retarget_command(command: &str, binary: &str, missing: &str) -> Option<String> {
    let binary = binary.strip_prefix(r"\\?\").unwrap_or(binary);
    for quote in ["\"", "'"] {
        let Some(rest) = command
            .strip_prefix(quote)
            .and_then(|tail| tail.strip_prefix(binary))
            .and_then(|tail| tail.strip_prefix(quote))
        else {
            continue;
        };
        return ends_the_word(rest).then(|| format!("{quote}{missing}{quote}{rest}"));
    }
    let rest = command.strip_prefix(binary)?;
    ends_the_word(rest).then(|| format!("{missing}{rest}"))
}

/// Whether what follows an executable leaves it a whole word.
fn ends_the_word(rest: &str) -> bool {
    rest.is_empty() || rest.starts_with(' ')
}

#[test]
fn a_missing_hook_binary_reads_the_same_in_status_and_doctor() {
    let fixture = Fixture::wired();
    let missing = fixture.break_the_hook_binary();

    let status = fixture.stdout(&["status"]);
    let doctor = fixture.stdout(&["doctor"]);
    let headline = "Claude Code: hook binary missing";
    let sentence = sentence_under(&status, headline);
    assert_eq!(sentence, sentence_under(&doctor, headline));
    assert!(
        sentence.contains("`rift enable --agent claude-code`"),
        "{sentence}"
    );

    let status = fixture.json(&["status", "--json"]);
    let doctor = fixture.json(&["doctor", "--json"]);
    assert_eq!(status["health"], doctor["integration_health"]);
    let health = status["health"].as_array().unwrap();
    let element = health
        .iter()
        .find(|element| element["reason"] == "hook-binary-missing")
        .unwrap_or_else(|| panic!("{health:?}"));
    assert_eq!(element["agent"], "claude-code");
    assert_eq!(element["severity"], "error");
    assert_eq!(element["sentence"], sentence.as_str());
    assert_eq!(element["detail"], missing.as_str());
    for key in ["checkpoints", "sessions", "captured_events"] {
        assert!(status[key].is_i64(), "{key} kept: {status}");
    }
}

#[test]
fn a_healthy_repository_prints_none_and_an_empty_array() {
    let fixture = Fixture::wired();
    for args in [["status"], ["doctor"]] {
        let text = fixture.stdout(&args);
        assert!(
            text.lines()
                .any(|line| line == "  integration health: none"),
            "{args:?}: {text}"
        );
    }
    let status = fixture.json(&["status", "--json"]);
    let doctor = fixture.json(&["doctor", "--json"]);
    assert_eq!(status["health"], serde_json::json!([]));
    assert_eq!(doctor["integration_health"], serde_json::json!([]));
}

#[test]
fn recall_status_and_doctor_print_the_same_health_sentence_and_array() {
    let fixture = Fixture::wired();
    fixture.break_the_hook_binary();

    let recall = fixture.stdout(&["recall", "status"]);
    let doctor = fixture.stdout(&["doctor"]);
    let headline = "Claude Code: hook binary missing";
    assert_eq!(
        sentence_under(&recall, headline),
        sentence_under(&doctor, headline)
    );

    let recall = fixture.json(&["recall", "status", "--json"]);
    let doctor = fixture.json(&["doctor", "--json"]);
    assert_eq!(recall["health"], doctor["integration_health"]);
    assert!(
        recall["health"]
            .as_array()
            .is_some_and(|health| !health.is_empty()),
        "{recall}"
    );
}

#[test]
fn a_healthy_repository_reads_none_on_recall_status_between_support_and_policy() {
    let fixture = Fixture::wired();
    let text = fixture.stdout(&["recall", "status"]);
    let line = |needle: &str| {
        text.lines()
            .position(|line| line.starts_with(needle))
            .unwrap_or_else(|| panic!("no {needle:?} in {text}"))
    };
    let health = line("  integration health: none");
    assert!(line("Support truth") < health, "{text}");
    assert!(health < line("Serving policy"), "{text}");
    let json = fixture.json(&["recall", "status", "--json"]);
    assert_eq!(json["health"], serde_json::json!([]));
}

#[test]
fn reading_health_writes_none_of_its_inputs_even_under_fix() {
    let fixture = Fixture::wired();
    fixture.break_the_hook_binary();
    let before = fixture.inputs();
    assert!(
        before.contains_key(&fixture.hook_file()),
        "{:?}",
        before.keys()
    );
    for args in [
        &["status", "--json"][..],
        &["recall", "status", "--json"][..],
        &["doctor", "--json"][..],
        &["doctor", "--fix", "--json"][..],
    ] {
        let out = fixture.rift(args);
        assert_eq!(out.status.code(), Some(0), "{args:?}: {out:?}");
        assert_eq!(fixture.inputs(), before, "{args:?} wrote an input");
    }
}

#[test]
fn a_quoted_windows_hook_command_is_retargeted_through_its_decoded_executable() {
    let command = r#""D:\a\rift-cli\rift-cli\target\debug\rift.exe" hooks claude-code PostToolUse"#;
    let binary = r"\\?\D:\a\rift-cli\rift-cli\target\debug\rift.exe";
    let missing = r"C:\Users\runner\gone\rift";
    let json = serde_json::to_string(command).unwrap();
    let spelled = common::shell_spelling(Path::new(binary));
    assert!(!json.contains(&spelled), "the raw-text search misses it");
    assert_eq!(
        retarget_command(command, binary, missing).as_deref(),
        Some(r#""C:\Users\runner\gone\rift" hooks claude-code PostToolUse"#)
    );
}

#[test]
fn a_hook_command_is_retargeted_only_when_its_whole_executable_is_the_binary() {
    let binary = "/opt/rift";
    assert_eq!(
        retarget_command("'/opt/rift' hooks claude-code Stop", binary, "/gone").as_deref(),
        Some("'/gone' hooks claude-code Stop")
    );
    assert_eq!(
        retarget_command("/opt/rift hooks claude-code Stop", binary, "/gone").as_deref(),
        Some("/gone hooks claude-code Stop")
    );
    for other in [
        "/opt/rift2 hooks claude-code Stop",
        "\"/opt/rift.old\" hooks claude-code Stop",
        "/usr/bin/env /opt/rift hooks claude-code Stop",
    ] {
        assert_eq!(retarget_command(other, binary, "/gone"), None, "{other}");
    }
}
