//! Which checkout a hook was fired in, and the bytes a read-shaped call
//! resolves to in it.
//!
//! Separate because worktree provenance is one question asked in two places —
//! the observation a session's row carries, and the hash a recorded read is
//! keyed by — and both answers must name the same member of the family.

use std::path::Path;

use rift_core::Result;

use super::owed::search_result_args;
use super::CapturedHook;
use crate::hooks::now_ms;

/// The `basis` word of an observation a recognised shell read's leading `cd`
/// decided — the one word `session_worktree_cd` admits that
/// `session_worktree` cannot.
const SHELL_CD_BASIS: &str = "shell_cd";

/// The capture-note key a refused observation is said under; one key, so a
/// refusal that recurs on every hook is one note, not one per hook.
const REFUSED_NOTE_KEY: &str = "worktree-observation-refused";

/// The worktree-provenance step of one agent hook: read the
/// payload's own facts through the adapter that mapped this hook, and record
/// the observation under the session's OWNER — the label every other
/// post-ingest step is addressed by.
///
/// Best-effort and infallible by signature: a provenance label is never a
/// reason to fail an agent's hook, and the timer keeps its phase either way so
/// the budget instrument sees what this step cost. A row the schema refuses
/// is not silent, though: it is said once on the capture-note channel, which
/// the next commit hook and `rift doctor` report, because a refusal recurs on
/// every hook until the vocabulary and the writer agree again.
pub(super) fn observe_hook_worktree(run: &CapturedHook<'_>, owner: &str) {
    let observed = run.t.phase("observe_worktree", || {
        observe_payload_worktree(run, owner, now_ms())
    });
    if let Err(refused @ rift_core::RiftError::ConstraintRefused { .. }) = observed {
        crate::capture_queue::mark_note(
            &run.repo.git_dir,
            REFUSED_NOTE_KEY,
            &format!("worktree observation not recorded: {refused}"),
        );
    }
}

/// The observation one hook's payload implies, at `now`: its directory, and
/// the shell command it carries when it carries one.
fn observe_payload_worktree(run: &CapturedHook<'_>, owner: &str, now: i64) -> Result<bool> {
    let raw = run.raw;
    let facts = run
        .registry
        .get(&raw.agent)
        .map(|adapter| adapter.facts(&raw.verb, &raw.payload))
        .unwrap_or_default();
    observe_worktree(
        run.rt,
        run.store,
        run.repo,
        facts.cwd.as_ref(),
        shell_command(raw, &facts),
        &raw.session,
        &rift_shadow::ids::session_row_id(owner, &raw.session),
        facts.child_session.as_deref(),
        now,
    )
}

/// The command line of a shell call, read through the family's pinned
/// tool-field row; `None` for every call that is not a shell call.
fn shell_command<'a>(
    raw: &'a rift_shadow::hooks::RawHook,
    facts: &rift_agent::HookFacts,
) -> Option<&'a str> {
    let tool = facts.tool_name.as_deref()?;
    let row = rift_agent::tool_fields::tool_field_row(&raw.agent, tool)?;
    if row.shape != rift_agent::tool_fields::ToolShape::Shell {
        return None;
    }
    search_result_args(row, &raw.payload)?
        .get(row.fact("command").unwrap_or("command"))?
        .as_str()
}

/// Record the `session_worktree` observation this hook implies, when it is a
/// state the session has not been seen in before.
///
/// **Content-addressed over the state, never the time**, so a redelivered hook,
/// a re-run and a rebuild all derive the same `observation_id` and the insert
/// collapses; a session that MOVES — a worktree-isolated subagent is the case
/// this exists for — gets a second row, and the ordered set is the history an
/// auditor reads.
///
/// **Two layers of idempotence, on purpose.** The database layer skips a
/// conflict on the state's own id and is what makes the record correct. The
/// session cache's `last_worktree` is a *memo* in front of it and is what makes
/// it cheap: the overwhelmingly common hook observes exactly what the previous
/// hook observed, and on that path this function reads three small files and
/// touches no database at all. A lost, stale or absent memo costs one redundant
/// no-op insert and never a row — which is why the memo is written only once
/// the row is on record (landed now, or already there), never after a refusal,
/// so a refused observation is tried again by the next identical hook; and why
/// a session with no cache file still records.
///
/// Returns whether a row was written (the tests' oracle; the caller ignores it).
pub(in crate::hooks) fn observe_worktree(
    rt: &tokio::runtime::Runtime,
    store: &rift_store::SqliteStore,
    repo: &rift_git::RepoPaths,
    cwd: Option<&rift_agent::CwdFact>,
    command: Option<&str>,
    native_session: &str,
    session_row_id: &str,
    subagent_id: Option<&str>,
    now: i64,
) -> Result<bool> {
    use rift_shadow::hooks::session_cache;

    let (observed, basis) = observed_worktree(repo, cwd, command);
    let head = rift_git::head_state(&observed);
    let seen = rift_shadow::hooks::WorktreeObservation {
        token: observed.worktree_token(),
        name: observed.member_name(),
        linked: observed.is_linked(),
        branch: head.branch,
        head: head.oid,
        subagent: subagent_id.map(str::to_owned),
    };

    // The memo lives beside the session's own state, in the member's sessions
    // directory the ingest just wrote the cache to — never the OBSERVED
    // worktree's, which may be a different member of the same family.
    let sessions = crate::family::run_state::sessions_dir(&repo.git_dir);
    let cached = session_cache::read_cache(&sessions, native_session)
        .ok()
        .flatten();
    if cached
        .as_ref()
        .is_some_and(|c| c.last_worktree.as_ref() == Some(&seen))
    {
        return Ok(false);
    }

    let state_id = rift_shadow::ids::session_worktree_observation_id(
        session_row_id,
        &seen.token,
        seen.branch.as_deref(),
        seen.head.as_deref(),
        seen.subagent.as_deref(),
    );
    let row = rift_store::SessionWorktreeRow {
        observation_id: rift_shadow::ids::session_worktree_filed_id(&state_id, basis),
        session_id: session_row_id.to_owned(),
        worktree_token: seen.token.clone(),
        worktree_name: seen.name.clone(),
        is_linked: i64::from(seen.linked),
        // A LOCAL ADDRESS, canonicalized so the same checkout observed through
        // `/tmp/x` and `/private/tmp/x` is one path. This column is why the
        // relation is projection-only: it never enters a shard.
        work_tree: canonical_display(&observed.work_tree),
        branch: seen.branch.clone(),
        head_oid: seen.head.clone(),
        subagent_id: seen.subagent.clone(),
        basis: basis.to_owned(),
        first_observed_at: now,
        schema_version: 1,
    };
    // A refusal returns here, before the memo: only a row on record is memoized.
    let wrote = rt.block_on(rift_store::record_session_worktree(store, &row))?;

    // Memoize only what landed. A cache that does not exist is not invented
    // here: the ingest owns that file's existence, and a session it declined to
    // cache is not one this path should conjure state for.
    if let Some(mut cache) = cached {
        cache.last_worktree = Some(seen);
        session_cache::write_cache(&sessions, &cache)?;
    }
    Ok(wrote)
}

/// Which checkout the observation names, and the `basis` token that says how it
/// was decided (`rift_store::schema::SESSION_WORKTREE_BASES`, or `shell_cd`).
///
/// The payload's directory wins when the adapter carries one — it is the agent
/// speaking about itself, while the hook process's cwd is wherever the shell
/// that spawned it happened to be — but only when it resolves to a checkout of
/// **this same family**. A payload naming a directory in another repository (or
/// none at all) falls back to the discovered capture repo with `process_cwd`,
/// because filing one family's session under another family's worktree is the
/// misattributed credit the provenance rules forbid, and a basis token that
/// lied about where the value came from would hide it.
///
/// One thing outranks the payload's directory: a shell read the grammar
/// recognises whose leading `cd` lands in a *different* member of the same
/// family. The payload says where the agent's shell sat, and the `cd` says
/// where it read; the read is the member's, so the observation is too.
fn observed_worktree(
    repo: &rift_git::RepoPaths,
    cwd: Option<&rift_agent::CwdFact>,
    command: Option<&str>,
) -> (rift_git::RepoPaths, &'static str) {
    let claimed = payload_worktree(repo, cwd).unwrap_or_else(|| (repo.clone(), "process_cwd"));
    let Some(member) = shell_cd_member(repo, cwd, command) else {
        return claimed;
    };
    if canonical_display(&member.work_tree) == canonical_display(&claimed.0.work_tree) {
        return claimed;
    }
    (member, SHELL_CD_BASIS)
}

/// The checkout the payload's own directory names, when it is one of this
/// family's.
fn payload_worktree(
    repo: &rift_git::RepoPaths,
    cwd: Option<&rift_agent::CwdFact>,
) -> Option<(rift_git::RepoPaths, &'static str)> {
    let fact = cwd?;
    let found = rift_git::discover(Path::new(&fact.path)).ok()?;
    if !same_family(&found, repo) {
        return None;
    }
    Some((found, fact.source.as_str()))
}

/// The member of this family a recognised shell read's leading `cd` moved to.
///
/// A relative directory is read from the payload's directory, the only place
/// the agent's shell is known to have stood; with no payload directory it
/// names nothing, because the hook process's own directory is not the
/// shell's. The target is canonicalised before discovery, so a symlink or a
/// `..` names the member it resolves to and the worktree token is the one
/// the member's own path gives.
fn shell_cd_member(
    repo: &rift_git::RepoPaths,
    cwd: Option<&rift_agent::CwdFact>,
    command: Option<&str>,
) -> Option<rift_git::RepoPaths> {
    let dir = rift_agent::shell_read::leading_cd(command?)?;
    let dir = Path::new(&dir);
    let target = match cwd {
        Some(fact) => Path::new(&fact.path).join(dir),
        None if dir.is_absolute() => dir.to_path_buf(),
        None => return None,
    };
    let found = rift_git::discover(&std::fs::canonicalize(target).ok()?).ok()?;
    if !same_family(&found, repo) {
        return None;
    }
    Some(found)
}

/// Whether two checkouts share one git common dir — one family.
fn same_family(found: &rift_git::RepoPaths, repo: &rift_git::RepoPaths) -> bool {
    canonical_display(&found.common_dir) == canonical_display(&repo.common_dir)
}

/// A path as an address: canonical where the filesystem can say so, verbatim
/// where it cannot (a checkout removed between the hook and this read).
fn canonical_display(p: &Path) -> String {
    std::fs::canonicalize(p)
        .unwrap_or_else(|_| p.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

/// The worktree read-hasher: resolves a read-shaped tool call's
/// payload path (absolute or repo-relative) to a repo-relative path plus the
/// current bytes' identity, using the SAME hashing the recall plane's
/// blob-oid map uses. A path outside the repo, vanished, or unsafe resolves
/// to no fact — capture never guesses.
pub(in crate::hooks) fn worktree_read_hasher(
    work_tree: &Path,
) -> impl Fn(&str) -> Option<rift_shadow::ReadContent> {
    let hash_root = Some(
        work_tree
            .canonicalize()
            .unwrap_or_else(|_| work_tree.to_path_buf()),
    );
    move |raw_path: &str| -> Option<rift_shadow::ReadContent> {
        let wt = hash_root.as_ref()?;
        let p = Path::new(raw_path);
        // Canonicalize an absolute payload path so symlinked prefixes (macOS
        // `/var` -> `/private/var`) still resolve under the worktree root.
        // Spelled repository-relative with `/` on every platform: this string
        // becomes `ReadContent.rel_path` and part of the content-addressed
        // `tool_call_id`, and every lookup is keyed with `/`.
        let rel_norm = if p.is_absolute() {
            let canon = p.canonicalize().ok()?;
            rift_recall_substrate::worktree::repo_relative_slash(wt, &canon)?
        } else {
            rift_recall_substrate::worktree::repo_relative_slash(Path::new(""), p)?
        };
        let (blob, bytes) = rift_recall_substrate::worktree::hash_blob_bytes(wt, &rel_norm)?;
        // The token measurement is NOT taken here any more, at any
        // size. It used to be, up to 1 MiB, and `hook-read-tool` measured
        // p50 102.39 ms against a 100 ms budget on a 120-line file — because
        // the cost is not the count but the tokenizer: `cl100k_base`
        // initialises in 234 ms (release, this machine) before the first
        // byte, then 7 ms for 5 KiB, 89 ms for 64 KiB, 1.38 s for 1 MiB. No
        // inline cap keeps a hook inside the budget when the first count
        // alone is over it, so the cap is zero and the count is owed to the
        // detached worker (`count_read_tokens_job`), which appends the same
        // `file_read_tokens` row at the same read identity. Only real BPE
        // figures persist ("measured or absent"): the chars/4 fallback never
        // lands a durable number, here or there.
        let _ = bytes;
        Some(rift_shadow::ReadContent {
            rel_path: blob.path,
            blob_oid: blob.blob_oid,
            content_sha256: blob.content_sha256,
            tokens: None,
        })
    }
}

#[cfg(test)]
mod shell_cd_tests {
    use std::path::Path;

    use rift_shadow::hooks::{read_cache, AdapterRegistry, RawHook};
    use rift_store::{CanonicalStore as _, Store as _};

    use super::*;
    use crate::hooks::capture_repo;
    use crate::hooks::tests::{projection_in, repo_with_linked_worktree, seed_cache};
    use crate::hooktime::HookTimer;

    /// A Claude Code shell call, after the fact, as its hook payload spells
    /// one: the agent sat in `cwd` and ran `command`.
    fn shell_hook(cwd: &Path, command: &str) -> RawHook {
        RawHook {
            agent: rift_core::AgentId::new("claude-code"),
            verb: "PostToolUse".to_owned(),
            session: "s-cd".to_owned(),
            turn: Some(1),
            occurred_at_ms: 1_000,
            payload: serde_json::json!({
                "session_id": "s-cd",
                "cwd": cwd.display().to_string(),
                "tool_name": "Bash",
                "tool_input": { "command": command },
            }),
        }
    }

    /// Run the hook's observation step from the main checkout, as the
    /// ingest runs it, and return what the session was filed under:
    /// `(worktree_name, basis)` for every observation, oldest first.
    fn observed_from_main(main: &Path, tmp: &Path, command: &str) -> Vec<(String, String)> {
        let repo = capture_repo(main).unwrap();
        let (store, rt) = projection_in(tmp);
        seed_cache(&repo.git_dir, "s-cd");
        let raw = shell_hook(main, command);
        let timer = crate::hooktime::HookTimer::new("agent-hook", false);
        let registry = AdapterRegistry::with_builtin();
        let run = CapturedHook {
            t: &timer,
            rt: &rt,
            store: &store,
            repo: &repo,
            registry: &registry,
            raw: &raw,
            repo_id: "repo",
        };
        assert!(observe_payload_worktree(&run, "claude-code", 10).unwrap());
        let relation = rt.block_on(rift_store::SessionWorktreeRow::relation(&store));
        rt.block_on(store.query(
            &format!(
                "SELECT worktree_name, basis FROM {relation} \
                 ORDER BY first_observed_at, observation_id"
            ),
            &[],
        ))
        .unwrap()
        .iter()
        .map(|r| (r.get_text(0).unwrap(), r.get_text(1).unwrap()))
        .collect()
    }

    /// A refused observation leaves the hook successful, says itself once on
    /// the capture-note channel, and leaves the memo unwritten, so the next
    /// identical hook tries the row again instead of trusting one that never
    /// landed.
    #[test]
    fn a_refused_observation_is_noted_once_and_never_memoized() {
        let tmp = tempfile::tempdir().unwrap();
        let (main, _linked) = repo_with_linked_worktree(tmp.path());
        let repo = capture_repo(&main).unwrap();
        let (store, rt) = projection_admitting_only_commit();
        seed_cache(&repo.git_dir, "s-cd");
        let (raw, timer) = (
            shell_hook(&main, "true"),
            HookTimer::new("agent-hook", false),
        );
        let registry = AdapterRegistry::with_builtin();
        let run = CapturedHook {
            t: &timer,
            rt: &rt,
            store: &store,
            repo: &repo,
            registry: &registry,
            raw: &raw,
            repo_id: "repo",
        };
        observe_hook_worktree(&run, "claude-code");
        let memo = || {
            read_cache(
                &crate::family::run_state::sessions_dir(&repo.git_dir),
                "s-cd",
            )
            .unwrap()
            .unwrap()
            .last_worktree
        };
        assert_eq!(memo(), None, "a refused row is never memoized");
        let again = observe_payload_worktree(&run, "claude-code", 20);
        assert!(
            matches!(&again, Err(rift_core::RiftError::ConstraintRefused { table, constraint })
                if table == "session_worktree" && constraint == "check(basis)"),
            "the identical hook tries the row again: {again:?}"
        );
        assert_eq!(memo(), None);
        let notes = crate::capture_queue::peek_errors(&repo.git_dir);
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(notes[0].contains("check(basis)"), "{notes:?}");
    }

    /// A projection whose `session_worktree.basis` admits only `commit`: the
    /// shape of a schema whose vocabulary does not hold the hook's word.
    fn projection_admitting_only_commit() -> (rift_store::SqliteStore, tokio::runtime::Runtime) {
        let store = rift_store::SqliteStore::open_in_memory().unwrap();
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let ddl = "CREATE TABLE session_worktree (observation_id TEXT NOT NULL PRIMARY KEY, \
            session_id TEXT NOT NULL, worktree_token TEXT NOT NULL, \
            worktree_name TEXT NOT NULL, is_linked INTEGER NOT NULL, work_tree TEXT NOT NULL, \
            branch TEXT, head_oid TEXT, subagent_id TEXT, \
            basis TEXT NOT NULL CHECK (basis IN ('commit')), \
            first_observed_at INTEGER NOT NULL, schema_version INTEGER NOT NULL) STRICT;";
        rt.block_on(async { store.conn().await.unwrap().execute_batch(ddl).await })
            .unwrap();
        (store, rt)
    }

    /// A path as an agent's POSIX shell spells it: the verbatim prefix
    /// dropped and `/` throughout, since the grammar reads `\` as an escape
    /// and `?` as a glob. `C:/…` is still absolute and canonicalises to the
    /// same directory.
    fn shell_spelling(path: &Path) -> String {
        let spelled = path.display().to_string();
        let plain = spelled.strip_prefix(r"\\?\").unwrap_or(&spelled);
        plain.replace('\\', "/")
    }

    fn filed(name: &str, basis: &str) -> Vec<(String, String)> {
        vec![(name.to_owned(), basis.to_owned())]
    }

    /// The agent sat in main and read a file of the member: the read is
    /// filed under the member, and the basis says a `cd` decided it.
    #[test]
    fn a_read_after_cd_into_a_member_is_filed_under_the_member() {
        let tmp = tempfile::tempdir().unwrap();
        let (main, linked) = repo_with_linked_worktree(tmp.path());
        let command = format!("cd {} && sed -n '1,40p' src/a.rs", shell_spelling(&linked));
        assert_eq!(
            observed_from_main(&main, tmp.path(), &command),
            filed("linked", "shell_cd")
        );
    }

    /// A `cd` into another repository is never followed: that family's
    /// checkout is not this family's member.
    #[test]
    fn a_cd_into_an_unrelated_repository_keeps_the_payload_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let (main, _linked) = repo_with_linked_worktree(tmp.path());
        let stranger = tmp.path().join("stranger");
        rift_git::test_support::fake_repo(&stranger).unwrap();
        let command = format!(
            "cd {} && sed -n '1,40p' src/a.rs",
            shell_spelling(&stranger)
        );
        assert_eq!(
            observed_from_main(&main, tmp.path(), &command),
            filed("main", "payload_cwd")
        );
    }

    /// A command the grammar does not read moves nothing, `cd` or not.
    #[test]
    fn an_unrecognised_command_keeps_the_payload_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let (main, linked) = repo_with_linked_worktree(tmp.path());
        let command = format!("cd {} && cargo test", linked.display());
        assert_eq!(
            observed_from_main(&main, tmp.path(), &command),
            filed("main", "payload_cwd")
        );
    }

    /// A relative `cd` is read from the directory the payload says the agent
    /// sat in.
    #[test]
    fn a_relative_cd_resolves_against_the_payload_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let (main, _linked) = repo_with_linked_worktree(tmp.path());
        assert_eq!(
            observed_from_main(&main, tmp.path(), "cd ../linked && cat a.txt"),
            filed("linked", "shell_cd")
        );
    }

    /// A symlink into the member is the member: the target is canonicalised
    /// before the family is asked about it.
    #[cfg(unix)]
    #[test]
    fn a_symlinked_cd_target_canonicalises_to_the_member() {
        let tmp = tempfile::tempdir().unwrap();
        let (main, linked) = repo_with_linked_worktree(tmp.path());
        let alias = tmp.path().join("alias");
        std::os::unix::fs::symlink(&linked, &alias).unwrap();
        let command = format!("cd {} && head -n 5 a.txt", alias.display());
        assert_eq!(
            observed_from_main(&main, tmp.path(), &command),
            filed("linked", "shell_cd")
        );
    }

    /// The new word goes to the relation that admits it, and its id is not
    /// the bare state id the older relation would have used.
    #[test]
    fn a_cd_observation_id_covers_its_basis() {
        let filed = rift_shadow::ids::session_worktree_filed_id;
        assert_eq!(filed("abc", "payload_cwd"), "abc");
        assert_ne!(filed("abc", SHELL_CD_BASIS), "abc");
        assert!(!rift_store::schema::SESSION_WORKTREE_BASES.contains(&SHELL_CD_BASIS));
    }
}
