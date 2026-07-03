//! `rift hook[s] …` — the git + agent hook entrypoints (P3).
//!
//! Three surfaces:
//! * `rift hook install` — write the six absolute-path git-hook shims.
//! * `rift hooks git <name> --git-dir <dir> -- <hook args…>` — the git lifecycle
//!   entrypoints. Every commit-path hook runs under the **never-block** guard
//!   (`catch_unwind` + `exit(0)`); only `pre-push` may exit non-zero, and only to
//!   reject a `rift/*` ref.
//! * `rift hooks <agent> <verb>` — the stdin-JSON capture contract. It ingests one
//!   hook into the per-repo projection and **never fails the agent** (always exits
//!   0; a capture error is swallowed and the process exits "deferred").

use std::io::Read as _;
use std::path::{Path, PathBuf};

use rift_core::Result;
use rift_shadow::bridge::ingest_hook;
use rift_shadow::hooks::{
    build_raw_hook, install_hooks, schema_json, screen_push, AdapterRegistry, HookInstall,
    PushDecision, PushRef,
};

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
        Some((agent, args)) => run_agent_hook(agent, args),      // diverges
        None => {
            println!(
                "usage: rift hook install | rift hooks git <name> | rift hooks <agent> <verb> | rift hooks --schema"
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

// --- install ----------------------------------------------------------------

/// `rift hook install`: write the absolute-path shims into the repo's hooks dir.
fn cmd_install(args: &[String]) -> Result<()> {
    let git_dir = resolve_git_dir(flag(args, "--git-dir").map(PathBuf::from));
    let hooks_dir = flag(args, "--hooks-dir").map(PathBuf::from);
    let rift_bin = std::env::current_exe()?;
    let written = install_hooks(&HookInstall {
        git_dir,
        rift_bin,
        hooks_dir,
    })?;
    println!("rift: installed {} git hooks", written.len());
    for p in &written {
        println!("  {}", p.display());
    }
    Ok(())
}

// --- git hooks (never-block) ------------------------------------------------

/// `rift hooks git <name> --git-dir <dir> -- <forwarded args…>`. Diverges: the
/// commit-path hooks always exit 0; `pre-push` exits 1 only to reject `rift/*`.
fn run_git_hook(args: &[String]) -> ! {
    let name = args.first().map(String::as_str).unwrap_or_default();
    if name == "pre-push" {
        // The one hook that may reject: read the ref updates from stdin and screen.
        match screen_stdin_push() {
            PushDecision::RejectRiftRefs => {
                eprintln!("rift: refusing to push internal rift/* refs");
                std::process::exit(1);
            }
            PushDecision::Allow => {
                // A push of real refs is the local `on-pr` signal (a PR opens /
                // updates when the branch leaves the machine). Guarded: the
                // archive can defer, the push itself is already allowed.
                let git_dir = flag(args, "--git-dir");
                capture_guarded(|| on_pr_archive(git_dir.as_deref()));
            }
        }
    }
    // All other hooks: run capture work guarded so nothing can abort the git op.
    // The hook's `--git-dir` identifies the target repo (cwd is unreliable — a GUI
    // client may run the shim from anywhere), so capture never touches the wrong tree.
    let git_dir = flag(args, "--git-dir");
    capture_guarded(|| capture_for(name, git_dir.as_deref(), forwarded(args)));
}

/// Read `pre-push` stdin (`<local> <lsha> <remote> <rsha>` lines) and screen it.
fn screen_stdin_push() -> PushDecision {
    let mut buf = String::new();
    if std::io::stdin().read_to_string(&mut buf).is_err() {
        // Could not read the ref list — fail closed (reject) rather than risk leak.
        return PushDecision::RejectRiftRefs;
    }
    let refs: Vec<PushRef> = buf.lines().filter_map(PushRef::parse_line).collect();
    screen_push(&refs)
}

/// The capture body for a commit-path hook. On `post-commit` it runs the live
/// loop — diff-capture (P4 B3) and, when an agent session is live + the backend is
/// provisioned, the real backend archive (P1 B2) — all behind the never-block
/// guard, so a missing backend or a read error defers rather than aborting the
/// developer's `git commit`. A test hook lets the e2e suite verify the guard
/// swallows a panic.
// The injected panic exists precisely to exercise the never-block guard — the one
// place a panic is intentional, and it is always swallowed by `capture_guarded`.
fn capture_for(name: &str, git_dir: Option<&str>, fwd: &[String]) -> Result<()> {
    assert!(
        std::env::var_os("RIFT_HOOK_TEST_PANIC").is_none(),
        "injected capture panic (must be swallowed)"
    );
    match name {
        "post-commit" => capture_post_commit(git_dir)?,
        // Trailer injection (P3 S1): `prepare-commit-msg` appends the Rift
        // trailers when a session is live; `commit-msg` normalizes them
        // idempotently. Both only rewrite the message file — never reject.
        "prepare-commit-msg" => trailer_prepare(git_dir, fwd)?,
        "commit-msg" => trailer_normalize(fwd)?,
        _ => {}
    }
    Ok(())
}

/// The forwarded hook args (everything after the `--` separator).
fn forwarded(args: &[String]) -> &[String] {
    args.iter()
        .position(|a| a == "--")
        .map_or(&[], |i| args.get(i + 1..).unwrap_or(&[]))
}

/// The raw session capture bytes handed to condense. Empty in v0.1 (the wrapped
/// backend owns the shard content); the ONE definition shared by the archive
/// path and the trailer's checkpoint-id prediction, so the predicted id and the
/// archived id can never diverge.
fn session_raw_capture() -> rift_core::SecretBytes {
    rift_core::SecretBytes::new(Vec::new())
}

/// Map the validated config token to the strategy enum.
fn strategy_kind(token: &str) -> rift_shadow::hooks::strategy::StrategyKind {
    use rift_shadow::hooks::strategy::StrategyKind;
    match token {
        "on-pr" => StrategyKind::OnPr,
        "scheduled" => StrategyKind::Scheduled,
        "policy-driven" => StrategyKind::PolicyDriven,
        _ => StrategyKind::ManualCommit,
    }
}

/// Load the effective config for a worktree (secure defaults when absent/broken —
/// a config problem must never block a git op).
fn effective_config(work_tree: &Path) -> rift_config::EffectiveConfig {
    rift_config::EffectiveConfig::load(&work_tree.join(".rift"))
        .or_else(|_| rift_config::EffectiveConfig::secure_default())
        .unwrap_or_else(|_| {
            rift_config::EffectiveConfig::secure_default()
                .unwrap_or_else(|_| unreachable!("secure default config is infallible"))
        })
}

/// `prepare-commit-msg`: inject the `Rift-Checkpoint`/`Rift-Attribution`
/// trailers when a session is live. The checkpoint id is the **predicted**
/// condense id (computable pre-commit; the post-commit archive then makes it
/// true); attribution is the live turn-boundary preview. Never rejects — any
/// failure leaves the message untouched.
fn trailer_prepare(git_dir: Option<&str>, fwd: &[String]) -> Result<()> {
    use rift_shadow::hooks::session_cache::live_sessions;
    use rift_shadow::hooks::trailers::{self, Trailers};

    let Some(msg_file) = fwd.first() else {
        return Ok(());
    };
    // Merge/squash/amend messages come with a `source` arg — leave them alone
    // (the spec's trailer applies to fresh commit messages).
    if fwd.len() > 1 && fwd.get(1).is_some_and(|s| s == "merge" || s == "squash") {
        return Ok(());
    }
    let start = match git_dir.and_then(|gd| work_tree_from_git_dir(Path::new(gd))) {
        Some(work_tree) => work_tree,
        None => std::env::current_dir()?,
    };
    let repo = rift_git::discover(&start)?;
    let Some(sess) = live_sessions(&repo.git_dir)?.into_iter().next() else {
        return Ok(());
    };

    let checkpoint_id =
        rift_shadow::predicted_checkpoint_id(&sess.session_id, &session_raw_capture())?;
    let store = rift_store::open_capture_projection(projection_db_for(&repo.work_tree)?)?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .map_err(rift_core::RiftError::from)?;
    let attribution = rt.block_on(rift_shadow::attribution_preview(
        &store,
        &sess.agent,
        &sess.session_id,
    ))?;

    let message = std::fs::read_to_string(msg_file)?;
    let injected = trailers::inject(
        &message,
        &Trailers {
            checkpoint_id: Some(checkpoint_id),
            attribution,
        },
    );
    if injected != message {
        std::fs::write(msg_file, injected)?;
    }
    Ok(())
}

/// `commit-msg`: normalize Rift trailers idempotently (dedup, canonical order).
/// Never rejects the commit.
fn trailer_normalize(fwd: &[String]) -> Result<()> {
    use rift_shadow::hooks::trailers;
    let Some(msg_file) = fwd.first() else {
        return Ok(());
    };
    let message = std::fs::read_to_string(msg_file)?;
    let normalized = trailers::normalize(&message);
    if normalized != message {
        std::fs::write(msg_file, normalized)?;
    }
    Ok(())
}

/// `pre-push` (after the ref screen allowed the push): the local `on-pr` driver.
/// When the configured base strategy is `on-pr` and a session is live, a push of
/// real refs is the PR-update signal and archives a checkpoint.
fn on_pr_archive(git_dir: Option<&str>) -> Result<()> {
    use rift_shadow::hooks::session_cache::live_sessions;
    use rift_shadow::hooks::strategy::{decide, GitEvent, TriggerCtx};

    let start = match git_dir.and_then(|gd| work_tree_from_git_dir(Path::new(gd))) {
        Some(work_tree) => work_tree,
        None => std::env::current_dir()?,
    };
    let repo = rift_git::discover(&start)?;
    let cfg = effective_config(&repo.work_tree);
    if cfg.checkpoint_strategy() != "on-pr" {
        return Ok(());
    }
    let Some(sess) = live_sessions(&repo.git_dir)?.into_iter().next() else {
        return Ok(());
    };
    let reader = rift_git::GixBlobReader::open(&repo.work_tree)?;
    let Some(commit) = reader.head_hex()? else {
        return Ok(());
    };
    let store = rift_store::open_capture_projection(projection_db_for(&repo.work_tree)?)?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .map_err(rift_core::RiftError::from)?;
    let now = now_ms();
    let last = rt.block_on(rift_store::latest_archive_ms(&store, &sess.session_id))?;
    let decision = decide(
        strategy_kind(cfg.checkpoint_strategy()),
        &TriggerCtx {
            git_event: Some(GitEvent::PrSignal),
            session_state: state_from_token(&sess.state),
            sensitive_path_touched: false,
            clock_ms: now,
            last_archive_ms: last,
            schedule_interval_ms: cfg.schedule_interval_ms(),
        },
    );
    if decision.archive {
        archive_now(
            &rt,
            &store,
            &repo.work_tree,
            &commit,
            &sess.session_id,
            decision,
            now,
        )?;
    }
    Ok(())
}

/// Run one live archive through the routed wrapper backend (a no-op when the
/// backend is not provisioned — capture never blocks the git op).
fn archive_now(
    rt: &tokio::runtime::Runtime,
    store: &rift_store::SqliteStore,
    work_tree: &Path,
    commit: &str,
    session: &str,
    decision: rift_shadow::hooks::strategy::ArchiveDecision,
    now: i64,
) -> Result<()> {
    use rift_shadow::backend::{EntireBackend, NativeBackend, RouteTable, RoutedBackend};
    use rift_shadow::entire::SystemRunner;

    let Some(binary) = provisioned_binary() else {
        tracing::debug!("archive skipped: capture backend not provisioned");
        return Ok(());
    };
    let runner = SystemRunner::new(binary);
    let backend = RoutedBackend::new(
        EntireBackend::new(&runner),
        NativeBackend,
        RouteTable::all_wrapper(),
    );
    let raw = session_raw_capture();
    rt.block_on(rift_shadow::run_decision(
        &backend, store, work_tree, session, &raw, decision, commit, now,
    ))?;
    Ok(())
}

/// The per-repo projection db path for `work_tree` (honors `RIFT_CACHE_DIR`).
fn projection_db_for(work_tree: &std::path::Path) -> Result<PathBuf> {
    let repo = crate::verify::repo_id_for(work_tree)?;
    let root = rift_store::resolve_cache_root(|k| std::env::var(k).ok());
    Ok(rift_store::projection_db_under(&root, &repo))
}

/// Resolve the pinned, verified capture-backend binary if it is provisioned on
/// disk. `None` (not yet set up / hash mismatch) means archive is skipped and only
/// the read-only diff-capture runs. Never downloads, never names the upstream tool.
fn provisioned_binary() -> Option<PathBuf> {
    let pin = rift_shadow::entire::PinnedSubstrate::from_embedded().ok()?;
    let bp = pin.binary_path();
    (bp.exists() && pin.verify_on_disk(&bp).is_ok()).then_some(bp)
}

/// Resolve a worktree root from a hook's `--git-dir`, handling linked worktrees.
///
/// For a linked worktree git sets `GIT_DIR=<repo>/.git/worktrees/<name>`, which
/// holds a `gitdir` file pointing at the worktree's `.git` file — its parent is the
/// worktree root. (Naively taking `git_dir.parent()` would walk into
/// `.git/worktrees` and `discover` up to the *main* worktree, capturing the wrong
/// tree.) For the main worktree there is no `gitdir` file and the parent is correct.
fn work_tree_from_git_dir(git_dir: &Path) -> Option<PathBuf> {
    if let Ok(s) = std::fs::read_to_string(git_dir.join("gitdir")) {
        // `<worktree>/.git` → the worktree root is its parent.
        return Path::new(s.trim()).parent().map(Path::to_path_buf);
    }
    // A bare relative `--git-dir .git` (git's default hook env) has an EMPTY
    // parent — that means "the worktree is the cwd", so let callers fall back.
    match git_dir.parent() {
        Some(p) if !p.as_os_str().is_empty() => Some(p.to_path_buf()),
        _ => None,
    }
}

/// Parse a cached phase token into a [`SessionState`] (default `Idle`).
fn state_from_token(token: &str) -> rift_shadow::hooks::SessionState {
    use rift_shadow::hooks::SessionState;
    match token {
        "active" => SessionState::Active,
        "compacting" => SessionState::Compacting,
        "ending" => SessionState::Ending,
        "ended" => SessionState::Ended,
        _ => SessionState::Idle,
    }
}

/// `post-commit`: capture the committed diff (`C`), and — when a live agent
/// session's configured strategy decides to archive and the backend is
/// provisioned — run the real backend archive, anchoring the per-file
/// attribution to the new checkpoint. The sensitive-path trigger (P3 S4) is
/// evaluated from the configured globs over the commit's changed paths, and
/// composes over any base strategy.
fn capture_post_commit(git_dir: Option<&str>) -> Result<()> {
    use rift_shadow::backend::{EntireBackend, NativeBackend, RouteTable, RoutedBackend};
    use rift_shadow::entire::SystemRunner;
    use rift_shadow::hooks::session_cache::live_sessions;
    use rift_shadow::hooks::strategy::{decide, GitEvent, TriggerCtx};

    // Resolve the target work tree from the hook's `--git-dir` (linked-worktree
    // aware), else cwd. Worktree-safe `discover` then handles the `.git`-file case.
    let start = match git_dir.and_then(|gd| work_tree_from_git_dir(Path::new(gd))) {
        Some(work_tree) => work_tree,
        None => std::env::current_dir()?,
    };
    let repo = rift_git::discover(&start)?;
    // The just-created commit is HEAD. An unborn HEAD (no commit yet) means there is
    // nothing to capture — return before touching the projection.
    let reader = rift_git::GixBlobReader::open(&repo.work_tree)?;
    let Some(commit) = reader.head_hex()? else {
        return Ok(());
    };

    let store = rift_store::open_capture_projection(projection_db_for(&repo.work_tree)?)?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .map_err(rift_core::RiftError::from)?;
    let now = now_ms();

    // Bind any unbound sessions to the local developer identity (best-effort:
    // identity failure must never fail the developer's git operation).
    let _ = rt.block_on(crate::identity::bind_new_sessions(
        &store,
        &repo.work_tree,
        now,
    ));

    // The committed diff, computed once: it feeds the sensitive-path trigger,
    // the turn-boundary attribution replay, and the durable C-diff facts.
    let mut capture = rift_shadow::attribution::capture_commit(&reader, &commit)?;
    let cfg = effective_config(&repo.work_tree);
    let sensitive_path_touched = capture.files.iter().any(|f| cfg.is_sensitive_path(&f.path));

    // A live agent session under the configured strategy may archive now; with
    // the backend provisioned this is the real pinned-binary spawn.
    if let Some(sess) = live_sessions(&repo.git_dir)?.into_iter().next() {
        // Turn-boundary attribution (P4 S2): replay the session's H_i/A_i edit
        // stack through the LIFO engine so committed lines resolve to
        // agent/human instead of all-unknown. Best-effort — a replay failure
        // leaves the honest unknown counts.
        let _ = rt.block_on(rift_shadow::enrich_attribution(
            &store,
            &reader,
            &sess.agent,
            &sess.session_id,
            &mut capture,
        ));
        let last = rt.block_on(rift_store::latest_archive_ms(&store, &sess.session_id))?;
        let decision = decide(
            strategy_kind(cfg.checkpoint_strategy()),
            &TriggerCtx {
                git_event: Some(GitEvent::PostCommit),
                session_state: state_from_token(&sess.state),
                sensitive_path_touched,
                clock_ms: now,
                last_archive_ms: last,
                schedule_interval_ms: cfg.schedule_interval_ms(),
            },
        );
        if decision.archive {
            if let Some(binary) = provisioned_binary() {
                let runner = SystemRunner::new(binary);
                let backend = RoutedBackend::new(
                    EntireBackend::new(&runner),
                    NativeBackend,
                    RouteTable::all_wrapper(),
                );
                let raw = session_raw_capture();
                rt.block_on(rift_shadow::archive_and_capture_with(
                    &capture,
                    &backend,
                    &store,
                    &repo.work_tree,
                    &commit,
                    &sess.session_id,
                    &raw,
                    decision,
                    now,
                ))?;
                return Ok(());
            }
        }
    }

    // No live session or no provisioned backend: still record the durable C-diff
    // facts (file_revision + changed_hunk); attribution waits for a checkpoint.
    rt.block_on(rift_shadow::persist_commit_capture(&store, &capture, None))?;
    Ok(())
}

/// Run capture work but isolate failure: log + swallow any error or panic, then
/// return success to git. Diverges via `exit(0)`.
fn capture_guarded<F: FnOnce() -> Result<()>>(f: F) -> ! {
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
    if let Ok(Err(e)) = outcome {
        // Error variants carry identifiers/reasons only — never key/transcript bytes.
        tracing::warn!(error = %e, "capture deferred");
        eprintln!("rift: capture deferred ({e})");
    }
    std::process::exit(0);
}

// --- agent hooks (never fails the agent) ------------------------------------

/// `rift hooks <agent> <verb>`: ingest one stdin-JSON hook. Diverges: always exit
/// 0 (recorded), or non-zero never — a capture error is swallowed as "deferred".
fn run_agent_hook(agent: &str, args: &[String]) -> ! {
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = try_ingest_agent_hook(agent, args);
    }));
    let _ = outcome;
    std::process::exit(0);
}

/// Best-effort agent-hook ingest; any error is the caller's to swallow.
fn try_ingest_agent_hook(agent: &str, args: &[String]) -> Result<()> {
    let verb = args.first().cloned().unwrap_or_default();
    // Read the untrusted stdin envelope and build the hook **totally** (bounded,
    // no panic). A malformed/oversized payload or agent token defers (no write).
    let mut body = Vec::new();
    std::io::stdin().read_to_end(&mut body)?;
    let Ok(raw) = build_raw_hook(agent, &verb, &body, now_ms()) else {
        return Ok(()); // malformed/oversized/agent token → deferred
    };

    let git_dir = resolve_git_dir(None);
    let store = rift_store::open_capture_projection(projection_db()?)?;
    let repo_id = std::env::current_dir()
        .ok()
        .and_then(|cwd| crate::verify::repo_id_for(&cwd).ok())
        .map(|r| r.to_string())
        .unwrap_or_default();

    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .map_err(rift_core::RiftError::from)?;
    let outcome = rt.block_on(async {
        let out = ingest_hook(
            &raw,
            &AdapterRegistry::with_builtin(),
            &store,
            &git_dir,
            agent,
            &repo_id,
            now_ms(),
        )
        .await?;
        // Bind any newly-appeared sessions to the local developer identity
        // (best-effort: identity failure must never fail the agent's hook).
        if let Ok(cwd) = std::env::current_dir() {
            let _ = crate::identity::bind_new_sessions(&store, &cwd, now_ms()).await;
        }
        Ok::<_, rift_core::RiftError>(out)
    })?;
    // Turn-boundary working-tree snapshots (P4 S2): close the H_i/A_i windows
    // this hook implied. Best-effort — never fails the agent's hook.
    let _ = snapshot_boundaries(&rt, &store, agent, &raw.session, &outcome.boundaries);
    // Post-ingest strategy drivers (P3 S4): the session-end condense effect and
    // the scheduled interval. Best-effort — never fails the agent's hook.
    let _ = drive_post_ingest(
        &rt,
        &store,
        &raw.session,
        agent,
        outcome.state,
        &outcome.effects,
    );
    Ok(())
}

/// Capture the working-tree snapshot for each boundary this hook closed.
fn snapshot_boundaries(
    rt: &tokio::runtime::Runtime,
    store: &rift_store::SqliteStore,
    agent: &str,
    session: &str,
    boundaries: &[(u32, rift_shadow::Boundary)],
) -> Result<()> {
    if boundaries.is_empty() {
        return Ok(());
    }
    let cwd = std::env::current_dir()?;
    let repo = rift_git::discover(&cwd)?;
    let reader = rift_git::GixBlobReader::open(&repo.work_tree)?;
    let head = reader.head_hex()?;
    for (ordinal, boundary) in boundaries {
        rt.block_on(rift_shadow::capture_boundary(
            store,
            &repo,
            &reader,
            head.as_deref(),
            agent,
            session,
            *ordinal,
            *boundary,
            now_ms(),
        ))?;
    }
    Ok(())
}

/// Run the archive work an agent hook may have triggered: the state machine's
/// `RequestCondense` effect (session end), and the `scheduled` strategy's
/// interval check. A missing repo/HEAD/backend defers silently.
fn drive_post_ingest(
    rt: &tokio::runtime::Runtime,
    store: &rift_store::SqliteStore,
    session: &str,
    agent: &str,
    state: rift_shadow::hooks::SessionState,
    effects: &[rift_shadow::hooks::Effect],
) -> Result<()> {
    use rift_shadow::hooks::strategy::{decide, ArchiveDecision, ArchiveReason, TriggerCtx};
    use rift_shadow::hooks::Effect;

    let cwd = std::env::current_dir()?;
    let repo = rift_git::discover(&cwd)?;
    let reader = rift_git::GixBlobReader::open(&repo.work_tree)?;
    let Some(commit) = reader.head_hex()? else {
        return Ok(());
    };
    let cfg = effective_config(&repo.work_tree);
    let base = strategy_kind(cfg.checkpoint_strategy());
    let now = now_ms();

    if effects.contains(&Effect::RequestCondense) {
        // The session ended: condense now (the effect is the state machine's
        // explicit request), then drive Ending → Ended.
        let decision = ArchiveDecision {
            archive: true,
            reason: ArchiveReason::SessionEnd,
            strategy: base,
        };
        archive_now(rt, store, &repo.work_tree, &commit, session, decision, now)?;
        let _ = rt.block_on(rift_shadow::bridge::mark_condense_done(
            store,
            &repo.git_dir,
            session,
            agent,
            now,
        ));
        return Ok(());
    }

    // The scheduled strategy fires from agent activity too — a long session
    // without commits still checkpoints on its interval.
    let last = rt.block_on(rift_store::latest_archive_ms(store, session))?;
    let decision = decide(
        base,
        &TriggerCtx {
            git_event: None,
            session_state: state,
            sensitive_path_touched: false,
            clock_ms: now,
            last_archive_ms: last,
            schedule_interval_ms: cfg.schedule_interval_ms(),
        },
    );
    if decision.archive {
        archive_now(rt, store, &repo.work_tree, &commit, session, decision, now)?;
    }
    Ok(())
}

// --- helpers ----------------------------------------------------------------

/// The value following `name` in `args` (`--git-dir <v>`), if present.
fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

/// Resolve the git dir: the explicit value if it exists, else `<cwd>/.git`.
fn resolve_git_dir(explicit: Option<PathBuf>) -> PathBuf {
    if let Some(p) = explicit {
        if p.exists() {
            return p;
        }
    }
    std::env::current_dir().map_or_else(|_| PathBuf::from(".git"), |c| c.join(".git"))
}

/// The per-repo projection db path for the current working directory.
pub(crate) fn projection_db() -> Result<PathBuf> {
    let cwd = std::env::current_dir()?;
    let repo = crate::verify::repo_id_for(&cwd)?;
    let root = rift_store::resolve_cache_root(|k| std::env::var(k).ok());
    Ok(rift_store::projection_db_under(&root, &repo))
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
    use rift_shadow::hooks::AgentId;

    #[test]
    fn agent_token_is_open_but_shape_validated() {
        // Well-formed first-party and third-party tokens build an AgentId; only the
        // registry decides whether they capture. Malformed tokens are rejected.
        assert!(AgentId::new("claude-code").is_well_formed());
        assert_eq!(AgentId::new("gemini"), AgentId::GEMINI);
        assert!(AgentId::new("acme-agent").is_well_formed());
        assert!(!AgentId::new("../evil").is_well_formed());
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

    #[test]
    fn resolve_git_dir_prefers_existing_explicit() {
        let tmp = tempfile::tempdir().unwrap();
        let gd = tmp.path().join(".git");
        std::fs::create_dir_all(&gd).unwrap();
        assert_eq!(resolve_git_dir(Some(gd.clone())), gd);
        // a non-existent explicit dir falls back to <cwd>/.git
        let fallback = resolve_git_dir(Some(PathBuf::from("/nope/.git")));
        assert!(fallback.ends_with(".git"));
    }

    #[test]
    fn work_tree_resolution_handles_main_and_linked_worktrees() {
        let tmp = tempfile::tempdir().unwrap();
        // Main worktree: GIT_DIR = <repo>/.git, no `gitdir` file → work tree = <repo>.
        let main_git = tmp.path().join("repo").join(".git");
        std::fs::create_dir_all(&main_git).unwrap();
        assert_eq!(
            work_tree_from_git_dir(&main_git),
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
        assert_eq!(work_tree_from_git_dir(&wt_git), Some(linked_wt));
    }
}
