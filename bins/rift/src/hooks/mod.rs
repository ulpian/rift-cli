//! `rift hook[s] …` — the git + agent hook entrypoints.
//!
//! Three surfaces:
//! * `rift hook install` — install the seven absolute-path git-hook entries.
//! * `rift hooks git <name> --git-dir <dir> -- <hook args…>` — the git lifecycle
//!   entrypoints. Every commit-path hook runs under the **never-block** guard
//!   (`catch_unwind` + `exit(0)`); only `pre-push` may exit non-zero, and only to
//!   reject a `rift/*` ref.
//! * `rift hooks <agent> <verb>` — the stdin-JSON capture contract. It ingests one
//!   hook into the per-repo projection and **never fails the agent** (always exits
//!   0; a capture error is swallowed and the process exits "deferred").

mod agent;
mod archive;
mod audit;
mod doc_gate;
mod gate;
mod git;
mod ingest;
mod install;
mod manifest;
mod recovery;
mod sweep;
mod worker;

use std::path::{Path, PathBuf};

use rift_core::Result;
use rift_shadow::hooks::schema_json;
#[cfg(unix)]
use rift_shadow::hooks::HOOK_NAMES;

use agent::run_agent_hook;
use audit::run_memd_hook;
use git::run_git_hook;
use install::cmd_install;
use worker::run_capture_worker;

pub(crate) use agent::hook_degradation;
pub(crate) use archive::{
    accrue_soak, attach_archive, import_archive, level_projection, STALE_REWRITE_TOML,
};
pub(crate) use audit::{append_memd_audit, journal_serve_envelope};
#[cfg(feature = "search")]
pub(crate) use gate::payload_cwd;
pub(crate) use gate::{gate_step, gate_turn, GateStep, AUDIT_IDS};
#[cfg(feature = "search")]
pub(crate) use git::effective_config;
pub(crate) use install::{
    effective_hooks_dir, family_sites, hook_names, install_family_hooks, report_lines,
};
pub(crate) use manifest::seed_manifest_baseline;
pub(crate) use recovery::basis_snapshots_at;
pub(crate) use sweep::{family_sweep_floor, sweep_stale_sessions, SessionSweep};
pub(crate) use worker::drain_capture_queue;
#[cfg(feature = "search")]
pub(crate) use worker::owe_search_index;

/// Detect the direct-link Unix hook entry before clap parses the native git-hook
/// arguments as Rift's human command tree.
#[cfg(unix)]
fn direct_git_hook_name_from(argv0: &std::ffi::OsStr) -> Option<&'static str> {
    let basename = Path::new(argv0).file_name()?.to_str()?;
    HOOK_NAMES.iter().copied().find(|name| *name == basename)
}

#[cfg(unix)]
pub(crate) fn direct_git_hook_name() -> Option<&'static str> {
    std::env::args_os()
        .next()
        .as_deref()
        .and_then(direct_git_hook_name_from)
}

/// Route a direct Unix hook link through the same guarded implementation as the
/// explicit `rift hooks git ...` compatibility entry.
#[cfg(unix)]
pub(crate) fn run_direct_git_hook(name: &str, forwarded_args: Vec<String>) -> ! {
    let git_dir = std::env::var("GIT_DIR")
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| ".git".to_owned());
    let mut wire = Vec::with_capacity(forwarded_args.len() + 4);
    wire.push(name.to_owned());
    wire.push("--git-dir".to_owned());
    wire.push(git_dir);
    wire.push("--".to_owned());
    wire.extend(forwarded_args);
    run_git_hook(&wire)
}

/// Entry point for the `hook`/`hooks` subcommand. `rest` is the argv after the
/// `hook`/`hooks` token.
///
/// # Errors
/// [`rift_core::RiftError`] only from `install` (the capture paths swallow and
/// exit 0 themselves).
pub(crate) fn run(rest: &[String]) -> Result<()> {
    match rest.split_first() {
        Some((sub, _)) if sub == "--schema" => cmd_schema(),
        Some((sub, args)) if sub == "install" => cmd_install(args),
        Some((sub, args)) if sub == "git" => run_git_hook(args), // diverges
        Some((sub, args)) if sub == "worker" => run_capture_worker(args), // diverges
        Some((sub, args)) if sub == "memd" => run_memd_hook(args), // diverges
        // The document converter, in a child that is this binary re-spawned:
        // routed here, before the agent arm, and no adapter is named `doc`.
        Some((sub, args)) if sub == "doc" => crate::doc_helper::run_doc_helper(args), // diverges
        Some((agent, args)) => run_agent_hook(agent, args),                           // diverges
        None => {
            println!(
                "usage: rift hook install [--repoint] [--binary PATH] [--allow-transient] | \
                 rift hooks git <name> | rift hooks <agent> <verb> | rift hooks --schema | \
                 rift hooks doc convert <path|-> [--ext EXT] [--json]"
            );
            Ok(())
        }
    }
}

/// `rift hooks --schema`: emit the versioned canonical-event schema (for SDK
/// codegen / adapter authors).
fn cmd_schema() -> Result<()> {
    let schema = schema_json();
    println!(
        "{}",
        serde_json::to_string_pretty(&schema).unwrap_or_else(|_| "{}".into())
    );
    Ok(())
}

/// The value following `name` in `args` (`--git-dir <v>`), if present.
pub(super) fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

/// The process working directory, or `.` if it is unreadable. Capture is
/// never allowed to fail on this — a hook that cannot read its own cwd still
/// has to exit 0.
pub(super) fn current_dir_or_dot() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

/// Resolve `cwd`'s repository the way git resolves it.
///
/// Returns the whole [`rift_git::RepoPaths`] — gitdir (where this worktree's
/// session state lives), work tree (what a repo-relative payload path resolves
/// against), COMMON dir (which family this checkout belongs to) and the
/// worktree token. It used to return a two-field struct mirroring the first
/// two; the worktree-provenance writer needs the other two, and a second type
/// holding half of a type is how the two come to disagree.
///
/// **A linked worktree's `.git` is a FILE**, not a directory — it holds
/// `gitdir: <main>/.git/worktrees/<name>`. The naive `<cwd>/.git` therefore
/// named a non-directory, and every `open`/`mkdir` beneath it failed `ENOTDIR`,
/// which `std::io` reports as `NotADirectory` and *not* `NotFound` — so the
/// session-cache read returned an error instead of "no session yet" and ingest
/// aborted at that `?` **before** it could write its receipt. Every agent hook
/// fired in a linked worktree was dropped, leaving no evidence it had ever
/// arrived. Discovery also walks *up*, so a hook fired from a subdirectory now
/// lands in its repository's projection instead of minting a private one under
/// the subdirectory's path.
///
/// **Outside a repository there is no repository.** This used to
/// fall back to `<cwd>/.git` — "exactly the pre-discovery behaviour" — and that
/// fallback is what minted phantoms: a hook whose cwd followed the shell out of
/// any checkout created a `.git` directory wherever it happened to be standing,
/// and the next `discover` walking up through it read that directory as a
/// repository with an identity of its own. Five of them existed in this tree
/// at once (several nested design-note and source directories), each holding
/// sessions that belonged to the real repository above them.
///
/// `None` is not a failure: the hook still exits 0 (a hook never blocks an
/// agent, ever), it simply records nothing. Totality lives in the caller,
/// which is where "record nothing" can actually be spelled.
pub(super) fn capture_repo(cwd: &Path) -> Option<rift_git::RepoPaths> {
    rift_git::discover(cwd).ok()
}

/// Where git looks up this repository's hooks: the **common** dir.
///
/// Linked worktrees do not get their own hooks — git resolves `hooks/<name>`
/// through the common dir, so one install covers every worktree of the repo
/// (measured: a commit made in a linked worktree runs `<main>/.git/hooks/…`,
/// with `GIT_DIR` pointing at the per-worktree admin dir, which is what the
/// shim forwards). An explicit `--git-dir` that exists still wins.
///
/// The discovered common dir is canonicalized: a linked worktree derives it
/// from git's `commondir` file, which is *relative* (`../..`), and the shim
/// paths this install prints are read by humans.
/// **Outside a repository there is no hooks directory** — `None`, and the
/// caller refuses. This used to answer `<cwd>/.git`, and the install that
/// followed CREATED that directory: the phantom-gitdir mint that was retired
/// everywhere else in this binary, still standing in the one command whose whole
/// job is to write into it. `capture_repo` lost the same fallback earlier; this
/// is the last of them.
pub(super) fn resolve_hooks_git_dir(explicit: Option<PathBuf>, cwd: &Path) -> Option<PathBuf> {
    if let Some(p) = explicit {
        if p.exists() {
            return Some(p);
        }
    }
    rift_git::discover(cwd)
        .ok()
        .map(|r| r.common_dir.canonicalize().unwrap_or(r.common_dir))
}

/// The **family's** capture projection for a repository root (the palette
/// dispatcher passes the selected project's — never process cwd).
///
/// Keyed by the family id recorded in the common dir, not by this checkout's
/// path: every linked worktree of a repository reads and writes
/// the one projection its main work tree already owns. The refusal arms — no
/// repository, no provable main, an unreadable manifest — belong to
/// [`crate::family::resolve_at`], which is where they can be spelled as the
/// command that fixes them rather than as a fresh, empty database.
///
/// # Errors
/// Propagates [`crate::family::projection_db`].
pub(crate) fn projection_db_at(root: &Path) -> Result<PathBuf> {
    crate::family::projection_db(root)
}

/// Wall-clock now (epoch ms) for the live capture path; 0 if the clock is unreadable.
pub(crate) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_millis()).ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hooks::audit::read_identified_bytes;
    use crate::hooks::git::{capture_sync_note, CAPTURE_SYNC_ENV};
    use crate::hooks::ingest::enrich_native_context;
    use crate::hooks::ingest::worktree::{observe_worktree, worktree_read_hasher};
    use crate::hooks::install::hooks_git_dir_or_refusal;
    use crate::hooks::manifest::projection_stamp;
    use crate::hooks::worker::line_slice;
    use rift_shadow::hooks::AgentId;

    /// The inline token cap is ZERO (measured: the
    /// tokenizer's initialisation alone is over the hook budget). The hasher
    /// records the read's identity and never a count, however small the file;
    /// the count is the detached worker's.
    #[test]
    fn the_read_hasher_records_identity_and_never_a_count() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("tiny.txt"), "one\n").unwrap();
        let hasher = worktree_read_hasher(tmp.path());
        let rc = hasher("tiny.txt").expect("a real file resolves");
        assert_eq!(rc.rel_path, "tiny.txt");
        assert_eq!(
            rc.content_sha256,
            "2c8b08da5ce60398e1f19af0e5dccc744df274b826abe585eaba68c525434806"
        );
        assert_eq!(rc.tokens, None, "no inline count at any size");
    }

    /// **A shell read is priced at the range it asked for.**
    ///
    /// `sed -n '40,60p' f` put twenty-one lines in front of a model, and
    /// booking the whole file against it would put tokens in the savings
    /// denominator that never reached a context window. The bytes counted are
    /// the file's own at the identity the fact recorded — not the client's
    /// rendering of them, which may have been truncated on the way to the
    /// terminal and is not what the file costs.
    #[test]
    fn a_ranged_shell_read_is_priced_over_its_lines_not_the_file() {
        let mut file = String::new();
        for n in 1..=200 {
            file.push_str(&format!("line {n} of the file\n"));
        }
        let slice = line_slice(&file, 40, 60);
        assert_eq!(slice.lines().count(), 21, "inclusive on both ends");
        assert!(slice.starts_with("line 40 "));
        assert!(slice.ends_with("line 60 of the file\n"));
        assert!(
            rift_tokens::count(&slice) < rift_tokens::count(&file),
            "the range is a fraction of the file, and so is its price"
        );

        // A bare `cat` names the end of the file, which clamps to the file.
        assert_eq!(line_slice(&file, 1, u32::MAX), file);
        // A range that starts past the last line delivered nothing, and
        // nothing is not rounded up to one line.
        assert_eq!(line_slice(&file, 900, 950), "");
    }

    /// **The late-count contract, on the shell path too.** The count is owed
    /// to the detached worker, and the worker counts the bytes the fact
    /// identified or none at all.
    ///
    /// Two ways a read goes unidentified, and neither may produce a figure. A
    /// path that resolves to no file leaves no read fact at all, so nothing is
    /// ever owed. And bytes that have moved on since the read — the working
    /// file edited, with no git blob carrying the old content — are refused by
    /// the worker's own identity check rather than counted under the new
    /// bytes, which would price a slice the model never saw.
    #[test]
    fn a_shell_read_of_an_unidentified_file_leaves_no_count() {
        let tmp = tempfile::tempdir().unwrap();
        let hasher = worktree_read_hasher(tmp.path());
        assert!(
            hasher("vanished.rs").is_none(),
            "no file, no fact — and with no fact there is no count to owe"
        );

        std::fs::write(tmp.path().join("moved.rs"), "one\ntwo\n").unwrap();
        let rc = hasher("moved.rs").expect("a real file resolves");
        assert_eq!(
            read_identified_bytes(tmp.path(), "moved.rs", &rc.content_sha256, &rc.blob_oid)
                .as_deref(),
            Some(b"one\ntwo\n".as_slice()),
            "the bytes the fact identified are the bytes the worker counts"
        );
        std::fs::write(tmp.path().join("moved.rs"), "one\ntwo\nthree\n").unwrap();
        assert!(
            read_identified_bytes(tmp.path(), "moved.rs", &rc.content_sha256, &rc.blob_oid)
                .is_none(),
            "measured or absent: the new bytes are never counted under the old identity"
        );
    }

    #[cfg(unix)]
    #[test]
    fn direct_hook_detection_accepts_only_installed_hook_names() {
        assert_eq!(
            direct_git_hook_name_from(std::ffi::OsStr::new("/repo/.git/hooks/prepare-commit-msg")),
            Some("prepare-commit-msg")
        );
        assert_eq!(
            direct_git_hook_name_from(std::ffi::OsStr::new("post-commit")),
            Some("post-commit")
        );
        assert_eq!(
            direct_git_hook_name_from(std::ffi::OsStr::new("rift")),
            None
        );
        assert_eq!(
            direct_git_hook_name_from(std::ffi::OsStr::new("pre-commit")),
            None
        );
    }

    #[test]
    fn agent_token_is_open_but_shape_validated() {
        // Well-formed first-party and third-party tokens build an AgentId; only the
        // registry decides whether they capture. Malformed tokens are rejected.
        assert!(AgentId::new("claude-code").is_well_formed());
        assert_eq!(AgentId::new("gemini"), AgentId::GEMINI);
        assert!(AgentId::new("acme-agent").is_well_formed());
        assert!(!AgentId::new("../evil").is_well_formed());
    }

    /// The variable's name is now shipped output and documentation, not just a
    /// private constant, so renaming it silently would strand both. Pinned to
    /// its literal for the same reason the queue pins the two values it
    /// publishes — a published name needs one assertion that fails when it
    /// changes.
    #[test]
    fn the_inline_drain_variable_is_named_and_qualified_where_it_ships() {
        assert_eq!(CAPTURE_SYNC_ENV, "RIFT_CAPTURE_SYNC");
        let note = capture_sync_note();
        assert!(note.contains(CAPTURE_SYNC_ENV), "{note}");
        // …and it carries the qualification, not only the name: a reader who
        // meets this variable for the first time must be told what it cannot
        // do, because "sync" reads like an opt-out of something.
        assert!(note.contains("never skips capture or redaction"), "{note}");
    }

    #[test]
    fn flag_extraction() {
        let args: Vec<String> = ["--git-dir", "/x/.git", "--", "a"]
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(flag(&args, "--git-dir"), Some("/x/.git".to_string()));
        assert_eq!(flag(&args, "--missing"), None);
    }

    /// A hermetic `git` invocation inside `dir` — user/system config switched
    /// off and every inherited `GIT_*` re-targeting variable scrubbed, so a
    /// fixture cannot be steered by the developer's environment.
    fn git_in(dir: &Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", dir.join("absent-gitconfig"))
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .env_remove("GIT_COMMON_DIR")
            .env_remove("GIT_OBJECT_DIRECTORY")
            .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
            .output()
            .expect("git is on PATH");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// A real repository with one commit and one **linked worktree**; returns
    /// `(main, linked)`. `core.hooksPath` is an empty directory so the
    /// developer's own installed Rift hooks can never fire inside a fixture.
    fn repo_with_linked_worktree(tmp: &Path) -> (PathBuf, PathBuf) {
        let main = tmp.join("main");
        let nohooks = tmp.join("nohooks");
        std::fs::create_dir_all(&main).unwrap();
        std::fs::create_dir_all(&nohooks).unwrap();
        git_in(&main, &["init", "-q", "."]);
        git_in(&main, &["config", "user.email", "t@t"]);
        git_in(&main, &["config", "user.name", "T"]);
        git_in(
            &main,
            &["config", "core.hooksPath", nohooks.to_str().unwrap()],
        );
        std::fs::write(main.join("a.txt"), "one\n").unwrap();
        git_in(&main, &["add", "a.txt"]);
        git_in(&main, &["commit", "-q", "-m", "seed"]);
        let linked = tmp.join("linked");
        git_in(
            &main,
            &[
                "worktree",
                "add",
                "-q",
                linked.to_str().unwrap(),
                "-b",
                "wt",
            ],
        );
        (main, linked)
    }

    /// A capture projection under `dir`, and the current-thread runtime the
    /// production hook path drives it with.
    fn projection_in(dir: &Path) -> (rift_store::SqliteStore, tokio::runtime::Runtime) {
        let store = rift_store::open_capture_projection(dir.join("projection.db")).unwrap();
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        (store, rt)
    }

    /// Every `session_worktree` row for `session`, oldest first —
    /// `(name, token, branch, basis, first_observed_at)`.
    fn observations(
        rt: &tokio::runtime::Runtime,
        store: &rift_store::SqliteStore,
        session: &str,
    ) -> Vec<(String, String, Option<String>, String, i64)> {
        use rift_store::CanonicalStore as _;
        rt.block_on(store.query(
            "SELECT worktree_name, worktree_token, branch, basis, first_observed_at \
             FROM session_worktree WHERE session_id = ? ORDER BY first_observed_at, observation_id",
            &[rift_store::Value::text(session.to_owned())],
        ))
        .unwrap()
        .iter()
        .map(|r| {
            (
                r.get_text(0).unwrap(),
                r.get_text(1).unwrap(),
                r.get_opt_text(2).unwrap(),
                r.get_text(3).unwrap(),
                r.get_i64(4).unwrap(),
            )
        })
        .collect()
    }

    /// Seed the session cache the ingest would have written, so the memo path
    /// under test is the production one.
    fn seed_cache(git_dir: &Path, session: &str) {
        let cache = rift_shadow::hooks::SessionCache::new(session, "claude-code", "active", 1);
        rift_shadow::hooks::write_cache(git_dir, &cache).unwrap();
    }

    /// The observation is content-addressed over the STATE, so a
    /// hook delivered twice — the ordinary case, and the one the hot-path memo
    /// exists for — is ONE row, not two.
    #[test]
    fn a_redelivered_hook_records_one_observation() {
        let tmp = tempfile::tempdir().unwrap();
        let (_main, linked) = repo_with_linked_worktree(tmp.path());
        let repo = capture_repo(&linked).unwrap();
        let (store, rt) = projection_in(tmp.path());
        seed_cache(&repo.git_dir, "s1");
        let row_id = rift_shadow::ids::session_row_id("claude-code", "s1");

        let first = observe_worktree(&rt, &store, &repo, None, "s1", &row_id, None, 1_000).unwrap();
        assert!(first, "the first observation of a state is a row");

        // Redelivered: the memo answers, no row, no database round trip.
        let again = observe_worktree(&rt, &store, &repo, None, "s1", &row_id, None, 2_000).unwrap();
        assert!(!again, "the same state is not observed twice");

        // And with the memo gone — a deleted cache, or one written before
        // worktree provenance existed — the
        // DATABASE still refuses the duplicate: the memo is speed, the id is
        // correctness.
        let mut cache = rift_shadow::hooks::read_cache(&repo.git_dir, "s1")
            .unwrap()
            .unwrap();
        assert!(
            cache.last_worktree.is_some(),
            "the memo records what landed"
        );
        cache.last_worktree = None;
        rift_shadow::hooks::write_cache(&repo.git_dir, &cache).unwrap();
        assert!(
            !observe_worktree(&rt, &store, &repo, None, "s1", &row_id, None, 3_000).unwrap(),
            "INSERT OR IGNORE on the state's own id"
        );

        let rows = observations(&rt, &store, &row_id);
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].0, "linked", "a member is named by its admin name");
        assert_eq!(rows[0].1, repo.worktree_token());
        assert_eq!(rows[0].2.as_deref(), Some("wt"), "the branch it was on");
        assert_eq!(rows[0].3, "process_cwd", "no payload named a directory");
        assert_eq!(rows[0].4, 1_000, "the FIRST observation's time stands");
    }

    /// The provenance audit claim: one session id can live in several checkouts
    /// — a worktree-isolated subagent runs under the parent's session id —
    /// and the history is the ordered set of its observations, not a last
    /// value that overwrote the others.
    #[test]
    fn a_session_observed_in_two_worktrees_has_two_rows_in_order() {
        let tmp = tempfile::tempdir().unwrap();
        let (main, linked) = repo_with_linked_worktree(tmp.path());
        let (main_repo, member) = (capture_repo(&main).unwrap(), capture_repo(&linked).unwrap());
        let (store, rt) = projection_in(tmp.path());
        let row_id = rift_shadow::ids::session_row_id("claude-code", "s2");
        seed_cache(&main_repo.git_dir, "s2");
        seed_cache(&member.git_dir, "s2");

        assert!(observe_worktree(&rt, &store, &main_repo, None, "s2", &row_id, None, 10).unwrap());
        assert!(observe_worktree(&rt, &store, &member, None, "s2", &row_id, None, 20).unwrap());
        // A subagent's hook in the member is a THIRD observation under the
        // same session: the subagent id is part of the state, so the row that
        // says "this work was a subagent's" cannot be swallowed by the row
        // that says the parent was here.
        assert!(observe_worktree(
            &rt,
            &store,
            &member,
            None,
            "s2",
            &row_id,
            Some("agent-7"),
            30
        )
        .unwrap());

        let rows = observations(&rt, &store, &row_id);
        assert_eq!(rows.len(), 3, "{rows:?}");
        assert_eq!(rows[0].0, "main");
        assert_eq!(rows[1].0, "linked");
        assert_eq!(rows[2].0, "linked");
        assert_ne!(rows[0].1, rows[1].1, "two members, two worktree tokens");
        assert!(
            rows[0].4 < rows[1].4 && rows[1].4 < rows[2].4,
            "the current worktree is the greatest first_observed_at: {rows:?}"
        );
    }

    /// The payload's own directory decides the label when the adapter carries
    /// one — and only while it names a checkout of the SAME family. A payload
    /// pointing at another repository falls back to the discovered capture repo
    /// under `process_cwd`, because filing this family's session under another
    /// family's worktree is the misattributed credit the provenance rules forbid.
    #[test]
    fn a_payload_cwd_labels_its_own_family_and_never_another() {
        let tmp = tempfile::tempdir().unwrap();
        let (main, linked) = repo_with_linked_worktree(tmp.path());
        let main_repo = capture_repo(&main).unwrap();
        let (store, rt) = projection_in(tmp.path());
        seed_cache(&main_repo.git_dir, "s3");
        let row_id = rift_shadow::ids::session_row_id("claude-code", "s3");

        // The hook process stands in the MAIN work tree; the agent says it is
        // working in the member. The member is what the row names.
        let in_member = rift_agent::CwdFact {
            path: linked.to_string_lossy().into_owned(),
            source: rift_agent::CwdSource::PayloadCwd,
        };
        assert!(observe_worktree(
            &rt,
            &store,
            &main_repo,
            Some(&in_member),
            "s3",
            &row_id,
            None,
            10
        )
        .unwrap());
        assert_eq!(observations(&rt, &store, &row_id)[0].0, "linked");
        assert_eq!(observations(&rt, &store, &row_id)[0].3, "payload_cwd");

        // A payload naming a checkout of a DIFFERENT family is not followed.
        let stranger = tmp.path().join("stranger");
        rift_git::test_support::fake_repo(&stranger).unwrap();
        let elsewhere = rift_agent::CwdFact {
            path: stranger.to_string_lossy().into_owned(),
            source: rift_agent::CwdSource::PayloadCwd,
        };
        assert!(observe_worktree(
            &rt,
            &store,
            &main_repo,
            Some(&elsewhere),
            "s3",
            &row_id,
            None,
            20
        )
        .unwrap());
        let rows = observations(&rt, &store, &row_id);
        assert_eq!(rows.len(), 2, "{rows:?}");
        assert_eq!(
            (rows[1].0.as_str(), rows[1].3.as_str()),
            ("main", "process_cwd"),
            "an out-of-family cwd falls back and SAYS it fell back"
        );
    }

    /// The one place the hook path could poison an append-only table with a
    /// value the schema drops on the floor: `basis`. Every token this binary
    /// can write is a member of the closed vocabulary, so the DAO's silent
    /// `INSERT OR IGNORE` arm is unreachable from here — asserted rather than
    /// argued, because "unreachable" is a claim that rots.
    #[test]
    fn the_basis_tokens_the_hook_writes_are_the_ones_the_schema_admits() {
        let admitted = rift_store::schema::SESSION_WORKTREE_BASES;
        for token in [
            rift_agent::CwdSource::PayloadCwd.as_str(),
            rift_agent::CwdSource::WorkspacePath.as_str(),
            "process_cwd",
        ] {
            assert!(admitted.contains(&token), "{token} is not in {admitted:?}");
        }
    }

    #[test]
    fn capture_repo_resolves_a_linked_worktree_to_a_real_gitdir() {
        let tmp = tempfile::tempdir().unwrap();
        let (_main, linked) = repo_with_linked_worktree(tmp.path());

        // The premise, pinned rather than taken on trust: a linked worktree's
        // `.git` is a FILE, so using it as a directory prefix — which is what
        // `<cwd>/.git` did — fails ENOTDIR. ENOTDIR is *not* NotFound, so it
        // travelled up the capture path as an error instead of as "no session
        // yet", and ingest aborted before writing its receipt.
        assert!(linked.join(".git").is_file(), "a linked .git is a file");
        let err = std::fs::create_dir_all(linked.join(".git").join("rift-sessions"))
            .expect_err("a file cannot hold a directory");
        assert_ne!(
            err.kind(),
            std::io::ErrorKind::NotFound,
            "ENOTDIR must not be mistaken for NotFound: {err:?}"
        );

        let repo = capture_repo(&linked).expect("a linked worktree is a repository");
        assert!(repo.git_dir.is_dir(), "resolved gitdir is a real directory");
        assert!(repo
            .git_dir
            .ends_with(Path::new("worktrees").join("linked")));
        assert_eq!(repo.work_tree, linked);
        // And the session dir it implies can actually be created — the whole
        // point of the fix.
        std::fs::create_dir_all(rift_shadow::hooks::session_cache::sessions_dir(
            &repo.git_dir,
        ))
        .expect("session state is writable in a linked worktree");
    }

    #[test]
    fn capture_repo_finds_the_repository_root_from_a_subdirectory() {
        let tmp = tempfile::tempdir().unwrap();
        let (main, linked) = repo_with_linked_worktree(tmp.path());
        // A hook fired from a subdirectory joins its repository rather than
        // minting a private gitdir + projection under the subdirectory's path.
        let deep = linked.join("a").join("b");
        std::fs::create_dir_all(&deep).unwrap();
        assert_eq!(capture_repo(&deep).unwrap().work_tree, linked);
        assert_eq!(
            capture_repo(&deep).unwrap().git_dir,
            capture_repo(&linked).unwrap().git_dir
        );
        let sub = main.join("src");
        std::fs::create_dir_all(&sub).unwrap();
        assert_eq!(capture_repo(&sub).unwrap().work_tree, main);
    }

    #[test]
    fn capture_repo_outside_a_repository_resolves_to_no_repository() {
        // The retired fallback answered `<cwd>/.git` here, and the caller then
        // created it. Both shapes that used to mint a phantom answer `None`
        // now: a directory with no `.git` at all, and one carrying the empty
        // `.git` a previous fallback left behind.
        let tmp = tempfile::tempdir().unwrap();
        let bare = tmp.path().join("not-a-repo");
        std::fs::create_dir_all(&bare).unwrap();
        assert!(capture_repo(&bare).is_none(), "no repository above `bare`");

        let haunted = tmp.path().join("haunted");
        rift_git::test_support::phantom_gitdir(&haunted).unwrap();
        assert!(
            capture_repo(&haunted).is_none(),
            "a directory called `.git` is not a gitdir"
        );
    }

    /// Worktree isolation: two checkouts of one repository
    /// keep separate session state, and share exactly one thing — the hooks.
    #[test]
    fn worktree_isolation_splits_session_state_and_shares_the_hooks_dir() {
        use rift_shadow::hooks::session_cache::sessions_dir;
        let tmp = tempfile::tempdir().unwrap();
        let (main, linked) = repo_with_linked_worktree(tmp.path());
        let (a, b) = (capture_repo(&main).unwrap(), capture_repo(&linked).unwrap());

        // Session state is per-worktree: an agent in one checkout can never
        // read, claim or condense a session live in the other.
        assert_ne!(a.git_dir, b.git_dir);
        assert_ne!(sessions_dir(&a.git_dir), sessions_dir(&b.git_dir));
        assert!(!sessions_dir(&b.git_dir).starts_with(sessions_dir(&a.git_dir)));
        // The projection is the opposite case, and this is where the family
        // model parts company with what this test used to assert: the two members
        // share ONE store, because they are one repository. Session state is
        // a checkout's private progress (the provenance rules keep it split); the facts
        // that state produces belong to the family.
        assert_eq!(
            crate::family::resolve_at(&a.work_tree).unwrap().family_id,
            crate::family::resolve_at(&b.work_tree).unwrap().family_id,
            "two members of one family file their facts in one projection"
        );

        // Hooks are the exception: git resolves `hooks/<name>` through the
        // COMMON dir, so one install covers every worktree — and installing
        // from the linked one must not try to write inside a `.git` file.
        let from_main = resolve_hooks_git_dir(None, &main).unwrap();
        let from_linked = resolve_hooks_git_dir(None, &linked).unwrap();
        assert!(from_linked.is_dir(), "hooks root is a real directory");
        assert_eq!(
            from_main.canonicalize().unwrap(),
            from_linked.canonicalize().unwrap(),
            "both worktrees install into one hooks dir"
        );
        // Canonical, not `…/worktrees/<name>/../..` — these paths are printed.
        assert!(!from_linked.to_string_lossy().contains(".."));
    }

    /// The first level of the injection stamp is a **family** fact: the cheap
    /// `len:mtime` short-circuit that decides whether a
    /// turn even asks the projection a question reads the file the whole family
    /// shares, so a sibling landing facts is visible from every member.
    ///
    /// A fact test, not new behaviour: `projection_stamp` was routed through
    /// [`crate::family::recall_db`] and this is the claim that routing makes.
    /// It is worth stating because the compounding the owner asked for dies
    /// quietly without it — a member whose stamp could not move would keep
    /// short-circuiting, and the family's shared facts would never reach it.
    ///
    /// The calibration arm is what makes it non-vacuous: the retired per-path
    /// derivation gives the two members two different files, so under the old
    /// rule a sibling's write was invisible here **and the test would pass
    /// against a stamp that never moved**.
    #[test]
    fn the_stamp_every_member_reads_moves_when_any_member_lands_facts() {
        use rift_store::{CanonicalStore as _, Value};

        let tmp = tempfile::tempdir().unwrap();
        let (main, linked) = repo_with_linked_worktree(tmp.path());

        // The old rule: two members, two files, and nothing either wrote could
        // be seen by the other.
        assert_ne!(
            rift_store::recall_db_for_repo(&main).unwrap(),
            rift_store::recall_db_for_repo(&linked).unwrap(),
            "the per-path derivation is what the family model replaced; if these agree \
             the arm below proves nothing"
        );

        // Before any facts: one file for the family, absent, and both members
        // say so in the same words.
        let before_main = projection_stamp(&main).expect("a member resolves its family's store");
        let before_linked = projection_stamp(&linked).expect("so does its sibling");
        assert_eq!(
            before_main, before_linked,
            "both members stamp ONE recall projection"
        );

        // The sibling lands a checkpoint, exactly as its own archive would.
        let db = crate::family::recall_db(&linked).unwrap();
        let store = rift_store::open_migrated(&db, &rift_store::recall_migrations()).unwrap();
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(async {
                store
                    .insert_or_ignore(
                        "n_checkpoint",
                        &["node_id", "dialect", "ordinal"],
                        &[Value::text("c1"), Value::text("rift"), Value::Int(1)],
                    )
                    .await
                    .unwrap();
            });
        drop(store);

        let after_main = projection_stamp(&main).unwrap();
        assert_ne!(
            after_main, before_main,
            "a sibling's facts move the stamp the other member reads"
        );
        assert_eq!(
            after_main,
            projection_stamp(&linked).unwrap(),
            "and the two members still read one stamp"
        );
    }

    #[test]
    fn resolve_hooks_git_dir_prefers_existing_explicit() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("repo");
        let gd = rift_git::test_support::fake_repo(&root).unwrap();
        assert_eq!(resolve_hooks_git_dir(Some(gd.clone()), &root), Some(gd));
        // a non-existent explicit dir falls back to the DISCOVERED gitdir
        let fallback = resolve_hooks_git_dir(Some(PathBuf::from("/nope/.git")), &root).unwrap();
        assert!(fallback.ends_with(".git"), "{}", fallback.display());
    }

    /// The retired phantom mint, in the one command left carrying it: outside
    /// every repository `rift hooks install` has nowhere to install, and it
    /// says so instead of creating a `.git` wherever the shell was standing.
    #[test]
    fn resolve_hooks_git_dir_outside_a_repository_mints_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let bare = tmp.path().join("nowhere");
        std::fs::create_dir_all(&bare).unwrap();
        assert_eq!(resolve_hooks_git_dir(None, &bare), None);
        assert!(
            !bare.join(".git").exists(),
            "asking must never mint a gitdir"
        );
        let refusal = hooks_git_dir_or_refusal(Some(PathBuf::from("/nope/.git")), &bare)
            .expect_err("the command refuses rather than inventing a repository");
        assert!(
            refusal.to_string().contains("not inside a git repository"),
            "{refusal}"
        );
        assert!(
            !bare.join(".git").exists(),
            "and refusing still mints nothing"
        );
    }

    /// `core.hooksPath` is the difference between the directory Rift writes and
    /// the directory git runs — and the fixture repository sets one, so this
    /// asserts against a real git answer, not a rehearsal of our own default.
    #[test]
    fn the_effective_hooks_dir_is_the_one_git_runs() {
        let tmp = tempfile::tempdir().unwrap();
        let (main, linked) = repo_with_linked_worktree(tmp.path());
        let nohooks = tmp.path().join("nohooks").canonicalize().unwrap();
        for vantage in [&main, &linked] {
            let resolved = effective_hooks_dir(vantage).expect("git answers");
            assert_eq!(
                resolved.canonicalize().unwrap(),
                nohooks,
                "core.hooksPath decides, from {}",
                vantage.display()
            );
            assert_ne!(
                resolved.canonicalize().unwrap(),
                resolve_hooks_git_dir(None, vantage).unwrap().join("hooks"),
                "and it is NOT the default this install would have assumed"
            );
        }
    }

    /// An in-memory recall store with its migrations applied, and a runtime to
    /// drive the audit appender on: the whole world the audit tests need, and
    /// the only way they reach it.
    struct AuditStore {
        store: rift_store::SqliteStore,
        rt: tokio::runtime::Runtime,
    }

    impl AuditStore {
        fn new() -> Self {
            use rift_store::{apply_migrations, recall_migrations, SqliteStore};
            let store = SqliteStore::open_in_memory().unwrap();
            apply_migrations(&store, &recall_migrations()).unwrap();
            let rt = tokio::runtime::Builder::new_current_thread()
                .build()
                .unwrap();
            Self { store, rt }
        }

        /// Deliver one envelope to the appender, as the hook does.
        fn append(&self, envelope: &[u8], now: i64) {
            self.rt
                .block_on(append_memd_audit(&self.store, envelope, now))
                .unwrap();
        }

        fn count(&self, relation: &str) -> i64 {
            use rift_store::CanonicalStore as _;
            self.rt.block_on(self.store.count(relation)).unwrap()
        }

        fn query(&self, sql: &str) -> Vec<rift_store::Row> {
            use rift_store::CanonicalStore as _;
            self.rt.block_on(self.store.query(sql, &[])).unwrap()
        }
    }

    /// An ExactRetrieved-shaped envelope, exactly as rift-memd emits it.
    fn exact_retrieved_envelope() -> String {
        serde_json::json!({
            "schema": 1,
            "verb": "memd.exact_retrieved",
            "idempotency_key": "k1",
            "payload": {
                "kind": "exact_retrieved",
                "handle": "file:a.rs@sha256:x",
                "reason": "debugging",
                "estimated_tokens_returned": 12
            }
        })
        .to_string()
    }

    /// A refusal envelope carries `target` (not `handle`).
    fn refused_envelope() -> String {
        serde_json::json!({
            "verb": "memd.refused",
            "idempotency_key": "k2",
            "payload": { "kind": "refused", "target": "secrets/.env", "reason": "policy_denied_exact" }
        })
        .to_string()
    }

    #[test]
    fn memd_audit_events_land_in_recall_db_idempotently() {
        let audit = AuditStore::new();
        audit.append(exact_retrieved_envelope().as_bytes(), 100);
        // Re-delivery of the same event is a no-op (content-addressed idempotency).
        audit.append(exact_retrieved_envelope().as_bytes(), 200);
        let rows =
            audit.query("SELECT verb, handle, reason, tokens_returned FROM memd_audit_event");
        assert_eq!(rows.len(), 1, "duplicate handoff must not double-count");
        assert_eq!(rows[0].get_text(0).unwrap(), "memd.exact_retrieved");
        assert_eq!(rows[0].get_text(1).unwrap(), "file:a.rs@sha256:x");
        assert_eq!(rows[0].get_i64(3).unwrap(), 12);

        // A refusal is still audited.
        audit.append(refused_envelope().as_bytes(), 300);
        assert_eq!(audit.count("memd_audit_event"), 2);

        // A malformed envelope defers (no write, no panic).
        audit.append(b"{ not json", 400);
        assert_eq!(audit.count("memd_audit_event"), 2);
        assert_eq!(
            audit.count("memd_call_event"),
            0,
            "legacy envelopes are savings-only"
        );
    }

    /// The legacy trail the test above leaves, delivered in the same order at
    /// the same clocks — the state the schema-2 envelopes below arrive in.
    fn legacy_audit_trail() -> AuditStore {
        let audit = AuditStore::new();
        audit.append(exact_retrieved_envelope().as_bytes(), 100);
        audit.append(exact_retrieved_envelope().as_bytes(), 200);
        audit.append(refused_envelope().as_bytes(), 300);
        audit.append(b"{ not json", 400);
        audit
    }

    /// A schema-2 envelope for one call of `verb`.
    fn v2_envelope(call_id: &str, verb: &str) -> String {
        serde_json::json!({
            "schema": 2,
            "verb": verb,
            "idempotency_key": call_id,
            "call": {
                "call_id": call_id,
                "thread_id": "thread-neutral",
                "tool_name": "rift_read_summary",
                "recorded_at": 500,
                "tokens_returned": 10,
                "tokens_avoided": 90
            },
            "payload": {
                "kind": "summary_served",
                "handle": "summary:neutral",
                "estimated_tokens_returned": 10,
                "estimated_tokens_avoided": 90
            }
        })
        .to_string()
    }

    /// V2 is invocation-addressed: two identical outcomes with different call
    /// ids are two calls, while replaying one envelope is a no-op.
    #[test]
    fn schema_2_calls_are_invocation_addressed_beside_the_legacy_trail() {
        let audit = two_schema_2_calls();
        assert_eq!(audit.count("memd_call_event"), 2);
        assert_eq!(audit.count("memd_audit_event"), 4);
    }

    /// The legacy trail, then two calls of one outcome — the first delivered
    /// twice — in the order the test above asserts over.
    fn two_schema_2_calls() -> AuditStore {
        let audit = legacy_audit_trail();
        let c1 = v2_envelope("call-1", "memd.summary_served");
        let c2 = v2_envelope("call-2", "memd.summary_served");
        audit.append(c1.as_bytes(), 999);
        audit.append(c1.as_bytes(), 999);
        audit.append(c2.as_bytes(), 999);
        audit
    }

    /// A post-decode internal failure counts as a call but never as savings.
    #[test]
    fn a_schema_2_internal_error_is_a_call_and_never_a_saving() {
        let audit = two_schema_2_calls();
        let internal = v2_envelope("call-3", "memd.internal_error");
        audit.append(internal.as_bytes(), 999);
        assert_eq!(audit.count("memd_call_event"), 3);
        assert_eq!(audit.count("memd_audit_event"), 4);
        let row = audit.query(
            "SELECT terminal_verb, recorded_at FROM memd_call_event \
             WHERE call_id = 'call-3'",
        );
        assert_eq!(row[0].get_text(0).unwrap(), "memd.internal_error");
        assert_eq!(row[0].get_i64(1).unwrap(), 500);
    }

    /// The two proper citation columns, populated from the SAME composed
    /// string the audit trail has always carried — the writer parses it
    /// exactly once, here, never at query time.
    #[test]
    fn a_citation_resolution_gets_a_memd_audit_citation_row() {
        let audit = AuditStore::new();
        let key = "a".repeat(64);
        let sha = "b".repeat(64);

        // A resolved citation: the `=file:<path>@sha256:<hash>` suffix rides
        // along, unparsed by this writer — only the head before `=` is asked
        // to be a `CitationTarget`.
        let resolved = serde_json::json!({
            "schema": 1,
            "verb": "memd.exact_retrieved",
            "idempotency_key": "cite-1",
            "payload": {
                "kind": "exact_retrieved",
                "handle": format!("pack:{key}/evidence:3=file:src/a.rs@sha256:{sha}"),
                "reason": "debugging",
                "estimated_tokens_returned": 12
            }
        })
        .to_string();
        audit.append(resolved.as_bytes(), 100);
        let rows = audit
            .query("SELECT pack_key, ordinal FROM memd_audit_citation WHERE audit_id = 'cite-1'");
        assert_eq!(rows.len(), 1, "a resolved citation gets exactly one row");
        assert_eq!(rows[0].get_text(0).unwrap(), key);
        assert_eq!(rows[0].get_i64(1).unwrap(), 3);

        // An UNRESOLVED refusal (unknown key / out-of-range ordinal / dropped
        // entry) carries no `=file:...` suffix at all — still a citation.
        let unresolved = serde_json::json!({
            "verb": "memd.refused",
            "idempotency_key": "cite-2",
            "payload": {
                "kind": "refused",
                "target": format!("pack:{key}/evidence:99"),
                "reason": "policy_denied_exact"
            }
        })
        .to_string();
        audit.append(unresolved.as_bytes(), 200);
        let rows = audit
            .query("SELECT pack_key, ordinal FROM memd_audit_citation WHERE audit_id = 'cite-2'");
        assert_eq!(rows.len(), 1, "an unresolved refusal is still a citation");
        assert_eq!(rows[0].get_i64(1).unwrap(), 99);

        // An ordinary handle (no citation grammar at all) gets no row — the
        // relation is sparse by construction, not by a second condition.
        let plain = serde_json::json!({
            "schema": 1,
            "verb": "memd.exact_retrieved",
            "idempotency_key": "plain-1",
            "payload": {
                "kind": "exact_retrieved",
                "handle": "file:a.rs@sha256:x",
                "reason": "debugging",
                "estimated_tokens_returned": 12
            }
        })
        .to_string();
        audit.append(plain.as_bytes(), 300);
        assert_eq!(
            audit.count("memd_audit_citation"),
            2,
            "a non-citation handle must not grow the companion relation"
        );

        // Re-delivering the SAME resolved citation is a no-op (the append-only
        // set-union posture every other audit relation holds).
        audit.append(resolved.as_bytes(), 400);
        assert_eq!(audit.count("memd_audit_citation"), 2);
    }

    /// A schema-4 envelope writes BOTH the per-call row and
    /// the first-observed-per-channel row, and never touches the historical
    /// `memd_call_event`/`mcp_connection_event` relations it supersedes.
    #[test]
    fn schema_4_writes_the_serve_channel_relations_and_nothing_mcp_era() {
        let audit = AuditStore::new();

        let event = rift_recall_events::MemdEvent::SummaryServed {
            handle: "summary:neutral".into(),
            estimated_tokens_returned: 12,
            estimated_tokens_avoided: 90,
        };
        let envelope = event.to_serve_call_json(
            rift_recall_serve::ServeChannel::SkillCli,
            "call-1",
            "rift_read_summary",
            rift_core::Timestamp::from_millis(123),
        );
        audit.append(envelope.as_bytes(), 999);

        let rows = audit.query(
            "SELECT channel, tool_name, terminal_verb, tokens_returned, tokens_avoided \
             FROM serve_call_event WHERE call_id = 'call-1'",
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].get_text(0).unwrap(), "skill_cli");
        assert_eq!(rows[0].get_text(1).unwrap(), "rift_read_summary");
        assert_eq!(rows[0].get_text(2).unwrap(), "memd.summary_served");
        assert_eq!(rows[0].get_i64(3).unwrap(), 12);
        assert_eq!(rows[0].get_i64(4).unwrap(), 90);

        let session_rows = audit
            .query("SELECT first_served_at FROM serve_session_event WHERE channel = 'skill_cli'");
        assert_eq!(session_rows.len(), 1);
        assert_eq!(session_rows[0].get_i64(0).unwrap(), 123);

        // A second, later call on the SAME channel adds a call row but does
        // NOT move `first_served_at` — the session row names when the
        // channel was first seen, not last.
        let event2 = rift_recall_events::MemdEvent::ManifestServed {
            estimated_tokens_returned: 5,
            estimated_tokens_avoided: 10,
        };
        let envelope2 = event2.to_serve_call_json(
            rift_recall_serve::ServeChannel::SkillCli,
            "call-2",
            "rift_known_state",
            rift_core::Timestamp::from_millis(456),
        );
        audit.append(envelope2.as_bytes(), 999);
        assert_eq!(audit.count("serve_call_event"), 2);
        assert_eq!(audit.count("serve_session_event"), 1);
        let still_first = audit
            .query("SELECT first_served_at FROM serve_session_event WHERE channel = 'skill_cli'");
        assert_eq!(
            still_first[0].get_i64(0).unwrap(),
            123,
            "first-observed must not move"
        );

        // The OTHER channel is independent: a HookInject call opens its own
        // session row without perturbing SkillCli's.
        let envelope3 = event2.to_serve_call_json(
            rift_recall_serve::ServeChannel::HookInject,
            "call-3",
            "hook-pointer",
            rift_core::Timestamp::from_millis(789),
        );
        audit.append(envelope3.as_bytes(), 999);
        assert_eq!(audit.count("serve_session_event"), 2);

        // Nothing here ever touches the historical relations it supersedes.
        assert_eq!(audit.count("memd_call_event"), 0);
        assert_eq!(audit.count("mcp_connection_event"), 0);

        // Replaying the exact same envelope is a no-op on both relations.
        audit.append(envelope.as_bytes(), 999);
        assert_eq!(audit.count("serve_call_event"), 3);
        assert_eq!(audit.count("serve_session_event"), 2);
    }

    /// A REAL `PackBuilt` event — built by the events
    /// crate, not hand-rolled JSON — lands with `tokens_returned > 0` in both
    /// audit relations, through both envelope paths rift-memd emits. Before
    /// the seam, pack rows audited 0 by key-name mismatch (the figure hid in
    /// `total_tokens`); a real refusal likewise carries its own payload cost.
    #[test]
    fn real_serve_events_audit_nonzero_returned_in_both_relations() {
        use rift_recall_events::{
            MemdEvent, Schema2CallContext, Schema2CallId, Schema2Terminal, Schema2ThreadId,
        };
        let audit = AuditStore::new();

        let pack = MemdEvent::PackBuilt {
            sections: 4,
            total_tokens: 500,
            estimated_tokens_returned: 648,
            estimated_tokens_avoided: 2_551_121,
        };
        // v1 path: the content-addressed envelope.
        audit.append(pack.to_hook_json().as_bytes(), 100);
        let rows = audit.query(
            "SELECT tokens_returned, tokens_avoided FROM memd_audit_event \
             WHERE verb = 'memd.pack_built'",
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].get_i64(0).unwrap(),
            648,
            "the seam's measure, not 0"
        );
        assert_eq!(rows[0].get_i64(1).unwrap(), 2_551_121);

        // v2 path: the invocation-addressed terminal envelope carries the SAME
        // figure into memd_call_event (and the legacy savings row).
        let thread = Schema2ThreadId::from_seed(b"process-neutral");
        let call = Schema2CallContext {
            thread_id: thread,
            call_id: Schema2CallId::derive(thread, 1),
            tool_name: "rift_context_pack".into(),
        };
        let v2 = Schema2Terminal::Event(&pack)
            .to_hook_json(&call, rift_core::Timestamp::from_millis(500));
        audit.append(v2.as_bytes(), 999);
        let calls = audit.query(
            "SELECT tokens_returned FROM memd_call_event \
             WHERE terminal_verb = 'memd.pack_built'",
        );
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].get_i64(0).unwrap(), 648);
        assert_eq!(
            audit.count("memd_audit_event"),
            2,
            "the v2 decision also retains a legacy savings row"
        );

        // A real refusal audits its own (small) payload cost, never 0 and
        // never the withheld bytes: no token class is exempt from accounting.
        let refused = MemdEvent::Refused {
            target: "file:secrets/.env@sha256:ab".into(),
            reason: "policy_denied_exact".into(),
            estimated_tokens_returned: 91,
        };
        audit.append(refused.to_hook_json().as_bytes(), 200);
        let refusal =
            audit.query("SELECT tokens_returned FROM memd_audit_event WHERE verb = 'memd.refused'");
        assert_eq!(refusal[0].get_i64(0).unwrap(), 91);
    }

    #[test]
    fn work_tree_resolution_handles_main_and_linked_worktrees() {
        let tmp = tempfile::tempdir().unwrap();
        // Main worktree: GIT_DIR = <repo>/.git, no `gitdir` file → work tree = <repo>.
        let main_git = tmp.path().join("repo").join(".git");
        std::fs::create_dir_all(&main_git).unwrap();
        assert_eq!(
            rift_shadow::hooks::session_cache::work_tree_of(&main_git),
            Some(tmp.path().join("repo"))
        );

        // Linked worktree: GIT_DIR = <repo>/.git/worktrees/feat, whose `gitdir` file
        // points at the linked worktree's `.git` file → work tree is ITS parent, not
        // the main worktree (the bug this guards against).
        let wt_git = main_git.join("worktrees").join("feat");
        std::fs::create_dir_all(&wt_git).unwrap();
        let linked_wt = tmp.path().join("feat-checkout");
        std::fs::write(
            wt_git.join("gitdir"),
            format!("{}\n", linked_wt.join(".git").display()),
        )
        .unwrap();
        assert_eq!(
            rift_shadow::hooks::session_cache::work_tree_of(&wt_git),
            Some(linked_wt)
        );
    }

    fn native_transcript(dir: &Path) -> std::path::PathBuf {
        let transcript = dir.join("session.jsonl");
        let lines = [
            serde_json::json!({"type":"user","message":{"content":"go"}}),
            serde_json::json!({"type":"assistant","isSidechain":false,"message":{"model":"claude-fable-5","usage":{"input_tokens":10,"output_tokens":1}}}),
            serde_json::json!({"type":"assistant","isSidechain":true,"message":{"model":"claude-fable-5","usage":{"input_tokens":900_000,"output_tokens":100_000}}}),
            serde_json::json!({"type":"assistant","isSidechain":false,"message":{"model":"claude-fable-5","usage":{"input_tokens":20,"output_tokens":2,"cache_read_input_tokens":30}}}),
        ]
        .into_iter()
        .map(|value| value.to_string())
        .collect::<Vec<_>>()
        .join("\n");
        std::fs::write(&transcript, lines).unwrap();
        transcript
    }

    fn native_hook(verb: &str, transcript: &Path) -> rift_shadow::hooks::RawHook {
        rift_shadow::hooks::RawHook {
            agent: rift_shadow::hooks::AgentId::CLAUDE_CODE,
            verb: verb.into(),
            session: "native".into(),
            turn: None,
            occurred_at_ms: 1,
            payload: serde_json::json!({"transcript_path": transcript}),
        }
    }

    /// A mid-turn hook carries the nic-gauge's context sample — the freshest
    /// main-chain call, sidechain spend excluded — and never a turn's spend:
    /// spend is the deferred settle's to record, at the Stop's own ids.
    #[test]
    fn pretooluse_enrichment_carries_context_only_and_never_turn_spend() {
        let dir = tempfile::tempdir().unwrap();
        let transcript = native_transcript(dir.path());
        let mut raw = native_hook("PreToolUse", &transcript);
        enrich_native_context(&mut raw);
        assert!(
            raw.payload.get("usage").is_none(),
            "a mid-turn hook never carries turn spend: {}",
            raw.payload
        );
        assert_eq!(
            raw.payload
                .pointer("/context_usage/input_tokens")
                .and_then(serde_json::Value::as_u64),
            Some(20)
        );
        assert_eq!(
            raw.payload
                .pointer("/context_usage/cache_read_input_tokens")
                .and_then(serde_json::Value::as_u64),
            Some(30)
        );
        assert_eq!(
            raw.payload.get("model").and_then(serde_json::Value::as_str),
            Some("claude-fable-5")
        );
    }

    /// The hook-stop budget fix, stated as a test: a `Stop` payload is not
    /// enriched at all, and in particular it does not wait out the transcript
    /// quiet window on a transcript that was written a moment ago.
    #[test]
    fn a_stop_payload_is_not_enriched_and_does_not_wait() {
        let dir = tempfile::tempdir().unwrap();
        let transcript = native_transcript(dir.path());
        let mut raw = native_hook("Stop", &transcript);
        let before = raw.payload.clone();
        let started = std::time::Instant::now();
        enrich_native_context(&mut raw);
        let elapsed = started.elapsed();
        assert_eq!(
            raw.payload, before,
            "a Stop payload is passed through untouched"
        );
        assert!(
            elapsed < std::time::Duration::from_millis(rift_agent::settle::QUIET_WINDOW_MS),
            "a Stop must not settle inline any more: took {elapsed:?}"
        );
    }
}
