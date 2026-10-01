//! The builders every subject of the read gate's tests reaches for: a hook as
//! the gate reads one, a checkout with a file in it, the levers resolved for a
//! family, the gate run against them, and the readers that look at what a
//! decision left behind.

use std::path::{Path, PathBuf};

use rift_agent::window_journal::Line;
use rift_shadow::hooks::{AdapterRegistry, RawHook};

use crate::hooks::gate::{read_gate, GateOutcome};

/// A hook as the gate reads one: the family, the verb, the window it
/// belongs to and the payload it carried.
pub(super) fn hook(agent: &str, verb: &str, session: &str, payload: serde_json::Value) -> RawHook {
    RawHook {
        agent: rift_core::AgentId::new(agent),
        verb: verb.to_owned(),
        session: session.to_owned(),
        turn: Some(4),
        occurred_at_ms: 1_700_000_000_000,
        payload,
    }
}

/// The same hook as a family whose native shape carries no turn sends it.
///
/// Every alpha family is this shape: the ordinal is not on the wire, and
/// the gate has to find it somewhere that costs no store.
pub(super) fn native_hook(
    agent: &str,
    verb: &str,
    session: &str,
    payload: serde_json::Value,
) -> RawHook {
    RawHook {
        turn: None,
        ..hook(agent, verb, session, payload)
    }
}

/// Open turn `ordinal` for `session` the way a prompt submission does:
/// through the transient session cache the ingest layer keeps beside the
/// git dir, which is the one place the gate may read a turn from.
pub(super) fn open_turn(work_tree: &Path, session: &str, ordinal: u32) {
    let git_dir = work_tree.join(".git");
    let mut cache = rift_shadow::hooks::session_cache::SessionCache::new(
        session,
        "claude-code",
        "active",
        1_700_000_000_000,
    );
    cache.current_turn_ordinal = ordinal;
    cache.turn_open = true;
    rift_shadow::hooks::session_cache::write_cache(&git_dir, &cache)
        .expect("the boundary is recorded");
}

/// A `Read` of `path`, as the pinned fields spell one.
pub(super) fn read_call(path: &Path, offset: Option<u32>, limit: Option<u32>) -> serde_json::Value {
    let mut input = serde_json::Map::new();
    input.insert(
        "file_path".to_owned(),
        serde_json::json!(path.display().to_string()),
    );
    if let Some(offset) = offset {
        input.insert("offset".to_owned(), serde_json::json!(offset));
    }
    if let Some(limit) = limit {
        input.insert("limit".to_owned(), serde_json::json!(limit));
    }
    serde_json::json!({ "tool_name": "Read", "tool_input": input })
}

/// A file of `lines` numbered lines.
pub(super) fn numbered(lines: u32) -> String {
    (1..=lines).fold(String::new(), |mut body, n| {
        use std::fmt::Write as _;
        let _ = writeln!(body, "line {n}");
        body
    })
}

/// A work tree with one file of `lines` numbered lines in it.
pub(super) fn checkout(lines: u32) -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().expect("a work tree");
    let file = tmp.path().join("src.rs");
    std::fs::write(&file, numbered(lines)).expect("the file");
    (tmp, file)
}

/// Another file of `lines` numbered lines beside the first.
pub(super) fn beside(work_tree: &Path, name: &str, lines: u32) -> PathBuf {
    let file = work_tree.join(name);
    std::fs::write(&file, numbered(lines)).expect("the file");
    file
}

pub(super) fn lines_of(work_tree: &Path, session: &str, window: Option<&str>) -> Vec<Line> {
    crate::window_journal::WindowJournal::open(work_tree, session, window)
        .load()
        .journal
        .lines()
        .to_vec()
}

pub(super) fn lines_journal(
    work_tree: &Path,
    session: &str,
) -> rift_agent::window_journal::Journal {
    crate::window_journal::WindowJournal::open(work_tree, session, None)
        .load()
        .journal
}

/// The levers as this repository ships them, resolved for one family:
/// observe everywhere, with the defaults. Built through the policy crate's
/// own `resolve` rather than by hand, so a test cannot grant itself a mode
/// the resolver would have capped.
pub(super) fn levers_for(
    family: &str,
    mode: rift_recall_policy::Mode,
) -> rift_recall_policy::Effective {
    let mut policy = rift_recall_policy::ServePolicy::default();
    policy.interception.mode = mode;
    // The line floor is a policy trade with its own tests, and these
    // fixtures are a handful of lines each: held at one so a walk here
    // exercises the gate it is written about rather than the floor.
    policy.interception.min_slice_lines = 1;
    // Named, not repository-wide: a family the measured caveats hold back
    // is reached only by being named, and a test that skipped the naming
    // would be testing the caveat rather than the lever.
    policy.interception.agents = rift_recall_policy::AgentModes {
        claude_code: (family == "claude-code").then_some(mode),
        codex: (family == "codex").then_some(mode),
        antigravity: (family == "antigravity").then_some(mode),
    };
    rift_recall_policy::resolve(
        &policy,
        family,
        crate::recall::family_capability(&rift_core::AgentId::new(family)),
    )
}

pub(super) fn observing() -> rift_recall_policy::Effective {
    levers_for("claude-code", rift_recall_policy::Mode::Observe)
}

pub(super) fn enforcing() -> rift_recall_policy::Effective {
    levers_for("claude-code", rift_recall_policy::Mode::Enforce)
}

pub(super) fn gate_observing(raw: &RawHook, work_tree: &Path) -> GateOutcome {
    gate_under(raw, work_tree, &observing())
}

pub(super) fn gate_under(
    raw: &RawHook,
    work_tree: &Path,
    effective: &rift_recall_policy::Effective,
) -> GateOutcome {
    read_gate(
        &AdapterRegistry::with_builtin(),
        raw,
        &work_tree.join(".git"),
        work_tree,
        effective,
    )
}

// ------------------------------------------------- the document gate ---

/// The accounting rows a gate left the worker, in the order it wrote them.
pub(super) fn audits(work_tree: &Path) -> Vec<crate::agent_queue::ServeAudit> {
    journaled(work_tree)
        .into_iter()
        .filter_map(|job| match job.kind {
            crate::agent_queue::JobKind::ServeAudit(audit) => Some(audit),
            _ => None,
        })
        .collect()
}

/// The journaled jobs a work tree's queue holds, in listing order.
pub(super) fn journaled(work_tree: &Path) -> Vec<crate::agent_queue::AgentJob> {
    crate::agent_queue::jobs(&work_tree.join(".git"))
        .into_iter()
        .map(|(_, job)| job)
        .collect()
}

/// The revision a file of this checkout is at, the way the gate reads it.
pub(super) fn revision(work_tree: &Path, rel: &str) -> rift_core::Sha256Id {
    let (blob, _) = rift_recall_substrate::worktree::hash_blob_bytes(work_tree, rel)
        .expect("the file is there");
    rift_core::Sha256Id::parse_hex(&blob.content_sha256).expect("a revision")
}

/// A read of the whole file, as a client asks for one.
pub(super) fn whole(path: &Path) -> serde_json::Value {
    read_call(path, Some(1), Some(2_000))
}

/// A command line, as the family that has no read tool at all writes one.
pub(super) fn shell_of(command: &str) -> serde_json::Value {
    serde_json::json!({
        "tool_name": "Bash",
        "tool_input": { "command": command },
    })
}

/// A path as a POSIX command line spells it: the verbatim prefix dropped
/// and `/` throughout. The shell-read grammar refuses `\` (a shell
/// metacharacter) and `?` (a glob) by design, so a Windows `display()`
/// would never be a read; `C:/Users/…` is still absolute to `Path` and
/// canonicalises to the same file.
pub(super) fn shell_spelling(path: &Path) -> String {
    let spelled = path.display().to_string();
    let plain = spelled.strip_prefix(r"\\?\").unwrap_or(&spelled);
    plain.replace('\\', "/")
}

/// Failure context for one absolute tempfile command, without document bytes.
#[cfg(feature = "doc")]
pub(super) fn absolute_shell_diagnostics(
    work_tree: &Path,
    document: &Path,
    command: &str,
) -> String {
    let recognized = rift_agent::shell_read::recognise(command);
    let canonical_root = std::fs::canonicalize(work_tree);
    let canonical_document = std::fs::canonicalize(document);
    let paths: Vec<_> = recognized
        .as_ref()
        .map(rift_agent::shell_read::ShellCall::read_requests)
        .unwrap_or_default()
        .into_iter()
        .map(|request| {
            let path = Path::new(&request.path);
            let resolved = if path.is_absolute() {
                path.to_path_buf()
            } else {
                work_tree.join(path)
            };
            let canonical = std::fs::canonicalize(&resolved);
            let inside = canonical.as_ref().ok().zip(canonical_root.as_ref().ok())
                .map(|(path, root)| path.strip_prefix(root));
            format!(
                "recognized_path={:?} absolute={} resolved={resolved:?} canonical={canonical:?} containment={inside:?}",
                request.path,
                path.is_absolute(),
            )
        })
        .collect();
    format!(
        "TEMP={:?} TMP={:?} TMPDIR={:?} temp_dir={:?}\nwork_tree={work_tree:?} document={document:?}\ncanonical_root={canonical_root:?} canonical_document={canonical_document:?}\nshell_spelling={:?} command={command:?}\nrecognized={recognized:?}\npaths={paths:?}",
        std::env::var_os("TEMP"),
        std::env::var_os("TMP"),
        std::env::var_os("TMPDIR"),
        std::env::temp_dir(),
        shell_spelling(document),
    )
}
