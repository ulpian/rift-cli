//! Which surface an agent runs on: read from the session record the agent
//! already wrote when a session cache names one, bounded, and from the
//! presence evidence otherwise.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::path::{Path, PathBuf};

use rift_agent_install::detect::{DetectionEnvironment, NativeIdeHost};
use rift_agent_install::health::{client_surface, Surface, SurfaceEvidence};
use rift_shadow_hooks::session_cache::SessionCache;

const CLAUDE_HEAD: &str = include_str!("../fixtures/health-surface/claude-code-head.jsonl");
const CODEX_VSCODE: &str = include_str!("../fixtures/health-surface/codex-vscode.jsonl");
const CODEX_EXEC: &str = include_str!("../fixtures/health-surface/codex-exec.jsonl");

/// How a case plants its evidence on a fresh [`Machine`].
type Plant = fn(&Machine);

/// A machine made of temporary directories: a home, one `PATH` directory and
/// one application root, all empty until a test plants evidence.
struct Machine {
    dir: tempfile::TempDir,
}

impl Machine {
    fn new() -> Self {
        let machine = Self {
            dir: tempfile::tempdir().unwrap(),
        };
        for sub in ["home", "bin", "apps", "transcripts"] {
            std::fs::create_dir_all(machine.dir.path().join(sub)).unwrap();
        }
        machine
    }

    fn home(&self) -> PathBuf {
        self.dir.path().join("home")
    }

    fn env(&self) -> DetectionEnvironment {
        DetectionEnvironment {
            search_path: Some(self.dir.path().join("bin").into_os_string()),
            home: Some(self.home()),
            app_roots: vec![self.dir.path().join("apps")],
            codex_home: None,
        }
    }

    fn plant(&self, relative: &str, bytes: &[u8]) -> PathBuf {
        let path = self.dir.path().join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, bytes).unwrap();
        path
    }

    fn path_command(&self, name: &str) {
        self.plant(&format!("bin/{name}"), b"");
    }

    fn vendor_claude(&self) {
        self.plant("home/.local/bin/claude", b"");
    }

    fn extension(&self, host_dir: &str, token: &str) {
        let (publisher, name) = match token {
            "claude-code" => ("anthropic", "claude-code"),
            "codex" => ("openai", "chatgpt"),
            other => panic!("no extension shape for {other}"),
        };
        let manifest =
            serde_json::json!({"publisher": publisher, "name": name, "version": "1.0.0"});
        self.plant(
            &format!("home/{host_dir}/extensions/{publisher}.{name}-1.0.0/package.json"),
            manifest.to_string().as_bytes(),
        );
    }

    fn jetbrains_plugin(&self) {
        self.plant(
            "home/.local/share/JetBrains/IntelliJIdea2025.2/plugins/claude-code-jetbrains-plugin/lib/claude-code-0.1.15.jar",
            b"",
        );
    }

    fn transcript(&self, name: &str, text: &str) -> PathBuf {
        self.plant(&format!("transcripts/{name}"), text.as_bytes())
    }
}

fn session(id: &str, agent: &str, updated: i64, transcript: Option<&Path>) -> SessionCache {
    let mut cache = SessionCache::new(id, agent, "active", updated);
    cache.transcript_path = transcript.map(|path| path.display().to_string());
    cache
}

/// The fixture head with its `entrypoint` value replaced on every line.
fn head_with(entrypoint: &str) -> String {
    CLAUDE_HEAD.replace("\"claude-vscode\"", &format!("\"{entrypoint}\""))
}

fn claude_surface_of(machine: &Machine, text: &str) -> (Surface, SurfaceEvidence) {
    let transcript = machine.transcript("claude.jsonl", text);
    let sessions = [session("s", "claude-code", 1, Some(&transcript))];
    client_surface("claude-code", &sessions, &machine.env())
}

#[test]
fn every_known_claude_code_entrypoint_names_its_surface() {
    let cases = [
        ("claude-vscode", Surface::Ide(NativeIdeHost::VsCode)),
        ("cli", Surface::Terminal),
        ("sdk-cli", Surface::Terminal),
        ("sdk-ts", Surface::Terminal),
        ("sdk-py", Surface::Terminal),
    ];
    for (entrypoint, expected) in cases {
        let machine = Machine::new();
        machine.jetbrains_plugin();
        assert_eq!(
            claude_surface_of(&machine, &head_with(entrypoint)),
            (expected, SurfaceEvidence::SessionRecord),
            "{entrypoint}"
        );
    }
}

/// The records Claude Code opens a transcript with carry no `entrypoint`;
/// the first one that does is the fifth.
#[test]
fn an_entrypoint_first_carried_on_line_five_is_found() {
    let first_carrier = CLAUDE_HEAD
        .lines()
        .position(|line| line.contains("\"entrypoint\""));
    assert_eq!(
        first_carrier,
        Some(4),
        "the fixture reproduces the late field"
    );
    let machine = Machine::new();
    assert_eq!(
        claude_surface_of(&machine, CLAUDE_HEAD),
        (
            Surface::Ide(NativeIdeHost::VsCode),
            SurfaceEvidence::SessionRecord
        )
    );
}

#[test]
fn an_unknown_entrypoint_falls_through_to_presence() {
    let machine = Machine::new();
    machine.path_command("claude");
    assert_eq!(
        claude_surface_of(&machine, &head_with("claude-desktop")),
        (Surface::Terminal, SurfaceEvidence::Presence)
    );
}

#[test]
fn an_entrypoint_past_the_line_bound_is_not_read() {
    let filler = CLAUDE_HEAD.lines().next().unwrap();
    let mut text = format!("{filler}\n").repeat(64);
    text.push_str(CLAUDE_HEAD.lines().nth(4).unwrap());
    text.push('\n');
    let machine = Machine::new();
    assert_eq!(
        claude_surface_of(&machine, &text),
        (Surface::Unknown, SurfaceEvidence::Presence)
    );

    let within = format!("{filler}\n").repeat(63) + CLAUDE_HEAD.lines().nth(4).unwrap() + "\n";
    assert_eq!(
        claude_surface_of(&machine, &within),
        (
            Surface::Ide(NativeIdeHost::VsCode),
            SurfaceEvidence::SessionRecord
        ),
        "the sixty-fourth line is still read"
    );
}

/// A head of several mebibytes with no line end is cut at the byte bound
/// rather than read whole.
#[test]
fn an_entrypoint_past_the_byte_bound_is_not_read() {
    let mut text = "x".repeat(4 * 1024 * 1024);
    text.push('\n');
    text.push_str(CLAUDE_HEAD.lines().nth(4).unwrap());
    text.push('\n');
    let machine = Machine::new();
    machine.jetbrains_plugin();
    assert_eq!(
        claude_surface_of(&machine, &text),
        (Surface::JetBrains, SurfaceEvidence::Presence)
    );
}

#[test]
fn an_unreadable_transcript_falls_through_to_presence() {
    let machine = Machine::new();
    machine.extension(".cursor", "claude-code");
    let missing = machine.dir.path().join("transcripts/missing.jsonl");
    let sessions = [session("s", "claude-code", 1, Some(&missing))];
    assert_eq!(
        client_surface("claude-code", &sessions, &machine.env()),
        (
            Surface::Ide(NativeIdeHost::Cursor),
            SurfaceEvidence::Presence
        )
    );
}

/// The newest Claude Code session that names a transcript is the one read:
/// not an older one, not a newer one naming none, not another agent's.
#[test]
fn the_newest_claude_code_session_names_the_surface() {
    let machine = Machine::new();
    let old = machine.transcript("old.jsonl", &head_with("cli"));
    let new = machine.transcript("new.jsonl", &head_with("claude-vscode"));
    let codex = machine.transcript("codex.jsonl", CODEX_EXEC);
    let sessions = [
        session("new", "claude-code", 20, Some(&new)),
        session("old", "claude-code", 10, Some(&old)),
        session("bare", "claude-code", 30, None),
        session("codex", "codex", 40, Some(&codex)),
    ];
    assert_eq!(
        client_surface("claude-code", &sessions, &machine.env()),
        (
            Surface::Ide(NativeIdeHost::VsCode),
            SurfaceEvidence::SessionRecord
        )
    );
}

#[test]
fn a_codex_session_names_its_client_through_the_same_function() {
    let machine = Machine::new();
    machine.path_command("codex");
    let vscode = machine.transcript("vscode.jsonl", CODEX_VSCODE);
    let exec = machine.transcript("exec.jsonl", CODEX_EXEC);
    let env = machine.env();
    assert_eq!(
        client_surface("codex", &[session("v", "codex", 1, Some(&vscode))], &env),
        (
            Surface::Ide(NativeIdeHost::VsCode),
            SurfaceEvidence::SessionRecord
        )
    );
    assert_eq!(
        client_surface("codex", &[session("e", "codex", 1, Some(&exec))], &env),
        (Surface::Terminal, SurfaceEvidence::SessionRecord)
    );
    let torn = machine.transcript("torn.jsonl", CODEX_VSCODE.lines().next().unwrap());
    assert_eq!(
        client_surface("codex", &[session("t", "codex", 1, Some(&torn))], &env),
        (Surface::Terminal, SurfaceEvidence::Presence),
        "a first line with no end names no client"
    );
}

#[test]
fn a_single_evidence_class_names_its_surface() {
    let cases: [(&str, Plant, Surface); 6] = [
        (
            "claude-code",
            |m| m.extension(".vscode", "claude-code"),
            Surface::Ide(NativeIdeHost::VsCode),
        ),
        (
            "codex",
            |m| m.extension(".vscode-insiders", "codex"),
            Surface::Ide(NativeIdeHost::VsCodeInsiders),
        ),
        (
            "codex",
            |m| m.extension(".windsurf", "codex"),
            Surface::Ide(NativeIdeHost::Windsurf),
        ),
        ("claude-code", Machine::jetbrains_plugin, Surface::JetBrains),
        ("gemini", |m| m.path_command("gemini"), Surface::Terminal),
        (
            "claude-code",
            |m| {
                m.path_command("claude");
                m.vendor_claude();
            },
            Surface::Terminal,
        ),
    ];
    for (token, plant, expected) in cases {
        let machine = Machine::new();
        plant(&machine);
        assert_eq!(
            client_surface(token, &[], &machine.env()),
            (expected, SurfaceEvidence::Presence),
            "{token} {expected:?}"
        );
    }
}

#[test]
fn mixed_or_missing_evidence_names_no_surface() {
    let cases: [(&str, Plant); 5] = [
        ("claude-code", |_| ()),
        ("claude-code", |m| {
            m.path_command("claude");
            m.extension(".vscode", "claude-code");
        }),
        ("claude-code", |m| {
            m.extension(".vscode", "claude-code");
            m.extension(".cursor", "claude-code");
        }),
        ("claude-code", |m| {
            m.extension(".vscode", "claude-code");
            m.jetbrains_plugin();
        }),
        ("claude-code", Machine::vendor_claude),
    ];
    for (index, (token, plant)) in cases.into_iter().enumerate() {
        let machine = Machine::new();
        plant(&machine);
        assert_eq!(
            client_surface(token, &[], &machine.env()),
            (Surface::Unknown, SurfaceEvidence::Presence),
            "case {index}"
        );
    }
}

#[test]
fn surfaces_serialize_as_stable_snake_case_strings() {
    let cases = [
        (Surface::Ide(NativeIdeHost::VsCode), "vscode"),
        (
            Surface::Ide(NativeIdeHost::VsCodeInsiders),
            "vscode_insiders",
        ),
        (Surface::Ide(NativeIdeHost::Cursor), "cursor"),
        (Surface::Ide(NativeIdeHost::Windsurf), "windsurf"),
        (Surface::Terminal, "terminal"),
        (Surface::JetBrains, "jetbrains"),
        (Surface::Unknown, "unknown"),
    ];
    for (surface, spelling) in cases {
        let json = serde_json::to_string(&surface).unwrap();
        assert_eq!(json, format!("\"{spelling}\""));
        assert_eq!(surface.as_str(), spelling);
        assert_eq!(serde_json::from_str::<Surface>(&json).unwrap(), surface);
    }
    assert!(serde_json::from_str::<Surface>("\"emacs\"").is_err());
    for (evidence, spelling) in [
        (SurfaceEvidence::SessionRecord, "session_record"),
        (SurfaceEvidence::Presence, "presence"),
    ] {
        assert_eq!(
            serde_json::to_string(&evidence).unwrap(),
            format!("\"{spelling}\"")
        );
    }
}

/// Reading a surface opens the named transcripts for reading and nothing
/// else: every file stays byte-identical and no file appears.
#[test]
fn reading_a_surface_leaves_every_file_byte_identical() {
    let machine = Machine::new();
    machine.path_command("claude");
    machine.extension(".vscode", "codex");
    let claude = machine.transcript("claude.jsonl", CLAUDE_HEAD);
    let codex = machine.transcript("codex.jsonl", CODEX_VSCODE);
    let sessions = [
        session("c", "claude-code", 1, Some(&claude)),
        session("x", "codex", 2, Some(&codex)),
    ];
    let before = snapshot(machine.dir.path());
    for token in ["claude-code", "codex", "gemini"] {
        let _answer = client_surface(token, &sessions, &machine.env());
    }
    assert_eq!(snapshot(machine.dir.path()), before);
}

fn snapshot(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut files = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else {
                let bytes = std::fs::read(&path).unwrap();
                files.push((path, bytes));
            }
        }
    }
    files.sort();
    files
}
