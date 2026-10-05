//! One skill verb, run through the bounded surface and booked.
//!
//! The only module of this plane that opens a store: everything decidable
//! from argv or from the caller's own file has already been decided by the
//! time anything here runs, so a refused call never pays for a projection.

use std::path::Path;

use rift_core::{Result, RiftError};
use rift_recall_serve::{decode_verb, ToolError, ToolResponse};
use rift_recall_substrate::GixSubstrate;
use rift_store::SqliteStore;
use serde_json::{json, Value};

use crate::cli::{exit, Outcome, RecallCmd};

use super::document::{owe_document_conversion, resolve_document};
use super::search::resolve_search;
use super::verb::{cross_branch_grant, search_content, verb_and_params};

/// Run one skill verb through the bounded surface and render its outcome.
///
/// Everything decidable without a store is decided here, in the order that
/// makes the cheapest refusal the earliest one: argv, then the repository's
/// own files, then the caller's named document. Only what survives all three
/// reaches [`answer_one_bounded_skill_verb`], which is where a projection is
/// finally opened.
pub(super) fn serve(cmd: &RecallCmd, repo: &Path) -> Result<Outcome> {
    let Some((verb, params)) = verb_and_params(cmd, repo) else {
        return Ok(Outcome::coded(
            exit::INTERNAL,
            "recall: no skill verb for this subcommand",
            json!({ "error": "no_skill_verb" }),
        ));
    };
    // Decided from argv alone, before a store is opened or a repository is
    // discovered: a malformed reason must cost nothing.
    let grant = match cross_branch_grant(cmd) {
        Ok(grant) => grant,
        Err(refusal) => return Ok(refusal),
    };
    let cfg = rift_config::EffectiveConfig::load(&repo.join(".rift"))
        .or_else(|_| rift_config::EffectiveConfig::secure_default())?;
    if let Some(refusal) = serving_precondition_refusal(repo, verb, &cfg) {
        return Ok(refusal);
    }
    let (tool, req) = decode_verb(verb, &params)
        .map_err(|e| RiftError::storage(format!("recall verb decode failed: {e}")))?;
    // The document verb's answer is a file in the family's sidecar store, and
    // this is the only process that can find it. Everything decidable from the
    // caller's own file — is it a document, is it there, is it inside the size
    // the family converts — is decided here, before a store is opened.
    let req = match resolve_document(tool, req, repo, &cfg) {
        Ok(req) => req,
        Err(refusal) => return Ok(refusal),
    };

    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .map_err(RiftError::from)?;
    rt.block_on(answer_one_bounded_skill_verb(ServeCall {
        cmd,
        repo,
        cfg: &cfg,
        verb,
        grant,
        tool,
        req,
    }))
}

/// Why this repository will not be served, or `None` to go on.
///
/// A repo with Recall off, or not set up on this machine, is answered honestly
/// rather than silently bootstrapped: opening the projection here would mint an
/// identity nobody initialized, create `recall.db` and flip the dashboard's
/// Disabled/Quiet distinction.
fn serving_precondition_refusal(
    repo: &Path,
    verb: &str,
    cfg: &rift_config::EffectiveConfig,
) -> Option<Outcome> {
    if !cfg.recall_enabled() {
        return Some(Outcome::coded(
            exit::PRECONDITION,
            "recall is not enabled for this repository (run `rift recall init`)",
            json!({ "error": "recall_disabled", "verb": verb }),
        ));
    }
    if !crate::family::provisioned_here(repo) {
        return Some(Outcome::coded(
            exit::PRECONDITION,
            "recall is not set up on this machine for this repository (run `rift init`)",
            json!({ "error": "not_provisioned", "verb": verb }),
        ));
    }
    None
}

/// One verb that survived every screen a store-free decision can make, with
/// everything the answer needs gathered in the order it was decided.
struct ServeCall<'a> {
    cmd: &'a RecallCmd,
    repo: &'a Path,
    cfg: &'a rift_config::EffectiveConfig,
    verb: &'static str,
    grant: rift_recall_types::CrossScopeGrant,
    tool: rift_recall_serve::ToolName,
    req: rift_recall_serve::ToolRequest,
}

/// Open the family's projection, resolve what only this process can resolve,
/// and hand the request to the bounded surface.
async fn answer_one_bounded_skill_verb(call: ServeCall<'_>) -> Result<Outcome> {
    let ServeCall {
        cmd,
        repo,
        cfg,
        verb,
        grant,
        tool,
        req,
    } = call;
    let family = crate::family::resolve_at(repo)?;
    let store = open_the_recall_store_of_this_family(&family)?;
    // Answer on fresh state: the same watermark-idempotent ingest pass the
    // dashboard and the memory daemon run. Skipped for the two verbs that
    // read no projection: an ingest pass before handing over a converted
    // document, or before ranking this checkout's own file list, would be
    // work nobody asked for and an answer that could not change because of
    // it.
    if tool.is_scope_bound() {
        refresh_the_recall_projection_over_this_worktree(&store, repo).await?;
    }
    // The search verb's answer is this member's own index and work tree,
    // and this is the only process that can reach either. It runs after the
    // store is open because half of frecency is a query over the projection
    // — and it is deliberately not preceded by an ingest pass: a search
    // answer cannot change because of one, only its warmth ordering can,
    // and paying a pass to re-order a ranking is work nobody asked for.
    let req = match resolve_search(tool, req, search_content(cmd), repo, cfg, &store).await {
        Ok(req) => req,
        Err(refusal) => return Ok(refusal),
    };
    // The agent asking is standing in ONE member of the family. It is
    // served the family's facts on its own line, and told how many it was
    // not shown.
    let (mut scope, ancestry) = crate::memory::serving_scope(&family, repo)?;
    // The one place a grant reaches a serving scope. The hook injector
    // builds its scope through the same `serving_scope` and never comes
    // here, so there is no code path by which an injection grants
    // (owner decision O5) — it is the call graph that says so, not a check.
    scope.cross = grant;
    let provenance = provenance_oracle_for_this_serve(tool, &family, repo).await;
    match crate::memory::tool_call_at_checkout(
        &store,
        repo,
        &scope,
        ancestry.as_ref(),
        provenance.as_ref(),
        tool,
        &req,
    )
    .await
    {
        Ok(resp) => Ok(book_the_serve_and_act_on_its_decision(
            &store, repo, &scope, verb, &req, &resp,
        )
        .await),
        Err(e) => Ok(failed(verb, &e)),
    }
}

/// The family's recall projection, migrated and open.
///
/// The one step of this plane that opens a store, reached from
/// [`answer_one_bounded_skill_verb`] alone: a decision made inside a hook has
/// the agent waiting on it, and this call belongs to the pull channel, which
/// is a short-lived process answering one question a caller asked.
fn open_the_recall_store_of_this_family(family: &rift_git::Family) -> Result<SqliteStore> {
    let db = crate::family::recall_db_of(family);
    rift_store::open_migrated(&db, &rift_store::recall_migrations())
}

/// Bring the projection up to this worktree's head before the answer is read
/// off it, through the same substrate seam the dashboard opens.
async fn refresh_the_recall_projection_over_this_worktree(
    store: &SqliteStore,
    repo: &Path,
) -> Result<()> {
    let sub = GixSubstrate::open(repo)?;
    crate::memory::refresh_recall(store, &sub, repo).await?;
    Ok(())
}

/// The story behind each delta.
///
/// It is resolved HERE and nowhere else: this is the pull channel, one
/// short-lived process answering one question a caller asked, which is the
/// only place in the tree that can afford to open the second projection. The
/// hook path builds its own scope through `serving_scope` and never reaches
/// this function, so the call graph is what keeps the gate free of it.
///
/// Skipped for the verbs that read no projection: a document conversion and a
/// path search have no delta to carry a story.
async fn provenance_oracle_for_this_serve(
    tool: rift_recall_serve::ToolName,
    family: &rift_git::Family,
    repo: &Path,
) -> Box<dyn rift_recall_types::ProvenanceOracle> {
    if !tool.is_scope_bound() {
        return Box::new(rift_recall_types::NoProvenance);
    }
    Box::new(
        crate::provenance::CaptureProvenance::read_capture_side(
            &crate::family::projection_db_of(family),
            repo,
        )
        .await,
    )
}

/// Book the serve that was made, act on what the surface decided, and render
/// it.
///
/// One serve, one neutral audit fact, into the same relation the savings panel
/// reads. What makes that a bijection rather than a payload dedup is the
/// per-invocation key in [`record_serve`]: two `known-state` serves over
/// unchanged state return identical figures and are still two serves.
/// Best-effort — a serve already made is never retracted because its audit
/// write failed, and nothing here retries.
async fn book_the_serve_and_act_on_its_decision(
    store: &SqliteStore,
    repo: &Path,
    scope: &rift_recall_types::Scope,
    verb: &str,
    req: &rift_recall_serve::ToolRequest,
    resp: &ToolResponse,
) -> Outcome {
    let keys = record_serve(store, verb, scope, resp).await.ok().flatten();
    // The dispatcher decided; this acts on the decision. A conversion is owed
    // only where the surface actually answered that one is — so a denied
    // document never queues work, and a document the family has already
    // decided against never queues it twice.
    if resp.payload.get("reason").and_then(Value::as_str)
        == Some(rift_recall_serve::TAG_SIDECAR_OWED)
    {
        owe_document_conversion(repo, req);
    }
    rendered(verb, resp, keys.as_ref())
}

/// The two keys one serve was booked under, as the answer prints them.
///
/// A serve made inside an agent session is attributed by the hook that
/// ingests this answer, and that hook opens no store, so the answer itself is
/// where the keys travel. Both are printed because the schema-1 key is a
/// salted hash of the payload and nothing recomputes a key.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ServeKeys {
    /// The schema-4 key, `serve_call_event.call_id`.
    call_id: String,
    /// The schema-1 key, `memd_audit_event.audit_id`.
    audit_id: String,
}

/// A source of per-invocation serve ids. Process-wide rather than per-call:
/// [`MonotonicUlidSource`](rift_core::MonotonicUlidSource) is strictly
/// increasing only *within one instance*, so a fresh source per call would
/// re-emit the same id for two serves inside the same millisecond — the very
/// collision this exists to prevent.
static SERVE_IDS: std::sync::LazyLock<rift_core::MonotonicUlidSource> =
    std::sync::LazyLock::new(rift_core::MonotonicUlidSource::new);

/// Append the serve's own terminal event to `recall.db` via the same envelope
/// `rift hooks memd` ingests, keyed by a fresh per-invocation ULID.
///
/// The unkeyed envelope addresses by payload hash, which collapses two
/// *distinct* serves whose figures happen to match — a repeated `known-state`
/// over unchanged state does exactly that — understating spend and so inflating
/// net savings, so the active CLI channel keys
/// each call explicitly. A CLI process invocation *is* the unit of a serve,
/// and the caller swallows failure without retrying, so keying per invocation
/// loses no collapse the payload key was providing.
///
/// The SAME serve additionally writes the channel-attributed
/// successor relation (`serve_call_event`/`serve_session_event`, schema 4) —
/// this CLI entrypoint is the `SkillCli` channel's one live caller today, so
/// this is where that write is real rather than aspirational. Best-effort,
/// same as the legacy write beside it: this ADDS a row, it never gates the
/// legacy one on the new one succeeding or vice versa.
///
/// Returns the keys both rows were booked under, or `None` where the audit
/// envelope named no key of its own.
async fn record_serve(
    store: &SqliteStore,
    verb: &str,
    scope: &rift_recall_types::Scope,
    resp: &ToolResponse,
) -> Result<Option<ServeKeys>> {
    use rift_core::UlidSource as _;
    let call_id = SERVE_IDS.next_ulid().to_hex();
    // A serve a cross-branch grant widened is audited as a FIXED
    // tag in the frozen neutral-tag column, and the person's own words go to
    // `serve_grant_event` beside it — stored, joined by the audit id, and never
    // re-served. Two relations because the two values have two contracts, not
    // because one was inconvenient.
    let granted = scope.cross.branches();
    let envelope = if granted {
        rift_recall_serve::grant_tagged_envelope(&resp.event, &call_id)
    } else {
        resp.event.to_hook_json_keyed(&call_id)
    };
    let now = crate::hooks::now_ms();
    crate::hooks::append_memd_audit(store, envelope.as_bytes(), now).await?;
    let audit_id = audit_id_of(&envelope);
    if granted {
        if let (Some(audit_id), Some(reason)) = (audit_id.as_deref(), scope.cross.reason()) {
            rift_recall_ledger::grants::record_serve_grant(
                store,
                audit_id,
                rift_recall_ledger::grants::GRANT_KIND_BRANCHES,
                reason,
                now,
            )
            .await?;
        }
    }

    let serve_call_envelope = resp.event.to_serve_call_json(
        rift_recall_serve::ServeChannel::SkillCli,
        &call_id,
        verb,
        rift_core::Timestamp::from_millis(crate::hooks::now_ms()),
    );
    crate::hooks::append_memd_audit(
        store,
        serve_call_envelope.as_bytes(),
        crate::hooks::now_ms(),
    )
    .await?;
    Ok(audit_id.map(|audit_id| ServeKeys { call_id, audit_id }))
}

/// The `memd_audit_event.audit_id` an envelope will land under — its own
/// `idempotency_key`, read from the document rather than re-derived, so the
/// grant row can never key itself to a row that does not exist.
fn audit_id_of(envelope: &str) -> Option<String> {
    serde_json::from_str::<Value>(envelope)
        .ok()?
        .get("idempotency_key")?
        .as_str()
        .map(str::to_owned)
}

/// The success document: the payload plus the accounting seam's own figures and
/// the tokenizer that produced them (a claim about savings always ships with
/// its provenance), and the keys the serve was booked under.
///
/// The keys are one line of their own, `serve: <call_id> <audit_id>`, second
/// in the text so a client that keeps only the head of a long answer still
/// hands them to the hook; they name rows and carry nothing of the answer.
fn rendered(verb: &str, resp: &ToolResponse, keys: Option<&ServeKeys>) -> Outcome {
    let mut doc = json!({
        "verb": verb,
        "tokenizer": resp.tokenizer,
        "estimated_tokens_returned": resp.estimated_tokens_returned,
        "estimated_tokens_avoided": resp.estimated_tokens_avoided,
        "avoided_basis": resp.avoided_basis.as_str(),
        "result": resp.payload,
    });
    if let (Some(keys), Some(fields)) = (keys, doc.as_object_mut()) {
        fields.insert(
            "serve".to_owned(),
            json!({ "call_id": keys.call_id, "audit_id": keys.audit_id }),
        );
    }
    let key_line = keys.map_or_else(String::new, |keys| {
        format!("serve: {} {}\n", keys.call_id, keys.audit_id)
    });
    let human = format!(
        "{verb}: {} tokens returned, ~{} avoided, {} ({})\n{key_line}{}",
        resp.estimated_tokens_returned,
        resp.estimated_tokens_avoided,
        resp.avoided_basis.as_str(),
        resp.tokenizer,
        serde_json::to_string_pretty(&resp.payload).unwrap_or_default(),
    );
    Outcome::ok(human, doc)
}

/// The failure document. Every reason is a neutral tag — a refusal names *why*,
/// never the withheld content — and the exit code separates "you asked wrong"
/// (usage) from "the surface said no" (failure).
fn failed(verb: &str, err: &ToolError) -> Outcome {
    let (code, tag, detail) = match err {
        ToolError::BudgetCallsExhausted => (
            exit::FAILURE,
            "budget_calls_exhausted",
            json!({ "hint": "start the turn with `rift recall known-state`" }),
        ),
        ToolError::QueryParamsRequired => (
            exit::USAGE,
            "query_params_required",
            json!({ "hint": "pass --task and at least one --file" }),
        ),
        ToolError::Denied(reason) => (
            exit::FAILURE,
            rift_recall_serve::omit_tag(*reason),
            Value::Null,
        ),
        ToolError::PackBudgetTooSmall { needed, cap } => (
            exit::FAILURE,
            "pack_budget_too_small",
            json!({ "needed": needed, "cap": cap, "hint": "call `rift recall changed-hunks`" }),
        ),
        ToolError::Internal(reason) => (exit::INTERNAL, "internal", json!({ "reason": reason })),
    };
    Outcome::coded(
        code,
        format!("{verb}: refused ({tag})"),
        json!({ "verb": verb, "error": tag, "detail": detail }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use rift_recall_serve::{AvoidedBasis, ToolName};

    /// Accounting is invariant across serving channels: two serves are
    /// two rows even when they return byte-identical figures, which is the
    /// steady state of this surface — a `known-state` call on unchanged state
    /// answers the same thing every time. The CLI channel gets this from its
    /// per-invocation key, and the level that matters is the row count in
    /// `recall.db`, not the key string.
    #[tokio::test]
    async fn two_identical_serves_are_two_audit_rows() {
        use rift_recall_events::MemdEvent;
        use rift_store::{apply_migrations, recall_migrations, CanonicalStore, SqliteStore};

        let store = SqliteStore::open_in_memory().expect("in-memory store");
        apply_migrations(&store, &recall_migrations()).expect("recall migrations");

        let resp = ToolResponse {
            tool: ToolName::KnownState,
            avoided_basis: AvoidedBasis::StatedRecorded,
            estimated_tokens_returned: 1168,
            estimated_tokens_avoided: 2_256_482,
            tokenizer: "cl100k_base",
            payload: json!({ "state": "known" }),
            event: MemdEvent::ManifestServed {
                estimated_tokens_returned: 1168,
                estimated_tokens_avoided: 2_256_482,
                pull_claim: None,
            },
        };
        let ungranted = rift_recall_types::Scope::unbounded("fam");
        record_serve(&store, "known-state", &ungranted, &resp)
            .await
            .expect("first serve audits");
        record_serve(&store, "known-state", &ungranted, &resp)
            .await
            .expect("second serve audits");

        let rows = store
            .query(
                "SELECT tokens_returned, tokens_avoided FROM memd_audit_event \
                 WHERE verb = 'memd.manifest_served'",
                &[],
            )
            .await
            .expect("audit rows");
        assert_eq!(rows.len(), 2, "a repeated serve must not be swallowed");
        for row in &rows {
            assert_eq!(row.get_i64(0).unwrap(), 1168);
            assert_eq!(row.get_i64(1).unwrap(), 2_256_482);
        }

        // The SAME two serves also land in the
        // channel-attributed successor relation, tagged `skill_cli` — this
        // CLI entrypoint IS that channel's one live caller.
        let serve_call_rows = store
            .query(
                "SELECT channel, tool_name, terminal_verb, tokens_returned, tokens_avoided \
                 FROM serve_call_event",
                &[],
            )
            .await
            .expect("serve_call_event rows");
        assert_eq!(serve_call_rows.len(), 2);
        for row in &serve_call_rows {
            assert_eq!(row.get_text(0).unwrap(), "skill_cli");
            assert_eq!(row.get_text(1).unwrap(), "known-state");
            assert_eq!(row.get_text(2).unwrap(), "memd.manifest_served");
            assert_eq!(row.get_i64(3).unwrap(), 1168);
            assert_eq!(row.get_i64(4).unwrap(), 2_256_482);
        }
        // The channel is first-observed exactly once, not once per call.
        assert_eq!(store.count("serve_session_event").await.unwrap(), 1);

        // The collapse the payload key *was* buying is still bought, now by the
        // invocation id: re-delivering one invocation's envelope stays a no-op.
        let replay = resp.event.to_hook_json_keyed("one-invocation");
        for at in [1_i64, 2] {
            crate::hooks::append_memd_audit(&store, replay.as_bytes(), at)
                .await
                .expect("replay is accepted");
        }
        assert_eq!(
            store.count("memd_audit_event").await.unwrap(),
            3,
            "a re-delivered handoff must still collapse"
        );
    }

    /// The answer names the keys its serve was booked under, on one line of
    /// its own and in the document, and those are the keys the rows carry.
    #[tokio::test]
    async fn the_answer_names_the_two_keys_its_serve_was_booked_under() {
        use rift_recall_events::MemdEvent;
        use rift_store::{apply_migrations, recall_migrations, CanonicalStore, SqliteStore};

        let store = SqliteStore::open_in_memory().expect("in-memory store");
        apply_migrations(&store, &recall_migrations()).expect("recall migrations");
        let resp = ToolResponse {
            tool: ToolName::KnownState,
            avoided_basis: AvoidedBasis::StatedRecorded,
            estimated_tokens_returned: 3,
            estimated_tokens_avoided: 4,
            tokenizer: "cl100k_base",
            payload: json!({ "state": "known" }),
            event: MemdEvent::ManifestServed {
                estimated_tokens_returned: 3,
                estimated_tokens_avoided: 4,
                pull_claim: None,
            },
        };
        let keys = record_serve(
            &store,
            "known-state",
            &rift_recall_types::Scope::unbounded("fam"),
            &resp,
        )
        .await
        .unwrap()
        .expect("the audit envelope names its key");
        let answer = rendered("known-state", &resp, Some(&keys));
        let line = answer.human.lines().nth(1).unwrap();
        assert_eq!(line, format!("serve: {} {}", keys.call_id, keys.audit_id));
        assert_eq!(answer.json["serve"]["call_id"], keys.call_id.as_str());
        assert_eq!(answer.json["serve"]["audit_id"], keys.audit_id.as_str());
        let audit = store
            .query("SELECT audit_id FROM memd_audit_event", &[])
            .await
            .unwrap();
        assert_eq!(audit[0].get_text(0).unwrap(), keys.audit_id);
        let call = store
            .query("SELECT call_id FROM serve_call_event", &[])
            .await
            .unwrap();
        assert_eq!(call[0].get_text(0).unwrap(), keys.call_id);

        let unkeyed = rendered("known-state", &resp, None);
        assert!(!unkeyed.human.contains("serve: "));
        assert!(unkeyed.json.get("serve").is_none());
    }

    /// The figure is printed beside the rule it was booked under, in the
    /// document and in the line a person reads.
    #[test]
    fn the_avoided_figure_carries_the_rule_it_was_booked_under() {
        use rift_recall_events::MemdEvent;
        let resp = ToolResponse {
            tool: ToolName::DiffSince,
            avoided_basis: AvoidedBasis::Unverified,
            estimated_tokens_returned: 7,
            estimated_tokens_avoided: 0,
            tokenizer: "cl100k_base",
            payload: json!({}),
            event: MemdEvent::ManifestServed {
                estimated_tokens_returned: 7,
                estimated_tokens_avoided: 0,
                pull_claim: None,
            },
        };
        let answer = rendered("diff-since", &resp, None);
        assert_eq!(answer.json["avoided_basis"], "unverified");
        assert_eq!(answer.json["estimated_tokens_avoided"], 0);
        assert!(
            answer
                .human
                .starts_with("diff-since: 7 tokens returned, ~0 avoided, unverified (cl100k_base)"),
            "{}",
            answer.human
        );
    }

    /// Refusals are neutral tags with an honest exit class: "you asked wrong"
    /// is a usage error, "the surface said no" is a failure, and neither ever
    /// carries withheld content.
    #[test]
    fn refusals_are_neutral_tags_with_honest_exit_codes() {
        let usage = failed("context-pack", &ToolError::QueryParamsRequired);
        assert_eq!(usage.code, exit::USAGE);
        assert_eq!(usage.json["error"], "query_params_required");

        let denied = failed(
            "retrieve-exact",
            &ToolError::Denied(rift_recall_types::OmitReason::SensitivePathRequiresReason),
        );
        assert_eq!(denied.code, exit::FAILURE);
        assert_eq!(denied.json["error"], "sensitive_path_requires_reason");

        let small = failed(
            "context-pack",
            &ToolError::PackBudgetTooSmall {
                needed: 900,
                cap: 10,
            },
        );
        assert_eq!(small.code, exit::FAILURE);
        assert_eq!(small.json["detail"]["needed"], 900);
    }

    /// Both halves of the grant audit, through the production writer.
    ///
    /// The frozen relation gets the FIXED tag and nothing else; the person's
    /// words get their own row, joined by the audit id of the serve they
    /// widened. What makes this worth a test rather than a comment is the
    /// direction of the failure it forbids: untrusted free text reaching a
    /// column whose whole contract is that it is a neutral tag.
    #[tokio::test]
    async fn a_grant_is_audited_with_its_reason() {
        use rift_recall_events::MemdEvent;
        use rift_store::{apply_migrations, recall_migrations, CanonicalStore, SqliteStore};

        let store = SqliteStore::open_in_memory().expect("in-memory store");
        apply_migrations(&store, &recall_migrations()).expect("recall migrations");

        let resp = ToolResponse {
            tool: ToolName::KnownState,
            avoided_basis: AvoidedBasis::StatedRecorded,
            estimated_tokens_returned: 12,
            estimated_tokens_avoided: 340,
            tokenizer: "cl100k_base",
            payload: json!({ "state": "known" }),
            event: MemdEvent::ManifestServed {
                estimated_tokens_returned: 12,
                estimated_tokens_avoided: 340,
                pull_claim: None,
            },
        };
        let mut scope = rift_recall_types::Scope::unbounded("fam");
        let reason = "reviewing the sibling worktree before the merge";
        scope.cross = rift_recall_types::CrossScopeGrant::grant(reason, true, false, false);

        record_serve(&store, "known-state", &scope, &resp)
            .await
            .expect("the widened serve audits");

        let audit = store
            .query(
                "SELECT audit_id, reason FROM memd_audit_event \
                 WHERE verb = 'memd.manifest_served'",
                &[],
            )
            .await
            .expect("audit rows");
        assert_eq!(audit.len(), 1);
        let audit_id = audit[0].get_text(0).unwrap();
        assert_eq!(
            audit[0].get_opt_text(1).unwrap().as_deref(),
            Some("cross-branch-grant"),
            "the frozen column carries the tag, and only the tag"
        );

        let grants = store
            .query(
                "SELECT audit_id, kind, reason_text FROM serve_grant_event",
                &[],
            )
            .await
            .expect("grant rows");
        assert_eq!(grants.len(), 1);
        assert_eq!(
            grants[0].get_text(0).unwrap(),
            audit_id,
            "joined to its serve"
        );
        assert_eq!(grants[0].get_text(1).unwrap(), "branches");
        assert_eq!(grants[0].get_text(2).unwrap(), reason);
    }

    /// An ungranted serve writes no grant row and leaves the neutral-tag column
    /// alone — the tag means something only because it is not always there.
    #[tokio::test]
    async fn an_ungranted_serve_writes_no_grant_and_no_tag() {
        use rift_recall_events::MemdEvent;
        use rift_store::{apply_migrations, recall_migrations, CanonicalStore, SqliteStore};

        let store = SqliteStore::open_in_memory().expect("in-memory store");
        apply_migrations(&store, &recall_migrations()).expect("recall migrations");
        let resp = ToolResponse {
            tool: ToolName::KnownState,
            avoided_basis: AvoidedBasis::StatedRecorded,
            estimated_tokens_returned: 1,
            estimated_tokens_avoided: 2,
            tokenizer: "cl100k_base",
            payload: json!({}),
            event: MemdEvent::ManifestServed {
                estimated_tokens_returned: 1,
                estimated_tokens_avoided: 2,
                pull_claim: None,
            },
        };
        record_serve(
            &store,
            "known-state",
            &rift_recall_types::Scope::unbounded("fam"),
            &resp,
        )
        .await
        .unwrap();
        assert_eq!(store.count("serve_grant_event").await.unwrap(), 0);
        let rows = store
            .query(
                "SELECT reason FROM memd_audit_event WHERE verb = 'memd.manifest_served'",
                &[],
            )
            .await
            .unwrap();
        assert!(rows[0].get_opt_text(0).unwrap().is_none());
    }
}
