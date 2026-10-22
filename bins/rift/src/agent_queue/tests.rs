use super::*;

const CALL: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn condense(session: &str, at: i64) -> AgentJob {
    AgentJob {
        schema_version: 1,
        work_tree: PathBuf::from("/w"),
        queued_at_ms: at,
        attempts: 0,
        kind: JobKind::Condense {
            session_id: session.to_owned(),
            agent: "claude-code".into(),
            repo_id: "repo".into(),
        },
    }
}

fn tokens(call: &str, at: i64) -> AgentJob {
    AgentJob {
        schema_version: 1,
        work_tree: PathBuf::from("/w"),
        queued_at_ms: at,
        attempts: 0,
        kind: JobKind::ReadTokens {
            tool_call_id: call.to_owned(),
            rel_path: "a.txt".into(),
            content_sha256: "ff".repeat(32),
            blob_oid: "ee".repeat(20),
            lines: None,
            schema_version: 1,
        },
    }
}

#[test]
fn atomicity_an_old_agent_completion_does_not_remove_a_new_delivery() {
    let tmp = tempfile::tempdir().unwrap();
    let owner = crate::capture_queue::acquire_lock(tmp.path(), 1_000).unwrap();
    let first = condense("session", 1_000);
    enqueue(tmp.path(), &first).unwrap();
    let observed = jobs(tmp.path());
    assert_eq!(observed.len(), 1);
    assert_eq!(observed[0].1, first);
    let mut second = condense("session", 2_000);
    second.work_tree = PathBuf::from("/replacement-worktree");
    let published = enqueue(tmp.path(), &second).unwrap();
    assert!(jobs(tmp.path()).contains(&(published.clone(), second.clone())));
    eprintln!("agent old/new share path: {}", observed[0].0 == published);
    crate::capture_queue::remove_owned(&observed[0].0, &owner).unwrap();
    assert!(
        jobs(tmp.path()).contains(&(published, second)),
        "completing observed A must leave successfully published B owed"
    );
}

/// The same work enqueued twice is one file, and a session id — agent
/// text — never becomes a path component itself.
#[test]
fn exact_redelivery_coalesces_and_changed_context_remains_separate() {
    let tmp = tempfile::tempdir().unwrap();
    let a = enqueue(tmp.path(), &condense("../evil session", 1)).unwrap();
    let b = enqueue(tmp.path(), &condense("../evil session", 2)).unwrap();
    assert_ne!(a, b);
    assert_eq!(
        a,
        enqueue(tmp.path(), &condense("../evil session", 1)).unwrap()
    );
    assert!(a
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .starts_with("condense-"));
    assert!(!a.to_string_lossy().contains("evil"));
    let listed = jobs(tmp.path());
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0].1, condense("../evil session", 1));
    assert_eq!(listed[1].1, condense("../evil session", 2));
}

/// Token counts drain before the condense that would freeze them into a
/// shard, whatever order they were queued in.
#[test]
fn token_jobs_list_before_condense_jobs() {
    let tmp = tempfile::tempdir().unwrap();
    enqueue(tmp.path(), &condense("s", 1)).unwrap();
    enqueue(tmp.path(), &tokens(CALL, 5)).unwrap();
    let kinds: Vec<u8> = jobs(tmp.path())
        .iter()
        .map(|(_, j)| j.kind.rank())
        .collect();
    assert_eq!(kinds, vec![0, 1]);
    assert_eq!(job_file_count(tmp.path()), 2);
}

/// A token job whose call id is not 64 lowercase hex is refused, never
/// written under a name it did not earn.
#[test]
fn a_token_job_needs_a_hex_call_id() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(enqueue(tmp.path(), &tokens("../x", 1)).is_err());
    assert!(enqueue(tmp.path(), &tokens(&CALL.to_uppercase(), 1)).is_err());
    assert!(jobs(tmp.path()).is_empty());
}

/// A file whose name disagrees with its content is skipped in place and
/// counted — the two halves of the doctor's report.
#[test]
fn a_renamed_job_is_skipped_but_counted() {
    let tmp = tempfile::tempdir().unwrap();
    let path = enqueue(tmp.path(), &condense("s", 1)).unwrap();
    let moved = path.with_file_name(format!("tokens-{CALL}.json"));
    std::fs::rename(&path, &moved).unwrap();
    assert!(jobs(tmp.path()).is_empty());
    assert_eq!(job_file_count(tmp.path()), 1);
    assert!(moved.exists(), "left for the doctor, not deleted");
}

/// Re-pinned when parking replaced removal. Failure is loud, bounded and
/// idempotent: the note lands on the shared said-once channel with the
/// retry texts, the count and the backoff are durable, and the third failure
/// **parks** the job — both halves proven: the file is still present after
/// the third failure AND `parked == true` with `next_due_ms` set. Past the
/// park the note is rewritten only for a new reason.
#[test]
fn a_failing_job_is_said_once_and_bounded() {
    let tmp = tempfile::tempdir().unwrap();
    let owner = crate::capture_queue::acquire_owner(tmp.path()).unwrap();
    let now = 1_790_000_000_000;
    let mut job = condense("s", 1);
    let path = enqueue(tmp.path(), &job).unwrap();
    for attempt in 1..crate::capture_queue::PARK_AFTER {
        let state = record_failure_owned(&path, &job, "no head", &owner, now).unwrap();
        assert_eq!((state.attempts, state.parked), (attempt, false));
        let said = crate::capture_queue::take_errors(tmp.path());
        assert_eq!(said.len(), 1);
        assert!(said[0].starts_with("no head; retried after "), "{said:?}");
        job = read_job(&path).unwrap().1;
        assert_eq!(job.attempts, attempt);
        assert!(
            jobs_due(tmp.path(), now).is_empty(),
            "not due before the backoff"
        );
        assert_eq!(jobs_due(tmp.path(), state.next_due_ms).len(), 1);
    }
    let parked = record_failure_owned(&path, &job, "no head", &owner, now).unwrap();
    assert!(parked.parked && parked.attempts == crate::capture_queue::PARK_AFTER);
    assert_eq!(parked.next_due_ms, now + 120_000);
    assert!(path.exists(), "parked, never removed");
    assert_eq!(jobs(tmp.path()).len(), 1, "a repair road still lists it");
    assert!(
        jobs_due(tmp.path(), i64::MAX).is_empty(),
        "the ordinary pass skips it"
    );
    assert_eq!(retry_states(tmp.path())[0].2, parked);
    assert_eq!(
        crate::capture_queue::take_errors(tmp.path()),
        vec![
            "no head — parked after 3 attempts; `rift doctor --fix` or \
             `rift session retry --session s` retries it"
        ]
    );
    // Past the park: the same reason says nothing new; a new reason does.
    job = read_job(&path).unwrap().1;
    record_failure_owned(&path, &job, "no head", &owner, now).unwrap();
    assert!(crate::capture_queue::take_errors(tmp.path()).is_empty());
    job = read_job(&path).unwrap().1;
    record_failure_owned(&path, &job, "disk full", &owner, now).unwrap();
    assert_eq!(
        crate::capture_queue::take_errors(tmp.path()),
        vec!["disk full"]
    );
    // The retry verb's write: due now, unparked, the history kept.
    job = read_job(&path).unwrap().1;
    unpark_owned(&path, &job, &owner).unwrap();
    let (_, job, state) = retry_states(tmp.path()).remove(0);
    assert_eq!(
        (state.attempts, state.next_due_ms, state.parked),
        (5, 0, false)
    );
    assert_eq!(job.attempts, 5);
    assert_eq!(jobs_due(tmp.path(), 0).len(), 1);
}

fn audit(call_id: &str, at: i64) -> AgentJob {
    AgentJob {
        schema_version: 1,
        work_tree: PathBuf::from("/w"),
        queued_at_ms: at,
        attempts: 0,
        kind: JobKind::ServeAudit(ServeAudit {
            call_id: call_id.to_owned(),
            decision: ServeAuditKind::ReadIntercepted,
            agent: "claude-code".into(),
            basis: None,
            frame: "a reason".into(),
            tag: None,
            total: None,
            declared_avoided: None,
            shape: None,
            session: None,
            window: None,
        }),
    }
}

/// Each new kind is journaled under an identity of its own making: the
/// decision's id, the document's content hash, or the single name a
/// pending index rebuild has. Nothing agent-supplied becomes a path
/// component, and an identity that is not path-safe is refused rather
/// than written under a name it did not earn.
#[test]
fn the_new_kinds_are_journaled_under_identities_they_earned() {
    let tmp = tempfile::tempdir().unwrap();
    let call = "0123456789abcdef0123456789abcdef";
    let a = enqueue(tmp.path(), &audit(call, 1)).unwrap();
    let b = enqueue(tmp.path(), &audit(call, 2)).unwrap();
    assert_ne!(a, b, "changed queue times retain separate deliveries");
    assert!(a
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .starts_with(&format!("audit-{call}--")));
    assert!(enqueue(tmp.path(), &audit("../x", 1)).is_err());
    assert!(enqueue(tmp.path(), &audit(&call.to_uppercase(), 1)).is_err());

    let index = AgentJob {
        kind: JobKind::SearchIndex,
        ..audit(call, 3)
    };
    assert!(enqueue(tmp.path(), &index)
        .unwrap()
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .starts_with("search-index--"));
    let doc = |sha: &str| AgentJob {
        kind: JobKind::DocConvert {
            rel_path: "docs/spec.docx".into(),
            content_sha256: sha.to_owned(),
        },
        ..audit(call, 4)
    };
    let sha = "ab".repeat(32);
    assert!(enqueue(tmp.path(), &doc(&sha))
        .unwrap()
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .starts_with(&format!("doc-{sha}--")));
    assert!(enqueue(tmp.path(), &doc("../evil")).is_err());
    let sweep = AgentJob {
        kind: JobKind::DocSweep,
        ..audit(call, 5)
    };
    assert!(enqueue(tmp.path(), &sweep)
        .unwrap()
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .starts_with("doc-sweep--"));
    assert_eq!(
        enqueue(tmp.path(), &sweep).unwrap(),
        enqueue(tmp.path(), &sweep).unwrap(),
        "three hooks asking for a pass have asked for one pass"
    );
    assert_eq!(
        jobs(tmp.path()).len(),
        5,
        "one file per complete context, nothing discarded"
    );
}

/// An index rebuild or run-state move is one pending file however many
/// callers ask, each at its own instant; one for another work tree is a
/// second delivery, and a kind that is not such a request keeps the
/// delivery a changed instant makes.
#[test]
fn a_repeatable_request_asked_at_three_instants_is_one_pending_delivery() {
    let tmp = tempfile::tempdir().unwrap();
    let ask = |kind: JobKind, at: i64| AgentJob {
        kind,
        ..condense("s", at)
    };
    for kind in [JobKind::SearchIndex, JobKind::MoveRunState] {
        let first = enqueue(tmp.path(), &ask(kind.clone(), 1)).unwrap();
        for at in [2, 3] {
            assert_eq!(enqueue(tmp.path(), &ask(kind.clone(), at)).unwrap(), first);
        }
        let mut elsewhere = ask(kind, 4);
        elsewhere.work_tree = PathBuf::from("/other");
        assert_ne!(enqueue(tmp.path(), &elsewhere).unwrap(), first);
    }
    let pass = enqueue(tmp.path(), &ask(JobKind::DocSweep, 5)).unwrap();
    assert_ne!(
        enqueue(tmp.path(), &ask(JobKind::DocSweep, 6)).unwrap(),
        pass
    );
    let listed: Vec<i64> = jobs(tmp.path())
        .iter()
        .map(|(_, j)| j.queued_at_ms)
        .collect();
    assert_eq!(
        listed,
        [1, 1, 4, 4, 5, 6],
        "each kept delivery keeps its first clock"
    );
}

/// Everything that is not a condense drains before the condense that
/// would freeze a session's shard, and a decision journaled first is
/// written first.
#[test]
fn accounting_and_index_work_drain_before_a_condense() {
    let tmp = tempfile::tempdir().unwrap();
    enqueue(tmp.path(), &condense("s", 1)).unwrap();
    enqueue(tmp.path(), &audit("0123456789abcdef0123456789abcdef", 5)).unwrap();
    enqueue(
        tmp.path(),
        &AgentJob {
            kind: JobKind::SearchIndex,
            ..condense("s", 2)
        },
    )
    .unwrap();
    let ranks: Vec<u8> = jobs(tmp.path())
        .iter()
        .map(|(_, j)| j.kind.rank())
        .collect();
    assert_eq!(ranks, vec![0, 0, 1]);
}

/// A sweep request round-trips through the journal with its floor, drains
/// after the condenses, and has one name however often it is asked for,
/// so a failure note about it is one note.
#[test]
fn a_sweep_request_round_trips_drains_last_and_has_one_name() {
    let tmp = tempfile::tempdir().unwrap();
    let sweep = |at: i64| AgentJob {
        kind: JobKind::SweepIdle { floor_ms: 300_000 },
        ..condense("s", at)
    };
    let a = enqueue(tmp.path(), &sweep(3)).unwrap();
    let b = enqueue(tmp.path(), &sweep(9)).unwrap();
    assert_ne!(a, b, "changed queue time is separate context");
    assert_eq!(a, enqueue(tmp.path(), &sweep(3)).unwrap());
    enqueue(tmp.path(), &condense("s", 1)).unwrap();
    let drained = jobs(tmp.path());
    assert_eq!(drained.len(), 3);
    assert_eq!(drained[1].1, sweep(3));
    assert_eq!(
        drained[2].1,
        sweep(9),
        "both sweeps drain last with their floor intact"
    );
    assert_eq!(note_key(&sweep(3)), note_key(&sweep(4)));
    assert_eq!(note_key(&sweep(3)), "agent-sweep-idle");
}

/// An interval archive has one name per session, so every hook inside one
/// due interval owes the same job; it is found pending by that name, drains
/// with the condenses after the token counts, and keeps the reason the
/// audit row spells.
#[test]
fn an_interval_archive_is_one_job_per_session_ranked_with_the_condenses() {
    let tmp = tempfile::tempdir().unwrap();
    let interval = |session: &str, at: i64, start: Option<i64>| AgentJob {
        kind: JobKind::Archive(IntervalArchive {
            session_id: session.to_owned(),
            agent: "claude-code".into(),
            reason: IntervalReason::SensitivePath,
            interval_start_ms: start,
        }),
        ..condense(session, at)
    };
    let archive = |session: &str, at: i64| interval(session, at, None);
    assert!(jobs(tmp.path()).is_empty());
    let a = enqueue(tmp.path(), &archive("s", 1)).unwrap();
    assert_eq!(
        a,
        enqueue(tmp.path(), &archive("s", 2)).unwrap(),
        "a second due hook in one interval asks for the pending archive"
    );
    assert_ne!(
        a,
        enqueue(tmp.path(), &interval("s", 2, Some(1))).unwrap(),
        "a due hook in the next interval owes a second"
    );
    assert_eq!(jobs(tmp.path()).len(), 2);
    assert_eq!(jobs(tmp.path())[0].1, archive("s", 1), "on the first clock");
    assert!(jobs(tmp.path()).iter().all(|(_, job)| matches!(&job.kind,
        JobKind::Archive(archive) if archive.session_id == "s")));
    assert_eq!(note_key(&archive("s", 1)), note_key(&archive("s", 9)));
    assert_ne!(note_key(&archive("s", 1)), note_key(&archive("t", 1)));
    enqueue(tmp.path(), &tokens(CALL, 5)).unwrap();
    let ranks: Vec<u8> = jobs(tmp.path())
        .iter()
        .map(|(_, j)| j.kind.rank())
        .collect();
    assert_eq!(ranks, vec![0, 1, 1]);
    let body = std::fs::read_to_string(&a).unwrap();
    assert!(body.contains(r#""reason":"sensitive-path""#), "{body}");
}

/// A boundary snapshot is one job per (session, turn, edge), and one
/// session's boundaries list in turn order, a start before its end, even
/// when two of them carry the same instant.
#[test]
fn boundary_snapshots_are_one_job_per_edge_and_list_in_turn_order() {
    let tmp = tempfile::tempdir().unwrap();
    let boundary = |ordinal: u32, edge: BoundaryEdge, at: i64| AgentJob {
        kind: JobKind::BoundarySnapshot(BoundarySnapshot {
            session_id: "../s".into(),
            agent: "claude-code".into(),
            ordinal,
            boundary: edge,
            head_oid: None,
            captured_at_ms: at,
        }),
        ..condense("s", at)
    };
    enqueue(tmp.path(), &boundary(10, BoundaryEdge::Start, 7)).unwrap();
    enqueue(tmp.path(), &boundary(9, BoundaryEdge::End, 7)).unwrap();
    let first = enqueue(tmp.path(), &boundary(9, BoundaryEdge::Start, 7)).unwrap();
    assert_ne!(
        first,
        enqueue(tmp.path(), &boundary(9, BoundaryEdge::Start, 8)).unwrap()
    );
    assert_eq!(
        first,
        enqueue(tmp.path(), &boundary(9, BoundaryEdge::Start, 7)).unwrap()
    );
    assert!(!first.to_string_lossy().contains("../s"));
    let listed: Vec<AgentJob> = jobs(tmp.path()).into_iter().map(|(_, j)| j).collect();
    assert_eq!(
        listed,
        vec![
            boundary(9, BoundaryEdge::Start, 7),
            boundary(9, BoundaryEdge::End, 7),
            boundary(10, BoundaryEdge::Start, 7),
            boundary(9, BoundaryEdge::Start, 8),
        ],
        "an instant orders first; a tie falls to turn and edge"
    );
}

/// The done-marker is what makes a redelivered or re-found condense a
/// no-op.
#[test]
fn the_done_marker_is_per_session() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(!is_condensed(tmp.path(), "a"));
    mark_condensed(tmp.path(), "a");
    assert!(is_condensed(tmp.path(), "a"));
    assert!(!is_condensed(tmp.path(), "b"));
    clear_condensed(tmp.path(), "a");
    assert!(
        !is_condensed(tmp.path(), "a"),
        "a new end owes a new condense"
    );
}

/// A serve's attribution is journaled under its call id, round-trips, and
/// drains before the checkpoint that freezes it; a job of a kind this
/// reader does not know is skipped and left for the binary that does.
#[test]
fn an_attribution_job_round_trips_and_an_unknown_kind_is_left_in_place() {
    let tmp = tempfile::tempdir().unwrap();
    let claim = JobKind::AttributeServe(ServeClaim {
        session: "s".to_owned(),
        window: Some("a".repeat(64)),
        call_id: "0".repeat(32),
        audit_id: "b".repeat(64),
    });
    let job = AgentJob {
        kind: claim,
        ..condense("s", 2)
    };
    let path = enqueue(tmp.path(), &job).unwrap();
    assert!(path
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .starts_with(&format!("attribute-{}--", "0".repeat(32))));
    enqueue(tmp.path(), &condense("s", 1)).unwrap();
    let drained: Vec<AgentJob> = jobs(tmp.path()).into_iter().map(|(_, j)| j).collect();
    assert_eq!(drained.first(), Some(&job), "attribution drains first");

    let unknown = queue_dir(tmp.path()).join("from-the-future.json");
    std::fs::write(
        &unknown,
        r#"{"schema_version":1,"work_tree":"/w","queued_at_ms":1,"kind":{"kind":"from-the-future"}}"#,
    )
    .unwrap();
    assert_eq!(jobs(tmp.path()).len(), 2);
    assert!(unknown.exists());
    let spoofed = JobKind::AttributeServe(ServeClaim {
        session: "s".to_owned(),
        window: None,
        call_id: "../x".to_owned(),
        audit_id: "b".repeat(64),
    });
    assert_eq!(spoofed.stem(), None);
}

/// Unknown fields are ignored on read: a newer writer's job still drains.
#[test]
fn unknown_fields_survive_legacy_read_and_owned_retry_and_context_tampering_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let owner = crate::capture_queue::acquire_owner(tmp.path()).unwrap();
    let job = condense("s", 1);
    let mut value = serde_json::to_value(&job).unwrap();
    value["future"] = serde_json::json!({"nested":[true,42]});
    let dir = queue_dir(tmp.path());
    std::fs::create_dir_all(&dir).unwrap();
    let stem = logical_stem(&job).unwrap();
    let legacy = dir.join(format!("{stem}.json"));
    std::fs::write(&legacy, value.to_string()).unwrap();
    assert_eq!(
        jobs(tmp.path()).len(),
        1,
        "plain legacy unknown fields remain readable"
    );
    std::fs::remove_file(legacy).unwrap();
    let path = crate::capture_queue::delivery::publish(&dir, &stem, &value).unwrap();
    let state = record_failure_owned(&path, &job, "test execution failure", &owner, 1).unwrap();
    assert!(!state.parked && state.attempts == 1);
    let (retained, retried) = read_job(&path).unwrap();
    assert_eq!(retained["future"], value["future"]);
    assert_eq!(retried.attempts, 1);
    let mut tampered = retained;
    tampered["future"] = false.into();
    std::fs::write(&path, tampered.to_string()).unwrap();
    assert!(
        jobs(tmp.path()).is_empty(),
        "in-place context mutation is not a valid producer"
    );
    assert!(path.exists());
}
