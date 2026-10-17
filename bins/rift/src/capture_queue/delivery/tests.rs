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

#[test]
fn identical_context_redelivery_preserves_the_owned_attempt_counter() {
    let (_temp, owner, dir) = owned_directory();
    let first = publish(&dir, "ticket", &payload(0)).unwrap();
    replace_attempts(&first, "ticket", &payload(0), 1, &owner).unwrap();
    assert_eq!(publish(&dir, "ticket", &payload(0)).unwrap(), first);
    assert_eq!(decode(&std::fs::read(first).unwrap()).unwrap(), payload(1));
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
    assert!(replace_attempts(&path, "ticket", &payload(1), 2, &owner).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), original);
    replace_attempts(&path, "ticket", &payload(0), 1, &owner).unwrap();
    assert_eq!(decode(&std::fs::read(path).unwrap()).unwrap(), payload(1));
}
