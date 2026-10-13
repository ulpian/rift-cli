use super::*;

const SESSION: &str = "s-1";

/// A session directory laid out as Claude Code keeps one: the transcript
/// beside `<session>/tool-results/`.
struct Session {
    _tmp: tempfile::TempDir,
    main: PathBuf,
    results: PathBuf,
}

impl Session {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let projects = tmp.path().join("projects");
        let results = projects.join(SESSION).join(SPILLS_DIR);
        std::fs::create_dir_all(&results).unwrap();
        Self {
            main: projects.join(format!("{SESSION}.jsonl")),
            results,
            _tmp: tmp,
        }
    }

    /// Write a spill file and return its address.
    fn spill_file(&self, name: &str, body: &[u8]) -> String {
        let path = self.results.join(name);
        std::fs::write(&path, body).unwrap();
        path.display().to_string()
    }

    fn spills(&self, transcript: &str) -> Vec<ArchivedSpill> {
        self.spills_within(transcript, MAX_SPILL_BYTES, MAX_SPILL_BUDGET)
    }

    fn spills_within(&self, transcript: &str, per_file: u64, budget: u64) -> Vec<ArchivedSpill> {
        let packs = RulePackSet::builtin();
        let mut scope = SpillScope::of(&self.main, SESSION, &packs).unwrap();
        scope.limits = SpillLimits {
            per_file,
            per_transcript: budget,
        };
        transcript_spills(&scope, transcript.as_bytes())
    }
}

/// A result line in the structured form a main transcript records.
fn structured(call: &str, address: &str) -> String {
    serde_json::json!({
        "type": "user",
        "message": { "role": "user", "content": [{
            "type": "tool_result", "tool_use_id": call,
            "content": format!("<persisted-output>\nOutput too large (60KB). Full output saved to: {address}\n\nPreview (first 2KB):\nhead"),
        }]},
        "toolUseResult": { "stdout": "head", "persistedOutputPath": address, "persistedOutputSize": 60_000 },
    })
    .to_string()
        + "\n"
}

/// A result line in the envelope-only form a subagent's transcript records.
fn enveloped(call: &str, text: &str) -> String {
    serde_json::json!({
        "type": "user", "isSidechain": true,
        "message": { "role": "user", "content": [{
            "type": "tool_result", "tool_use_id": call,
            "content": [{ "type": "text", "text": text }],
        }]},
    })
    .to_string()
        + "\n"
}

fn envelope(address: &str) -> String {
    format!("<persisted-output>\nOutput too large (34.8KB). Full output saved to: {address}\n\nPreview (first 2KB):\nhead")
}

/// The gate's output over `raw` with the built-in packs.
fn sealed(raw: &[u8]) -> Vec<u8> {
    let raw = rift_core::SecretBytes::new(raw.to_vec());
    rift_shadow::sealed_spill(&raw, &RulePackSet::builtin())
        .unwrap()
        .as_slice()
        .to_vec()
}

fn body(s: &ArchivedSpill) -> &[u8] {
    s.body.as_ref().unwrap().as_slice()
}

/// 60 K characters across many lines, the size of a typical spill.
fn sixty_k() -> Vec<u8> {
    let mut out = Vec::new();
    for i in 0..1_000 {
        out.extend_from_slice(format!("{i:05} {}\n", "x".repeat(53)).as_bytes());
    }
    assert_eq!(out.len(), 60_000);
    out
}

/// A 60 K-character result is sealed whole: every byte, through the same
/// gate, under the tool-use id the transcript names.
#[test]
fn a_sixty_thousand_character_spill_is_sealed_whole() {
    let s = Session::new();
    let raw = sixty_k();
    let address = s.spill_file("bsvopan06.txt", &raw);
    let spills = s.spills(&structured("toolu_big", &address));
    assert_eq!(spills.len(), 1);
    assert_eq!(spills[0].tool_use_id, "toolu_big");
    assert_eq!(spills[0].state, SpillState::Sealed);
    assert_eq!(body(&spills[0]), sealed(&raw).as_slice());
    assert_eq!(body(&spills[0]).len(), 60_000);
    assert_eq!(spills[0].source_bytes, None);
}

/// The seeded secret corpus, buried in a spill, never survives the seal.
#[test]
fn a_secret_inside_a_spill_is_redacted() {
    let secrets = super::super::test_corpus::seeded_secrets();
    let zwsp = '\u{200B}';
    let s = Session::new();
    let mut raw = sixty_k();
    for secret in &secrets {
        raw.extend_from_slice(format!("export TOKEN={secret} # line\n").as_bytes());
    }
    raw.extend_from_slice(format!("key=AKIA{zwsp}IOSFODNN7{zwsp}EXAMPLE end\n").as_bytes());
    let address = s.spill_file("bsecret.txt", &raw);
    let spills = s.spills(&structured("toolu_sec", &address));
    let text = String::from_utf8_lossy(body(&spills[0])).into_owned();
    for secret in &secrets {
        assert!(
            !text.contains(secret.as_str()),
            "{secret} survived the seal"
        );
    }
    assert!(!text.contains("AKIAIOSFODNN7EXAMPLE"));
    assert!(text.contains("[REDACTED"), "the gate ran");
}

/// A file already gone is recorded `spill_missing`, never dropped.
#[test]
fn a_spill_whose_file_is_gone_is_recorded_missing() {
    let s = Session::new();
    let address = s.results.join("gone.txt").display().to_string();
    let spills = s.spills(&structured("toolu_gone", &address));
    assert_eq!(spills.len(), 1);
    assert_eq!(spills[0].state, SpillState::Missing);
    assert_eq!(spills[0].state.word(), Some("spill_missing"));
    assert!(spills[0].body.is_none());
}

/// An address outside `<session>/tool-results` is refused before anything is
/// opened, whatever the file holds — a traversal, a sibling session, another
/// directory, a relative path, a dotted stem.
#[test]
fn an_address_escaping_the_session_dir_is_refused() {
    let s = Session::new();
    let outside = s.main.parent().unwrap().join("elsewhere.txt");
    std::fs::write(&outside, "AKIAIOSFODNN7EXAMPLE").unwrap();
    let results = s.results.display().to_string();
    let addresses = [
        format!("{results}/../../elsewhere.txt"),
        format!("{results}/./x.txt"),
        format!("{results}//x.txt"),
        outside.display().to_string(),
        s.main
            .parent()
            .unwrap()
            .join("s-2/tool-results/x.txt")
            .display()
            .to_string(),
        "tool-results/x.txt".to_owned(),
        format!("{results}/a.b.txt"),
        format!("{results}/x.json"),
    ];
    for (i, address) in addresses.iter().enumerate() {
        let spills = s.spills(&structured(&format!("toolu_{i}"), address));
        assert_eq!(spills[0].state, SpillState::Refused, "{address}");
        assert_eq!(spills[0].state.word(), Some("spill_refused"));
    }
}

/// A link is never followed: not as the file, and not as the `tool-results`
/// directory, though the address is spelled inside the session.
#[cfg(unix)]
#[test]
fn a_symlinked_spill_or_results_dir_is_refused() {
    let s = Session::new();
    let target = s.main.parent().unwrap().join("secret.txt");
    std::fs::write(&target, "AKIAIOSFODNN7EXAMPLE").unwrap();
    let link = s.results.join("blink.txt");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let spills = s.spills(&structured("toolu_link", &link.display().to_string()));
    assert_eq!(spills[0].state, SpillState::Refused);

    let other = Session::new();
    std::fs::remove_dir(&other.results).unwrap();
    std::os::unix::fs::symlink(s.results.clone(), &other.results).unwrap();
    s.spill_file("real.txt", b"real");
    let address = other.results.join("real.txt").display().to_string();
    let spills = other.spills(&structured("toolu_dir", &address));
    assert_eq!(spills[0].state, SpillState::Refused);
}

/// A subagent's result records no `toolUseResult`; its address is read from
/// the front of the envelope, and never from text that only mentions one.
#[test]
fn an_envelope_names_a_spill_only_from_its_front() {
    let s = Session::new();
    let address = s.spill_file("bsub.txt", b"subagent output\n");
    let quoted = format!("the log said {}", envelope(&address));
    let transcript =
        enveloped("toolu_sub", &envelope(&address)) + &enveloped("toolu_quote", &quoted);
    let spills = s.spills(&transcript);
    assert_eq!(spills.len(), 1, "only the front of a result names a spill");
    assert_eq!(spills[0].tool_use_id, "toolu_sub");
    assert_eq!(body(&spills[0]), b"subagent output\n");
}

/// The structured field outranks the envelope, and the first naming of a
/// call wins over a later one.
#[test]
fn the_structured_field_outranks_the_envelope_and_the_first_naming_wins() {
    let s = Session::new();
    let field = s.spill_file("bfield.txt", b"from the field\n");
    let text = s.spill_file("btext.txt", b"from the text\n");
    let line = serde_json::json!({
        "type": "user",
        "message": { "role": "user", "content": [{
            "type": "tool_result", "tool_use_id": "toolu_x", "content": envelope(&text),
        }]},
        "toolUseResult": { "persistedOutputPath": field },
    })
    .to_string()
        + "\n";
    let transcript = line + &structured("toolu_x", &text);
    let spills = s.spills(&transcript);
    assert_eq!(spills.len(), 1);
    assert_eq!(body(&spills[0]), b"from the field\n");
}

/// A file past the per-file bound is cut at its last line break inside the
/// bound and recorded truncated with its size; past the transcript's budget
/// the next spill is recorded unread.
#[test]
fn a_spill_past_the_bound_is_cut_at_a_line_and_recorded() {
    let s = Session::new();
    let a = s.spill_file("ba.txt", b"line one\nline two\nline three\n");
    let b = s.spill_file("bb.txt", b"short\n");
    let c = s.spill_file("bc.txt", b"never read\n");
    let transcript =
        structured("toolu_a", &a) + &structured("toolu_b", &b) + &structured("toolu_c", &c);
    let spills = s.spills_within(&transcript, 20, 24);
    assert_eq!(spills[0].state, SpillState::Truncated);
    assert_eq!(body(&spills[0]), b"line one\nline two\n");
    assert_eq!(spills[0].source_bytes, Some(29));
    assert_eq!(spills[0].state.word(), Some("spill_truncated"));
    assert_eq!(
        spills[1].state,
        SpillState::Sealed,
        "exactly the 6 bytes left"
    );
    assert_eq!(body(&spills[1]), b"short\n");
    assert_eq!(spills[2].state, SpillState::OverBudget);
    assert_eq!(spills[2].state.word(), Some("spill_over_budget"));
}

/// A result that names no spill, and a line that is not a record, cost
/// nothing and record nothing.
#[test]
fn a_transcript_without_spills_records_none() {
    let s = Session::new();
    let plain = enveloped("toolu_p", "ordinary output") + "not json persistedOutputPath\n";
    assert!(s.spills(&plain).is_empty());
}
