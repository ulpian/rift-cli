//! The search index the hook asks the worker to build, and the repositories
//! that never ask for one.

#[cfg(feature = "search")]
use std::path::Path;

use crate::hooks::worker::owe_search_index;
#[cfg(feature = "search")]
use crate::hooks::worker::rebuild_search_index_job;

/// A work tree with a real git dir, a source file and a secret.
#[cfg(feature = "search")]
fn indexable(mode: &str) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    rift_git::test_support::fake_repo(root).unwrap();
    std::fs::create_dir_all(root.join(".rift/policies")).unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/bus.rs"), "fn main() {}\n").unwrap();
    std::fs::write(root.join(".gitignore"), "ignored\n").unwrap();
    std::fs::write(root.join("ignored"), "not a path to answer with\n").unwrap();
    std::fs::write(
        root.join(".rift/config.toml"),
        "schema_version = 1\n[project]\nname = \"t\"\n[paths]\nsensitive_paths = [\"src/secret.rs\"]\n",
    )
    .unwrap();
    std::fs::write(root.join("src/secret.rs"), "const KEY: &str = \"x\";\n").unwrap();
    std::fs::write(
        root.join(".rift/policies/serve.toml"),
        format!("[search]\nmode = \"{mode}\"\n"),
    )
    .unwrap();
    tmp
}

#[cfg(feature = "search")]
fn index_job(work_tree: &Path) -> crate::agent_queue::AgentJob {
    crate::agent_queue::AgentJob {
        schema_version: 1,
        work_tree: work_tree.to_path_buf(),
        queued_at_ms: 0,
        attempts: 0,
        kind: crate::agent_queue::JobKind::SearchIndex,
    }
}

/// The worker's pass writes the index the next query loads, over the paths
/// git would list and never over one the repository calls sensitive.
#[cfg(feature = "search")]
#[test]
fn the_worker_builds_an_index_of_the_paths_this_checkout_may_answer_with() {
    let tmp = indexable("enforce");
    let root = tmp.path();
    rebuild_search_index_job(
        &crate::hooktime::HookTimer::new("test", false),
        &index_job(root),
    )
    .unwrap();

    let index = rift_recall_search::PathIndex::load(
        &rift_recall_search::index_path(root),
        &rift_recall_search::walk::current_key(root, &rift_recall_search::head_oid(root)),
    )
    .expect("the pass left an index the next query can load");
    let paths: Vec<&str> = index.entries().iter().map(|e| e.path.as_str()).collect();
    assert!(paths.contains(&"src/bus.rs"));
    assert!(!paths.contains(&"src/secret.rs"), "sensitive");
    assert!(!paths.contains(&"ignored"), "gitignored");
    assert!(
        !paths.iter().any(|path| path.starts_with(".rift/state")),
        "rift's own run state is not searchable: {paths:?}"
    );
}

/// A repository that turned the lever off gets no index written: the pass
/// is journaled and drained, and it does nothing.
#[cfg(feature = "search")]
#[test]
fn a_repository_with_search_off_is_not_indexed() {
    let tmp = indexable("off");
    rebuild_search_index_job(
        &crate::hooktime::HookTimer::new("test", false),
        &index_job(tmp.path()),
    )
    .unwrap();
    assert!(!rift_recall_search::index_path(tmp.path()).exists());
}

/// However many hooks ask for a rebuild, one rebuild is owed.
#[test]
fn a_rebuild_asked_for_by_three_hooks_is_one_job() {
    let tmp = tempfile::tempdir().unwrap();
    let git_dir = tmp.path().join(".git");
    std::fs::create_dir_all(&git_dir).unwrap();
    for _ in 0..3 {
        assert!(owe_search_index(&git_dir, tmp.path()));
    }
    let asked: Vec<_> = crate::agent_queue::jobs(&git_dir)
        .into_iter()
        .filter(|(_, job)| matches!(job.kind, crate::agent_queue::JobKind::SearchIndex))
        .collect();
    assert_eq!(asked.len(), 1);
}
