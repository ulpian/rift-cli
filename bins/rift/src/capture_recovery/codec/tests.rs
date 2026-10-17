use super::*;

fn input() -> (String, Value) {
    let value = serde_json::json!({
        "schema_version":1,"work_tree":"/fixture","queued_at_ms":1000,"attempts":2,
        "kind":{"kind":"boundary-snapshot","session_id":"fixture","agent":"claude-code","ordinal":1,"boundary":"start","head_oid":null,"captured_at_ms":1000}
    });
    let job: crate::agent_queue::AgentJob = serde_json::from_value(value.clone()).unwrap();
    let stem = crate::agent_queue::logical_stem(&job).unwrap();
    let name = format!(
        "{stem}--{}.json",
        crate::capture_queue::delivery::context_id(&value).unwrap()
    );
    (name, value)
}

#[test]
fn full_unknown_fields_and_attempts_are_retained_by_the_existing_unique_codec() {
    let (_, mut value) = input();
    value.as_object_mut().unwrap().insert(
        "future_context".into(),
        serde_json::json!({"nested":[1,"two"]}),
    );
    let job: crate::agent_queue::AgentJob = serde_json::from_value(value.clone()).unwrap();
    let name = format!(
        "{}--{}.json",
        crate::agent_queue::logical_stem(&job).unwrap(),
        crate::capture_queue::delivery::context_id(&value).unwrap()
    );
    let decoded = decode(OsStr::new(&name), &serde_json::to_vec(&value).unwrap()).unwrap();
    assert_eq!(decoded.full, value);
    assert_eq!(decoded.attempts, 2);
    assert!(decoded.context.get("attempts").is_none());
    assert_eq!(
        decoded.context.get("future_context"),
        value.get("future_context")
    );
}

#[test]
fn duplicate_unknown_keys_and_unsafe_names_refuse_without_echoing_them() {
    use std::error::Error as _;
    let (name, value) = input();
    let bytes = serde_json::to_string(&value).unwrap();
    let duplicate = bytes.replacen(
        '{',
        "{\"private_rejected_field\":1,\"private_rejected_field\":2,",
        1,
    );
    let error = decode(OsStr::new(&name), duplicate.as_bytes())
        .err()
        .unwrap();
    assert_eq!(format!("{error:?}"), "PinError");
    assert!(!error.to_string().contains("private_rejected_field"));
    assert!(error.source().is_none());
    for name in ["../escape.json", ".", "/absolute.json", "wrong.json"] {
        assert!(decode(OsStr::new(name), bytes.as_bytes()).is_err());
    }
}

#[test]
fn basenames_are_bounded_ascii_without_nul_or_path_components() {
    for length in [1, 255, 256] {
        assert!(basename(OsStr::new(&"a".repeat(length))).is_ok());
    }
    for name in [
        String::new(),
        "a".repeat(257),
        "nul\0suffix".into(),
        "é".into(),
        ".".into(),
        "..".into(),
        "a/b".into(),
        "a/".into(),
        "/a".into(),
    ] {
        assert!(basename(OsStr::new(&name)).is_err());
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;
        assert!(basename(OsStr::from_bytes(&[0xff])).is_err());
    }
}
