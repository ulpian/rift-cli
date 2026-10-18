use super::*;

fn owned_directory() -> (tempfile::TempDir, crate::capture_queue::QueueLock, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let owner = crate::capture_queue::acquire_owner(temp.path()).unwrap();
    let dir = crate::capture_queue::queue_dir(temp.path());
    (temp, owner, dir)
}

fn payload(attempts: u32) -> Value {
    serde_json::json!({"schema_version":1,"queued_at_ms":1000,"attempts":attempts,
        "future":{"nested":[{"retained":true}]}})
}

/// `payload(attempts)` with the two sibling retry members set.
fn retried(attempts: u32, next_due_ms: i64, parked: bool) -> Value {
    let mut value = payload(attempts);
    value["next_due_ms"] = next_due_ms.into();
    value["parked"] = parked.into();
    value
}

fn state(attempts: u32, next_due_ms: i64, parked: bool) -> RetryState {
    RetryState {
        attempts,
        next_due_ms,
        parked,
    }
}

#[test]
fn identical_context_redelivery_preserves_the_owned_attempt_counter() {
    let (_temp, owner, dir) = owned_directory();
    let first = publish(&dir, "ticket", &payload(0)).unwrap();
    replace_retry_state(&first, "ticket", &payload(0), state(1, 0, false), &owner).unwrap();
    assert_eq!(publish(&dir, "ticket", &payload(0)).unwrap(), first);
    assert_eq!(
        decode(&std::fs::read(first).unwrap()).unwrap(),
        retried(1, 0, false)
    );
}

/// The context SHA — and so the file name — is identical before and after a
/// retry-state write: all three retry members are outside the context, so a
/// backoff or a park never renames the delivery (L-0A §4.1 as amended).
#[test]
fn a_retry_state_write_keeps_the_context_id_and_the_file_name() {
    let (_temp, owner, dir) = owned_directory();
    let path = publish(&dir, "ticket", &payload(0)).unwrap();
    let before = context_id(&decode(&std::fs::read(&path).unwrap()).unwrap()).unwrap();
    replace_retry_state(
        &path,
        "ticket",
        &payload(0),
        state(3, 1_790_000_030_000, true),
        &owner,
    )
    .unwrap();
    let after_value = decode(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(context_id(&after_value).unwrap(), before);
    assert!(path.exists(), "the same file, rewritten in place");
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
    assert_eq!(
        retry_state(&after_value).unwrap(),
        state(3, 1_790_000_030_000, true)
    );
    assert_eq!(
        context(&after_value).unwrap(),
        context(&payload(0)).unwrap()
    );
}

/// The agreed shape (S9, 2026-10-17): exactly two top-level members beside
/// `attempts`, `next_due_ms` and `parked`, and no nested object — the outer
/// key set of a parked delivery is the context's keys plus those three.
#[test]
fn a_parked_delivery_adds_exactly_the_two_sibling_members() {
    let (_temp, owner, dir) = owned_directory();
    let path = publish(&dir, "ticket", &payload(0)).unwrap();
    replace_retry_state(&path, "ticket", &payload(0), state(3, 7, true), &owner).unwrap();
    let value = decode(&std::fs::read(&path).unwrap()).unwrap();
    let mut keys: Vec<&str> = value
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        vec![
            "attempts",
            "future",
            "next_due_ms",
            "parked",
            "queued_at_ms",
            "schema_version"
        ]
    );
}

/// A legacy delivery without the two members reads as due now and unparked;
/// a delivery carrying them keeps both through a read-modify-write of its
/// retry state and through a redelivery of the same context.
#[test]
fn legacy_deliveries_read_as_due_and_retry_members_survive_a_rewrite() {
    let (_temp, owner, dir) = owned_directory();
    assert_eq!(retry_state(&payload(2)).unwrap(), state(2, 0, false));
    assert!(state(2, 0, false).is_due(0));
    let path = publish(&dir, "ticket", &retried(1, 500, false)).unwrap();
    assert_eq!(publish(&dir, "ticket", &payload(0)).unwrap(), path);
    let kept = decode(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(retry_state(&kept).unwrap(), state(1, 500, false));
    replace_retry_state(&path, "ticket", &kept, state(2, 900, false), &owner).unwrap();
    let rewritten = decode(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(rewritten, retried(2, 900, false));
    assert!(!state(2, 900, false).is_due(899));
    assert!(state(2, 900, false).is_due(900));
    assert!(!state(2, 0, true).is_due(i64::MAX), "parked is never due");
}

/// The backoff as data (§4.1): 30 s, 60 s, 120 s, … capped at an hour, and
/// the third failure parks.
#[test]
fn a_failure_doubles_the_backoff_from_thirty_seconds_and_the_third_parks() {
    let now = 1_790_000_000_000;
    let first = RetryState::default().after_failure(now);
    assert_eq!(first, state(1, now + 30_000, false));
    let second = first.after_failure(now);
    assert_eq!(second, state(2, now + 60_000, false));
    let third = second.after_failure(now);
    assert_eq!(third, state(3, now + 120_000, true));
    let mut many = third;
    for _ in 0..20 {
        many = many.after_failure(now);
    }
    assert_eq!(many.next_due_ms, now + 3_600_000, "capped at an hour");
    assert!(many.parked);
    assert_eq!(third.unparked(), state(3, 0, false), "attempts are kept");
}

#[test]
fn migration_refuses_different_attempts_instead_of_discarding_source_debt() {
    let (_temp, _owner, dir) = owned_directory();
    let first = publish_exact(&dir, "ticket", &payload(1)).unwrap();
    assert!(publish_exact(&dir, "ticket", &payload(0)).is_err());
    assert_eq!(decode(&std::fs::read(first).unwrap()).unwrap(), payload(1));
}

#[test]
fn concurrent_identical_producers_publish_one_complete_inode_and_leave_no_stages() {
    let (_temp, _owner, dir) = owned_directory();
    let paths = std::thread::scope(|scope| {
        let writers: Vec<_> = (0..8)
            .map(|_| scope.spawn(|| publish(&dir, "ticket", &payload(0)).unwrap()))
            .collect();
        writers
            .into_iter()
            .map(|writer| writer.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert!(paths.windows(2).all(|pair| pair[0] == pair[1]));
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
    assert_eq!(
        decode(&std::fs::read(&paths[0]).unwrap()).unwrap(),
        payload(0)
    );
}

#[test]
fn duplicate_unknown_keys_at_any_depth_and_unsupported_schemas_are_refused() {
    for bytes in [
        br#"{"schema_version":1,"future":{"a":1,"a":2}}"#.as_slice(),
        br#"{"schema_version":1,"future":[{"a":1,"a":2}]}"#.as_slice(),
        br#"{"schema_version":1,"schema_version":1}"#.as_slice(),
        br#"{"schema_version":2}"#.as_slice(),
        br#"{"schema_version":1,"attempts":-1}"#.as_slice(),
        br#"{"schema_version":1,"next_due_ms":"soon"}"#.as_slice(),
        br#"{"schema_version":1,"parked":1}"#.as_slice(),
    ] {
        assert!(decode(bytes).is_err());
    }
    assert!(decode(br#"{"schema_version":1,"future":null}"#).is_ok());
}

#[test]
fn context_changes_have_distinct_paths_and_old_completion_cannot_erase_new_debt() {
    let (_temp, _owner, dir) = owned_directory();
    let first = publish(&dir, "ticket", &payload(0)).unwrap();
    let mut changed = payload(0);
    changed["queued_at_ms"] = 2000.into();
    let second = publish(&dir, "ticket", &changed).unwrap();
    assert_ne!(first, second);
    std::fs::remove_file(first).unwrap();
    assert_eq!(decode(&std::fs::read(second).unwrap()).unwrap(), changed);
}

#[test]
fn rejected_retry_keeps_the_complete_previous_payload_and_unknown_fields() {
    let (_temp, owner, dir) = owned_directory();
    let path = publish(&dir, "ticket", &payload(0)).unwrap();
    let original = std::fs::read(&path).unwrap();
    assert!(replace_retry_state(&path, "ticket", &payload(1), state(2, 0, false), &owner).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), original);
    replace_retry_state(&path, "ticket", &payload(0), state(1, 0, false), &owner).unwrap();
    assert_eq!(
        decode(&std::fs::read(path).unwrap()).unwrap(),
        retried(1, 0, false)
    );
}
