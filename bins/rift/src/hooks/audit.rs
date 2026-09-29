//! The serve-audit funnel: `rift hooks memd <verb>` and the queued serve-audit
//! job both append recall serve-decision events to the per-repo `recall.db`
//! audit relation. Separate because the rows it writes are what the savings
//! ledger, doctor and the dashboard read back; one module owns their shape.
//!
//! It also owns the family's serve journal: every envelope a row is written
//! from is appended, canonicalised, to a per-session file under the git common
//! dir, so the next checkpoint of that session can freeze it into its shard.
//! The projection is rebuildable and a rebuild deletes it; the shard is not,
//! so a serve that is only in the projection is a serve a rebuild erases.

use std::io::Read as _;
use std::path::{Path, PathBuf};

use rift_core::Result;

use super::now_ms;

pub(crate) use rift_recall_ledger::audit::append_memd_audit;

pub(crate) use frozen::{canonical_serve_line, serve_line_key};

/// The family's serve journal directory under the git common dir, beside the
/// push queue: keyed by the family and never by a checkout, because a
/// session's serves and its condense can run in different members.
const SERVE_JOURNAL_DIR: &str = "rift-serve-audit";

/// The journal of serves no session made — the skill CLI's, and a queued job
/// an older binary left without one. The next checkpoint of any session in
/// the family folds it.
const UNATTRIBUTED_JOURNAL: &str = "unattributed";

/// The deferred accounting of one hook decision: count what it avoided and
/// what it cost, and append the row to **both** serving relations.
///
/// It is deferred for the two reasons the agent-hook budget names. Counting
/// tokens means initialising the tokenizer (234 ms before a byte is counted),
/// and appending the row means opening `recall.db` (~80 ms). Neither belongs
/// on the path a tool call waits on, so the hook writes a file naming the
/// decision and this counts and writes it.
///
/// Both relations, because they answer two different questions and only one
/// of them each: `memd_audit_event` is what the savings feed sums, and
/// `serve_call_event` is where the decision's channel — here always the hook
/// — is recorded. The pair is written the same way the CLI serve path writes
/// its own, keyed by the decision's id so a redelivered job is one row.
pub(super) fn serve_audit_job(
    t: &crate::hooktime::HookTimer,
    job: &crate::agent_queue::AgentJob,
    audit: &crate::agent_queue::ServeAudit,
) -> Result<()> {
    let repo = rift_git::discover(&job.work_tree)?;
    let event = t.phase("audit_count", || serve_audit_event(&repo.work_tree, audit));
    let db = crate::family::recall_db(&repo.work_tree)?;
    let store = t.phase("open_recall", || {
        rift_store::open_migrated(&db, &rift_store::recall_migrations())
    })?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .map_err(rift_core::RiftError::from)?;
    // The decision's own clock, not the worker's: a row deferred by an hour
    // still records when the agent's turn spent (or saved) the tokens.
    let (audited, called) = serve_audit_envelopes(&event, audit, job.queued_at_ms);
    let booked = rt.block_on(async {
        append_memd_audit(&store, audited.as_bytes(), job.queued_at_ms).await?;
        append_memd_audit(&store, called.as_bytes(), job.queued_at_ms).await
    });
    // Journaled whatever the row writes returned, and silently like them: the
    // worker's error channel is the job's retry, and a retried job appends
    // the same lines again, which the archive's de-duplication collapses.
    let session = audit.session.as_deref();
    journal_serve_envelope(&repo.common_dir, session, audited.as_bytes());
    journal_serve_envelope(&repo.common_dir, session, called.as_bytes());
    booked
}

/// The two envelopes one decision becomes: the audit row addressed by the
/// decision's id, and the channel-attributed call row beside it. One function
/// so a test asserting "both relations" asserts the production pair.
pub(super) fn serve_audit_envelopes(
    event: &rift_recall_serve::MemdEvent,
    audit: &crate::agent_queue::ServeAudit,
    at_ms: i64,
) -> (String, String) {
    (
        event.to_hook_json_keyed(&audit.call_id),
        event.to_serve_call_json(
            rift_recall_serve::ServeChannel::HookInject,
            &audit.call_id,
            &format!("{}:{}", audit.agent, serve_audit_step(audit.decision)),
            rift_core::Timestamp::from_millis(at_ms),
        ),
    )
}

/// Which hook step decided, joined to the family in `tool_name` — the only
/// per-family axis these relations have, and the one the per-family honesty
/// reporting is owed from.
fn serve_audit_step(kind: crate::agent_queue::ServeAuditKind) -> &'static str {
    use crate::agent_queue::ServeAuditKind as K;
    match kind {
        K::ReadIntercepted | K::ReadRepeatObserved => "read-gate",
        // Its own step, not the read gate's: the per-family table is read for
        // what each lever is worth, and a figure folded into another lever's
        // step is a figure nobody can separate again.
        K::ReadPrevented | K::ReadPreventedObserved => "prevented",
        K::RecoveryInjected => "recovery-map",
        K::SearchAnswered | K::GrepCapped => "search-gate",
        K::DocRewritten | K::DocDeferred => "doc-gate",
    }
}

/// The tag a decision of this kind carries when it names none of its own.
fn serve_audit_default_tag(kind: crate::agent_queue::ServeAuditKind) -> &'static str {
    use crate::agent_queue::ServeAuditKind as K;
    use rift_recall_serve::{
        TAG_ALREADY_HELD, TAG_CONVERT_DEADLINE, TAG_DECLARED_BASELINE, TAG_PREVENTED,
        TAG_PREVENTED_OBSERVED,
    };
    match kind {
        K::ReadIntercepted | K::ReadRepeatObserved => TAG_ALREADY_HELD,
        K::ReadPrevented => TAG_PREVENTED,
        K::ReadPreventedObserved => TAG_PREVENTED_OBSERVED,
        K::DocRewritten => TAG_DECLARED_BASELINE,
        K::DocDeferred => TAG_CONVERT_DEADLINE,
        K::RecoveryInjected | K::SearchAnswered | K::GrepCapped => "",
    }
}

/// The figures a decision books: what its frame cost, what it avoided, and
/// the tag that says when a figure is not a measurement.
///
/// "Measured or absent" throughout. The avoided figure is the real tokenizer
/// count of the bytes the decision named, re-read under the identity it named
/// them by — the working file when it still hashes to it, the git blob when
/// it does not — so a file edited between the decision and this count is
/// never counted under a stale identity. Neither source, or no real
/// tokenizer, books zero and says which.
fn serve_audit_figures(
    work_tree: &Path,
    audit: &crate::agent_queue::ServeAudit,
) -> (u32, u32, Option<&'static str>) {
    use rift_recall_serve::{TAG_BASIS_UNAVAILABLE, TAG_DECLARED_BASELINE, TAG_TOKENS_UNMEASURED};
    // A lever that is only observing claims nothing, and says so with a zero
    // it can defend rather than with a figure it declines to publish: the row
    // still names the file and the lines, so the class can be sized before
    // anything is claimed for it.
    if audit.decision == crate::agent_queue::ServeAuditKind::ReadPreventedObserved {
        return (0, 0, None);
    }
    let counts_something = !audit.frame.is_empty() || audit.basis.is_some();
    if counts_something && rift_tokens::provenance() != rift_tokens::TOKENIZER {
        return (0, 0, Some(TAG_TOKENS_UNMEASURED));
    }
    let returned = rift_tokens::count(&audit.frame);
    // A document is priced by its own formula and never by the slice count
    // below it: what a container costs is what the client's rendering of it
    // would have cost, cut at the client's ceilings, and tokenizing the whole
    // of a 16 MiB zip as though it were text a read delivers would book a
    // figure no read could have spent.
    if matches!(
        audit.decision,
        crate::agent_queue::ServeAuditKind::DocRewritten
            | crate::agent_queue::ServeAuditKind::DocDeferred
    ) {
        return document_figures(work_tree, audit, returned);
    }
    // A declared baseline is the answer where there is no second reading to
    // measure: the native cost of a document nobody read is a fixture's
    // figure, and the tag is what keeps it from passing as a measurement.
    if let Some(declared) = audit.declared_avoided {
        return (returned, declared, Some(TAG_DECLARED_BASELINE));
    }
    if audit.decision == crate::agent_queue::ServeAuditKind::SearchAnswered {
        return search_figures(work_tree, audit, returned);
    }
    let Some(basis) = audit.basis.as_ref() else {
        return (returned, 0, None);
    };
    match audit_slice_tokens(work_tree, basis) {
        Some(avoided) => (returned, avoided, None),
        None => (returned, 0, Some(TAG_BASIS_UNAVAILABLE)),
    }
}

/// What one document decision books: the frame it cost, and the **declared**
/// native cost of the document it stood in for.
///
/// Declared, never measured — the read this figure prices never happened, so
/// there is no second reading to count — and computed here, in the worker,
/// from the document's own bytes under the hash the decision recorded. A
/// decision that claims nothing says so with `Some(0)` and is booked at that;
/// only the arm whose substitution is checked against the executed call asks
/// for the declaration. Bytes that no longer hash to the recorded identity are
/// bytes the agent never opened, so nothing is declared about them either.
fn document_figures(
    work_tree: &Path,
    audit: &crate::agent_queue::ServeAudit,
    returned: u32,
) -> (u32, u32, Option<&'static str>) {
    use rift_recall_serve::{TAG_BASIS_UNAVAILABLE, TAG_DECLARED_BASELINE};
    if let Some(declared) = audit.declared_avoided {
        return (returned, declared, Some(TAG_DECLARED_BASELINE));
    }
    let Some(basis) = audit.basis.as_ref() else {
        return (returned, 0, Some(TAG_BASIS_UNAVAILABLE));
    };
    let ext = Path::new(&basis.rel_path)
        .extension()
        .and_then(std::ffi::OsStr::to_str)
        .unwrap_or_default();
    match read_identified_bytes(
        work_tree,
        &basis.rel_path,
        &basis.content_sha256,
        &basis.blob_oid,
    ) {
        Some(bytes) => (
            returned,
            rift_recall_serve::declared_document_baseline(ext, &bytes),
            Some(TAG_DECLARED_BASELINE),
        ),
        None => (returned, 0, Some(TAG_BASIS_UNAVAILABLE)),
    }
}

/// What one answered name search books: the frame it cost, and the **declared**
/// native cost of the listing it stood in for.
///
/// Declared, never measured, for the same reason a document's is: the listing
/// this figure prices never ran. What is counted is the paths themselves, one
/// per line, as a native listing prints them — the matches the decision left
/// behind, already screened and already cut where the client this call was
/// made on cuts them.
///
/// **The ceiling is the shape's, not one figure for both shapes.** A glob tool
/// stops after its own file cap; a shell listing prints every match and the
/// client stops reading the result at its own inline character ceiling.
/// The decision journalled which of the two it was, and a job left by a binary
/// that did not takes the smaller ceiling — a figure that cannot be too large
/// is the only kind this may err toward.
///
/// It errs low in three ways at once, which is the direction a figure Rift will
/// claim credit for has to err in: the index skips what git ignores and the
/// native tool does not, so the priced set is a subset; the paths are
/// repository-relative where the tool prints absolute ones; and the tool's own
/// truncation notice is not counted at all.
///
/// A decision whose paths are gone — collected between the hook and the drain —
/// books nothing and says so, rather than booking a saving of zero as though
/// the listing had been free.
fn search_figures(
    work_tree: &Path,
    audit: &crate::agent_queue::ServeAudit,
    returned: u32,
) -> (u32, u32, Option<&'static str>) {
    use rift_recall_serve::{ListingShape, TAG_BASIS_UNAVAILABLE, TAG_DECLARED_BASELINE};
    let Some(paths) = crate::search_gate::priced_paths(work_tree, &audit.call_id) else {
        return (returned, 0, Some(TAG_BASIS_UNAVAILABLE));
    };
    let shape = audit.shape.unwrap_or(ListingShape::Glob);
    (
        returned,
        rift_recall_serve::declared_search_baseline(false, shape, &audit.agent, &paths),
        Some(TAG_DECLARED_BASELINE),
    )
}

/// The token count of the bytes a basis names — the whole file, or the run of
/// lines the decision was about.
fn audit_slice_tokens(work_tree: &Path, basis: &crate::agent_queue::AuditBasis) -> Option<u32> {
    let bytes = read_identified_bytes(
        work_tree,
        &basis.rel_path,
        &basis.content_sha256,
        &basis.blob_oid,
    )?;
    let text = String::from_utf8_lossy(&bytes);
    let Some(lines) = basis.lines else {
        return Some(rift_tokens::count(&text));
    };
    let slice: String = text
        .split_inclusive('\n')
        .skip(lines.lo as usize - 1)
        .take((lines.hi.saturating_sub(lines.lo) as usize) + 1)
        .collect();
    Some(rift_tokens::count(&slice))
}

/// The serving event one journaled decision is, figures and all.
pub(super) fn serve_audit_event(
    work_tree: &Path,
    audit: &crate::agent_queue::ServeAudit,
) -> rift_recall_serve::MemdEvent {
    use crate::agent_queue::ServeAuditKind as K;
    use rift_recall_serve::MemdEvent;
    let (estimated_tokens_returned, estimated_tokens_avoided, measurement) =
        serve_audit_figures(work_tree, audit);
    let handle = audit.basis.as_ref().map_or_else(String::new, |basis| {
        basis.lines.map_or_else(
            || rift_recall_serve::file_handle(&basis.rel_path, &basis.content_sha256),
            |lines| {
                rift_recall_serve::read_handle(
                    &basis.rel_path,
                    &basis.content_sha256,
                    lines.lo,
                    lines.hi,
                )
            },
        )
    });
    // What could not be measured outranks what the decision meant to say:
    // a reader must not have to know the measurement failed to read the row.
    // A declared baseline is not a failed measurement, so it yields to the
    // decision's own tag when there is one — a dropped substitution must read
    // as a dropped substitution, not as a figure nobody measured.
    let reason = measurement
        .filter(|m| *m != rift_recall_serve::TAG_DECLARED_BASELINE || audit.tag.is_none())
        .map(str::to_owned)
        .or_else(|| audit.tag.clone())
        .unwrap_or_else(|| serve_audit_default_tag(audit.decision).to_owned());
    match audit.decision {
        K::ReadIntercepted => MemdEvent::ReadIntercepted {
            handle,
            reason,
            estimated_tokens_returned,
            estimated_tokens_avoided,
        },
        K::ReadRepeatObserved => MemdEvent::ReadRepeatObserved { handle, reason },
        K::ReadPrevented | K::ReadPreventedObserved => MemdEvent::ReadPrevented {
            handle,
            reason,
            estimated_tokens_avoided,
        },
        K::RecoveryInjected => MemdEvent::RecoveryInjected {
            estimated_tokens_returned,
        },
        K::SearchAnswered => MemdEvent::SearchAnswered {
            reason,
            estimated_tokens_returned,
            estimated_tokens_avoided,
        },
        K::GrepCapped => MemdEvent::GrepCapped {
            handle: rift_recall_serve::grep_cap_handle(audit.total.unwrap_or_default()),
        },
        K::DocRewritten => MemdEvent::DocRewritten {
            handle,
            reason,
            estimated_tokens_returned,
            estimated_tokens_avoided,
        },
        K::DocDeferred => MemdEvent::DocDeferred {
            handle,
            reason,
            estimated_tokens_returned,
        },
    }
}

/// The bytes a read fact identified: the working file at `rel_path` when its
/// SHA-256 is still `content_sha256`, else the git blob `blob_oid` when its
/// SHA-256 is. `None` when neither holds — the count would describe bytes
/// the agent never read.
pub(super) fn read_identified_bytes(
    work_tree: &Path,
    rel_path: &str,
    content_sha256: &str,
    blob_oid: &str,
) -> Option<Vec<u8>> {
    use rift_git::BlobReader as _;
    let matches = |bytes: &[u8]| rift_shadow::sha256_hex(&[bytes]) == content_sha256;
    if let Some((blob, bytes)) =
        rift_recall_substrate::worktree::hash_blob_bytes(work_tree, rel_path)
    {
        if blob.content_sha256 == content_sha256 && matches(&bytes) {
            return Some(bytes);
        }
    }
    let reader = rift_git::GixBlobReader::open(work_tree).ok()?;
    let bytes = reader.read_blob(blob_oid).ok()??;
    matches(&bytes).then_some(bytes)
}

/// `rift hooks memd <verb>`: append one recall serve-decision event to the
/// per-repo `recall.db` audit relation. Dedicated so `memd.*` events **bypass**
/// the agent-adapter registry (which returns `unknown agent` and drops them).
/// Diverges: always exit 0 — an audit-write failure must never disturb the
/// recall server's response path.
pub(super) fn run_memd_hook(_args: &[String]) -> ! {
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = record_memd_event();
    }));
    let _ = outcome;
    std::process::exit(0);
}

/// Read the `memd.*` hook envelope from stdin and append it to the **family's**
/// `recall.db` serve-audit relation.
///
/// This used to key the store by the raw process cwd, which made a serve
/// audited from a subdirectory — or from a linked worktree — land in a
/// different, empty database from the one `rift recall <verb>` serves out of.
/// The cwd is now resolved to its work tree and that work tree to its family
/// so writer and reader cannot disagree about the file.
fn record_memd_event() -> Result<()> {
    let mut body = Vec::new();
    std::io::stdin().read_to_end(&mut body)?;
    let cwd = std::env::current_dir()?;
    let db = crate::family::recall_db(&crate::worktree_root(&cwd))?;
    let store = rift_store::open_migrated(&db, &rift_store::recall_migrations())?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .map_err(rift_core::RiftError::from)?;
    let booked = rt.block_on(append_memd_audit(&store, &body, now_ms()));
    // No envelope schema names a session, so this journals as unattributed.
    journal_serve_in(&cwd, None, &body);
    booked
}

/// The journal a serve of `session` is appended to, or `None` when the id is
/// not safe to name a file with — such a serve journals nothing, exactly as
/// the session cache refuses to key a file by it.
pub(crate) fn serve_journal_path(common_dir: &Path, session: Option<&str>) -> Option<PathBuf> {
    let name = match session {
        Some(id) if rift_shadow::hooks::is_safe_session_id(id) => id,
        Some(_) => return None,
        None => UNATTRIBUTED_JOURNAL,
    };
    Some(serve_journal_dir(common_dir).join(format!("{name}.jsonl")))
}

/// The family's serve journal directory. The common dir is canonicalised the
/// way the push queue's is, so two members spelling it differently write one
/// directory.
pub(crate) fn serve_journal_dir(common_dir: &Path) -> PathBuf {
    std::fs::canonicalize(common_dir)
        .unwrap_or_else(|_| common_dir.to_path_buf())
        .join(SERVE_JOURNAL_DIR)
}

/// Append one envelope's canonical line to the family's serve journal.
///
/// One `O_APPEND` write of the whole line, so two writers never interleave
/// inside a line. Best-effort like the row write it follows: an envelope the
/// canonicaliser refuses, an unsafe session id or an I/O error journals
/// nothing and says nothing, because a hook or a serve never fails on its
/// bookkeeping.
pub(crate) fn journal_serve_envelope(common_dir: &Path, session: Option<&str>, envelope: &[u8]) {
    use std::io::Write as _;
    let (Some(path), Some(mut line)) = (
        serve_journal_path(common_dir, session),
        canonical_serve_line(envelope),
    ) else {
        return;
    };
    line.push(b'\n');
    let Some(dir) = path.parent() else {
        return;
    };
    let _ = std::fs::create_dir_all(dir);
    let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    else {
        return;
    };
    let _ = file.write_all(&line);
}

/// [`journal_serve_envelope`] for a caller that holds a checkout rather than
/// its common dir.
pub(crate) fn journal_serve_in(work_tree: &Path, session: Option<&str>, envelope: &[u8]) {
    if let Ok(repo) = rift_git::discover(work_tree) {
        journal_serve_envelope(&repo.common_dir, session, envelope);
    }
}

mod frozen {
    //! The canonical form of a serve envelope — the only form a line may take
    //! in the serve journal and in a shard's `serve_audit.jsonl`.
    //!
    //! A shard artifact carries identifiers, hashes, paths, counts, timestamps
    //! and Rift's closed vocabularies, and never a byte a person or a model
    //! typed. A serve envelope is mostly that already; this module is where
    //! "mostly" becomes "only", member by member.
    //!
    //! The canonical line keeps the declared members and nothing else, replaces
    //! each member that can hold typed text with [`WITHHELD`], and is
    //! serialised with sorted keys, so its bytes are a pure function of the
    //! envelope and `git show` prints one object per line. A line with a member
    //! the table does not declare is refused whole: an unknown member is one
    //! nobody has audited.
    //!
    //! The idempotency key survives untouched. The appender reads it off the
    //! envelope (`idempotency_key`) and never recomputes it from the payload, so
    //! a frozen line appended again lands on the key of the row it was frozen
    //! from, placeholder or not.
    //!
    //! # The audit
    //!
    //! Top level, schema 1: `schema`, `verb`, `idempotency_key`, `payload`.
    //! Schema 4 adds `serve_call`: `call_id` (must equal the key), `channel`
    //! (the closed pair), `tool_name` (Rift's `<family>:<step>` or the skill
    //! verb; kept when it is in that grammar, [`WITHHELD`] otherwise),
    //! `recorded_at`, `tokens_returned`, `tokens_avoided`. Schemas 2 and 3 are
    //! refused: nothing produces them any more.
    //!
    //! | Payload member | Kinds | Frozen as |
    //! |---|---|---|
    //! | `kind` | every kind | kept; must name the verb's own kind |
    //! | `estimated_tokens_*`, `sections`, `total_tokens` | every kind that has them | kept (counts) |
    //! | `handle` | `read_*`, `doc_*` | kept when `file:<path>@sha256:<hash>[:L<lo>-<hi>]`, else withheld |
    //! | `handle` | `summary_served`, `invalidated` | kept when `summary:<path>`, else withheld |
    //! | `handle` | `exact_retrieved` | kept when a file, summary, `log:` or citation handle, else withheld |
    //! | `handle` | `grep_capped` | kept when `grep:matches=<n>`, else withheld |
    //! | `target` | `refused` | kept only when a citation; otherwise **withheld** |
    //! | `reason` | `exact_retrieved` | **withheld** (the caller's own words), unless it is the grant tag |
    //! | `reason` | every other kind that has one | kept when a closed tag (`[a-z0-9_-]`, at most 64 bytes), else withheld |
    //! | `reason` | a kind that declares none | kept only when it is the grant tag |
    //!
    //! `ExactRetrieved.reason` is the reason the caller supplied, typed and
    //! mandatory: a row rebuilt from a frozen line carries the placeholder,
    //! and the caller's words stay where they were only ever meant to be. A
    //! refusal's `target` is the string the caller asked for — a refusal is
    //! exactly the case where nothing proved it names a real file, and a
    //! fabricated target is text — so only a citation, a grammar the pack
    //! compiler composes, is kept. Every other `handle` is composed by Rift
    //! from a repository path and a content hash it computed.

    use serde_json::{Map, Value};

    /// What a frozen line carries where the envelope held typed text.
    pub(crate) const WITHHELD: &str = "withheld";

    /// How one payload member is frozen.
    #[derive(Clone, Copy)]
    enum Member {
        /// A token or section count.
        Count,
        /// A neutral tag from a closed vocabulary.
        Tag,
        /// Text the caller typed.
        Typed,
        /// `file:<path>@sha256:<hash>[:L<lo>-<hi>]`.
        FileHandle,
        /// `summary:<path>`.
        SummaryHandle,
        /// Any handle an exact retrieval can have served.
        ExactHandle,
        /// `grep:matches=<n>`.
        GrepHandle,
        /// What a refusal refused.
        RefusedTarget,
        /// A `reason` on a kind that declares none: only the grant tag.
        GrantTag,
    }

    const RETURNED: (&str, Member) = ("estimated_tokens_returned", Member::Count);
    const AVOIDED: (&str, Member) = ("estimated_tokens_avoided", Member::Count);
    const REASON: (&str, Member) = ("reason", Member::Tag);
    const FILE: (&str, Member) = ("handle", Member::FileHandle);
    const SUMMARY: (&str, Member) = ("handle", Member::SummaryHandle);

    /// The canonical line of one envelope, or `None` when it is not one this
    /// table declares.
    pub(crate) fn canonical_serve_line(envelope: &[u8]) -> Option<Vec<u8>> {
        let Ok(Value::Object(env)) = serde_json::from_slice::<Value>(envelope) else {
            return None;
        };
        let schema = env.get("schema")?.as_u64()?;
        let members: &[&str] = match schema {
            1 => &["idempotency_key", "payload", "schema", "verb"],
            4 => &["idempotency_key", "payload", "schema", "serve_call", "verb"],
            _ => return None,
        };
        if env.keys().any(|k| !members.contains(&k.as_str())) {
            return None;
        }
        let key = env.get("idempotency_key")?.as_str()?;
        let verb = env.get("verb")?.as_str()?;
        if !is_key(key) {
            return None;
        }
        let mut out = Map::new();
        out.insert("idempotency_key".into(), key.into());
        out.insert(
            "payload".into(),
            canonical_payload(verb, env.get("payload")?)?,
        );
        out.insert("schema".into(), schema.into());
        out.insert("verb".into(), verb.into());
        if schema == 4 {
            out.insert(
                "serve_call".into(),
                canonical_call(key, env.get("serve_call")?)?,
            );
        }
        serde_json::to_vec(&Value::Object(out)).ok()
    }

    /// The idempotency key a canonical line is addressed by.
    pub(crate) fn serve_line_key(line: &[u8]) -> Option<String> {
        let value = serde_json::from_slice::<Value>(line).ok()?;
        Some(value.get("idempotency_key")?.as_str()?.to_owned())
    }

    /// The declared members of one event kind's payload, as the audit table
    /// in the module doc states them.
    fn declared(kind: &str) -> Option<&'static [(&'static str, Member)]> {
        use Member::{ExactHandle, GrepHandle, RefusedTarget, Typed};
        Some(match kind {
            "manifest_served" | "search_served" => &[RETURNED, AVOIDED],
            "manifest_injected" | "recovery_injected" => &[RETURNED],
            "summary_served" => &[SUMMARY, RETURNED, AVOIDED],
            "exact_retrieved" => &[("handle", ExactHandle), ("reason", Typed), RETURNED],
            "pack_built" => &[
                ("sections", Member::Count),
                ("total_tokens", Member::Count),
                RETURNED,
                AVOIDED,
            ],
            "invalidated" => &[SUMMARY, RETURNED],
            "refused" => &[("target", RefusedTarget), REASON, RETURNED],
            "read_intercepted" | "doc_rewritten" | "doc_served" => {
                &[FILE, REASON, RETURNED, AVOIDED]
            }
            "read_repeat_observed" => &[FILE, REASON],
            "read_prevented" => &[FILE, REASON, AVOIDED],
            "search_answered" => &[REASON, RETURNED, AVOIDED],
            "grep_capped" => &[("handle", GrepHandle)],
            "doc_deferred" => &[FILE, REASON, RETURNED],
            _ => return None,
        })
    }

    /// The payload with every member frozen by its rule, or `None` when the
    /// kind is unknown, disagrees with the verb, or carries an undeclared
    /// member.
    fn canonical_payload(verb: &str, payload: &Value) -> Option<Value> {
        let kind = verb.strip_prefix("memd.")?;
        let (Some(members), Value::Object(fields)) = (declared(kind), payload) else {
            return None;
        };
        if fields.get("kind")?.as_str()? != kind {
            return None;
        }
        let mut out = Map::new();
        for (name, value) in fields {
            let rule = match members.iter().find(|(declared, _)| declared == name) {
                Some((_, rule)) => *rule,
                None if name == "kind" => {
                    out.insert(name.clone(), kind.into());
                    continue;
                }
                None if name == "reason" => Member::GrantTag,
                None => return None,
            };
            out.insert(name.clone(), frozen_member(rule, value)?);
        }
        Some(Value::Object(out))
    }

    /// The call record of a schema-4 line, or `None` when it is not the one
    /// the appender would book.
    fn canonical_call(key: &str, call: &Value) -> Option<Value> {
        const MEMBERS: [&str; 6] = [
            "call_id",
            "channel",
            "recorded_at",
            "tokens_avoided",
            "tokens_returned",
            "tool_name",
        ];
        let Value::Object(fields) = call else {
            return None;
        };
        if fields.keys().any(|k| !MEMBERS.contains(&k.as_str())) {
            return None;
        }
        let channel = fields.get("channel")?.as_str()?;
        let tool_name = fields.get("tool_name")?.as_str()?;
        if fields.get("call_id")?.as_str()? != key
            || rift_recall_serve::ServeChannel::parse(channel).is_none()
        {
            return None;
        }
        let mut out = Map::new();
        out.insert("call_id".into(), key.into());
        out.insert("channel".into(), channel.into());
        out.insert(
            "recorded_at".into(),
            fields.get("recorded_at")?.as_i64()?.into(),
        );
        for count in ["tokens_avoided", "tokens_returned"] {
            out.insert(count.into(), fields.get(count)?.as_u64()?.into());
        }
        let tool_name = if is_tool_name(tool_name) {
            tool_name
        } else {
            WITHHELD
        };
        out.insert("tool_name".into(), tool_name.into());
        Some(Value::Object(out))
    }

    /// One member frozen by its rule; `None` when its JSON type is not the
    /// one the rule reads, which refuses the whole line.
    fn frozen_member(rule: Member, value: &Value) -> Option<Value> {
        if let Member::Count = rule {
            return Some(value.as_u64()?.into());
        }
        let text = value.as_str()?;
        Some(if is_kept(rule, text) { text } else { WITHHELD }.into())
    }

    /// Whether a text member's value is one its rule lets through.
    fn is_kept(rule: Member, text: &str) -> bool {
        let grant = text == rift_recall_serve::CROSS_BRANCH_GRANT_TAG;
        match rule {
            // A count that arrives as text is not a count.
            Member::Count => false,
            Member::Tag => is_tag(text),
            Member::Typed | Member::GrantTag => grant,
            Member::FileHandle => text.is_empty() || is_file_handle(text),
            Member::SummaryHandle => text.strip_prefix("summary:").is_some_and(is_path),
            Member::ExactHandle => is_exact_handle(text),
            Member::GrepHandle => text.strip_prefix("grep:matches=").is_some_and(is_digits),
            Member::RefusedTarget => is_citation(text),
        }
    }

    /// A handle an exact retrieval can have served: a file, a summary, a log
    /// entry or a citation.
    fn is_exact_handle(text: &str) -> bool {
        is_file_handle(text)
            || text.strip_prefix("summary:").is_some_and(is_path)
            || text.strip_prefix("log:").is_some_and(is_key)
            || is_citation(text)
    }

    /// `pack:<key>/evidence:<n>[@sha256:<hash>]`, optionally followed by
    /// `=file:…` — the address the pack compiler composed and, after the `=`,
    /// the record it resolved to.
    fn is_citation(text: &str) -> bool {
        let (head, resolved) = match text.split_once('=') {
            Some((head, tail)) => (head, Some(tail)),
            None => (text, None),
        };
        rift_recall_serve::pack::CitationTarget::parse(head).is_some()
            && resolved.is_none_or(is_file_handle)
    }

    /// `file:<path>@sha256:<64 hex>`, optionally `:L<lo>-<hi>`.
    fn is_file_handle(text: &str) -> bool {
        let Some((path, tail)) = text
            .strip_prefix("file:")
            .and_then(|rest| rest.rsplit_once("@sha256:"))
        else {
            return false;
        };
        let (hash, lines) = match tail.split_once(':') {
            Some((hash, lines)) => (hash, Some(lines)),
            None => (tail, None),
        };
        let range_ok = lines.is_none_or(|lines| {
            lines
                .strip_prefix('L')
                .and_then(|range| range.split_once('-'))
                .is_some_and(|(lo, hi)| is_digits(lo) && is_digits(hi))
        });
        is_path(path) && is_hex64(hash) && range_ok
    }

    /// A repository path as a handle carries it: non-empty, bounded, and free
    /// of control characters.
    fn is_path(path: &str) -> bool {
        !path.is_empty() && path.len() <= 4096 && !path.chars().any(char::is_control)
    }

    fn is_hex64(text: &str) -> bool {
        text.len() == 64 && text.bytes().all(|b| b.is_ascii_hexdigit())
    }

    fn is_digits(text: &str) -> bool {
        !text.is_empty() && text.len() <= 10 && text.bytes().all(|b| b.is_ascii_digit())
    }

    /// A closed-vocabulary tag; the empty tag is what a decision without one
    /// carries.
    fn is_tag(text: &str) -> bool {
        text.len() <= 64
            && text
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
    }

    /// An idempotency key or a log id: a hash, a ULID or a caller's call id.
    fn is_key(text: &str) -> bool {
        !text.is_empty()
            && text.len() <= 128
            && text
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    }

    /// `<family>:<step>` or a skill verb.
    fn is_tool_name(text: &str) -> bool {
        !text.is_empty()
            && text.len() <= 128
            && text
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b':' | b'.'))
    }
}

/// The read gate: what a context window is handed, what it stops holding, and
/// the one direction every refusal leans in.
#[cfg(test)]
mod serve_audit_tests {
    use rift_store::{apply_migrations, recall_migrations, CanonicalStore, SqliteStore};

    use super::*;
    use crate::agent_queue::{AuditBasis, AuditLines, ServeAudit, ServeAuditKind};

    /// A work tree with one file of numbered lines, and the identity of its
    /// bytes as the gate would have journaled it.
    fn checkout(lines: u32) -> (tempfile::TempDir, AuditBasis, String) {
        let tmp = tempfile::tempdir().expect("a work tree");
        let body = (1..=lines).fold(String::new(), |mut body, n| {
            use std::fmt::Write as _;
            let _ = writeln!(body, "line {n} of a file somebody read");
            body
        });
        std::fs::write(tmp.path().join("src.rs"), &body).expect("the file");
        let (blob, _) = rift_recall_substrate::worktree::hash_blob_bytes(tmp.path(), "src.rs")
            .expect("the bytes");
        (
            tmp,
            AuditBasis {
                rel_path: "src.rs".to_owned(),
                content_sha256: blob.content_sha256,
                blob_oid: blob.blob_oid,
                lines: Some(AuditLines { lo: 1, hi: 10 }),
            },
            body,
        )
    }

    fn decision(kind: ServeAuditKind, basis: Option<AuditBasis>, frame: &str) -> ServeAudit {
        ServeAudit {
            call_id: "0123456789abcdef0123456789abcdef".to_owned(),
            decision: kind,
            agent: "claude-code".to_owned(),
            basis,
            frame: frame.to_owned(),
            tag: None,
            total: None,
            declared_avoided: None,
            shape: None,
            session: None,
        }
    }

    /// One row of the savings relation: verb, handle, reason, returned,
    /// avoided.
    type AuditRow = (String, String, String, i64, i64);
    /// One row of the channel relation: channel, tool name, returned, avoided.
    type CallRow = (String, String, i64, i64);

    /// Write one decision's pair of envelopes to a store, exactly as the
    /// worker does, and hand back the two rows it left.
    fn book(work_tree: &Path, audit: &ServeAudit) -> (Vec<AuditRow>, Vec<CallRow>) {
        let store = SqliteStore::open_in_memory().expect("a store");
        apply_migrations(&store, &recall_migrations()).expect("the schema");
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("a runtime");
        let event = serve_audit_event(work_tree, audit);
        let (audited, called) = serve_audit_envelopes(&event, audit, 1_700_000_000_000);
        rt.block_on(async {
            append_memd_audit(&store, audited.as_bytes(), 1)
                .await
                .expect("the audit row");
            append_memd_audit(&store, called.as_bytes(), 1)
                .await
                .expect("the call row");
        });
        let audit_rows = rt
            .block_on(store.query(
                "SELECT verb, handle, COALESCE(reason, ''), tokens_returned, tokens_avoided \
                 FROM memd_audit_event",
                &[],
            ))
            .expect("the audit relation")
            .iter()
            .map(|r| {
                (
                    r.get_text(0).unwrap_or_default(),
                    r.get_text(1).unwrap_or_default(),
                    r.get_text(2).unwrap_or_default(),
                    r.get_i64(3).unwrap_or_default(),
                    r.get_i64(4).unwrap_or_default(),
                )
            })
            .collect();
        let call_rows = rt
            .block_on(store.query(
                "SELECT channel, tool_name, tokens_returned, tokens_avoided FROM serve_call_event",
                &[],
            ))
            .expect("the call relation")
            .iter()
            .map(|r| {
                (
                    r.get_text(0).unwrap_or_default(),
                    r.get_text(1).unwrap_or_default(),
                    r.get_i64(2).unwrap_or_default(),
                    r.get_i64(3).unwrap_or_default(),
                )
            })
            .collect();
        (audit_rows, call_rows)
    }

    /// ★ **An answered search says in its own row which kind of answer it
    /// was, and what its avoided figure is.**
    ///
    /// The row used to carry the two figures and no word. An answer that was
    /// made and an answer that was only recorded were then told apart by an
    /// avoided figure of zero, which is an inference about a row rather than a
    /// fact in it — and it is not even a sound one, since a decision whose
    /// paths were collected before the worker reached them books zero too.
    /// The tag the decision already computed now reaches the relation, through
    /// the reason column that was there all along.
    #[test]
    fn an_answered_search_books_the_tag_that_says_what_its_figure_is() {
        let tmp = tempfile::tempdir().expect("a work tree");
        let frame = "[rift] Search answered: 2 of 40 paths matching **/*.rs";
        let paths = ["src/alpha/handler.rs", "src/beta/handler.rs"];

        // The answer that was made: the worker finds the paths the decision
        // left, prices them, and says the figure is declared and not measured.
        let made = ServeAudit {
            shape: Some(rift_recall_serve::ListingShape::Listing),
            ..decision(ServeAuditKind::SearchAnswered, None, frame)
        };
        let answers = tmp.path().join(".rift/state/search/answers");
        std::fs::create_dir_all(&answers).expect("the run state");
        std::fs::write(answers.join(&made.call_id), paths.join("\n")).expect("the paths");

        let (rows, _) = book(tmp.path(), &made);
        assert_eq!(rows.len(), 1);
        let (verb, handle, reason, returned, avoided) = &rows[0];
        assert_eq!(verb, "memd.search_answered");
        assert_eq!(handle, "", "a listing is about no file and no revision");
        assert_eq!(reason, rift_recall_serve::TAG_DECLARED_BASELINE);
        assert_eq!(*returned, i64::from(rift_tokens::count(frame)));
        assert_eq!(
            *avoided,
            i64::from(rift_recall_serve::declared_search_baseline(
                false,
                rift_recall_serve::ListingShape::Listing,
                "claude-code",
                &paths.map(str::to_owned),
            )),
            "priced at the ceiling the journalled shape names"
        );

        // The answer that was only recorded: the same decision, not acted on.
        // Nothing was put in front of a model and nothing was avoided — and
        // now the row says which of the two zeroes it is.
        let observed = ServeAudit {
            tag: Some("answer_observed".to_owned()),
            declared_avoided: Some(0),
            frame: String::new(),
            ..decision(ServeAuditKind::SearchAnswered, None, "")
        };
        let (rows, _) = book(tmp.path(), &observed);
        assert_eq!(rows.len(), 1);
        let (verb, _, reason, returned, avoided) = &rows[0];
        assert_eq!(verb, "memd.search_answered");
        assert_eq!(reason, "answer_observed");
        assert_eq!((*returned, *avoided), (0, 0));
    }

    /// The load-bearing claim of the accounting: a decline books the REAL
    /// tokenizer count of the slice it declined as avoided, the count of the
    /// frame it sent instead as returned, and it books them in **both**
    /// relations — the one the savings feed sums and the one that says which
    /// channel the decision travelled on.
    #[test]
    fn a_deny_books_the_measured_slice_and_the_frame_in_both_relations() {
        assert_eq!(
            rift_tokens::provenance(),
            rift_tokens::TOKENIZER,
            "these figures are only claims if the real tokenizer is missing"
        );
        let (tmp, basis, body) = checkout(60);
        let frame = "You already hold src.rs lines 1-10 at this revision.";
        let audit = decision(ServeAuditKind::ReadIntercepted, Some(basis), frame);
        let slice: String = body.split_inclusive('\n').take(10).collect();

        let (audit_rows, call_rows) = book(tmp.path(), &audit);
        assert_eq!(audit_rows.len(), 1);
        let (verb, handle, reason, returned, avoided) = &audit_rows[0];
        assert_eq!(verb, "memd.read_intercepted");
        assert!(handle.starts_with("file:src.rs@sha256:"), "{handle}");
        assert!(handle.ends_with(":L1-10"), "{handle}");
        assert_eq!(reason, rift_recall_serve::TAG_ALREADY_HELD);
        assert_eq!(*returned, i64::from(rift_tokens::count(frame)));
        assert_eq!(*avoided, i64::from(rift_tokens::count(&slice)));
        assert!(
            *avoided > *returned,
            "a decline that costs more than it saves is not one"
        );

        assert_eq!(call_rows.len(), 1);
        let (channel, tool_name, call_returned, call_avoided) = &call_rows[0];
        assert_eq!(channel, "hook_inject");
        assert_eq!(
            tool_name, "claude-code:read-gate",
            "the family and the step"
        );
        assert_eq!((call_returned, call_avoided), (returned, avoided));
    }

    /// The same read, recorded and not acted on: the pool is counted in
    /// place, and nothing is claimed. Zero because the row has no field for
    /// anything else, on both sides.
    #[test]
    fn an_observe_row_books_zero_avoided() {
        let (tmp, basis, _) = checkout(60);
        let audit = decision(ServeAuditKind::ReadRepeatObserved, Some(basis), "");
        let (audit_rows, call_rows) = book(tmp.path(), &audit);
        assert_eq!(audit_rows[0].0, "memd.read_repeat_observed");
        assert!(
            audit_rows[0].1.ends_with(":L1-10"),
            "the slice is still named"
        );
        assert_eq!((audit_rows[0].3, audit_rows[0].4), (0, 0));
        assert_eq!((call_rows[0].2, call_rows[0].3), (0, 0));
    }

    /// The bytes the decision named are gone — the file moved on and there is
    /// no object database to fall back to — so the avoided figure is zero and
    /// the row says WHY rather than passing a guess off as a measurement.
    #[test]
    fn basis_unavailable_books_zero_with_the_tag() {
        let (tmp, basis, _) = checkout(60);
        std::fs::write(tmp.path().join("src.rs"), "the file moved on\n").expect("a rewrite");
        let frame = "You already hold these lines.";
        let audit = decision(ServeAuditKind::ReadIntercepted, Some(basis), frame);
        let (audit_rows, _) = book(tmp.path(), &audit);
        assert_eq!(audit_rows[0].2, rift_recall_serve::TAG_BASIS_UNAVAILABLE);
        assert_eq!(
            audit_rows[0].4, 0,
            "nothing was counted, so nothing is claimed"
        );
        assert_eq!(
            audit_rows[0].3,
            i64::from(rift_tokens::count(frame)),
            "the frame still cost what it cost"
        );
    }

    /// A capped search avoided nothing: the search ran, and Rift put nothing
    /// in its output. What the cap produced is the total the tool reported,
    /// and the row carries it in the identity a reader parses back.
    #[test]
    fn a_grep_cap_books_no_avoided_tokens_and_records_the_total() {
        let tmp = tempfile::tempdir().expect("a work tree");
        let mut audit = decision(ServeAuditKind::GrepCapped, None, "");
        audit.total = Some(137);
        let (audit_rows, call_rows) = book(tmp.path(), &audit);
        assert_eq!(audit_rows[0].0, "memd.grep_capped");
        assert_eq!(audit_rows[0].1, "grep:matches=137");
        assert_eq!(
            rift_recall_serve::grep_cap_matches(&audit_rows[0].1),
            Some(137)
        );
        assert_eq!((audit_rows[0].3, audit_rows[0].4), (0, 0));
        assert_eq!(call_rows[0].1, "claude-code:search-gate");
        assert_eq!((call_rows[0].2, call_rows[0].3), (0, 0));
    }

    /// A document decision books the figure its own record declares, with the
    /// tag that keeps a declaration from reading as a measurement — and the
    /// basis is still what names the document.
    #[test]
    fn a_declared_baseline_books_its_figure_under_the_tag_that_says_so() {
        let (tmp, mut basis, _) = checkout(20);
        basis.lines = None;
        let mut audit = decision(ServeAuditKind::DocRewritten, Some(basis), "the sidecar");
        audit.declared_avoided = Some(4_200);
        let (audit_rows, call_rows) = book(tmp.path(), &audit);
        assert_eq!(audit_rows[0].0, "memd.doc_rewritten");
        assert!(audit_rows[0].1.starts_with("file:src.rs@sha256:"));
        assert!(
            !audit_rows[0].1.contains(":L"),
            "a document has no line range"
        );
        assert_eq!(audit_rows[0].2, rift_recall_serve::TAG_DECLARED_BASELINE);
        assert_eq!(audit_rows[0].4, 4_200);
        assert_eq!(call_rows[0].1, "claude-code:doc-gate");
    }

    /// A declared figure is a note on the number, not on the decision: when
    /// the decision carries its own tag, the row keeps that tag. A dropped
    /// substitution is booked as a dropped substitution, and a decline that
    /// named the sidecar says that — both with the zero they claim, never a
    /// tokenized container.
    #[test]
    fn a_decisions_own_tag_outranks_a_declared_baseline_but_not_a_failed_measurement() {
        let (tmp, mut basis, _) = checkout(20);
        basis.lines = None;
        let mut dropped = decision(ServeAuditKind::DocDeferred, Some(basis.clone()), "");
        dropped.tag = Some("rewrite_dropped".to_owned());
        dropped.declared_avoided = Some(0);
        let (rows, _) = book(tmp.path(), &dropped);
        assert_eq!(rows[0].0, "memd.doc_deferred");
        assert_eq!(
            rows[0].2, "rewrite_dropped",
            "the drop is what the row says"
        );
        assert_eq!((rows[0].3, rows[0].4), (0, 0));

        let mut redirected = decision(ServeAuditKind::DocRewritten, Some(basis.clone()), "a frame");
        redirected.tag = Some("redirect_unverified".to_owned());
        redirected.declared_avoided = Some(0);
        let (rows, _) = book(tmp.path(), &redirected);
        assert_eq!(rows[0].2, "redirect_unverified");
        assert_eq!(rows[0].4, 0, "a decline claims nothing");
        assert!(rows[0].3 > 0, "the frame it cost is still booked");

        // A measurement that FAILED still outranks the decision's tag.
        let mut unmeasurable = decision(ServeAuditKind::DocRewritten, Some(basis), "a frame");
        unmeasurable.tag = Some("redirect_unverified".to_owned());
        unmeasurable.declared_avoided = None;
        std::fs::remove_file(tmp.path().join("src.rs")).unwrap();
        let (rows, _) = book(tmp.path(), &unmeasurable);
        assert_eq!(rows[0].2, rift_recall_serve::TAG_BASIS_UNAVAILABLE);
    }

    /// ★ A substitution books the document's declared native cost, computed
    /// where the tokenizer is allowed to run.
    ///
    /// The hook asks for the declaration and never makes it — a decision has
    /// an agent waiting on it — so the row the worker writes is where the
    /// figure appears, and it is the same formula the document verb serves
    /// under. Asserted against that function rather than against a number, so
    /// the two surfaces cannot drift into two baselines.
    #[test]
    fn a_substitution_books_the_documents_declared_native_cost() {
        let tmp = tempfile::tempdir().expect("a work tree");
        // Bytes with no text in them: what a client hands a model when it
        // opens a container it cannot parse, and dearer per character than
        // anything readable.
        let mut container = Vec::new();
        let mut seed: u64 = 0x9E37_79B9_7F4A_7C15;
        for _ in 0..80_000 {
            seed = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            container.push((seed >> 33) as u8);
        }
        std::fs::write(tmp.path().join("plan.docx"), &container).expect("the document");
        let (blob, _) = rift_recall_substrate::worktree::hash_blob_bytes(tmp.path(), "plan.docx")
            .expect("the bytes");
        let basis = AuditBasis {
            rel_path: "plan.docx".to_owned(),
            content_sha256: blob.content_sha256,
            // Empty exactly as the gate journals it: the declaration is over
            // the working file under the hash recorded with it.
            blob_oid: String::new(),
            lines: None,
        };
        let mut audit = decision(ServeAuditKind::DocRewritten, Some(basis), "");
        audit.tag = Some(rift_recall_serve::TAG_DECLARED_BASELINE.to_owned());
        audit.declared_avoided = None;
        let (rows, calls) = book(tmp.path(), &audit);
        let declared = rift_recall_serve::declared_document_baseline("docx", &container);
        assert!(declared > 0, "the probe declares something: {declared}");
        assert_eq!(rows[0].0, "memd.doc_rewritten");
        assert_eq!(rows[0].2, rift_recall_serve::TAG_DECLARED_BASELINE);
        assert_eq!(rows[0].4, i64::from(declared));
        assert_eq!(rows[0].3, 0, "a substitution pushes no frame");
        assert_eq!(calls[0].1, "claude-code:doc-gate");

        // The same decision about bytes the file no longer holds declares
        // nothing: those are bytes the agent never opened.
        std::fs::write(tmp.path().join("plan.docx"), b"different").expect("the document");
        let (rows, _) = book(tmp.path(), &audit);
        assert_eq!(rows[0].2, rift_recall_serve::TAG_BASIS_UNAVAILABLE);
        assert_eq!(rows[0].4, 0);
    }
}

/// What a serve envelope may carry into a shard, and where its line lands.
#[cfg(test)]
mod serve_journal_tests {
    use proptest::prelude::*;
    use rift_recall_serve::{MemdEvent, ServeChannel};
    use serde_json::Value;

    use super::frozen::WITHHELD;
    use super::*;

    /// Every event kind the type has, with `text` in every string member.
    fn every_event(text: &str) -> Vec<MemdEvent> {
        let mut events = counted_events();
        events.extend(text_events(text));
        events
    }

    /// The kinds that carry counts and nothing else.
    fn counted_events() -> Vec<MemdEvent> {
        vec![
            MemdEvent::ManifestServed {
                estimated_tokens_returned: 3,
                estimated_tokens_avoided: 9,
            },
            MemdEvent::ManifestInjected {
                estimated_tokens_returned: 3,
            },
            MemdEvent::PackBuilt {
                sections: 2,
                total_tokens: 7,
                estimated_tokens_returned: 3,
                estimated_tokens_avoided: 9,
            },
            MemdEvent::RecoveryInjected {
                estimated_tokens_returned: 3,
            },
            MemdEvent::SearchServed {
                estimated_tokens_returned: 3,
                estimated_tokens_avoided: 9,
            },
        ]
    }

    /// The kinds that carry a string member, each holding `text`.
    fn text_events(text: &str) -> Vec<MemdEvent> {
        let s = || text.to_owned();
        vec![
            MemdEvent::SummaryServed {
                handle: s(),
                estimated_tokens_returned: 3,
                estimated_tokens_avoided: 9,
            },
            MemdEvent::ExactRetrieved {
                handle: s(),
                reason: s(),
                estimated_tokens_returned: 3,
            },
            MemdEvent::Invalidated {
                handle: s(),
                estimated_tokens_returned: 3,
            },
            MemdEvent::Refused {
                target: s(),
                reason: s(),
                estimated_tokens_returned: 3,
            },
            MemdEvent::ReadIntercepted {
                handle: s(),
                reason: s(),
                estimated_tokens_returned: 3,
                estimated_tokens_avoided: 9,
            },
            MemdEvent::ReadRepeatObserved {
                handle: s(),
                reason: s(),
            },
            MemdEvent::ReadPrevented {
                handle: s(),
                reason: s(),
                estimated_tokens_avoided: 9,
            },
            MemdEvent::SearchAnswered {
                reason: s(),
                estimated_tokens_returned: 3,
                estimated_tokens_avoided: 9,
            },
            MemdEvent::GrepCapped { handle: s() },
            MemdEvent::DocRewritten {
                handle: s(),
                reason: s(),
                estimated_tokens_returned: 3,
                estimated_tokens_avoided: 9,
            },
            MemdEvent::DocDeferred {
                handle: s(),
                reason: s(),
                estimated_tokens_returned: 3,
            },
            MemdEvent::DocServed {
                handle: s(),
                reason: s(),
                estimated_tokens_returned: 3,
                estimated_tokens_avoided: 9,
            },
        ]
    }

    /// Both envelopes one event becomes, schema 1 then schema 4.
    fn envelopes(event: &MemdEvent) -> [String; 2] {
        let at = rift_core::Timestamp::from_millis(1_700_000_000_000);
        [
            event.to_hook_json_keyed("call-1"),
            event.to_serve_call_json(
                ServeChannel::HookInject,
                "call-1",
                "claude-code:read-gate",
                at,
            ),
        ]
    }

    fn frozen(envelope: &str) -> Value {
        let line = canonical_serve_line(envelope.as_bytes()).expect("a declared envelope freezes");
        serde_json::from_slice(&line).unwrap()
    }

    /// Every kind, both schemas: one line, keys sorted (re-serialising it is
    /// the identity), the key and the verb the envelope carried, and a line
    /// that is its own canonical form.
    #[test]
    fn every_event_kind_freezes_to_a_sorted_line_that_keeps_its_key() {
        let file = format!("file:src/a.rs@sha256:{}:L1-4", "a".repeat(64));
        for event in every_event(&file) {
            for envelope in envelopes(&event) {
                let line = canonical_serve_line(envelope.as_bytes()).expect("freezes");
                let value: Value = serde_json::from_slice(&line).unwrap();
                assert_eq!(serde_json::to_vec(&value).unwrap(), line, "keys sorted");
                let original: Value = serde_json::from_str(&envelope).unwrap();
                assert_eq!(value["idempotency_key"], original["idempotency_key"]);
                assert_eq!(value["verb"], original["verb"]);
                assert_eq!(
                    canonical_serve_line(&line),
                    Some(line.clone()),
                    "idempotent"
                );
                assert!(!line.contains(&b'\n'), "one line");
            }
        }
    }

    /// The caller's reason and a refusal's typed target are withheld; Rift's
    /// own handles, tags and the grant tag are kept.
    #[test]
    fn typed_text_is_withheld_and_rifts_own_vocabulary_is_kept() {
        let exact = MemdEvent::ExactRetrieved {
            handle: format!("file:src/a.rs@sha256:{}", "b".repeat(64)),
            reason: "because the customer password is hunter2".to_owned(),
            estimated_tokens_returned: 1,
        };
        let line = frozen(&exact.to_hook_json_keyed("k"));
        assert_eq!(line["payload"]["reason"], WITHHELD);
        assert_eq!(line["payload"]["handle"], exact_handle(&exact));
        let granted = rift_recall_serve::grant_tagged_envelope(&exact, "k");
        assert_eq!(
            frozen(&granted)["payload"]["reason"],
            rift_recall_serve::CROSS_BRANCH_GRANT_TAG
        );
        let served = MemdEvent::ManifestServed {
            estimated_tokens_returned: 1,
            estimated_tokens_avoided: 2,
        };
        let granted = rift_recall_serve::grant_tagged_envelope(&served, "k");
        assert_eq!(
            frozen(&granted)["payload"]["reason"],
            rift_recall_serve::CROSS_BRANCH_GRANT_TAG
        );

        let refused = |target: &str| MemdEvent::Refused {
            target: target.to_owned(),
            reason: "policy_denied_exact".to_owned(),
            estimated_tokens_returned: 1,
        };
        let typed = frozen(&refused("secrets/prod.env please").to_hook_json_keyed("k"));
        assert_eq!(typed["payload"]["target"], WITHHELD);
        assert_eq!(typed["payload"]["reason"], "policy_denied_exact");
        let citation = format!("pack:{}/evidence:3", "c".repeat(64));
        assert_eq!(
            frozen(&refused(&citation).to_hook_json_keyed("k"))["payload"]["target"],
            citation.as_str()
        );

        let odd_tag = MemdEvent::SearchAnswered {
            reason: "Not A Tag".to_owned(),
            estimated_tokens_returned: 1,
            estimated_tokens_avoided: 1,
        };
        assert_eq!(
            frozen(&odd_tag.to_hook_json_keyed("k"))["payload"]["reason"],
            WITHHELD
        );
    }

    fn exact_handle(event: &MemdEvent) -> String {
        match event {
            MemdEvent::ExactRetrieved { handle, .. } => handle.clone(),
            _ => String::new(),
        }
    }

    /// A member nobody audited refuses the whole line, at every level, and so
    /// do the retired schemas and a call record the appender would not book.
    #[test]
    fn a_line_with_a_member_nobody_audited_is_refused() {
        let event = MemdEvent::GrepCapped {
            handle: "grep:matches=4".to_owned(),
        };
        let [audited, called] = envelopes(&event);
        let plant = |envelope: &str, pointer: &str| {
            let mut value: Value = serde_json::from_str(envelope).unwrap();
            value
                .pointer_mut(pointer)
                .unwrap()
                .as_object_mut()
                .unwrap()
                .insert("note".into(), "hi".into());
            value.to_string()
        };
        for planted in [
            plant(&audited, ""),
            plant(&audited, "/payload"),
            plant(&called, "/serve_call"),
        ] {
            assert_eq!(canonical_serve_line(planted.as_bytes()), None, "{planted}");
        }
        let edit = |envelope: &str, pointer: &str, to: Value| {
            let mut value: Value = serde_json::from_str(envelope).unwrap();
            *value.pointer_mut(pointer).unwrap() = to;
            value.to_string()
        };
        for refused in [
            edit(&audited, "/schema", 2.into()),
            edit(&audited, "/verb", "memd.refused".into()),
            edit(&called, "/serve_call/call_id", "another".into()),
            edit(&called, "/serve_call/channel", "mcp".into()),
            edit(&audited, "/idempotency_key", "a key with spaces".into()),
        ] {
            assert_eq!(canonical_serve_line(refused.as_bytes()), None, "{refused}");
        }
        assert_eq!(canonical_serve_line(b"not json"), None);
    }

    /// Every string value in a parsed line, at any depth.
    fn strings_in(value: &Value, out: &mut Vec<String>) {
        match value {
            Value::String(text) => out.push(text.clone()),
            Value::Object(members) => members.values().for_each(|v| strings_in(v, out)),
            Value::Array(items) => items.iter().for_each(|v| strings_in(v, out)),
            _ => (),
        }
    }

    proptest! {
        /// Whatever text reaches a member that can hold it, no run of it longer
        /// than a hash survives into the line: every string left is a declared
        /// member's own value — a key, a verb, a tag, a handle Rift composed —
        /// or the placeholder.
        #[test]
        fn no_typed_text_longer_than_a_hash_survives(
            words in "[ -~]{0,20}",
            run in "[!-~]{65,90}",
        ) {
            let text = format!("{words} {run}");
            for event in every_event(&text) {
                for envelope in envelopes(&event) {
                    let line = canonical_serve_line(envelope.as_bytes()).expect("freezes");
                    let mut strings = Vec::new();
                    strings_in(&serde_json::from_slice(&line).unwrap(), &mut strings);
                    prop_assert!(strings.iter().all(|s| !s.contains(&run)), "{strings:?}");
                }
            }
        }
    }
}
