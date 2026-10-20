//! Remedies: every reason has a sentence on every surface, the Codex sentence
//! is the trust row's own, the warnings say nothing is being lost, Rift's own
//! defect asks nothing of the user, and the `--json` element carries the
//! stable reason code.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use rift_agent_install::detect::NativeIdeHost;
use rift_agent_install::health::{remedy, Finding, Reason, Remedy, Severity, Surface};
use rift_agent_install::native::{
    CodexClient, CodexHookTrust, CodexHookVerdict, CodexSurface, CodexTrust,
};

/// The four surfaces a sentence is written for.
const SURFACES: [Surface; 4] = [
    Surface::Ide(NativeIdeHost::VsCode),
    Surface::Terminal,
    Surface::JetBrains,
    Surface::Unknown,
];

/// Codex's trust with the given verdicts, as a session run on `surface`.
fn trust(verdicts: &[CodexHookVerdict], surface: CodexSurface) -> CodexTrust {
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
            surface,
            version: Some("0.157.0".to_owned()),
        }),
    }
}

/// A finding for `reason` about the agent the reason can name.
fn finding(reason: Reason) -> Finding {
    match reason {
        Reason::CodexHooksSkipped => Finding::new(reason, "codex", None, "Event0".to_owned()),
        Reason::CodexTrustUnverifiable => {
            Finding::new(reason, "codex", None, "codex 0.156.0".to_owned())
        }
        Reason::CodexVersionUnsupported => {
            Finding::new(reason, "codex", None, "codex 0.164.0".to_owned())
        }
        Reason::LifecycleHooksMissing => Finding::new(
            reason,
            "claude-code",
            Some("s1"),
            "no SessionStart, UserPromptSubmit; 3 tool events".to_owned(),
        ),
        Reason::HookBinaryMissing => {
            Finding::new(reason, "claude-code", None, "/gone/rift".to_owned())
        }
        Reason::HookBinaryDisagrees => {
            Finding::new(reason, "antigravity", None, "/a/rift vs /b/rift".to_owned())
        }
        Reason::SessionBehindEvents => Finding::new(
            reason,
            "claude-code",
            Some("s2"),
            "2 events after the last transition".to_owned(),
        ),
    }
}

/// The remedy for `reason` on `surface`, with a skipped-hooks trust beside it.
fn remedy_on(reason: Reason, surface: Surface) -> Remedy {
    let codex = trust(
        &[CodexHookVerdict::Modified, CodexHookVerdict::Trusted],
        CodexSurface::Terminal,
    );
    remedy(&finding(reason), surface, Some(&codex))
}

#[test]
fn every_reason_has_a_headline_and_a_sentence_on_every_surface() {
    for reason in Reason::ALL {
        for surface in SURFACES {
            let remedy = remedy_on(reason, surface);
            assert!(!remedy.headline.is_empty(), "{reason:?} on {surface:?}");
            assert!(!remedy.headline.contains('\n'), "{}", remedy.headline);
            assert!(remedy.sentence.ends_with('.'), "{}", remedy.sentence);
            assert_eq!(remedy.reason, reason);
            assert_eq!(remedy.severity, reason.severity());
            assert_eq!(remedy.surface, surface);
        }
    }
}

#[test]
fn a_rift_command_remedy_says_to_act_in_a_terminal_in_this_repository() {
    for reason in [
        Reason::HookBinaryMissing,
        Reason::HookBinaryDisagrees,
        Reason::LifecycleHooksMissing,
    ] {
        for surface in SURFACES {
            let sentence = remedy_on(reason, surface).sentence.to_lowercase();
            assert!(
                sentence.contains("in a terminal in this repository"),
                "{sentence}"
            );
        }
    }
}

#[test]
fn a_remedy_names_the_editor_the_agent_runs_in() {
    let vscode = remedy_on(
        Reason::HookBinaryMissing,
        Surface::Ide(NativeIdeHost::VsCode),
    );
    assert!(vscode.sentence.contains("VS Code"), "{}", vscode.sentence);
    let cursor = remedy_on(
        Reason::LifecycleHooksMissing,
        Surface::Ide(NativeIdeHost::Cursor),
    );
    assert!(
        cursor.sentence.contains(" In Cursor, "),
        "{}",
        cursor.sentence
    );
    let jetbrains = remedy_on(Reason::LifecycleHooksMissing, Surface::JetBrains);
    assert!(
        jetbrains.sentence.contains("JetBrains"),
        "{}",
        jetbrains.sentence
    );
    let terminal = remedy_on(Reason::HookBinaryMissing, Surface::Terminal);
    assert!(
        !terminal.sentence.contains("VS Code"),
        "{}",
        terminal.sentence
    );
}

#[test]
fn the_codex_sentence_is_the_trust_rows_own_word_for_word() {
    use CodexHookVerdict::{Modified, Trusted, Untrusted};
    let cases = [
        (vec![Modified, Modified, Trusted], CodexSurface::Terminal),
        (vec![Untrusted, Trusted], CodexSurface::Terminal),
        (vec![Modified, Untrusted, Trusted], CodexSurface::Terminal),
        (vec![Modified, Untrusted], CodexSurface::VsCode),
    ];
    for (verdicts, codex_surface) in cases {
        let codex = trust(&verdicts, codex_surface);
        let surface = match codex_surface {
            CodexSurface::VsCode => Surface::Ide(NativeIdeHost::VsCode),
            CodexSurface::Terminal => Surface::Terminal,
        };
        let finding = Finding::new(Reason::CodexHooksSkipped, "codex", None, String::new());
        let remedy = remedy(&finding, surface, Some(&codex));
        assert_eq!(Some(remedy.sentence), codex.remedy(), "{verdicts:?}");
        let skipped = codex.needs_review().len();
        assert_eq!(
            remedy.headline,
            format!("Codex: {skipped} of Rift's hooks skipped")
        );
    }
}

#[test]
fn the_warnings_say_capture_is_not_being_lost() {
    for reason in [
        Reason::CodexTrustUnverifiable,
        Reason::CodexVersionUnsupported,
        Reason::HookBinaryDisagrees,
    ] {
        assert_eq!(reason.severity(), Severity::Warning);
        for surface in SURFACES {
            let sentence = remedy_on(reason, surface).sentence;
            assert!(sentence.contains("not being lost"), "{sentence}");
        }
    }
}

#[test]
fn rifts_own_defect_asks_nothing_of_the_user() {
    for surface in SURFACES {
        let sentence = remedy_on(Reason::SessionBehindEvents, surface).sentence;
        assert!(
            sentence.contains("error, not yours; nothing to do"),
            "{sentence}"
        );
        assert!(!sentence.contains('`'), "names no command: {sentence}");
    }
}

#[test]
fn no_remedy_has_rift_write_another_clients_trust_or_configuration() {
    for reason in Reason::ALL {
        for surface in SURFACES {
            let sentence = remedy_on(reason, surface).sentence;
            for forbidden in ["config.toml", "~/.codex", "Rift trusts", "Rift will trust"] {
                assert!(!sentence.contains(forbidden), "{sentence}");
            }
        }
    }
}

#[test]
fn the_json_element_carries_each_reasons_stable_code() {
    for reason in Reason::ALL {
        let json = serde_json::to_value(remedy_on(reason, Surface::Terminal)).unwrap();
        assert_eq!(json["reason"], reason.as_str());
        assert_eq!(json["severity"], reason.severity().as_str());
        assert_eq!(json["surface"], "terminal");
        for key in ["agent", "session", "headline", "sentence", "detail"] {
            assert!(json.get(key).is_some(), "{key} missing from {json}");
        }
    }
}
