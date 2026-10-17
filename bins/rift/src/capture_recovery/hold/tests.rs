#![cfg(unix)]
use super::*;
use crate::capture_recovery::test_support::{write_record, Fixture};
use std::os::unix::fs::PermissionsExt as _;

#[test]
fn matching_live_owner_plan_and_hold_admit_only_the_explicit_operation() {
    let f = Fixture::new();
    let admitted = admit_own_hold(&f.owner, &f.evidence, "fixture-op").unwrap();
    admitted.check_current().unwrap();
    assert!(admit_own_hold(&f.owner, &f.evidence, "another-op").is_err());
    assert!(crate::family::run_state::prepare_owned(&f.owner).is_err());
    let other = Fixture::new();
    assert!(admit_own_hold(&other.owner, &f.evidence, "fixture-op").is_err());
    assert_eq!(
        std::fs::read(f.evidence.join("plan.json")).unwrap(),
        f.plan_bytes
    );
}

#[test]
fn closed_hold_wire_refuses_duplicates_unknown_versions_fields_and_oversize() {
    let valid=br#"{"schema_version":1,"operation_id":"op","member_key":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","plan_sha256":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"}"#;
    let hold = Hold::decode(valid).unwrap();
    assert!(Hold::decode(&hold.encode().unwrap()).is_ok());
    for bytes in [valid.to_vec(), valid.to_vec()] {
        let text = String::from_utf8(bytes).unwrap();
        assert!(Hold::decode(
            text.replace(
                "\"schema_version\":1",
                "\"schema_version\":1,\"schema_version\":1"
            )
            .as_bytes()
        )
        .is_err());
        assert!(Hold::decode(
            text.replace("\"schema_version\":1", "\"schema_version\":2")
                .as_bytes()
        )
        .is_err());
        assert!(Hold::decode(
            text.replace(
                "\"schema_version\":1",
                "\"secret-field\":1,\"schema_version\":1"
            )
            .as_bytes()
        )
        .is_err());
    }
    assert!(Hold::decode(&vec![b' '; MAX_HOLD_BYTES + 1]).is_err());
}

#[test]
fn changed_plan_hold_or_private_namespace_revokes_the_retained_admission() {
    for target in ["hold", "plan", "private"] {
        let f = Fixture::new();
        let admitted = admit_own_hold(&f.owner, &f.evidence, "fixture-op").unwrap();
        match target {
            "hold" => {
                let bytes = std::fs::read(f.hold_path()).unwrap();
                std::fs::remove_file(f.hold_path()).unwrap();
                write_record(&f.hold_path(), &bytes);
            }
            "plan" => {
                std::fs::write(f.evidence.join("plan.json"), b"{}").unwrap();
            }
            _ => std::fs::set_permissions(&f.evidence, std::fs::Permissions::from_mode(0o755))
                .unwrap(),
        }
        assert!(admitted.check_current().is_err());
    }
}

#[test]
fn malformed_future_wrong_member_hash_paths_or_mode_never_admit() {
    for target in [
        "malformed",
        "future",
        "member",
        "hash",
        "mode",
        "missing",
        "plan-path",
        "raw-plan",
    ] {
        let f = Fixture::new();
        let path = f.hold_path();
        let original = std::fs::read(&path).unwrap();
        let mut value: serde_json::Value = serde_json::from_slice(&original).unwrap();
        match target {
            "malformed" => std::fs::write(&path, b"{").unwrap(),
            "future" => {
                value["schema_version"] = serde_json::json!(2);
                std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
            }
            "member" => {
                value["member_key"] = serde_json::json!("a".repeat(64));
                std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
            }
            "hash" => {
                value["plan_sha256"] = serde_json::json!("b".repeat(64));
                std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
            }
            "mode" => {
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
            }
            "missing" => std::fs::remove_file(&path).unwrap(),
            "plan-path" => {
                let mut p: serde_json::Value = serde_json::from_slice(&f.plan_bytes).unwrap();
                p["evidence_dir"] = serde_json::json!("/different");
                std::fs::write(
                    f.evidence.join("plan.json"),
                    serde_json::to_vec(&p).unwrap(),
                )
                .unwrap();
            }
            _ => {
                let mut bytes = f.plan_bytes.clone();
                bytes.push(b' ');
                std::fs::write(f.evidence.join("plan.json"), bytes).unwrap();
            }
        }
        assert!(admit_own_hold(&f.owner, &f.evidence, "fixture-op").is_err());
    }
}

#[test]
fn nonregular_oversize_records_and_replaced_namespaces_fail_closed() {
    for target in [
        "symlink",
        "directory",
        "fifo",
        "oversize",
        "plan-symlink",
        "plan-oversize",
        "namespace",
    ] {
        let f = Fixture::new();
        let hold = f.hold_path();
        match target {
            "oversize" => std::fs::write(&hold, vec![b' '; MAX_HOLD_BYTES + 1]).unwrap(),
            "plan-oversize" => std::fs::write(
                f.evidence.join("plan.json"),
                vec![b' '; super::super::plan::MAX_PLAN_BYTES + 1],
            )
            .unwrap(),
            "plan-symlink" => {
                let p = f.evidence.join("plan.json");
                std::fs::rename(&p, f.evidence.join("saved")).unwrap();
                std::os::unix::fs::symlink("saved", p).unwrap();
            }
            "namespace" => {
                let parent = f.evidence.parent().unwrap();
                std::fs::rename(&f.evidence, parent.join("old-operation")).unwrap();
                std::fs::create_dir(&f.evidence).unwrap();
            }
            _ => {
                std::fs::remove_file(&hold).unwrap();
                match target {
                    "symlink" => {
                        std::os::unix::fs::symlink(f.evidence.join("plan.json"), &hold).unwrap();
                    }
                    "directory" => std::fs::create_dir(&hold).unwrap(),
                    _ => assert!(std::process::Command::new("mkfifo")
                        .arg(&hold)
                        .status()
                        .unwrap()
                        .success()),
                }
            }
        }
        assert!(admit_own_hold(&f.owner, &f.evidence, "fixture-op").is_err());
    }
}

#[test]
fn supplied_evidence_alias_and_replaced_live_member_or_namespace_refuse() {
    let f = Fixture::new();
    let alias = f.evidence.parent().unwrap().join("alias");
    std::os::unix::fs::symlink(&f.evidence, &alias).unwrap();
    assert!(admit_own_hold(&f.owner, &alias, "fixture-op").is_err());
    for target in ["evidence", "member"] {
        let f = Fixture::new();
        let admitted = admit_own_hold(&f.owner, &f.evidence, "fixture-op").unwrap();
        let path = if target == "evidence" {
            &f.evidence
        } else {
            &f.member
        };
        let saved = path.with_file_name(format!(
            "{}-saved",
            path.file_name().unwrap().to_str().unwrap()
        ));
        std::fs::rename(path, &saved).unwrap();
        std::fs::create_dir(path).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(admitted.check_current().is_err());
        // All renamed fixture evidence stays under its original fixture-owned parent.
        assert!(saved.exists());
    }
}

#[test]
fn existing_cold_intermediates_cannot_disappear_rebind_or_be_symlinks() {
    for action in ["replace", "missing", "symlink"] {
        let f = Fixture::cold();
        std::fs::create_dir_all(&f.run_root).unwrap();
        if action == "symlink" {
            std::fs::remove_dir(&f.run_root).unwrap();
            std::os::unix::fs::symlink(&f.evidence, &f.run_root).unwrap();
            assert!(admit_own_hold(&f.owner, &f.evidence, "fixture-op").is_err());
            continue;
        }
        let admitted = admit_own_hold(&f.owner, &f.evidence, "fixture-op").unwrap();
        let saved = f.run_root.with_file_name("saved-run");
        std::fs::rename(&f.run_root, &saved).unwrap();
        if action == "replace" {
            std::fs::create_dir(&f.run_root).unwrap();
        }
        assert!(admitted.check_current().is_err());
        assert!(saved.exists());
    }
}

/// The journal capability is the sessions capability's sibling: the same
/// marker, the same member, the same recheck; a journal replaced under it or a
/// marker gone is noticed, and nothing is listed before readiness.
#[test]
fn the_agent_queue_capability_shares_the_marker_member_and_recheck_discipline() {
    let f = Fixture::new();
    let admitted = admit_own_hold(&f.owner, &f.evidence, "fixture-op").unwrap();
    let queue_dir = rift_shadow::hooks::run_state::RunRoot::AgentQueue.under(&f.member);
    assert!(admitted.agent_queue_namespace().is_err(), "no journal yet");
    rift_shadow::hooks::run_state::create_private_dir(&queue_dir).unwrap();
    let queue = admitted.agent_queue_namespace().unwrap();
    queue.recheck().unwrap();
    assert_eq!(queue.queue().path, queue_dir);
    assert_eq!(queue.plan().operation(), "fixture-op");
    assert_eq!(
        queue.plan().reservation(),
        "one-eligible-explicit-current-boundary@1"
    );
    assert_eq!(queue.private().unwrap().path, f.evidence);
    let marker = f.member.join(rift_shadow::hooks::run_state::MOVED_MARKER);
    let saved = f.member.join("marker-saved");
    std::fs::rename(&marker, &saved).unwrap();
    assert!(queue.recheck().is_err());
    assert!(admitted.agent_queue_namespace().is_err());
    std::fs::rename(&saved, &marker).unwrap();
    let queue = admitted.agent_queue_namespace().unwrap();
    let aside = f.member.join("queue-aside");
    std::fs::rename(&queue_dir, &aside).unwrap();
    std::fs::create_dir(&queue_dir).unwrap();
    std::fs::set_permissions(&queue_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(queue.recheck().is_err(), "a replaced journal");
    assert!(aside.exists());
    let cold = Fixture::cold();
    let cold_hold = admit_own_hold(&cold.owner, &cold.evidence, "fixture-op").unwrap();
    assert!(
        cold_hold.agent_queue_namespace().is_err(),
        "unmoved: no capability"
    );
}
