//! The serve-audit funnel: `rift hooks memd <verb>` and the queued serve-audit
//! job both append recall serve-decision events to the per-repo `recall.db`
//! audit relation. Separate because the rows it writes are what the savings
//! ledger, doctor and the dashboard read back; one module owns their shape.
//!
//! It also owns the frozen form of those rows. A serve made inside a session
//! is written with the row that names the session, and the session's next
//! checkpoint freezes every serve that row names into its shard, rebuilt from
//! the rows by [`session_serve_lines`]: the projection is rebuildable and a
//! rebuild deletes it, the shard is not, and the store is the one record the
//! artifact is read from.

use std::io::Read as _;
use std::path::Path;

use rift_core::Result;
use rift_recall_ledger::attribution::{
    append_attributed_serve, attribute_booked_serve, ServeAttribution,
};
use rift_store::CanonicalStore as _;

use super::now_ms;

mod frozen;

pub(crate) use rift_recall_ledger::audit::append_memd_audit;

pub(crate) use frozen::session_serve_lines;

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
    let db = crate::family::recall_db(&repo.work_tree).map_err(|error| {
        crate::capture_queue::proof_read(error, "serve proof-store location is unavailable")
    })?;
    let store = t
        .phase("open_recall", || {
            rift_store::open_migrated(&db, &rift_store::recall_migrations())
        })
        .map_err(|error| {
            crate::capture_queue::proof_read(error, "serve prior-key proof is unavailable")
        })?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .map_err(rift_core::RiftError::from)?;
    // The decision's own clock, not the worker's: a row deferred by an hour
    // still records when the agent's turn spent (or saved) the tokens.
    let (audited, called) = serve_audit_envelopes(&event, audit, job.queued_at_ms);
    let attribution = serve_attribution(audit.session.as_deref(), audit.window.as_deref());
    let expected = [
        expected_serve_row(&audited, job.queued_at_ms)?,
        expected_serve_row(&called, job.queued_at_ms)?,
    ];
    rt.block_on(async {
        for row in &expected {
            row.require_absent(&store).await?;
        }
        for envelope in [audited, called] {
            append_attributed_serve(
                &store,
                envelope.as_bytes(),
                job.queued_at_ms,
                attribution.as_ref(),
            )
            .await?;
        }
        for row in &expected {
            row.require_exact(&store).await?;
        }
        if let Some(attribution) = &attribution {
            require_serve_attribution(&store, &expected[0].key, "memd_audit_event", attribution)
                .await?;
            require_serve_attribution(&store, &expected[1].key, "serve_call_event", attribution)
                .await?;
        }
        Ok(())
    })
}

/// Attribute one skill-CLI serve to the session whose tool result named its
/// keys: one `memd_audit_session` row per key its relation holds.
///
/// A family with no `recall.db` holds no serve, so nothing is opened (and no
/// projection is minted) for it; a session id the capture plane would refuse
/// attributes nothing, for the reason [`serve_attribution`] gives.
pub(super) fn attribute_serve_job(
    t: &crate::hooktime::HookTimer,
    job: &crate::agent_queue::AgentJob,
    claim: &crate::agent_queue::ServeClaim,
) -> Result<()> {
    let Some(attribution) = serve_attribution(Some(&claim.session), claim.window.as_deref()) else {
        return Ok(());
    };
    let repo = rift_git::discover(&job.work_tree)?;
    let db = crate::family::recall_db(&repo.work_tree).map_err(|error| {
        crate::capture_queue::proof_read(error, "serve proof-store location is unavailable")
    })?;
    let Some(store) = t
        .phase("open_recall", || {
            rift_store::open_existing_migrated(&db, &rift_store::recall_migrations())
        })
        .map_err(|error| {
            crate::capture_queue::proof_read(error, "serve attribution proof is unavailable")
        })?
    else {
        return Ok(());
    };
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .map_err(rift_core::RiftError::from)?;
    rt.block_on(async {
        for key in [&claim.audit_id, &claim.call_id] {
            let rows = store
                .query(
                    "SELECT serve_key FROM memd_audit_session WHERE serve_key = ?1",
                    &[rift_store::Value::text(key)],
                )
                .await
                .map_err(|error| {
                    crate::capture_queue::proof_read(
                        error,
                        "serve attribution prior-key proof is unavailable",
                    )
                })?;
            if !rows.is_empty() {
                return Err(crate::capture_queue::deferred(
                    "serve attribution input is already recorded without complete job proof",
                ));
            }
        }
        attribute_booked_serve(&store, &claim.audit_id, &claim.call_id, &attribution).await?;
        for (key, relation) in [
            (&claim.audit_id, "memd_audit_event"),
            (&claim.call_id, "serve_call_event"),
        ] {
            require_serve_attribution(&store, key, relation, &attribution).await?;
        }
        Ok(())
    })
}

/// Expected immutable facts derived from the job's own rendered envelope.
struct ExpectedServeRow {
    key: String,
    query: &'static str,
    row: rift_store::Row,
}

impl ExpectedServeRow {
    async fn require_absent(&self, store: &rift_store::SqliteStore) -> Result<()> {
        let rows = store
            .query(self.query, &[rift_store::Value::text(&self.key)])
            .await
            .map_err(|error| {
                crate::capture_queue::proof_read(error, "serve persisted-row proof is unavailable")
            })?;
        if !rows.is_empty() {
            return Err(crate::capture_queue::deferred(
                "serve key lacks complete replay input proof",
            ));
        }
        let associations = store
            .query(
                "SELECT serve_key FROM memd_audit_session WHERE serve_key = ?1",
                &[rift_store::Value::text(&self.key)],
            )
            .await
            .map_err(|error| {
                crate::capture_queue::proof_read(
                    error,
                    "serve association prior-key proof is unavailable",
                )
            })?;
        if !associations.is_empty() {
            return Err(crate::capture_queue::deferred(
                "serve key has an existing association without exact input proof",
            ));
        }
        Ok(())
    }

    async fn require_exact(&self, store: &rift_store::SqliteStore) -> Result<()> {
        let rows = store
            .query(self.query, &[rift_store::Value::text(&self.key)])
            .await
            .map_err(|error| {
                crate::capture_queue::proof_read(error, "serve persisted-row proof is unavailable")
            })?;
        if rows != [self.row.clone()] {
            return Err(crate::capture_queue::deferred(
                "serve key conflicts with actual persisted facts",
            ));
        }
        Ok(())
    }
}

fn json_field<'a>(object: &'a serde_json::Value, name: &str) -> &'a serde_json::Value {
    object.get(name).unwrap_or(&serde_json::Value::Null)
}

fn expected_serve_row(envelope: &str, now: i64) -> Result<ExpectedServeRow> {
    use rift_store::Value;
    let env: serde_json::Value = serde_json::from_str(envelope)
        .map_err(|_| crate::capture_queue::deferred("serve envelope is unavailable"))?;
    let key = json_field(&env, "idempotency_key")
        .as_str()
        .ok_or_else(|| crate::capture_queue::deferred("serve key is unavailable"))?
        .to_owned();
    let verb = Value::text(json_field(&env, "verb").as_str().unwrap_or_default());
    let payload = json_field(&env, "payload");
    let optional = |value: &serde_json::Value| value.as_str().map_or(Value::Null, Value::text);
    let count = |value: &serde_json::Value| Value::Int(value.as_i64().unwrap_or(0));
    let (query, values) = if json_field(&env, "schema").as_u64() == Some(4) {
        let call = json_field(&env, "serve_call");
        ("SELECT call_id, channel, tool_name, terminal_verb, tokens_returned, tokens_avoided, recorded_at, schema_version FROM serve_call_event WHERE call_id = ?1",
            vec![Value::text(&key), optional(json_field(call, "channel")), optional(json_field(call, "tool_name")), verb,
                count(json_field(call, "tokens_returned")), count(json_field(call, "tokens_avoided")), count(json_field(call, "recorded_at")), Value::Int(1)])
    } else {
        let handle = payload
            .get("handle")
            .or_else(|| payload.get("target"))
            .unwrap_or(&serde_json::Value::Null);
        ("SELECT audit_id, verb, handle, reason, tokens_returned, tokens_avoided, recorded_at, schema_version FROM memd_audit_event WHERE audit_id = ?1",
            vec![Value::text(&key), verb, optional(handle), optional(json_field(payload, "reason")),
                count(json_field(payload, "estimated_tokens_returned")), count(json_field(payload, "estimated_tokens_avoided")), Value::Int(now), Value::Int(1)])
    };
    Ok(ExpectedServeRow {
        key,
        query,
        row: rift_store::Row::new(values),
    })
}

async fn require_serve_attribution(
    store: &rift_store::SqliteStore,
    key: &str,
    relation: &str,
    attribution: &ServeAttribution,
) -> Result<()> {
    use rift_store::Value;
    let rows = store.query("SELECT serve_key, relation, session_id, window_key, schema_version FROM memd_audit_session WHERE serve_key = ?1 AND relation = ?2",
        &[Value::text(key), Value::text(relation)]).await.map_err(|error| crate::capture_queue::proof_read(error, "serve attribution readback is unavailable"))?;
    let expected = rift_store::Row::new(vec![
        Value::text(key),
        Value::text(relation),
        Value::text(&attribution.session_id),
        attribution
            .window_key
            .as_deref()
            .map_or(Value::Null, Value::text),
        Value::Int(1),
    ]);
    if rows != [expected] {
        return Err(crate::capture_queue::deferred(
            "serve attribution is absent or conflicts",
        ));
    }
    Ok(())
}

/// The attribution a hook-side serve is written with: its session when the
/// id is one the capture plane accepts, and the window key when it is one.
///
/// Screened here, at the writer, because a line that names an unsafe session
/// is refused whole when it is frozen: a serve whose session cannot be named
/// is written unattributed rather than attributed and then lost. A job an
/// older binary queued names no session, and is unattributed too.
pub(crate) fn serve_attribution(
    session: Option<&str>,
    window_key: Option<&str>,
) -> Option<ServeAttribution> {
    let session_id = session.filter(|id| rift_shadow::hooks::is_safe_session_id(id))?;
    Some(ServeAttribution {
        session_id: session_id.to_owned(),
        window_key: window_key
            .filter(|key| frozen::is_window_key(key))
            .map(str::to_owned),
    })
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
        K::ReadIntercepted | K::ReadRepeatObserved | K::ReadFollowUp => "read-gate",
        // Its own step, not the read gate's: the per-family table is read for
        // what each lever is worth, and a figure folded into another lever's
        // step is a figure nobody can separate again.
        K::ReadPrevented | K::ReadPreventedObserved => "prevented",
        K::ToolOutputHeld => "output-held",
        K::RecoveryInjected => "recovery-map",
        K::SearchAnswered | K::GrepCapped => "search-gate",
        K::DocRewritten | K::DocDeferred => "doc-gate",
    }
}

/// The tag a decision of this kind carries when it names none of its own.
fn serve_audit_default_tag(kind: crate::agent_queue::ServeAuditKind) -> &'static str {
    use crate::agent_queue::ServeAuditKind as K;
    use rift_recall_serve::{
        TAG_ALREADY_HELD, TAG_CONVERT_DEADLINE, TAG_DECLARED_BASELINE, TAG_FAMILY_FOLLOW_UP,
        TAG_PREVENTED, TAG_PREVENTED_OBSERVED,
    };
    match kind {
        K::ReadIntercepted | K::ReadRepeatObserved => TAG_ALREADY_HELD,
        K::ReadFollowUp => TAG_FAMILY_FOLLOW_UP,
        K::ReadPrevented => TAG_PREVENTED,
        K::ReadPreventedObserved => TAG_PREVENTED_OBSERVED,
        K::DocRewritten => TAG_DECLARED_BASELINE,
        K::DocDeferred => TAG_CONVERT_DEADLINE,
        K::RecoveryInjected | K::SearchAnswered | K::GrepCapped | K::ToolOutputHeld => "",
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
    if matches!(
        audit.decision,
        crate::agent_queue::ServeAuditKind::ReadPreventedObserved
            | crate::agent_queue::ServeAuditKind::ToolOutputHeld
    ) {
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
    if audit.decision == crate::agent_queue::ServeAuditKind::ReadFollowUp {
        return follow_up_figures(work_tree, audit, returned);
    }
    let Some(basis) = audit.basis.as_ref() else {
        return (returned, 0, None);
    };
    match audit_slice_tokens(work_tree, basis) {
        Some(avoided) => (returned, avoided, None),
        None => (returned, 0, Some(TAG_BASIS_UNAVAILABLE)),
    }
}

/// What a read charged against an earlier decline books: the bytes it
/// delivered, measured, as **returned**, and nothing avoided.
///
/// Returned rather than a negative avoided figure, because a row is never
/// edited and a figure is never below zero: every surface nets avoided less
/// returned, so the cost lands on the same line as the credit it qualifies.
/// Bytes that can no longer be identified book nothing and say so — the cost
/// is then missing, which this tag is what makes visible.
fn follow_up_figures(
    work_tree: &Path,
    audit: &crate::agent_queue::ServeAudit,
    frame: u32,
) -> (u32, u32, Option<&'static str>) {
    use rift_recall_serve::TAG_BASIS_UNAVAILABLE;
    let Some(basis) = audit.basis.as_ref() else {
        return (frame, 0, Some(TAG_BASIS_UNAVAILABLE));
    };
    match audit_slice_tokens(work_tree, basis) {
        Some(read) => (frame.saturating_add(read), 0, None),
        None => (frame, 0, Some(TAG_BASIS_UNAVAILABLE)),
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
        K::ReadIntercepted | K::ReadFollowUp => MemdEvent::ReadIntercepted {
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
        K::ToolOutputHeld => MemdEvent::ToolOutputHeld {
            handle,
            would_save_bytes: audit.total.unwrap_or_default(),
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
/// so writer and reader cannot disagree about the file. No envelope schema
/// names a session, so the row is written unattributed.
fn record_memd_event() -> Result<()> {
    let mut body = Vec::new();
    std::io::stdin().read_to_end(&mut body)?;
    let cwd = std::env::current_dir()?;
    let db = crate::family::recall_db(&crate::worktree_root(&cwd))?;
    let store = rift_store::open_migrated(&db, &rift_store::recall_migrations())?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .map_err(rift_core::RiftError::from)?;
    rt.block_on(append_memd_audit(&store, &body, now_ms()))
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
            window: None,
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

    /// ★ A read of bytes a family decline was credited with books what it
    /// delivered as **returned**, measured, and nothing avoided — in both
    /// relations, under its own tag — so every net of avoided less returned
    /// takes it off the decline it is charged against.
    #[test]
    fn a_family_follow_up_books_the_measured_slice_as_returned_and_nothing_avoided() {
        assert_eq!(rift_tokens::provenance(), rift_tokens::TOKENIZER);
        let (tmp, basis, body) = checkout(60);
        let audit = ServeAudit {
            tag: Some(rift_recall_serve::TAG_FAMILY_FOLLOW_UP.to_owned()),
            ..decision(ServeAuditKind::ReadFollowUp, Some(basis), "")
        };
        let slice: String = body.split_inclusive('\n').take(10).collect();
        let (audit_rows, call_rows) = book(tmp.path(), &audit);
        assert_eq!(audit_rows.len(), 1);
        let (verb, handle, reason, returned, avoided) = &audit_rows[0];
        assert_eq!(verb, "memd.read_intercepted");
        assert!(handle.ends_with(":L1-10"), "{handle}");
        assert_eq!(reason, rift_recall_serve::TAG_FAMILY_FOLLOW_UP);
        assert_eq!(*returned, i64::from(rift_tokens::count(&slice)));
        assert!(*returned > 0);
        assert_eq!(*avoided, 0);
        assert_eq!(call_rows[0].1, "claude-code:read-gate");
        assert_eq!((call_rows[0].2, call_rows[0].3), (*returned, 0));
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

    /// Held output observed is its own verb on its own step: the bytes a
    /// frame would spare ride in the row, and no token figure is claimed.
    #[test]
    fn audit_books_held_output_as_its_own_verb_with_the_bytes_it_would_spare_and_no_tokens() {
        let (tmp, basis, _) = checkout(20);
        let mut held = decision(ServeAuditKind::ToolOutputHeld, Some(basis), "");
        held.total = Some(740);
        let event = serve_audit_event(tmp.path(), &held);
        assert!(
            matches!(
                &event,
                rift_recall_serve::MemdEvent::ToolOutputHeld {
                    would_save_bytes: 740,
                    ..
                }
            ),
            "{event:?}"
        );
        let (rows, calls) = book(tmp.path(), &held);
        assert_eq!(rows[0].0, "memd.tool_output_held");
        assert_eq!(
            (rows[0].3, rows[0].4),
            (0, 0),
            "an observed lever claims nothing"
        );
        assert!(calls[0].1.ends_with(":output-held"), "{calls:?}");
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

    fn queued_audit_job(
        work_tree: &Path,
        kind: crate::agent_queue::JobKind,
        at: i64,
    ) -> crate::agent_queue::AgentJob {
        crate::agent_queue::AgentJob {
            schema_version: 1,
            work_tree: work_tree.to_path_buf(),
            queued_at_ms: at,
            attempts: 0,
            kind,
        }
    }

    fn attribution_rows(work_tree: &Path) -> Vec<Vec<String>> {
        let db = crate::family::recall_db(work_tree).unwrap();
        let store = rift_store::open_migrated(&db, &rift_store::recall_migrations()).unwrap();
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        rt.block_on(store.query(
            "SELECT serve_key, relation, session_id, COALESCE(window_key, '') FROM memd_audit_session ORDER BY relation, serve_key", &[],
        )).unwrap().iter().map(|row| (0..4).map(|n| row.get_text(n).unwrap()).collect()).collect()
    }

    fn drain_audit_job(job: &crate::agent_queue::AgentJob) -> std::path::PathBuf {
        let git_dir = job.work_tree.join(".git");
        let path = crate::agent_queue::enqueue(&git_dir, job).unwrap();
        super::super::worker::drain_jobs(git_dir.to_str(), super::super::worker::LockWait::None);
        path
    }

    fn audit_collision_setup() -> (tempfile::TempDir, ServeAudit, Vec<Vec<String>>) {
        let tmp = tempfile::tempdir().unwrap();
        rift_git::test_support::fake_repo(tmp.path()).unwrap();
        let mut audit = decision(ServeAuditKind::ReadPreventedObserved, None, "");
        audit.session = Some("session-a".into());
        let first = queued_audit_job(
            tmp.path(),
            crate::agent_queue::JobKind::ServeAudit(audit.clone()),
            1000,
        );
        assert!(
            !drain_audit_job(&first).exists(),
            "fresh A completes before B is queued"
        );
        let rows = attribution_rows(tmp.path());
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|row| row[2] == "session-a"));
        (tmp, audit, rows)
    }

    fn canonical_audit_facts(work_tree: &Path) -> Vec<Vec<rift_store::Row>> {
        let db = crate::family::recall_db(work_tree).unwrap();
        let store = rift_store::open_migrated(&db, &rift_store::recall_migrations()).unwrap();
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        [
            "SELECT * FROM memd_audit_event ORDER BY audit_id",
            "SELECT * FROM serve_call_event ORDER BY call_id",
            "SELECT * FROM memd_audit_session ORDER BY relation, serve_key",
        ]
        .iter()
        .map(|sql| rt.block_on(store.query(sql, &[])).unwrap())
        .collect()
    }

    fn assert_audit_collision_retained(job: &crate::agent_queue::AgentJob, rows: &[Vec<String>]) {
        let facts = canonical_audit_facts(&job.work_tree);
        let git_dir = job.work_tree.join(".git");
        let path = crate::agent_queue::enqueue(&git_dir, job).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(
            crate::agent_queue::jobs(&git_dir).len(),
            1,
            "B is the sole owed context"
        );
        for _ in 0..2 {
            super::super::worker::drain_jobs(
                git_dir.to_str(),
                super::super::worker::LockWait::None,
            );
            assert_eq!(canonical_audit_facts(&job.work_tree), facts);
            assert!(path.exists(), "conflicting B remains owed");
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
            assert_eq!(crate::agent_queue::read_job(&path).unwrap().1.attempts, 0);
            assert_eq!(attribution_rows(&job.work_tree), rows);
        }
    }

    #[test]
    fn pending_context_serve_audit_cannot_consume_another_session_after_a_completed() {
        let (tmp, mut audit, rows) = audit_collision_setup();
        audit.session = Some("session-b".into());
        let second = queued_audit_job(
            tmp.path(),
            crate::agent_queue::JobKind::ServeAudit(audit),
            2000,
        );
        assert_audit_collision_retained(&second, &rows);
    }

    #[test]
    fn pending_context_attribute_serve_cannot_consume_a_conflicting_completed_claim() {
        let (tmp, audit, rows) = audit_collision_setup();
        let audit_id = rows
            .iter()
            .find(|row| row[1] == "memd_audit_event")
            .unwrap()[0]
            .clone();
        let claim = crate::agent_queue::ServeClaim {
            session: "session-b".into(),
            window: None,
            call_id: audit.call_id,
            audit_id,
        };
        let second = queued_audit_job(
            tmp.path(),
            crate::agent_queue::JobKind::AttributeServe(claim),
            2000,
        );
        assert_audit_collision_retained(&second, &rows);
    }
    #[test]
    fn unavailable_audit_proof_retains_both_job_kinds_across_two_actual_drains() {
        for attributed in [false, true] {
            let tmp = tempfile::tempdir().unwrap();
            rift_git::test_support::fake_repo(tmp.path()).unwrap();
            let git_dir = tmp.path().join(".git");
            let db = crate::family::recall_db(tmp.path()).unwrap();
            std::fs::create_dir_all(db.parent().unwrap()).unwrap();
            std::fs::write(&db, b"unreadable-audit-proof-store").unwrap();
            let kind = if attributed {
                crate::agent_queue::JobKind::AttributeServe(crate::agent_queue::ServeClaim {
                    session: "s-proof".into(),
                    window: None,
                    call_id: "a".repeat(32),
                    audit_id: "b".repeat(64),
                })
            } else {
                crate::agent_queue::JobKind::ServeAudit(decision(
                    ServeAuditKind::ReadPreventedObserved,
                    None,
                    "",
                ))
            };
            let job = queued_audit_job(tmp.path(), kind, 1000);
            let path = crate::agent_queue::enqueue(&git_dir, &job).unwrap();
            let before = std::fs::read(&path).unwrap();
            let capture = rift_store::open_capture_projection(
                crate::hooks::projection_db_at(tmp.path()).unwrap(),
            )
            .unwrap();
            let rt = tokio::runtime::Builder::new_current_thread()
                .build()
                .unwrap();
            let checkpoints = rt
                .block_on(capture.query("SELECT * FROM checkpoint ORDER BY checkpoint_id", &[]))
                .unwrap();
            for _ in 0..2 {
                super::super::worker::drain_jobs(
                    git_dir.to_str(),
                    super::super::worker::LockWait::None,
                );
                assert_eq!(std::fs::read(&path).unwrap(), before);
                assert_eq!(crate::agent_queue::read_job(&path).unwrap().1.attempts, 0);
                // The unavailable recall store remains byte-identical; no row
                // can be inspected or claimed from its malformed bytes.
                assert_eq!(std::fs::read(&db).unwrap(), b"unreadable-audit-proof-store");
                assert_eq!(
                    rt.block_on(
                        capture.query("SELECT * FROM checkpoint ORDER BY checkpoint_id", &[])
                    )
                    .unwrap(),
                    checkpoints
                );
            }
        }
    }
    fn orphan_association_refuses_new_event(call_orphan: bool, attributed: bool) {
        let (tmp, mut audit, _) = audit_collision_setup();
        audit.call_id = "e".repeat(32);
        audit.session = attributed.then(|| "new-session".into());
        let event = serve_audit_event(tmp.path(), &audit);
        let (decision, call) = serve_audit_envelopes(&event, &audit, 2000);
        let expected =
            expected_serve_row(if call_orphan { &call } else { &decision }, 2000).unwrap();
        let db = crate::family::recall_db(tmp.path()).unwrap();
        let store = rift_store::open_migrated(&db, &rift_store::recall_migrations()).unwrap();
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        assert!(rt
            .block_on(store.insert_or_ignore(
                "memd_audit_session",
                &[
                    "serve_key",
                    "relation",
                    "session_id",
                    "window_key",
                    "schema_version"
                ],
                &[
                    rift_store::Value::text(&expected.key),
                    rift_store::Value::text(if call_orphan {
                        "serve_call_event"
                    } else {
                        "memd_audit_event"
                    }),
                    rift_store::Value::text("orphan-session"),
                    rift_store::Value::Null,
                    rift_store::Value::Int(1)
                ]
            ))
            .unwrap());
        let rows = attribution_rows(tmp.path());
        assert_eq!(
            rows.len(),
            3,
            "completed A associations plus canonical orphan"
        );
        let job = queued_audit_job(
            tmp.path(),
            crate::agent_queue::JobKind::ServeAudit(audit),
            2000,
        );
        assert_audit_collision_retained(&job, &rows);
    }

    #[test]
    fn an_orphan_audit_association_cannot_be_activated_by_an_unattributed_event() {
        orphan_association_refuses_new_event(false, false);
    }

    #[test]
    fn an_orphan_call_association_cannot_conflict_with_a_new_attributed_event() {
        orphan_association_refuses_new_event(true, true);
    }
}
