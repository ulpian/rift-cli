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
use std::path::PathBuf;

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
            PushDecision::Allow => std::process::exit(0),
        }
    }
    // All other hooks: run capture work guarded so nothing can abort the git op.
    capture_guarded(|| capture_for(name));
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

/// The (deferred) capture body for a commit-path hook. The heavy archival pipeline
/// (gix diff-capture + condense) lands with the live loop; today this is a safe
/// no-op so the hook proves the never-block contract end-to-end. A test hook lets
/// the e2e suite verify the guard swallows a panic.
// The injected panic exists precisely to exercise the never-block guard — the one
// place a panic is intentional, and it is always swallowed by `capture_guarded`.
fn capture_for(_name: &str) -> Result<()> {
    assert!(
        std::env::var_os("RIFT_HOOK_TEST_PANIC").is_none(),
        "injected capture panic (must be swallowed)"
    );
    Ok(())
}

/// Run capture work but isolate failure: log + swallow any error or panic, then
/// return success to git. Diverges via `exit(0)`.
fn capture_guarded<F: FnOnce() -> Result<()>>(f: F) -> ! {
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
    if let Ok(Err(e)) = outcome {
        // Error variants carry identifiers/reasons only — never key/transcript bytes.
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
    let db = projection_db()?;
    if let Some(parent) = db.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let store = rift_store::SqliteStore::open(&db)?;
    store.create_canonical_schema()?;

    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .map_err(rift_core::RiftError::from)?;
    rt.block_on(async {
        ingest_hook(
            &raw,
            &AdapterRegistry::with_builtin(),
            &store,
            &git_dir,
            agent,
            now_ms(),
        )
        .await
    })?;
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
}
