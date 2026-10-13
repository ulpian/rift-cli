use super::*;

const SESSION: &str = "s-1";

/// A session whose cache names its transcript, with the transcript and its
/// `subagents/` directory where Claude Code keeps them.
struct Session {
    _tmp: tempfile::TempDir,
    git_dir: PathBuf,
    main: PathBuf,
    subagents: PathBuf,
}

impl Session {
    fn new(main_body: &str) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let git_dir = tmp.path().join("git");
        let sessions = rift_shadow::hooks::session_cache::sessions_dir(&git_dir);
        std::fs::create_dir_all(&sessions).unwrap();
        let main = tmp.path().join("projects").join(format!("{SESSION}.jsonl"));
        let subagents = tmp.path().join("projects").join(SESSION).join("subagents");
        std::fs::create_dir_all(&subagents).unwrap();
        std::fs::write(&main, main_body).unwrap();
        let cache = serde_json::json!({
            "schema": 1, "session_id": SESSION, "state": "active", "agent": "claude-code",
            "current_turn_ordinal": 1, "compaction_boundary": null, "updated_at_ms": 1,
            "transcript_path": main.display().to_string(),
        });
        std::fs::write(sessions.join(format!("{SESSION}.json")), cache.to_string()).unwrap();
        Self {
            _tmp: tmp,
            git_dir,
            main,
            subagents,
        }
    }

    fn subagent(&self, agent: &str, body: &str) -> PathBuf {
        let path = self.subagents.join(format!("agent-{agent}.jsonl"));
        std::fs::write(&path, body).unwrap();
        path
    }

    fn tasks(&self, observations: &[ArchivedWorktreeObservation]) -> Vec<ArchivedTask> {
        session_tasks(&TaskSources {
            git_dir: &self.git_dir,
            session_id: SESSION,
            packs: &RulePackSet::builtin(),
            observations,
            main: Some(self.main.clone()),
        })
    }
}

fn meta(task: &ArchivedTask) -> serde_json::Value {
    serde_json::from_slice(&task.metadata_json()).unwrap()
}

fn line(ts: &str, text: &str) -> String {
    serde_json::json!({
        "type": "assistant", "timestamp": ts,
        "message": { "role": "assistant", "content": [{ "type": "text", "text": text }] },
    })
    .to_string()
        + "\n"
}

/// A spawn result in the text form a background launch records.
fn launched(call: &str, agent: &str) -> String {
    serde_json::json!({
        "type": "user",
        "message": { "role": "user", "content": [{
            "type": "tool_result", "tool_use_id": call,
            "content": [{ "type": "text", "text": format!("Async agent launched.\nagentId: {agent} (internal)") }],
        }]},
    })
    .to_string()
        + "\n"
}

#[test]
fn a_subagent_with_no_spawn_is_sealed_under_its_agent_id_as_unmatched() {
    let s = Session::new(&line("2026-10-11T10:00:00Z", "no spawn here"));
    s.subagent("a9", &line("2026-10-11T10:00:01Z", "orphan"));
    let tasks = s.tasks(&[]);
    assert_eq!(tasks.len(), 1);
    let m = meta(&tasks[0]);
    assert_eq!(m["tool_use_id"], "a9");
    assert_eq!(m["agent_id"], "a9");
    assert_eq!(m["spawn"], "unmatched");
    assert_eq!(m["parent_session"], SESSION);
    assert!(
        tasks[0].transcript.is_some(),
        "an unmatched file is still sealed"
    );
}

#[test]
fn a_nested_subagent_is_matched_through_its_parent_subagents_file() {
    let s = Session::new(&launched("toolu_outer", "a1"));
    s.subagent("a1", &launched("toolu_inner", "a2"));
    s.subagent("a2", &line("2026-10-11T10:00:01Z", "nested work"));
    let names: Vec<String> = s.tasks(&[]).iter().map(|t| t.tool_use_id.clone()).collect();
    assert_eq!(names, ["toolu_outer", "toolu_inner"]);
}

#[test]
fn the_claimed_address_wins_over_the_listing_and_carries_the_type() {
    let s = Session::new(&launched("toolu_a", "a1"));
    s.subagent("a1", &line("2026-10-11T10:00:01Z", "stale copy"));
    let moved = s.main.with_file_name("moved-a1.jsonl");
    std::fs::write(&moved, line("2026-10-11T10:00:02Z", "the stated file")).unwrap();
    let dir = rift_shadow::hooks::session_cache::sessions_dir(&s.git_dir).join("s-1.subagents");
    std::fs::create_dir_all(&dir).unwrap();
    let claim = serde_json::json!({
        "schema_version": 1, "agent_id": "a1", "agent_type": "Explore",
        "transcript_path": moved.display().to_string(),
    });
    std::fs::write(dir.join("a1.json"), claim.to_string()).unwrap();
    let tasks = s.tasks(&[]);
    let full = tasks[0].transcript.as_ref().unwrap().full.as_slice();
    assert!(String::from_utf8_lossy(full).contains("the stated file"));
    assert_eq!(meta(&tasks[0])["subagent_type"], "Explore");
}

#[test]
fn an_unreadable_subagent_file_is_recorded_not_dropped() {
    let s = Session::new("");
    std::fs::create_dir_all(s.subagents.join("agent-a1.jsonl")).unwrap();
    let tasks = s.tasks(&[]);
    assert_eq!(meta(&tasks[0])["task_transcript"], "unreadable");
    assert!(tasks[0].transcript.is_none());
}

#[test]
fn the_worktree_is_the_earliest_observation_the_subagents_own_hook_made() {
    let s = Session::new(&launched("toolu_a", "a1"));
    s.subagent("a1", &line("2026-10-11T10:00:01Z", "work"));
    let seen = |name: &str, subagent: Option<&str>, at: i64| ArchivedWorktreeObservation {
        session: SESSION.into(),
        token: format!("{name:0>12}"),
        name: name.into(),
        linked: name != "main",
        branch: None,
        subagent: subagent.map(str::to_owned),
        basis: "payload_cwd".into(),
        at,
    };
    let observations = [
        seen("main", None, 1),
        seen("late", Some("a1"), 9),
        seen("wt", Some("a1"), 5),
    ];
    let m = meta(&s.tasks(&observations)[0]);
    assert_eq!(m["worktree"]["name"], "wt");
    assert_eq!(m["worktree"]["linked"], true);
}

#[test]
fn an_unchanged_session_collects_byte_identical_tasks() {
    let s = Session::new(&launched("toolu_a", "a1"));
    s.subagent("a1", &line("2026-10-11T10:00:01Z", "work"));
    let once: Vec<Vec<u8>> = s
        .tasks(&[])
        .iter()
        .map(ArchivedTask::metadata_json)
        .collect();
    let twice: Vec<Vec<u8>> = s
        .tasks(&[])
        .iter()
        .map(ArchivedTask::metadata_json)
        .collect();
    assert_eq!(once, twice);
}

/// The redaction corpus over a subagent file: every seeded secret and PII row
/// is masked in both sealed artifacts, as it is in the main transcript.
#[test]
fn the_redaction_corpus_leaves_nothing_verbatim_in_a_subagent_transcript() {
    const CORPUS: [&str; 8] = [
        "AKIAIOSFODNN7EXAMPLE",
        "ghp_1234567890abcdefghijklmnopqrstuvwxyzAB",
        "123-45-6789",
        "+1-415-555-0199",
        "4111 1111 1111 1111",
        "alice@example.com",
        "xoxb-123456789012-1234567890123-abcdefghijklmnopqrstuvwx",
        "sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGHIJ",
    ];
    let s = Session::new(&launched("toolu_a", "a1"));
    let body: String = CORPUS
        .iter()
        .map(|secret| line("2026-10-11T10:00:01Z", &format!("value {secret} here")))
        .collect();
    s.subagent("a1", &body);
    let tasks = s.tasks(&[]);
    let pair = tasks[0].transcript.as_ref().unwrap();
    for artifact in [pair.full.as_slice(), pair.compact.as_slice()] {
        let text = String::from_utf8_lossy(artifact);
        for secret in CORPUS {
            assert!(!text.contains(secret), "{secret} survived: {text}");
        }
    }
}

#[test]
fn the_time_span_reads_the_earliest_and_latest_record() {
    let body = line("2026-10-11T10:00:05.250Z", "b")
        + &line("2026-10-11T10:00:01Z", "a")
        + "{\"timestamp\":\"not a time\"}\n";
    assert_eq!(
        time_span(body.as_bytes()),
        (1_791_712_801_000, 1_791_712_805_250)
    );
    assert_eq!(time_span(b"{}"), (0, 0));
}

#[test]
fn only_a_safe_agent_id_is_read_out_of_a_result_text() {
    let content = serde_json::json!("agentId: a1 then agentId: ../x and agentId: ");
    assert_eq!(agent_ids_in(Some(&content)), ["a1"]);
    assert!(agent_ids_in(None).is_empty());
}

/// A spawn result in the structured form a foreground subagent records.
fn finished(call: &str, agent: &str) -> String {
    serde_json::json!({
        "type": "user",
        "message": { "role": "user", "content": [{
            "type": "tool_result", "tool_use_id": call,
            "content": [{ "type": "text", "text": "done" }],
        }]},
        "toolUseResult": { "agentId": agent, "status": "completed" },
    })
    .to_string()
        + "\n"
}

/// Tool output is untrusted text: an earlier result that merely says
/// `agentId: a1` must not take a1 from the call whose structured result names it.
#[test]
fn a_structured_spawn_wins_over_an_earlier_text_claim() {
    let body = launched("toolu_forged", "a1") + &finished("toolu_real", "a1");
    let s = Session::new(&body);
    s.subagent("a1", &line("2026-10-11T10:00:01Z", "work"));
    assert_eq!(s.tasks(&[])[0].tool_use_id, "toolu_real");
}

/// A listed file that is a symlink is recorded unreadable and never followed.
#[cfg(unix)]
#[test]
fn a_listed_symlink_is_unreadable_and_not_followed() {
    let s = Session::new(&launched("toolu_a", "a1"));
    let target = s.main.with_file_name("elsewhere.jsonl");
    std::fs::write(&target, line("2026-10-11T10:00:01Z", "outside")).unwrap();
    std::os::unix::fs::symlink(&target, s.subagents.join("agent-a1.jsonl")).unwrap();
    let tasks = s.tasks(&[]);
    assert_eq!(meta(&tasks[0])["task_transcript"], "unreadable");
    assert!(tasks[0].transcript.is_none());
}

/// A claimed address that is a symlink is refused the same way, and a claim
/// whose address is not a `.jsonl` file is not a claim at all.
#[cfg(unix)]
#[test]
fn a_claimed_symlink_or_non_jsonl_address_is_never_read() {
    let s = Session::new(&(launched("toolu_a", "a1") + &launched("toolu_b", "a2")));
    let real = s.main.with_file_name("real.jsonl");
    std::fs::write(&real, line("2026-10-11T10:00:01Z", "outside")).unwrap();
    let link = s.main.with_file_name("link.jsonl");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let not_jsonl = s.main.with_file_name("notes.txt");
    std::fs::write(&not_jsonl, "secret notes").unwrap();
    let dir = rift_shadow::hooks::session_cache::sessions_dir(&s.git_dir).join("s-1.subagents");
    std::fs::create_dir_all(&dir).unwrap();
    for (agent, path) in [("a1", &link), ("a2", &not_jsonl)] {
        let claim = serde_json::json!({
            "schema_version": 1, "agent_id": agent, "transcript_path": path.display().to_string(),
        });
        std::fs::write(dir.join(format!("{agent}.json")), claim.to_string()).unwrap();
    }
    let tasks = s.tasks(&[]);
    assert_eq!(tasks.len(), 1, "the non-.jsonl claim is no candidate");
    assert_eq!(meta(&tasks[0])["agent_id"], "a1");
    assert_eq!(meta(&tasks[0])["task_transcript"], "unreadable");
}

/// A spill a subagent's result names lands beside that subagent's
/// transcript, in its own subtree's manifest, though the file sits in the
/// parent session's `tool-results` directory where the client keeps it.
#[test]
fn a_spill_named_by_a_subagent_lands_in_its_subtree() {
    let s = Session::new(&launched("toolu_a", "a1"));
    let results = s.subagents.parent().unwrap().join("tool-results");
    std::fs::create_dir_all(&results).unwrap();
    let file = results.join("bsub01.txt");
    std::fs::write(&file, "subagent spill AKIAIOSFODNN7EXAMPLE\n").unwrap();
    let envelope = format!(
        "<persisted-output>\nOutput too large (34.8KB). Full output saved to: {}\n\nPreview (first 2KB):\nsub",
        file.display()
    );
    let result = serde_json::json!({
        "type": "user", "isSidechain": true, "agentId": "a1",
        "message": { "role": "user", "content": [{
            "type": "tool_result", "tool_use_id": "toolu_inner_read",
            "content": [{ "type": "text", "text": envelope }],
        }]},
    });
    s.subagent("a1", &format!("{result}\n"));
    let tasks = s.tasks(&[]);
    let spills = &tasks[0].transcript.as_ref().unwrap().spills;
    assert_eq!(spills.len(), 1);
    assert_eq!(spills[0].tool_use_id, "toolu_inner_read");
    let body = String::from_utf8_lossy(spills[0].body.as_ref().unwrap().as_slice()).into_owned();
    assert!(body.starts_with("subagent spill") && !body.contains("AKIAIOSFODNN7EXAMPLE"));
    let m = meta(&tasks[0]);
    assert_eq!(m["spills"][0]["tool_use_id"], "toolu_inner_read", "{m}");
    assert_eq!(m["spills"][0]["bytes"], body.len());
}
