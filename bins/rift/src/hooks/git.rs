//! `rift hooks git <name>`: the git lifecycle entrypoints, every one under the
//! never-block guard (`catch_unwind` + `exit(0)`; only `pre-push` may exit
//! non-zero, and only to reject a `rift/*` ref). Separate because this is the
//! code on git's critical path: it captures, queues and returns, and hands the
//! expensive work to the detached worker.

use std::io::Read as _;
use std::path::Path;

use rift_core::Result;
use rift_shadow::hooks::{screen_push, PushDecision, PushRef};

use super::archive::{archive_now, read_facts_for_archive, record_pack_notes};
use super::worker::{drain_capture_queue, owe_document_sweep, owe_search_index};
use super::{current_dir_or_dot, flag, now_ms, projection_db_at};

/// `rift hooks git <name> --git-dir <dir> -- <forwarded args…>`. Diverges: the
/// commit-path hooks always exit 0; `pre-push` exits 1 only to reject `rift/*`.
pub(super) fn run_git_hook(args: &[String]) -> ! {
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
                let fwd = forwarded(args).to_vec();
                capture_guarded(|| {
                    let out = on_pr_archive(git_dir.as_deref());
                    // The push-queue drain rides the same guard: it must never
                    // decide whether the developer's push proceeds.
                    drain_for_push(git_dir.as_deref(), &fwd);
                    out
                });
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
/// loop — diff-capture and, when an agent session is live, the native
/// checkpoint archive — all behind the never-block guard, so a read error defers
/// rather than aborting the developer's `git commit`. A test hook lets the e2e
/// suite verify the guard swallows a panic.
// The injected panic exists precisely to exercise the never-block guard — the one
// place a panic is intentional, and it is always swallowed by `capture_guarded`.
fn capture_for(name: &str, git_dir: Option<&str>, fwd: &[String]) -> Result<()> {
    assert!(
        std::env::var_os("RIFT_HOOK_TEST_PANIC").is_none(),
        "injected capture panic (must be swallowed)"
    );
    match name {
        "post-commit" => capture_post_commit(git_dir)?,
        // Trailer injection: `prepare-commit-msg` appends the Rift
        // trailers when a session is live; `commit-msg` normalizes them
        // idempotently. Both only rewrite the message file — never reject.
        "prepare-commit-msg" => trailer_prepare(git_dir, fwd)?,
        "commit-msg" => trailer_normalize(fwd)?,
        // The adoption hook, the one that runs when git creates a worktree.
        // `git_dir` is deliberately not passed:
        // on the checkout that matters — `git worktree add` — the forwarded
        // value is a relative `.git` that names a *file*, and the handler
        // resolves its member from the cwd instead.
        "post-checkout" => post_checkout_adopt(fwd)?,
        _ => {}
    }
    Ok(())
}

/// The member-local file whose presence means "already wired": the native
/// installer's ownership manifest, which every adoption writes and refreshes.
const PROVISIONED_MANIFEST: &str = ".rift/state/native-integrations.json";

/// `post-checkout` — the one moment git tells Rift a worktree was born, and it
/// tells it from inside the new tree.
///
/// **Measured on git 2.50.1 (2026-08-30):** on `git worktree add` the hook runs
/// with cwd = the new worktree and `GIT_DIR` **unset**, so the Unix-only
/// `run_direct_git_hook` forwards a relative `--git-dir .git` — which in a
/// linked member is a `.git` *file*, not a directory. This handler therefore
/// resolves its member from `discover(cwd)` and never from the forwarded value,
/// and the admin dir its note lands in comes from that same discovery. A
/// handler that trusted `--git-dir` here would write into a path under a file.
///
/// The shape of the fast path is the 100 ms git-latency budget, spelled out: `$3` is
/// read before anything touches the disk, each exit after it costs `discover`
/// plus a `stat`, and nothing on it opens a projection, spawns `git`, or asks
/// for the effective hooks directory. What remains — one rift process spawn
/// plus discovery — is *not* free; it is published as the `member-switch`
/// harness row rather than asserted (a review corrected the "one `stat`" of
/// the first draft: the real cost is a spawn plus discovery).
///
/// A member the family has nothing to wire (no agent enabled) is deliberately
/// **not** given a marker: an empty intent is a success with nothing to do, and
/// a marker there would read as a refusal to `rift worktree adopt` and to the
/// doctor leg. That member re-reads the projection on each branch switch —
/// visible in the row, well inside the budget, and honest.
///
/// Best-effort by contract: a failure writes the adoption marker (adoption's
/// own act) plus a note in the member's capture queue, and `capture_guarded`
/// turns even a panic into exit 0. A hook that failed somebody's `git worktree
/// add` would be worse than a member that is not wired yet.
fn post_checkout_adopt(fwd: &[String]) -> Result<()> {
    let t = crate::hooktime::HookTimer::from_env("post-checkout");
    // `<previous HEAD> <new HEAD> <1 = branch checkout | 0 = file checkout>`.
    // A file checkout created no worktree; this is the one exit that costs not
    // even a `stat`.
    if fwd.get(2).map(String::as_str) != Some("1") {
        return Ok(());
    }
    let Ok(paths) = t.phase("discover", || rift_git::discover(&current_dir_or_dot())) else {
        // Outside any repository, Rift records nothing and mints nothing — the
        // rule that retired the phantom gitdirs an earlier design left behind.
        return Ok(());
    };
    // The main work tree is what `rift init` adopts, and it is where most
    // checkouts happen: this is the exit an ordinary `git switch` takes.
    if !paths.is_linked() {
        return Ok(());
    }
    let member = paths.work_tree.clone();
    if member.join(PROVISIONED_MANIFEST).is_file() {
        return Ok(());
    }
    // A member whose adoption has already been refused must not pay for the
    // whole of it on every branch switch to fail the same way — a review
    // found it did.
    // `rift worktree adopt` and `rift doctor --fix` are what clear the marker.
    if crate::adopt::marker_reason(&member).is_some() {
        return Ok(());
    }
    let machine = crate::adopt::machine_for(&paths)?;
    let intent = t.phase("intent", || {
        crate::family::projection_db(&member).map_or_else(
            |error| crate::adopt::Intent::Unknown(error.to_string()),
            |db| crate::adopt::enabled_agents(&db),
        )
    });
    let report = t.phase("adopt", || {
        crate::adopt::adopt_member(&member, &machine, &intent, now_ms())
    });
    if !report.is_clean() {
        // Said out loud on the channel a hook has: the next sweep prints it,
        // and `rift doctor` names the member. The admin dir is the DISCOVERED
        // one — a queue under the forwarded `.git` file could not be created.
        //
        // The act comes before the diagnosis because a note is truncated at 300
        // characters, and the sentence that has to survive is the one the
        // reader can do something about. The member is not named: the queue it
        // is written into is that member's own.
        crate::capture_queue::mark_note(
            &paths.git_dir,
            "worktree-adoption",
            &format!(
                "post-checkout did not wire this worktree — `rift worktree adopt` \
                 or `rift doctor --fix` retries it. {}",
                report.summary()
            ),
        );
    }
    Ok(())
}

/// The forwarded hook args (everything after the `--` separator).
fn forwarded(args: &[String]) -> &[String] {
    args.iter()
        .position(|a| a == "--")
        .map_or(&[], |i| args.get(i + 1..).unwrap_or(&[]))
}

/// The raw session capture bytes handed to condense: the working tree's
/// opt-in inclusion document, and empty when the repository has no
/// `.worktreeinclude` — which is every repository that existed before that
/// control, so their checkpoints keep condensing byte-identically. This is what
/// the native archive carries.
///
/// The ONE definition shared by the archive path and the trailer's
/// checkpoint-id prediction, so the two hooks can never disagree about *what*
/// they measured — only about *when* they looked. That sharing is why it takes
/// a work tree rather than reading the process cwd: `prepare-commit-msg` and
/// `post-commit` must answer from the same tree, and a git hook's cwd is not
/// guaranteed to be either.
///
/// **The trailer is a claim, not the landed id.** The prediction is
/// transcript-blind by design — `prepare-commit-msg` cannot detach and owns
/// the 100 ms git budget — so on a
/// transcript-carrying session the trailer names an id that never lands. The
/// read side resolves through the permanent union of what the commit CLAIMS
/// and what the projection RECORDED (`checkpoint_commit`, the shard's own
/// anchor); "predicted id == landed id" is retired as a property anywhere it
/// was written.
///
/// Raw and local by type: [`rift_core::SecretBytes`] in, and the only durable
/// exit is `condense`'s `redact` call.
pub(super) fn session_raw_capture(work_tree: &Path) -> rift_core::SecretBytes {
    rift_shadow::inclusion::capture_document(work_tree)
}

/// Map the validated config token to the strategy enum.
///
/// `"import"` is deliberately absent: it is the import marker `rift import`
/// stamps on the read-only checkpoints it lands, not a capture strategy, and
/// nothing can be configured to archive under it
/// (`rift_config::CHECKPOINT_STRATEGIES` does not carry it, so a config that
/// names it is refused at load).
pub(super) fn strategy_kind(token: &str) -> rift_shadow::hooks::strategy::StrategyKind {
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
pub(crate) fn effective_config(work_tree: &Path) -> rift_config::EffectiveConfig {
    rift_config::EffectiveConfig::load(&work_tree.join(".rift"))
        .or_else(|_| rift_config::EffectiveConfig::secure_default())
        .unwrap_or_else(|_| {
            rift_config::EffectiveConfig::secure_default()
                .unwrap_or_else(|_| unreachable!("secure default config is infallible"))
        })
}

/// `prepare-commit-msg`: inject the `Rift-Checkpoint`/`Rift-Attribution`
/// trailers when a session is live. The checkpoint id is the **predicted**
/// condense id — a claim computable pre-commit, exact for transcript-less
/// sessions and one transcript stale otherwise (decision (b), RULED
/// 2026-08-29: the trailer/anchor union on the read side is the permanent
/// answer, never a heavier hook); attribution is the live turn-boundary
/// preview. Never rejects — any failure leaves the message untouched.
fn trailer_prepare(git_dir: Option<&str>, fwd: &[String]) -> Result<()> {
    use rift_shadow::hooks::session_cache::live_sessions_for;
    use rift_shadow::hooks::trailers::{self, Trailers};

    let t = crate::hooktime::HookTimer::from_env("prepare-commit-msg");
    let Some(msg_file) = fwd.first() else {
        return Ok(());
    };
    // Merge/squash/amend messages come with a `source` arg — leave them alone
    // (the spec's trailer applies to fresh commit messages).
    if fwd.len() > 1 && fwd.get(1).is_some_and(|s| s == "merge" || s == "squash") {
        return Ok(());
    }
    let start = match git_dir
        .and_then(|gd| rift_shadow::hooks::session_cache::work_tree_of(Path::new(gd)))
    {
        Some(work_tree) => work_tree,
        None => std::env::current_dir()?,
    };
    let repo = t.phase("discover", || rift_git::discover(&start))?;
    let Some(sess) = t
        .phase("live_sessions", || {
            live_sessions_for(&repo.git_dir, &repo.common_dir, &repo.work_tree)
        })?
        .into_iter()
        .next()
    else {
        return Ok(());
    };

    let store = t.phase("open_projection", || {
        rift_store::open_capture_projection(projection_db_at(&repo.work_tree)?)
    })?;
    let rt = t.phase("tokio_build", || {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .map_err(rift_core::RiftError::from)
    })?;
    // The prediction folds in the read facts captured so far, exactly as the
    // post-commit archive will — same query, same digest.
    let facts = t.phase("read_facts", || {
        read_facts_for_archive(&rt, &store, &sess.agent, &sess.session_id)
    });
    let raw = t.phase("raw_capture", || session_raw_capture(&repo.work_tree));
    let (packs, notes) = t.phase("rule_packs", || {
        crate::redactor_packs::capture_rule_packs(&repo.work_tree)
    });
    // Noticed here, said by `post-commit`'s sweep a moment later. Printing here
    // would work — this hook has git's terminal — but it would print on every
    // commit forever, and the queue already owns a channel that says a thing
    // once and stops the moment it stops being true.
    record_pack_notes(&repo.git_dir, &notes);
    // `redact` is NESTED inside `predict_id`, and it is the instrument's first
    // nested phase, so two things about reading it:
    //
    // * `HookTimer::phase` records on COMPLETION, so `redact` prints BEFORE its
    //   parent in `report_line`;
    // * `predict_id` therefore INCLUDES `redact`. `report_line`'s own doc invites
    //   a harness to sum by key — summing every phase here double-counts the
    //   redaction. Subtract it, or read left to right.
    //
    // It exists because the 100 ms budget has never been measured at any opted-in
    // `.worktreeinclude` size and "the cost is in `predict_id`" is not something
    // anyone can act on. The split lives in `rift-shadow` beside the single id
    // implementation, so this times the real redaction rather than a second one:
    // redacting again to time it would double the dominant cost of the one hook
    // that cannot detach, inside the very budget being measured.
    let checkpoint_id = t.phase("predict_id", || {
        let redacted = t.phase("redact", || rift_shadow::predict_redaction(&raw, &packs))?;
        rift_shadow::predicted_checkpoint_id_from(&sess.session_id, redacted, &facts)
    })?;
    let attribution = t.phase("attribution_preview", || {
        rt.block_on(rift_shadow::attribution_preview(
            &store,
            &sess.agent,
            &sess.session_id,
        ))
    })?;

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
/// The `pre-push` half of the push queue: ship queued
/// checkpoint refs **only** when the remote being pushed to is the one this
/// repository configured for them.
///
/// `fwd` is git's own `pre-push` argv — `<remote-name> <remote-url>` — so the
/// decision is made against the destination git is actually contacting, not
/// against a guess. Every other remote drains nothing and prints nothing at
/// all: a developer pushing their code to their own remote must not discover
/// that Rift also sent a capture ledger somewhere, and must not even be told
/// about a queue they did not ask about.
///
/// Total and silent by construction — it returns `()`, so no failure here can
/// change the hook's exit status. A remote that is down, a queue that is
/// locked, a repository that cannot be discovered: all of them leave the work
/// queued for `rift sync flush`.
fn drain_for_push(git_dir: Option<&str>, fwd: &[String]) {
    let (Some(remote), Some(url)) = (fwd.first(), fwd.get(1)) else {
        return;
    };
    let Some(start) = git_dir
        .and_then(|gd| rift_shadow::hooks::session_cache::work_tree_of(Path::new(gd)))
        .or_else(|| std::env::current_dir().ok())
    else {
        return;
    };
    let Ok(repo) = rift_git::discover(&start) else {
        return;
    };
    let Ok(Some(target)) = crate::push_queue::ShipTarget::resolve(&repo) else {
        return;
    };
    if !target.matches_push(remote, url) {
        return;
    }
    let name = target.name.clone();
    let shipper = crate::push_queue::GitPushShipper::new(&repo, target);
    // The queue belongs to the whole repository family, so a `pre-push` from any member
    // ships what any member enqueued — the case that shipped nothing before,
    // because a linked worktree's admin dir held no queue at all. Legacy
    // admin-dir queues are adopted first, once, on the way past.
    let _ = crate::push_queue::adopt_legacy(&repo);
    match crate::push_queue::drain(&repo.common_dir, &shipper, now_ms()) {
        Ok(report) if !report.shipped.is_empty() => {
            // stderr, never stdout: a hook's stdout belongs to git.
            eprintln!(
                "rift: shipped {} checkpoint ref(s) to '{name}'",
                report.shipped.len()
            );
        }
        _ => {}
    }
}

fn on_pr_archive(git_dir: Option<&str>) -> Result<()> {
    use rift_shadow::hooks::session_cache::live_sessions_for;
    use rift_shadow::hooks::strategy::{decide, GitEvent, TriggerCtx};

    let start = match git_dir
        .and_then(|gd| rift_shadow::hooks::session_cache::work_tree_of(Path::new(gd)))
    {
        Some(work_tree) => work_tree,
        None => std::env::current_dir()?,
    };
    let repo = rift_git::discover(&start)?;
    let cfg = effective_config(&repo.work_tree);
    if cfg.checkpoint_strategy() != "on-pr" {
        return Ok(());
    }
    let Some(sess) = live_sessions_for(&repo.git_dir, &repo.common_dir, &repo.work_tree)?
        .into_iter()
        .next()
    else {
        return Ok(());
    };
    let reader = rift_git::GixBlobReader::open(&repo.work_tree)?;
    let Some(commit) = reader.head_hex()? else {
        return Ok(());
    };
    let store = rift_store::open_capture_projection(projection_db_at(&repo.work_tree)?)?;
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
        // Nothing here turns on whether the backend was available: a strategy
        // archive that defers simply did not happen, and the next signal asks
        // again.
        let _checkpoint = archive_now(
            &rt,
            &store,
            &repo.work_tree,
            &commit,
            &sess.session_id,
            &sess.agent,
            decision,
            now,
        )?;
    }
    Ok(())
}

/// Parse a cached phase token into a `SessionState` (default `Idle`).
pub(super) fn state_from_token(token: &str) -> rift_shadow::hooks::SessionState {
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
/// attribution to the new checkpoint. The sensitive-path trigger is
/// evaluated from the configured globs over the commit's changed paths, and
/// composes over any base strategy.
fn capture_post_commit(git_dir: Option<&str>) -> Result<()> {
    use rift_shadow::hooks::session_cache::live_sessions_for;

    // Resolve the target work tree from the hook's `--git-dir` (linked-worktree
    // aware), else cwd. Worktree-safe `discover` then handles the `.git`-file case.
    let t = crate::hooktime::HookTimer::from_env("post-commit");
    let start = match git_dir
        .and_then(|gd| rift_shadow::hooks::session_cache::work_tree_of(Path::new(gd)))
    {
        Some(work_tree) => work_tree,
        None => std::env::current_dir()?,
    };
    let repo = t.phase("discover", || rift_git::discover(&start))?;
    // The just-created commit is HEAD. An unborn HEAD (no commit yet) means there is
    // nothing to capture — return before queueing anything.
    let reader = t.phase("gix_open", || {
        rift_git::GixBlobReader::open(&repo.work_tree)
    })?;
    let Some(commit) = t.phase("head_hex", || reader.head_hex())? else {
        return Ok(());
    };
    let sess = t
        .phase("live_sessions", || {
            live_sessions_for(&repo.git_dir, &repo.common_dir, &repo.work_tree)
        })?
        .into_iter()
        .next();

    // Everything above is what the WORKER cannot re-derive for itself: which
    // commit anchored this hook, and which session was live at the instant it
    // ran. Everything below the queue write — the diff, the attribution replay,
    // the strategy decision, the condense and the nine `git` child processes the
    // archive spawns — is deferred, because it measured 103-204 ms and the
    // budget is 100 ms of git latency for all three of a commit's hooks.
    let entry = crate::capture_queue::QueueEntry {
        schema_version: 1,
        commit,
        work_tree: repo.work_tree.clone(),
        session_id: sess.as_ref().map(|s| s.session_id.clone()),
        agent: sess.as_ref().map(|s| s.agent.clone()),
        session_state: sess.as_ref().map(|s| s.state.clone()),
        queued_at_ms: now_ms(),
        attempts: 0,
    };
    t.phase("enqueue", || {
        crate::capture_queue::enqueue(&repo.git_dir, &entry)
    })?;
    // A commit is the other moment the set of documents can have changed, and
    // the worker below is already being dispatched for the condense. Best
    // effort by construction: a pass that cannot be journaled is a saving not
    // taken, never a commit that failed.
    t.phase("owe_doc_sweep", || {
        owe_document_sweep(&repo.git_dir, &repo.work_tree)
    });
    // And the other thing a commit changes: which paths exist. Same shape,
    // same best effort — an index not asked for is a walk the next cold query
    // pays for, never a commit that failed.
    t.phase("owe_search_index", || {
        owe_search_index(&repo.git_dir, &repo.work_tree)
    });

    // A failed condense must stay LOUD — a silent condense failure once cost
    // weeks of unnoticed capture loss. Detaching
    // moves the failure off the commit that caused it, so the marker the worker
    // left is named HERE, one commit later, rather than nowhere.
    for reason in crate::capture_queue::take_errors(&repo.git_dir) {
        eprintln!("rift: {reason}");
    }

    t.phase("spawn_worker", || {
        if std::env::var_os(CAPTURE_SYNC_ENV).is_some() {
            // Drain here instead of detaching. For a caller that must observe
            // the checkpoint before it returns — an end-to-end test, or a CI
            // run that has nowhere to put a background child — since with the
            // real spawn the shard lands after this process has already exited.
            let _ = drain_capture_queue(repo.git_dir.to_str());
        } else {
            spawn_capture_worker(&repo.git_dir);
        }
    });
    Ok(())
}

/// Set (to any value) to run the condense **inline** in `post-commit` instead
/// of detaching it to a background child.
///
/// It ships, so it is documented here and named by `rift hooks install` rather
/// than living only in this file — a review found it shipped as an
/// environment variable that changes hook behaviour and appeared nowhere a
/// user would look.
///
/// **What it can do: make the hook slower.** The inline drain does the
/// condense — the diff, the attribution replay, the archive's git children —
/// inside git's own latency instead of after it, which is exactly the 103-204 ms
/// the detach exists to move off the 100 ms budget.
///
/// **What it cannot do: skip anything.** The queue entry is written *above* the
/// branch, unconditionally, so capture's durable record of intent does not
/// depend on this variable at all; and both arms then run the same
/// [`drain_capture_queue`], so the condense, the redaction gate and the archive
/// are the same code either way. The variable chooses **who** drains and
/// **when**, never **whether**. Both arms are covered end to end: every other
/// case in `hooks_e2e` sets it (for determinism), and
/// `the_detached_worker_archives_after_the_hook_has_already_returned` takes the
/// default path and asserts the same shard and the same drained queue.
///
/// Intended for a caller that must observe the checkpoint before it returns —
/// an end-to-end test, or a CI run with nowhere to put a background child.
pub(super) const CAPTURE_SYNC_ENV: &str = "RIFT_CAPTURE_SYNC";

/// The line `rift hooks install` prints about [`CAPTURE_SYNC_ENV`], built from
/// the constant the hook actually reads so the two cannot drift apart.
pub(super) fn capture_sync_note() -> String {
    format!(
        "the condense runs detached; set {CAPTURE_SYNC_ENV}=1 to run it inline \
         (slower — it never skips capture or redaction)"
    )
}

/// Start the detached capture worker and do **not** wait for it.
///
/// The hook returns to git immediately and then `exit(0)`s, so the child is
/// orphaned and reaped by init — no zombie, and nothing for git to wait on.
/// Its stdio is `/dev/null` so it can never write over the terminal git is
/// using. It stays in the caller's process group: a Ctrl-C during a long
/// condense kills it, which is a supported outcome — the entry stays queued and
/// the next commit's worker takes it. (Detaching further would need `setsid`
/// through `pre_exec`, which is `unsafe`, and this workspace forbids unsafe.)
///
/// A spawn failure is silent by design: the entry is already durable, so the
/// work is deferred to the next hook, never lost.
pub(super) fn spawn_capture_worker(git_dir: &Path) {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    // A Unix hook may have entered through `.git/hooks/post-commit`, which is
    // a direct symlink to Rift. On macOS `current_exe()` preserves that entry
    // path. Spawning it with `hooks worker ...` would therefore re-enter the
    // argv0 hook dispatcher as another `post-commit`, ignore the worker args,
    // and leave the durable queue entry untouched. Resolve the installed link
    // before spawning the detached CLI subcommand; fall back only if the
    // executable disappeared between lookup and resolution.
    let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
    let _ = std::process::Command::new(exe)
        .arg("hooks")
        .arg("worker")
        .arg("--git-dir")
        .arg(git_dir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

/// Run capture work but isolate failure: log + swallow any error or panic, then
/// return success to git. Diverges via `exit(0)`.
pub(super) fn capture_guarded<F: FnOnce() -> Result<()>>(f: F) -> ! {
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
    if let Ok(Err(e)) = outcome {
        // Error variants carry identifiers/reasons only — never key/transcript bytes.
        tracing::warn!(error = %e, "capture deferred");
        eprintln!("rift: capture deferred ({e})");
    }
    std::process::exit(0);
}
