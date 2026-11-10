//! The seeders the manifest's own tests share.
//!
//! One file rather than one per subject: the same projection, the same armed
//! stamp and the same committed checkout answer questions in five modules, and
//! a second copy of a seeder is a second thing to keep true.

use std::path::PathBuf;

use crate::hooks::agent::{drop_projection_store, set_up_here};

use super::held::{held_revisions, HeldRevision};
use super::stamps::{baseline_stamp, projection_stamp, record_stamp};

/// The commit the fixture repository's `HEAD` resolves to.
///
/// Synthetic, like the repository: `GixAncestry` walks from it, finds no
/// object behind it, and answers with the head alone — which is exactly the
/// ancestry of a repository holding one commit, and is what puts the seeded
/// checkpoint's anchor ON this member's line. Without an anchor the
/// injector would be silent for the honest reason — scope is never widened
/// on uncertainty — and every fixture below would be asserting that rule
/// rather than the stamp it is about.
pub(super) const FIXTURE_HEAD: &str = "1111111111111111111111111111111111111111";

/// A repository Recall is on for.
pub(super) fn fixture_repo_with_recall_on() -> tempfile::TempDir {
    let repo = tempfile::tempdir().unwrap();
    // A real gitdir: the injector resolves its store through the family,
    // and a family belongs to a repository. A fixture that
    // is not one exercises the "records nothing" arm, not the injector.
    rift_git::test_support::fake_repo(repo.path()).unwrap();
    std::fs::create_dir_all(repo.path().join(".rift")).unwrap();
    std::fs::write(
        repo.path().join(".rift").join("config.toml"),
        "schema_version = 1\n[project]\nname = \"t\"\n[features]\ntotal_recall = true\n",
    )
    .unwrap();
    set_up_here(repo.path());
    repo
}

/// A repository with a real git object database, so the revision a session
/// holds can be reached the way production reaches it.
pub(super) fn fixture_committed_repo(
    tmp: &std::path::Path,
) -> (std::path::PathBuf, impl Fn(&[&str]) -> String) {
    let repo = tmp.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let at = repo.clone();
    let git = move |args: &[&str]| -> String {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(&at)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", at.join("absent-gitconfig"))
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .output()
            .expect("git is on PATH");
        assert!(out.status.success(), "git {args:?}");
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    };
    git(&["init", "-q"]);
    git(&["config", "user.email", "t@example.invalid"]);
    git(&["config", "user.name", "t"]);
    std::fs::create_dir_all(repo.join(".rift")).unwrap();
    std::fs::write(
        repo.join(".rift").join("config.toml"),
        "schema_version = 1\n[project]\nname = \"t\"\n[features]\ntotal_recall = true\n",
    )
    .unwrap();
    (repo, git)
}

/// One open recall projection, with a method per relation a seeded fixture
/// writes a row into.
///
/// The three seeders below are the same four inserts in different quantities,
/// so the inserts are named once and composed rather than retyped — a column
/// list spelled three times is three places for a schema change to be missed.
struct ProjectionSeeder {
    db: PathBuf,
    store: rift_store::SqliteStore,
    rt: tokio::runtime::Runtime,
}

impl ProjectionSeeder {
    /// Open the family's recall projection, setting the fixture up on this
    /// machine first — the injector asks both questions.
    fn open_at(repo: &std::path::Path) -> Self {
        let db = crate::family::recall_db(repo).unwrap();
        set_up_here(repo);
        let store = rift_store::open_recall_projection(&db).unwrap();
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        Self { db, store, rt }
    }

    fn seed_checkpoint_row(&self, id: &str, ordinal: i64) {
        use rift_store::{CanonicalStore as _, Value};

        self.rt
            .block_on(self.store.insert_or_ignore(
                "n_checkpoint",
                &[
                    "node_id",
                    "dialect",
                    "ordinal",
                    "prev_checkpoint",
                    "strategy",
                    "merkle_root",
                    "developer",
                ],
                &[
                    Value::text(id),
                    Value::text("rift"),
                    Value::Int(ordinal),
                    Value::Null,
                    Value::Null,
                    Value::Null,
                    Value::Null,
                ],
            ))
            .unwrap();
    }

    /// The commit a checkpoint is anchored on — what puts it on a member's
    /// line, or off it.
    fn seed_checkpoint_commit_row(&self, id: &str, anchor: &str) {
        use rift_store::{CanonicalStore as _, Value};

        self.rt
            .block_on(self.store.insert_or_ignore(
                "n_checkpoint_commit",
                &["node_id", "commit_oid", "schema_version"],
                &[Value::text(id), Value::text(anchor), Value::Int(1)],
            ))
            .unwrap();
    }

    fn seed_file_revision_row(&self, node_id: &str, path: &str, sha: &str, size: i64) {
        use rift_store::{CanonicalStore as _, Value};

        self.rt
            .block_on(self.store.insert_or_ignore(
                "n_file_revision",
                &["node_id", "path", "blob_oid", "content_sha256", "size"],
                &[
                    Value::text(node_id),
                    Value::text(path),
                    Value::text(sha),
                    Value::text(sha),
                    Value::Int(size),
                ],
            ))
            .unwrap();
    }

    fn seed_file_read_row(&self, read: &SeededRead<'_>) {
        use rift_store::{CanonicalStore as _, Value};

        self.rt
            .block_on(self.store.insert_or_ignore(
                "n_file_read",
                &[
                    "node_id",
                    "checkpoint_id",
                    "tool_call_id",
                    "file_revision",
                    "input_tokens",
                    "read_ordinal",
                    "byte_range_lo",
                    "byte_range_hi",
                ],
                &[
                    Value::text(read.node_id),
                    Value::text(read.checkpoint_id),
                    Value::text(read.tool_call_id),
                    Value::text(read.file_revision),
                    Value::Int(read.input_tokens),
                    Value::Int(read.read_ordinal),
                    Value::Null,
                    Value::Null,
                ],
            ))
            .unwrap();
    }
}

/// One seeded read fact — five identifiers and two numbers, which is more
/// positional arguments than a reader can keep straight.
struct SeededRead<'a> {
    node_id: &'a str,
    checkpoint_id: &'a str,
    tool_call_id: &'a str,
    file_revision: &'a str,
    input_tokens: i64,
    read_ordinal: i64,
}

/// The `main` branch resolving to [`FIXTURE_HEAD`], so the ancestry walk has a
/// head to start from.
fn seed_head_ref(repo: &std::path::Path) {
    let heads = repo.join(".git").join("refs").join("heads");
    std::fs::create_dir_all(&heads).unwrap();
    std::fs::write(heads.join("main"), format!("{FIXTURE_HEAD}\n")).unwrap();
}

/// A projection with a real delta in it: one checkpoint, two file
/// revisions, both read. Every other manifest test runs against an empty
/// projection, where [`turn_manifest`](super::turn_manifest)'s injection
/// branch is never taken at all — so the audit write inside it, and the id it
/// keys by, were never executed by any test.
pub(super) fn seed_two_read_revisions(repo: &std::path::Path) -> PathBuf {
    seed_head_ref(repo);
    let seeder = ProjectionSeeder::open_at(repo);
    seeder.seed_checkpoint_row("c1", 1);
    seeder.seed_checkpoint_commit_row("c1", FIXTURE_HEAD);
    for (i, path) in ["crates/rift-store/src/schema.rs", "bins/rift/src/hooks.rs"]
        .into_iter()
        .enumerate()
    {
        let rev = format!("rev{i}");
        seeder.seed_file_revision_row(&rev, path, &format!("{i:064x}"), 10);
        seeder.seed_file_read_row(&SeededRead {
            node_id: &format!("read{i}"),
            checkpoint_id: "c1",
            tool_call_id: &format!("tc{i}"),
            file_revision: &rev,
            input_tokens: 100,
            read_ordinal: 1,
        });
    }
    seeder.db
}

/// [`seed_two_read_revisions`] with `n` synthetic revisions, for the cases
/// that need a delta bigger than `diff_since`'s own response cap.
pub(super) fn seed_many_read_revisions(repo: &std::path::Path, n: usize) -> PathBuf {
    seed_head_ref(repo);
    let seeder = ProjectionSeeder::open_at(repo);
    seeder.seed_checkpoint_row("c1", 1);
    seeder.seed_checkpoint_commit_row("c1", FIXTURE_HEAD);
    for i in 0..n {
        let rev = format!("rev{i}");
        let path = format!("crates/pkg{i}/src/mod{i}.rs");
        seeder.seed_file_revision_row(&rev, &path, &format!("{i:064x}"), 4096);
        seeder.seed_file_read_row(&SeededRead {
            node_id: &format!("read{i}"),
            checkpoint_id: "c1",
            tool_call_id: &format!("tc{i}"),
            file_revision: &rev,
            input_tokens: 1000,
            read_ordinal: i64::try_from(i).unwrap() + 1,
        });
    }
    seeder.db
}

/// Land one checkpoint with an anchor commit and one read fact, the way a
/// sibling member's archive would.
pub(super) fn seed_sibling_checkpoint(
    repo: &std::path::Path,
    id: &str,
    ordinal: i64,
    anchor: &str,
    sha: &str,
) {
    let seeder = ProjectionSeeder::open_at(repo);
    seeder.seed_checkpoint_row(id, ordinal);
    seeder.seed_checkpoint_commit_row(id, anchor);
    let rev = format!("rev_{id}");
    seeder.seed_file_revision_row(&rev, &format!("crates/{id}/src/lib.rs"), sha, 10);
    seeder.seed_file_read_row(&SeededRead {
        node_id: &format!("read_{id}"),
        checkpoint_id: id,
        tool_call_id: &format!("tc_{id}"),
        file_revision: &rev,
        input_tokens: 100,
        read_ordinal: ordinal * 1_000_000,
    });
}

/// Arm both levels of the stamp for `key`, exactly as a turn that accounted
/// for this state would have left them.
pub(super) fn arm_turn_stamp(repo: &std::path::Path, key: &str) {
    let projection = projection_stamp(repo);
    let scoped = scoped_stamp_as_it_stands(repo);
    record_stamp(repo, key, projection.as_deref(), scoped.as_deref());
}

/// The stamp a turn accounting for the state as it stands NOW would leave.
pub(super) fn stamp_as_it_stands(repo: &std::path::Path, key: &str) -> String {
    format!(
        "{}|{}|{}",
        projection_stamp(repo).unwrap(),
        baseline_stamp(repo, key),
        scoped_stamp_as_it_stands(repo).unwrap()
    )
}

/// The scoped stamp for this member, as the injector would compute it.
pub(super) fn scoped_stamp_as_it_stands(repo: &std::path::Path) -> Option<String> {
    let rt = tokio::runtime::Builder::new_current_thread().build().ok()?;
    let db = crate::family::recall_db(repo).ok()?;
    let store = rift_store::open_recall_projection(&db).ok()?;
    let family = crate::family::try_at(repo)?;
    let (scope, ancestry) = crate::memory::serving_scope(&family, repo).ok()?;
    rt.block_on(rift_recall_ledger::scope_query::scoped_stamp(
        &store,
        &scope,
        ancestry.as_ref(),
    ))
    .ok()
}

/// Record that the window read all of `text` at the revision those bytes
/// hash to — the line the gate writes on a read it let through.
pub(super) fn journal_a_read(
    repo: &std::path::Path,
    session: &str,
    rel: &str,
    text: &str,
    at_ms: u64,
) {
    let lines = u32::try_from(text.lines().count().max(1)).unwrap();
    crate::window_journal::WindowJournal::open(repo, session, None)
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

/// The revisions a window holds, read from the journal the gate wrote —
/// the thunk the production caller passes.
pub(super) fn held_from_window_journal(repo: &std::path::Path, session: &str) -> Vec<HeldRevision> {
    held_revisions(
        &crate::window_journal::WindowJournal::open(repo, session, None)
            .load()
            .journal,
    )
}

pub(super) fn drop_fixture_family_store(repo: &std::path::Path) {
    if let Ok(db) = crate::family::recall_db(repo) {
        if let Some(dir) = db.parent() {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
    drop_projection_store(repo);
}
