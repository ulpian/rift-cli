//! `rift recall …`: the agent-facing serving plane plus Recall's
//! terminal-owned lifecycle (`init`/`status`/`disable`).
//!
//! The skill verbs are the **CLI spelling of the one bounded surface**:
//! argv is rendered into the dispatcher's params object and decoded by the same
//! [`rift_recall_serve::decode_verb`] → [`decode_request`] path, then dispatched
//! through the same policy gates, per-turn budget, and accounting seam. Each
//! call keys its own neutral serve fact by a per-invocation id, so two serves
//! returning identical figures are still two rows. The audit is best-effort: a
//! serve already made is never retracted because its row could not be written.
//!
//! Division of labour with `rift memory …`: that surface answers for a *human*
//! and supplies the known state itself (it hashes the worktree for you); this
//! one answers for an *agent*, which states what it already holds via
//! `--known-hash`. Nothing is inferred on the agent's behalf.
//!
//! [`decode_request`]: rift_recall_serve::decode_request

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::Path;

use rift_core::{Result, RiftError};
use rift_recall_ledger::grants::MAX_GRANT_REASON_BYTES;
use rift_recall_serve::{decode_verb, ToolError, ToolResponse};
use rift_recall_substrate::GixSubstrate;
use rift_recall_types::CrossScopeGrant;
use rift_store::SqliteStore;
use serde_json::{json, Value};

use crate::cli::{exit, Outcome, RecallCmd};

pub(crate) fn run(cmd: &RecallCmd, repo: &Path) -> rift_core::Result<Outcome> {
    match cmd {
        RecallCmd::Init => {
            let report =
                crate::features::recall_install(repo).map_err(rift_core::RiftError::storage)?;
            let statuses = rift_agent_install::native::diagnose_all(repo);
            let mut human = format!(
                "Recall initialized: {} native integrations, {} files changed\n",
                report.integrations.len(),
                report.changed_files.len()
            );
            render_status(&mut human, &statuses);
            Ok(Outcome::ok(
                human.trim_end().to_owned(),
                serde_json::json!({
                    "initialized": true,
                    "changed_files": report.changed_files,
                    "integrations": statuses,
                }),
            ))
        }
        RecallCmd::Status => {
            let statuses = rift_agent_install::native::diagnose_all(repo);
            let clients = rift_agent_install::diagnose(repo, false);
            let boundary_responses = observed_boundary_responses(repo);
            let mut human = String::from("Recall native integrations\n");
            render_status(&mut human, &statuses);
            let policy = rift_recall_policy::load_serve_policy(&repo.join(".rift"));
            let assessments =
                support_assessments(repo, &statuses, &clients, &boundary_responses, &policy);
            render_support(&mut human, &assessments);
            render_policy(&mut human, &policy);
            // The figure this plane is judged by, on the table that says
            // whether it is switched on: a mode line with no measurement
            // beside it is the reason the two were ever read apart.
            let savings = crate::memory::savings_report(repo)?;
            render_savings_shares(&mut human, &savings);
            Ok(Outcome::ok(
                human.trim_end().to_owned(),
                serde_json::json!({
                    "integrations": integration_status_json(&statuses, &assessments),
                    "policy": policy_json(&policy),
                    "savings": crate::memory::savings_json(&savings),
                }),
            ))
        }
        RecallCmd::Disable => {
            let changed =
                crate::features::recall_remove(repo).map_err(rift_core::RiftError::storage)?;
            Ok(Outcome::ok(
                format!(
                    "Recall disabled; removed Rift-owned entries from {} files (data retained)",
                    changed.len()
                ),
                serde_json::json!({
                    "disabled": true,
                    "changed_files": changed,
                    "data_retained": true,
                }),
            ))
        }
        skill => serve(skill, repo),
    }
}

// --- the skill-verb serving plane --------------------------------------------

/// The `(verb, params)` a skill-verb invocation carries. `None` for the three
/// lifecycle subcommands, which [`run`] has already handled — expressed as an
/// `Option` rather than a panic so the never-guess discipline holds even if a
/// future variant is added and this table is not updated.
fn verb_and_params(cmd: &RecallCmd, repo: &Path) -> Option<(&'static str, Value)> {
    use rift_recall_serve::ToolName as T;
    let ranked = |a: &crate::cli::RecallRankedArgs| {
        json!({
            "task": a.task,
            "files_in_focus": focus_paths(repo, &a.files),
            "error_signatures": a.errors,
            "known_hashes": a.known_hashes,
            "token_budget": a.token_budget.unwrap_or(0),
        })
    };
    Some(match cmd {
        RecallCmd::Init | RecallCmd::Status | RecallCmd::Disable => return None,
        RecallCmd::KnownState(a) => (T::KnownState.verb_name(), ranked(a)),
        // Both shapes of one verb: the dispatcher decides which it is from
        // whether a path and a revision are both present, so argv is rendered
        // as it was typed and nothing is inferred here.
        RecallCmd::ChangedHunks(a) => (
            T::ChangedHunks.verb_name(),
            json!({
                "target": a.target,
                "known_hashes": a.known_hashes,
                "task": a.task,
                "files_in_focus": focus_paths(repo, &a.files),
                "error_signatures": a.errors,
                "token_budget": a.token_budget.unwrap_or(0),
            }),
        ),
        RecallCmd::ContextPack(a) => (T::ContextPack.verb_name(), ranked(a)),
        RecallCmd::DiffSince(a) => (
            T::DiffSince.verb_name(),
            json!({
                "known_hashes": a.known_hashes,
                "token_budget": a.token_budget.unwrap_or(0),
            }),
        ),
        RecallCmd::FileIfChanged(a) => (
            T::FileIfChanged.verb_name(),
            json!({ "target": a.target, "known_hashes": a.known_hashes }),
        ),
        RecallCmd::ReadSummary(a) => (
            T::ReadSummary.verb_name(),
            json!({ "target": a.target, "known_hashes": a.known_hashes }),
        ),
        RecallCmd::RetrieveExact(a) => (
            T::RetrieveExact.verb_name(),
            json!({ "target": a.handle, "reason": a.reason }),
        ),
        RecallCmd::CostHotspots(a) => (T::CostHotspots.verb_name(), json!({ "top_n": a.top_n })),
        // Only the path. The document's hash, its declared native cost and the
        // sidecar the family holds for it are RESOLVED, not typed, and they
        // are attached after this decoder rather than through it — a params
        // object that could carry them would let a caller hand the surface the
        // bytes it is asking the surface to serve.
        RecallCmd::DocRead(a) => (T::DocRead.verb_name(), json!({ "target": a.target })),
        // Only the query. Which paths matched, what git says about them and
        // what the lines inside them say are RESOLVED — out of this member's
        // index and work tree — and attached after this decoder rather than
        // through it, for the reason the document verb's are.
        RecallCmd::Search(a) => (T::Search.verb_name(), json!({ "target": a.query })),
    })
}

/// The cross-scope grant this invocation asked for, or the usage
/// refusal that stops it.
///
/// `--cross-branch <REASON>` is how a person deliberately widens containment
/// past this checkout's branch. Two refusals, both **usage** errors, because
/// both are the caller asking wrong rather than the surface saying no:
///
/// * **over [`MAX_GRANT_REASON_BYTES`] bytes.** The bound is in BYTES, not
///   characters, because that is what the column stores and what the writer
///   enforces; a 101-character reason in a two-byte script is over it. Named in
///   the message, so the caller can act on it rather than guess.
/// * **blank.** [`CrossScopeGrant::grant`] answers a blank reason by granting
///   nothing — reason-required by construction — and silently not widening
///   after someone asked to widen is exactly the quiet no-op that teaches
///   people not to trust a flag. Say it instead.
///
/// The writer bounds the text again on its own ([`rift_recall_ledger::grants`]),
/// and that is not redundancy: this refusal is a courtesy to the caller, and the
/// writer's cut is the guarantee about the append-only row.
fn cross_branch_grant(cmd: &RecallCmd) -> std::result::Result<CrossScopeGrant, Outcome> {
    let reason = match cmd {
        // Nothing to widen. The three lifecycle subcommands have no scope at
        // all, and neither of the two verbs whose answer is not in the
        // projection is contained by one: a sidecar is keyed by the document's
        // own bytes and is the same file for every member of the family, and a
        // search answers out of the checkout the caller is standing in. No
        // branch axis either way (`ToolName::is_scope_bound`), and a flag with
        // nothing behind it is worse than no flag.
        RecallCmd::Init
        | RecallCmd::Status
        | RecallCmd::Disable
        | RecallCmd::DocRead(_)
        | RecallCmd::Search(_) => None,
        RecallCmd::KnownState(a) | RecallCmd::ContextPack(a) => a.cross_branch.as_deref(),
        RecallCmd::ChangedHunks(a) => a.cross_branch.as_deref(),
        RecallCmd::DiffSince(a) => a.cross_branch.as_deref(),
        RecallCmd::FileIfChanged(a) | RecallCmd::ReadSummary(a) => a.cross_branch.as_deref(),
        RecallCmd::RetrieveExact(a) => a.cross_branch.as_deref(),
        RecallCmd::CostHotspots(a) => a.cross_branch.as_deref(),
    };
    let Some(reason) = reason else {
        return Ok(CrossScopeGrant::none()); // the default: nothing is widened
    };
    // The hint rides the HUMAN line as well as the document, unlike the
    // dispatcher's refusals below: this one is about argv the caller typed, so
    // the person can fix it from what they see. It names a length and a bound,
    // never the reason text itself.
    let usage = |tag: &str, hint: String| {
        Outcome::coded(
            exit::USAGE,
            format!("recall: refused ({tag}) — {hint}"),
            json!({
                "error": tag,
                "detail": { "hint": hint, "max_reason_bytes": MAX_GRANT_REASON_BYTES },
            }),
        )
    };
    if reason.len() > MAX_GRANT_REASON_BYTES {
        return Err(usage(
            "cross_branch_reason_too_long",
            format!(
                "--cross-branch takes at most {MAX_GRANT_REASON_BYTES} bytes; this was {}",
                reason.len()
            ),
        ));
    }
    if reason.trim().is_empty() {
        return Err(usage(
            "cross_branch_reason_required",
            "--cross-branch widens containment and is audited; say why".to_owned(),
        ));
    }
    // Branches only. Widening the developer or repo axis is a different act
    // with a different blast radius, and this flag is not it.
    Ok(CrossScopeGrant::grant(reason, true, false, false))
}

/// The focus set, as repo-relative paths.
///
/// `files_in_focus` is an input to the input-keyed `PackKey`, so an
/// absolute path is not a cosmetic difference: two members of one family asking
/// the identical question about the identical file would compute two keys and
/// miss each other's cache entry — the pack cache would stop compounding at
/// exactly the boundary a shared store exists to cross. A path outside this
/// repository is left alone: it is not ours to rewrite, and it will simply not
/// match anything.
fn focus_paths(repo: &Path, files: &[String]) -> Vec<String> {
    files.iter().map(|f| repo_relative(repo, f)).collect()
}

/// The name a verb argument keys the window journal with: repository-relative
/// and `/`-spelled, whichever base and separator the caller typed it with.
fn repo_relative(repo: &Path, path: &str) -> String {
    use rift_recall_substrate::worktree::repo_relative_slash;
    let trimmed = path.strip_prefix("./").unwrap_or(path);
    let candidate = Path::new(trimmed);
    for base in [Some(repo.to_path_buf()), repo.canonicalize().ok()]
        .into_iter()
        .flatten()
    {
        if candidate.strip_prefix(&base).is_ok() {
            return repo_relative_slash(&base, candidate).unwrap_or_default();
        }
    }
    repo_relative_slash(Path::new(""), candidate).unwrap_or_else(|| trimmed.to_owned())
}

/// Resolve the document half of a `doc-read` call: the bytes'
/// identity, the declared cost of the read this verb replaces, and the sidecar
/// the family holds for it — or the refusal that stops the call.
///
/// **Nothing is converted here.** Conversion costs a process and a parse, and
/// the asking path is the one place that must not pay for it: a document the
/// family has not converted yet is answered `pending`, the work is journaled
/// for the detached worker, and the next call is answered from the store. That
/// is the same fallback the hook takes when a conversion misses its deadline,
/// and it is why this verb is safe to call on anything.
///
/// Every refusal below is a **usage** refusal, decided from the caller's own
/// file: it is not a document Rift converts, it is not there, it is over the
/// size the family converts, or documents are switched off for this
/// repository. None of them is a decision of the serving surface, so none of
/// them books a serve.
fn resolve_document(
    tool: rift_recall_serve::ToolName,
    req: rift_recall_serve::ToolRequest,
    repo: &Path,
    cfg: &rift_config::EffectiveConfig,
) -> std::result::Result<rift_recall_serve::ToolRequest, Outcome> {
    use rift_recall_serve::{Document, SidecarState, ToolName};

    if tool != ToolName::DocRead {
        return Ok(req);
    }
    let named = req.target.clone().unwrap_or_default();
    let usage = |tag: &str, human: String| {
        Outcome::coded(
            exit::USAGE,
            format!("doc-read: refused ({tag}) — {human}"),
            json!({ "verb": "doc-read", "error": tag, "detail": { "path": named } }),
        )
    };
    // Through the same lexical rule the serving policy normalizes with, so a
    // path dressed up with `..` is refused rather than joined to anything.
    let Some(rel) = rift_recall_policy::normalize_rel(&repo_relative(repo, &named)) else {
        return Err(usage(
            "path_outside_repository",
            "name a file inside this repository".to_owned(),
        ));
    };
    let ext = Path::new(&rel)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_owned();
    if !crate::doc_store::is_document_extension(&ext) {
        return Err(usage(
            "not_a_document",
            format!(
                "Rift converts {}; read anything else directly",
                crate::doc_store::DOCUMENT_EXTENSIONS.join(", ")
            ),
        ));
    }
    let policy = rift_recall_policy::ServePolicy::load(&repo.join(".rift"));
    if policy.doc.mode == rift_recall_policy::Mode::Off {
        return Err(Outcome::coded(
            exit::PRECONDITION,
            "doc-read: documents are switched off for this repository ([doc] mode)".to_owned(),
            json!({ "verb": "doc-read", "error": "doc_off" }),
        ));
    }
    let path = repo.join(&rel);
    // The size is read before the bytes are: a document over the cap is never
    // converted, so a caller asking for one is told now rather than left
    // waiting for a conversion that will not happen.
    let Ok(meta) = std::fs::metadata(&path) else {
        return Err(usage("no_such_document", "no file at that path".to_owned()));
    };
    if meta.len() > policy.doc.max_input_bytes {
        return Err(usage(
            "document_over_the_conversion_cap",
            format!(
                "{} bytes; the cap is {}",
                meta.len(),
                policy.doc.max_input_bytes
            ),
        ));
    }
    let Ok(bytes) = std::fs::read(&path) else {
        return Err(usage(
            "no_such_document",
            "the file cannot be read".to_owned(),
        ));
    };
    let source_sha256 = rift_shadow::sha256_hex(&[&bytes]);
    let declared_baseline = rift_recall_serve::declared_document_baseline(&ext, &bytes);
    // THE GATE THAT MATTERS IS THE DISPATCHER'S, on the path, before it looks
    // at any of this. The same question is asked one step earlier for one
    // reason: a document this repository denies or calls sensitive should not
    // have its Markdown read into this process at all. What that leaves in the
    // request is the default `Owed`, which the surface never reaches — it
    // refuses on the path first — so it is not a claim about the family's
    // store.
    let withheld = cfg.is_sensitive_path(&rel)
        || policy
            .denied_paths
            .iter()
            .any(|glob| rift_config::glob_match(glob, &rel));
    let sidecar = match (withheld, crate::family::try_at(repo)) {
        (false, Some(family)) => {
            let store = crate::doc_store::DocStore::for_member(&family.common_dir, repo);
            crate::doc_store::sidecar_key(&source_sha256, &policy.doc)
                .map_or(SidecarState::Owed, |key| held_sidecar(&store, &key, repo))
        }
        _ => SidecarState::Owed,
    };
    Ok(rift_recall_serve::ToolRequest {
        target: Some(rel),
        document: Some(Document {
            source_sha256,
            declared_baseline,
            sidecar,
        }),
        ..req
    })
}

/// What the family's store says about one key, and — when it holds an answer —
/// this member's view of it.
///
/// The member view is a hardlink into the checkout the caller is standing in,
/// so the path the answer names is one the caller can open. Linking is
/// best-effort: the Markdown is served either way, and a member that could not
/// be given a view is told so by the absence of a path rather than by a
/// refusal.
fn held_sidecar(
    store: &crate::doc_store::DocStore,
    key: &str,
    repo: &Path,
) -> rift_recall_serve::SidecarState {
    use rift_recall_serve::{Sidecar, SidecarState};

    if store.is_skipped(key) {
        return SidecarState::Declined;
    }
    if !store.holds(key) {
        return SidecarState::Owed;
    }
    let Ok(markdown) = std::fs::read_to_string(store.family_path(key)) else {
        return SidecarState::Owed;
    };
    let member = store.link(key).ok();
    let path = member
        .as_ref()
        .and_then(|m| rift_recall_substrate::worktree::repo_relative_slash(repo, m))
        .unwrap_or_default();
    SidecarState::Held(Sidecar { path, markdown })
}

/// Journal the conversion a document verb found owing.
///
/// A request, not the work: the file is a couple of hundred bytes and one
/// rename, and the worker decides whether there is anything to do. Keyed by the
/// document's bytes, so two callers meeting the same document owe one
/// conversion. Best-effort — a serve already made is never retracted because a
/// job file could not be written.
fn owe_document_conversion(repo: &Path, req: &rift_recall_serve::ToolRequest) -> bool {
    let (Some(rel_path), Some(doc)) = (req.target.as_deref(), req.document.as_ref()) else {
        return false;
    };
    let Ok(paths) = rift_git::discover(repo) else {
        return false;
    };
    crate::agent_queue::enqueue(
        &paths.git_dir,
        &crate::agent_queue::AgentJob {
            schema_version: 1,
            work_tree: paths.work_tree.clone(),
            queued_at_ms: crate::hooks::now_ms(),
            attempts: 0,
            kind: crate::agent_queue::JobKind::DocConvert {
                rel_path: rel_path.to_owned(),
                content_sha256: doc.source_sha256.clone(),
            },
        },
    )
    .is_ok()
}

/// Resolve the search half of a `search` call: what this member's path index
/// answers for the query, the lines inside the paths it ranked when the caller
/// asked for them, and the declared cost of the listing that answer replaces —
/// or the refusal that stops the call.
///
/// **Nothing walks past the cold budget here.** An index that is missing or
/// stale is rebuilt inline only while a bounded walk fits inside
/// [`COLD_BUDGET`](rift_recall_search::COLD_BUDGET); past it the call is
/// answered `pending`, a rebuild is asked of the detached worker, and the next
/// call is answered from a file. That is the same rule the hook arms are held
/// to, and it is why this verb is safe to call on any repository.
///
/// The facts the ranking needs are gathered *here* rather than in the engine,
/// which is what keeps the engine a pure function of its arguments: the window
/// journals of every member of the family (so a file a sibling worktree opened
/// this morning is warm here this afternoon), the projection's own read counts,
/// and what git says is in flight.
#[cfg(feature = "search")]
async fn resolve_search(
    tool: rift_recall_serve::ToolName,
    req: rift_recall_serve::ToolRequest,
    content: bool,
    repo: &Path,
    cfg: &rift_config::EffectiveConfig,
    store: &SqliteStore,
) -> std::result::Result<rift_recall_serve::ToolRequest, Outcome> {
    use rift_recall_search::{
        engine::PathQuery, open_for, FrecencyFacts, NativeEngine, Opened, SearchEngine as _,
        StatusFacts, COLD_BUDGET,
    };
    use rift_recall_serve::{
        declared_search_baseline, IndexState, Search, SearchPath, ToolName, NATIVE_LISTING_PATHS,
    };

    if tool != ToolName::Search {
        return Ok(req);
    }
    let query = req.target.clone().unwrap_or_default();
    if query.trim().is_empty() {
        return Err(Outcome::coded(
            exit::USAGE,
            "search: refused (empty_query) — say what to look for".to_owned(),
            json!({ "verb": "search", "error": "empty_query" }),
        ));
    }
    let policy = rift_recall_policy::ServePolicy::load(&repo.join(".rift"));
    if policy.search.mode == rift_recall_policy::Mode::Off {
        return Err(Outcome::coded(
            exit::PRECONDITION,
            "search: search is switched off for this repository ([search] mode)".to_owned(),
            json!({ "verb": "search", "error": "search_off" }),
        ));
    }
    let pending = |req: rift_recall_serve::ToolRequest| rift_recall_serve::ToolRequest {
        search: Some(Search {
            content,
            declared_baseline: 0,
            index: IndexState::Pending,
        }),
        ..req
    };
    let head = rift_recall_search::head_oid(repo);
    let index = match open_for(repo, &head, cfg.sensitive_paths(), COLD_BUDGET) {
        Opened::Fresh(index) => index,
        // Built inside the budget and worth keeping: the caller here is not on
        // a hook's clock, and the next one — which may be — finds a file rather
        // than a walk. Best-effort; a checkout Rift cannot write to still got
        // its answer.
        Opened::Cold(index) => {
            let _ = index.save(&rift_recall_search::index_path(repo));
            index
        }
        Opened::Absent => {
            owe_search_index(repo);
            return Ok(pending(req));
        }
    };

    let status = StatusFacts::collect(repo);
    let mut frecency = FrecencyFacts::from_members(&family_work_trees(repo));
    frecency.absorb_read_counts(family_read_counts(store).await);
    let engine = NativeEngine::new(index)
        .with_frecency(frecency)
        .with_status(status);

    let ranked = engine.rank_paths(&PathQuery {
        text: query.clone(),
        // The native listing's own cap, because that is the set the declared
        // baseline prices: ranking fewer would claim a listing nobody would
        // have received, and ranking more would price one nobody would either.
        limit: NATIVE_LISTING_PATHS,
        now_ms: u64::try_from(crate::hooks::now_ms()).unwrap_or(0),
    });
    let paths: Vec<String> = ranked.hits.iter().map(|hit| hit.path.clone()).collect();
    let lines = if content {
        matched_lines(&engine, repo, &query, &paths, policy.search.grep_head_limit)
    } else {
        Vec::new()
    };
    let hits = ranked
        .hits
        .iter()
        .map(|hit| SearchPath {
            path: hit.path.clone(),
            status: git_word(engine.status(), &hit.path),
        })
        .collect();
    // What may be claimed is the listing this answer may stand in for, which
    // is the ranked head minus the paths the chokepoint will refuse to render:
    // a path Rift will not name is not one it may bill for either. The screen
    // runs here rather than after the answer so that the two can never differ.
    //
    // Priced as a glob and never as a shell listing: this verb is nobody's
    // tool call. It is a fuzzy query an operator typed, it has no client
    // result ceiling of its own, and the smaller of the two bounds is the one
    // a figure Rift will claim credit for has to take. The third screen is
    // empty on purpose — an operator who asked this verb for a name is shown
    // the name, so the repository's sensitive list is not a screen here, and a
    // path this verb WILL say out loud is one it may also price.
    let priced = rift_recall_serve::priceable_listing(
        &rift_recall_policy::PolicyEngine::new(cfg).with_serve_policy(policy.clone()),
        rift_recall_serve::ListingShape::Glob,
        rift_recall_serve::NO_CLIENT_FAMILY,
        &paths,
        |_| false,
    );
    Ok(rift_recall_serve::ToolRequest {
        search: Some(Search {
            content,
            declared_baseline: declared_search_baseline(
                content,
                rift_recall_serve::ListingShape::Glob,
                rift_recall_serve::NO_CLIENT_FAMILY,
                &priced,
            ),
            index: IndexState::Answered {
                hits,
                total: ranked.total,
                lines,
            },
        }),
        ..req
    })
}

/// The lines the caller asked for, inside the paths that ranked.
///
/// A separate pass, and separately budgeted, because it is a different cost and
/// a different risk: ranking touches no file content at all, and this reads
/// bytes an agent may then be shown. The cap is the one this repository already
/// declares for a content search that carried none of its own — one knob, not
/// two — and the engine's own screens decide which of the ranked paths may be
/// opened at all.
#[cfg(feature = "search")]
fn matched_lines(
    engine: &rift_recall_search::NativeEngine,
    repo: &Path,
    query: &str,
    paths: &[String],
    head_limit: u32,
) -> Vec<rift_recall_serve::SearchLine> {
    use rift_recall_search::{content::ContentQuery, query::literal_needle, SearchEngine as _};

    engine
        .content_lines(
            repo,
            &ContentQuery {
                needle: literal_needle(query),
                ignore_case: true,
                paths: paths.to_vec(),
                max_hits: head_limit as usize,
                ..ContentQuery::default()
            },
        )
        .into_iter()
        .map(|hit| rift_recall_serve::SearchLine {
            path: hit.path,
            line: u32::try_from(hit.line_number).unwrap_or(u32::MAX),
            text: hit.line,
        })
        .collect()
}

/// The same call in a build without the search seam.
///
/// A refusal rather than a `pending`, and the difference matters: `pending`
/// promises a rebuild the worker will do, and this build has nothing to do it
/// with. Saying which of the two it is costs a line and saves an agent a
/// second call that would answer the same.
#[cfg(not(feature = "search"))]
#[allow(clippy::unused_async)] // one signature for both builds
async fn resolve_search(
    tool: rift_recall_serve::ToolName,
    req: rift_recall_serve::ToolRequest,
    _content: bool,
    _repo: &Path,
    _cfg: &rift_config::EffectiveConfig,
    _store: &SqliteStore,
) -> std::result::Result<rift_recall_serve::ToolRequest, Outcome> {
    if tool != rift_recall_serve::ToolName::Search {
        return Ok(req);
    }
    Err(Outcome::coded(
        exit::PRECONDITION,
        "search: this build has no search engine".to_owned(),
        json!({ "verb": "search", "error": "search_not_in_this_build" }),
    ))
}

/// What git says about one ranked path, as the closed vocabulary the renderer
/// and the answer both read.
///
/// Untracked first, then the working tree, then the index: a file that is both
/// modified and staged is reported modified, because the working tree is the
/// newer of the two facts and the one the developer is looking at.
#[cfg(feature = "search")]
fn git_word(status: &rift_recall_search::StatusFacts, path: &str) -> rift_recall_serve::PathStatus {
    use rift_recall_serve::PathStatus;

    if status.is_untracked(path) {
        PathStatus::Untracked
    } else if status.modified.contains(path) {
        PathStatus::Modified
    } else if status.staged.contains(path) {
        PathStatus::Staged
    } else {
        PathStatus::Tracked
    }
}

/// Every checkout of this repository family, this one included.
///
/// The window journals are per member and the interest they record is the
/// family's, so a file a sibling worktree opened this morning ranks warm here
/// this afternoon. A repository that cannot be enumerated contributes this
/// checkout alone, which ranks colder rather than wrongly.
#[cfg(feature = "search")]
fn family_work_trees(repo: &Path) -> Vec<std::path::PathBuf> {
    let Ok(paths) = rift_git::discover(repo) else {
        return vec![repo.to_path_buf()];
    };
    let mut trees: Vec<std::path::PathBuf> = rift_git::family_members(&paths)
        .into_iter()
        .map(|member| member.work_tree)
        .collect();
    if !trees.iter().any(|tree| tree == repo) {
        trees.push(repo.to_path_buf());
    }
    trees
}

/// How many times the projection has recorded a read of each path.
///
/// The half of frecency the window journals cannot see: reads ingested from
/// checkpoints this checkout's own run state never held. They carry no time of
/// their own — they are the whole history rather than an event — so they add
/// accesses and leave recency to the window journals. Bounded, and read
/// best-effort: a projection that will not answer ranks every path colder,
/// never wrongly.
#[cfg(feature = "search")]
async fn family_read_counts(store: &SqliteStore) -> Vec<(String, u32)> {
    use rift_store::CanonicalStore as _;

    store
        .query(
            "SELECT r.path, COUNT(*) AS n FROM n_file_read fr \
             JOIN n_file_revision r ON r.node_id = fr.file_revision \
             GROUP BY r.path ORDER BY n DESC LIMIT 5000",
            &[],
        )
        .await
        .unwrap_or_default()
        .iter()
        .filter_map(|row| {
            let path = row.get_text(0).ok()?;
            let count = row.get_i64(1).ok()?;
            Some((path, u32::try_from(count).unwrap_or(u32::MAX)))
        })
        .collect()
}

/// Ask for a rebuild of this checkout's path index.
///
/// A request, not the work, and the same one the session-start and post-commit
/// hooks make: whichever of them asks first, the worker walks once. Best-effort
/// — an answer already given is never retracted because a job file could not be
/// written.
#[cfg(feature = "search")]
fn owe_search_index(repo: &Path) -> bool {
    let Ok(paths) = rift_git::discover(repo) else {
        return false;
    };
    crate::hooks::owe_search_index(&paths.git_dir, &paths.work_tree)
}

/// Whether this invocation asked for matching lines as well as ranked paths.
///
/// Read off argv rather than off the params object, because the params object
/// is the decoder's and the decoder never sets the search input: `--content`
/// decides which native call the answer stands in for, and so which baseline it
/// may claim, which is a decision the resolver makes and not the surface.
fn search_content(cmd: &RecallCmd) -> bool {
    matches!(cmd, RecallCmd::Search(a) if a.content)
}

/// Run one skill verb through the bounded surface and render its outcome.
fn serve(cmd: &RecallCmd, repo: &Path) -> Result<Outcome> {
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
    // A repo with Recall off, or not set up on this machine, is answered
    // honestly rather than silently bootstrapped: opening the projection here
    // would mint an identity nobody initialized, create `recall.db` and flip
    // the dashboard's Disabled/Quiet distinction.
    let cfg = rift_config::EffectiveConfig::load(&repo.join(".rift"))
        .or_else(|_| rift_config::EffectiveConfig::secure_default())?;
    if !cfg.recall_enabled() {
        return Ok(Outcome::coded(
            exit::PRECONDITION,
            "recall is not enabled for this repository (run `rift recall init`)",
            json!({ "error": "recall_disabled", "verb": verb }),
        ));
    }
    if !crate::family::provisioned_here(repo) {
        return Ok(Outcome::coded(
            exit::PRECONDITION,
            "recall is not set up on this machine for this repository (run `rift init`)",
            json!({ "error": "not_provisioned", "verb": verb }),
        ));
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
    rt.block_on(async {
        let family = crate::family::resolve_at(repo)?;
        let db = rift_store::recall_db_for_family(&family);
        let store = rift_store::open_migrated(&db, &rift_store::recall_migrations())?;
        // Answer on fresh state: the same watermark-idempotent ingest pass the
        // dashboard and the memory daemon run. Skipped for the two verbs that
        // read no projection: an ingest pass before handing over a converted
        // document, or before ranking this checkout's own file list, would be
        // work nobody asked for and an answer that could not change because of
        // it.
        if tool.is_scope_bound() {
            let sub = GixSubstrate::open(repo)?;
            crate::memory::refresh_recall(&store, &sub, repo).await?;
        }
        // The search verb's answer is this member's own index and work tree,
        // and this is the only process that can reach either. It runs after the
        // store is open because half of frecency is a query over the projection
        // — and it is deliberately not preceded by an ingest pass: a search
        // answer cannot change because of one, only its warmth ordering can,
        // and paying a pass to re-order a ranking is work nobody asked for.
        let req = match resolve_search(tool, req, search_content(cmd), repo, &cfg, &store).await {
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
        // The story behind each delta. It is resolved HERE and nowhere else:
        // this is the pull channel, one short-lived process answering one
        // question a caller asked, which is the only place in the tree that can
        // afford to open the second projection. The hook path builds its own
        // scope through `serving_scope` and never reaches this function, so the
        // call graph is what keeps the gate free of it.
        //
        // Skipped for the verbs that read no projection: a document conversion
        // and a path search have no delta to carry a story.
        let provenance: Box<dyn rift_recall_types::ProvenanceOracle> = if tool.is_scope_bound() {
            Box::new(
                crate::provenance::CaptureProvenance::read_capture_side(
                    &rift_store::projection_db_for_family(&family),
                    repo,
                )
                .await,
            )
        } else {
            Box::new(rift_recall_types::NoProvenance)
        };
        match crate::memory::tool_call_raw(
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
            Ok(resp) => {
                // One serve, one neutral audit fact, into the same relation the
                // savings panel reads. What makes that a bijection rather than a
                // payload dedup is the per-invocation key in `record_serve`:
                // two `known-state` serves over unchanged state return identical
                // figures and are still two serves. Best-effort — a serve
                // already made is never retracted because its audit write
                // failed, and nothing here retries.
                let _ = record_serve(&store, verb, &scope, &resp).await;
                // The dispatcher decided; this acts on the decision. A
                // conversion is owed only where the surface actually answered
                // that one is — so a denied document never queues work, and a
                // document the family has already decided against never queues
                // it twice.
                if resp.payload.get("reason").and_then(Value::as_str)
                    == Some(rift_recall_serve::TAG_SIDECAR_OWED)
                {
                    owe_document_conversion(repo, &req);
                }
                Ok(rendered(verb, &resp))
            }
            Err(e) => Ok(failed(verb, &e)),
        }
    })
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
async fn record_serve(
    store: &SqliteStore,
    verb: &str,
    scope: &rift_recall_types::Scope,
    resp: &ToolResponse,
) -> Result<()> {
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
    if granted {
        if let (Some(audit_id), Some(reason)) = (audit_id_of(&envelope), scope.cross.reason()) {
            rift_recall_ledger::grants::record_serve_grant(
                store,
                &audit_id,
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
    .await
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
/// its provenance).
fn rendered(verb: &str, resp: &ToolResponse) -> Outcome {
    let doc = json!({
        "verb": verb,
        "tokenizer": resp.tokenizer,
        "estimated_tokens_returned": resp.estimated_tokens_returned,
        "estimated_tokens_avoided": resp.estimated_tokens_avoided,
        "result": resp.payload,
    });
    let human = format!(
        "{verb}: {} tokens returned, ~{} avoided ({})\n{}",
        resp.estimated_tokens_returned,
        resp.estimated_tokens_avoided,
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

fn render_status(
    out: &mut String,
    statuses: &[rift_agent_install::native::NativeIntegrationStatus],
) {
    let _ = writeln!(
        out,
        "  {:<16} {:<12} {:<12} installed",
        "agent", "descriptor", "hooks"
    );
    for status in statuses {
        // The availability sentence is the installer's own, so this table,
        // doctor and Project Settings name the same evidence for the same row.
        let _ = writeln!(
            out,
            "  {:<16} {:<12} {:<12} {}",
            status.display_name,
            state_name(&status.wiring),
            status
                .hook_wiring
                .as_ref()
                .map_or_else(|| "-".to_owned(), state_name),
            status.availability(),
        );
    }
}

/// The support tier an operator may rely on. `observe` is deliberately not a
/// tier: it is a requested policy mode, reported separately below.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
enum Support {
    Full,
    ConfiguredUnverified,
    Degraded,
    ComingSoon,
}

/// Facts added to each native integration row. The descriptor's state remains
/// in the existing `wiring` field; these fields add the other independent
/// axes without breaking callers that already consume the native diagnostic.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
struct SupportAssessment {
    agent: String,
    support: Support,
    hook_configuration: String,
    runtime_connection: &'static str,
    context_delivery: &'static str,
    requested_mode: &'static str,
    effective_mode: &'static str,
    degraded_reason: Option<String>,
}

#[derive(Clone)]
struct SupportFacts {
    alpha: bool,
    descriptor: rift_agent_install::native::NativeWiring,
    hooks: Option<rift_agent_install::native::NativeHookWiring>,
    runtime_receipt: bool,
    boundary_response: bool,
    codex_trust: Option<rift_agent_install::native::CodexTrust>,
    hook_failure: Option<String>,
}

fn derive_support(facts: SupportFacts) -> (Support, Option<String>) {
    use rift_agent_install::native::{CodexTrust, NativeHookWiring, NativeWiring};

    if !facts.alpha {
        return (Support::ComingSoon, None);
    }
    let descriptor_problem = match facts.descriptor {
        NativeWiring::Managed => None,
        NativeWiring::Missing => Some("Rift descriptor is missing"),
        NativeWiring::Conflict => Some("Rift descriptor conflicts with an existing file"),
        NativeWiring::Invalid => Some("Rift descriptor is malformed"),
    };
    if let Some(reason) = descriptor_problem {
        return (Support::Degraded, Some(reason.to_owned()));
    }
    let hook_problem = match facts.hooks {
        Some(NativeHookWiring::Managed) => None,
        Some(NativeHookWiring::Missing) | None => Some("required native hook is missing"),
        Some(NativeHookWiring::Stale) => Some("Rift native hook bundle is stale"),
        Some(NativeHookWiring::Conflict) => Some("Rift native hook bundle conflicts"),
        Some(NativeHookWiring::Invalid) => Some("native hook configuration is malformed"),
    };
    if let Some(reason) = hook_problem {
        return (Support::Degraded, Some(reason.to_owned()));
    }
    if let Some(reason) = facts.hook_failure {
        return (Support::Degraded, Some(reason));
    }
    if facts.codex_trust == Some(CodexTrust::Pending) {
        return (
            Support::ConfiguredUnverified,
            Some("Codex hook trust is pending for this path".to_owned()),
        );
    }
    if !facts.runtime_receipt {
        return (
            Support::ConfiguredUnverified,
            Some("no runtime hook receipt observed".to_owned()),
        );
    }
    if !facts.boundary_response {
        return (
            Support::ConfiguredUnverified,
            Some("no boundary response emission observed".to_owned()),
        );
    }
    (Support::Full, None)
}

fn support_assessments(
    repo: &Path,
    statuses: &[rift_agent_install::native::NativeIntegrationStatus],
    clients: &[rift_agent_install::ClientStatus],
    boundary_responses: &BTreeSet<String>,
    policy: &rift_recall_policy::ServePolicySnapshot,
) -> Vec<SupportAssessment> {
    let receipts: BTreeMap<&str, bool> = clients
        .iter()
        .map(|client| (client.client.token(), client.last_hook_at_ms.is_some()))
        .collect();
    let codex_home = std::env::var_os("CODEX_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|home| std::path::PathBuf::from(home).join(".codex"))
        });

    statuses
        .iter()
        .map(|status| {
            let alpha = rift_agent_install::native::is_alpha_supported(&status.agent);
            let codex_trust = (status.agent == rift_core::AgentId::CODEX.as_str()).then(|| {
                rift_agent_install::native::codex_hook_trust(
                    &repo.join(".codex/hooks.json"),
                    codex_home.as_deref(),
                )
            });
            let runtime_receipt = receipts
                .get(status.agent.as_str())
                .copied()
                .unwrap_or(false);
            let boundary_response = boundary_responses.contains(&status.agent);
            let facts = SupportFacts {
                alpha,
                descriptor: status.wiring,
                hooks: status.hook_wiring,
                runtime_receipt,
                boundary_response,
                codex_trust,
                hook_failure: crate::hooks::hook_degradation(repo, &status.agent),
            };
            let (support, degraded_reason) = derive_support(facts);
            let (requested_mode, effective_mode) = if alpha {
                let agent = rift_core::AgentId::new(status.agent.clone());
                let effective = rift_recall_policy::resolve(
                    &policy.policy,
                    &status.agent,
                    family_capability(&agent),
                );
                (
                    effective.read_deny_requested.as_str(),
                    effective.read_deny.as_str(),
                )
            } else {
                ("off", "off")
            };
            SupportAssessment {
                agent: status.agent.clone(),
                support,
                hook_configuration: status
                    .hook_wiring
                    .as_ref()
                    .map_or_else(|| "not_applicable".to_owned(), state_name),
                runtime_connection: if runtime_receipt {
                    "observed"
                } else {
                    "unobserved"
                },
                context_delivery: if boundary_response {
                    "observed"
                } else {
                    "unobserved"
                },
                requested_mode,
                effective_mode,
                degraded_reason,
            }
        })
        .collect()
}

/// Read only the append-only hook-channel rows that identify a per-agent
/// boundary response. Every dispatcher-owned plan now writes its family and
/// semantic kind into this existing relation after native encoding succeeds.
fn observed_boundary_responses(repo: &Path) -> BTreeSet<String> {
    use rift_store::CanonicalStore as _;

    let Ok(db) = crate::family::recall_db(repo) else {
        return BTreeSet::new();
    };
    if !db.exists() {
        return BTreeSet::new();
    }
    let Ok(store) = rift_store::SqliteStore::open_read_only(&db) else {
        return BTreeSet::new();
    };
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread().build() else {
        return BTreeSet::new();
    };
    runtime
        .block_on(store.query(
            "SELECT DISTINCT tool_name FROM serve_call_event \
             WHERE channel = 'hook_inject'",
            &[],
        ))
        .unwrap_or_default()
        .into_iter()
        .filter_map(|row| row.get_text(0).ok())
        .filter_map(|tool| boundary_agent_from_tool_name(&tool))
        .collect()
}

fn boundary_agent_from_tool_name(tool: &str) -> Option<String> {
    let (agent, step) = tool.split_once(':')?;
    matches!(
        step,
        "recovery-map"
            | "session-pointer"
            | "delta-manifest"
            | "boundary-context"
            | "read-replacement"
            | "read-replacement-fallback"
    )
    .then(|| agent.to_owned())
}

fn integration_status_json(
    statuses: &[rift_agent_install::native::NativeIntegrationStatus],
    assessments: &[SupportAssessment],
) -> Vec<Value> {
    statuses
        .iter()
        .zip(assessments)
        .map(|(status, assessment)| {
            let mut value = serde_json::to_value(status).unwrap_or(Value::Null);
            let extras = serde_json::to_value(assessment).unwrap_or(Value::Null);
            if let (Some(object), Some(extra)) = (value.as_object_mut(), extras.as_object()) {
                for (key, value) in extra {
                    if key != "agent" {
                        object.insert(key.clone(), value.clone());
                    }
                }
            }
            value
        })
        .collect()
}

fn render_support(out: &mut String, assessments: &[SupportAssessment]) {
    let _ = writeln!(out, "\nSupport truth");
    let _ = writeln!(
        out,
        "  {:<16} {:<24} {:<20} {:<12} {:<12}",
        "agent", "support", "hooks", "runtime", "context"
    );
    for assessment in assessments {
        let _ = writeln!(
            out,
            "  {:<16} {:<24} {:<20} {:<12} {:<12}",
            assessment.agent,
            state_name(&assessment.support),
            assessment.hook_configuration,
            assessment.runtime_connection,
            assessment.context_delivery,
        );
    }
}

/// The families this program implements to the full extent of their published
/// surface, in the order status prints them.
const ALPHA_FAMILIES: [rift_core::AgentId; 3] = [
    rift_core::AgentId::CLAUDE_CODE,
    rift_core::AgentId::CODEX,
    rift_core::AgentId::ANTIGRAVITY,
];

/// The families whose edit tool's own result is pinned to tell the model that
/// the file it just wrote is still current in its context, each beside the
/// sentence its committed artifact quotes.
///
/// A row here is the whole of the answer to "may a decline rest on this
/// window's own edit?", and **absence is the answer for everyone else**: Antigravity's pinned page states in as many words that nothing says
/// what `replace_file_content` or `write_to_file` returns, Codex's pinned
/// source carries no result text at all, and a family nobody has pinned yet
/// gets the safe answer without anyone remembering to add it.
///
/// The excerpts are not decoration: the test below holds each one against the
/// artifact it claims to come from, so a row cannot be added here by
/// conviction alone.
const PINNED_EDIT_ECHO: &[(rift_core::AgentId, &str)] = &[(
    rift_core::AgentId::CLAUDE_CODE,
    "file state is current in your context — no need to Read it back",
)];

/// What one family's pinned rows allow, read from the single table that
/// asserts a channel from a committed pin.
///
/// This is the bridge the policy crate deliberately does not have: it stays a
/// leaf, and the capability arrives as four facts. The hook path calls this
/// beside [`rift_recall_policy::resolve`], both of which read a file and
/// nothing else — no store is opened to decide what a lever may do.
pub(crate) fn family_capability(
    agent: &rift_core::AgentId,
) -> rift_recall_policy::FamilyCapability {
    use rift_agent::respond::{Channel, RewriteForm, CAPABILITIES};
    let mut capability = rift_recall_policy::FamilyCapability::default();
    for (family, _verb, channel) in CAPABILITIES {
        if family != agent {
            continue;
        }
        match channel {
            Channel::DenyWithReason { confirmed, .. } => {
                capability.confirmed_reason_channel |= *confirmed;
            }
            Channel::UpdatedInput { forms } => {
                capability.document_sidecar_rewrite |=
                    forms.contains(&RewriteForm::DocumentSidecarPath);
                capability.grep_head_limit_rewrite |= forms.contains(&RewriteForm::GrepHeadLimit);
            }
            Channel::AdditionalContext { .. } | Channel::InjectSteps { .. } | Channel::Neutral => {}
        }
    }
    capability.pinned_edit_echo = PINNED_EDIT_ECHO.iter().any(|(family, _)| family == agent);
    capability
}

/// The first reason a family's levers sit below what the file asked for. One
/// note per row: the causes are the same handful, and a reader chasing a lever
/// that is not doing what the file says needs the first one, not a list.
fn cap_note(effective: &rift_recall_policy::Effective) -> &'static str {
    effective
        .read_deny_cap
        .or(effective.search_answer_cap)
        .or(effective.doc_redirect_cap)
        .or(effective.grep_cap_off)
        .or(effective.doc_rewrite_off)
        .map_or("-", rift_recall_policy::CapReason::as_str)
}

fn source_name(source: rift_recall_policy::ServePolicySource) -> &'static str {
    match source {
        rift_recall_policy::ServePolicySource::Declared => "declared",
        rift_recall_policy::ServePolicySource::Default => "absent; defaults apply",
        rift_recall_policy::ServePolicySource::Invalid => "invalid; every lever off",
        rift_recall_policy::ServePolicySource::Refused => "refused; every lever off",
    }
}

/// The three tables as they were read, then what each family's levers may
/// actually do. Both halves are printed because they answer different
/// questions: the first is what the repository asked for, the second is what
/// its agents' pinned channels allow — and the second is the one that runs.
fn render_policy(out: &mut String, snapshot: &rift_recall_policy::ServePolicySnapshot) {
    let p = &snapshot.policy;
    let _ = writeln!(
        out,
        "\nServing policy  .rift/policies/serve.toml: {}",
        source_name(snapshot.source)
    );
    let _ = writeln!(
        out,
        "  interception  mode {}  max_slice_bytes {}  min_slice_lines {}  max_inline_hash_bytes {}  recovery_budget {}",
        p.interception.mode.as_str(),
        p.interception.max_slice_bytes,
        p.interception.min_slice_lines,
        p.interception.max_inline_hash_bytes,
        p.interception.recovery_budget,
    );
    let _ = writeln!(
        out,
        "  search        mode {}  grep_head_limit {}  glob_top_n {}  answer_budget_tokens {}",
        p.search.mode.as_str(),
        p.search.grep_head_limit,
        p.search.glob_top_n,
        p.search.answer_budget_tokens,
    );
    let _ = writeln!(
        out,
        "  doc           mode {}  channel {}  formats {}  table_rows {}  max_input_bytes {}",
        p.doc.mode.as_str(),
        p.doc.channel.as_str(),
        p.doc.formats.as_str(),
        p.doc.table_rows,
        p.doc.max_input_bytes,
    );
    let _ = writeln!(
        out,
        "                pdf_min_chars_per_page {}  convert_budget_ms {}  ttl_days {}",
        p.doc.pdf_min_chars_per_page, p.doc.convert_budget_ms, p.doc.ttl_days,
    );
    let _ = writeln!(
        out,
        "  {:<13} {:<9} {:<9} {:<9} {:<9} {:<12} note",
        "agent", "re-read", "listing", "grep cap", "doc read", "doc decline"
    );
    for agent in &ALPHA_FAMILIES {
        let e = rift_recall_policy::resolve(p, agent.as_str(), family_capability(agent));
        let _ = writeln!(
            out,
            "  {:<13} {:<9} {:<9} {:<9} {:<9} {:<12} {}",
            agent.as_str(),
            e.read_deny.as_str(),
            e.search_answer.as_str(),
            e.grep_cap.as_str(),
            e.doc_rewrite.as_str(),
            e.doc_redirect.as_str(),
            cap_note(&e),
        );
    }
}

/// One line per family with its savings share, under the overall figure.
///
/// Every number here was computed by the one shared function the reread
/// report, the `savings` verb and the dashboard also call — this renders, it
/// does not calculate, so the status table cannot drift from the card.
fn render_savings_shares(out: &mut String, r: &crate::memory::SavingsReport) {
    let s = &r.savings;
    let _ = writeln!(
        out,
        "\nSavings share  avoided ÷ (avoided + read), {}",
        r.window.label()
    );
    if let Some(note) = &r.note {
        let _ = writeln!(out, "  not measured  {note}");
        return;
    }
    let _ = writeln!(
        out,
        "  {:<13} {:<9} avoided {} ÷ {}  n {} session(s) with reads",
        "overall",
        crate::memory::fmt_share(s.share()),
        s.tokens_avoided,
        s.tokens_avoided.saturating_add(s.read_tokens),
        s.sessions_with_reads,
    );
    if s.families.is_empty() {
        let _ = writeln!(out, "  (no family has a decision or a read yet)");
    }
    for f in &s.families {
        let note = if f.decided_without_reads() {
            "  decided, no captured reads"
        } else {
            ""
        };
        let _ = writeln!(
            out,
            "  {:<13} {:<9} honesty {:+}  n {}{note}",
            f.family,
            crate::memory::fmt_share(f.share()),
            f.honesty(),
            f.sessions_with_reads,
        );
    }
    let _ = writeln!(
        out,
        "  {:<13} {:<9} excluding families that decided with no captured reads",
        "adjusted",
        crate::memory::fmt_share(s.share_excluding_undenominated()),
    );
}

/// The same two halves as JSON, for a caller that would rather read the
/// numbers than the table.
fn policy_json(snapshot: &rift_recall_policy::ServePolicySnapshot) -> Value {
    let p = &snapshot.policy;
    let agents: Vec<Value> = ALPHA_FAMILIES
        .iter()
        .map(|agent| {
            let e = rift_recall_policy::resolve(p, agent.as_str(), family_capability(agent));
            json!({
                "agent": agent.as_str(),
                "interception_requested": e.read_deny_requested.as_str(),
                "interception": e.read_deny.as_str(),
                "search_answer": e.search_answer.as_str(),
                "grep_cap": e.grep_cap.as_str(),
                "doc_rewrite": e.doc_rewrite.as_str(),
                "doc_redirect": e.doc_redirect.as_str(),
                "note": cap_note(&e),
            })
        })
        .collect();
    json!({
        "source": source_name(snapshot.source),
        "fail_closed": p.fail_closed,
        "interception": {
            "mode": p.interception.mode.as_str(),
            "max_slice_bytes": p.interception.max_slice_bytes,
            "min_slice_lines": p.interception.min_slice_lines,
            "max_inline_hash_bytes": p.interception.max_inline_hash_bytes,
            "recovery_budget": p.interception.recovery_budget,
        },
        "search": {
            "mode": p.search.mode.as_str(),
            "grep_head_limit": p.search.grep_head_limit,
            "glob_top_n": p.search.glob_top_n,
            "answer_budget_tokens": p.search.answer_budget_tokens,
        },
        "doc": {
            "mode": p.doc.mode.as_str(),
            "channel": p.doc.channel.as_str(),
            "formats": p.doc.formats.as_str(),
            "table_rows": p.doc.table_rows,
            "max_input_bytes": p.doc.max_input_bytes,
            "pdf_min_chars_per_page": p.doc.pdf_min_chars_per_page,
            "convert_budget_ms": p.doc.convert_budget_ms,
            "ttl_days": p.doc.ttl_days,
        },
        "agents": agents,
    })
}

fn state_name<T: serde::Serialize>(state: &T) -> String {
    serde_json::to_value(state)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_else(|| "unknown".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser as _;
    use rift_recall_serve::{ToolName, TOOL_SURFACE};

    fn managed_facts() -> SupportFacts {
        SupportFacts {
            alpha: true,
            descriptor: rift_agent_install::native::NativeWiring::Managed,
            hooks: Some(rift_agent_install::native::NativeHookWiring::Managed),
            runtime_receipt: false,
            boundary_response: false,
            codex_trust: None,
            hook_failure: None,
        }
    }

    #[test]
    fn configured_without_a_receipt_is_not_full() {
        let (support, reason) = derive_support(managed_facts());
        assert_eq!(support, Support::ConfiguredUnverified);
        assert_eq!(reason.as_deref(), Some("no runtime hook receipt observed"));
    }

    #[test]
    fn a_receipt_without_a_boundary_response_is_not_full() {
        let (support, reason) = derive_support(SupportFacts {
            runtime_receipt: true,
            ..managed_facts()
        });
        assert_eq!(support, Support::ConfiguredUnverified);
        assert_eq!(
            reason.as_deref(),
            Some("no boundary response emission observed")
        );
    }

    #[test]
    fn all_four_support_facts_are_required_for_full() {
        let (support, reason) = derive_support(SupportFacts {
            runtime_receipt: true,
            boundary_response: true,
            ..managed_facts()
        });
        assert_eq!(support, Support::Full);
        assert_eq!(reason, None);
    }

    #[test]
    fn conflicting_or_missing_required_configuration_is_degraded() {
        for facts in [
            SupportFacts {
                descriptor: rift_agent_install::native::NativeWiring::Conflict,
                ..managed_facts()
            },
            SupportFacts {
                hooks: Some(rift_agent_install::native::NativeHookWiring::Invalid),
                ..managed_facts()
            },
            SupportFacts {
                hooks: Some(rift_agent_install::native::NativeHookWiring::Missing),
                ..managed_facts()
            },
        ] {
            let (support, reason) = derive_support(facts);
            assert_eq!(support, Support::Degraded);
            assert!(reason.is_some());
        }
    }

    #[test]
    fn codex_pending_trust_prevents_full() {
        let (support, reason) = derive_support(SupportFacts {
            runtime_receipt: true,
            boundary_response: true,
            codex_trust: Some(rift_agent_install::native::CodexTrust::Pending),
            ..managed_facts()
        });
        assert_eq!(support, Support::ConfiguredUnverified);
        assert_eq!(
            reason.as_deref(),
            Some("Codex hook trust is pending for this path")
        );
    }

    #[test]
    fn a_hook_failure_degrades_an_otherwise_full_integration() {
        let (support, reason) = derive_support(SupportFacts {
            runtime_receipt: true,
            boundary_response: true,
            hook_failure: Some("hook execution did not complete".to_owned()),
            ..managed_facts()
        });
        assert_eq!(support, Support::Degraded);
        assert_eq!(reason.as_deref(), Some("hook execution did not complete"));
    }

    #[test]
    fn every_non_alpha_adapter_is_always_coming_soon() {
        for agent in [
            "gemini",
            "cursor",
            "copilot-cli",
            "opencode",
            "factory-droid",
            "ollama",
        ] {
            assert!(!rift_agent_install::native::is_alpha_supported(agent));
            let facts = SupportFacts {
                alpha: false,
                descriptor: rift_agent_install::native::NativeWiring::Invalid,
                hooks: Some(rift_agent_install::native::NativeHookWiring::Conflict),
                runtime_receipt: true,
                boundary_response: true,
                ..managed_facts()
            };
            let (support, reason) = derive_support(facts);
            assert_eq!(support, Support::ComingSoon);
            assert_eq!(reason, None);
        }
    }

    #[test]
    fn support_fields_extend_the_existing_native_json_row() {
        let native = rift_agent_install::native::NativeIntegrationStatus {
            agent: "claude-code".to_owned(),
            display_name: "Claude Code".to_owned(),
            detection: rift_agent_install::native::NativeDetection::Detected,
            wiring: rift_agent_install::native::NativeWiring::Managed,
            executable: Some(std::path::PathBuf::from("/opt/claude")),
            installations: vec![rift_agent_install::native::NativeInstallation {
                source: rift_agent_install::native::NativeInstallSource::PathCommand,
                version: None,
                executable: Some(std::path::PathBuf::from("/opt/claude")),
                path: std::path::PathBuf::from("/opt/claude"),
            }],
            hook_wiring: Some(rift_agent_install::native::NativeHookWiring::Managed),
        };
        let assessment = SupportAssessment {
            agent: native.agent.clone(),
            support: Support::Full,
            hook_configuration: "managed".to_owned(),
            runtime_connection: "observed",
            context_delivery: "observed",
            requested_mode: "enforce",
            effective_mode: "enforce",
            degraded_reason: None,
        };
        let rows = integration_status_json(&[native], &[assessment]);
        let row = &rows[0];
        assert_eq!(row["display_name"], "Claude Code");
        assert_eq!(row["detection"], "detected");
        assert_eq!(row["wiring"], "managed");
        assert_eq!(row["support"], "full");
        assert_eq!(row["hook_configuration"], "managed");
        assert_eq!(row["runtime_connection"], "observed");
        assert_eq!(row["context_delivery"], "observed");
        assert_eq!(row["requested_mode"], "enforce");
        assert_eq!(row["effective_mode"], "enforce");
        assert_eq!(row["degraded_reason"], Value::Null);
    }

    #[test]
    fn only_attributable_boundary_rows_prove_context_delivery() {
        assert_eq!(
            boundary_agent_from_tool_name("antigravity:recovery-map").as_deref(),
            Some("antigravity")
        );
        assert_eq!(
            boundary_agent_from_tool_name("codex:session-pointer").as_deref(),
            Some("codex")
        );
        assert_eq!(
            boundary_agent_from_tool_name("antigravity:read-replacement-fallback").as_deref(),
            Some("antigravity")
        );
        assert_eq!(boundary_agent_from_tool_name("codex:read-gate"), None);
        assert_eq!(boundary_agent_from_tool_name("known-state"), None);
    }

    /// The committed pins, by `include_str!` rather than a runtime read, so a
    /// row that outlives the artifact it quotes fails to compile rather than
    /// failing on someone's machine.
    const CLAUDE_CODE_TOOL_FIELDS: &str = include_str!(
        "../../../crates/rift-agent-claude-code/fixtures/claude-code-tool-field-surface.md"
    );
    const ANTIGRAVITY_TOOL_FIELDS: &str = include_str!(
        "../../../crates/rift-agent-antigravity/fixtures/antigravity-tool-field-surface.md"
    );

    /// ★ Every edit-echo row quotes the artifact it names, and the two
    /// families whose artifacts say nothing about what an edit returns have no
    /// row — the fact that keeps a decline off an agent's only sight of its
    /// own edit is a pin, not an opinion.
    #[test]
    fn an_edit_echo_row_is_quoted_by_the_pinned_artifact_it_names() {
        assert_eq!(
            PINNED_EDIT_ECHO.len(),
            1,
            "one family is pinned today; a new row needs its artifact quoted here too"
        );
        for (family, excerpt) in PINNED_EDIT_ECHO {
            assert_eq!(family, &rift_core::AgentId::CLAUDE_CODE);
            assert!(
                CLAUDE_CODE_TOOL_FIELDS.contains(excerpt),
                "the pinned artifact does not contain {excerpt:?}"
            );
        }

        assert!(
            !family_capability(&rift_core::AgentId::ANTIGRAVITY).pinned_edit_echo,
            "the page says nothing about what this family's edit tools return"
        );
        assert!(
            ANTIGRAVITY_TOOL_FIELDS.contains(
                "Nothing on the page says what\n`replace_file_content` or `write_to_file` returns."
            ),
            "and its artifact is where that is written down"
        );
        assert!(!family_capability(&rift_core::AgentId::CODEX).pinned_edit_echo);
        assert!(family_capability(&rift_core::AgentId::CLAUDE_CODE).pinned_edit_echo);
    }

    /// Minimal satisfying argv per verb: ranked verbs need the query-aware
    /// params, target verbs a target, retrieve-exact its mandatory reason.
    fn argv_for(t: ToolName) -> Vec<&'static str> {
        match t {
            ToolName::KnownState | ToolName::ContextPack => {
                vec![t.verb_name(), "--task", "t", "--file", "a.rs"]
            }
            // The shape the manifest's hint teaches: a path and the revision
            // the caller holds, and no ranking params at all.
            ToolName::ChangedHunks => {
                vec![t.verb_name(), "a.rs", "--known-hash", "aaaaaaaaaaaaaaaa"]
            }
            ToolName::DiffSince | ToolName::CostHotspots => vec![t.verb_name()],
            ToolName::FileIfChanged | ToolName::ReadSummary => vec![t.verb_name(), "a.rs"],
            ToolName::RetrieveExact => {
                vec![t.verb_name(), "file:a.rs@sha256:aa", "--reason", "why"]
            }
            // A path, and nothing else: the document verb takes no flags, and
            // what it needs beyond the path is resolved rather than typed.
            ToolName::DocRead => vec![t.verb_name(), "spec.docx"],
            // A query, and nothing else that is required: which paths matched
            // and what the lines in them say is resolved, not typed.
            ToolName::Search => vec![t.verb_name(), "read gate"],
        }
    }

    /// Parse a full `rift recall …` argv and hand back the `RecallCmd`.
    fn parse(argv: &[&str]) -> RecallCmd {
        let mut full = vec!["rift", "recall"];
        full.extend_from_slice(argv);
        match crate::cli::Cli::try_parse_from(full)
            .expect("argv parses")
            .cmd
        {
            Some(crate::cli::Command::Recall(r)) => r,
            other => panic!("expected a recall subcommand, got {other:?}"),
        }
    }

    /// Render the policy half of `rift recall status` over a repository whose
    /// `.rift/policies/serve.toml` is `src` (absent when `None`).
    fn status_policy(src: Option<&str>) -> (String, Value) {
        let dir = tempfile::tempdir().unwrap();
        let rift_dir = dir.path().join(".rift");
        if let Some(src) = src {
            let pol = rift_dir.join("policies");
            std::fs::create_dir_all(&pol).unwrap();
            std::fs::write(pol.join("serve.toml"), src).unwrap();
        }
        let snapshot = rift_recall_policy::load_serve_policy(&rift_dir);
        let mut out = String::new();
        render_policy(&mut out, &snapshot);
        (out, policy_json(&snapshot))
    }

    /// The status table carries the figure this plane is judged by, one line
    /// per family, and names the family that decided with no captured reads
    /// rather than folding it silently into the overall number.
    #[test]
    fn status_prints_a_savings_share_for_every_family() {
        use rift_recall_ledger::savings::{FamilyShare, Savings};
        let report = crate::memory::SavingsReport {
            projection_db: "p".to_owned(),
            recall_db: "r".to_owned(),
            note: None,
            savings: Savings {
                tokens_avoided: 1_000,
                tokens_prevented: 0,
                tokens_returned: 160,
                read_tokens: 9_000,
                channels: rift_recall_ledger::savings::ChannelTokens::default(),
                reads_unmeasured: 0,
                sessions_with_reads: 3,
                families: vec![
                    FamilyShare {
                        family: "claude-code".to_owned(),
                        tokens_avoided: 500,
                        tokens_prevented: 0,
                        tokens_returned: 160,
                        decisions: 4,
                        read_tokens: 9_000,
                        channels: rift_recall_ledger::savings::ChannelTokens::default(),
                        reads_unmeasured: 0,
                        sessions_with_reads: 3,
                    },
                    FamilyShare {
                        family: "codex".to_owned(),
                        tokens_avoided: 500,
                        tokens_prevented: 0,
                        tokens_returned: 0,
                        decisions: 3,
                        read_tokens: 0,
                        channels: rift_recall_ledger::savings::ChannelTokens::default(),
                        reads_unmeasured: 0,
                        sessions_with_reads: 0,
                    },
                ],
            },
            window: rift_recall_ledger::savings_window::Boundary::SinceReset,
            enforce_cut_ms: None,
            enforce_cut_source: "none".to_owned(),
            observe_sessions: 0,
            enforce_sessions: 0,
        };
        let mut out = String::new();
        render_savings_shares(&mut out, &report);
        assert!(out.contains("overall       10.0%"), "{out}");
        assert!(out.contains("n 3 session(s) with reads"), "{out}");
        assert!(out.contains("claude-code   5.3%"), "{out}");
        assert!(
            out.contains("codex         100.0%") && out.contains("decided, no captured reads"),
            "{out}"
        );
        assert!(out.contains("adjusted      5.3%"), "{out}");
    }

    /// An unresolvable family says so, on the same table, instead of printing
    /// a percentage nobody measured.
    #[test]
    fn an_unmeasured_family_prints_the_reason_not_a_percentage() {
        let report = crate::memory::SavingsReport {
            projection_db: "(unresolved)".to_owned(),
            recall_db: "(unresolved)".to_owned(),
            note: Some("the family's stores did not resolve".to_owned()),
            savings: rift_recall_ledger::savings::Savings::default(),
            window: rift_recall_ledger::savings_window::Boundary::SinceReset,
            enforce_cut_ms: None,
            enforce_cut_source: "not resolved".to_owned(),
            observe_sessions: 0,
            enforce_sessions: 0,
        };
        let mut out = String::new();
        render_savings_shares(&mut out, &report);
        assert!(out.contains("not measured"), "{out}");
        assert!(!out.contains('%'), "{out}");
    }

    /// **The two floors are one measurement.**
    ///
    /// `rift-agent` ships the SDK's default and `rift-recall-policy` the
    /// policy's; they are separate constants because the two crates are used
    /// separately, and neither depends on the other, so nothing inside either
    /// can hold them together. This binary depends on both, so this is where
    /// the reconciliation can live at all — and it matters, because the floor
    /// is a consequence of the frame's measured cost and a frame that grew must
    /// move both or neither.
    #[test]
    fn the_sdk_and_the_policy_ship_the_same_slice_floor() {
        assert_eq!(
            rift_agent::DEFAULT_MIN_SLICE_LINES,
            rift_recall_policy::DEFAULT_MIN_SLICE_LINES,
            "the two floors drifted apart; both follow the reason frame's \
             measured cost (`the_reason_frame_pays_for_itself_at_the_floor`)"
        );
    }

    /// What an operator is owed by a status line: the three tables as the file
    /// was read, and then — for every family this program implements — what
    /// each of that family's levers may actually do. The two halves differ,
    /// which is the whole reason both are printed.
    #[test]
    fn status_prints_the_three_tables_and_a_mode_for_every_family() {
        let (out, json) = status_policy(None);
        assert!(out.contains(".rift/policies/serve.toml: absent; defaults apply"));
        assert!(out.contains(
            "interception  mode enforce  max_slice_bytes 98304  min_slice_lines 28  max_inline_hash_bytes 1048576  recovery_budget 800"
        ));
        assert!(out.contains(
            "search        mode enforce  grep_head_limit 40  glob_top_n 8  answer_budget_tokens 160"
        ));
        assert!(out.contains(
            "doc           mode enforce  channel rewrite  formats all  table_rows 200  max_input_bytes 16777216"
        ));
        assert!(out.contains("pdf_min_chars_per_page 200  convert_budget_ms 250  ttl_days 30"));
        assert!(
            out.contains("agent         re-read   listing   grep cap  doc read  doc decline  note")
        );
        assert!(
            out.contains("claude-code   enforce   enforce   enforce   enforce   enforce      -")
        );
        for family in ["codex", "antigravity"] {
            assert!(
                out.contains(&format!(
                    "{family:<13} enforce   enforce   off       off       enforce"
                )),
                "{family} uses native declines where it has no document/search amendment channel:\n{out}"
            );
        }
        assert_eq!(json["source"], "absent; defaults apply");
        assert_eq!(json["fail_closed"], false);
        assert_eq!(json["doc"]["ttl_days"], 30);
        assert_eq!(json["agents"][0]["agent"], "claude-code");
        assert_eq!(json["agents"][0]["grep_cap"], "enforce");
    }

    /// Retry telemetry is visible in metrics, never a support-tier or mode
    /// gate. Repository-wide enforcement therefore reaches all three alpha
    /// families without an agent-specific override.
    #[test]
    fn retry_telemetry_never_demotes_a_supported_family_on_the_status_line() {
        let (out, json) = status_policy(Some("[interception]\nmode = \"enforce\"\n"));
        assert!(
            out.contains("interception  mode enforce"),
            "the file asked for it:\n{out}"
        );
        for family in ["claude-code", "codex", "antigravity"] {
            let row = out
                .lines()
                .find(|line| line.trim_start().starts_with(family))
                .unwrap_or_else(|| panic!("{family} has no row:\n{out}"));
            assert!(row.contains("enforce"), "{family} is not active: {row}");
            assert!(
                !row.contains("measured retry"),
                "{family} has a stale retry caveat: {row}"
            );
        }
        let agents = json["agents"].as_array().expect("one row per family");
        let codex = agents
            .iter()
            .find(|row| row["agent"] == "codex")
            .expect("a codex row");
        assert_eq!(codex["interception"], "enforce");
        assert_eq!(codex["interception_requested"], "enforce");
        assert_eq!(codex["note"], "no native document/search amendment channel");
    }

    /// A file Rift could not read is not a file it works around. The status
    /// line says so and shows every lever off — including the two that decline
    /// nothing and would otherwise act.
    #[test]
    fn an_unreadable_policy_shows_every_lever_off_and_says_why() {
        let (out, json) = status_policy(Some("[doc]\nttl_days = 0\n"));
        assert!(out.contains(".rift/policies/serve.toml: invalid; every lever off"));
        for family in ["claude-code", "codex", "antigravity"] {
            assert!(
                out.contains(&format!(
                    "{family:<13} off       off       off       off       off"
                )),
                "{family} still has a live lever:\n{out}"
            );
        }
        assert_eq!(json["fail_closed"], true);
        assert_eq!(json["interception"]["mode"], "off");
    }

    /// What a family may be asked for is read from the one table that asserts
    /// a channel from a committed pin, never from a list kept here. The three
    /// families' pins differ in exactly the ways the levers care about.
    #[test]
    fn the_capability_bridge_reads_the_pinned_table() {
        let claude = family_capability(&rift_core::AgentId::CLAUDE_CODE);
        assert!(claude.confirmed_reason_channel);
        assert!(claude.document_sidecar_rewrite && claude.grep_head_limit_rewrite);

        let codex = family_capability(&rift_core::AgentId::CODEX);
        assert!(
            codex.confirmed_reason_channel,
            "a recorded run on the client that has the surface confirmed it"
        );
        assert!(!codex.document_sidecar_rewrite && !codex.grep_head_limit_rewrite);
        assert!(
            !codex.pinned_edit_echo,
            "nothing pins what this family's edit tool tells the model it wrote"
        );

        let antigravity = family_capability(&rift_core::AgentId::ANTIGRAVITY);
        assert!(antigravity.confirmed_reason_channel);
        assert!(!antigravity.document_sidecar_rewrite && !antigravity.grep_head_limit_rewrite);

        // A family with no rows at all is a family with no channel, not a
        // family that inherits somebody else's.
        let unknown = family_capability(&rift_core::AgentId::new("some-editor"));
        assert_eq!(unknown, rift_recall_policy::FamilyCapability::default());
    }

    /// Naming one supported family explicitly does not change the other
    /// supported families' repository-wide enforcement.
    #[test]
    fn an_agent_override_does_not_demote_other_supported_families() {
        let (out, json) = status_policy(Some(
            "[interception]\nmode = \"enforce\"\n\n[interception.agents]\nantigravity = \"enforce\"\n",
        ));
        assert!(out.contains("claude-code   enforce"));
        assert!(out.contains("codex         enforce"), "{out}");
        assert!(out.contains("antigravity   enforce"));
        assert_eq!(json["agents"][1]["interception_requested"], "enforce");
        assert_eq!(json["agents"][1]["interception"], "enforce");
    }

    #[test]
    fn human_status_names_path_availability_without_implying_active_use() {
        let mut out = String::new();
        render_status(
            &mut out,
            &[rift_agent_install::native::NativeIntegrationStatus {
                agent: "codex".to_owned(),
                display_name: "Codex".to_owned(),
                detection: rift_agent_install::native::NativeDetection::Detected,
                wiring: rift_agent_install::native::NativeWiring::Missing,
                executable: Some(std::path::PathBuf::from("/usr/local/bin/codex")),
                installations: vec![
                    rift_agent_install::native::NativeInstallation {
                        source: rift_agent_install::native::NativeInstallSource::PathCommand,
                        version: None,
                        executable: Some(std::path::PathBuf::from("/usr/local/bin/codex")),
                        path: std::path::PathBuf::from("/usr/local/bin/codex"),
                    },
                    rift_agent_install::native::NativeInstallation {
                        source: rift_agent_install::native::NativeInstallSource::IdeExtension(
                            rift_agent_install::native::NativeIdeHost::VsCode,
                        ),
                        version: Some("26.803.61601".to_owned()),
                        executable: None,
                        path: std::path::PathBuf::from(
                            "/home/test/.vscode/extensions/openai.chatgpt-26.803.61601",
                        ),
                    },
                ],
                hook_wiring: Some(rift_agent_install::native::NativeHookWiring::Managed),
            }],
        );
        assert!(out.contains("present via PATH command + VS Code extension 26.803.61601"));
        assert!(out.contains("managed"));
        assert!(!out.contains("detected"));
    }

    /// The anti-drift property that makes this surface trustworthy: clap's
    /// subcommand spelling, the dispatcher's verb spelling, and the tool the
    /// call lands on are the same closed set of eight. A tool added to
    /// `TOOL_SURFACE` without a CLI verb (or vice versa) fails here rather
    /// than shipping a surface that advertises a command that does not run.
    #[test]
    fn every_dispatcher_tool_has_a_clap_verb_that_decodes_back_to_it() {
        for tool in TOOL_SURFACE.iter().copied() {
            let cmd = parse(&argv_for(tool));
            let (verb, params) = verb_and_params(&cmd, Path::new("/repo")).expect("a skill verb");
            assert_eq!(verb, tool.verb_name());
            let (decoded, _req) = decode_verb(verb, &params).expect("the verb decodes");
            assert_eq!(decoded, tool, "clap verb {verb} must land on {tool:?}");
        }
    }

    /// The lifecycle subcommands are terminal-owned, not part of the agent's
    /// bounded surface — they must never be routed through the dispatcher.
    #[test]
    fn lifecycle_subcommands_are_not_skill_verbs() {
        for argv in [vec!["init"], vec!["status"], vec!["disable"]] {
            assert!(
                verb_and_params(&parse(&argv), Path::new("/repo")).is_none(),
                "{argv:?}"
            );
        }
    }

    /// argv reaches the dispatcher verbatim: repeated flags accumulate, and an
    /// absent `--token-budget` means "unbudgeted" (0), never a silent default
    /// the agent did not ask for.
    #[test]
    fn repeated_flags_accumulate_and_absent_budget_is_zero() {
        let cmd = parse(&[
            "known-state",
            "--task",
            "fix the gate",
            "--file",
            "a.rs",
            "--file",
            "b.rs",
            "--error",
            "E0432",
            "--known-hash",
            "aa",
            "--known-hash",
            "bb",
        ]);
        let (_verb, params) = verb_and_params(&cmd, Path::new("/repo")).unwrap();
        assert_eq!(
            params["files_in_focus"],
            serde_json::json!(["a.rs", "b.rs"])
        );
        assert_eq!(params["error_signatures"], serde_json::json!(["E0432"]));
        assert_eq!(params["known_hashes"], serde_json::json!(["aa", "bb"]));
        assert_eq!(params["token_budget"], serde_json::json!(0));
    }

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
            estimated_tokens_returned: 1168,
            estimated_tokens_avoided: 2_256_482,
            tokenizer: "cl100k_base",
            payload: json!({ "state": "known" }),
            event: MemdEvent::ManifestServed {
                estimated_tokens_returned: 1168,
                estimated_tokens_avoided: 2_256_482,
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
            estimated_tokens_returned: 12,
            estimated_tokens_avoided: 340,
            tokenizer: "cl100k_base",
            payload: json!({ "state": "known" }),
            event: MemdEvent::ManifestServed {
                estimated_tokens_returned: 12,
                estimated_tokens_avoided: 340,
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
            estimated_tokens_returned: 1,
            estimated_tokens_avoided: 2,
            tokenizer: "cl100k_base",
            payload: json!({}),
            event: MemdEvent::ManifestServed {
                estimated_tokens_returned: 1,
                estimated_tokens_avoided: 2,
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

    /// The focus set reaches `PackKey` repo-relative, whichever spelling the
    /// caller used. Two members of one family asking the same question about
    /// the same file must compute the same key, or the pack cache stops
    /// compounding exactly where a shared store is supposed to start.
    #[test]
    fn focus_paths_are_normalised_to_repo_relative() {
        let repo = std::path::Path::new("/fam/main");
        assert_eq!(
            focus_paths(
                repo,
                &[
                    "/fam/main/src/a.rs".to_owned(),
                    "./src/b.rs".to_owned(),
                    "src/c.rs".to_owned(),
                    "/elsewhere/d.rs".to_owned(),
                ]
            ),
            vec![
                "src/a.rs".to_owned(),
                "src/b.rs".to_owned(),
                "src/c.rs".to_owned(),
                // Not ours to rewrite; it will simply match nothing.
                "/elsewhere/d.rs".to_owned(),
            ]
        );
    }

    /// The flag reaches the grant on **every** verb, through the production
    /// argv path — clap parse, then the same function `serve` calls.
    ///
    /// Per-verb rather than once, because the argument lives on five separate
    /// clap structs: a verb whose struct missed the field would advertise a
    /// flag that parses nowhere, or parse one that widens nothing.
    ///
    /// Every verb a scope contains, which is every verb whose answer comes out
    /// of the projection. The document verb is not one: a sidecar is keyed by
    /// the document's own bytes and is the same file for every member of the
    /// family, so there is no branch axis to widen — and a flag with nothing
    /// behind it is exactly what this test exists to prevent.
    #[test]
    fn the_cross_branch_flag_reaches_the_grant_on_every_scoped_verb() {
        for tool in TOOL_SURFACE.iter().copied().filter(|t| t.is_scope_bound()) {
            let mut argv = argv_for(tool);
            argv.push("--cross-branch");
            argv.push("reviewing the sibling worktree");
            let cmd = parse(&argv);
            let grant = cross_branch_grant(&cmd).expect("a real reason is accepted");
            assert!(grant.branches(), "{tool:?} did not widen the branch axis");
            assert_eq!(grant.reason(), Some("reviewing the sibling worktree"));
            assert!(
                !grant.developers() && !grant.repos(),
                "{tool:?}: --cross-branch widens ONE axis"
            );
        }
    }

    /// Absent flag, no grant — which is what makes the audited tag mean
    /// something when it is there.
    #[test]
    fn an_absent_cross_branch_flag_builds_no_grant() {
        for tool in TOOL_SURFACE.iter().copied() {
            let grant = cross_branch_grant(&parse(&argv_for(tool))).expect("no flag, no refusal");
            assert!(!grant.is_any(), "{tool:?}");
            assert!(grant.reason().is_none());
        }
        // The lifecycle subcommands have no scope to widen at all.
        for argv in [vec!["init"], vec!["status"], vec!["disable"]] {
            assert!(!cross_branch_grant(&parse(&argv)).unwrap().is_any());
        }
    }

    /// The bound is in BYTES and the refusal says so — a usage error, decided
    /// from argv before a store is opened.
    #[test]
    fn a_cross_branch_reason_over_the_byte_bound_is_a_usage_error() {
        let over = "a".repeat(MAX_GRANT_REASON_BYTES + 1);
        let argv = vec![
            "known-state",
            "--task",
            "t",
            "--file",
            "a.rs",
            "--cross-branch",
            over.as_str(),
        ];
        let refusal = cross_branch_grant(&parse(&argv)).expect_err("over the bound is refused");
        assert_eq!(refusal.code, exit::USAGE);
        assert_eq!(refusal.json["error"], "cross_branch_reason_too_long");
        assert_eq!(
            refusal.json["detail"]["max_reason_bytes"],
            serde_json::json!(MAX_GRANT_REASON_BYTES),
            "the refusal names the bound"
        );
        assert!(refusal.human.contains(&MAX_GRANT_REASON_BYTES.to_string()));

        // Exactly at the bound is accepted: the refusal is `>`, not `>=`.
        let at = "a".repeat(MAX_GRANT_REASON_BYTES);
        let argv = vec![
            "known-state",
            "--task",
            "t",
            "--file",
            "a.rs",
            "--cross-branch",
            at.as_str(),
        ];
        assert!(cross_branch_grant(&parse(&argv)).unwrap().branches());

        // BYTES, not characters: 101 two-byte characters is 202 bytes and over
        // a 200-byte bound, while being well under it counted as characters.
        let wide = "é".repeat(MAX_GRANT_REASON_BYTES / 2 + 1);
        assert!(wide.chars().count() <= MAX_GRANT_REASON_BYTES);
        assert!(wide.len() > MAX_GRANT_REASON_BYTES);
        let argv = vec![
            "known-state",
            "--task",
            "t",
            "--file",
            "a.rs",
            "--cross-branch",
            wide.as_str(),
        ];
        let refusal = cross_branch_grant(&parse(&argv)).expect_err("bytes, not characters");
        assert_eq!(refusal.json["error"], "cross_branch_reason_too_long");
    }

    /// A blank reason is said out loud rather than silently granting nothing.
    #[test]
    fn a_blank_cross_branch_reason_is_refused_rather_than_swallowed() {
        for blank in ["", "   ", "\t"] {
            let argv = vec!["diff-since", "--cross-branch", blank];
            let refusal = cross_branch_grant(&parse(&argv)).expect_err("blank is refused");
            assert_eq!(refusal.code, exit::USAGE);
            assert_eq!(refusal.json["error"], "cross_branch_reason_required");
        }
    }
    // --- the document verb ---------------------------------------------------

    /// The two spellings are ONE verb, and this is where that is checkable.
    ///
    /// `rift doc read <path>` is an alias, not a second command: it parses into
    /// the same arguments the long spelling parses into and routes through the
    /// same function, so a change to one cannot leave the other behind. The
    /// census records it `false` — guidance names the taught spelling — and
    /// this is the assertion that the untaught spelling still answers the same
    /// thing.
    #[test]
    fn the_document_alias_and_the_long_spelling_are_one_verb() {
        let long = parse(&["doc-read", "docs/plan.docx"]);
        let short = match crate::cli::Cli::try_parse_from(["rift", "doc", "read", "docs/plan.docx"])
            .expect("the alias parses")
            .cmd
        {
            Some(crate::cli::Command::Doc(crate::cli::DocCmd::Read(a))) => {
                crate::cli::RecallCmd::DocRead(a)
            }
            other => panic!("expected `doc read`, got {other:?}"),
        };
        let here = Path::new("/repo");
        let (verb, params) = verb_and_params(&long, here).expect("a skill verb");
        assert_eq!(verb, ToolName::DocRead.verb_name());
        assert_eq!(
            verb_and_params(&short, here),
            Some((verb, params)),
            "the alias must reach the same verb with the same params"
        );
    }

    /// A repository with no `.rift` policy and no family: enough to drive every
    /// decision this verb makes from the caller's own file.
    fn doc_repo() -> tempfile::TempDir {
        tempfile::tempdir().expect("a temp repo")
    }

    fn doc_request(path: &str) -> rift_recall_serve::ToolRequest {
        let (_tool, req) = rift_recall_serve::decode_verb("doc-read", &json!({ "target": path }))
            .expect("the verb decodes");
        req
    }

    fn resolve_in(
        repo: &Path,
        path: &str,
    ) -> std::result::Result<rift_recall_serve::ToolRequest, Outcome> {
        let cfg = rift_config::EffectiveConfig::secure_default().expect("a default config");
        resolve_document(ToolName::DocRead, doc_request(path), repo, &cfg)
    }

    /// Everything decidable from the caller's own file is decided before a
    /// store is opened, and each refusal says which of the four it is. They are
    /// USAGE refusals on purpose: none of them is the serving surface saying
    /// no, so none of them books a serve.
    #[test]
    fn what_is_not_a_document_is_refused_before_anything_is_opened() {
        let dir = doc_repo();
        let repo = dir.path();
        std::fs::write(repo.join("notes.md"), "# notes\n").unwrap();
        std::fs::write(repo.join("plan.docx"), b"PK\x03\x04binary").unwrap();

        for (path, tag) in [
            ("notes.md", "not_a_document"),
            ("src/main.rs", "not_a_document"),
            ("missing.docx", "no_such_document"),
            ("../outside.docx", "path_outside_repository"),
        ] {
            let refusal = resolve_in(repo, path).expect_err(path);
            assert_eq!(refusal.code, exit::USAGE, "{path}");
            assert_eq!(refusal.json["error"], tag, "{path}");
        }

        // A document over the size the family converts is told now, rather than
        // left waiting for a conversion that will never be attempted.
        let policies = repo.join(".rift/policies");
        std::fs::create_dir_all(&policies).unwrap();
        std::fs::write(policies.join("serve.toml"), "[doc]\nmax_input_bytes = 4\n").unwrap();
        let refusal = resolve_in(repo, "plan.docx").expect_err("over the cap");
        assert_eq!(refusal.code, exit::USAGE);
        assert_eq!(refusal.json["error"], "document_over_the_conversion_cap");

        // And a repository that switched documents off answers that, rather
        // than converting or pretending there is nothing there.
        std::fs::write(policies.join("serve.toml"), "[doc]\nmode = \"off\"\n").unwrap();
        let refusal = resolve_in(repo, "plan.docx").expect_err("documents are off");
        assert_eq!(refusal.code, exit::PRECONDITION);
        assert_eq!(refusal.json["error"], "doc_off");
    }

    /// A document Rift converts is resolved to its identity and the declared
    /// cost of the read this verb replaces — and, with no family store behind
    /// it, to the honest `Owed`. Nothing is converted on this path.
    #[test]
    fn a_document_resolves_to_its_identity_and_its_declared_cost() {
        let dir = doc_repo();
        let repo = dir.path();
        let bytes = b"PK\x03\x04 a container full of bytes nobody can read as text";
        std::fs::write(repo.join("plan.docx"), bytes).unwrap();

        let req = resolve_in(repo, "plan.docx").expect("a document resolves");
        assert_eq!(req.target.as_deref(), Some("plan.docx"));
        let doc = req.document.expect("the document half is resolved");
        assert_eq!(doc.source_sha256, rift_shadow::sha256_hex(&[bytes]));
        assert_eq!(
            doc.declared_baseline,
            rift_recall_serve::declared_document_baseline("docx", bytes),
            "the declaration is the one function that prices a native read"
        );
        assert_eq!(doc.sidecar, rift_recall_serve::SidecarState::Owed);

        // The verb takes the path as typed — absolute or repo-relative — and
        // answers about the same file either way.
        let absolute = repo.join("plan.docx");
        let same = resolve_in(repo, &absolute.to_string_lossy()).expect("an absolute path");
        assert_eq!(same.target.as_deref(), Some("plan.docx"));

        // A PDF declares nothing: its pages are images, and no pinned artifact
        // gives a vision-token rate.
        std::fs::write(repo.join("plan.pdf"), bytes).unwrap();
        let pdf = resolve_in(repo, "plan.pdf").expect("a pdf resolves");
        assert_eq!(pdf.document.expect("resolved").declared_baseline, 0);
    }

    /// What the family's store says, in this member's words: the Markdown and a
    /// path inside this checkout when it holds one, the decision when it has
    /// made one, and `Owed` when it has neither.
    #[cfg(feature = "doc")]
    #[test]
    fn the_family_store_answers_in_this_members_own_paths() {
        use rift_recall_serve::SidecarState;

        let family = doc_repo();
        let member = doc_repo();
        let store = crate::doc_store::DocStore::for_member(family.path(), member.path());
        let key = "a".repeat(64);

        assert_eq!(
            held_sidecar(&store, &key, member.path()),
            SidecarState::Owed
        );

        store
            .put(&key, "# converted\n")
            .expect("the family converts once");
        match held_sidecar(&store, &key, member.path()) {
            SidecarState::Held(sidecar) => {
                assert_eq!(sidecar.markdown, "# converted\n");
                assert_eq!(sidecar.path, format!(".rift/state/doc/{key}.md"));
                assert!(
                    member.path().join(&sidecar.path).is_file(),
                    "the answer names a path inside the checkout the caller is standing in"
                );
            }
            other => panic!("the family holds it: {other:?}"),
        }

        // A decision against a document outranks the answer: it is a fact about
        // the document, and nothing will convert it later either.
        let declined = "b".repeat(64);
        store.mark_skipped(&declined, "thin");
        assert_eq!(
            held_sidecar(&store, &declined, member.path()),
            SidecarState::Declined
        );
    }

    // --- the search verb -----------------------------------------------------

    /// A checkout with a couple of files in it and no `.rift` policy: enough to
    /// drive every decision this resolver makes, and nothing it does not.
    #[cfg(feature = "search")]
    fn search_repo(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("a temp repo");
        for (rel, body) in files {
            let path = dir.path().join(rel);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(path, body).unwrap();
        }
        dir
    }

    #[cfg(feature = "search")]
    fn search_request(query: &str) -> rift_recall_serve::ToolRequest {
        let (_tool, req) = rift_recall_serve::decode_verb("search", &json!({ "target": query }))
            .expect("the verb decodes");
        req
    }

    #[cfg(feature = "search")]
    fn resolve_search_in(
        repo: &Path,
        query: &str,
        content: bool,
    ) -> std::result::Result<rift_recall_serve::ToolRequest, Outcome> {
        let cfg = rift_config::EffectiveConfig::secure_default().expect("a default config");
        let store = rift_store::SqliteStore::open_in_memory().expect("a projection");
        rift_store::apply_migrations(&store, &rift_store::recall_migrations()).expect("migrated");
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("a runtime");
        rt.block_on(resolve_search(
            rift_recall_serve::ToolName::Search,
            search_request(query),
            content,
            repo,
            &cfg,
            &store,
        ))
    }

    /// The resolver hands the surface a ranked answer over this checkout's own
    /// files, and prices the listing it stands in for.
    ///
    /// What is asserted here is the join the surface cannot see: the engine ran
    /// over the member's index, the paths came back best-first for the query,
    /// the total is the whole match set rather than the served prefix, and the
    /// declared baseline is a real figure over those same paths.
    #[cfg(feature = "search")]
    #[test]
    fn a_search_resolves_this_checkouts_own_paths_and_prices_the_listing() {
        let dir = search_repo(&[
            ("bins/rift/src/hooks.rs", "fn read_gate() {}\n"),
            ("crates/rift-recall-serve/src/lib.rs", "fn dispatch() {}\n"),
            ("README.md", "# rift\n"),
        ]);
        let req = resolve_search_in(dir.path(), "hooks", false).expect("a resolved search");
        let search = req.search.as_ref().expect("the resolver attached one");
        assert!(!search.content);
        let rift_recall_serve::IndexState::Answered { hits, total, lines } = &search.index else {
            panic!("a walkable checkout is not pending");
        };
        assert_eq!(
            hits.first().map(|hit| hit.path.as_str()),
            Some("bins/rift/src/hooks.rs"),
            "the query's own word ranks first: {hits:?}"
        );
        assert!(lines.is_empty(), "no content was asked for");
        assert!(*total >= 1);
        assert!(
            search.declared_baseline > 0,
            "a listing of real paths is not free"
        );
        // The query travels as the target and nothing else: the ranked answer
        // is resolved, never typed.
        assert_eq!(req.target.as_deref(), Some("hooks"));
    }

    /// `--content` reads the lines out of the files that ranked, and the answer
    /// claims nothing for them.
    ///
    /// **Scoped to the ranked paths, deliberately, and it is a real limit.** A
    /// content search here is "of the files this query is about, which lines
    /// match", so a literal that appears in no path is not found — where a
    /// native grep would have walked the whole tree. That is a narrower answer,
    /// never a wrong one, and narrowing is the direction this plane errs in:
    /// the caller still has its own grep, and this verb never claims to have
    /// replaced one — it declares no baseline for a content call at all.
    #[cfg(feature = "search")]
    #[test]
    fn a_content_search_resolves_the_matching_lines_and_declares_no_baseline() {
        let dir = search_repo(&[
            (
                "bins/rift/src/hooks.rs",
                "fn hooks_gate() {}\nfn other() {}\n",
            ),
            ("README.md", "# rift\n"),
        ]);
        let req = resolve_search_in(dir.path(), "hooks", true).expect("a resolved search");
        let search = req.search.as_ref().expect("the resolver attached one");
        assert!(search.content);
        assert_eq!(
            search.declared_baseline, 0,
            "a content search has no declared native cost"
        );
        let rift_recall_serve::IndexState::Answered { lines, .. } = &search.index else {
            panic!("a walkable checkout is not pending");
        };
        assert_eq!(lines.len(), 1, "{lines:?}");
        let line = lines.first().expect("one line");
        assert_eq!(line.path, "bins/rift/src/hooks.rs");
        assert_eq!(line.line, 1);
        assert_eq!(line.text, "fn hooks_gate() {}");

        // The limit, stated as a test rather than as a sentence: a literal no
        // ranked path is about is not found, and the answer is empty rather
        // than a walk of the tree.
        let elsewhere = resolve_search_in(dir.path(), "readme", true).expect("a resolved search");
        let rift_recall_serve::IndexState::Answered { lines, .. } = &elsewhere
            .search
            .as_ref()
            .expect("the resolver attached one")
            .index
        else {
            panic!("a walkable checkout is not pending");
        };
        assert!(
            lines.iter().all(|line| line.text != "fn other() {}"),
            "a content search is scoped to the paths the query ranked: {lines:?}"
        );
    }

    /// Two refusals, both decided before the engine runs: a query with nothing
    /// in it, and a repository that has switched search off.
    ///
    /// The second is the owner's one kill switch for this plane, and it has to
    /// close the verb as well as the hook arms — a lever that stays reachable
    /// through a second door is not switched off.
    #[cfg(feature = "search")]
    #[test]
    fn an_empty_query_and_a_repository_with_search_off_are_refused() {
        let dir = search_repo(&[("README.md", "# rift\n")]);
        let blank = resolve_search_in(dir.path(), "   ", false).expect_err("blank is refused");
        assert_eq!(blank.code, exit::USAGE);
        assert_eq!(blank.json["error"], "empty_query");

        let policies = dir.path().join(".rift/policies");
        std::fs::create_dir_all(&policies).unwrap();
        std::fs::write(policies.join("serve.toml"), "[search]\nmode = \"off\"\n").unwrap();
        let off = resolve_search_in(dir.path(), "hooks", false).expect_err("switched off");
        assert_eq!(off.code, exit::PRECONDITION);
        assert_eq!(off.json["error"], "search_off");
    }

    /// A sensitive path is not in the index at all, so it cannot be ranked,
    /// named or grepped by anything downstream.
    ///
    /// The surface screens the deny list a second time; this is the screen
    /// *before* it, and it is the reason the second one has nothing to do in
    /// the ordinary case. The fixture asserts its own premise so it cannot pass
    /// because a built-in glob moved.
    #[cfg(feature = "search")]
    #[test]
    fn a_sensitive_path_never_enters_the_index_a_search_ranks() {
        let cfg = rift_config::EffectiveConfig::secure_default().expect("a default config");
        assert!(
            cfg.is_sensitive_path("config/.env.production"),
            "the fixture's premise: the shipped config treats this path as sensitive"
        );
        let dir = search_repo(&[
            ("config/.env.production", "STRIPE_KEY=live\n"),
            ("config/settings.rs", "const A: u8 = 1;\n"),
        ]);
        let req = resolve_search_in(dir.path(), "config", true).expect("a resolved search");
        let search = req.search.as_ref().expect("the resolver attached one");
        let rift_recall_serve::IndexState::Answered { hits, lines, .. } = &search.index else {
            panic!("a walkable checkout is not pending");
        };
        assert!(
            hits.iter().all(|hit| hit.path != "config/.env.production"),
            "{hits:?}"
        );
        assert!(lines.iter().all(|line| line.text != "STRIPE_KEY=live"));
    }

    /// The two spellings are ONE verb, and this is where that is checkable.
    ///
    /// `rift search <query>` is an alias, not a second command: it parses into
    /// the same arguments the long spelling parses into and routes through the
    /// same function, so a change to one cannot leave the other behind. The
    /// census records it `false` — guidance names the taught spelling — and
    /// this is the assertion that the untaught spelling still answers the same
    /// thing, `--content` included.
    #[test]
    fn the_search_alias_and_the_long_spelling_are_one_verb() {
        for argv in [
            vec!["search", "read gate"],
            vec!["search", "read gate", "--content"],
        ] {
            let long = parse(&argv);
            let mut short_argv = vec!["rift"];
            short_argv.extend_from_slice(&argv);
            let short = match crate::cli::Cli::try_parse_from(short_argv)
                .expect("the alias parses")
                .cmd
            {
                Some(crate::cli::Command::Search(a)) => crate::cli::RecallCmd::Search(a),
                other => panic!("expected `search`, got {other:?}"),
            };
            let here = Path::new("/repo");
            let (verb, params) = verb_and_params(&long, here).expect("a skill verb");
            assert_eq!(verb, ToolName::Search.verb_name());
            assert_eq!(
                verb_and_params(&short, here),
                Some((verb, params)),
                "the alias must reach the same verb with the same params"
            );
            assert_eq!(
                search_content(&short),
                search_content(&long),
                "and carry the same content decision"
            );
        }
    }

    /// The verb takes no `--cross-branch`, and that is a routing fact rather
    /// than a missing flag: its answer is this checkout's own file list, which
    /// no branch axis contains.
    #[test]
    fn the_search_verb_has_no_scope_to_widen() {
        let cmd = parse(&["search", "read gate"]);
        let grant = cross_branch_grant(&cmd).unwrap_or_else(|_| panic!("no flag, no refusal"));
        assert!(!grant.branches());
        assert!(!rift_recall_serve::ToolName::Search.is_scope_bound());
        assert!(!search_content(&cmd));
        assert!(search_content(&parse(&[
            "search",
            "read gate",
            "--content"
        ])));
    }
}
