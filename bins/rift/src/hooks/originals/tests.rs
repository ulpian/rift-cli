use std::path::Path;

use rift_shadow::backend::parse_spool_record;
use rift_shadow::backend::rewrite::Stream;

use super::*;

const SESSION: &str = "sess-1";
const CALL: &str = "toolu_01SPOOL";

fn repo(root: &Path) -> rift_git::RepoPaths {
    let git = root.join(".git");
    std::fs::create_dir_all(&git).unwrap();
    rift_git::RepoPaths {
        git_dir: git.clone(),
        common_dir: git,
        work_tree: root.to_path_buf(),
        worktree_hash: [0; 6],
    }
}

fn family() -> RepoId {
    RepoId::new("0000000000000000000000000000beef").unwrap()
}

fn window() -> String {
    "ab".repeat(32)
}

const FOLD_FIRST_LINE: [LineRange; 1] = [LineRange {
    stream: Stream::Stdout,
    start: 0,
    end: 1,
}];

fn site<'a>(
    repo: &'a rift_git::RepoPaths,
    family: &'a RepoId,
    window: &'a str,
    tool_use_id: &'a str,
) -> RewriteSite<'a> {
    RewriteSite {
        repo,
        session_id: SESSION,
        agent_id: None,
        window_key: window,
        family_key: family,
        tool_use_id,
        form: RewriteForm::FoldedOutput,
        replaced: &FOLD_FIRST_LINE,
    }
}

fn original(stdout: &str, stderr: &str) -> ToolOriginal {
    ToolOriginal {
        stdout: SecretBytes::new(stdout.as_bytes().to_vec()),
        stderr: SecretBytes::new(stderr.as_bytes().to_vec()),
        interrupted: false,
    }
}

fn spooled(repo: &rift_git::RepoPaths, call: &str) -> PathBuf {
    session_spool_dir(&repo.common_dir, SESSION).join(format!("{call}{SPOOL_SUFFIX}"))
}

/// A spooled record reads back in the one grammar: the header names the
/// call, window, family, form and ranges, and the body is the gate's output
/// over the canonical record, every seeded secret masked; the receipt's
/// digest is the body's.
#[test]
fn a_spooled_original_is_the_gate_s_record_and_the_receipt_names_its_digest() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = repo(tmp.path());
    let (family, window) = (family(), window());
    let secrets = super::super::archive::test_corpus::seeded_secrets();
    let stdout = format!("first line\n{}\n", secrets.join("\n"));
    let receipt = spool_untimed(
        &site(&repo, &family, &window, CALL),
        &original(&stdout, "warn"),
    )
    .unwrap();

    let record = std::fs::read(spooled(&repo, CALL)).unwrap();
    let (header, body) = parse_spool_record(&record);
    let header = header.unwrap();
    assert_eq!(header.tool_use_id, CALL);
    assert_eq!(header.window_key, window);
    assert_eq!(header.family_key, family.as_str());
    assert_eq!(header.form, RewriteForm::FoldedOutput);
    assert_eq!(header.replaced, FOLD_FIRST_LINE);
    assert_eq!(header.sha256, receipt.sha256);
    assert_eq!(receipt.sha256, rift_shadow::sha256_hex(&[body]));
    assert_eq!(receipt.bytes, u64::try_from(body.len()).unwrap());
    let raw = SecretBytes::new(original_record_json(&stdout, "warn", false));
    let gate = rift_shadow::redact::redact_with(&raw, &rift_shadow::redact::RulePackSet::builtin())
        .unwrap();
    assert_eq!(body, gate.redacted.as_slice());
    let text = String::from_utf8_lossy(&record);
    for secret in &secrets {
        assert!(
            !text.contains(secret.as_str()),
            "a seeded secret reached the spool"
        );
    }
    assert!(text.contains("first line"));
}

/// The ceiling is on characters, stdout and stderr together: exactly 30,000
/// spools, one more is refused and leaves nothing.
#[test]
fn an_original_over_thirty_thousand_characters_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = repo(tmp.path());
    let (family, window) = (family(), window());
    let half = "é".repeat(MAX_REWRITE_CHARS / 2);
    let at_ceiling = original(&half, &half);
    assert!(spool_untimed(&site(&repo, &family, &window, "toolu_fits"), &at_ceiling).is_ok());
    let over = original(&half, &format!("{half}x"));
    assert_eq!(
        spool_untimed(&site(&repo, &family, &window, "toolu_over"), &over),
        Err(SpoolRefusal::TooLarge)
    );
    let huge = original(&"x".repeat(MAX_REWRITE_CHARS * 4 + 1), "");
    assert_eq!(
        spool_untimed(&site(&repo, &family, &window, "toolu_huge"), &huge),
        Err(SpoolRefusal::TooLarge)
    );
    assert!(!spooled(&repo, "toolu_over").exists());
    assert!(!spooled(&repo, "toolu_huge").exists());
}

/// An id that cannot name a path or a journal row is refused before anything
/// is created.
#[test]
fn unsafe_ids_are_refused_and_create_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = repo(tmp.path());
    let (family, window) = (family(), window());
    let base = || site(&repo, &family, &window, CALL);
    let short = "ab".to_owned();
    for bad in [
        RewriteSite {
            session_id: "../up",
            ..base()
        },
        RewriteSite {
            session_id: ".hidden",
            ..base()
        },
        RewriteSite {
            tool_use_id: "a/b",
            ..base()
        },
        RewriteSite {
            tool_use_id: "..",
            ..base()
        },
        RewriteSite {
            agent_id: Some("a b"),
            ..base()
        },
        RewriteSite {
            window_key: &short,
            ..base()
        },
        RewriteSite {
            window_key: &"AB".repeat(32),
            ..base()
        },
    ] {
        assert_eq!(
            spool_untimed(&bad, &original("a\n", "")),
            Err(SpoolRefusal::UnsafeId)
        );
    }
    assert!(!family_state_dir(&repo.common_dir).exists());
}

/// A stream that is not text, or ranges that do not fit, are malformed.
#[test]
fn a_non_text_stream_or_ranges_that_do_not_fit_are_malformed() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = repo(tmp.path());
    let (family, window) = (family(), window());
    let binary = ToolOriginal {
        stdout: SecretBytes::new(vec![0xff, 0xfe]),
        stderr: SecretBytes::new(Vec::new()),
        interrupted: true,
    };
    let s = site(&repo, &family, &window, CALL);
    assert_eq!(spool_untimed(&s, &binary), Err(SpoolRefusal::Malformed));
    let past_end = [LineRange {
        stream: Stream::Stderr,
        start: 0,
        end: 5,
    }];
    let wide = RewriteSite {
        replaced: &past_end,
        ..site(&repo, &family, &window, CALL)
    };
    assert_eq!(
        spool_untimed(&wide, &original("a", "b")),
        Err(SpoolRefusal::Malformed)
    );
    let none = RewriteSite {
        replaced: &[],
        ..site(&repo, &family, &window, CALL)
    };
    assert_eq!(
        spool_untimed(&none, &original("a", "b")),
        Err(SpoolRefusal::Malformed)
    );
}

/// One entry per call: a second spool is refused by name and the first
/// record is left byte for byte.
#[test]
fn a_second_spool_of_one_call_is_refused_and_never_overwrites() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = repo(tmp.path());
    let (family, window) = (family(), window());
    let s = site(&repo, &family, &window, CALL);
    assert!(spool_untimed(&s, &original("first\n", "")).is_ok());
    let before = std::fs::read(spooled(&repo, CALL)).unwrap();
    assert_eq!(
        spool_untimed(&s, &original("second\n", "")),
        Err(SpoolRefusal::AlreadySpooled)
    );
    assert_eq!(std::fs::read(spooled(&repo, CALL)).unwrap(), before);
    let dir = session_spool_dir(&repo.common_dir, SESSION);
    let names: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        names,
        [format!("{CALL}{SPOOL_SUFFIX}")],
        "no temporary is left"
    );
}

/// A spool that used its budget refuses rather than writes.
#[test]
fn a_spool_past_its_budget_refuses_and_writes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = repo(tmp.path());
    let (family, window) = (family(), window());
    let s = site(&repo, &family, &window, CALL);
    let late = || SPOOL_BUDGET + Duration::from_millis(1);
    assert_eq!(
        spool_within(&s, &original("a\n", ""), &late, SPOOL_BUDGET),
        Err(SpoolRefusal::OverBudget)
    );
    assert!(!spooled(&repo, CALL).exists());
    let early = || Duration::ZERO;
    assert!(spool_within(&s, &original("a\n", ""), &early, SPOOL_BUDGET).is_ok());
}

/// The timed entry point either spools whole or refuses over its budget and
/// leaves nothing; which one depends on the machine's speed, so the verdict
/// holds for both.
#[test]
fn the_timed_spool_writes_whole_or_not_at_all() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = repo(tmp.path());
    let (family, window) = (family(), window());
    let outcome = spool_original(&site(&repo, &family, &window, CALL), original("a\n", ""));
    match outcome {
        Ok(receipt) => {
            let record = std::fs::read(spooled(&repo, CALL)).unwrap();
            let (_, body) = parse_spool_record(&record);
            assert_eq!(receipt.sha256, rift_shadow::sha256_hex(&[body]));
        }
        Err(refusal) => {
            assert_eq!(refusal, SpoolRefusal::OverBudget);
            assert!(!spooled(&repo, CALL).exists());
        }
    }
}

/// Every refusal has its own stable word.
#[test]
fn every_refusal_has_a_stable_word() {
    let words: Vec<&str> = [
        SpoolRefusal::TooLarge,
        SpoolRefusal::AlreadySpooled,
        SpoolRefusal::UnsafeId,
        SpoolRefusal::UnsafePath,
        SpoolRefusal::Malformed,
        SpoolRefusal::Unsealed,
        SpoolRefusal::Io,
        SpoolRefusal::OverBudget,
    ]
    .into_iter()
    .map(SpoolRefusal::word)
    .collect();
    assert_eq!(
        words,
        [
            "too_large",
            "already_spooled",
            "unsafe_id",
            "unsafe_path",
            "malformed",
            "unsealed",
            "io",
            "over_budget"
        ]
    );
}

/// Doctor's source lists each spooled session with its record count.
#[test]
fn spooled_sessions_are_listed_with_their_record_counts() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = repo(tmp.path());
    let (family, window) = (family(), window());
    assert!(spooled_sessions(&repo.common_dir).is_empty());
    for call in ["toolu_a", "toolu_b"] {
        assert!(spool_untimed(&site(&repo, &family, &window, call), &original("a", "")).is_ok());
    }
    let listed = spooled_sessions(&repo.common_dir);
    assert_eq!(listed.len(), 1);
    assert_eq!(
        (listed[0].session_id.as_str(), listed[0].records),
        (SESSION, 2)
    );
}

#[cfg(unix)]
mod unix {
    use std::os::unix::fs::PermissionsExt as _;

    use super::*;

    fn mode(path: &Path) -> u32 {
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    /// The record is owner-only and so are the directories the spool made.
    #[test]
    fn the_record_is_owner_only_in_owner_only_directories() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = repo(tmp.path());
        let (family, window) = (family(), window());
        assert!(spool_untimed(&site(&repo, &family, &window, CALL), &original("a", "")).is_ok());
        assert_eq!(mode(&spooled(&repo, CALL)), 0o600);
        let session = session_spool_dir(&repo.common_dir, SESSION);
        assert_eq!(mode(&session), 0o700);
        assert_eq!(mode(session.parent().unwrap()), 0o700);
    }

    /// A link anywhere on the spool's path is refused, and nothing is
    /// written through it.
    #[test]
    fn a_symlinked_spool_directory_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let elsewhere = tmp.path().join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        let (family, window) = (family(), window());
        for link in ["family", "spool", "session"] {
            let root = tmp.path().join(link);
            let repo = repo(&root);
            let state = family_state_dir(&repo.common_dir);
            let at = match link {
                "family" => state,
                "spool" => state.join(SPOOL_DIR),
                _ => session_spool_dir(&repo.common_dir, SESSION),
            };
            std::fs::create_dir_all(at.parent().unwrap()).unwrap();
            std::os::unix::fs::symlink(&elsewhere, &at).unwrap();
            assert_eq!(
                spool_untimed(&site(&repo, &family, &window, CALL), &original("a", "")),
                Err(SpoolRefusal::UnsafePath),
                "{link}"
            );
        }
        assert_eq!(std::fs::read_dir(&elsewhere).unwrap().count(), 0);
    }
}

#[test]
fn a_fallback_record_cannot_be_spooled_as_an_original() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = repo(tmp.path());
    let policy = tmp.path().join(".rift/policies/redactors");
    std::fs::create_dir_all(&policy).unwrap();
    std::fs::write(
        policy.join("acme.toml"),
        r#"schema_version = 1
name = "acme"
version = "2026.08.0"
[[rules]]
id = "osc"
regex = '^\[?REDACTED'
"#,
    )
    .unwrap();
    let (family, window) = (family(), window());
    assert_eq!(
        spool_untimed(
            &site(&repo, &family, &window, CALL),
            &original("REDACTED ssn=\n123456789", "")
        ),
        Err(SpoolRefusal::Unsealed)
    );
    assert!(!spooled(&repo, CALL).exists());
}
