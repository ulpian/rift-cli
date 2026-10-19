//! Integration health: each reason fires on its fixture and stays silent on
//! its healthy twin, the alarm window bounds the two detectors that read
//! event times, and the assessment reads without writing.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use rift_agent_install::health::{assess, remedy, Facts, Finding, Reason, Severity, Surface};
use rift_agent_install::native::{
    CodexClient, CodexHookTrust, CodexHookVerdict, CodexSurface, CodexTrust,
};
use rift_shadow_hooks::session_cache::{SessionCache, IDLE_TIMEOUT_MS};
use rift_store::{CapturedEventRow, SessionTransitionRow, SqliteStore};

/// The injected clock: ten days after the epoch, so a day before it is still
/// a positive time.
const NOW: i64 = 10 * IDLE_TIMEOUT_MS;

/// The capture path's ordinal for a start it synthesized rather than received.
const SYNTHESIZED: i64 = 4_294_967_295;

/// A projection written through the store's own append-only writers, then
/// handed to the detectors opened read-only.
struct Projection {
    _dir: tempfile::TempDir,
    path: PathBuf,
    writer: Option<SqliteStore>,
    runtime: tokio::runtime::Runtime,
}

impl Projection {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("projection.db");
        let writer = rift_store::open_capture_projection(&path).unwrap();
        Self {
            _dir: dir,
            path,
            writer: Some(writer),
            runtime: tokio::runtime::Builder::new_current_thread()
                .build()
                .unwrap(),
        }
    }

    fn event(&self, session: &str, kind: &str, ordinal: i64, at: i64) -> &Self {
        let row = CapturedEventRow {
            idempotency_key: format!("{session}-{kind}-{ordinal}-{at}"),
            session_id: session.to_owned(),
            turn_ordinal: 0,
            event_ordinal: ordinal,
            kind: kind.to_owned(),
            trust: "git_fact".to_owned(),
            payload_sha256: "0".repeat(64),
            occurred_at_ms: at,
            schema_version: 1,
        };
        let store = self.writer.as_ref().unwrap();
        self.runtime
            .block_on(rift_store::record_captured_event(store, &row))
            .unwrap();
        self
    }

    fn transition(&self, session: &str, to: &str, at: i64) -> &Self {
        let row = SessionTransitionRow {
            transition_id: format!("{session}-{to}-{at}"),
            session_id: session.to_owned(),
            from_state: "active".to_owned(),
            to_state: to.to_owned(),
            trigger_kind: "stop".to_owned(),
            occurred_at: at,
            schema_version: 1,
        };
        let store = self.writer.as_ref().unwrap();
        self.runtime
            .block_on(rift_store::record_session_transition(store, &row))
            .unwrap();
        self
    }

    /// Close the writer and open the file the way a caller must.
    fn read_only(&mut self) -> SqliteStore {
        self.writer = None;
        SqliteStore::open_read_only(&self.path).unwrap()
    }
}

fn cache(session: &str, agent: &str, state: &str) -> SessionCache {
    serde_json::from_value(serde_json::json!({
        "schema": 1,
        "session_id": session,
        "state": state,
        "agent": agent,
        "current_turn_ordinal": 1,
        "compaction_boundary": null,
        "updated_at_ms": NOW,
    }))
    .unwrap()
}

fn facts<'a>(sessions: &'a [SessionCache], projection: Option<&'a SqliteStore>) -> Facts<'a> {
    Facts {
        sessions,
        projection,
        codex_trust: None,
        hook_binaries: BTreeMap::new(),
        binary_of_record: None,
    }
}

fn reasons(findings: &[Finding]) -> Vec<Reason> {
    findings.iter().map(|finding| finding.reason).collect()
}

fn codex(verdicts: &[CodexHookVerdict], version: &str) -> CodexTrust {
    CodexTrust {
        hooks: verdicts
            .iter()
            .enumerate()
            .map(|(index, verdict)| CodexHookTrust {
                event: format!("Event{index}"),
                verdict: *verdict,
            })
            .collect(),
        client: Some(CodexClient {
            surface: CodexSurface::Terminal,
            version: Some(version.to_owned()),
        }),
    }
}

/// A file with the execute bit, standing in for a `rift` build.
fn fake_binary(path: &Path) -> PathBuf {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, b"#!/bin/sh\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    path.to_path_buf()
}

#[test]
fn every_reason_serialises_to_its_stable_string_and_severity() {
    for reason in Reason::ALL {
        let json = serde_json::to_value(reason).unwrap();
        assert_eq!(json, reason.as_str());
    }
    let warnings: Vec<Reason> = Reason::ALL
        .into_iter()
        .filter(|reason| reason.severity() == Severity::Warning)
        .collect();
    assert_eq!(
        warnings,
        [
            Reason::CodexTrustUnverifiable,
            Reason::CodexVersionUnsupported,
            Reason::HookBinaryDisagrees
        ]
    );
    assert_eq!(serde_json::to_value(Severity::Error).unwrap(), "error");
    assert_eq!(serde_json::to_value(Severity::Warning).unwrap(), "warning");
}

#[test]
fn codex_skipping_rifts_hooks_is_an_error_naming_the_events() {
    use CodexHookVerdict::{Modified, Trusted, Untrusted};
    let mut facts = facts(&[], None);
    facts.codex_trust = Some(codex(&[Trusted, Modified, Untrusted], "0.157.0"));
    let findings = assess(&facts, NOW);
    assert_eq!(reasons(&findings), [Reason::CodexHooksSkipped]);
    assert_eq!(findings[0].severity, Severity::Error);
    assert_eq!(findings[0].agent, "codex");
    assert_eq!(findings[0].detail, "Event1, Event2");
}

#[test]
fn codex_trusting_every_hook_reports_nothing() {
    let mut facts = facts(&[], None);
    facts.codex_trust = Some(codex(&[CodexHookVerdict::Trusted; 10], "0.157.0"));
    assert!(assess(&facts, NOW).is_empty());
}

#[test]
fn an_unproven_codex_version_is_a_warning_naming_the_version() {
    let mut facts = facts(&[], None);
    facts.codex_trust = Some(codex(&[CodexHookVerdict::Unverifiable; 2], "0.150.0"));
    let findings = assess(&facts, NOW);
    assert_eq!(reasons(&findings), [Reason::CodexTrustUnverifiable]);
    assert_eq!(findings[0].severity, Severity::Warning);
    assert_eq!(findings[0].detail, "codex 0.150.0");
}

#[test]
fn a_codex_newer_than_every_pin_is_an_unsupported_version_warning_not_untrusted() {
    let mut facts = facts(&[], None);
    facts.codex_trust = Some(codex(&[CodexHookVerdict::Unverifiable; 2], "0.163.0"));
    let findings = assess(&facts, NOW);
    assert_eq!(reasons(&findings), [Reason::CodexVersionUnsupported]);
    assert_eq!(findings[0].severity, Severity::Warning);
    assert_eq!(findings[0].detail, "codex 0.163.0");
    let trust = facts.codex_trust.as_ref();
    let sentence = remedy(&findings[0], Surface::Terminal, trust).sentence;
    let summary = trust.map(CodexTrust::summary).unwrap_or_default();
    assert!(summary.starts_with("unsupported version — Codex 0.163.0 is newer than 0.157.0"));
    let shared = summary.trim_start_matches("unsupported version — ");
    assert!(sentence.starts_with(shared), "{sentence}");
    assert!(!sentence.contains("untrusted"), "{sentence}");
}

#[test]
fn tool_events_without_an_opening_or_a_prompt_mean_the_lifecycle_hooks_are_missing() {
    let mut projection = Projection::new();
    projection
        .event("s-tools", "tool_call", 0, NOW - 1_000)
        .event("s-tools", "tool_result", 1, NOW - 900);
    let store = projection.read_only();
    let sessions = [cache("s-tools", "claude-code", "active")];
    let findings = assess(&facts(&sessions, Some(&store)), NOW);
    assert_eq!(reasons(&findings), [Reason::LifecycleHooksMissing]);
    assert_eq!(findings[0].session.as_deref(), Some("s-tools"));
    assert_eq!(
        findings[0].detail,
        "no SessionStart, UserPromptSubmit; 2 tool events"
    );
}

#[test]
fn a_session_with_its_start_prompt_and_tools_is_healthy() {
    let mut projection = Projection::new();
    projection
        .event("s-whole", "session_start", 0, NOW - 3_000)
        .event("s-whole", "prompt_submit", 0, NOW - 2_000)
        .event("s-whole", "tool_call", 0, NOW - 1_000);
    let store = projection.read_only();
    let sessions = [cache("s-whole", "claude-code", "active")];
    assert!(assess(&facts(&sessions, Some(&store)), NOW).is_empty());
}

#[test]
fn a_lifecycle_gap_whose_last_event_is_older_than_the_alarm_window_is_not_reported() {
    let mut projection = Projection::new();
    projection.event("s-old", "tool_call", 0, NOW - IDLE_TIMEOUT_MS - 1);
    let store = projection.read_only();
    let sessions = [cache("s-old", "claude-code", "active")];
    assert!(assess(&facts(&sessions, Some(&store)), NOW).is_empty());
    let inside = NOW - 1;
    let findings = assess(&facts(&sessions, Some(&store)), inside);
    assert_eq!(reasons(&findings), [Reason::LifecycleHooksMissing]);
}

#[test]
fn antigravitys_synthesized_start_does_not_count_as_a_native_opening() {
    let mut projection = Projection::new();
    projection
        .event("s-synth", "session_start", SYNTHESIZED, NOW - 2_000)
        .event("s-synth", "tool_call", 0, NOW - 1_000)
        .event("s-native", "session_start", 0, NOW - 2_000)
        .event("s-native", "tool_call", 0, NOW - 1_000);
    let store = projection.read_only();
    let broken = [cache("s-synth", "antigravity", "active")];
    let findings = assess(&facts(&broken, Some(&store)), NOW);
    assert_eq!(reasons(&findings), [Reason::LifecycleHooksMissing]);
    assert_eq!(findings[0].detail, "no PreInvocation; 1 tool events");
    let healthy = [cache("s-native", "antigravity", "active")];
    assert!(assess(&facts(&healthy, Some(&store)), NOW).is_empty());
}

#[test]
fn only_the_newest_recent_session_of_an_agent_is_judged() {
    let mut projection = Projection::new();
    projection
        .event("s-before", "tool_call", 0, NOW - 5_000)
        .event("s-after", "session_start", 0, NOW - 2_000)
        .event("s-after", "tool_call", 0, NOW - 1_000);
    let store = projection.read_only();
    let sessions = [
        cache("s-before", "claude-code", "active"),
        cache("s-after", "claude-code", "active"),
    ];
    assert!(assess(&facts(&sessions, Some(&store)), NOW).is_empty());
}

#[test]
fn a_hook_binary_that_is_gone_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let gone = dir.path().join("target/debug/rift");
    let mut facts = facts(&[], None);
    facts
        .hook_binaries
        .insert("claude-code".to_owned(), gone.clone());
    facts.binary_of_record = Some(gone.clone());
    let findings = assess(&facts, NOW);
    assert_eq!(reasons(&findings), [Reason::HookBinaryMissing]);
    assert_eq!(findings[0].detail, gone.display().to_string());
}

#[test]
fn a_hook_binary_that_is_the_record_through_another_spelling_reports_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let real = fake_binary(&dir.path().join("target/release/rift"));
    let spelled = dir.path().join("target/release/../release/rift");
    let mut facts = facts(&[], None);
    facts.hook_binaries.insert("codex".to_owned(), spelled);
    facts.binary_of_record = Some(real);
    assert!(assess(&facts, NOW).is_empty());
}

#[test]
fn a_hook_binary_other_than_the_record_is_a_warning() {
    let dir = tempfile::tempdir().unwrap();
    let debug = fake_binary(&dir.path().join("target/debug/rift"));
    let release = fake_binary(&dir.path().join("target/release/rift"));
    let mut facts = facts(&[], None);
    facts.hook_binaries.insert("antigravity".to_owned(), debug);
    facts.binary_of_record = Some(release);
    let findings = assess(&facts, NOW);
    assert_eq!(reasons(&findings), [Reason::HookBinaryDisagrees]);
    assert_eq!(findings[0].severity, Severity::Warning);
}

#[test]
fn a_session_recorded_ended_while_its_events_kept_arriving_is_behind() {
    let mut projection = Projection::new();
    projection
        .event("s-behind", "session_start", 0, NOW - 5_000)
        .transition("s-behind", "ended", NOW - 4_000)
        .event("s-behind", "tool_call", 0, NOW - 3_000)
        .event("s-done", "session_start", 0, NOW - 5_000)
        .event("s-done", "session_end", 0, NOW - 3_000)
        .transition("s-done", "ended", NOW - 4_000);
    let store = projection.read_only();
    let sessions = [
        cache("s-behind", "claude-code", "ended"),
        cache("s-done", "claude-code", "ended"),
    ];
    let findings = assess(&facts(&sessions, Some(&store)), NOW);
    assert_eq!(reasons(&findings), [Reason::SessionBehindEvents]);
    assert_eq!(findings[0].session.as_deref(), Some("s-behind"));
    assert_eq!(findings[0].detail, "1 events after the last transition");
}

#[test]
fn a_late_event_older_than_the_alarm_window_is_no_longer_behind() {
    let mut projection = Projection::new();
    let late = NOW - IDLE_TIMEOUT_MS - 1;
    projection
        .event("s-stale", "session_start", 0, late - 2_000)
        .transition("s-stale", "idle", late - 1_000)
        .event("s-stale", "session_start", 1, late);
    let store = projection.read_only();
    let sessions = [cache("s-stale", "claude-code", "idle")];
    assert!(assess(&facts(&sessions, Some(&store)), NOW).is_empty());
    let findings = assess(&facts(&sessions, Some(&store)), late);
    assert_eq!(reasons(&findings), [Reason::SessionBehindEvents]);
}

#[test]
fn shuffled_cache_order_gives_identical_findings() {
    let mut projection = Projection::new();
    projection
        .event("s-a", "tool_call", 0, NOW - 1_000)
        .event("s-b", "tool_call", 0, NOW - 1_000)
        .event("s-c", "session_start", 0, NOW - 3_000)
        .transition("s-c", "ended", NOW - 2_000)
        .event("s-c", "tool_call", 0, NOW - 1_000)
        .event("s-d", "tool_call", 0, NOW - 1_000);
    let store = projection.read_only();
    let forward = [
        cache("s-a", "claude-code", "active"),
        cache("s-b", "claude-code", "active"),
        cache("s-c", "codex", "ended"),
        cache("s-d", "antigravity", "idle"),
    ];
    let mut backward = forward.clone();
    backward.reverse();
    let first = assess(&facts(&forward, Some(&store)), NOW);
    let second = assess(&facts(&backward, Some(&store)), NOW);
    assert_eq!(first, second);
    assert_eq!(
        reasons(&first),
        [
            Reason::LifecycleHooksMissing,
            Reason::LifecycleHooksMissing,
            Reason::SessionBehindEvents,
            Reason::SessionBehindEvents,
        ]
    );
}

/// Every regular file under `root`, path to bytes.
fn snapshot(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut files = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else {
                files.insert(path.clone(), std::fs::read(&path).unwrap());
            }
        }
    }
    files
}

#[test]
fn gathering_and_assessing_leave_every_file_they_read_byte_identical() {
    let root = tempfile::tempdir().unwrap();
    let repo = root.path().join("repo");
    let family = root.path().join("family");
    let codex_home = root.path().join("codex-home");
    std::fs::create_dir_all(repo.join(".claude")).unwrap();
    std::fs::create_dir_all(repo.join(".codex")).unwrap();
    std::fs::create_dir_all(&family).unwrap();
    std::fs::create_dir_all(&codex_home).unwrap();
    std::fs::write(codex_home.join("config.toml"), b"[hooks]\n").unwrap();
    let debug = fake_binary(&root.path().join("target/debug/rift"));
    let release = fake_binary(&root.path().join("target/release/rift"));
    rift_shadow_hooks::record_binary_of_record(
        &family,
        &release,
        rift_shadow_hooks::RecordedBy::Init,
    )
    .unwrap();
    let hook = |agent: &str, verb: &str| {
        serde_json::json!({ "hooks": { verb: [{ "hooks": [{
            "type": "command",
            "command": format!("\"{}\" hooks {agent} {verb}", debug.display()),
        }]}]}})
        .to_string()
    };
    std::fs::write(
        repo.join(".claude/settings.local.json"),
        hook("claude-code", "SessionStart"),
    )
    .unwrap();
    std::fs::write(
        repo.join(".codex/hooks.json"),
        hook("codex", "SessionStart"),
    )
    .unwrap();
    let mut projection = Projection::new();
    projection
        .event("s-tools", "tool_call", 0, NOW - 1_000)
        .transition("s-tools", "ended", NOW - 2_000);
    let store = projection.read_only();
    let sessions = [cache("s-tools", "claude-code", "ended")];

    let before = (
        snapshot(root.path()),
        std::fs::read(&projection.path).unwrap(),
    );
    let facts = Facts::gather(&repo, &family, Some(&codex_home), &sessions, Some(&store));
    let findings = assess(&facts, NOW);
    let after = (
        snapshot(root.path()),
        std::fs::read(&projection.path).unwrap(),
    );

    assert_eq!(before, after);
    assert_eq!(
        reasons(&findings),
        [
            Reason::CodexTrustUnverifiable,
            Reason::LifecycleHooksMissing,
            Reason::HookBinaryDisagrees,
            Reason::HookBinaryDisagrees,
            Reason::SessionBehindEvents,
        ]
    );
}
