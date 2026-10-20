use super::*;

/// Set a fixture up **on this machine**: its family's projection under
/// the cache root, the one fact `provisioned_here` reads. Recall on in
/// the file is not enough — the injector asks both questions, so a
/// fixture without a projection exercises the "records nothing" arm.
pub(in crate::hooks) fn set_up_here(repo: &std::path::Path) {
    crate::projection::open_projection(repo).expect("the fixture's family projection opens");
}

/// Remove the projection `set_up_here` created under the machine cache.
pub(in crate::hooks) fn drop_projection_store(repo: &std::path::Path) {
    if let Ok(db) = crate::family::projection_db(repo) {
        if let Some(dir) = db.parent() {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

/// A Recall-enabled repository set up on this machine, and the path of
/// its family's recall store.
///
/// A real gitdir, because injection books its spend in the FAMILY's recall
/// store and a family belongs to a repository. The projection lives in the
/// machine cache, keyed by this temp repo's family (which adopts the main
/// work tree's legacy ids, so it is this path's) — so it is the test's to
/// create and, through [`drop_recall_repo`], the test's to remove.
fn recall_repo() -> (tempfile::TempDir, PathBuf) {
    let repo = tempfile::tempdir().unwrap();
    rift_git::test_support::fake_repo(repo.path()).unwrap();
    std::fs::create_dir_all(repo.path().join(".rift")).unwrap();
    std::fs::write(
        repo.path().join(".rift").join("config.toml"),
        "schema_version = 1\n[project]\nname = \"t\"\n[features]\ntotal_recall = true\n",
    )
    .unwrap();
    let db = crate::family::recall_db(repo.path()).unwrap();
    set_up_here(repo.path());
    (repo, db)
}

/// Remove what [`recall_repo`] created under the machine cache.
fn drop_recall_repo(repo: &tempfile::TempDir, db: &Path) {
    if let Some(dir) = db.parent() {
        let _ = std::fs::remove_dir_all(dir);
    }
    drop_projection_store(repo.as_root());
}

/// The fixture's root for a cleanup, whether the test holds its `repo`
/// as a `TempDir` or as a path — the tests spell it both ways.
pub(in crate::hooks) trait AsRoot {
    fn as_root(&self) -> &std::path::Path;
}
impl AsRoot for tempfile::TempDir {
    fn as_root(&self) -> &std::path::Path {
        self.path()
    }
}
impl AsRoot for std::path::Path {
    fn as_root(&self) -> &std::path::Path {
        self
    }
}
impl AsRoot for std::path::PathBuf {
    fn as_root(&self) -> &std::path::Path {
        self
    }
}

/// The family and the verb the injection chain is driven on here, spelled
/// once: a pointer is due at any of this family's boundaries, and the
/// prompt is the one every turn has.
const CC: rift_core::AgentId = rift_core::AgentId::CLAUDE_CODE;
const PROMPT: &str = "UserPromptSubmit";
/// The prompt body the positive path is driven with.
const FIRST_PROMPT: &[u8] = br#"{"session_id":"s-1"}"#;

/// The dedup property this surface guarantees: **at most one pointer per
/// context window**, whatever the ids look like and however many times a
/// hook is retried, crashed, or run concurrently. The claim is the whole
/// mechanism, so the property is stated over the claim.
///
/// The agent id is swept alongside the session id because that is the axis
/// the claim gained: a subagent's hooks arrive under its parent's session,
/// so "once per session" would have been a latch that silenced every
/// window but the first. `None` — the ordinary main thread — is one of the
/// windows swept, and is distinct from every named one.
#[test]
fn at_most_one_pointer_is_ever_claimed_per_context_window() {
    use proptest::prelude::*;

    proptest!(|(ids in proptest::collection::vec("\\PC{0,64}", 1..4),
                agents in proptest::collection::vec(proptest::option::of("\\PC{0,32}"), 1..4),
                retries in 1usize..6)| {
        let repo = tempfile::tempdir().expect("tempdir");
        // The generator may draw one pair twice; "once per window" is a
        // claim about *distinct* windows, and re-presenting a pair is the
        // retry axis `retries` already sweeps — so state it over the set.
        let windows: std::collections::BTreeSet<(&str, Option<&str>)> = ids
            .iter()
            .map(String::as_str)
            .filter(|id| !id.is_empty())
            .flat_map(|id| {
                // An empty agent id and no agent id are ONE window, by the
                // key's own rule: the absent half is spelled as an empty
                // part rather than left out, so the two cannot be told
                // apart and must not be counted apart here either.
                agents.iter().map(move |agent| {
                    (id, agent.as_deref().filter(|agent| !agent.is_empty()))
                })
            })
            .collect();
        for (id, agent) in windows {
            let wins = (0..retries)
                .filter(|_| claim_injection(repo.path(), id, agent).is_some())
                .count();
            prop_assert_eq!(wins, 1, "window {:?}/{:?} injected {} times", id, agent, wins);
        }
        // A distinct window is a distinct claim: dedup must not be a
        // global latch that silences every later window.
        let fresh = claim_injection(repo.path(), "a-session-never-seen-before", None);
        prop_assert!(fresh.is_some());
    });
}

/// A subagent shares its parent's session id and is still introduced
/// exactly once — and introducing it costs the parent nothing.
///
/// Stated over the claim because the claim is where the two windows used
/// to collide: keyed by the session alone, whichever of the two arrived
/// first took the marker and the other was answered with silence forever.
#[test]
fn a_subagent_claims_its_own_pointer_under_its_parents_session() {
    let repo = tempfile::tempdir().unwrap();
    let parent = claim_injection(repo.path(), "s-shared", None);
    let child = claim_injection(repo.path(), "s-shared", Some("a0e79504"));
    let sibling = claim_injection(repo.path(), "s-shared", Some("a2f52a50"));
    assert!(parent.is_some(), "the main thread is introduced");
    assert!(child.is_some(), "and so is the window inside it");
    assert!(sibling.is_some(), "and so is the second one");
    assert_eq!(
        claim_injection(repo.path(), "s-shared", Some("a0e79504")),
        None,
        "the same window twice is one pointer"
    );
    let mut names = [parent, child, sibling]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    names.sort();
    names.dedup();
    assert_eq!(names.len(), 3, "three windows, three occurrences");
    // The occurrence a spend row is keyed by is the window journal's key,
    // and not a second rule that happens to agree with it today.
    assert!(names.contains(&crate::window_journal::key("s-shared", Some("a0e79504"))));
}

/// A session id is agent-supplied text: it names the claim but never
/// becomes a path component, so a traversal-shaped or newline-bearing id
/// cannot escape `.rift/state/injection/`. The agent id is agent-supplied
/// text too, and is screened by the same hash.
#[test]
fn a_hostile_session_id_never_becomes_a_path() {
    let repo = tempfile::tempdir().unwrap();
    let dir = repo.path().join(".rift/state/injection");
    for hostile in [
        "../../../etc/passwd",
        "/absolute/elsewhere",
        "with\nnewline",
        "with/slash",
    ] {
        assert!(claim_injection(repo.path(), hostile, None).is_some());
        assert!(claim_injection(repo.path(), "s-1", Some(hostile)).is_some());
    }
    let entries: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(std::result::Result::ok)
        .collect();
    assert_eq!(entries.len(), 8, "one flat marker per window");
    for e in entries {
        let name = e.file_name().to_string_lossy().into_owned();
        let stem = name
            .strip_suffix(".json")
            .unwrap_or_else(|| panic!("{name} is not a marker file"));
        assert!(
            stem.chars().all(|c| c.is_ascii_hexdigit()),
            "{name} is not a hashed marker name"
        );
    }
}

#[test]
fn antigravity_receives_a_pending_read_replacement_before_the_next_model_call() {
    let repo = tempfile::tempdir().unwrap();
    let raw = rift_shadow::hooks::RawHook {
        agent: rift_core::AgentId::ANTIGRAVITY,
        verb: "PreToolUse".to_owned(),
        session: "s-replacement".to_owned(),
        turn: None,
        occurred_at_ms: 10,
        payload: serde_json::json!({}),
    };
    arm_pending_replacement(repo.path(), &raw, "[rift] lines 4-20 are unchanged");
    let body = br#"{"session_id":"s-replacement","invocationNum":7}"#;
    let candidate = pending_replacement_candidate(
        repo.path(),
        &rift_core::AgentId::ANTIGRAVITY,
        "PreInvocation",
        body,
    )
    .expect("a pending result bypasses the first-invocation-only boundary gate");
    assert_eq!(
        candidate.kind,
        rift_recall_types::ContextKind::ReadReplacement
    );
    assert!(candidate.fallback);
    let plan = rift_recall_serve::ServeDispatcher::plan_context([candidate]);
    let response = rift_agent::respond::respond_with_plan(
        &rift_core::AgentId::ANTIGRAVITY,
        "PreInvocation",
        &plan,
        &rift_agent::respond::GateDecision::Allow,
    )
    .expect("the pinned persistent userMessage channel encodes the plan");
    assert!(response["injectSteps"][0]["userMessage"]
        .as_str()
        .is_some_and(|text| text.contains("lines 4-20 are unchanged")));
    let marker = pending_replacement_path(repo.path(), "s-replacement");
    assert!(marker.exists(), "planning alone does not consume delivery");
    emit_response(&response, Vec::new(), || {
        complete_pending_replacement_in(repo.path(), "s-replacement", &plan);
    })
    .unwrap();
    assert!(
        !marker.exists(),
        "successful local emission consumes it once"
    );
}

#[test]
fn content_offset_runs_raw_but_prepares_a_bounded_structural_outline() {
    let repo = tempfile::tempdir().unwrap();
    let ledger = repo.path().join("LEDGER.md");
    std::fs::write(
        &ledger,
        "# Goal\nbody\n## First gate\nbody\n### Acceptance\nbody\n",
    )
    .unwrap();
    let raw = rift_shadow::hooks::RawHook {
        agent: rift_core::AgentId::ANTIGRAVITY,
        verb: "PreToolUse".to_owned(),
        session: "s-offset".to_owned(),
        turn: None,
        occurred_at_ms: 10,
        payload: serde_json::json!({
            "toolCall": {
                "name": "view_file",
                "args": {
                    "AbsolutePath": ledger,
                    "ContentOffset": 230_000,
                    "StartLine": 270,
                    "EndLine": 339,
                }
            }
        }),
    };
    let plan = antigravity_content_offset_plan(repo.path(), &raw).expect("a bounded plan");
    assert!(plan.contains("L1 # Goal"));
    assert!(plan.contains("L3 ## First gate"));
    assert!(plan.contains("L5 ### Acceptance"));
    assert!(plan.contains("raw call ran untouched"));
    assert!(plan.len() < 4_096);
}

/// Silence is the default: a payload that names no session yields no
/// pointer, so a hook Rift cannot attribute never injects.
#[test]
fn a_payload_without_a_session_yields_no_pointer() {
    assert_eq!(payload_session_id(b"not json at all"), None);
    assert_eq!(payload_session_id(b"{}"), None);
    assert_eq!(payload_session_id(br#"{"session_id":""}"#), None);
    assert_eq!(payload_session_id(br#"{"session_id":123}"#), None);
    assert_eq!(
        payload_session_id(br#"{"session_id":"s-1"}"#).as_deref(),
        Some("s-1")
    );
}

/// A family with no verified context channel is never offered a pointer,
/// and no verb other than the family's injection points is either — while
/// the family whose channel *is* pinned gets it, on both of its boundaries
/// and in the shape the pinned row names.
///
/// "The decision is made before any repository state is touched" is
/// asserted here rather than described: every refusal runs against a
/// repository this test owns, and the root is handed over through a
/// closure that records being asked, so a refusal that reached for a
/// repository at all reddens the test. Nothing in this suite may read or
/// write the checkout it happens to be running in — the developer's own
/// hooks run out of that tree, and a claim marker written there would make
/// every later run of this test pass for the wrong reason.
#[test]
fn only_the_verified_channel_and_verb_can_inject() {
    // Recall on and a projection of its own, so a refusal below is a fact
    // about the family and the verb — never about a repository that could
    // not have answered anyway.
    let (repo, db) = recall_repo();

    for (agent, verb) in [
        ("claude-code", "PostToolUse"),
        ("claude-code", "PreCompact"),
        // The family above has four context channels — its two turn
        // boundaries, the one that opens a subagent's window, and nothing
        // else; the family below has its two. The gate verb carries a
        // reason for one call, not the facts a turn holds, and the end of
        // a turn is not a row either family has at all.
        ("claude-code", "SubagentStop"),
        ("codex", "PreToolUse"),
        ("codex", "Stop"),
        ("gemini", "BeforeModel"),
        ("antigravity", "PreToolUse"),
        ("some-stranger", "UserPromptSubmit"),
    ] {
        let asked = std::cell::Cell::new(false);
        assert_eq!(
            session_pointer_rooted(agent, verb, br#"{"session_id":"s-1"}"#, || {
                asked.set(true);
                Some(repo.path().to_path_buf())
            }),
            None,
            "{agent} {verb} must not inject"
        );
        assert!(
            !asked.get(),
            "{agent} {verb} is refused before any repository is reached for"
        );
    }

    for (verb, session) in [
        ("UserPromptSubmit", "s-codex-prompt"),
        ("SessionStart", "s-codex-start"),
    ] {
        let body = format!(r#"{{"session_id":"{session}"}}"#);
        let text = session_pointer_rooted("codex", verb, body.as_bytes(), || {
            Some(repo.path().to_path_buf())
        })
        .unwrap_or_else(|| panic!("codex {verb} is a pinned boundary and injects"));
        let response = rift_agent::respond::respond(&rift_core::AgentId::CODEX, verb, Some(&text))
            .unwrap_or_else(|| panic!("codex {verb} has a pinned response shape"));
        assert_eq!(
            response["hookSpecificOutput"]["additionalContext"]
                .as_str()
                .unwrap_or_default(),
            text,
            "codex {verb} travels on the channel its pinned row names"
        );
    }

    drop_recall_repo(&repo, &db);
}

/// The positive path, end to end: on a Recall-enabled repository the first
/// prompt of a session gets the pointer, the session's next prompt gets
/// silence, and the tokens the first one cost are booked as spend. Stated
/// over the whole chain (config gate → claim → tokenizer → audit row)
/// because the primitives were already green while the chain was what a
/// caller actually gets; the two booking tests below drive the same chain
/// through [`delivered_first_pointer`].
#[test]
fn the_first_prompt_of_a_session_injects_once() {
    let (repo, db) = recall_repo();
    let pointer = session_pointer_in(repo.path(), &CC, PROMPT, FIRST_PROMPT)
        .expect("the first prompt injects");
    assert!(
        pointer.contains("rift recall"),
        "the pointer must teach the skill surface: {pointer}"
    );
    assert_eq!(
        session_pointer_in(repo.path(), &CC, PROMPT, FIRST_PROMPT),
        None,
        "the same session's next prompt is silent"
    );
    drop_recall_repo(&repo, &db);
}

#[test]
fn a_delivered_pointer_books_its_tokens_as_spend_and_claims_no_savings() {
    use rift_store::CanonicalStore as _;

    let (repo, db) = recall_repo();
    let (pointer, rt, store) = delivered_first_pointer(repo.path(), &db);
    let rows = rt
        .block_on(store.query(
            "SELECT tokens_returned, tokens_avoided FROM memd_audit_event \
             WHERE verb = 'memd.manifest_injected'",
            &[],
        ))
        .unwrap();
    assert_eq!(rows.len(), 1, "one injection, one spend row");
    assert_eq!(
        rt.block_on(store.count("memd_audit_event")).unwrap(),
        1,
        "and the injection is the only row: it is audited under its own \
         verb, never doubled into the pull side's"
    );
    assert_eq!(
        u32::try_from(rows[0].get_i64(0).unwrap()).unwrap(),
        rift_tokens::count(&pointer),
        "spend is the real tokenizer's count of the injected text"
    );
    assert_eq!(
        rows[0].get_i64(1).unwrap(),
        0,
        "injection claims no savings"
    );
    drop(store);
    drop_recall_repo(&repo, &db);
}

#[test]
fn a_delivered_pointer_leaves_one_hook_channel_receipt() {
    use rift_store::CanonicalStore as _;

    let (repo, db) = recall_repo();
    let (pointer, rt, store) = delivered_first_pointer(repo.path(), &db);
    let calls = rt
        .block_on(store.query(
            "SELECT channel, tool_name, tokens_returned FROM serve_call_event",
            &[],
        ))
        .unwrap();
    assert_eq!(calls.len(), 1, "one delivered pointer, one channel receipt");
    assert_eq!(calls[0].get_text(0).unwrap(), "hook_inject");
    assert_eq!(calls[0].get_text(1).unwrap(), "claude-code:session-pointer");
    assert_eq!(
        u32::try_from(calls[0].get_i64(2).unwrap()).unwrap(),
        rift_tokens::count(&pointer)
    );
    drop(store);
    drop_recall_repo(&repo, &db);
}

/// The first prompt's pointer injected, the next prompt's silence met, and
/// the pointer delivered — then the family's recall store opened over what
/// the delivery booked, with the runtime to read it on.
fn delivered_first_pointer(
    repo: &Path,
    db: &Path,
) -> (String, tokio::runtime::Runtime, rift_store::SqliteStore) {
    let pointer =
        session_pointer_in(repo, &CC, PROMPT, FIRST_PROMPT).expect("the first prompt injects");
    assert_eq!(
        session_pointer_in(repo, &CC, PROMPT, FIRST_PROMPT),
        None,
        "the same session's next prompt is silent"
    );
    let plan =
        rift_recall_serve::ServeDispatcher::plan_context([rift_recall_types::ContextCandidate {
            kind: rift_recall_types::ContextKind::SessionPointer,
            text: pointer.clone(),
            avoided_tokens: 0,
            fallback: false,
        }]);
    record_context_delivery_in(repo, CC.as_str(), &plan, None);
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let store = rift_store::open_migrated(db, &rift_store::recall_migrations()).unwrap();
    (pointer, rt, store)
}

/// The window a subagent gets, through the same chain: the verb that opens
/// it is a boundary, the pointer is what it carries, and the parent's own
/// introduction is untouched by it.
///
/// The last assertion is the one that could not be made before: the two
/// windows share a session id, so a claim keyed by the session alone would
/// make this pair of calls one pointer and one silence — in whichever
/// order they happened to arrive.
#[test]
fn a_subagents_window_is_introduced_at_the_verb_that_opens_it() {
    let (repo, db) = recall_repo();

    let started = serde_json::json!({
        "session_id": "s-sub",
        "hook_event_name": "SubagentStart",
        "agent_id": "a0e79504",
        "agent_type": "general-purpose",
    })
    .to_string();
    let pointer = session_pointer_in(repo.path(), &CC, "SubagentStart", started.as_bytes())
        .expect("the verb that opens a window introduces it");
    assert!(
        pointer.contains("rift recall"),
        "a subagent is handed the same sealed pointer: {pointer}"
    );
    assert_eq!(
        session_pointer_in(repo.path(), &CC, "SubagentStart", started.as_bytes()),
        None,
        "the same window twice is one pointer, and no manifest follows it"
    );
    // A second subagent of the same turn is a second window.
    let sibling = serde_json::json!({
        "session_id": "s-sub",
        "hook_event_name": "SubagentStart",
        "agent_id": "a2f52a50",
        "agent_type": "general-purpose",
    })
    .to_string();
    assert!(
        session_pointer_in(repo.path(), &CC, "SubagentStart", sibling.as_bytes()).is_some(),
        "a sibling window is introduced too"
    );
    // …and the parent has not been introduced by any of it.
    assert!(
        session_pointer_in(repo.path(), &CC, PROMPT, br#"{"session_id":"s-sub"}"#).is_some(),
        "the main thread's own introduction is still owed"
    );

    drop_recall_repo(&repo, &db);
}
