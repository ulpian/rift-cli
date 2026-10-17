use super::*;

const SELECTED_PLAN: &[u8] = r#"{"budgets":{"max_artifact_bytes":8388608,"max_cut_bytes":8388608,"max_inventory_entries":8192,"max_selected_tickets":8192,"max_ticket_bytes":65536,"max_total_ticket_bytes":67108864},"canonical_git_dir":"/fixture/évidence/repository.git","canonical_work_tree":"/fixture/évidence/checkout","evidence_dir":"/fixture/private/fixture-op-2","member_key":"fc3e528b5b06fc357b86863e95b2c46dda9888bd9ed83165af21fe46aac1767a","operation_id":"fixture-op-2","repo_id":"fixture-repo","schema_version":1,"selector":{"inventory":"complete-bounded","kind":"boundary-snapshots-only","reservation":"one-eligible-explicit-current-boundary@1","schema_version":1,"unknown_fields":"refuse"}}"#.as_bytes();
const SELECTED_HOLD: &[u8] = r#"{"member_key":"fc3e528b5b06fc357b86863e95b2c46dda9888bd9ed83165af21fe46aac1767a","operation_id":"fixture-op-2","plan_sha256":"74ca4167bf8a36b1950575cd23d704abe41f101ff2c9c93e903bdcb5b21f30c9","schema_version":1}"#.as_bytes();

#[test]
fn selected_python_vectors_keep_exact_canonical_bytes_raw_hashes_and_nul_domain() {
    let plan = Plan::decode(SELECTED_PLAN).unwrap();
    assert_eq!(plan.encode().unwrap(), SELECTED_PLAN);
    assert_eq!(
        plan.sha256().unwrap().as_str(),
        "74ca4167bf8a36b1950575cd23d704abe41f101ff2c9c93e903bdcb5b21f30c9"
    );
    assert_eq!(
        super::super::hold::Hold::decode(SELECTED_HOLD)
            .unwrap()
            .encode()
            .unwrap(),
        SELECTED_HOLD
    );
    assert_eq!(
        member_key(
            &RepoId::new("fixture-repo").unwrap(),
            "/fixture/évidence/repository.git"
        )
        .unwrap()
        .to_hex(),
        "fc3e528b5b06fc357b86863e95b2c46dda9888bd9ed83165af21fe46aac1767a"
    );
    assert_eq!(
        format!("{:x}", Sha256::digest(SELECTED_HOLD)),
        "d9e4a9b3e46ec589dbe88ec738c85934425c07d34c392a48db06a22728c8af03"
    );
    let canonical = codec::wire_encode(&serde_json::json!({"canonical_git_dir":"/fixture/évidence/repository.git","repo_id":"fixture-repo"}),MAX_PLAN_BYTES).unwrap();
    let preimage = [
        b"rift-capture-recovery-member@1\0".as_slice(),
        canonical.as_slice(),
    ]
    .concat();
    assert_eq!(preimage.len(), 113);
    #[expect(
        clippy::naive_bytecount,
        reason = "a one-off fixture check; the bytecount crate is not a dependency"
    )]
    let zero_bytes = preimage.iter().filter(|c| **c == 0).count();
    assert_eq!(zero_bytes, 1);
    assert_eq!(
        format!("{:x}", Sha256::digest(&preimage)),
        "fc3e528b5b06fc357b86863e95b2c46dda9888bd9ed83165af21fe46aac1767a"
    );
    assert!(!SELECTED_PLAN.ends_with(b"\n"));
    assert_eq!(plan.wire.budgets.max_inventory_entries, 8192);
}

fn rejected(value: serde_json::Value) {
    let bytes = serde_json::to_vec(&value).unwrap();
    assert!(Plan::decode(&bytes).is_err());
}

#[test]
fn schemas_budgets_selectors_and_paths_are_closed_checked_and_bounded() {
    let valid: serde_json::Value = serde_json::from_slice(SELECTED_PLAN).unwrap();
    for key in valid.as_object().unwrap().keys() {
        let mut value = valid.clone();
        value.as_object_mut().unwrap().remove(key);
        rejected(value);
    }
    for (key, replacement) in [
        ("schema_version", serde_json::json!(2)),
        ("operation_id", serde_json::json!("../unsafe")),
        ("operation_id", serde_json::json!("a".repeat(65))),
        ("repo_id", serde_json::json!("bad repo")),
        ("repo_id", serde_json::json!("x".repeat(129))),
        ("canonical_git_dir", serde_json::json!("/a/../b")),
        ("canonical_work_tree", serde_json::json!("relative")),
        ("evidence_dir", serde_json::json!("/a\u{0}b")),
        (
            "evidence_dir",
            serde_json::json!("/".to_owned() + &"a".repeat(4096)),
        ),
        ("unknown", serde_json::json!("secret-rejected-value")),
    ] {
        let mut value = valid.clone();
        value[key] = replacement;
        rejected(value);
    }
    for key in [
        "kind",
        "schema_version",
        "inventory",
        "reservation",
        "unknown_fields",
        "extra",
    ] {
        let mut value = valid.clone();
        value["selector"][key] = serde_json::json!("unsupported");
        rejected(value);
    }
    for key in valid["budgets"].as_object().unwrap().keys() {
        for number in [
            serde_json::json!(0),
            serde_json::json!(-1),
            serde_json::json!(1.5),
            serde_json::json!(u64::MAX),
        ] {
            let mut value = valid.clone();
            value["budgets"][key] = number;
            rejected(value);
        }
    }
    let mut value = valid;
    value["budgets"]["max_inventory_entries"] = serde_json::json!(8193);
    rejected(value);
}

#[test]
fn recursively_duplicate_valid_fields_unknowns_and_trailing_data_refuse_closed() {
    let source = std::str::from_utf8(SELECTED_PLAN).unwrap();
    for (needle, replacement) in [
        (
            "\"schema_version\":1,\"selector\"",
            "\"schema_version\":1,\"schema_version\":1,\"selector\"",
        ),
        (
            "\"inventory\":\"complete-bounded\"",
            "\"inventory\":\"complete-bounded\",\"inventory\":\"complete-bounded\"",
        ),
        (
            "\"max_cut_bytes\":8388608",
            "\"max_cut_bytes\":8388608,\"max_cut_bytes\":8388608",
        ),
    ] {
        assert!(Plan::decode(source.replace(needle, replacement).as_bytes()).is_err());
    }
    assert!(Plan::decode(&[SELECTED_PLAN, b"{}"].concat()).is_err());
    assert!(Plan::decode(&vec![b' '; MAX_PLAN_BYTES + 1]).is_err());
    assert!(Plan::decode(b"\xff").is_err());
    let error = Plan::decode(b"{\"rejected-secret-field\":true}")
        .err()
        .unwrap();
    assert!(!format!("{error:?} {error}").contains("rejected-secret"));
    assert!(std::error::Error::source(&error).is_none());
}

#[test]
fn map_order_changes_decode_but_persisted_encoding_is_one_canonical_no_lf_form() {
    let value: serde_json::Value = serde_json::from_slice(SELECTED_PLAN).unwrap();
    let pretty = serde_json::to_vec_pretty(&value).unwrap();
    assert_ne!(pretty, SELECTED_PLAN);
    assert_eq!(
        Plan::decode(&pretty).unwrap().encode().unwrap(),
        SELECTED_PLAN
    );
    let mut value = value;
    value["budgets"]["max_selected_tickets"] = serde_json::json!(8192);
    value["budgets"]["max_inventory_entries"] = serde_json::json!(8191);
    rejected(value);
    for path in ["/a//b", "/a/./b", "/a/", "/a/../b", ""] {
        assert!(lexical_path(path).is_err());
    }
    assert!(lexical_path(&format!("/{}", "a".repeat(4095))).is_ok());
    assert!(operation(&"x".repeat(64)).is_ok());
}

#[test]
fn every_nested_field_is_required_and_unknown_budget_fields_are_refused() {
    let valid: serde_json::Value = serde_json::from_slice(SELECTED_PLAN).unwrap();
    for object in ["selector", "budgets"] {
        for key in valid[object].as_object().unwrap().keys() {
            let mut value = valid.clone();
            value[object].as_object_mut().unwrap().remove(key);
            rejected(value);
        }
        let mut value = valid.clone();
        value[object]["unknown-secret-field"] = serde_json::json!("rejected");
        rejected(value);
    }
    let mut value = valid;
    value["operation_id"] = serde_json::json!("é");
    rejected(value);
}
