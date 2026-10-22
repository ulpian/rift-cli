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

/// The real enqueue serialization of one boundary ticket, as the journal
/// writes it: the vectors below are mutations of this, never a hand-written schema.
fn enqueued(tree: &str) -> Value {
    let job = crate::agent_queue::AgentJob {
        schema_version: 1,
        work_tree: tree.into(),
        queued_at_ms: 1000,
        attempts: 2,
        kind: crate::agent_queue::JobKind::BoundarySnapshot(crate::agent_queue::BoundarySnapshot {
            session_id: "native-1".into(),
            agent: "claude-code".into(),
            ordinal: 3,
            boundary: crate::agent_queue::BoundaryEdge::End,
            head_oid: None,
            captured_at_ms: 900,
        }),
    };
    serde_json::to_value(&job).unwrap()
}

fn gate(value: &Value, tree: &str) -> PinResult<Shape> {
    let job: crate::agent_queue::AgentJob = serde_json::from_value(value.clone()).unwrap();
    let stem = crate::agent_queue::logical_stem(&job).unwrap();
    let name = format!(
        "{stem}--{}.json",
        crate::capture_queue::delivery::context_id(value).unwrap()
    );
    let decoded = decode(OsStr::new(&name), &serde_json::to_vec(value).unwrap())?;
    recovery_gate(&decoded, Path::new(tree))
}

#[test]
fn the_recovery_gate_admits_the_real_enqueue_shape_and_the_retry_members_only() {
    let value = enqueued("/fixture");
    let Shape::Boundary(facts) = gate(&value, "/fixture").unwrap() else {
        panic!("a boundary ticket");
    };
    assert_eq!(facts.attempts, 2);
    assert_eq!(facts.queued_at_ms, 1000);
    assert_eq!(facts.snapshot.ordinal, 3);
    assert_eq!(facts.snapshot.session_id, "native-1");
    let mut tolerated = value.clone();
    let outer = tolerated.as_object_mut().unwrap();
    outer.insert("next_due_ms".into(), serde_json::json!(5000));
    outer.insert("parked".into(), serde_json::json!(true));
    assert!(matches!(
        gate(&tolerated, "/fixture"),
        Ok(Shape::Boundary(_))
    ));
    let mut without_attempts = value.clone();
    without_attempts.as_object_mut().unwrap().remove("attempts");
    let Shape::Boundary(facts) = gate(&without_attempts, "/fixture").unwrap() else {
        panic!("absent attempts is zero, semantically");
    };
    assert_eq!(facts.attempts, 0);
    let mut head = value.clone();
    head["kind"]["head_oid"] = serde_json::json!("0123456789abcdef0123456789abcdef01234567");
    assert!(matches!(gate(&head, "/fixture"), Ok(Shape::Boundary(_))));
}

#[test]
fn the_recovery_gate_refuses_every_unknown_field_tree_time_id_and_head_shape() {
    let value = enqueued("/fixture");
    let mut cases: Vec<(&str, Value)> = Vec::new();
    let mut outer_unknown = value.clone();
    outer_unknown["future"] = serde_json::json!(1);
    cases.push(("outer unknown", outer_unknown));
    let mut nested_unknown = value.clone();
    nested_unknown["kind"]["note"] = serde_json::json!("x");
    cases.push(("nested unknown", nested_unknown));
    let mut nested_attempts = value.clone();
    nested_attempts["kind"]["attempts"] = serde_json::json!(1);
    cases.push(("nested attempts", nested_attempts));
    let mut due = value.clone();
    due["next_due_ms"] = serde_json::json!("soon");
    cases.push(("next_due_ms not an integer", due));
    let mut negative_due = value.clone();
    negative_due["next_due_ms"] = serde_json::json!(-1);
    cases.push(("next_due_ms negative", negative_due));
    let mut parked = value.clone();
    parked["parked"] = serde_json::json!(1);
    cases.push(("parked not a bool", parked));
    let mut other_tree = value.clone();
    other_tree["work_tree"] = serde_json::json!("/elsewhere");
    cases.push(("another tree", other_tree));
    let mut dirty_tree = value.clone();
    dirty_tree["work_tree"] = serde_json::json!("/fixture/../fixture");
    cases.push(("unclean tree", dirty_tree));
    let mut negative_queued = value.clone();
    negative_queued["queued_at_ms"] = serde_json::json!(-5);
    cases.push(("negative queued", negative_queued));
    let mut negative_captured = value.clone();
    negative_captured["kind"]["captured_at_ms"] = serde_json::json!(-5);
    cases.push(("negative captured", negative_captured));
    let mut unsafe_native = value.clone();
    unsafe_native["kind"]["session_id"] = serde_json::json!("../escape");
    cases.push(("unsafe native", unsafe_native));
    let mut short_head = value.clone();
    short_head["kind"]["head_oid"] = serde_json::json!("abc");
    cases.push(("short head", short_head));
    let mut upper_head = value.clone();
    upper_head["kind"]["head_oid"] = serde_json::json!("0123456789ABCDEF0123456789ABCDEF01234567");
    cases.push(("uppercase head", upper_head));
    for (label, case) in cases {
        assert!(gate(&case, "/fixture").is_err(), "{label}");
    }
    let mut other_kind = value;
    other_kind["kind"] = serde_json::json!({"kind":"doc-sweep"});
    assert!(matches!(gate(&other_kind, "/fixture"), Ok(Shape::Other)));
}

/// The context digest is the queue's full value minus ONLY the three top-level
/// retry members (`attempts`, `next_due_ms`, `parked`): nested attempts and a
/// retained unknown field change it, whitespace does not; the ticket digest is
/// the raw bytes and changes with whitespace.
#[test]
fn the_context_digest_drops_top_level_retry_members_only_and_the_ticket_digest_is_raw() {
    use sha2::{Digest as _, Sha256};
    let digest = |value: &Value| {
        let context = crate::capture_queue::delivery::context(value).unwrap();
        format!(
            "{:x}",
            Sha256::digest(crate::capture_queue::delivery::encode(&context).unwrap())
        )
    };
    let value = enqueued("/fixture");
    let mut more_attempts = value.clone();
    more_attempts["attempts"] = serde_json::json!(7);
    assert_eq!(digest(&value), digest(&more_attempts));
    let mut nested = value.clone();
    nested["kind"]["attempts"] = serde_json::json!(7);
    assert_ne!(digest(&value), digest(&nested));
    let mut retried = value.clone();
    retried["next_due_ms"] = serde_json::json!(1);
    retried["parked"] = serde_json::json!(true);
    assert_eq!(
        digest(&value),
        digest(&retried),
        "retry state is not context"
    );
    let mut nested_due = value.clone();
    nested_due["kind"]["next_due_ms"] = serde_json::json!(1);
    assert_ne!(digest(&value), digest(&nested_due), "only top level strips");
    let mut retained = value.clone();
    retained["future_context"] = serde_json::json!(1);
    assert_ne!(
        digest(&value),
        digest(&retained),
        "a retained sibling is context"
    );
    let compact = serde_json::to_vec(&value).unwrap();
    let spaced = serde_json::to_vec_pretty(&value).unwrap();
    assert_eq!(
        digest(&crate::capture_queue::delivery::decode(&compact).unwrap()),
        digest(&crate::capture_queue::delivery::decode(&spaced).unwrap())
    );
    assert_ne!(Sha256::digest(&compact), Sha256::digest(&spaced));
}

/// The retry-member pin: a ticket the delivery path
/// rewrites with `next_due_ms`/`parked` keeps its `stem--ctx` name valid, so
/// recovery decodes it with the same context and digest; a changed context
/// field under that same name still refuses.
#[test]
fn retry_members_never_change_the_context_id_a_versioned_name_binds() {
    let (name, value) = input();
    let before = decode(OsStr::new(&name), &serde_json::to_vec(&value).unwrap()).unwrap();
    let mut retried = value.clone();
    retried["attempts"] = serde_json::json!(3);
    retried["next_due_ms"] = serde_json::json!(4000);
    retried["parked"] = serde_json::json!(true);
    let after = decode(OsStr::new(&name), &serde_json::to_vec(&retried).unwrap()).unwrap();
    assert_eq!(after.attempts, 3);
    assert_eq!(after.context, before.context);
    assert_eq!(
        context_sha256(&after).unwrap(),
        context_sha256(&before).unwrap()
    );
    let mut moved = value;
    moved["queued_at_ms"] = serde_json::json!(2000);
    assert!(decode(OsStr::new(&name), &serde_json::to_vec(&moved).unwrap()).is_err());
}
