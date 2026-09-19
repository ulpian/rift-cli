//! The read facts a hook ingest records: what one read is, at the row it
//! lives on; the read a command line performs where a family has no read
//! tool; and how a shell read's path becomes an address the caller's hasher
//! can resolve.

use rift_shadow_hooks::{CanonicalEvent, EventKind, HookFacts};

use super::IngestCtx;
use crate::project::ReadContent;

/// One read fact a hook ingest recorded, at the row it lives on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedRead {
    /// The read's content-addressed `tool_call.tool_call_id`
    /// (`ids::read_call_row_id`).
    pub tool_call_id: String,
    /// Repo-relative path of the bytes read.
    pub rel_path: String,
    /// Git blob oid of the bytes at read time.
    pub blob_oid: String,
    /// SHA-256 (hex) of the bytes at read time.
    pub content_sha256: String,
    /// Whether a `file_read_tokens` row landed with the fact (the hasher
    /// measured inline) — `false` means the count is still owed.
    pub tokens_recorded: bool,
    /// The 1-based inclusive line range the call asked for, when the call
    /// *named* one — a command line does, a read tool's structured arguments
    /// are left alone. `None` means the whole file, which is what the count
    /// has always been for a structured read.
    pub lines: Option<(u32, u32)>,
    /// The event schema version the fact was recorded under.
    pub schema_version: i64,
}

/// The read a command line performed, as `(address, lines)`, where the
/// structured fact is absent.
///
/// A family whose read is a *command line* leaves `facts()` nothing to
/// report: Codex has no read tool at all, so `cat`, `head -n N` and
/// `sed -n 'A,Bp'` are how its context window fills, and every one of those
/// reads was invisible to the projection — which is the denominator every
/// savings share divides by. The authority consulted here is the same
/// recogniser that already classified this exact call for the read gate,
/// and it is consulted only where the structured fact is absent, so a read
/// tool's fact keeps the whole-file count it has always had.
///
/// Asked only where a read fact could land — a `tool_result` in this hook's
/// fan-out — because resolving one costs the caller a file read and a hash,
/// and the call that is only *about* to run has nothing to record yet.
///
/// A recognised command whose lines are not derivable *from the request
/// alone* — a `grep`, a decorated whole-file print, a `head` or `tail`
/// relying on a default nothing pins, and a counted `tail`, whose line
/// numbers are a function of a length this layer never learns — is a read
/// whose range nobody here can name, and an unnameable range is not booked
/// as a read at all. The read gate resolves the counted `tail` because it
/// has the bytes in hand for its own hash; this path has only a hashing
/// callback that answers with an identity and not a line count, so it
/// keeps the answer it always gave. The direction is the recogniser's own:
/// a missed read costs a saving, an invented one costs a claim.
///
/// The first read the line performs, likewise, and not all of them: this
/// fact is one path and one range on one tool call, and a second would
/// need a second row.
pub(super) fn commanded_read(
    ctx: &IngestCtx<'_>,
    normalized: &HookFacts,
    events: &[CanonicalEvent],
) -> Option<(String, (u32, u32))> {
    let lands_a_result = events.iter().any(|ev| ev.kind == EventKind::ToolResult);
    if normalized.read_path.is_some() || !lands_a_result {
        return None;
    }
    let request = ctx
        .registry
        .get(&ctx.raw.agent)?
        .read_request(&ctx.raw.verb, &ctx.raw.payload)?;
    let lines = request.conservative_lines()?;
    // …and spelled as an address the caller's hasher can resolve, which is
    // not the same thing as the address the shell read it as.
    let cwd = normalized.cwd.as_ref().map(|fact| fact.path.as_str());
    Some((shell_read_address(request.path, cwd), lines))
}

/// One read fact at the row the projector put it on.
pub(super) fn recorded_read(
    owner: &str,
    ev: &CanonicalEvent,
    content: &ReadContent,
    lines: Option<(u32, u32)>,
) -> RecordedRead {
    RecordedRead {
        tool_call_id: crate::ids::read_call_row_id(
            &crate::ids::session_row_id(owner, &ev.session),
            ev.turn_ordinal,
            &content.rel_path,
            &content.content_sha256,
        ),
        rel_path: content.rel_path.clone(),
        blob_oid: content.blob_oid.clone(),
        content_sha256: content.content_sha256.clone(),
        tokens_recorded: content.tokens.is_some(),
        lines,
        schema_version: i64::from(ev.schema_version),
    }
}

/// A command line's path, as an address the read hasher can resolve.
///
/// A shell read spells its path the way the agent's shell read it: relative to
/// the agent's working directory, which is not necessarily the work-tree root
/// the hasher resolves a relative path against. Joining the two here is what
/// stops a `cat mod.rs` run in a subdirectory from being recorded against a
/// same-named file at the root — a fact about a file nobody read, which is
/// worse than no fact at all. An absolute path is already an address, and a
/// payload that names no directory leaves the path exactly as it was rather
/// than having one guessed for it.
fn shell_read_address(path: String, cwd: Option<&str>) -> String {
    let relative = std::path::Path::new(&path);
    if relative.is_absolute() {
        return path;
    }
    // Joined textually with `/`, not with `Path::join`: the address goes to
    // the read hasher, which keys the ledger with `/` on every platform, and
    // the platform joiner would put a `\` into it on Windows.
    match cwd {
        Some(dir) => format!("{}/{path}", dir.trim_end_matches(['/', '\\'])),
        None => path,
    }
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::{raw, store};
    use super::super::{ingest_hook, IngestOutcome};
    use super::*;
    use rift_shadow_hooks::{AdapterRegistry, AgentId};
    use rift_store::CanonicalStore;

    /// The outcome names every read fact this hook recorded, at the row the
    /// projector put it on, and says whether its count is still owed — what
    /// the hook path journals for the detached worker.
    #[tokio::test]
    async fn the_outcome_carries_the_reads_row_id_and_whether_a_count_is_owed() {
        let s = store();
        let reg = AdapterRegistry::with_builtin();
        let tmp = tempfile::tempdir().unwrap();
        let mut read = raw("PostToolUse", 1);
        read.payload =
            serde_json::json!({"tool_name": "Read", "tool_input": {"file_path": "src/lib.rs"}});
        let hasher = |path: &str| {
            Some(crate::project::ReadContent {
                rel_path: path.to_owned(),
                blob_oid: "oid-1".into(),
                content_sha256: "aa11".into(),
                tokens: None,
            })
        };
        let out = ingest_hook(
            &read,
            &reg,
            &s,
            tmp.path(),
            "claude-code",
            "repo-1",
            10,
            Some(&hasher),
        )
        .await
        .unwrap();
        let session_row = crate::ids::session_row_id("claude-code", "01JSESSION");
        assert_eq!(
            out.read_calls,
            vec![RecordedRead {
                tool_call_id: crate::ids::read_call_row_id(&session_row, 1, "src/lib.rs", "aa11"),
                rel_path: "src/lib.rs".into(),
                blob_oid: "oid-1".into(),
                content_sha256: "aa11".into(),
                tokens_recorded: false,
                lines: None,
                schema_version: 1,
            }]
        );
        let rows = s
            .query("SELECT tool_call_id FROM tool_call", &[])
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].get_text(0).unwrap(), out.read_calls[0].tool_call_id);

        // A hook with no read fact owes nothing.
        let out = ingest_hook(
            &raw("PreToolUse", 1),
            &reg,
            &s,
            tmp.path(),
            "claude-code",
            "repo-1",
            11,
            Some(&hasher),
        )
        .await
        .unwrap();
        assert!(out.read_calls.is_empty());
    }

    /// A Codex session's reads are command lines: the family has no read tool
    /// at all, so every `cat` it ran was a file's worth of tokens the
    /// projection never saw and the savings denominator never counted. A
    /// command the pinned grammar recognises lands the same read fact a read
    /// tool's does — same row shape, same owed count — and the tool call it
    /// hangs on names the shell, which is what tells the two apart later
    /// without a column that does not exist.
    #[tokio::test]
    async fn a_recognised_shell_read_lands_as_a_read_fact_with_its_tokens() {
        let s = store();
        let reg = AdapterRegistry::with_builtin();
        let tmp = tempfile::tempdir().unwrap();
        let mut read = raw("PostToolUse", 1);
        read.agent = AgentId::CODEX;
        read.payload = serde_json::json!({
            "tool_name": "Bash",
            "tool_input": {"command": "cat src/lib.rs"},
        });
        let hasher = |path: &str| {
            Some(crate::project::ReadContent {
                rel_path: path.to_owned(),
                blob_oid: "oid-7".into(),
                content_sha256: "cc77".into(),
                tokens: None,
            })
        };
        let out = ingest_hook(
            &read,
            &reg,
            &s,
            tmp.path(),
            "codex",
            "repo-1",
            10,
            Some(&hasher),
        )
        .await
        .unwrap();
        let session_row = crate::ids::session_row_id("codex", "01JSESSION");
        assert_eq!(
            out.read_calls,
            vec![RecordedRead {
                tool_call_id: crate::ids::read_call_row_id(&session_row, 1, "src/lib.rs", "cc77"),
                rel_path: "src/lib.rs".into(),
                blob_oid: "oid-7".into(),
                content_sha256: "cc77".into(),
                tokens_recorded: false,
                // A bare `cat` reaches the end of the file, so the range the
                // worker prices clamps to the file itself (`HiKind::END_OF_FILE`).
                lines: Some((1, u32::MAX)),
                schema_version: 1,
            }]
        );
        let rows = s
            .query(
                "SELECT tc.tool_name FROM tool_call tc \
                 JOIN tool_reads_file trf ON trf.tool_call_id = tc.tool_call_id",
                &[],
            )
            .await
            .unwrap();
        assert_eq!(rows.len(), 1, "the read fact links to its own tool call");
        assert_eq!(
            rows[0].get_text(0).unwrap(),
            "Bash",
            "the existing tool_name column is what separates a shell read from a read tool"
        );
    }

    /// The range the command named travels with the fact, so the count the
    /// worker owes is the file's tokens over *those* lines. `sed -n '40,80p'`
    /// asked for forty-one lines; pricing it at the whole file would put
    /// tokens in the denominator that never reached a context window.
    #[tokio::test]
    async fn a_ranged_shell_read_carries_its_lines_to_the_count() {
        let s = store();
        let reg = AdapterRegistry::with_builtin();
        let tmp = tempfile::tempdir().unwrap();
        let mut read = raw("PostToolUse", 1);
        read.agent = AgentId::CODEX;
        read.payload = serde_json::json!({
            "tool_name": "Bash",
            "tool_input": {"command": "sed -n '40,80p' src/lib.rs"},
        });
        let hasher = |path: &str| {
            Some(crate::project::ReadContent {
                rel_path: path.to_owned(),
                blob_oid: "oid-8".into(),
                content_sha256: "dd88".into(),
                tokens: None,
            })
        };
        let out = ingest_hook(
            &read,
            &reg,
            &s,
            tmp.path(),
            "codex",
            "repo-1",
            10,
            Some(&hasher),
        )
        .await
        .unwrap();
        assert_eq!(out.read_calls.len(), 1);
        assert_eq!(out.read_calls[0].lines, Some((40, 80)));
    }

    /// A command line names its path the way its shell read it — relative to
    /// the agent's directory, which need not be the work-tree root the
    /// hasher resolves against. The payload's own directory is what closes
    /// that gap; without one the path is left alone rather than guessed at.
    #[tokio::test]
    async fn a_shell_read_is_addressed_from_the_directory_the_payload_names() {
        let reg = AdapterRegistry::with_builtin();
        let seen = std::sync::Mutex::new(Vec::<String>::new());
        let hasher = |path: &str| {
            seen.lock().ok()?.push(path.to_owned());
            Some(crate::project::ReadContent {
                rel_path: "crates/a/src/mod.rs".into(),
                blob_oid: "oid-3".into(),
                content_sha256: "ff33".into(),
                tokens: None,
            })
        };
        let s = store();
        let tmp = tempfile::tempdir().unwrap();
        let mut read = raw("PostToolUse", 1);
        read.agent = AgentId::CODEX;
        read.payload = serde_json::json!({
            "tool_name": "Bash",
            "cwd": "/repo/crates/a/src",
            "tool_input": {"command": "cat mod.rs"},
        });
        ingest_hook(
            &read,
            &reg,
            &s,
            tmp.path(),
            "codex",
            "repo-1",
            10,
            Some(&hasher),
        )
        .await
        .unwrap();
        assert_eq!(
            seen.lock().unwrap().as_slice(),
            ["/repo/crates/a/src/mod.rs"],
            "a bare name in a subdirectory is not a file at the root"
        );
    }

    /// Everything the grammar does not spell leaves no fact at all, and so
    /// does everything it spells as a read whose lines this layer cannot name.
    /// A member that is not a read makes the whole line a small program whose
    /// effect nobody here can read; a `grep` prints what matched; a bare
    /// `head` relies on a default nothing pins; and a counted `tail` names its
    /// lines only against a length this layer never learns, because the
    /// hashing callback it has answers with an identity and not a line count.
    /// The direction is deliberate: an uncounted read makes the savings share
    /// look larger than it is, which is a shortfall someone can find, where an
    /// invented one is a claim nobody can check.
    #[tokio::test]
    async fn a_command_whose_lines_are_not_derivable_leaves_no_read_fact() {
        for command in [
            "cat src/lib.rs && touch done",
            "cat src/lib.rs | grep needle",
            "tail -n 5 src/lib.rs",
            "head src/lib.rs",
            "grep needle src/lib.rs",
            // A line that only labels its own output read nothing to book.
            "echo done",
        ] {
            let (store, out) = ingest_command(command).await;
            assert!(
                out.read_calls.is_empty(),
                "`{command}` is outside the grammar or names no derivable range"
            );
            assert_eq!(
                store.count("tool_reads_file").await.unwrap(),
                0,
                "{command}"
            );
        }
    }

    /// A command line that reads several files leaves **one** fact, for the
    /// first of them.
    ///
    /// This row is one path and one range on one tool call, and a second read
    /// would need a second row — which is a schema question, not a recognition
    /// one. The read gate is where every member of such a line is accounted
    /// for; here the first is recorded and the rest cost a saving, which is
    /// the same direction every other absence on this path takes.
    #[tokio::test]
    async fn a_command_line_of_several_reads_books_the_first_of_them() {
        for (command, path, lines) in [
            ("cat src/lib.rs | head -n 20", "src/lib.rs", (1, 20)),
            ("cat src/a.rs src/b.rs", "src/a.rs", (1, u32::MAX)),
            (
                "cd crates && sed -n '4,9p' a.rs && cat b.rs",
                "crates/a.rs",
                (4, 9),
            ),
            // A label between the reads is not one of them.
            (
                "cd crates && echo \"=== a ===\" && sed -n '4,9p' a.rs && echo && cat b.rs",
                "crates/a.rs",
                (4, 9),
            ),
        ] {
            let (store, out) = ingest_command(command).await;
            assert_eq!(out.read_calls.len(), 1, "`{command}` books one read");
            assert_eq!(out.read_calls[0].rel_path, path, "{command}");
            assert_eq!(out.read_calls[0].lines, Some(lines), "{command}");
            assert_eq!(
                store.count("tool_reads_file").await.unwrap(),
                1,
                "{command}"
            );
        }
    }

    /// One Codex `Bash` result through the ingest, with a hasher that resolves
    /// whatever path it is handed.
    async fn ingest_command(command: &str) -> (impl CanonicalStore, IngestOutcome) {
        let reg = AdapterRegistry::with_builtin();
        let hasher = |path: &str| {
            Some(crate::project::ReadContent {
                rel_path: path.to_owned(),
                blob_oid: "oid-9".into(),
                content_sha256: "ee99".into(),
                tokens: None,
            })
        };
        let s = store();
        let tmp = tempfile::tempdir().unwrap();
        let mut read = raw("PostToolUse", 1);
        read.agent = AgentId::CODEX;
        read.payload = serde_json::json!({
            "tool_name": "Bash",
            "tool_input": {"command": command},
        });
        let out = ingest_hook(
            &read,
            &reg,
            &s,
            tmp.path(),
            "codex",
            "repo-1",
            10,
            Some(&hasher),
        )
        .await
        .unwrap();
        (s, out)
    }
}
