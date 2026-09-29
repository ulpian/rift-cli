//! The document gate: a read of a binary document is redirected to its
//! converted Markdown sidecar, with the rewrite proved to differ from the
//! agent's call in exactly one field. Separate because it is the only gate
//! step that can rewrite a tool call, and the whole of it is behind the `doc`
//! feature.

use std::path::Path;

use rift_shadow::hooks::AdapterRegistry;

#[cfg(feature = "doc")]
use super::gate::{gate_rel_path, gate_step, payload_cwd, GateStep, AUDIT_IDS};
#[cfg(feature = "doc")]
use super::git::effective_config;
#[cfg(feature = "doc")]
use super::worker::owe_document_sweep;

/// The root field a `PreToolUse`/`PostToolUse` payload carries a tool's own
/// arguments in.
///
/// Read here rather than through the adapter because the amendment this gate
/// makes is *of that object*: `rift_agent::respond::Rewrite` carries the call
/// as the agent wrote it precisely so the one changed field can be proved to be
/// the only difference, and a rewrite built from a reconstruction of the call
/// would delete whatever the reconstruction forgot. Pinned as the envelope
/// field beside `tool_name` by every alpha family's hook-surface artifact, and
/// already the field `rift_agent::default_facts` reads the read path out of.
#[cfg(feature = "doc")]
const TOOL_INPUT_FIELD: &str = "tool_input";

/// The neutral tag on a decline this repository reached and did not make.
///
/// A lever in `observe` is a lever that decides everything and acts on
/// nothing, so the row it books is the size of the pool rather than a claim on
/// any of it: the frame is empty because nothing was put in front of a model,
/// and the tag says which of the two silences this was.
#[cfg(feature = "doc")]
const TAG_REDIRECT_OBSERVED: &str = "redirect_observed";

/// The neutral tag on a decline that named the sidecar rather than amending
/// the call.
///
/// It books no avoided figure, and the tag is which zero it is. A substitution
/// is checked against the input the tool really ran with, so a rewritten read
/// can be said not to have happened; a decline cannot — the frame asks the
/// model to go elsewhere and nothing here observes whether it did, and one
/// decline per key means the repeat of the same call runs. The document's
/// declared cost is claimed on the arm that can be verified and on no other.
#[cfg(feature = "doc")]
const TAG_REDIRECT_UNVERIFIED: &str = "redirect_unverified";

/// The neutral tag on a substitution that was made and did not happen.
///
/// The client applies the last amendment it is handed for one event, so a
/// sibling hook that amends the same call wins and Rift's path never reaches
/// the tool. Rift does not fight over the channel: it records the fact and
/// stops using it for that window.
#[cfg(feature = "doc")]
const TAG_REWRITE_DROPPED: &str = "rewrite_dropped";

/// What the document gate concluded about one tool call.
#[cfg(feature = "doc")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum DocAnswer {
    /// Read this instead. Carries the call as the agent wrote it, because the
    /// amendment replaces the whole input object and the one changed field has
    /// to be provable against the original.
    Rewrite {
        /// The call as the agent wrote it.
        tool_input: serde_json::Value,
        /// This member's view of the sidecar, absolutely named.
        sidecar: String,
    },
    /// Decline this one read, with the rendered sentence naming the sidecar.
    Redirect {
        /// The frame, written by the one renderer allowed to write one.
        reason: String,
    },
}

/// The same conclusion in a build without the document seam.
///
/// The gate above is a stub there, so there is no answer for it to reach and
/// no variant for one to be written as. Declared rather than removed so that
/// the plumbing that carries an answer to the response — and the one place
/// that composes that response — is one shape in both builds instead of two.
#[cfg(not(feature = "doc"))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum DocAnswer {}

/// What one hook's document gate concluded: the answer the agent is owed, and
/// whether the detached worker was left something to write.
#[derive(Debug, Default)]
pub(super) struct DocOutcome {
    /// An accounting row or a conversion was journaled, so the worker is
    /// spawned for it too.
    pub(super) journaled: bool,
    /// The answer, for the overwhelming majority of calls `None`.
    pub(super) answer: Option<DocAnswer>,
}

/// The hook a document decision is made for, and the repository's policy as
/// it stands for this call: what every step past the gate's own preconditions
/// reads, and everything the journal a decision owes is written under.
#[cfg(feature = "doc")]
struct DocCtx<'a> {
    /// Where the detached worker's jobs are journaled.
    git_dir: &'a Path,
    /// The checkout the call was made in.
    work_tree: &'a Path,
    /// The hook itself: its agent, session, clock and payload.
    raw: &'a rift_shadow::hooks::RawHook,
    /// The repository's serving policy, which scopes what may be converted.
    policy: &'a rift_recall_policy::ServePolicy,
    /// The two levers and the channel, resolved for this family.
    effective: &'a rift_recall_policy::Effective,
}

/// **A read of a document is answered with the Markdown Rift already holds.**
///
/// Runs before [`read_gate`](super::gate::read_gate) and under the same rules: it opens no store and
/// initialises no tokenizer, it reads files and nothing else, and every way of
/// failing to answer leaves the agent's call exactly as it was written. What
/// it may do is hash the document, ask the family store whether that answer
/// exists, link it into this member — and, on a miss, spend the policy's
/// conversion budget on a child process and *nothing more*: past the
/// deadline the read proceeds untouched and the conversion is journaled for
/// the worker: a hook has an agent waiting on it, and the honest answer to
/// "not ready yet" is the read the agent already wrote.
///
/// The family store is the reason a miss is rare: a session's first boundary
/// hook asks for a pass over the repository's tracked documents, and one
/// member's pass is every member's.
#[cfg(feature = "doc")]
pub(super) fn doc_gate(
    t: &crate::hooktime::HookTimer,
    registry: &AdapterRegistry,
    raw: &rift_shadow::hooks::RawHook,
    git_dir: &Path,
    work_tree: &Path,
    policy: &rift_recall_policy::ServePolicy,
    effective: &rift_recall_policy::Effective,
) -> DocOutcome {
    let quiet = DocOutcome::default();
    // Two levers, and this whole path is worth nothing when neither is held:
    // a repository with `[doc] mode = "off"`, or a family whose pinned rows
    // document neither an amendment nor a confirmed reason, never gets here.
    if effective.doc_rewrite == rift_recall_policy::Mode::Off
        && effective.doc_redirect == rift_recall_policy::Mode::Off
    {
        return quiet;
    }
    // The family store is addressed by the family's own common directory, not
    // by this checkout: two members of one repository read one another's
    // answers, and a temporary subagent worktree pays for none of them.
    let Some(family) = crate::family::try_at(work_tree) else {
        return quiet;
    };
    let store = crate::doc_store::DocStore::for_member(&family.common_dir, work_tree);
    // Named here and handed down, so the seam a test drives is the seam
    // production runs: the re-spawned child is the only converter this binary
    // has, and it is reached under the *gate's* budget rather than the worker's.
    let converter = gate_document_converter(effective);
    doc_gate_in(
        t,
        registry,
        raw,
        git_dir,
        work_tree,
        policy,
        effective,
        &store,
        converter
            .as_ref()
            .map(|found| found as &dyn rift_recall_doc::DocConverter),
    )
}

/// The same gate in a build without the document seam: a package built
/// without the parsers converts nothing and substitutes nothing.
#[cfg(not(feature = "doc"))]
pub(super) fn doc_gate(
    _t: &crate::hooktime::HookTimer,
    _registry: &AdapterRegistry,
    _raw: &rift_shadow::hooks::RawHook,
    _git_dir: &Path,
    _work_tree: &Path,
    _policy: &rift_recall_policy::ServePolicy,
    _effective: &rift_recall_policy::Effective,
) -> DocOutcome {
    DocOutcome::default()
}

/// The gate against one store — the seam a test drives, so the claim under
/// test is "this document is answered with that sidecar" rather than "this
/// machine has a git family".
#[cfg(feature = "doc")]
#[allow(clippy::too_many_arguments)]
pub(super) fn doc_gate_in(
    t: &crate::hooktime::HookTimer,
    registry: &AdapterRegistry,
    raw: &rift_shadow::hooks::RawHook,
    git_dir: &Path,
    work_tree: &Path,
    policy: &rift_recall_policy::ServePolicy,
    effective: &rift_recall_policy::Effective,
    store: &crate::doc_store::DocStore,
    converter: Option<&dyn rift_recall_doc::DocConverter>,
) -> DocOutcome {
    let quiet = DocOutcome::default();
    // A window is what is told and what is amended, and a hook that names no
    // session names no window.
    if raw.session.is_empty() {
        return quiet;
    }
    let Some(adapter) = registry.get(&raw.agent) else {
        return quiet;
    };
    // The context window, not the session: a subagent's hooks arrive under its
    // parent's session id and carry their own agent id, and one of them having
    // been told where the Markdown is says nothing about the other.
    let facts = adapter.facts(&raw.verb, &raw.payload);
    let window =
        crate::doc_store::DocWindow::at(work_tree, &raw.session, facts.child_session.as_deref());
    let Some(step) = gate_step(&raw.agent, &raw.verb, &raw.payload) else {
        return quiet;
    };
    let ctx = DocCtx {
        git_dir,
        work_tree,
        raw,
        policy,
        effective,
    };
    match step {
        GateStep::Decide => {}
        // A read's own result, which is where a substitution is verified —
        // and it is asked before the call is read as a document read, because
        // the path a verified result carries is the sidecar, and a sidecar is
        // Markdown rather than a document.
        GateStep::CrossCheck => {
            return document_verify(&ctx, &facts, &window);
        }
        GateStep::Evict(_) => return quiet,
    }
    let Some(call) = document_call(&raw.agent, &raw.payload, &facts, work_tree) else {
        return quiet;
    };
    answer_document_read(&ctx, t, &call, &window, store, converter)
}

/// Answer one read that is a document read: refuse what the repository never
/// serves, key the document's bytes, find or make its sidecar under the gate's
/// budget, and choose the channel the answer goes out on.
///
/// Every check keeps its place in that order, because an earlier refusal must
/// win: nothing is hashed for a path the repository declined, and nothing is
/// converted for a document the family already decided against.
#[cfg(feature = "doc")]
fn answer_document_read(
    ctx: &DocCtx<'_>,
    t: &crate::hooktime::HookTimer,
    call: &DocumentCall,
    window: &crate::doc_store::DocWindow,
    store: &crate::doc_store::DocStore,
    converter: Option<&dyn rift_recall_doc::DocConverter>,
) -> DocOutcome {
    let quiet = DocOutcome::default();
    // The repository's own two refusals, made before a byte is read: a path it
    // declared sensitive, and a path its serving policy never serves. A
    // sidecar is durable text derived from the file, so a file that must not
    // be served must not be re-encoded beside the checkout either.
    if !crate::doc_store::is_convertible_path(
        &effective_config(ctx.work_tree),
        ctx.policy,
        &call.rel_path,
    ) {
        return quiet;
    }
    let (key, sha) = match document_key(&call.absolute, ctx.policy, ctx.effective) {
        Ok(keyed) => keyed,
        // Too large to hash while the agent waits. The answer is still worth
        // having, so the bounded pass is asked for — it runs where nobody is
        // waiting, under a budget that is about killing a wedged parser rather
        // than about latency — and this read runs exactly as it was written.
        Err(OVER_HASH_CEILING) => {
            return DocOutcome {
                journaled: owe_document_sweep(ctx.git_dir, ctx.work_tree),
                answer: None,
            };
        }
        Err(_) => return quiet,
    };
    // Decided against once, for the whole family: a PDF that is page images
    // rather than text, an encrypted or malformed container. Nothing here
    // tries again, and the read runs as the agent wrote it.
    if store.is_skipped(&key) {
        return quiet;
    }
    let sidecar = match document_sidecar(t, store, converter, call, &key, &sha, ctx.policy) {
        Ok(path) => path,
        // A fault of the document itself, remembered for the whole family by
        // the conversion that found it: nothing is owed, because nothing will
        // ever convert it.
        Err("") => return quiet,
        Err(tag) => return defer_document_conversion(ctx, &call.rel_path, &sha, tag),
    };
    let Some(named) = sidecar.to_str() else {
        return quiet;
    };
    document_answer(ctx, call, &sha, &key, named, window)
}

/// Leave the conversion to the detached worker, and book the deferral.
///
/// Not now, and nothing about the document says never — a deadline, a helper
/// this installation does not have, a link that would not be made. The read
/// runs as it was written and the conversion becomes the detached worker's,
/// which is the whole of what a deadline on an agent's critical path may mean.
#[cfg(feature = "doc")]
fn defer_document_conversion(ctx: &DocCtx<'_>, rel_path: &str, sha: &str, tag: &str) -> DocOutcome {
    let owed = journal_document_conversion(ctx, rel_path, sha);
    let booked = journal_document_decision(
        ctx,
        crate::agent_queue::ServeAuditKind::DocDeferred,
        rel_path,
        sha,
        "",
        tag,
        Some(0),
    );
    DocOutcome {
        journaled: owed || booked,
        answer: None,
    }
}

/// The read of a document this call is, or `None` when it is not one.
///
/// The family's own pinned tool-field row is the scoping: a tool with no row
/// is a tool this gate does not understand, and a row's shape decides which of
/// the two ways of asking for a whole file this is — a structured read tool,
/// or a command line the published grammar spells. Every refusal here leaves
/// the call untouched, and each closes a way an answer could be about
/// something other than what the agent asked for:
///
/// * a call carrying an argument that addresses **part** of the file is not a
///   whole-document read — `pages` addresses pages of a PDF, `offset`/`limit`
///   and `StartLine`/`EndLine` address lines, and none of them survives a
///   change of the bytes they address, which is also the pinned tool-field
///   artifact's own reading of `pages`. The only arguments read past are the
///   ones the row itself names as the client's envelope, which arrive on every
///   call and say nothing about a range;
/// * a command line is a document read only as plain `cat` on one path: the
///   grammar's other forms decorate the output or deliver part of it, and this
///   is the first decline made on a shell channel, so every form beyond the
///   one the evidence covers is a saving forgone rather than a risk taken. A
///   pipeline, a second argument or a character the shell would act on never
///   reaches the grammar at all;
/// * a path this checkout has no name for belongs to no checkout this gate
///   describes;
/// * an extension the converter does not know is not a document.
///
/// The shell arm is reached only on a family whose pinned rows document no
/// amendment of a tool call. Rift takes **one** document channel per family:
/// where a call can be amended, the channel is the amendment on the family's
/// read tool, and a second channel on the same family would decline calls its
/// own channel already answers in silence.
#[cfg(feature = "doc")]
fn document_call(
    agent: &rift_core::AgentId,
    payload: &serde_json::Value,
    facts: &rift_agent::HookFacts,
    work_tree: &Path,
) -> Option<DocumentCall> {
    use rift_agent::tool_fields::ToolShape;
    let row = rift_agent::tool_fields::tool_field_row(agent, facts.tool_name.as_deref()?)?;
    let args = document_args(row, payload)?;
    let amends = crate::recall::family_capability(agent).document_sidecar_rewrite;
    let (named, amendable) = match row.shape {
        ToolShape::Read => (
            whole_document_path(row, args)?,
            // The call as the agent wrote it, kept whole because the amendment
            // replaces the object and the one changed field has to be provable
            // against the original — and kept only where the family's pinned
            // rows say the amendment exists at all.
            amends.then(|| serde_json::Value::Object(args.clone())),
        ),
        ToolShape::Shell if !amends => (shell_document_path(row, args)?, None),
        ToolShape::Shell | ToolShape::Edit | ToolShape::Search | ToolShape::None => return None,
    };
    let root = work_tree.canonicalize().ok()?;
    let rel_path = gate_rel_path(&root, &named, payload_cwd(facts).as_deref())?;
    let absolute = root.join(&rel_path);
    let extension = absolute.extension().and_then(|e| e.to_str())?;
    if !rift_recall_doc::is_supported_extension(extension) {
        return None;
    }
    Some(DocumentCall {
        rel_path,
        absolute,
        amendable,
    })
}

/// The arguments object this family's pinned row says a call carries them in.
///
/// Read off the row rather than known here: two families put them flat under
/// the envelope field beside `tool_name`, one nests them, and the row is where
/// that fact is already pinned to an artifact. A row that names no nesting is
/// the flat spelling.
#[cfg(feature = "doc")]
fn document_args(
    row: rift_agent::tool_fields::ToolFieldRow,
    payload: &serde_json::Value,
) -> Option<&serde_json::Map<String, serde_json::Value>> {
    let mut node = payload;
    for segment in row.fact("args").unwrap_or(TOOL_INPUT_FIELD).split('.') {
        node = node.get(segment)?;
    }
    node.as_object()
}

/// The path a structured read tool names, when the call is for the **whole**
/// file and nothing narrower.
#[cfg(feature = "doc")]
fn whole_document_path(
    row: rift_agent::tool_fields::ToolFieldRow,
    args: &serde_json::Map<String, serde_json::Value>,
) -> Option<String> {
    let path = row.fact("path")?;
    let envelope = || row.fact("envelope").into_iter().flat_map(|f| f.split(','));
    if args
        .keys()
        .any(|key| key != path && !envelope().any(|named| named == key))
    {
        return None;
    }
    args.get(path)?.as_str().map(str::to_owned)
}

/// The path a command line read in full, when it is one of those.
///
/// The command is the only argument the call may carry: anything beside it is
/// something unpinned that could change which file the line reads, and a
/// decline naming the sidecar of a *different* document is worse than no
/// decline at all.
///
/// One read, and the whole of one file. A command line that reads several —
/// a chain, or a `cat` over several paths — has no single document to send
/// the model to, and this gate names a sidecar or says nothing.
#[cfg(feature = "doc")]
fn shell_document_path(
    row: rift_agent::tool_fields::ToolFieldRow,
    args: &serde_json::Map<String, serde_json::Value>,
) -> Option<String> {
    use rift_agent::shell_read::{ShellCall, ShellSpan};
    if args.len() != 1 {
        return None;
    }
    let command = args.get(row.fact("command")?)?.as_str()?;
    match rift_agent::shell_read::recognise(command)? {
        ShellCall::Read(reads) => match reads.as_slice() {
            [read] if read.span == ShellSpan::WholeFile && read.narrowing.is_none() => {
                Some(read.path.clone())
            }
            _ => None,
        },
        ShellCall::Listing(_) => None,
    }
}

/// One document read, resolved against this checkout.
#[cfg(feature = "doc")]
struct DocumentCall {
    /// This checkout's own spelling of the path.
    rel_path: String,
    /// The same file, absolutely.
    absolute: std::path::PathBuf,
    /// The call as the agent wrote it, on a family that documents the
    /// amendment of one — and `None` where the only channel is the decline, so
    /// that no path through this gate can amend a call no client would apply.
    amendable: Option<serde_json::Value>,
}

/// Why this document has no key on this path.
#[cfg(feature = "doc")]
const OVER_HASH_CEILING: &str = "over_hash_ceiling";

/// The sidecar key for this document's current bytes, and their SHA-256.
///
/// The size is read before the bytes are, against **two** ceilings, and they
/// are different promises. `max_input_bytes` is what the converter will parse
/// at all. `max_inline_hash_bytes` is what a hook may read and hash while an
/// agent waits — the same ceiling the read gate applies for the same reason,
/// and the reason is measured rather than assumed: hashing sixteen mebibytes
/// costs about a tenth of a second in an optimized build and roughly twenty
/// times that in the unoptimized one a developer's own hooks run, against a
/// hundred-millisecond budget, while a mebibyte costs single-figure
/// milliseconds either way.
///
/// So a document larger than the inline ceiling is not keyed here at all: the
/// caller asks the detached worker for a pass instead, and this read runs
/// exactly as it was written. The hash is of the file's bytes and of nothing
/// else, which is what makes the key the identity of the *answer* — the same
/// key the worker and the proactive pass compute.
#[cfg(feature = "doc")]
fn document_key(
    absolute: &Path,
    policy: &rift_recall_policy::ServePolicy,
    effective: &rift_recall_policy::Effective,
) -> std::result::Result<(String, String), &'static str> {
    let meta = std::fs::metadata(absolute).map_err(|_| "unreadable")?;
    if !meta.is_file() || meta.len() > effective.max_input_bytes {
        return Err("uncountable");
    }
    if meta.len() > effective.max_inline_hash_bytes {
        return Err(OVER_HASH_CEILING);
    }
    let bytes = std::fs::read(absolute).map_err(|_| "unreadable")?;
    let sha = rift_shadow::sha256_hex(&[&bytes]);
    let key = crate::doc_store::sidecar_key(&sha, &policy.doc).ok_or("unreadable")?;
    Ok((key, sha))
}

/// This member's view of the sidecar, or the tag naming why there is none.
///
/// Three outcomes and one budget. The family already holds the answer, in
/// which case this is a `stat` and a hardlink; or the helper produces it
/// inside the policy's conversion budget; or the budget runs out — and past it
/// the read proceeds untouched and the work becomes the worker's, which is the
/// whole of what a deadline on an agent's critical path may mean.
#[cfg(feature = "doc")]
fn document_sidecar(
    t: &crate::hooktime::HookTimer,
    store: &crate::doc_store::DocStore,
    converter: Option<&dyn rift_recall_doc::DocConverter>,
    call: &DocumentCall,
    key: &str,
    sha: &str,
    policy: &rift_recall_policy::ServePolicy,
) -> std::result::Result<std::path::PathBuf, &'static str> {
    if store.holds(key) {
        return store.link(key).map_err(|_| "link");
    }
    let Some(converter) = converter else {
        return Err("no_converter");
    };
    match t.phase("doc_convert", || {
        crate::doc_store::convert_into_store(
            store,
            converter,
            &call.absolute,
            &policy.doc,
            Some(sha),
        )
    }) {
        crate::doc_store::Conversion::Linked(path)
        | crate::doc_store::Conversion::Converted(path) => Ok(path),
        // A fault of the document itself, remembered for the family: nothing
        // is owed and nothing will be retried, so this is not a deferral.
        crate::doc_store::Conversion::Refused(_) => Err(""),
        crate::doc_store::Conversion::Deferred(tag) => Err(tag),
    }
}

/// The re-spawned converter under the **gate's** budget.
///
/// The policy's `convert_budget_ms` and not the worker's thirty seconds: this
/// one runs while the agent waits, and the deadline is enforced by killing the
/// child rather than by asking it to hurry.
#[cfg(feature = "doc")]
fn gate_document_converter(
    effective: &rift_recall_policy::Effective,
) -> Option<rift_recall_doc::ChildProcessConverter> {
    crate::doc_helper::self_exec_converter(std::time::Duration::from_millis(u64::from(
        effective.convert_budget_ms,
    )))
}

/// Which channel this document read is answered on, and the accounting the
/// answer owes.
///
/// Two channels. The **substitution** amends the path and says nothing to the
/// model, so it acts wherever the family's pinned rows document the amendment
/// and the repository has not asked to be told instead (O14: it declines
/// nothing, adds nothing to the context and asks the model for nothing). The
/// **decline** names the sidecar in a sentence the model reads, and it is what
/// answers every call the amendment cannot: a family whose calls no client
/// will amend, a repository that chose `channel = "redirect"`, and a window
/// whose substitution was verified to have been dropped. It acts only on a
/// reason channel a recorded run has confirmed — which is a fact about the
/// family, already resolved into the lever this reads — and only once per
/// sidecar, so the sentence's own escape hatch is true.
#[cfg(feature = "doc")]
fn document_answer(
    ctx: &DocCtx<'_>,
    call: &DocumentCall,
    sha: &str,
    key: &str,
    sidecar: &str,
    window: &crate::doc_store::DocWindow,
) -> DocOutcome {
    use rift_recall_policy::{DocChannel, Mode};
    let ran = DocOutcome::default();
    // A call no client would amend, a repository that asked to be told rather
    // than amended, or a window whose substitution was verified to have been
    // dropped: each of the three stands the amendment down and leaves the
    // decline as this window's channel.
    let redirecting = call.amendable.is_none()
        || ctx.effective.doc_channel == DocChannel::Redirect
        || window.is_redirecting();
    if !redirecting {
        return document_rewrite(ctx.effective, call, sha, key, sidecar, window);
    }
    if ctx.effective.doc_redirect == Mode::Off {
        // No lever is being held: a repository that shut the table decides
        // nothing here and records nothing either.
        return ran;
    }
    let Some(reason) = redirect_frame(ctx.work_tree, &call.rel_path, sidecar) else {
        return ran;
    };
    // One decline per sidecar, which is what makes the frame's own escape
    // hatch true: the repeated call succeeds. Taken in both modes, so that
    // what `observe` records is the decision `enforce` would have made and
    // not one row per read of the same document.
    if !window.redirect_once(key) {
        return ran;
    }
    book_redirect(ctx, call, sha, reason)
}

/// The substitution: amend the call's path to the sidecar, when the lever is
/// enforced and the call is one a client would amend.
///
/// Remembered before the answer is given, so the result of this very call can
/// be checked against it — and **nothing is booked here**. An amendment is a
/// request to the client, not an outcome: the client applies the last one it
/// is handed for an event, so a sibling hook amending the same call wins and
/// the agent reads the document in full. A row written now would claim a
/// saving that the result may contradict, in a relation nothing rewrites; the
/// claim is made where the executed call proves it, and a turn that never
/// comes back books nothing at all.
#[cfg(feature = "doc")]
fn document_rewrite(
    effective: &rift_recall_policy::Effective,
    call: &DocumentCall,
    sha: &str,
    key: &str,
    sidecar: &str,
    window: &crate::doc_store::DocWindow,
) -> DocOutcome {
    let (Some(tool_input), rift_recall_policy::Mode::Enforce) =
        (call.amendable.as_ref(), effective.doc_rewrite)
    else {
        return DocOutcome::default();
    };
    window.arm_rewrite(&call.rel_path, key, sha);
    DocOutcome {
        journaled: false,
        answer: Some(DocAnswer::Rewrite {
            tool_input: tool_input.clone(),
            sidecar: sidecar.to_owned(),
        }),
    }
}

/// The decline's sentence, naming the sidecar, or `None` when no sentence can.
///
/// Both paths in the sentence are named the way this checkout names them,
/// which is also the way every other frame names a file: a span inside a
/// pushed frame is bounded, and an absolute path to a 64-character sidecar
/// name is long enough to reach that bound in an ordinary checkout.
///
/// The sentence is written by the one renderer allowed to write one; this
/// function supplies the two paths. A frame that cannot name the sidecar —
/// withheld, or cut short by the span bound — is not a frame at all, because
/// where to read instead is the whole of what it says; and a decline with no
/// frame is an unexplained tool failure, so the read runs instead.
#[cfg(feature = "doc")]
fn redirect_frame(work_tree: &Path, rel_path: &str, sidecar: &str) -> Option<String> {
    let here = rift_recall_substrate::worktree::repo_relative_slash(work_tree, Path::new(sidecar))?;
    let here = here.as_str();
    let reason = rift_recall_serve::render_redirect_frame(rel_path, here)?;
    if !reason.contains(here) {
        return None;
    }
    Some(reason)
}

/// Book the decline, and give it when the lever is enforced.
#[cfg(feature = "doc")]
fn book_redirect(ctx: &DocCtx<'_>, call: &DocumentCall, sha: &str, reason: String) -> DocOutcome {
    use crate::agent_queue::ServeAuditKind;
    if ctx.effective.doc_redirect != rift_recall_policy::Mode::Enforce {
        // Reached, decided, and not acted on: a lever ships in `observe` so
        // that what it would be worth is measured before it speaks to a
        // model, and a mode that recorded nothing would leave that question
        // unanswerable. Every check above ran, so the row sizes the pool this
        // lever would work on; the frame is left out because nothing was put
        // in front of a model, and a sentence nobody read is not a cost this
        // row may book against itself.
        let booked = journal_document_decision(
            ctx,
            ServeAuditKind::DocRewritten,
            &call.rel_path,
            sha,
            "",
            TAG_REDIRECT_OBSERVED,
            Some(0),
        );
        return DocOutcome {
            journaled: booked,
            answer: None,
        };
    }
    let booked = journal_document_decision(
        ctx,
        ServeAuditKind::DocRewritten,
        &call.rel_path,
        sha,
        &reason,
        TAG_REDIRECT_UNVERIFIED,
        Some(0),
    );
    DocOutcome {
        journaled: booked,
        answer: Some(DocAnswer::Redirect { reason }),
    }
}

/// **Did the substitution this window was given actually happen?**
///
/// A read's result carries the input the tool really ran with, so the question
/// is answerable exactly rather than inferred, and both answers are decided
/// here because both are facts about the same amendment:
///
/// * the executed path is the **sidecar** Rift named. The read the agent wrote
///   never happened, and this is the one moment in the whole document channel
///   at which a saving may be claimed: the declaration is journaled now, over
///   the document the amendment stood in for, under the identity its bytes had
///   when it was made. Once — the arming is consumed, so a second result for
///   one amendment claims nothing;
/// * the executed path is still the **document**. Another hook amended the
///   same call and the client kept its amendment instead, so the agent read
///   the container in full and there is nothing to claim. Rift does not fight
///   over a channel it shares: the drop is recorded once with the zero it
///   avoided and the window is flipped, and from here on its document reads
///   are answered by the decline, which no sibling hook can overwrite.
///
/// The figure itself is never computed here — a hook has an agent waiting on
/// it and counts no tokens — which is what `None` asks the worker for.
#[cfg(feature = "doc")]
fn document_verify(
    ctx: &DocCtx<'_>,
    facts: &rift_agent::HookFacts,
    window: &crate::doc_store::DocWindow,
) -> DocOutcome {
    let DocCtx { work_tree, raw, .. } = *ctx;
    let quiet = DocOutcome::default();
    if let Some(key) = executed_sidecar_key(&raw.agent, &raw.payload, facts, work_tree) {
        // A sidecar this window never armed is an agent reading Rift's own
        // Markdown on its own account, and no read of a document was avoided
        // by it. Costs one `stat` and never a hash.
        let Some(armed) = window.take_armed_sidecar(&key) else {
            return quiet;
        };
        return DocOutcome {
            journaled: journal_document_decision(
                ctx,
                crate::agent_queue::ServeAuditKind::DocRewritten,
                &armed.rel_path,
                &armed.content_sha256,
                "",
                rift_recall_serve::TAG_DECLARED_BASELINE,
                None,
            ),
            answer: None,
        };
    }
    let Some(call) = document_call(&raw.agent, &raw.payload, facts, work_tree) else {
        return quiet;
    };
    // Nothing was substituted for this path in this window, so its result
    // contradicts nothing.
    let Some(sha) = window.take_armed(&call.rel_path) else {
        return quiet;
    };
    if !window.flip_to_redirect() {
        // Already flipped by an earlier dropped substitution: the fact is
        // recorded once, not once per read that meets it.
        return quiet;
    }
    DocOutcome {
        journaled: journal_document_decision(
            ctx,
            crate::agent_queue::ServeAuditKind::DocDeferred,
            &call.rel_path,
            &sha,
            "",
            TAG_REWRITE_DROPPED,
            Some(0),
        ),
        answer: None,
    }
}

/// The sidecar key the input a tool really ran with names, when it names one
/// of this member's views.
///
/// The same pinned row the decision was made from, read the same way: a
/// structured read tool, the family's own arguments object, the path field it
/// names, and the client's envelope fields read past. The extension check the
/// decision makes is deliberately absent — what is being recognised here is
/// Rift's own Markdown, which is exactly what a document read is not.
#[cfg(feature = "doc")]
fn executed_sidecar_key(
    agent: &rift_core::AgentId,
    payload: &serde_json::Value,
    facts: &rift_agent::HookFacts,
    work_tree: &Path,
) -> Option<String> {
    use rift_agent::tool_fields::ToolShape;
    let row = rift_agent::tool_fields::tool_field_row(agent, facts.tool_name.as_deref()?)?;
    if !matches!(row.shape, ToolShape::Read) {
        return None;
    }
    let named = whole_document_path(row, document_args(row, payload)?)?;
    let root = work_tree.canonicalize().ok()?;
    let rel_path = gate_rel_path(&root, &named, payload_cwd(facts).as_deref())?;
    crate::doc_store::member_sidecar_key(&rel_path).map(str::to_owned)
}

/// Journal the conversion a missed deadline leaves owed, and say whether the
/// worker was left anything.
///
/// Best effort, like every other journal on this path: a job that cannot be
/// written costs a conversion the proactive pass will make anyway, and never
/// the agent's call.
#[cfg(feature = "doc")]
fn journal_document_conversion(ctx: &DocCtx<'_>, rel_path: &str, sha: &str) -> bool {
    crate::agent_queue::enqueue(
        ctx.git_dir,
        &crate::agent_queue::AgentJob {
            schema_version: 1,
            work_tree: ctx.work_tree.to_path_buf(),
            queued_at_ms: ctx.raw.occurred_at_ms,
            attempts: 0,
            kind: crate::agent_queue::JobKind::DocConvert {
                rel_path: rel_path.to_owned(),
                content_sha256: sha.to_owned(),
            },
        },
    )
    .is_ok()
}

/// Journal the accounting one document decision owes.
///
/// The avoided figure is **declared**, never measured: what a document read
/// would have cost the context is a property of the client's own rendering of
/// zip or PDF bytes, and there is no second reading of it to count. `declared`
/// is the figure this decision claims — `Some(0)` for an arm that claims
/// nothing, and `None` for the one arm that does, which is the substitution a
/// result has just been checked against and no other. It leaves the
/// declaration to the worker because the formula counts tokens and this is an
/// agent's critical path. `blob_oid` is empty because the declaration is over the
/// working file's own bytes under the hash recorded here, and a document that
/// no longer hashes to it is one nothing may be declared about.
#[cfg(feature = "doc")]
fn journal_document_decision(
    ctx: &DocCtx<'_>,
    decision: crate::agent_queue::ServeAuditKind,
    rel_path: &str,
    sha: &str,
    frame: &str,
    tag: &str,
    declared: Option<u32>,
) -> bool {
    if tag.is_empty() {
        return false;
    }
    crate::agent_queue::enqueue(
        ctx.git_dir,
        &crate::agent_queue::AgentJob {
            schema_version: 1,
            work_tree: ctx.work_tree.to_path_buf(),
            queued_at_ms: ctx.raw.occurred_at_ms,
            attempts: 0,
            kind: crate::agent_queue::JobKind::ServeAudit(crate::agent_queue::ServeAudit {
                call_id: {
                    use rift_core::UlidSource as _;
                    AUDIT_IDS.next_ulid().to_hex()
                },
                decision,
                agent: ctx.raw.agent.as_str().to_owned(),
                basis: Some(crate::agent_queue::AuditBasis {
                    rel_path: rel_path.to_owned(),
                    content_sha256: sha.to_owned(),
                    blob_oid: String::new(),
                    lines: None,
                }),
                frame: frame.to_owned(),
                tag: Some(tag.to_owned()),
                total: None,
                declared_avoided: declared,
                // Not a name search: nothing here stood in for a listing.
                shape: None,
                session: Some(ctx.raw.session.clone()),
            }),
        },
    )
    .is_ok()
}
