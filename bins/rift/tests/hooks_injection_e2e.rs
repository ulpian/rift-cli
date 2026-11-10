//! End-to-end: what a hook hands the model on a boundary — the recovery map
//! after a compaction, the pointer a subagent's window is introduced with, and
//! the same facts on Codex's and Antigravity's own pinned channels.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use std::path::Path;

mod common;
#[allow(dead_code)] // The hooks suites share these builders; each file uses a subset.
#[path = "common/hooks_fixture.rs"]
mod hooks_fixture;
use hooks_fixture::{
    agent_hook, agent_hook_out, agent_hook_raw, declined_with, enable_recall, enforce_for,
    journal_kinds, json_str, seed_live_session_repo, sync_env, window_key,
};

/// Drive one native Claude Code hook and hand back what it wrote to stdout —
/// the bytes the client parses, which is the only place an injection can be
/// observed as the agent sees it.
fn agent_hook_stdout(
    repo: &Path,
    verb: &str,
    session: &str,
    turn: u32,
    payload: &str,
    env: &[(&str, &str)],
) -> String {
    agent_hook_out(repo, "claude-code", verb, session, turn, payload, env)
}

/// The text a boundary hook handed the model, out of the object the client
/// reads: the family's pinned context field, on the verb it answered.
fn additional_context(out: &str, verb: &str) -> String {
    let response: serde_json::Value = serde_json::from_str(out.trim())
        .unwrap_or_else(|e| panic!("the hook wrote an object the client can read: {e} in {out:?}"));
    assert_eq!(
        response["hookSpecificOutput"]["hookEventName"], verb,
        "{response}"
    );
    response["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap_or_else(|| panic!("the text travels in this family's field: {response}"))
        .to_owned()
}

/// Through the real binary: a session reads a file, its context is compacted,
/// and the hook that starts the session again hands it back the map of what it
/// stopped holding — once, on the family's own pinned channel, with the verdict
/// that lets the model skip the re-read altogether.
///
/// This is the whole boundary in one walk, and each step is a fact the unit
/// tests cannot state: that the second boundary verb is wired at all (the
/// family's pinned surface carries two, and only one of them was written on
/// before), that the map travels in the field the client reads, that the
/// journal lines the compaction did not delete are what it is built from, and
/// that the same hook arriving twice does not hand the map over twice.
#[test]
fn a_compacted_context_is_handed_what_it_stopped_holding() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    let cache = tmp.path().join("cache");
    let env = [
        ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
        ("RIFT_NO_KEYCHAIN", "1"),
        ("RIFT_CAPTURE_SYNC", "1"),
    ];
    std::fs::create_dir_all(repo.join(".rift")).unwrap();
    std::fs::write(
        repo.join(".rift").join("config.toml"),
        "schema_version = 1\n[project]\nname = \"t\"\n[features]\ntotal_recall = true\n",
    )
    .unwrap();
    let file = repo.join("a.txt");
    std::fs::write(&file, "one\ntwo\nthree\n").unwrap();
    let read = format!(
        r#"{{"tool_name":"Read","tool_input":{{"file_path":{},"offset":1,"limit":500}}}}"#,
        json_str(&file)
    );
    agent_hook(&repo, "PreToolUse", "e2e-live-1", 1, &read, &env);
    agent_hook(&repo, "PreCompact", "e2e-live-1", 2, "{}", &env);

    let out = agent_hook_stdout(
        &repo,
        "SessionStart",
        "e2e-live-1",
        3,
        r#"{"source":"compact"}"#,
        &env,
    );
    let map = additional_context(&out, "SessionStart");
    assert!(map.contains("a.txt@"), "{map}");
    assert!(map.contains("L1-3"), "the lines it held: {map}");
    assert!(
        map.contains("unchanged since your read"),
        "nothing moved, so the re-read can be skipped: {map}"
    );
    assert!(
        !map.contains("one\ntwo"),
        "identifiers and numbers, never a byte of the file: {map}"
    );

    // The same boundary again hands over no second map.
    let again = agent_hook_stdout(
        &repo,
        "SessionStart",
        "e2e-live-1",
        4,
        r#"{"source":"compact"}"#,
        &env,
    );
    assert!(
        !again.contains("stopped holding") && !again.contains("Context was compacted"),
        "one eviction, one map: {again}"
    );

    // And the read after the compaction is a first read again: the journal kept
    // the lines it was built from, and they satisfy nothing.
    agent_hook(&repo, "PreToolUse", "e2e-live-1", 5, &read, &env);
    assert_eq!(
        journal_kinds(&repo),
        vec!["read", "evict", "evict", "hold", "evict", "read"],
        "the pre-eviction lines stay, the map records what it announced, and \
         the re-read is not a repeat"
    );
}

/// Through the real binary: the window a subagent runs in is introduced at the
/// one boundary it has, exactly once, and its parent's own introduction is
/// neither spent by it nor owed to it.
///
/// Every step is a fact the unit tests cannot state. That the family answers on
/// this verb at all — the pinned row and the recorded run are both new, and
/// before them a subagent received nothing, because neither hook a session is
/// introduced at fires inside one. That the claim is keyed by the **window**:
/// the payload names the *parent's* session id, so a marker keyed by the
/// session alone turns this walk into one pointer and three silences, in
/// whichever order the hooks happened to arrive. That two subagents of one turn
/// are two windows. And that a file only a subagent read is not announced back
/// to the parent as something it has read itself.
#[test]
fn a_subagents_window_is_introduced_once_at_the_verb_that_opens_it() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    let cache = tmp.path().join("cache");
    let env = sync_env(&cache);
    enable_recall(&repo);
    let session = "e2e-sub-1";

    // The main thread is introduced at a boundary it has.
    let parent = additional_context(
        &agent_hook_stdout(&repo, "UserPromptSubmit", session, 1, "{}", &env),
        "UserPromptSubmit",
    );
    assert!(
        parent.contains("rift recall") && parent.contains("rift agent-help"),
        "the pointer names the surface it points at: {parent}"
    );

    // …and a subagent at the only one it has, under the same session id. Its
    // coordinator held nothing, so it is handed nothing: it works to a brief
    // from a window already shown the map. The window is still claimed.
    let child = subagent_start(&repo, session, "a0e79504", 2, &env);
    assert!(
        child.trim().is_empty(),
        "a subagent whose coordinator held nothing is told nothing: {child:?}"
    );
    let repeat = subagent_start(&repo, session, "a0e79504", 2, &env);
    assert!(
        repeat.trim().is_empty(),
        "one window, one claim — and no manifest behind it: {repeat:?}"
    );
    let sibling = subagent_start(&repo, session, "a2f52a50", 2, &env);
    assert!(
        sibling.trim().is_empty(),
        "a second subagent of the same turn is a second window, told nothing either: {sibling:?}"
    );

    // Three windows, three markers, each named by the key the window journal
    // uses — which is what makes the parent's claim and the children's
    // independent.
    let markers = injection_markers(&repo);
    assert_eq!(markers.len(), 3, "one flat marker per window: {markers:?}");
    for agent_id in ["", "a0e79504", "a2f52a50"] {
        assert!(
            markers.contains(&format!("{}.json", window_key(session, agent_id))),
            "no marker for the window {session}/{agent_id:?}: {markers:?}"
        );
    }

    // A file only the subagent reads belongs to the subagent's window, so the
    // parent's next boundary is not told it has read it.
    let file = repo.join("a.txt");
    std::fs::write(&file, "one\ntwo\nthree\n").unwrap();
    let read = format!(
        r#"{{"tool_name":"Read","tool_input":{{"file_path":{},"offset":1,"limit":500}},"agent_id":"a0e79504","agent_type":"general-purpose"}}"#,
        json_str(&file)
    );
    agent_hook(&repo, "PreToolUse", session, 3, &read, &env);
    std::fs::write(&file, "one\nTWO\nthree\n").unwrap();
    let next = agent_hook_stdout(&repo, "UserPromptSubmit", session, 4, "{}", &env);
    assert!(
        !next.contains("a.txt"),
        "a subagent's read is not the parent's to be told about: {next:?}"
    );
    assert!(
        !next.contains("you have read changed"),
        "the parent has read nothing, so nothing it read changed: {next:?}"
    );
}

/// The one boundary a subagent has, under its parent's session id.
fn subagent_start(
    repo: &Path,
    session: &str,
    agent_id: &str,
    turn: u32,
    env: &[(&str, &str)],
) -> String {
    agent_hook_stdout(
        repo,
        "SubagentStart",
        session,
        turn,
        &format!(r#"{{"agent_id":"{agent_id}","agent_type":"general-purpose"}}"#),
        env,
    )
}

/// The flat injection markers under this checkout's run state, by name.
fn injection_markers(repo: &Path) -> std::collections::BTreeSet<String> {
    std::fs::read_dir(repo.join(".rift").join("state").join("injection"))
        .unwrap()
        .filter_map(std::result::Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect()
}

/// Through the real binary, under a repository that asks for declines: the
/// first re-read after a compaction is allowed and the second is declined
/// again.
///
/// This is the whole of what an eviction does to the decision, stated where it
/// can actually fail. Coverage recorded before the boundary is kept — the
/// recovery map is built from it — and satisfies nothing, so the context that
/// no longer holds the bytes is never starved of them; the read that follows
/// re-records what it was handed, and the repeat after *that* is a repeat
/// again. Read as one walk: declined, compacted, allowed, declined.
#[test]
fn the_second_post_compaction_read_of_an_unchanged_file_is_denied() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    let cache = tmp.path().join("cache");
    let env = [
        ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
        ("RIFT_NO_KEYCHAIN", "1"),
        ("RIFT_CAPTURE_SYNC", "1"),
    ];
    std::fs::create_dir_all(repo.join(".rift")).unwrap();
    std::fs::write(
        repo.join(".rift").join("config.toml"),
        "schema_version = 1\n[project]\nname = \"t\"\n[features]\ntotal_recall = true\n",
    )
    .unwrap();
    enforce_for(&repo, "claude-code");
    let file = repo.join("b.txt");
    std::fs::write(&file, "one\ntwo\nthree\n").unwrap();
    let read = format!(
        r#"{{"tool_name":"Read","tool_input":{{"file_path":{},"offset":1,"limit":500}}}}"#,
        json_str(&file)
    );
    let read_at = |turn: u32| -> String {
        agent_hook_stdout(&repo, "PreToolUse", "e2e-live-1", turn, &read, &env)
    };

    assert!(
        declined_with(&read_at(1)).is_none(),
        "the first read of a file is never declined"
    );
    let repeat = declined_with(&read_at(2)).expect("the repeat is declined while it is held");
    assert!(repeat.contains("b.txt"), "{repeat}");

    // The context is compacted, and is handed what it stopped holding.
    agent_hook(&repo, "PreCompact", "e2e-live-1", 3, "{}", &env);
    let start = agent_hook_stdout(
        &repo,
        "SessionStart",
        "e2e-live-1",
        4,
        r#"{"source":"compact"}"#,
        &env,
    );
    assert!(
        start.contains("unchanged since your read"),
        "the map says the file did not move: {start}"
    );

    assert!(
        declined_with(&read_at(5)).is_none(),
        "the first re-read after a compaction is a first read: the window no \
         longer holds the bytes the map only *named*"
    );
    let second = declined_with(&read_at(6))
        .expect("and the read after that one is a repeat of what it now holds");
    assert!(second.contains("b.txt"), "{second}");

    assert_eq!(
        journal_kinds(&repo),
        vec!["read", "deny", "evict", "evict", "hold", "read", "deny"],
        "declined, compacted, told what it had held, allowed, declined — and \
         the read that follows the announcement answers it, so nothing books \
         a read that did happen"
    );
}

/// The reads this family makes: twelve bounded `sed` reads through `shell`, spelled the way
/// the client spells them — a relative path resolved against the working
/// directory the payload states. The strict recogniser is what puts those
/// lines in the journal a map is built from, so without it this family's map
/// would be empty; and nothing declines them, because this family is observed.
/// Each call is also the standing assertion that injection stays off the gate
/// path: this verb answers zero bytes.
fn codex_shell_reads(repo: &Path, session: &str, env: &[(&str, &str)]) -> Vec<String> {
    let files: Vec<String> = (0..12)
        .map(|i| format!("src/module-{i:02}/file.rs"))
        .collect();
    for name in &files {
        let path = repo.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut body = String::new();
        for line in 1..=40 {
            std::fmt::Write::write_fmt(&mut body, format_args!("line {line}\n")).unwrap();
        }
        std::fs::write(&path, body).unwrap();
        let read = format!(
            r#"{{{},"tool_name":"Bash","tool_input":{{"command":"sed -n '1,40p' {name}"}}}}"#,
            codex_session_fields(repo, session)
        );
        let out = agent_hook_raw(repo, "codex", "PreToolUse", &read, env);
        // Byte-empty, which is two facts in one: this family is observed and
        // never declined, and the gate path is not an injection channel — a
        // pointer or a map that leaked onto it would show up here as bytes.
        assert!(
            out.is_empty(),
            "the gate path answers nothing at all for this family — no \
             decision, and no additionalContext: {out:?}"
        );
    }
    files
}

/// The two fields every payload of this family carries that the boundary and
/// the gate read: which conversation this is, and where the agent was standing.
fn codex_session_fields(repo: &Path, session: &str) -> String {
    format!(
        r#""session_id":"{session}","cwd":{}"#,
        json_str(&repo.canonicalize().unwrap())
    )
}

/// Recall on, and a map budget an operator would have to write: the family's
/// declared bound sits above every budget Rift ships, so a list that overruns
/// its ceiling is reached by lowering the operator's number rather than by
/// seeding hundreds of reads into one window.
fn seed_recall_with_a_small_map_budget(repo: &Path) {
    std::fs::create_dir_all(repo.join(".rift").join("policies")).unwrap();
    std::fs::write(
        repo.join(".rift").join("config.toml"),
        "schema_version = 1\n[project]\nname = \"t\"\n[features]\ntotal_recall = true\n",
    )
    .unwrap();
    std::fs::write(
        repo.join(".rift").join("policies").join("serve.toml"),
        "[interception]\nrecovery_budget = 200\n",
    )
    .unwrap();
}

/// A map that overran its budget lost whole entries and counted them: every
/// line that survived names a file, the revision the window held and the lines
/// it can honestly claim, and what went is a number rather than a cut.
///
/// A `cat` names no end — the client's own result cap decides where the output
/// stops — so the lines an entry claims start and end at the line it began on.
fn assert_the_map_dropped_whole_entries(map: &str, files: &[String]) {
    assert!(
        rift_tokens::count(map) <= 200,
        "inside the operator's budget: {map}"
    );
    let shown: Vec<&str> = map.lines().filter(|line| line.starts_with("- ")).collect();
    assert!(
        !shown.is_empty() && shown.len() < files.len(),
        "entries were dropped, not the frame: {map}"
    );
    assert!(map.contains("not shown"), "and counted: {map}");
    for line in &shown {
        assert!(
            files.iter().any(|name| line.contains(name.as_str()))
                && line.contains('@')
                && line.contains("L1-"),
            "a dropped entry is dropped whole: {line}"
        );
    }
}

/// Through the shipped binary, on the Codex stdin contract: this family's two
/// boundary hooks carry the same facts Claude Code's do — the session pointer
/// once, silence on a quiet turn, and the recovery map after a compaction —
/// on the context field its own generated schemas pin, inside the bound Rift
/// declared for its own hook handlers.
///
/// Each step is a fact the unit tests cannot state: that the family answers on
/// the channel at all (it was pinned and unwritten until the bound had a single
/// home), that both of its boundary verbs are wired, that the quiet turn stays
/// quiet, that the reads this family makes through `shell` are what the map is
/// built from, that the map rides the first boundary hook after this family's
/// own compaction signal — it names no source, so the journal's last signal
/// is what decides — and that a map larger than the budget is paid for in whole
/// entries and a count, never in a cut the client makes.
///
/// Boundary injection is independent of whether a separate tool call is
/// allowed or declined; this walk isolates the context channel.
#[test]
fn codex_receives_the_pointer_and_manifest_through_its_pinned_channel() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    let cache = tmp.path().join("cache");
    let env = sync_env(&cache);
    seed_recall_with_a_small_map_budget(&repo);

    // The gate path is not a context channel, and this is where that is
    // enforced end to end: a tool call on a session that has never been
    // introduced is the one payload an injection would have something to say
    // on, and the answer is still zero bytes. A pointer or a map that leaked
    // onto this verb reddens here rather than arriving beside a decision.
    let gate = agent_hook_raw(
        &repo,
        "codex",
        "PreToolUse",
        &format!(
            r#"{{{},"tool_name":"Bash","tool_input":{{"command":"cat README.md"}}}}"#,
            codex_session_fields(&repo, "codex-e2e-gate")
        ),
        &env,
    );
    assert!(
        gate.is_empty(),
        "the gate path carries no injection: {gate:?}"
    );

    // The first prompt of a session is introduced, once.
    let pointer = codex_context(
        &codex_boundary(&repo, "UserPromptSubmit", "codex-e2e-1", &env),
        "UserPromptSubmit",
    );
    assert!(
        pointer.contains("rift recall") && pointer.contains("rift agent-help"),
        "the pointer names the surface it points at: {pointer}"
    );

    // A quiet turn is quiet: nothing has changed and nothing is owed, so this
    // family's contract for "no answer" is no bytes at all.
    let quiet = codex_boundary(&repo, "UserPromptSubmit", "codex-e2e-1", &env);
    assert!(
        quiet.trim().is_empty(),
        "an unchanged known state injects nothing: {quiet:?}"
    );

    // The other boundary verb is wired too, and a second session is a session
    // that holds nothing and is introduced again.
    let start = codex_context(
        &codex_boundary(&repo, "SessionStart", "codex-e2e-2", &env),
        "SessionStart",
    );
    assert_eq!(start, pointer, "the same introduction on either boundary");
}

/// The recovery map rides the first boundary hook after this family's own
/// compaction signal — it names no source, so the journal's last signal is
/// what decides — built from the reads this family makes through `shell`; a
/// map larger than the budget is paid for in whole entries and a count, never
/// in a cut the client makes; and the boundary after that hands over no
/// second map.
#[test]
fn codex_receives_the_recovery_map_once_after_its_own_compaction_signal() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    let cache = tmp.path().join("cache");
    let env = sync_env(&cache);
    seed_recall_with_a_small_map_budget(&repo);
    codex_context(
        &codex_boundary(&repo, "UserPromptSubmit", "codex-e2e-1", &env),
        "UserPromptSubmit",
    );

    let files = codex_shell_reads(&repo, "codex-e2e-1", &env);
    codex_boundary(&repo, "PostCompact", "codex-e2e-1", &env);
    let map = codex_context(
        &codex_boundary(&repo, "UserPromptSubmit", "codex-e2e-1", &env),
        "UserPromptSubmit",
    );
    assert!(
        map.contains("Context was compacted"),
        "the map says why it is here: {map}"
    );
    assert!(
        map.contains("unchanged since your read"),
        "nothing moved, so the re-read can be skipped: {map}"
    );
    assert!(
        !map.contains("one\ntwo"),
        "identifiers and numbers, never a byte of a file: {map}"
    );
    // Over the budget, so it is paid for in whole entries and a count — the
    // client never sees a head, a tail and a path to the rest.
    assert_the_map_dropped_whole_entries(&map, &files);

    // And the boundary after that hands over no second map.
    let again = codex_boundary(&repo, "UserPromptSubmit", "codex-e2e-1", &env);
    assert!(
        !again.contains("Context was compacted"),
        "one eviction, one map: {again}"
    );
}

/// One of this family's boundary hooks, with the two fields every payload of
/// it carries and nothing else: what it wrote to stdout.
fn codex_boundary(repo: &Path, verb: &str, session: &str, env: &[(&str, &str)]) -> String {
    let body = format!("{{{}}}", codex_session_fields(repo, session));
    agent_hook_raw(repo, "codex", verb, &body, env)
}

/// The context field, and the assertion that what travels in it is inside
/// the bound written into this family's own hook configuration — which is
/// what keeps the client from spilling the middle of a frame to a file.
fn codex_context(out: &str, verb: &str) -> String {
    let declared = rift_agent::respond::DECLARED_CONTEXT_LIMIT_TOKENS;
    let text = additional_context(out, verb);
    assert!(
        rift_tokens::count(&text) <= declared,
        "{} tokens over the declared bound of {declared}",
        rift_tokens::count(&text)
    );
    text
}

/// Through the shipped binary, on the Antigravity stdin contract: the hook that
/// runs before a model invocation hands the session its pointer **once per
/// execution**, at the first invocation and at no other.
///
/// This family's boundary hook is the one that fires before every invocation
/// rather than once a turn, so the walk is written the way the loop actually
/// runs: the first invocation, then the invocations inside the tool loop, then
/// the hook after the invocation, then the same first invocation delivered
/// twice, then a second execution. Each step is a fact the unit tests cannot
/// state — that the step travels in the field this family's client reads, that
/// the kind is the one a recorded run found still readable later in the loop,
/// that a repeat delivery costs nothing, and that a new execution is a new
/// session and is introduced again.
#[test]
fn antigravity_is_injected_once_per_execution_at_invocation_zero() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    let cache = tmp.path().join("cache");
    let env = [
        ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
        ("RIFT_NO_KEYCHAIN", "1"),
        ("RIFT_CAPTURE_SYNC", "1"),
    ];
    std::fs::create_dir_all(repo.join(".rift")).unwrap();
    std::fs::write(
        repo.join(".rift").join("config.toml"),
        "schema_version = 1\n[project]\nname = \"t\"\n[features]\ntotal_recall = true\n",
    )
    .unwrap();
    let boundary = |conversation: &str, invocation: u32| {
        format!(
            r#"{{"conversationId":"{conversation}","invocationNum":{invocation},"initialNumSteps":1,"modelName":"m","workspacePaths":[{}]}}"#,
            json_str(&repo)
        )
    };
    let answer = |body: &str, verb: &str| -> serde_json::Value {
        let out = agent_hook_raw(&repo, "antigravity", verb, body, &env);
        serde_json::from_str(out.trim()).unwrap_or_else(|e| {
            panic!("the hook wrote an object the client can read: {e} in {out:?}")
        })
    };

    // The first invocation of an execution: one step, of the kind the pinned
    // row names, carrying the pointer.
    let first = answer(&boundary("agy-e2e-1", 0), "PreInvocation");
    let steps = first["injectSteps"]
        .as_array()
        .unwrap_or_else(|| panic!("the pointer travels as a trajectory step: {first}"));
    assert_eq!(steps.len(), 1, "one boundary, one step: {first}");
    let pointer = steps[0]["userMessage"]
        .as_str()
        .unwrap_or_else(|| panic!("the kind that is still readable later: {first}"))
        .to_owned();
    assert!(
        pointer.contains("rift recall") && pointer.contains("rift agent-help"),
        "the pointer names the surface it points at: {pointer}"
    );

    // Every invocation inside the tool loop, and the hook after one, is the
    // acknowledgement the family requires and nothing else.
    for invocation in [1_u32, 2, 7] {
        assert_eq!(
            answer(&boundary("agy-e2e-1", invocation), "PreInvocation"),
            serde_json::json!({}),
            "invocation {invocation} is inside the loop, not at its boundary"
        );
    }
    assert_eq!(
        answer(&boundary("agy-e2e-1", 7), "PostInvocation"),
        serde_json::json!({}),
        "one loop injected into twice spends twice for one fact"
    );

    // The same first invocation delivered again: the claim is durable, so the
    // conversation is introduced once however often the hook arrives.
    let again = answer(&boundary("agy-e2e-1", 0), "PreInvocation");
    assert!(
        !again.to_string().contains("rift agent-help"),
        "one execution, one pointer: {again}"
    );

    // A second execution is a second conversation, and is introduced again.
    let second = answer(&boundary("agy-e2e-2", 0), "PreInvocation");
    assert_eq!(
        second["injectSteps"][0]["userMessage"],
        serde_json::Value::from(pointer.as_str()),
        "a new execution holds nothing, and is handed the same introduction"
    );
}
