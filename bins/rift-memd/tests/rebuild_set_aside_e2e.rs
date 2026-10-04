//! `rift-memd rebuild` sets the recall projection aside, and `rift backfill
//! --recall-from` reads back what the rebuild could not derive.
//!
//! A rebuild re-derives every serve a checkpoint shard carries. A serve a
//! person makes at a terminal names no session, so no shard carries it, and
//! the projection being replaced is its only copy. These tests drive the
//! shipped binaries over a fixture repository: a session starts, a person
//! makes one `rift recall file-if-changed` serve, and the rebuilds and the
//! backfill run over the projection that leaves.
//!
//! Hermetic by construction, as `rebuild_roundtrip_e2e` is: every root either
//! binary could resolve on the developer's machine is redirected under a
//! tempdir. The binaries read their own clock; the set-aside stamp is the one
//! instant they write into a name, and nothing here asserts on it beyond its
//! shape. The stamp itself is pinned against an injected clock by the
//! set-aside module's own tests.
//!
//! The `rift` binary is the sibling of `rift-memd` in the same target
//! directory, which a workspace run (or `cargo test -p rift -p rift-memd`)
//! builds before any test starts.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use rift_store::{CanonicalStore, SqliteStore, Value};

/// Environment that re-targets a child git or rift at another repository.
const RETARGETING_ENV: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_INDEX_FILE",
    "GIT_CEILING_DIRECTORIES",
];

/// The cache root every test projects under unless it names another.
const CACHE: &str = "cache";

/// The two keys a serve's answer names: its call row and its decision row.
#[derive(Debug, Clone)]
struct Serve {
    call_id: String,
    audit_id: String,
}

/// A fixture repository and the redirected roots both binaries run under.
struct Fixture {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    home: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let home = tmp.path().join("home");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        seed_repository(&repo);
        Self {
            _tmp: tmp,
            repo,
            home,
        }
    }

    /// Every root either binary resolves, under `home`, with the cache root
    /// this call names.
    fn env(&self, cache: &str) -> Vec<(&'static str, PathBuf)> {
        let home = &self.home;
        vec![
            ("RIFT_CACHE_DIR", home.join(cache)),
            ("RIFT_CONFIG_DIR", home.join("config")),
            ("RIFT_FAMILY_DIR", home.join("families")),
            ("XDG_DATA_HOME", home.join("data")),
            ("LOCALAPPDATA", home.join("data")),
            ("HOME", home.clone()),
            ("RIFT_NO_KEYCHAIN", PathBuf::from("1")),
            ("RIFT_CAPTURE_SYNC", PathBuf::from("1")),
        ]
    }

    /// `rift <args>` from the fixture repository, `stdin` fed to it: the exit
    /// code and stdout.
    fn rift(&self, args: &[&str], stdin: &str) -> (i32, String) {
        let mut cmd = Command::new(rift_bin());
        cmd.current_dir(&self.repo)
            .args(args)
            .envs(self.env(CACHE))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        for key in RETARGETING_ENV {
            cmd.env_remove(key);
        }
        let mut child = cmd.spawn().unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(stdin.as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
        )
    }

    /// A `--json` verb that answers: exit 0, parsed.
    fn rift_json(&self, args: &[&str]) -> serde_json::Value {
        let (code, out) = self.rift(args, "");
        assert_eq!(code, 0, "{args:?}: {out}");
        serde_json::from_str(&out).unwrap_or_else(|e| panic!("{args:?}: {e}: {out}"))
    }

    /// `rift-memd rebuild --repo` over the fixture under `cache`: its stdout.
    fn memd_rebuild(&self, cache: &str) -> String {
        let out = self.memd_rebuild_output(cache);
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        assert!(
            out.status.success(),
            "rift-memd rebuild: {stdout}{}",
            String::from_utf8_lossy(&out.stderr)
        );
        stdout
    }

    fn memd_rebuild_output(&self, cache: &str) -> std::process::Output {
        let repo = self.repo.display().to_string();
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_rift-memd"));
        cmd.current_dir(&self.home)
            .args(["rebuild", "--repo", &repo])
            .envs(self.env(cache));
        for key in RETARGETING_ENV {
            cmd.env_remove(key);
        }
        cmd.output().unwrap()
    }

    /// A session starts, and a person makes one serve at a terminal: the
    /// serve's keys and the recall projection it was booked in.
    fn session_and_a_person_made_serve(&self) -> (Serve, PathBuf) {
        let start = serde_json::json!({
            "session_id": "set-aside-1",
            "hook_event_name": "SessionStart",
            "source": "startup",
            "cwd": self.repo.display().to_string(),
        });
        let (code, _) = self.rift(
            &["hooks", "claude-code", "SessionStart"],
            &start.to_string(),
        );
        assert_eq!(code, 0, "a capture hook never fails the agent");
        let (code, out) = self.rift(&["recall", "file-if-changed", "a.txt"], "");
        assert_eq!(code, 0, "{out}");
        let (call_id, audit_id) = out
            .lines()
            .find_map(|line| line.strip_prefix("serve: "))
            .and_then(|keys| keys.split_once(' '))
            .unwrap_or_else(|| panic!("the answer names its keys: {out}"));
        let serve = Serve {
            call_id: call_id.to_owned(),
            audit_id: audit_id.to_owned(),
        };
        let savings = self.rift_json(&["savings", "--json"]);
        let recall = PathBuf::from(savings["recall_db"].as_str().unwrap());
        assert_eq!(serve_rows(&recall, &serve), (1, 1), "the serve is booked");
        (serve, recall)
    }

    /// `rift backfill --recall-from <set_aside> --json [--dry-run]`.
    fn backfill(&self, set_aside: &Path, dry_run: bool) -> serde_json::Value {
        let source = set_aside.display().to_string();
        let mut args = vec!["--json", "backfill", "--recall-from", &source];
        if dry_run {
            args.push("--dry-run");
        }
        self.rift_json(&args)
    }
}

/// Two rebuilds set aside two projections under two names, each holding the
/// projection that preceded it, and print the fingerprint a rebuild over no
/// projection prints.
#[test]
fn each_rebuild_sets_aside_the_projection_before_it_and_prints_the_same_fingerprint() {
    let fx = Fixture::new();
    let (serve, db) = fx.session_and_a_person_made_serve();
    let fresh = fx.memd_rebuild("fresh");
    assert!(
        !fresh.contains("set aside:"),
        "nothing to set aside: {fresh}"
    );

    let live = sha256_of(&db);
    let first = fx.memd_rebuild(CACHE);
    let first_aside = set_aside_named(&first);
    assert_eq!(sha256_of(&first_aside), live, "the bytes moved, unchanged");
    assert_eq!(
        historical_serve_rows(&first_aside, &serve),
        (1, 1),
        "kept aside"
    );
    assert_eq!(serve_rows(&db, &serve), (0, 0), "no shard carries it");

    let rebuilt = sha256_of(&db);
    let second = fx.memd_rebuild(CACHE);
    let second_aside = set_aside_named(&second);
    assert_ne!(first_aside, second_aside);
    assert_eq!(sha256_of(&second_aside), rebuilt);
    assert_eq!(sha256_of(&first_aside), live, "a record is never rewritten");

    assert_eq!(fingerprint(&first), fingerprint(&fresh));
    assert_eq!(fingerprint(&second), fingerprint(&fresh));
    let mut found: Vec<_> = set_asides_beside(&db)
        .into_iter()
        .map(|path| path.canonicalize().unwrap())
        .collect();
    found.sort();
    let mut expected = vec![first_aside, second_aside];
    expected.sort();
    assert_eq!(found, expected);
}

/// The serve the rebuild could not derive comes back from the set-aside
/// projection, the savings document reads as it did before the rebuild, a
/// second backfill inserts nothing, a dry run writes nothing, and the
/// set-aside file is read and never written.
#[test]
fn a_backfill_from_the_set_aside_projection_restores_the_serve_a_rebuild_dropped() {
    let fx = Fixture::new();
    let (serve, db) = fx.session_and_a_person_made_serve();
    let before = fx.rift_json(&["savings", "--json"])["overall"].clone();
    let set_aside = set_aside_named(&fx.memd_rebuild(CACHE));
    let dropped = fx.rift_json(&["savings", "--json"])["overall"].clone();
    assert_ne!(dropped, before, "the rebuild dropped the person-made serve");
    let record = sha256_of(&set_aside);

    let dry = fx.backfill(&set_aside, true);
    assert_eq!(serve_rows(&db, &serve), (0, 0), "a dry run writes nothing");
    assert_eq!(inserted(&dry, "memd_audit_event"), 0);
    assert!(read(&dry, "serve_call_event") >= 1, "{dry}");

    let applied = fx.backfill(&set_aside, false);
    assert_eq!(serve_rows(&db, &serve), (1, 1), "restored: {applied}");
    assert!(inserted(&applied, "memd_audit_event") >= 1, "{applied}");
    assert!(inserted(&applied, "serve_call_event") >= 1, "{applied}");
    assert_eq!(
        fx.rift_json(&["savings", "--json"])["overall"],
        before,
        "the figures read before the rebuild are read again"
    );

    let again = fx.backfill(&set_aside, false);
    for table in recall_tables(&again) {
        assert_eq!(inserted(&again, &table), 0, "{table}: {again}");
    }
    assert_eq!(sha256_of(&set_aside), record, "the record is never written");
}

/// A file that is not a recall projection — here the capture projection — is
/// refused, and nothing is written.
#[test]
fn a_backfill_from_a_file_that_is_not_a_recall_projection_is_refused() {
    let fx = Fixture::new();
    let (serve, db) = fx.session_and_a_person_made_serve();
    fx.memd_rebuild(CACHE);
    let savings = fx.rift_json(&["savings", "--json"]);
    let capture = PathBuf::from(savings["projection_db"].as_str().unwrap());
    let source = capture.display().to_string();

    let (code, out) = fx.rift(&["backfill", "--recall-from", &source], "");

    assert_ne!(code, 0, "refused: {out}");
    assert_eq!(serve_rows(&db, &serve), (0, 0));
}

/// `rift doctor` names every set-aside projection, and `rift clean` leaves
/// each one where it was, byte for byte.
#[test]
fn doctor_names_the_set_aside_projections_and_clean_leaves_them() {
    let fx = Fixture::new();
    fx.session_and_a_person_made_serve();
    let asides = [
        set_aside_named(&fx.memd_rebuild(CACHE)),
        set_aside_named(&fx.memd_rebuild(CACHE)),
    ];
    let records: Vec<String> = asides.iter().map(|p| sha256_of(p)).collect();

    let (code, doctor) = fx.rift(&["doctor"], "");
    assert_eq!(code, 0, "{doctor}");
    for aside in &asides {
        let name = aside.file_name().unwrap().to_string_lossy();
        assert!(doctor.contains(&*name), "doctor names {name}: {doctor}");
    }
    assert!(doctor.contains("rift backfill --recall-from"), "{doctor}");

    let (code, clean) = fx.rift(&["clean"], "");
    assert_eq!(code, 0, "{clean}");
    let after: Vec<String> = asides.iter().map(|p| sha256_of(p)).collect();
    assert_eq!(after, records, "clean removes no record");
}

/// The path a rebuild's `set aside:` line names, on the line before
/// `rebuilt:`, in the documented shape.
fn set_aside_named(stdout: &str) -> PathBuf {
    let lines: Vec<&str> = stdout.lines().collect();
    let at = lines
        .iter()
        .position(|line| line.starts_with("rebuilt:"))
        .unwrap();
    let path = lines[at - 1]
        .strip_prefix("set aside:")
        .unwrap_or_else(|| panic!("a set-aside line before `rebuilt:`: {stdout}"))
        .trim();
    let path = PathBuf::from(path);
    let name = path.file_name().unwrap().to_string_lossy().into_owned();
    let stamp = name.strip_prefix("recall.db.set-aside-").unwrap();
    let (stamp, counter) = stamp.split_once('.').unwrap_or((stamp, ""));
    assert!(counter.bytes().all(|b| b.is_ascii_digit()), "{name}");
    let shape = stamp.len() == 16
        && stamp.as_bytes()[8] == b'T'
        && stamp.ends_with('Z')
        && stamp[..8].bytes().all(|b| b.is_ascii_digit())
        && stamp[9..15].bytes().all(|b| b.is_ascii_digit());
    assert!(shape, "YYYYMMDDTHHMMSSZ: {name}");
    assert!(path.is_file(), "{}", path.display());
    path
}

/// The rebuild's printed fingerprint.
fn fingerprint(stdout: &str) -> String {
    stdout
        .lines()
        .find_map(|line| line.strip_prefix("fingerprint:"))
        .unwrap_or_else(|| panic!("a fingerprint: {stdout}"))
        .trim()
        .to_owned()
}

/// Every set-aside projection beside `db`, sidecars aside.
fn set_asides_beside(db: &Path) -> Vec<PathBuf> {
    let prefix = format!("{}.set-aside-", db.file_name().unwrap().to_string_lossy());
    std::fs::read_dir(db.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            let name = path.file_name().unwrap().to_string_lossy();
            name.starts_with(&prefix) && !name.ends_with("-wal") && !name.ends_with("-shm")
        })
        .collect()
}

/// The recall tables a backfill document reports.
fn recall_tables(doc: &serde_json::Value) -> Vec<String> {
    doc["recall_imported"]
        .as_array()
        .unwrap_or_else(|| panic!("recall counts: {doc}"))
        .iter()
        .map(|t| t["table"].as_str().unwrap().to_owned())
        .collect()
}

/// One recall table's count under `field` in a backfill document.
fn recall_count(doc: &serde_json::Value, table: &str, field: &str) -> i64 {
    doc["recall_imported"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["table"] == table)
        .unwrap_or_else(|| panic!("{table} reported: {doc}"))[field]
        .as_i64()
        .unwrap()
}

fn read(doc: &serde_json::Value, table: &str) -> i64 {
    recall_count(doc, table, "read")
}

fn inserted(doc: &serde_json::Value, table: &str) -> i64 {
    recall_count(doc, table, "inserted")
}

/// How many `memd_audit_event` and `serve_call_event` rows carry the serve's
/// two keys in the projection at `db`, opened read-only.
fn serve_rows(db: &Path, serve: &Serve) -> (i64, i64) {
    let store = SqliteStore::open_read_only(db).unwrap();
    serve_rows_in(&store, serve)
}

#[test]
fn a_binary_rebuild_refuses_a_retained_wal_writer_and_preserves_its_later_rows() {
    let fx = Fixture::new();
    let (_, db) = fx.session_and_a_person_made_serve();
    let store = SqliteStore::open(&db).unwrap();
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let conn = rt.block_on(rift_store::Store::conn(&store)).unwrap();
    rt.block_on(conn.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE writer_probe (n INTEGER); INSERT INTO writer_probe VALUES (7)")).unwrap();
    drop(store);
    for value in [8, 9] {
        let refused = fx.memd_rebuild_output(CACHE);
        assert!(!refused.status.success());
        let stderr = String::from_utf8_lossy(&refused.stderr);
        assert!(stderr.contains("ProjectionBusy"), "{stderr}");
        assert!(
            stderr.contains(&format!(
                "{:?}",
                db.canonicalize().unwrap().display().to_string()
            )),
            "{stderr}"
        );
        assert!(stderr.contains("retry after it finishes"), "{stderr}");
        assert!(!String::from_utf8_lossy(&refused.stdout).contains("set aside:"));
        rt.block_on(conn.execute("INSERT INTO writer_probe VALUES (?)", &[Value::Int(value)]))
            .unwrap();
        let reader = SqliteStore::open_read_only(&db).unwrap();
        let rows = rt
            .block_on(reader.query("SELECT n FROM writer_probe ORDER BY n", &[]))
            .unwrap();
        assert_eq!(rows.last().unwrap().get_i64(0).unwrap(), value);
    }
    drop(conn);
    let preserved = set_aside_named(&fx.memd_rebuild(CACHE));
    let historical = rift_store::HistoricalProjectionSnapshot::user_selected(preserved).unwrap();
    let reader = SqliteStore::open_historical_snapshot(historical).unwrap();
    let rows = rt
        .block_on(reader.query("SELECT n FROM writer_probe ORDER BY n", &[]))
        .unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[2].get_i64(0).unwrap(), 9);
}

fn historical_serve_rows(db: &Path, serve: &Serve) -> (i64, i64) {
    let selected = rift_store::HistoricalProjectionSnapshot::user_selected(db).unwrap();
    let store = SqliteStore::open_historical_snapshot(selected).unwrap();
    serve_rows_in(&store, serve)
}

fn serve_rows_in(store: &SqliteStore, serve: &Serve) -> (i64, i64) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let count = |sql: &str, key: &str| {
        let rows = rt.block_on(store.query(sql, &[Value::text(key)])).unwrap();
        rows[0].get_i64(0).unwrap()
    };
    (
        count(
            "SELECT count(*) FROM memd_audit_event WHERE audit_id = ?",
            &serve.audit_id,
        ),
        count(
            "SELECT count(*) FROM serve_call_event WHERE call_id = ?",
            &serve.call_id,
        ),
    )
}

/// SHA-256 of a file's bytes.
fn sha256_of(path: &Path) -> String {
    rift_recall_substrate::worktree::sha256_hex(&std::fs::read(path).unwrap())
}

/// The `rift` binary: the sibling of the `rift-memd` cargo built for this
/// test, since cargo names a binary's path only to its own package's tests.
fn rift_bin() -> PathBuf {
    let memd = Path::new(env!("CARGO_BIN_EXE_rift-memd"));
    let rift = memd.with_file_name(format!("rift{}", std::env::consts::EXE_SUFFIX));
    assert!(
        rift.is_file(),
        "no rift binary beside rift-memd at {}: build both (cargo test -p rift -p rift-memd)",
        rift.display()
    );
    rift
}

/// A repository with one commit and Recall declared on.
fn seed_repository(repo: &Path) {
    git(repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("a.txt"), "one\n").unwrap();
    git(repo, &["add", "a.txt"]);
    git(repo, &["commit", "-q", "-m", "seed"]);
    std::fs::create_dir_all(repo.join(".rift")).unwrap();
    std::fs::write(
        repo.join(".rift/config.toml"),
        "schema_version = 1\n[project]\nname = \"t\"\n[features]\ntotal_recall = true\n",
    )
    .unwrap();
}

/// Run `git` in `dir` with a fixed identity and no ambient re-targeting.
fn git(dir: &Path, args: &[&str]) {
    let mut cmd = Command::new("git");
    cmd.current_dir(dir)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", dir.join("absent-gitconfig"))
        .env("GIT_AUTHOR_NAME", "rift-test")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "rift-test")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .env("GIT_AUTHOR_DATE", "2026-01-01T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2026-01-01T00:00:00Z");
    for key in RETARGETING_ENV {
        cmd.env_remove(key);
    }
    let out = cmd.output().expect("git is on PATH");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}
