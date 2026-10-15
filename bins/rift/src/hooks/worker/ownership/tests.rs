//! Invalid local completion proof retains the original delivery without retries.
use super::*;

#[test]
fn unavailable_condense_proof_stays_owed_across_two_real_drains() {
    for format in ["malformed", "future", "duplicate", "absent-checkpoint"] {
        let tmp = tempfile::tempdir().unwrap();
        rift_git::test_support::fake_repo(tmp.path()).unwrap();
        let git_dir = tmp.path().join(".git");
        let job = AgentJob {
            schema_version: 1,
            work_tree: tmp.path().to_path_buf(),
            queued_at_ms: 1000,
            attempts: 0,
            kind: crate::agent_queue::JobKind::Condense {
                session_id: "proof-session".into(),
                agent: "claude-code".into(),
                repo_id: "repo".into(),
            },
        };
        let path = crate::agent_queue::enqueue(&git_dir, &job).unwrap();
        let original = std::fs::read(&path).unwrap();
        let value = crate::capture_queue::delivery::decode(&original).unwrap();
        let context = crate::capture_queue::delivery::context(&value).unwrap();
        let record = serde_json::json!({"schema_version":1,"context":context,"checkpoint_id":"f".repeat(64)});
        let bytes = match format {
            "malformed" => b"{broken".to_vec(),
            "future" => serde_json::to_vec(&serde_json::json!({"schema_version":2})).unwrap(),
            "duplicate" => br#"{"schema_version":1,"schema_version":1}"#.to_vec(),
            _ => serde_json::to_vec(&record).unwrap(),
        };
        let name = format!(
            "completed-{}.done",
            crate::capture_queue::delivery::context_id(&context).unwrap()
        );
        let marker = crate::agent_queue::queue_dir(&git_dir).join(name);
        std::fs::write(&marker, &bytes).unwrap();
        let store = rift_store::open_capture_projection(
            crate::hooks::projection_db_at(tmp.path()).unwrap(),
        )
        .unwrap();
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let before = rt
            .block_on(store.query("SELECT * FROM checkpoint ORDER BY checkpoint_id", &[]))
            .unwrap();
        for _ in 0..2 {
            crate::hooks::worker::drain_jobs(
                git_dir.to_str(),
                crate::hooks::worker::LockWait::None,
            );
            assert_eq!(std::fs::read(&path).unwrap(), original, "{format}");
            assert_eq!(
                crate::agent_queue::read_job(&path).unwrap().1.attempts,
                0,
                "{format}"
            );
            assert_eq!(std::fs::read(&marker).unwrap(), bytes, "{format}");
            assert_eq!(
                rt.block_on(store.query("SELECT * FROM checkpoint ORDER BY checkpoint_id", &[]))
                    .unwrap(),
                before,
                "{format}"
            );
        }
    }
}

#[test]
fn unreadable_condense_cache_stays_owed_across_two_real_drains() {
    let tmp = tempfile::tempdir().unwrap();
    rift_git::test_support::fake_repo(tmp.path()).unwrap();
    let git_dir = tmp.path().join(".git");
    let sessions = crate::family::run_state::sessions_dir(&git_dir);
    std::fs::create_dir_all(&sessions).unwrap();
    let cache = sessions.join("unreadable-session.json");
    std::fs::write(&cache, b"{broken").unwrap();
    let job = AgentJob {
        schema_version: 1,
        work_tree: tmp.path().to_owned(),
        queued_at_ms: 1000,
        attempts: 0,
        kind: crate::agent_queue::JobKind::Condense {
            session_id: "unreadable-session".into(),
            agent: "claude-code".into(),
            repo_id: "repo".into(),
        },
    };
    let path = crate::agent_queue::enqueue(&git_dir, &job).unwrap();
    let before = std::fs::read(&path).unwrap();
    for _ in 0..2 {
        crate::hooks::worker::drain_jobs(git_dir.to_str(), crate::hooks::worker::LockWait::None);
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert_eq!(crate::agent_queue::read_job(&path).unwrap().1.attempts, 0);
        assert_eq!(std::fs::read(&cache).unwrap(), b"{broken");
    }
}

#[test]
fn unavailable_prior_key_proofs_retain_each_delivery_without_retrying() {
    use crate::agent_queue::{BoundaryEdge, BoundarySnapshot, JobKind};
    let kinds = [
        JobKind::ReadTokens {
            tool_call_id: "1".repeat(64),
            rel_path: "a.txt".into(),
            content_sha256: "2".repeat(64),
            blob_oid: "3".repeat(40),
            lines: None,
            schema_version: 1,
        },
        JobKind::ToolResultTokens {
            tool_result_id: "4".repeat(64),
            session_id: "s".into(),
            tool_name: "Bash".into(),
            channel: "stdout".into(),
            search: crate::search_measure::MeasuredSearch::Unmeasured,
            tree_key: None,
        },
        JobKind::BoundarySnapshot(BoundarySnapshot {
            session_id: "s".into(),
            agent: "claude-code".into(),
            ordinal: 1,
            boundary: BoundaryEdge::Start,
            head_oid: None,
            captured_at_ms: 1,
        }),
    ];
    for kind in kinds {
        let tmp = tempfile::tempdir().unwrap();
        rift_git::test_support::fake_repo(tmp.path()).unwrap();
        let git_dir = tmp.path().join(".git");
        let db = crate::hooks::projection_db_at(tmp.path()).unwrap();
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        std::fs::write(&db, b"unreadable-proof-store").unwrap();
        let job = AgentJob {
            schema_version: 1,
            work_tree: tmp.path().to_owned(),
            queued_at_ms: 1000,
            attempts: 0,
            kind,
        };
        let path = crate::agent_queue::enqueue(&git_dir, &job).unwrap();
        let before = std::fs::read(&path).unwrap();
        for _ in 0..2 {
            crate::hooks::worker::drain_jobs(
                git_dir.to_str(),
                crate::hooks::worker::LockWait::None,
            );
            assert_eq!(std::fs::read(&path).unwrap(), before);
            assert_eq!(crate::agent_queue::read_job(&path).unwrap().1.attempts, 0);
            assert_eq!(std::fs::read(&db).unwrap(), b"unreadable-proof-store");
        }
    }
}

#[cfg(unix)]
#[test]
fn admission_accepts_a_checkout_alias_but_refuses_other_missing_and_file_inputs() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("checkout");
    let other = tmp.path().join("other");
    let git_dir = rift_git::test_support::fake_repo(&root).unwrap();
    rift_git::test_support::fake_repo(&other).unwrap();
    let alias = tmp.path().join("alias");
    std::os::unix::fs::symlink(&root, &alias).unwrap();
    let file = root.join("file-input");
    std::fs::write(&file, b"not-a-worktree").unwrap();
    let owner = crate::capture_queue::acquire_owner(&git_dir).unwrap();
    for (work_tree, accepted) in [
        (alias, true),
        (root.join("gone"), false),
        (file, false),
        (other, false),
    ] {
        let job = AgentJob {
            schema_version: 1,
            work_tree: work_tree.clone(),
            queued_at_ms: 1,
            attempts: 0,
            kind: crate::agent_queue::JobKind::MoveRunState,
        };
        let path = crate::agent_queue::enqueue(&git_dir, &job).unwrap();
        assert_eq!(validate_job(&owner, &path, &job).is_ok(), accepted);
        crate::capture_queue::remove_owned(&path, &owner).unwrap();
        let entry = crate::capture_queue::QueueEntry {
            schema_version: 1,
            work_tree,
            queued_at_ms: 1,
            attempts: 0,
            commit: "a".repeat(40),
            session_id: None,
            agent: None,
            session_state: None,
        };
        let path = crate::capture_queue::enqueue(&git_dir, &entry).unwrap();
        assert_eq!(validate_capture(&owner, &path, &entry).is_ok(), accepted);
        crate::capture_queue::remove_owned(&path, &owner).unwrap();
    }
}
