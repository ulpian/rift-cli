//! A transcript written in the test, small enough to count by hand, so the
//! rows added after the buckets each have a pinned figure: the first sights
//! cut by the two floors, a compaction and the reading after it, the pull
//! verbs actually run, and the by-name re-reads cut by what was handed down.

use std::fmt::Write as _;
use std::path::Path;

use serde_json::{json, Value};

use super::inheritance::Inheritance;
use super::render::{render_json, render_table};
use super::replay::replay;
use super::report::Report;
use super::tests::SENTINEL;
use super::Options;

/// One session being written: its records, in order, one second apart.
struct Session {
    lines: Vec<String>,
    clock: u32,
    /// What every record identity starts with, so two windows never share one.
    prefix: &'static str,
    /// The prompt id the records carry, where the client writes one.
    prompt_id: Option<&'static str>,
}

impl Session {
    fn new() -> Self {
        Self::window("u", 0, None)
    }

    /// A window whose records start at second `clock`.
    fn window(prefix: &'static str, clock: u32, prompt_id: Option<&'static str>) -> Self {
        Self {
            lines: Vec::new(),
            clock,
            prefix,
            prompt_id,
        }
    }

    /// The record every kind shares, stamped one second after the last.
    fn record(&mut self, kind: &str, mut extra: Value) -> String {
        self.clock += 1;
        let stamp = format!(
            "2026-02-01T{:02}:{:02}:{:02}.000Z",
            self.clock / 3600,
            self.clock / 60 % 60,
            self.clock % 60
        );
        let uuid = format!("{}{:04}", self.prefix, self.clock);
        if let Some(map) = extra.as_object_mut() {
            if let Some(id) = self.prompt_id {
                map.insert("promptId".to_owned(), json!(id));
            }
            map.insert("type".to_owned(), json!(kind));
            map.insert("uuid".to_owned(), json!(uuid));
            map.insert("sessionId".to_owned(), json!("sess-rows"));
            map.insert("cwd".to_owned(), json!("/repo"));
            map.insert("timestamp".to_owned(), json!(stamp));
        }
        extra.to_string()
    }

    /// A person's prompt, which starts a turn.
    fn prompt(&mut self, text: &str) {
        let line = self.record(
            "user",
            json!({ "message": { "role": "user", "content": format!("{SENTINEL} {text}") } }),
        );
        self.lines.push(line);
    }

    /// A tool call and its result.
    fn call(&mut self, id: &str, name: &str, input: &Value, result: &str) {
        let call = self.record(
            "assistant",
            json!({ "message": { "role": "assistant", "content": [
                { "type": "tool_use", "id": id, "name": name, "input": input }
            ] } }),
        );
        let answer = self.record(
            "user",
            json!({ "message": { "role": "user", "content": [
                { "type": "tool_result", "tool_use_id": id, "content": result }
            ] } }),
        );
        self.lines.push(call);
        self.lines.push(answer);
    }

    /// A read through the read tool.
    fn read(&mut self, id: &str, path: &str, range: Option<(u32, u32)>) {
        let mut input = json!({ "file_path": format!("/repo/{path}") });
        if let (Some((offset, limit)), Some(map)) = (range, input.as_object_mut()) {
            map.insert("offset".to_owned(), json!(offset));
            map.insert("limit".to_owned(), json!(limit));
        }
        self.call(
            id,
            "Read",
            &input,
            "alpha beta gamma delta epsilon zeta eta",
        );
    }

    /// A shell call.
    fn shell(&mut self, id: &str, command: &str, result: &str) {
        self.call(id, "Bash", &json!({ "command": command }), result);
    }

    /// The two records the client writes for one compaction.
    fn compaction(&mut self) {
        let boundary = self.record("system", json!({ "subtype": "compact_boundary" }));
        let summary = self.record(
            "user",
            json!({ "isCompactSummary": true,
                    "message": { "role": "user", "content": "the summary" } }),
        );
        self.lines.push(boundary);
        self.lines.push(summary);
    }
}

/// `n` short lines.
fn body(n: u32) -> String {
    (1..=n).fold(String::new(), |mut out, i| {
        let _ = writeln!(out, "line {i} of plain text");
        out
    })
}

/// The session replayed against a checkout holding the files it reads.
fn replayed(session: &Session) -> Report {
    replayed_tree(session, None)
}

/// The session and the subagent it spawned, replayed against a checkout
/// holding the files they read. The subagent's sidecar names the call `ag1`.
fn replayed_tree(session: &Session, agent: Option<&Session>) -> Report {
    let tmp = tempfile::tempdir().unwrap();
    let corpus = tmp.path().join("corpus");
    std::fs::create_dir_all(&corpus).unwrap();
    std::fs::write(corpus.join("sess-rows.jsonl"), session.lines.join("\n")).unwrap();
    if let Some(agent) = agent {
        let dir = corpus.join("sess-rows").join("subagents");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("agent-x.jsonl"), agent.lines.join("\n")).unwrap();
        let meta = json!({ "agentType": "general-purpose", "toolUseId": "ag1" });
        std::fs::write(dir.join("agent-x.meta.json"), meta.to_string()).unwrap();
    }
    let repo = tmp.path().join("repo");
    for (path, lines) in [
        ("src/big.rs", 500),
        ("src/other.rs", 500),
        ("src/pages.rs", 500),
        ("src/multi.rs", 500),
        ("src/tiny.rs", 5),
        ("src/small.rs", 10),
        ("docs/notes.md", 500),
    ] {
        let file = repo.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, body(lines)).unwrap();
    }
    replay(&Options {
        transcripts: Path::new(&corpus).to_path_buf(),
        repo,
        json: false,
        history: None,
        history_refs: vec!["HEAD".to_owned()],
        artifact: None,
    })
    .unwrap()
}

/// The session every test below counts by hand.
fn session() -> Session {
    let mut s = Session::new();
    s.prompt("look around");
    s.read("a1", "src/big.rs", None);
    s.read("a2", "docs/notes.md", Some((1, 100)));
    s.read("a3", "src/small.rs", None);
    s.read("a4", "src/pages.rs", Some((1, 100)));
    s.read("a5", "src/other.rs", Some((120, 40)));
    s.shell("a6", "cat src/multi.rs src/tiny.rs", "alpha beta");
    s.shell(
        "p1",
        "rift recall known-state --task t --file src/big.rs",
        "src/big.rs is new",
    );
    s.shell(
        "p2",
        "cd /tmp/scratch-repo && rift recall changed-hunks",
        "src/a.rs",
    );
    s.shell("p3", "rift recall known-state --help", "usage");
    s.shell(
        "p4",
        "echo 'rift recall retrieve-exact'",
        "rift recall retrieve-exact",
    );
    s.read("a7", "src/big.rs", Some((1, 500)));
    s.compaction();
    s.prompt("again");
    s.read("b1", "src/small.rs", None);
    for turn in 0..5 {
        s.prompt(&format!("later {turn}"));
    }
    s.shell("b2", "sed -n '1,5p' src/big.rs", "line 1 of plain text");
    s
}

/// ★ One call of each shape: a whole-file read of a large code file, a
/// range of a large Markdown file, a small file, a first page, a mid-file
/// range, and a two-file chain holding a large code member — each in its
/// own class, and the classes add up to the first-sight bucket. A compaction
/// empties the window, so what it reads after is a first sight again.
#[test]
fn the_first_sights_are_cut_by_the_two_floors_and_add_up() {
    let report = replayed(&session());
    let json: Value = serde_json::from_str(&render_json(&report)).unwrap();
    let split = &json["first_sight_split"];
    let calls = |key: &str| split[key]["calls"].as_u64().unwrap();
    assert_eq!(
        calls("under_both_floors"),
        2,
        "and the small file again after the compaction"
    );
    assert_eq!(calls("large_not_code"), 1);
    assert_eq!(split["large_not_code_markdown"]["calls"], 1);
    assert_eq!(calls("code_whole"), 1);
    assert_eq!(
        calls("code_first_page"),
        2,
        "and the first lines of big.rs after it"
    );
    assert_eq!(calls("code_mid_file"), 1);
    assert_eq!(calls("multi_member_or_blocked"), 1);
    assert_eq!(split["multi_member_or_blocked_with_large_code"]["calls"], 1);
    let tokens: u64 = [
        "under_both_floors",
        "large_not_code",
        "code_whole",
        "code_first_page",
        "code_mid_file",
        "multi_member_or_blocked",
    ]
    .iter()
    .map(|key| split[key]["tokens"].as_u64().unwrap())
    .sum();
    let bucket = &json["buckets"]["first_sight"];
    assert_eq!(tokens, bucket["tokens"].as_u64().unwrap());
    assert_eq!(report.bucket(super::Bucket::FirstSight).calls, 8);
}

/// ★ The boundary and the summary are one compaction; the small file read
/// in the turn after it is in the storm and a re-read by the read tool; the
/// `sed` read six turns on is a re-read by the shell in the middle band.
#[test]
fn a_compaction_is_counted_once_and_the_rereads_after_it_are_cut() {
    let report = replayed(&session());
    assert_eq!(report.evictions, 2, "two records");
    let json: Value = serde_json::from_str(&render_json(&report)).unwrap();
    let after = &json["post_compaction"];
    assert_eq!(after["compactions"], 1);
    assert_eq!(after["reads_within_three_turns"]["calls"], 1);
    assert_eq!(after["reads_within_three_turns_of_a_held_path"]["calls"], 1);
    assert_eq!(after["held_path_reads_by_channel"]["read_tool"]["calls"], 1);
    assert_eq!(after["held_path_reads_by_channel"]["shell"]["calls"], 1);
    let bands = &after["held_path_reads_by_turn_distance"];
    assert_eq!(bands["up_to_3"]["calls"], 1);
    assert_eq!(bands["from_4_to_14"]["calls"], 1);
    assert_eq!(bands["from_15"]["calls"], 0);
}

/// ★ Two verbs really run (one in a scratch repository), one asked for help,
/// one only named by an `echo`; the answer that named `src/big.rs` was
/// followed by a whole-file read of it.
#[test]
fn the_pull_verbs_are_counted_by_invocation_with_probes_apart() {
    let report = replayed(&session());
    let json: Value = serde_json::from_str(&render_json(&report)).unwrap();
    let pulls = &json["pull_answers"];
    assert_eq!(pulls["by_verb"]["known-state"]["calls"], 1);
    assert_eq!(pulls["by_verb"]["changed-hunks"]["calls"], 1);
    assert_eq!(
        pulls["by_verb"]["retrieve-exact"]["calls"], 0,
        "only an echo"
    );
    assert_eq!(pulls["all"]["calls"], 2);
    assert_eq!(pulls["lookup_verbs"]["calls"], 2);
    assert_eq!(pulls["probes"]["calls"], 1);
    assert_eq!(pulls["asking_for_help"]["calls"], 1);
    assert_eq!(pulls["followed_by_a_read_of_a_named_path"]["calls"], 1);
    assert_eq!(
        pulls["followed_by_a_whole_file_read_of_a_named_path"]["calls"],
        1
    );
    let table = render_table(&report);
    for rendered in [&table, &render_json(&report)] {
        assert!(!rendered.contains(SENTINEL));
        assert!(!rendered.contains("scratch-repo"));
        assert!(!rendered.contains("big.rs"));
    }
}

/// ★ A spawner that read lines 1-100 of one file and lines 1-40 of another
/// before it spawned, and a third file only after, with a prompt naming all
/// three: the child's read inside the first handed-down run is counted
/// inherited, its read of the second beyond the run it was handed is the
/// middle class, and its read of the third, which the spawner held no line
/// of at the spawn, is the first. The three add up to the by-name row.
#[test]
fn the_by_name_reads_are_cut_by_what_the_spawner_handed_down() {
    let mut main = Session::window("m", 0, Some("P1"));
    main.prompt("coordinate");
    main.read("m1", "src/other.rs", Some((1, 100)));
    main.read("m2", "src/multi.rs", Some((1, 40)));
    let naming = json!({
        "prompt": "Review src/other.rs, src/multi.rs and src/pages.rs",
        "subagent_type": "general-purpose"
    });
    main.call("ag1", "Agent", &naming, "done");
    main.read("m3", "src/pages.rs", Some((1, 100)));
    let mut child = Session::window("c", 200, Some("P1"));
    child.prompt("Review src/other.rs, src/multi.rs and src/pages.rs");
    child.read("c1", "src/other.rs", Some((1, 100)));
    child.read("c2", "src/multi.rs", Some((1, 200)));
    child.read("c3", "src/pages.rs", Some((1, 100)));
    let report = replayed_tree(&main, Some(&child));
    let by_name = report.sibling_by_inheritance[Inheritance::ByName.index()];
    assert_eq!(by_name.calls, 3, "{:?}", report.sibling_by_inheritance);
    let json: Value = serde_json::from_str(&render_json(&report)).unwrap();
    let split = &json["by_name_split"];
    assert_eq!(split["inherited"]["calls"], 1);
    assert_eq!(split["beyond_inherited_lines"]["calls"], 1);
    assert_eq!(split["spawner_held_no_line"]["calls"], 1);
    let sum = |field: &str| -> u64 {
        [
            "spawner_held_no_line",
            "beyond_inherited_lines",
            "inherited",
        ]
        .iter()
        .map(|key| split[key][field].as_u64().unwrap())
        .sum()
    };
    assert_eq!(sum("calls"), by_name.calls);
    assert_eq!(sum("tokens"), by_name.tokens);
}

/// ★ A client writes records as it receives them, so a read written after
/// the compaction can carry a stamp from before it. The reading after a
/// compaction goes by where the record is in the window's file, not by its
/// stamp: this read is a re-read, and a read stamped later but written
/// before the compaction is not.
#[test]
fn the_reading_after_a_compaction_goes_by_file_order_not_by_stamp() {
    let mut s = Session::new();
    s.prompt("go");
    s.read("o1", "src/big.rs", Some((1, 100)));
    s.compaction();
    s.clock -= 4;
    s.read("o2", "src/big.rs", Some((1, 100)));
    s.clock += 20;
    s.read("o3", "src/pages.rs", Some((1, 100)));
    let report = replayed(&s);
    let json: Value = serde_json::from_str(&render_json(&report)).unwrap();
    let after = &json["post_compaction"];
    assert_eq!(after["compactions"], 1);
    assert_eq!(after["held_path_reads_by_channel"]["read_tool"]["calls"], 1);
    assert_eq!(
        after["reads_within_three_turns"]["calls"], 2,
        "both reads follow it"
    );
    assert_eq!(after["reads_within_three_turns_of_a_held_path"]["calls"], 1);
}
