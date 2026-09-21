//! The seeders the recovery boundary's own tests share.
//!
//! One file rather than a copy per module: a compacted window with reads
//! behind it is the fixture three modules put their questions to, and a second
//! copy of it is a second thing to keep true. It carries no `#[cfg(test)]`
//! marker of its own because the module declaration in `mod.rs` carries one,
//! and that declaration sits at the foot of the file so the production half
//! above it stays whole.

use crate::hooks::agent::{drop_projection_store, set_up_here};

/// The families and verbs this chain is driven on, spelled once.
pub(super) const CC: rift_core::AgentId = rift_core::AgentId::CLAUDE_CODE;
pub(super) const START: &str = "SessionStart";

/// A Recall-enabled repository with a real git dir, so every store this
/// path opens is a family's.
pub(super) fn recall_repo(tmp: &std::path::Path) -> std::path::PathBuf {
    rift_git::test_support::fake_repo(tmp).unwrap();
    std::fs::create_dir_all(tmp.join(".rift")).unwrap();
    std::fs::write(
        tmp.join(".rift").join("config.toml"),
        "schema_version = 1\n[project]\nname = \"t\"\n[features]\ntotal_recall = true\n",
    )
    .unwrap();
    set_up_here(tmp);
    tmp.to_path_buf()
}

/// Write `text` to `rel` and record that the window read all of it, at the
/// revision it has right now.
pub(super) fn read_and_hold(
    repo: &std::path::Path,
    session: &str,
    rel: &str,
    text: &str,
    at_ms: u64,
) {
    std::fs::write(repo.join(rel), text).unwrap();
    let journal = crate::window_journal::WindowJournal::open(repo, session, None);
    let lines = u32::try_from(text.lines().count().max(1)).unwrap();
    journal
        .append(&rift_agent::window_journal::Line::Read {
            turn: 1,
            path: rel.to_owned(),
            hash: rift_core::Sha256Id::parse_hex(&rift_shadow::sha256_hex(&[text.as_bytes()]))
                .unwrap(),
            range: rift_agent::window_journal::Interval::new(1, lines).unwrap(),
            total_lines: lines,
            at_ms,
        })
        .unwrap();
}

pub(super) fn evict(repo: &std::path::Path, session: &str, at_ms: u64) {
    crate::window_journal::WindowJournal::open(repo, session, None)
        .append(&rift_agent::window_journal::Line::Evict {
            turn: 1,
            reason: rift_agent::window_journal::EvictReason::PreCompact,
            at_ms,
        })
        .unwrap();
}

pub(super) fn compacted(session: &str) -> Vec<u8> {
    serde_json::json!({ "session_id": session, "source": "compact" })
        .to_string()
        .into_bytes()
}

pub(super) fn drop_family_store(repo: &std::path::Path) {
    if let Ok(db) = crate::family::recall_db(repo) {
        if let Some(dir) = db.parent() {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
    drop_projection_store(repo);
}
