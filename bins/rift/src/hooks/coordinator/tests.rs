//! The coordinator section: when it is said, what it names, what it costs,
//! and what it never writes.

use rift_agent::window_journal::{Interval, Line};

use super::*;
use crate::window_journal::WindowJournal;

const CC: AgentId = AgentId::CLAUDE_CODE;
const SESSION: &str = "s-coord";
const CHILD: &str = "agent-7";

fn hash(seed: u8) -> rift_core::Sha256Id {
    rift_hashchain::leaf_hash(&[seed])
}

fn short(seed: u8) -> String {
    hash(seed).to_hex()[..16].to_owned()
}

fn read(path: &str, revision: u8, at_ms: u64) -> Line {
    Line::Read {
        turn: 1,
        path: path.to_owned(),
        hash: hash(revision),
        range: Interval::new(1, 40).unwrap(),
        total_lines: 40,
        at_ms,
    }
}

fn coordinator_read(repo: &Path, line: &Line) {
    WindowJournal::open(repo, SESSION, None)
        .append(line)
        .unwrap();
}

fn opening(repo: &Path) -> Opening<'_> {
    Opening {
        repo,
        agent: &CC,
        verb: "SubagentStart",
        session: SESSION,
        window: CHILD,
    }
}

fn held(paths: &[&str]) -> Vec<CoordinatorRead> {
    paths
        .iter()
        .zip(0_u8..)
        .map(|(path, seed)| CoordinatorRead {
            path: (*path).to_owned(),
            hash: hash(seed),
            at_ms: u64::from(seed),
        })
        .collect()
}

#[test]
fn the_section_names_what_the_coordinator_read_last_first() {
    let repo = tempfile::tempdir().unwrap();
    coordinator_read(repo.path(), &read("src/older.rs", 1, 10));
    coordinator_read(repo.path(), &read("src/newer.rs", 2, 20));
    let text = coordinator_section(&opening(repo.path()), "").expect("two reads to name");
    let (newer, older) = (hash(2).to_hex(), hash(1).to_hex());
    let in_order = [
        rift_recall_serve::CoordinatorRead {
            path: "src/newer.rs",
            hash: &newer,
        },
        rift_recall_serve::CoordinatorRead {
            path: "src/older.rs",
            hash: &older,
        },
    ];
    let expected = rift_recall_serve::render_coordinator_reads_naming(&in_order, 0).unwrap();
    assert_eq!(text, expected.0);
    assert!(
        text.contains(&format!("src/newer.rs@{}", short(2))),
        "{text}"
    );
}

#[test]
fn a_coordinator_that_has_read_nothing_is_silence() {
    let repo = tempfile::tempdir().unwrap();
    assert_eq!(coordinator_section(&opening(repo.path()), ""), None);
}

#[test]
fn what_the_subagent_already_holds_is_not_named() {
    let repo = tempfile::tempdir().unwrap();
    coordinator_read(repo.path(), &read("src/a.rs", 1, 10));
    WindowJournal::open(repo.path(), SESSION, Some(CHILD))
        .append(&read("src/a.rs", 1, 5))
        .unwrap();
    assert_eq!(coordinator_section(&opening(repo.path()), ""), None);
}

/// Injection switched off is switched off for the section too.
#[test]
fn a_zero_turn_budget_is_silence() {
    let repo = tempfile::tempdir().unwrap();
    let policies = repo.path().join(".rift").join("policies");
    std::fs::create_dir_all(&policies).unwrap();
    std::fs::write(policies.join("serve.toml"), "injection_turn_budget = 0\n").unwrap();
    coordinator_read(repo.path(), &read("src/a.rs", 1, 10));
    assert_eq!(coordinator_section(&opening(repo.path()), ""), None);
}

/// The section fits the room the turn's budget leaves after the pointer,
/// dropping names into the count; when not even the count fits, nothing is
/// said.
#[test]
fn the_section_fits_the_room_the_pointer_leaves() {
    let six = held(&[
        "src/m0.rs",
        "src/m1.rs",
        "src/m2.rs",
        "src/m3.rs",
        "src/m4.rs",
        "src/m5.rs",
    ]);
    let whole = fitted_section(&six, u32::MAX).unwrap();
    let room = rift_tokens::count(&whole) - 1;
    let fitted = fitted_section(&six, room).unwrap();
    assert!(rift_tokens::count(&fitted) <= room, "{fitted}");
    assert!(fitted.contains("src/m0.rs"), "{fitted}");
    assert!(
        !fitted.contains("src/m5.rs"),
        "the tail is dropped: {fitted}"
    );
    assert_eq!(fitted_section(&six, 3), None);

    let repo = tempfile::tempdir().unwrap();
    coordinator_read(repo.path(), &read("src/a.rs", 1, 10));
    let pointer = "word ".repeat(2_000);
    assert_eq!(coordinator_section(&opening(repo.path()), &pointer), None);
}

/// End to end through the boundary composer, on a repository set up on this
/// machine: the subagent's window opens with the pointer and the section in
/// one push, booked as one spend row; a retried opening says nothing; and the
/// subagent's own journal — the only thing a decline may rest on — gains no
/// row from it.
#[test]
fn a_subagent_window_opens_once_with_its_coordinators_reads() {
    use crate::hooks::agent::{drop_projection_store, session_pointer_in, set_up_here};

    let repo = tempfile::tempdir().unwrap();
    rift_git::test_support::fake_repo(repo.path()).unwrap();
    std::fs::create_dir_all(repo.path().join(".rift")).unwrap();
    std::fs::write(
        repo.path().join(".rift").join("config.toml"),
        "schema_version = 1\n[project]\nname = \"t\"\n[features]\ntotal_recall = true\n",
    )
    .unwrap();
    let db = crate::family::recall_db(repo.path()).unwrap();
    set_up_here(repo.path());
    coordinator_read(repo.path(), &read("src/briefed.rs", 4, 10));
    let body = serde_json::json!({
        "session_id": SESSION,
        "hook_event_name": "SubagentStart",
        "agent_id": CHILD,
    })
    .to_string();
    let journal = crate::window_journal::journal_dir(repo.path()).join(format!(
        "{}.jsonl",
        crate::window_journal::key(SESSION, Some(CHILD))
    ));

    let text = session_pointer_in(repo.path(), &CC, "SubagentStart", body.as_bytes())
        .expect("the window is introduced");
    let pointer = crate::agent_help::session_pointer();
    let section = text
        .strip_prefix(pointer.as_str())
        .and_then(|rest| rest.strip_prefix('\n'))
        .expect("the pointer, then the section on a line of its own");
    assert!(
        section.contains(&format!("src/briefed.rs@{}", short(4))),
        "{section}"
    );
    assert_eq!(
        session_pointer_in(repo.path(), &CC, "SubagentStart", body.as_bytes()),
        None,
        "a retried opening pushes nothing twice"
    );
    assert!(!journal.exists(), "the subagent's journal gains no row");
    assert_eq!(spend_rows(&db), vec![i64::from(rift_tokens::count(&text))]);

    if let Some(dir) = db.parent() {
        let _ = std::fs::remove_dir_all(dir);
    }
    drop_projection_store(repo.path());
}

/// The tokens of every injection row in the family's recall store.
fn spend_rows(db: &Path) -> Vec<i64> {
    use rift_store::CanonicalStore as _;

    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let store = rift_store::open_migrated(db, &rift_store::recall_migrations()).unwrap();
    let rows = rt
        .block_on(store.query(
            "SELECT tokens_returned FROM memd_audit_event \
             WHERE verb = 'memd.manifest_injected'",
            &[],
        ))
        .unwrap();
    rows.iter().map(|row| row.get_i64(0).unwrap()).collect()
}
