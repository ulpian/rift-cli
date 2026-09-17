//! **A content search that declared no result cap is given one, once.**
//!
//! The whole of what Rift does to a content search. It is not answered, not
//! substituted and not declined — a search inside file contents has an answer
//! no index of Rift's can stand in for, and a decline would send the model
//! looking for one that does not exist. What a search that named no bound can
//! be given is the bound its own tool documents: the family's pinned rows
//! carry an argument for it, the tool states its true total even when that
//! bound truncates the listing, and the model reads that total and asks again
//! when it wants the rest. Rift writes nothing into the result.
//!
//! Four rules hold the lever honest, and each is a fact this module keeps:
//!
//! 1. **A cap is only ever added.** A call that named its own bound — an
//!    explicit `0` included — chose one, and a choice already made is not one
//!    to make again.
//! 2. **One cap per key per context window.** The key is the search itself
//!    with the two parameters that page through its results removed, so the
//!    follow-up the tool's own total invites — the same search, with or
//!    without an offset — runs with no bound at all. That is the escape hatch,
//!    and it is why the cap costs the agent nothing it cannot take back.
//! 3. **Verified where the result proves it.** The executed call carries the
//!    input the tool really ran with, so a cap another hook's amendment
//!    displaced is a fact rather than an inference. A displaced cap is
//!    recorded and this window stops amending; it is never escalated to a
//!    decline, because a content search is never substituted.
//! 4. **Nothing here opens a store or counts a token.** The window's memory is
//!    marker files under this member's own run state, and the accounting row is
//!    journaled for the detached worker.
//!
//! **A name search is answered from the index Rift already holds.** The other
//! half of this module, and the mirror image of the rule above: a search for
//! file *names* — a glob, or a `find … -name` listing under the published
//! grammar — has an answer that fits in a sentence, so it gets one. The
//! ranked head of the matches, the total the head is a head of, and the fact
//! that repeating the call runs it in full. Four rules again:
//!
//! 1. **From the index or not at all.** The index is a file this member keeps
//!    and the detached worker writes. Warm, it is loaded and matched inside
//!    the hook's budget; cold or stale, the search runs exactly as written and
//!    the walk is asked of the worker. A hook never walks a tree.
//! 2. **One answer per name search per context window**, on a family whose
//!    reason channel a recorded run has confirmed. The repeat is allowed —
//!    that is what makes the frame's own escape hatch true — and a repeat
//!    inside the turn that was answered is counted, because a lever that sends
//!    the model straight back for the full list has saved nothing.
//! 3. **Names, never contents, and never a total nobody counted.** The frame
//!    carries paths and a count. A content search is never answered this way
//!    and never substituted by a name search.
//! 4. **Nothing here opens a store or counts a token either.** The frame is
//!    fitted to the operator's budget by characters, and the tokens it cost —
//!    and the listing it stood in for — are counted by the worker.
//!
//! The re-query rate — the honesty figure this lever is judged by — is defined
//! in exactly one place: this window directory. `c-<key>` is a search this
//! window capped, `q-<key>` is one that was asked again *in the same turn*
//! after being capped, and the rate is the second count over the first. A
//! reader outside this binary counts the files; nothing recomputes the
//! definition from a different set of lines.

use std::path::{Path, PathBuf};

use rift_recall_policy::Mode;
use rift_shadow::hooks::{AdapterRegistry, RawHook};

/// The amendment one call is owed: the call as the agent wrote it, and the cap
/// to add to it.
///
/// The whole input travels because the amendment replaces the object rather
/// than patching it, and carrying the original is what lets the one changed
/// field be proved to be the only difference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GrepCap {
    /// The call as the agent wrote it.
    pub(crate) tool_input: serde_json::Value,
    /// The bound to add.
    pub(crate) head_limit: u32,
}

/// What one hook's search gate concluded.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct SearchOutcome {
    /// An accounting row was journaled, so the detached worker is owed a run.
    pub(crate) journaled: bool,
    /// What the call is owed, `None` for every call this gate leaves alone.
    pub(crate) answer: Option<SearchAnswer>,
}

/// The two things this gate may hand back, which are deliberately different
/// kinds of thing.
///
/// A content search is **amended**: it runs, and Rift adds the bound the
/// caller left out. A name search is **answered**: it does not run, and the
/// model reads the answer instead. Nothing in this module ever does the first
/// to a name search or the second to a content one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SearchAnswer {
    /// The bound to add to a content search that declared none.
    Cap(GrepCap),
    /// The rendered answer a name search is declined with. A build without the
    /// search seam has no index to answer one from, so nothing constructs it
    /// there and the response arm below simply never matches.
    #[cfg_attr(not(feature = "search"), allow(dead_code))]
    Listing(String),
}

/// The pinned fact that says a row is the **content** search.
///
/// Read off the family's own tool-field row rather than matched by tool name:
/// what a family calls its content search is that family's to say, and the
/// row is where it is already pinned to an artifact.
const GREP_KIND: &str = "grep";

/// The second of the two parameters that page through a search's results.
///
/// The first is the row's own `head_limit`. Both are named verbatim by the
/// pinned tool-field artifact, in the sentence that says what the tool's total
/// still covers: "The total covers every match even when the tool's
/// `head_limit` or `offset` parameters truncate the listed per-file entries."
/// They are the two arguments a caller changes to walk through one search's
/// results, so neither belongs in the identity of the search itself — a
/// follow-up that pages is the same search asked again, and the rule that lets
/// it run uncapped depends on exactly that.
const PAGING_FIELD: &str = "offset";

/// The field a family's call carries its arguments in, when its row names no
/// other.
const ARGS_FIELD: &str = "tool_input";

/// **The gate.** Runs beside the read and document gates and under the same
/// rules: it opens no store, initialises no tokenizer, reads and writes files
/// under this member's own run state, and every way of failing to decide
/// leaves the agent's call exactly as it was written.
#[allow(clippy::too_many_arguments)]
pub(crate) fn search_gate(
    t: &crate::hooktime::HookTimer,
    registry: &AdapterRegistry,
    raw: &RawHook,
    git_dir: &Path,
    work_tree: &Path,
    policy: &rift_recall_policy::ServePolicy,
    effective: &rift_recall_policy::Effective,
) -> SearchOutcome {
    let quiet = SearchOutcome::default();
    // Two shut tables and a hook that names no context window: three ways of
    // having nothing to decide, and none of them costs a file read.
    if raw.session.is_empty()
        || (effective.grep_cap == Mode::Off && effective.search_answer == Mode::Off)
    {
        return quiet;
    }
    let Some(adapter) = registry.get(&raw.agent) else {
        return quiet;
    };
    let facts = adapter.facts(&raw.verb, &raw.payload);
    // A NAME search is decided before a content search is amended, and the two
    // arms never see the same call: what a family calls each of them is that
    // family's own pinned fact, read here through the adapter that holds it.
    if let Some(outcome) = t.phase("search_answer", || {
        name_answer(
            t, registry, raw, git_dir, work_tree, policy, effective, &facts,
        )
    }) {
        return outcome;
    }
    if effective.grep_cap == Mode::Off {
        return quiet;
    }
    let Some(row) = facts
        .tool_name
        .as_deref()
        .and_then(|tool| rift_agent::tool_fields::tool_field_row(&raw.agent, tool))
    else {
        return quiet;
    };
    if row.fact("kind") != Some(GREP_KIND) {
        return quiet;
    }
    // The argument the cap is added to, named by the row and by nothing here.
    // No pinned argument, no amendment: a family whose row states no bound has
    // no bound to be given.
    let (Some(capped_field), Some(args)) = (row.fact("head_limit"), search_args(row, &raw.payload))
    else {
        return quiet;
    };
    let Some(key) = search_key(args, capped_field) else {
        return quiet;
    };
    // The context window, not the session: a subagent runs under its parent's
    // session id with its own agent id, and one of them having been given a
    // bound says nothing about the other.
    let window = SearchWindow::at(work_tree, &raw.session, facts.child_session.as_deref());
    let Some(step) = crate::hooks::gate_step(&raw.agent, &raw.verb, &raw.payload) else {
        return quiet;
    };
    match step {
        crate::hooks::GateStep::Decide => decide(
            raw,
            git_dir,
            work_tree,
            effective,
            &window,
            args,
            capped_field,
            &key,
        ),
        crate::hooks::GateStep::CrossCheck => {
            verify(raw, git_dir, work_tree, &window, args, capped_field, &key)
        }
        crate::hooks::GateStep::Evict(_) => quiet,
    }
}

/// **Decide.** The call is a content search this family can be amended on;
/// whether it is amended is the whole of what follows.
#[allow(clippy::too_many_arguments)]
fn decide(
    raw: &RawHook,
    git_dir: &Path,
    work_tree: &Path,
    effective: &rift_recall_policy::Effective,
    window: &SearchWindow,
    args: &serde_json::Map<String, serde_json::Value>,
    capped_field: &str,
    key: &str,
) -> SearchOutcome {
    let quiet = SearchOutcome::default();
    // Rule 1: a caller that named its own bound — including the explicit "no
    // bound" of a zero — chose it, and this gate leaves every such call
    // untouched. Presence, never value: the argument being written at all is
    // the choice.
    if args.contains_key(capped_field) {
        return quiet;
    }
    // A window whose amendment was verified to have been dropped is a window
    // sharing the channel with somebody else's hook. Rift does not fight over
    // it: the fact was recorded once and this window's searches run as they
    // were written.
    if window.is_dropped() {
        return quiet;
    }
    let turn = crate::hooks::gate_turn(raw, git_dir);
    match window.claim(CAP_PREFIX, key, turn) {
        // Rule 2: this search was already capped in this window, so this one
        // runs with no bound at all — the escape hatch the tool's own total
        // invites the model to take. Taken inside the turn that capped it, the
        // repeat is also the honesty figure this lever is judged by.
        Claim::Repeat { capped_in_turn } => {
            if capped_in_turn == turn {
                window.note_requery(REQUERY_PREFIX, key);
            }
            quiet
        }
        Claim::Fresh => match effective.grep_cap {
            // Remembered before the answer is given, so the result of this
            // very call can be checked against it. Nothing is booked here: an
            // amendment is a request to the client, and the row is written
            // where the executed call proves it happened.
            Mode::Enforce => {
                window.arm(key, effective.grep_head_limit);
                SearchOutcome {
                    journaled: false,
                    answer: Some(SearchAnswer::Cap(GrepCap {
                        tool_input: serde_json::Value::Object(args.clone()),
                        head_limit: effective.grep_head_limit,
                    })),
                }
            }
            // Reached, decided, and not acted on. Every check above ran, so
            // the row sizes what this lever would be working on; there is
            // nothing to verify, because nothing was asked of the client.
            Mode::Observe => SearchOutcome {
                journaled: journal_cap(git_dir, work_tree, raw),
                answer: None,
            },
            Mode::Off => quiet,
        },
    }
}

/// **Did the cap this window added actually reach the tool?**
///
/// The executed call carries the input the tool really ran with, so the
/// question is answered exactly rather than inferred:
///
/// * the bound is on the executed call — the amendment happened, and this is
///   where the row is written. Once: the arming is consumed, so a second
///   result for one amendment books nothing;
/// * the bound is gone — a sibling hook amended the same call and the client
///   kept its amendment instead. The search ran in full, there is nothing to
///   record about it but the fact of the drop, and **no decline follows**: a
///   content search is never substituted, so the honest response to losing the
///   channel is to stop using it for this window.
fn verify(
    raw: &RawHook,
    git_dir: &Path,
    work_tree: &Path,
    window: &SearchWindow,
    args: &serde_json::Map<String, serde_json::Value>,
    capped_field: &str,
    key: &str,
) -> SearchOutcome {
    let quiet = SearchOutcome::default();
    // A search this window never amended is a search whose result contradicts
    // nothing. Costs one `stat`.
    let Some(armed) = window.take_armed(key) else {
        return quiet;
    };
    // The bound this window asked for, not merely *a* bound. The key is the
    // search without its paging arguments, so a call that was amended and
    // never came back — a rejected call, a session that ended — leaves an
    // arming that the same search, re-issued later with a bound the agent
    // chose for itself, would otherwise consume: a row saying Rift capped a
    // search it did not cap. Equality closes that, and it costs one comparison
    // against a value this module wrote down.
    if args.get(capped_field).and_then(serde_json::Value::as_u64) == Some(u64::from(armed)) {
        return SearchOutcome {
            journaled: journal_cap(git_dir, work_tree, raw),
            answer: None,
        };
    }
    if args.contains_key(capped_field) {
        // A bound is on the executed call and it is not the one this window
        // asked for. Nothing here can tell a sibling hook's amendment from the
        // agent's own second thoughts, and neither is a cap Rift made, so
        // nothing is booked — and the window stops amending, because the one
        // reading that would matter is the one where the channel is shared.
        window.mark_dropped();
        return quiet;
    }
    window.mark_dropped();
    quiet
}

/// The arguments object this family's pinned row says a call carries them in.
///
/// Read off the row rather than known here: two families put them flat under
/// the envelope field beside `tool_name`, one nests them, and the row is where
/// that fact is already pinned to an artifact.
fn search_args(
    row: rift_agent::tool_fields::ToolFieldRow,
    payload: &serde_json::Value,
) -> Option<&serde_json::Map<String, serde_json::Value>> {
    let mut node = payload;
    for segment in row.fact("args").unwrap_or(ARGS_FIELD).split('.') {
        node = node.get(segment)?;
    }
    node.as_object()
}

/// **The identity of one search**: everything the caller wrote except the two
/// parameters that page through the results.
///
/// A digest rather than the arguments themselves, for two reasons that point
/// the same way: it is a file name in a directory this member keeps, and a
/// pattern is the agent's own text — which never becomes a path, a marker name
/// or anything else Rift writes down in the clear.
///
/// Sorted before it is hashed, so two spellings of one call in different
/// argument orders are one key.
fn search_key(
    args: &serde_json::Map<String, serde_json::Value>,
    capped_field: &str,
) -> Option<String> {
    let mut fields: Vec<(&str, String)> = args
        .iter()
        .filter(|(name, _)| name.as_str() != capped_field && name.as_str() != PAGING_FIELD)
        .map(|(name, value)| (name.as_str(), value.to_string()))
        .collect();
    if fields.is_empty() {
        return None;
    }
    fields.sort_unstable();
    let mut bytes = Vec::new();
    for (name, value) in fields {
        bytes.extend_from_slice(name.as_bytes());
        bytes.push(0);
        bytes.extend_from_slice(value.as_bytes());
        bytes.push(0);
    }
    Some(rift_hashchain::leaf_hash(&bytes).to_hex())
}

/// Journal the accounting one cap owes, and say whether the worker was left
/// anything.
///
/// The row books **no figures at all**: the search ran, Rift added nothing to
/// its output and withheld nothing from it, so there is neither a cost nor a
/// saving to claim. What the row carries is that a cap happened, which is the
/// count the search fixture divides by.
///
/// The total the tool reports is deliberately absent. The pinned artifact says
/// the total is stated in the `count` mode's own rendered output — text, in a
/// shape no artifact pins — and the fields a result carries beside it are the
/// *returned* counts, as the recorded observation of them says in so many
/// words. Rift reads pinned fields and never a rendering, so the count this
/// row carries is unknown rather than a number read out of a format nobody
/// pinned. The total's real audience is the model, which is handed it by the
/// tool itself.
fn journal_cap(git_dir: &Path, work_tree: &Path, raw: &RawHook) -> bool {
    crate::agent_queue::enqueue(
        git_dir,
        &crate::agent_queue::AgentJob {
            schema_version: 1,
            work_tree: work_tree.to_path_buf(),
            queued_at_ms: raw.occurred_at_ms,
            attempts: 0,
            kind: crate::agent_queue::JobKind::ServeAudit(crate::agent_queue::ServeAudit {
                call_id: {
                    use rift_core::UlidSource as _;
                    crate::hooks::AUDIT_IDS.next_ulid().to_hex()
                },
                decision: crate::agent_queue::ServeAuditKind::GrepCapped,
                agent: raw.agent.as_str().to_owned(),
                // A search is about no file and no revision, and the frame is
                // empty because nothing was put in front of a model.
                basis: None,
                frame: String::new(),
                tag: None,
                total: None,
                declared_avoided: None,
                // A bounded content search stood in for no listing at all.
                shape: None,
            }),
        },
    )
    .is_ok()
}

// ------------------------------------------------ answering a name search ---

/// How many paths a native listing of this repository would have put in front
/// of the model before it truncated.
///
/// The pinned figure, not a round one: the `Glob` tool's own page says
/// "Results are sorted by modification time and capped at 100 files", recorded
/// verbatim in this family's tool-field pin. It bounds the list the worker
/// prices, because that list has to be the one the model would really have
/// received or the figure is about a listing nobody would have got.
#[cfg(feature = "search")]
use rift_recall_serve::NATIVE_LISTING_PATHS;

/// The characters one token of the answer budget is worth.
///
/// The same conversion the boundary map makes, and here it is the bound rather
/// than a starting point: a decision runs inside a hook, and the tokenizer
/// belongs to the worker that runs afterwards. It bites only on unusually long
/// paths — the operator's real bound on this frame is how many paths it names.
#[cfg(feature = "search")]
const ANSWER_CHARS_PER_TOKEN: usize = 4;

/// The marker prefix for a name search this window has answered.
#[cfg(feature = "search")]
const ANSWER_PREFIX: &str = "g-";

/// The marker prefix for a search this window bounded.
const CAP_PREFIX: &str = "c-";

/// The marker prefix for a bounded search asked again in the turn it was
/// bounded in: the re-query rate's own definition, which is `q-` over `c-`.
const REQUERY_PREFIX: &str = "q-";

/// The marker prefix for an answered search asked again in the turn it was
/// answered in: the escape hatch taken immediately, counted apart from the
/// bound's re-queries so neither figure is the other's numerator.
#[cfg(feature = "search")]
const REPEAT_PREFIX: &str = "r-";

/// The directory the paths one answer stood in for are left in for the worker.
///
/// Named in every build, because a checkout written to by one that answers
/// name searches is still swept by one that does not.
const ANSWERS_DIR: &str = "answers";

/// The neutral tag on an answer that was decided and not made.
#[cfg(feature = "search")]
const TAG_ANSWER_OBSERVED: &str = "answer_observed";

/// A search for file **names** that Rift can answer from the index it holds.
#[cfg(feature = "search")]
#[derive(Debug, Clone, PartialEq, Eq)]
struct NameSearch {
    /// The repository-relative directory the search is scoped to; empty for
    /// the whole tree.
    dir: String,
    /// The pattern, exactly as the agent wrote it.
    pattern: String,
    /// Which native call this is, which decides two separate things: how the
    /// pattern is matched (see [`NameSearch::whole_path`]) and what ceiling
    /// the listing it stands in for is priced at (see
    /// `rift_recall_serve::SearchCeiling`). One fact rather than two, so a
    /// search can never be matched as one shape and priced as the other.
    shape: rift_recall_serve::ListingShape,
}

#[cfg(feature = "search")]
impl NameSearch {
    /// The identity of one name search: what it looks in, what it looks for,
    /// and which of the two ways it is matched.
    ///
    /// A digest, for the reason [`search_key`] is one — it becomes a file name,
    /// and the pattern is the agent's own text, which Rift never writes down in
    /// the clear.
    /// Which native call this is — the axis its declared baseline is priced
    /// on.
    fn shape(&self) -> rift_recall_serve::ListingShape {
        self.shape
    }

    /// Whether the pattern is matched against the whole repository-relative
    /// path — which is what a glob means — or against the file's name alone,
    /// which is what the listing grammar's `-name` means. The two are
    /// different questions and answering one with the other would list files
    /// the agent did not ask for.
    fn whole_path(&self) -> bool {
        matches!(self.shape, rift_recall_serve::ListingShape::Glob)
    }

    fn key(&self) -> String {
        let mut bytes = Vec::with_capacity(self.dir.len() + self.pattern.len() + 3);
        bytes.push(u8::from(self.whole_path()));
        bytes.extend_from_slice(self.dir.as_bytes());
        bytes.push(0);
        bytes.extend_from_slice(self.pattern.as_bytes());
        rift_hashchain::leaf_hash(&bytes).to_hex()
    }

    /// Whether one indexed path is one of this search's matches.
    fn matches(&self, path: &str) -> bool {
        let Some(rest) = self.under_scope(path) else {
            return false;
        };
        if self.whole_path() {
            return rift_config::glob_match(&self.pattern, rest);
        }
        // The listing grammar's pattern is a file name, and `find -name` walks
        // the whole subtree: every depth, matched on the last segment.
        rest.rsplit('/')
            .next()
            .is_some_and(|name| rift_config::glob_match(&self.pattern, name))
    }

    /// The path relative to this search's directory, or `None` when it is not
    /// under it at all.
    fn under_scope<'a>(&self, path: &'a str) -> Option<&'a str> {
        if self.dir.is_empty() {
            return Some(path);
        }
        path.strip_prefix(&self.dir)?.strip_prefix('/')
    }
}

/// The name search this call is, or `None` when it is not one this gate can
/// answer.
///
/// Every refusal leaves the call untouched, and each closes a way an answer
/// could be about something other than what the agent asked for:
///
/// * a **glob** carrying an argument its pinned row does not name is not a
///   glob over the whole tree — the family's own page names the pattern and
///   nothing else, so a call that scopes itself with an argument no artifact
///   pins is a call this gate does not understand;
/// * a **listing with no pattern** is a request for the contents of a
///   directory, and directories are exactly what an index of files cannot
///   enumerate: it would answer `ls` with no subdirectory names and a total
///   that counted none of them;
/// * a directory this checkout has no name for belongs to no checkout this
///   gate can answer about;
/// * a pattern longer than the matcher's own bound is a payload rather than a
///   pattern, and an empty one is not a search.
#[cfg(feature = "search")]
fn name_search(
    agent: &rift_core::AgentId,
    payload: &serde_json::Value,
    facts: &rift_agent::HookFacts,
    kind: &rift_agent::SearchKind,
    work_tree: &Path,
) -> Option<NameSearch> {
    use rift_agent::SearchKind;
    let bounded = |dir: String, pattern: &str, shape: rift_recall_serve::ListingShape| {
        (!pattern.is_empty() && pattern.len() <= rift_recall_search::query::MAX_GLOB_BYTES).then(
            || NameSearch {
                dir,
                pattern: pattern.to_owned(),
                shape,
            },
        )
    };
    match kind {
        SearchKind::Glob { pattern } => {
            let row = rift_agent::tool_fields::tool_field_row(agent, facts.tool_name.as_deref()?)?;
            let named = row.fact("pattern")?;
            let args = search_args(row, payload)?;
            if args.keys().any(|key| key != named) {
                return None;
            }
            bounded(
                String::new(),
                pattern,
                rift_recall_serve::ListingShape::Glob,
            )
        }
        SearchKind::Listing { dir, pattern } => {
            let root = work_tree.canonicalize().ok()?;
            let scope = scope_dir(&root, dir, crate::hooks::payload_cwd(facts).as_deref())?;
            bounded(
                scope,
                pattern.as_deref()?,
                rift_recall_serve::ListingShape::Listing,
            )
        }
        SearchKind::Grep { .. } => None,
    }
}

/// One directory, as this checkout names it: repository-relative, `/`-spelled,
/// empty for the root itself.
///
/// One spelling, for the reason the read ledger has one: a relative name
/// resolved against some other directory, or a symlinked prefix, would make
/// two answers about two different trees look like one. A relative name is
/// resolved against the directory the payload itself states and against
/// nothing else — the hook process's own is where the client launched it, not
/// where the agent stood.
#[cfg(feature = "search")]
fn scope_dir(root: &Path, named: &str, cwd: Option<&Path>) -> Option<String> {
    let dir = Path::new(named);
    let joined;
    let dir = if dir.is_absolute() {
        dir
    } else {
        joined = cwd.filter(|at| at.is_absolute())?.join(dir);
        joined.as_path()
    };
    let canonical = dir.canonicalize().ok()?;
    if !canonical.is_dir() {
        return None;
    }
    // The root itself is the empty scope, which the chokepoint spells as no
    // name at all; a directory outside the root is no scope.
    canonical.strip_prefix(root).ok()?;
    Some(rift_recall_substrate::worktree::repo_relative_slash(root, &canonical).unwrap_or_default())
}

/// The name-search arm, or `None` when this call is not one — which is every
/// call in a build without the search seam, where there is no index to answer
/// from and the search runs exactly as it was written.
#[cfg(feature = "search")]
#[allow(clippy::too_many_arguments)]
fn name_answer(
    t: &crate::hooktime::HookTimer,
    registry: &AdapterRegistry,
    raw: &RawHook,
    git_dir: &Path,
    work_tree: &Path,
    policy: &rift_recall_policy::ServePolicy,
    effective: &rift_recall_policy::Effective,
    facts: &rift_agent::HookFacts,
) -> Option<SearchOutcome> {
    let request = registry
        .get(&raw.agent)?
        .search_request(&raw.verb, &raw.payload)?;
    let named = name_search(&raw.agent, &raw.payload, facts, &request.kind, work_tree)?;
    // A decision, and only a decision: an answered call never runs, so there
    // is no result to check it against, and a result that arrives anyway is
    // the repeat this window already allows.
    if !matches!(
        crate::hooks::gate_step(&raw.agent, &raw.verb, &raw.payload),
        Some(crate::hooks::GateStep::Decide)
    ) {
        return Some(SearchOutcome::default());
    }
    let window = SearchWindow::at(work_tree, &raw.session, facts.child_session.as_deref());
    Some(answer_name_search(
        t, raw, git_dir, work_tree, policy, effective, &window, &named,
    ))
}

/// The same arm in a build without the search seam.
#[cfg(not(feature = "search"))]
#[allow(clippy::too_many_arguments)]
fn name_answer(
    _t: &crate::hooktime::HookTimer,
    _registry: &AdapterRegistry,
    _raw: &RawHook,
    _git_dir: &Path,
    _work_tree: &Path,
    _policy: &rift_recall_policy::ServePolicy,
    _effective: &rift_recall_policy::Effective,
    _facts: &rift_agent::HookFacts,
) -> Option<SearchOutcome> {
    None
}

/// **Answer.** The call is a name search on a family that may be declined; what
/// it is answered with, and whether it is answered at all, is the whole of what
/// follows.
#[cfg(feature = "search")]
#[allow(clippy::too_many_arguments)]
fn answer_name_search(
    t: &crate::hooktime::HookTimer,
    raw: &RawHook,
    git_dir: &Path,
    work_tree: &Path,
    policy: &rift_recall_policy::ServePolicy,
    effective: &rift_recall_policy::Effective,
    window: &SearchWindow,
    named: &NameSearch,
) -> SearchOutcome {
    let quiet = SearchOutcome::default();
    if effective.search_answer == Mode::Off {
        return quiet;
    }
    // The index or nothing. A hook that walked a tree to answer a search would
    // spend more than the search costs, so a cold or stale index is a search
    // that runs exactly as it was written and a walk the worker is asked for.
    let Some(index) = warm_index(t, work_tree) else {
        return SearchOutcome {
            journaled: crate::hooks::owe_search_index(git_dir, work_tree),
            answer: None,
        };
    };
    let matched: Vec<rift_recall_search::FileEntry> = index
        .entries()
        .iter()
        .filter(|entry| named.matches(&entry.path))
        .cloned()
        .collect();
    // A search this index has no match for is not a search Rift knows the
    // answer to. The index skips what git ignores and the tool does not, so
    // "nothing here" is a statement about the index and never about the tree —
    // and a model told there is no such file stops looking for it.
    if matched.is_empty() {
        return quiet;
    }
    let total = matched.len();
    // What the native call itself would have printed, which is a different set
    // from the one Rift is about to name. A glob tool stops after a hundred
    // files, so the head this hook ranked is the whole of it; a shell listing
    // prints every match and is cut only by how much of the result the client
    // reads back — so that shape is priced over the match set, not over the
    // ranked head, or the claim describes a listing nobody would have
    // received.
    let shape = named.shape();
    let would_have_printed: Vec<String> = match shape {
        rift_recall_serve::ListingShape::Glob => Vec::new(),
        rift_recall_serve::ListingShape::Listing => {
            matched.iter().map(|entry| entry.path.clone()).collect()
        }
    };
    let Some(ranked) = rank(work_tree, raw, window, index.key().clone(), matched) else {
        return quiet;
    };
    let listed: Vec<String> = ranked.hits.iter().map(|hit| hit.path.clone()).collect();
    // Two sets, screened by one screen, because they answer two questions.
    // What Rift may **name** is the ranked head, which is bounded by what a
    // frame can hold. What Rift may **price** is what the client would have
    // shown for this shape, which for a listing is very much larger. Both are
    // answered here, before the frame is written and before anything is
    // priced, because a path Rift will not name is one it may not claim to
    // have saved either — and because a name it will not say is not a name it
    // writes into its own run state on the way to the worker.
    //
    // Three screens, not two. The repository's own `denied_paths` and the
    // structural safety of the name are the serve crate's; the repository's
    // *sensitive* list is this call site's, because the policy engine keeps
    // it private and the one other caller of that function — the `search`
    // verb — deliberately does name sensitive paths to the operator who
    // asked. So it is passed in here, where a hook answers for a model, and
    // it runs before the price and not only before the frame.
    let config = crate::hooks::effective_config(work_tree);
    let sensitive = |path: &str| config.is_sensitive_path(path);
    let engine = rift_recall_policy::PolicyEngine::new(&config).with_serve_policy(policy.clone());
    let nameable = rift_recall_serve::priceable_listing(
        &engine,
        rift_recall_serve::ListingShape::Glob,
        raw.agent.as_str(),
        &listed,
        sensitive,
    );
    let priced = match shape {
        rift_recall_serve::ListingShape::Glob => nameable.clone(),
        rift_recall_serve::ListingShape::Listing => rift_recall_serve::priceable_listing(
            &engine,
            shape,
            raw.agent.as_str(),
            &would_have_printed,
            sensitive,
        ),
    };
    let Some(reason) = answer_frame(&config, &nameable, total, named, effective) else {
        return quiet;
    };
    // The whole point is to hand the model less than the listing would have.
    // A short listing is one the answer cannot beat — the frame's own
    // sentence costs more than the handful of paths it stands in for — and a
    // lever that fires there spends tokens to save none. Characters rather
    // than tokens, because the tokenizer belongs to the worker; the
    // comparison errs against answering, because the listing this is measured
    // against is spelled the short way and the native one is not.
    if reason.chars().count() >= priced.join("\n").chars().count() {
        return quiet;
    }
    // One answer per name search per window, which is what makes the frame's
    // own escape hatch true: the repeat runs the search in full. Taken in both
    // modes, so what `observe` records is the decision `enforce` would have
    // made and not one row per glob of the same pattern.
    let turn = crate::hooks::gate_turn(raw, git_dir);
    if let Claim::Repeat { capped_in_turn } = window.claim(ANSWER_PREFIX, &named.key(), turn) {
        // Asked again inside the turn it was answered in: the escape hatch
        // taken immediately, which is the figure this lever is judged by.
        if capped_in_turn == turn {
            window.note_requery(REPEAT_PREFIX, &named.key());
        }
        return quiet;
    }
    if effective.search_answer != Mode::Enforce {
        // Reached, decided, and not acted on. Every check above ran, so the
        // row sizes what this lever would be working on; the frame is left out
        // because nothing was put in front of a model, and a sentence nobody
        // read is not a cost this row may book against itself.
        return SearchOutcome {
            journaled: journal_answer(git_dir, work_tree, raw, shape, "", None, Some(0)),
            answer: None,
        };
    }
    // The paths the answer stood in for, left for the worker to price with the
    // real tokenizer under the identity of this decision. Cut where the native
    // tool cuts, because that is the listing the model would have received.
    let call_id = {
        use rift_core::UlidSource as _;
        crate::hooks::AUDIT_IDS.next_ulid().to_hex()
    };
    // Best effort, and the row is written either way: a frame the model read
    // is a cost whether or not the listing behind it can be priced, and a
    // decision whose paths are missing books that cost with no claim beside
    // it rather than none at all.
    let _ = leave_priced_paths(work_tree, &call_id, &priced);
    let journaled = journal_answer(
        git_dir,
        work_tree,
        raw,
        shape,
        &reason,
        Some(&call_id),
        None,
    );
    SearchOutcome {
        journaled,
        answer: Some(SearchAnswer::Listing(reason)),
    }
}

/// This checkout's index, **loaded and never built**.
///
/// The probe that decides whether the saved index still answers is bounded by
/// the same rule a cold build is: a tree big enough for the walk to be
/// unaffordable is one where deciding whether the file is stale is unaffordable
/// too, and a hook that spent its budget finding that out would have spent it
/// for nothing. Either way the answer here is `None`, and `None` is a search
/// that runs.
#[cfg(feature = "search")]
fn warm_index(
    t: &crate::hooktime::HookTimer,
    work_tree: &Path,
) -> Option<rift_recall_search::PathIndex> {
    let path = rift_recall_search::index_path(work_tree);
    // One `stat` before anything expensive. A checkout the worker has not been
    // round yet is the common cold case — every session before its first pass,
    // and every tree too large for one — and asking git and the directory tree
    // whether a file that is not there is stale costs the whole answer's
    // budget to learn nothing.
    if !path.is_file() {
        return None;
    }
    let head = t.phase("search_head", || rift_recall_search::head_oid(work_tree));
    let deadline = std::time::Instant::now() + rift_recall_search::COLD_BUDGET;
    let key = t.phase("search_probe", || {
        rift_recall_search::walk::current_key_within(work_tree, &head, Some(deadline))
    })?;
    t.phase("search_load", || {
        rift_recall_search::PathIndex::load(&path, &key)
    })
}

/// The matches, best first, with the warmth of this conversation's own reading
/// folded in.
///
/// The ranking is the engine's and not this module's: same index, same facts,
/// same query, same order. What this supplies is the two arguments a hook can
/// afford — the matched paths, and the window's own read ledger, which is one
/// bounded file the page cache is already holding. What it deliberately does
/// not supply is git status: a full status of a working tree is not a hook's
/// to run, and a frame that tagged nothing is honest where a frame that
/// guessed would not be.
#[cfg(feature = "search")]
fn rank(
    work_tree: &Path,
    raw: &RawHook,
    window: &SearchWindow,
    key: rift_recall_search::IndexKey,
    matched: Vec<rift_recall_search::FileEntry>,
) -> Option<rift_recall_search::Ranked> {
    use rift_recall_search::SearchEngine as _;
    let index = rift_recall_search::PathIndex::new(key, matched).ok()?;
    let mut frecency = rift_recall_search::FrecencyFacts::new();
    frecency.absorb_ledger(&window.reads(work_tree));
    Some(
        rift_recall_search::NativeEngine::new(index)
            .with_frecency(frecency)
            .rank_paths(&rift_recall_search::PathQuery {
                // No fuzzy half at all: the pattern already said which paths
                // are answers, and every one of them answers it equally well.
                // What is left to decide is the order, which is warmth.
                text: String::new(),
                limit: NATIVE_LISTING_PATHS,
                now_ms: u64::try_from(raw.occurred_at_ms).unwrap_or_default(),
            }),
    )
}

/// The sentence the model reads, fitted to the operator's budget.
///
/// Written by the one renderer allowed to write one; this function chooses the
/// paths and states the total. Two bounds apply, in this order: the operator's
/// `glob_top_n`, which is how many paths an answer names, and the character
/// budget, which drops from the ranked tail — the coldest end — until the
/// frame fits. The total never moves: it is the count of what matched, and a
/// frame that shrank its list while restating its total is the honest shape.
#[cfg(feature = "search")]
fn answer_frame(
    config: &rift_config::EffectiveConfig,
    listed: &[String],
    total: usize,
    named: &NameSearch,
    effective: &rift_recall_policy::Effective,
) -> Option<String> {
    let top_n = usize::try_from(effective.glob_top_n).unwrap_or_default();
    // The index is built without the paths this repository calls sensitive —
    // but a list edited after the index was written moves no directory mtime
    // and no HEAD, so the saved index stays fresh and can still hold a path
    // that has since become sensitive. This is the belt on top of that brace:
    // a name is the one thing an answer says out loud. The caller answered the
    // same screen before anything was priced, so a path this drops was never
    // billed for either.
    let mut shown: Vec<rift_recall_serve::SearchHit<'_>> = listed
        .iter()
        .filter(|path| !config.is_sensitive_path(path))
        .take(top_n)
        .map(|path| rift_recall_serve::SearchHit {
            path,
            // Nothing is claimed about the working tree: see `rank`.
            status: rift_recall_serve::PathStatus::Tracked,
        })
        .collect();
    let budget_chars = usize::try_from(effective.answer_budget_tokens)
        .unwrap_or_default()
        .saturating_mul(ANSWER_CHARS_PER_TOKEN);
    while !shown.is_empty() {
        let text = rift_recall_serve::render_search_answer(&shown, total, &named.pattern)?;
        if text.chars().count() <= budget_chars {
            return Some(text);
        }
        shown.pop();
    }
    None
}

/// Leave the paths this answer stood in for where the worker will find them.
///
/// A file rather than a field of the journaled row: what the listing would have
/// cost is a tokenizer question, the tokenizer belongs to the worker, and the
/// worker needs the paths themselves to ask it. Keyed by the decision's own id,
/// under this member's own run state, and collected by `rift clean` with the
/// window markers beside it if the worker never comes.
#[cfg(feature = "search")]
fn leave_priced_paths(work_tree: &Path, call_id: &str, paths: &[String]) -> bool {
    let dir = work_tree.join(".rift/state/search").join(ANSWERS_DIR);
    if std::fs::create_dir_all(&dir).is_err() {
        return false;
    }
    std::fs::write(dir.join(call_id), paths.join("\n")).is_ok()
}

/// Journal the accounting one answer owes.
///
/// `frame` is what the model was handed, which the worker counts; `call_id`
/// names the paths it stood in for, which the worker prices; `shape` is which
/// native call they stood in for, which is the ceiling the worker prices them
/// at; `declared` is the figure a decision that was only observed books
/// instead — zero, because nothing was answered and nothing was avoided.
#[cfg(feature = "search")]
#[allow(clippy::too_many_arguments)]
fn journal_answer(
    git_dir: &Path,
    work_tree: &Path,
    raw: &RawHook,
    shape: rift_recall_serve::ListingShape,
    frame: &str,
    call_id: Option<&str>,
    declared: Option<u32>,
) -> bool {
    let call_id = call_id.map_or_else(
        || {
            use rift_core::UlidSource as _;
            crate::hooks::AUDIT_IDS.next_ulid().to_hex()
        },
        str::to_owned,
    );
    crate::agent_queue::enqueue(
        git_dir,
        &crate::agent_queue::AgentJob {
            schema_version: 1,
            work_tree: work_tree.to_path_buf(),
            queued_at_ms: raw.occurred_at_ms,
            attempts: 0,
            kind: crate::agent_queue::JobKind::ServeAudit(crate::agent_queue::ServeAudit {
                call_id,
                decision: crate::agent_queue::ServeAuditKind::SearchAnswered,
                agent: raw.agent.as_str().to_owned(),
                // A listing is about no file and no revision.
                basis: None,
                frame: frame.to_owned(),
                tag: declared.map(|_| TAG_ANSWER_OBSERVED.to_owned()),
                total: None,
                declared_avoided: declared,
                shape: Some(shape),
            }),
        },
    )
    .is_ok()
}

/// The paths one answer stood in for, as the worker reads them back.
///
/// `None` when the file is gone — a `clean` between the decision and the
/// drain, a member removed — which the worker books as a figure it could not
/// establish rather than as a saving of nothing.
#[cfg(feature = "search")]
pub(crate) fn priced_paths(work_tree: &Path, call_id: &str) -> Option<Vec<String>> {
    let path = work_tree
        .join(".rift/state/search")
        .join(ANSWERS_DIR)
        .join(call_id);
    let text = std::fs::read_to_string(&path).ok()?;
    // Read once and taken away: the figure is booked once, and a file left
    // behind would be collected later as an answer nobody priced.
    let _ = std::fs::remove_file(&path);
    Some(text.lines().map(str::to_owned).collect())
}

/// The same, in a build that answers no name search: there is nothing to
/// price, because nothing here ever stood in for a listing.
#[cfg(not(feature = "search"))]
pub(crate) fn priced_paths(_work_tree: &Path, _call_id: &str) -> Option<Vec<String>> {
    None
}

// ----------------------------------------- one context window's decisions ---

/// The directory a window's search decisions are remembered in, under this
/// member's own run state.
const WINDOW_DIR: &str = "w";

/// The marker that says an amendment this window made was displaced, so this
/// window has stopped making them.
const DROPPED_MARKER: &str = "dropped";

/// What one **context window** has already been given a bound for.
///
/// Marker files whose *existence* is the fact, created with `create_new` so
/// that the process which created one is the one that acted: a client may run
/// its hooks in parallel, and a read-modify-write of a shared record would let
/// two hooks both conclude they were first — which here means capping a search
/// twice and taking away the escape hatch that makes the cap safe.
///
/// Per member, under `.rift/state/`, keyed by nothing about the checkout: a
/// window is a conversation, a conversation happens in one checkout, and
/// losing the directory costs one extra cap and can never cost an answer.
struct SearchWindow {
    dir: PathBuf,
    /// The window's own identity, which is also what its read ledger is named
    /// by: one conversation, one key, wherever this member writes about it.
    #[cfg(feature = "search")]
    key: String,
}

/// What a window says about a search it is being asked to bound.
#[derive(Debug, PartialEq, Eq)]
enum Claim {
    /// This window has not bounded this search before, and this call is the
    /// one that claimed it.
    Fresh,
    /// It has, in the turn named — so this call runs unbounded.
    Repeat {
        /// The turn the cap was made in, which is what makes a repeat inside
        /// that turn a re-query rather than ordinary later work.
        capped_in_turn: u32,
    },
}

impl SearchWindow {
    /// The window a hook belongs to, as this member sees it.
    fn at(work_tree: &Path, session_id: &str, agent_id: Option<&str>) -> Self {
        let key = crate::read_ledger::key(session_id, agent_id);
        Self {
            dir: work_tree
                .join(".rift/state/search")
                .join(WINDOW_DIR)
                .join(&key),
            #[cfg(feature = "search")]
            key,
        }
    }

    /// The read ledger this window writes: the same key, in the directory the
    /// search crate names for it.
    #[cfg(feature = "search")]
    fn reads(&self, work_tree: &Path) -> PathBuf {
        rift_recall_search::reads_dir(work_tree).join(format!("{}.jsonl", self.key))
    }

    /// Claim the one decision this window may make for `key`.
    ///
    /// `prefix` is which kind of decision — a search bounded, or a name search
    /// answered. The two are one mechanism because they are one rule: a
    /// decision about a search is made once per window and the repeat runs.
    fn claim(&self, prefix: &str, key: &str, turn: u32) -> Claim {
        if self.create(&format!("{prefix}{key}"), turn.to_string().as_bytes()) {
            return Claim::Fresh;
        }
        // Some earlier call in this window decided this search. Which turn it
        // did so in is the only thing left to learn, and a body this module
        // did not write reads as a turn nobody is in.
        let capped_in_turn = std::fs::read_to_string(self.dir.join(format!("{prefix}{key}")))
            .ok()
            .and_then(|body| body.trim().parse().ok())
            .unwrap_or(u32::MAX);
        Claim::Repeat { capped_in_turn }
    }

    /// Remember the bound this window asked the client for, so the result of
    /// that call can be checked against it.
    ///
    /// The value and not merely the fact: an arming the executed call never
    /// answered outlives the call that made it, and the only thing that tells
    /// a bound Rift asked for from one the agent chose later is what the
    /// number is.
    fn arm(&self, key: &str, head_limit: u32) {
        let _ = self.create(&format!("a-{key}"), head_limit.to_string().as_bytes());
    }

    /// Take the arming for `key`: the bound this window really did ask for on
    /// this search, consumed so that one amendment is answered once.
    ///
    /// A body this module did not write, or one from a build that wrote none,
    /// reads as a bound no policy can resolve to — so it matches nothing and
    /// books nothing.
    fn take_armed(&self, key: &str) -> Option<u32> {
        let path = self.dir.join(format!("a-{key}"));
        let body = std::fs::read_to_string(&path).ok();
        std::fs::remove_file(&path).ok()?;
        Some(
            body.and_then(|body| body.trim().parse().ok())
                .unwrap_or(u32::MAX),
        )
    }

    /// Record that this search was asked again inside the turn it was decided
    /// in.
    ///
    /// Once per search: the figure is "how many decided searches were asked
    /// again", not "how many times". `prefix` keeps the two levers' figures
    /// apart — a bounded search asked again is the re-query rate, an answered
    /// one asked again is the escape hatch taken immediately, and a reader
    /// dividing one count by the other must not be handed their sum.
    fn note_requery(&self, prefix: &str, key: &str) -> bool {
        self.create(&format!("{prefix}{key}"), b"")
    }

    /// Whether this window has stopped amending calls.
    fn is_dropped(&self) -> bool {
        self.dir.join(DROPPED_MARKER).exists()
    }

    /// Stop amending, and say whether **this** call is the one that stopped it.
    fn mark_dropped(&self) -> bool {
        self.create(DROPPED_MARKER, b"dropped")
    }

    /// Create one marker, and say whether this call created it.
    ///
    /// The names are digests and the bodies are a turn ordinal — nothing
    /// derived from the repository's text or from the agent's own words, which
    /// is why these need no narrower permissions than the run state around
    /// them.
    fn create(&self, name: &str, body: &[u8]) -> bool {
        if std::fs::create_dir_all(&self.dir).is_err() {
            return false;
        }
        let Ok(mut file) = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(self.dir.join(name))
        else {
            return false;
        };
        use std::io::Write as _;
        let _ = file.write_all(body);
        true
    }
}

// ---------------------------------------------------------- collecting it ---

/// **Collect what ended conversations left behind.**
///
/// Two kinds of thing accumulate under this member's search state, and neither
/// had anything that removed it: one directory per context window, holding one
/// small marker per decision, and one file per answered search holding the
/// paths that answer stood in for. A window is a conversation, conversations
/// end, and the markers of one that ended a month ago decide nothing.
///
/// By age and never wholesale, for the reason the recovery markers are: a
/// marker is what stops a live window being answered twice, and dropping a
/// fresh one costs a second decision in a conversation that is still open —
/// exactly the spend this lever exists to remove. Nothing this module did not
/// write is touched, however it got there: a window directory is named by a
/// digest, and an answer file by an id of the shape this process mints.
pub(crate) fn sweep(work_tree: &Path, now_ms: i64, ttl_ms: i64, dry_run: bool) -> (usize, usize) {
    let root = work_tree.join(".rift/state/search");
    let cutoff = now_ms.saturating_sub(ttl_ms);
    let expired = |path: &Path| {
        std::fs::metadata(path)
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|at| at.duration_since(std::time::UNIX_EPOCH).ok())
            .and_then(|since| i64::try_from(since.as_millis()).ok())
            .is_some_and(|modified_ms| modified_ms < cutoff)
    };
    let mut windows = 0usize;
    if let Ok(entries) = std::fs::read_dir(root.join(WINDOW_DIR)) {
        for entry in entries.flatten() {
            let path = entry.path();
            let named = path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| is_hex(name, 64));
            if !named || !path.is_dir() || !expired(&path) {
                continue;
            }
            windows += 1;
            if !dry_run {
                let _ = std::fs::remove_dir_all(&path);
            }
        }
    }
    let mut answers = 0usize;
    if let Ok(entries) = std::fs::read_dir(root.join(ANSWERS_DIR)) {
        for entry in entries.flatten() {
            let path = entry.path();
            let named = path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| is_hex(name, 32));
            if !named || !path.is_file() || !expired(&path) {
                continue;
            }
            answers += 1;
            if !dry_run {
                let _ = std::fs::remove_file(&path);
            }
        }
    }
    (windows, answers)
}

/// Whether `name` is exactly `len` lowercase hex digits — the one shape this
/// module writes a path component in.
fn is_hex(name: &str, len: usize) -> bool {
    name.len() == len
        && name
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use rift_recall_policy::Mode;
    use rift_shadow::hooks::{AdapterRegistry, RawHook};
    use serde_json::json;

    use super::{search_gate, GrepCap, SearchAnswer, SearchOutcome};

    /// The family this lever exists for, and the levers as this repository
    /// ships them for it.
    fn levers() -> rift_recall_policy::Effective {
        rift_recall_policy::resolve(
            &rift_recall_policy::ServePolicy::default(),
            "claude-code",
            crate::recall::family_capability(&rift_core::AgentId::CLAUDE_CODE),
        )
    }

    /// One hook as a client sends it, in the turn `turn`.
    fn hook(verb: &str, turn: u32, payload: serde_json::Value) -> RawHook {
        RawHook {
            agent: rift_core::AgentId::CLAUDE_CODE,
            verb: verb.to_owned(),
            session: "s-1".to_owned(),
            turn: Some(turn),
            occurred_at_ms: 1_700_000_000_000,
            payload,
        }
    }

    /// A content search, spelled with the argument names the family's pinned
    /// row and its own published page name.
    fn grep(input: serde_json::Value) -> serde_json::Value {
        json!({ "tool_name": "Grep", "tool_input": input })
    }

    /// One call through the gate, against a checkout the test owns.
    fn gate(
        raw: &RawHook,
        work_tree: &Path,
        effective: &rift_recall_policy::Effective,
    ) -> SearchOutcome {
        gate_under(
            raw,
            work_tree,
            &rift_recall_policy::ServePolicy::default(),
            effective,
        )
    }

    /// The same, under a repository policy of the test's own — the deny list
    /// is the one thing about a name search that a default cannot express.
    fn gate_under(
        raw: &RawHook,
        work_tree: &Path,
        policy: &rift_recall_policy::ServePolicy,
        effective: &rift_recall_policy::Effective,
    ) -> SearchOutcome {
        search_gate(
            &crate::hooktime::HookTimer::new("agent-hook", false),
            &AdapterRegistry::with_builtin(),
            raw,
            &work_tree.join(".git"),
            work_tree,
            policy,
            effective,
        )
    }

    /// The amendment an outcome carries, or a failure naming what it carried
    /// instead: every test below is about one of the two kinds of answer, and
    /// reading one as the other would pass for the wrong reason.
    fn cap_of(outcome: &SearchOutcome) -> GrepCap {
        match outcome.answer.clone() {
            Some(SearchAnswer::Cap(cap)) => cap,
            other => panic!("a bound was owed, not {other:?}"),
        }
    }

    /// The whole response the client would receive for this amendment.
    fn response(cap: &GrepCap) -> serde_json::Value {
        rift_agent::respond::gate(
            &rift_core::AgentId::CLAUDE_CODE,
            "PreToolUse",
            &rift_agent::respond::GateDecision::Rewrite(
                rift_agent::respond::Rewrite::GrepHeadLimit {
                    tool_input: &cap.tool_input,
                    head_limit: cap.head_limit,
                },
            ),
        )
        .expect("the family's pinned row documents this amendment")
    }

    /// The accounting rows this hook journaled for the detached worker.
    fn journaled(work_tree: &Path) -> Vec<crate::agent_queue::AgentJob> {
        crate::agent_queue::jobs(&work_tree.join(".git"))
            .into_iter()
            .map(|(_, job)| job)
            .collect()
    }

    /// Files under this window's directory whose names begin with `prefix` —
    /// the re-query figure's one definition, counted the way a reader outside
    /// this binary counts it.
    fn markers(work_tree: &Path, prefix: &str) -> usize {
        let dir = work_tree.join(".rift/state/search/w");
        let Ok(windows) = std::fs::read_dir(&dir) else {
            return 0;
        };
        windows
            .flatten()
            .filter_map(|window| std::fs::read_dir(window.path()).ok())
            .flatten()
            .flatten()
            .filter(|entry| entry.file_name().to_string_lossy().starts_with(prefix))
            .count()
    }

    /// ★ A content search that declared no result cap is given one — and the
    /// call the tool runs is otherwise the call the agent wrote, field for
    /// field, in the one channel the family's pinned rows allow.
    #[test]
    fn a_grep_without_head_limit_gets_one_and_nothing_else_changes() {
        let tmp = tempfile::tempdir().expect("a work tree");
        let input = json!({
            "pattern": "fn main",
            "path": "crates",
            "glob": "**/*.rs",
            "type": "rust",
            "output_mode": "content",
            "multiline": true,
        });
        let call = hook("PreToolUse", 1, grep(input.clone()));

        let outcome = gate(&call, tmp.path(), &levers());
        let cap = cap_of(&outcome);
        assert_eq!(cap.head_limit, 40, "the bound this repository ships");
        assert_eq!(cap.tool_input, input, "the call travels whole");

        let response = response(&cap);
        assert_eq!(
            response.pointer("/hookSpecificOutput/permissionDecision"),
            Some(&serde_json::Value::from("allow")),
            "an amendment declines nothing: {response}"
        );
        let amended = response
            .pointer("/hookSpecificOutput/updatedInput")
            .and_then(serde_json::Value::as_object)
            .expect("the amended call");
        let mut expected = input.as_object().expect("the call").clone();
        expected.insert("head_limit".to_owned(), serde_json::Value::from(40));
        assert_eq!(amended, &expected, "one field added and nothing else moved");
        assert!(
            !outcome.journaled,
            "an amendment is a request to the client, and books nothing until the result proves it"
        );
        assert!(journaled(tmp.path()).is_empty());
    }

    /// ★ A caller that named its own bound chose one, and an explicit zero is
    /// as much a choice as any other number.
    #[test]
    fn an_explicit_head_limit_is_never_touched() {
        let tmp = tempfile::tempdir().expect("a work tree");
        for chosen in [json!(0), json!(5), json!(1000)] {
            let call = hook(
                "PreToolUse",
                1,
                grep(json!({ "pattern": "needle", "head_limit": chosen })),
            );
            let outcome = gate(&call, tmp.path(), &levers());
            assert_eq!(
                outcome,
                SearchOutcome::default(),
                "head_limit {chosen} was the caller's own choice"
            );
        }
        assert_eq!(
            markers(tmp.path(), "c-"),
            0,
            "a call this gate leaves alone is not a cap it made"
        );
    }

    /// ★ The escape hatch: the same search, asked again in this window, runs
    /// with no bound at all — with or without the offset that pages through
    /// the results the first one truncated.
    #[test]
    fn a_same_key_follow_up_grep_runs_uncapped() {
        let tmp = tempfile::tempdir().expect("a work tree");
        let input = json!({ "pattern": "needle", "path": ".", "output_mode": "content" });
        let first = hook("PreToolUse", 1, grep(input.clone()));
        assert!(
            gate(&first, tmp.path(), &levers()).answer.is_some(),
            "the first is bounded"
        );

        let again = hook("PreToolUse", 2, grep(input.clone()));
        assert_eq!(
            gate(&again, tmp.path(), &levers()).answer,
            None,
            "the same search runs in full"
        );

        let mut paged = input.as_object().expect("the call").clone();
        paged.insert("offset".to_owned(), serde_json::Value::from(40));
        let onward = hook("PreToolUse", 2, grep(serde_json::Value::Object(paged)));
        assert_eq!(
            gate(&onward, tmp.path(), &levers()).answer,
            None,
            "paging through one search's results is that same search"
        );

        // A different search is a different key, and gets its own one bound.
        let other = hook("PreToolUse", 2, grep(json!({ "pattern": "other" })));
        assert!(
            gate(&other, tmp.path(), &levers()).answer.is_some(),
            "another search is another decision"
        );
    }

    /// A cap another hook's amendment displaced is recorded, and it is the end
    /// of this window's amendments — never the start of a decline. A content
    /// search is never substituted, so there is nothing to decline it in
    /// favour of.
    #[test]
    fn a_dropped_head_limit_is_recorded_and_never_denied() {
        let tmp = tempfile::tempdir().expect("a work tree");
        let input = json!({ "pattern": "needle", "output_mode": "content" });
        let call = hook("PreToolUse", 1, grep(input.clone()));
        assert!(gate(&call, tmp.path(), &levers()).answer.is_some());

        // The result names the call as the agent wrote it: the bound Rift
        // asked for never reached the tool.
        let result = hook("PostToolUse", 1, grep(input.clone()));
        let outcome = gate(&result, tmp.path(), &levers());
        assert_eq!(
            outcome,
            SearchOutcome::default(),
            "a dropped bound books nothing, and answers nothing"
        );
        assert!(
            journaled(tmp.path()).is_empty(),
            "a search that ran in full saved nothing and cost nothing"
        );
        assert!(
            tmp.path()
                .join(".rift/state/search/w")
                .read_dir()
                .expect("the window")
                .flatten()
                .any(|w| w.path().join("dropped").exists()),
            "the drop is recorded"
        );

        // And the window stands down: the next search runs as written.
        let next = hook("PreToolUse", 2, grep(json!({ "pattern": "another" })));
        assert_eq!(gate(&next, tmp.path(), &levers()).answer, None);
    }

    /// The bound that reached the tool is the one moment this lever books a
    /// row: the executed call carries it, so the cap can be said to have
    /// happened. Once — a second result for one amendment claims nothing.
    #[test]
    fn a_carried_head_limit_books_the_row_the_result_proves() {
        let tmp = tempfile::tempdir().expect("a work tree");
        let input = json!({ "pattern": "needle", "output_mode": "content" });
        let call = hook("PreToolUse", 1, grep(input.clone()));
        assert!(gate(&call, tmp.path(), &levers()).answer.is_some());

        let mut executed = input.as_object().expect("the call").clone();
        executed.insert("head_limit".to_owned(), serde_json::Value::from(40));
        let result = hook("PostToolUse", 1, grep(serde_json::Value::Object(executed)));
        assert!(
            gate(&result, tmp.path(), &levers()).journaled,
            "the executed call carried the bound"
        );
        let rows = journaled(tmp.path());
        assert_eq!(rows.len(), 1);
        let crate::agent_queue::JobKind::ServeAudit(audit) = &rows[0].kind else {
            panic!("a serving decision: {:?}", rows[0].kind);
        };
        assert_eq!(
            audit.decision,
            crate::agent_queue::ServeAuditKind::GrepCapped
        );
        assert!(
            audit.basis.is_none() && audit.frame.is_empty(),
            "a search is about no file, and nothing was put in front of a model"
        );

        let again = hook("PostToolUse", 1, grep(input));
        assert!(
            !gate(&again, tmp.path(), &levers()).journaled,
            "one amendment is answered once"
        );
    }

    /// The honesty figure: a search bounded in this turn and asked again in
    /// the same turn is a re-query, counted once, in the one place the fixture
    /// reads it from.
    #[test]
    fn a_capped_grep_followed_by_the_same_key_counts_toward_the_requery_rate() {
        let tmp = tempfile::tempdir().expect("a work tree");
        let input = json!({ "pattern": "needle", "path": "." });
        assert!(gate(
            &hook("PreToolUse", 3, grep(input.clone())),
            tmp.path(),
            &levers()
        )
        .answer
        .is_some());
        assert_eq!(markers(tmp.path(), "c-"), 1);
        assert_eq!(markers(tmp.path(), "q-"), 0, "nothing asked again yet");

        // Same turn, same search: the model came back for the rest.
        for _ in 0..2 {
            assert_eq!(
                gate(
                    &hook("PreToolUse", 3, grep(input.clone())),
                    tmp.path(),
                    &levers()
                )
                .answer,
                None
            );
        }
        assert_eq!(
            markers(tmp.path(), "q-"),
            1,
            "how many capped searches were asked again, not how many times"
        );

        // A later turn is ordinary work, not the re-query this rate is about.
        let other = json!({ "pattern": "elsewhere" });
        assert!(gate(
            &hook("PreToolUse", 4, grep(other.clone())),
            tmp.path(),
            &levers()
        )
        .answer
        .is_some());
        assert_eq!(
            gate(&hook("PreToolUse", 5, grep(other)), tmp.path(), &levers()).answer,
            None
        );
        assert_eq!(markers(tmp.path(), "c-"), 2);
        assert_eq!(markers(tmp.path(), "q-"), 1);
    }

    /// A repository that asked to be told rather than amended is told: every
    /// check runs, the row sizes what the lever would have worked on, and the
    /// call reaches the tool exactly as the agent wrote it.
    #[test]
    fn an_observed_family_records_the_cap_it_did_not_make() {
        let tmp = tempfile::tempdir().expect("a work tree");
        let observed = rift_recall_policy::Effective {
            grep_cap: Mode::Observe,
            ..levers()
        };
        let input = json!({ "pattern": "needle", "path": "." });
        let outcome = gate(
            &hook("PreToolUse", 1, grep(input.clone())),
            tmp.path(),
            &observed,
        );
        assert_eq!(outcome.answer, None, "nothing was amended");
        assert!(outcome.journaled, "and the decision was recorded");
        let rows = journaled(tmp.path());
        assert_eq!(rows.len(), 1);

        // The same search still runs unbounded afterwards, and books no second
        // row: what observe records is the decision enforce would have made.
        let again = gate(&hook("PreToolUse", 1, grep(input)), tmp.path(), &observed);
        assert_eq!(again, SearchOutcome::default());
        assert_eq!(journaled(tmp.path()).len(), 1);
    }

    /// Every other tool, every other verb and a family whose pinned rows
    /// document no amendment are left alone. A name search is not in the list
    /// because it is the other arm's business, and is answered rather than
    /// amended — never the reverse.
    #[test]
    fn nothing_but_a_content_search_on_an_amendable_family_is_touched() {
        let tmp = tempfile::tempdir().expect("a work tree");
        let untouched = [
            json!({ "tool_name": "Read", "tool_input": { "file_path": "a.txt" } }),
            json!({ "tool_name": "Bash", "tool_input": { "command": "rg needle src" } }),
            json!({ "tool_name": "Grep" }),
            json!({ "tool_name": "Grep", "tool_input": {} }),
        ];
        for payload in untouched {
            let call = hook("PreToolUse", 1, payload.clone());
            assert_eq!(
                gate(&call, tmp.path(), &levers()),
                SearchOutcome::default(),
                "{payload}"
            );
        }
        for verb in ["PreCompact", "SessionStart", "Stop"] {
            let call = hook(verb, 1, grep(json!({ "pattern": "needle" })));
            assert_eq!(gate(&call, tmp.path(), &levers()), SearchOutcome::default());
        }
        // A family that documents no amendment resolves the lever to `off`,
        // and this gate never reaches a marker for it.
        let codex = rift_recall_policy::resolve(
            &rift_recall_policy::ServePolicy::default(),
            "codex",
            crate::recall::family_capability(&rift_core::AgentId::CODEX),
        );
        assert_eq!(codex.grep_cap, Mode::Off);
        let call = RawHook {
            agent: rift_core::AgentId::CODEX,
            ..hook("PreToolUse", 1, grep(json!({ "pattern": "needle" })))
        };
        assert_eq!(gate(&call, tmp.path(), &codex), SearchOutcome::default());
        assert_eq!(markers(tmp.path(), "c-"), 0);
    }

    /// **An arming outlived by its call books nothing.**
    ///
    /// The key is the search without its paging arguments, so a bound this
    /// window asked for and never saw answered — a rejected call, a session
    /// that ended — would otherwise be consumed by the same search re-issued
    /// with a bound the agent chose for itself.
    #[test]
    fn a_stale_arming_with_another_value_books_nothing() {
        let tmp = tempfile::tempdir().expect("a work tree");
        let input = json!({ "pattern": "needle", "path": "." });
        let capped = gate(
            &hook("PreToolUse", 1, grep(input.clone())),
            tmp.path(),
            &levers(),
        );
        assert_eq!(cap_of(&capped).head_limit, 40);

        // The agent's own bound, on the same search, in the same window.
        let mut chosen = input.as_object().expect("the call").clone();
        chosen.insert("head_limit".to_owned(), json!(200));
        let executed = json!({
            "tool_name": "Grep",
            "tool_input": chosen,
            "tool_response": { "mode": "content", "numFiles": 1 },
        });
        let verified = gate(&hook("PostToolUse", 1, executed), tmp.path(), &levers());
        assert!(
            !verified.journaled,
            "a bound Rift did not ask for is not a cap Rift made"
        );
        assert!(journaled(tmp.path()).is_empty());
    }

    // ------------------------------------------ answering a name search ---

    /// A checkout with an index over `files`, built the way the detached
    /// worker builds it.
    ///
    /// The skeleton comes first and the walk second, on purpose: the index is
    /// keyed by the newest directory modification time it saw, and a directory
    /// this hook is about to create — the journal under `.git`, the ledgers
    /// under `.rift` — would move that key out from under the index the test
    /// just built. A real checkout has both long before an agent runs.
    #[cfg(feature = "search")]
    fn indexed(files: &[(&str, &str)]) -> tempfile::TempDir {
        let tmp = tempfile::tempdir().expect("a work tree");
        let root = tmp.path();
        for dir in [".git/rift-agent-queue", ".rift/state/reads"] {
            std::fs::create_dir_all(root.join(dir)).expect("the skeleton");
        }
        for (rel, body) in files {
            let path = root.join(rel);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("a directory");
            }
            std::fs::write(path, body).expect("a file");
        }
        rift_recall_search::rebuild(root, &rift_recall_search::head_oid(root), &[])
            .expect("the worker's own pass");
        tmp
    }

    /// One name search, as each family spells it.
    #[cfg(feature = "search")]
    fn glob(pattern: &str) -> serde_json::Value {
        json!({ "tool_name": "Glob", "tool_input": { "pattern": pattern } })
    }

    /// The answer an outcome carries, or a failure naming what it carried.
    #[cfg(feature = "search")]
    fn frame_of(outcome: &SearchOutcome) -> String {
        match outcome.answer.clone() {
            Some(SearchAnswer::Listing(frame)) => frame,
            other => panic!("an answer was owed, not {other:?}"),
        }
    }

    /// ★ **A name search is answered once, with the ranked head and the total
    /// it is a head of — and the repeat runs the search in full.**
    ///
    /// The three claims are one lever: the answer is a strict subset that says
    /// so, the escape hatch is the identical call, and the escape hatch is
    /// what makes hiding the tail safe.
    #[cfg(feature = "search")]
    #[test]
    fn a_glob_is_answered_once_with_the_top_n_and_the_total_and_the_repeat_runs_in_full() {
        let files: Vec<(String, String)> = (0..60)
            .map(|n| {
                (
                    format!("src/module{n}/handler.rs"),
                    format!("fn f{n}() {{}}\n"),
                )
            })
            .chain([("README.md".to_owned(), "prose\n".to_owned())])
            .collect();
        let borrowed: Vec<(&str, &str)> = files
            .iter()
            .map(|(p, b)| (p.as_str(), b.as_str()))
            .collect();
        let tmp = indexed(&borrowed);
        let call = hook("PreToolUse", 1, glob("**/*.rs"));

        let outcome = gate(&call, tmp.path(), &levers());
        let frame = frame_of(&outcome);
        assert!(
            frame.starts_with("[rift] Search answered: 8 of 60 paths matching"),
            "the head, and the total it is a head of: {frame}"
        );
        assert_eq!(
            frame.lines().filter(|l| l.starts_with("- ")).count(),
            8,
            "the operator's top-n, and no more: {frame}"
        );
        assert!(
            frame.contains("Repeat this exact call for the full list."),
            "the escape hatch is stated: {frame}"
        );
        assert!(
            !frame.contains("README.md"),
            "only what the pattern matched: {frame}"
        );
        assert!(
            outcome.journaled,
            "the decision is journaled for the worker"
        );
        assert_eq!(markers(tmp.path(), "g-"), 1, "one answer, one marker");

        // The whole response the client receives: a decision with the reason
        // in the field this family's pinned row names, and no amendment.
        let response = rift_agent::respond::gate(
            &rift_core::AgentId::CLAUDE_CODE,
            "PreToolUse",
            &rift_agent::respond::GateDecision::Deny { reason: &frame },
        )
        .expect("the family's confirmed reason channel");
        assert_eq!(
            response.pointer("/hookSpecificOutput/permissionDecision"),
            Some(&serde_json::Value::from("deny")),
            "{response}"
        );
        assert_eq!(
            response.pointer("/hookSpecificOutput/permissionDecisionReason"),
            Some(&serde_json::Value::from(frame.as_str())),
            "the frame travels whole: {response}"
        );
        assert!(
            response
                .pointer("/hookSpecificOutput/updatedInput")
                .is_none(),
            "a decline carries no amendment: {response}"
        );

        // The identical call, in the same window: it runs, exactly as written.
        let again = gate(&call, tmp.path(), &levers());
        assert_eq!(again.answer, None, "the repeat runs the search in full");
        assert_eq!(markers(tmp.path(), "g-"), 1, "and no second answer");
        assert_eq!(
            markers(tmp.path(), "r-"),
            1,
            "a repeat inside the answered turn is counted, and apart from a bound's"
        );
        assert_eq!(
            markers(tmp.path(), "q-"),
            0,
            "which is not the bounded search's re-query rate"
        );
        // A different conversation is a different window, and is answered.
        let elsewhere = RawHook {
            session: "s-2".to_owned(),
            ..hook("PreToolUse", 1, glob("**/*.rs"))
        };
        assert!(matches!(
            gate(&elsewhere, tmp.path(), &levers()).answer,
            Some(SearchAnswer::Listing(_))
        ));
    }

    /// ★ **A content search is never answered from an index.**
    ///
    /// The name search and the content search reach this gate through the same
    /// door and leave through different ones: one is answered, the other is
    /// bounded, and neither is ever given the other's treatment.
    #[cfg(feature = "search")]
    #[test]
    fn grep_is_never_substituted() {
        // Forty files, so an answer would beat the listing it stands in for:
        // on a one-file tree the cost guard stands every answer down and the
        // assertions below could not fail for the defect they exist for.
        let files: Vec<(String, String)> = (0..40)
            .map(|n| {
                (
                    format!("src/module{n}/handler.rs"),
                    format!("fn needle{n}() {{}}\n"),
                )
            })
            .chain([("src/a.rs".to_owned(), "fn needle() {}\n".to_owned())])
            .collect();
        let borrowed: Vec<(&str, &str)> = files
            .iter()
            .map(|(p, b)| (p.as_str(), b.as_str()))
            .collect();
        let tmp = indexed(&borrowed);
        let content = [
            grep(json!({ "pattern": "needle", "path": "." })),
            // The same search on the shell channel, under the published
            // grammar: a `grep` on one file is a read of lines, never a
            // listing, and this gate answers no part of it.
            json!({ "tool_name": "Bash", "tool_input": { "command": "grep needle src/a.rs" } }),
            json!({ "tool_name": "Bash", "tool_input": { "command": "rg needle src" } }),
        ];
        for payload in content {
            let outcome = gate(
                &hook("PreToolUse", 1, payload.clone()),
                tmp.path(),
                &levers(),
            );
            assert!(
                !matches!(outcome.answer, Some(SearchAnswer::Listing(_))),
                "a content search is never answered by a list of names: {payload}"
            );
        }
        assert_eq!(markers(tmp.path(), "g-"), 0, "and no answer was claimed");
    }

    /// ★ **A family whose reason channel no recorded run has confirmed is
    /// never answered — the search runs, and the decision is recorded.**
    #[cfg(feature = "search")]
    #[test]
    fn an_unconfirmed_family_observes() {
        let files: Vec<(String, String)> = (0..40)
            .map(|n| {
                (
                    format!("src/module{n}/handler.rs"),
                    "fn f() {}\n".to_owned(),
                )
            })
            .collect();
        let borrowed: Vec<(&str, &str)> = files
            .iter()
            .map(|(p, b)| (p.as_str(), b.as_str()))
            .collect();
        let tmp = indexed(&borrowed);
        let unconfirmed = rift_recall_policy::Effective {
            search_answer: Mode::Observe,
            search_answer_cap: Some(rift_recall_policy::CapReason::UnconfirmedReasonChannel),
            ..levers()
        };
        let call = hook("PreToolUse", 1, glob("**/*.rs"));

        let outcome = gate(&call, tmp.path(), &unconfirmed);
        assert_eq!(outcome.answer, None, "nothing was put to the model");
        assert!(outcome.journaled, "and the decision was recorded");
        let rows = journaled(tmp.path());
        assert_eq!(rows.len(), 1);
        let crate::agent_queue::JobKind::ServeAudit(audit) = &rows[0].kind else {
            panic!("an accounting row");
        };
        assert_eq!(
            audit.decision,
            crate::agent_queue::ServeAuditKind::SearchAnswered
        );
        assert!(
            audit.frame.is_empty(),
            "a sentence nobody read costs nothing"
        );
        assert_eq!(audit.declared_avoided, Some(0), "and claims nothing");
        assert_eq!(audit.tag.as_deref(), Some("answer_observed"));
        assert!(
            !std::fs::exists(tmp.path().join(".rift/state/search/answers")).unwrap_or(false),
            "an observed decision leaves the worker no listing to price"
        );

        // The same search still runs afterwards, and books no second row.
        let again = gate(&call, tmp.path(), &unconfirmed);
        assert_eq!(again.answer, None);
        assert_eq!(journaled(tmp.path()).len(), 1);
    }

    /// **A cold index is a search that runs, and a walk the worker is asked
    /// for.** A hook never walks a tree while an agent waits on it.
    #[cfg(feature = "search")]
    #[test]
    fn a_cold_index_lets_the_glob_run_and_asks_for_a_rebuild() {
        let tmp = tempfile::tempdir().expect("a work tree");
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src")).expect("a directory");
        std::fs::write(root.join("src/a.rs"), "fn a() {}\n").expect("a file");

        let outcome = gate(&hook("PreToolUse", 1, glob("**/*.rs")), root, &levers());
        assert_eq!(outcome.answer, None, "the search runs as it was written");
        assert!(outcome.journaled, "and the walk is asked of the worker");
        assert!(
            journaled(root)
                .iter()
                .any(|job| matches!(job.kind, crate::agent_queue::JobKind::SearchIndex)),
            "the request is for the index and not for an accounting row"
        );
        assert!(
            !rift_recall_search::index_path(root).exists(),
            "and nothing was walked here"
        );
        assert_eq!(markers(root, "g-"), 0, "no answer was claimed either");
    }

    /// **A path this repository refuses to serve is neither named in an answer
    /// nor priced into what the answer claims.**
    ///
    /// The two halves are one screen, and the order matters: the listing that
    /// may be priced is decided before the frame is written, so a denied path
    /// cannot be counted into the saving and then left out of the sentence. It
    /// is also never written into this member's own run state on the way to the
    /// worker — the paths left there are the ones an answer would have said.
    #[cfg(feature = "search")]
    #[test]
    fn a_denied_path_is_neither_named_nor_priced() {
        let mut files: Vec<(String, String)> = (0..28)
            .map(|n| {
                (
                    format!("src/module{n}/handler.rs"),
                    "fn a() {}\n".to_owned(),
                )
            })
            .collect();
        files.extend((0..12).map(|n| {
            (
                format!("internal/plan{n}/notes.rs"),
                "fn b() {}\n".to_owned(),
            )
        }));
        let borrowed: Vec<(&str, &str)> = files
            .iter()
            .map(|(p, b)| (p.as_str(), b.as_str()))
            .collect();
        let tmp = indexed(&borrowed);
        let policy = rift_recall_policy::ServePolicy {
            denied_paths: vec!["internal/**".to_owned()],
            ..rift_recall_policy::ServePolicy::default()
        };
        let outcome = gate_under(
            &hook("PreToolUse", 1, glob("**/*.rs")),
            tmp.path(),
            &policy,
            &levers(),
        );
        let frame = frame_of(&outcome);
        assert!(
            !frame.contains("internal/"),
            "a denied path is not a name an answer says: {frame}"
        );
        assert!(
            frame.starts_with("[rift] Search answered: 8 of 40 paths matching"),
            "and the total is still what matched, so the count is not a claim \
             about the tree: {frame}"
        );

        let priced = std::fs::read_dir(tmp.path().join(".rift/state/search/answers"))
            .expect("one answer was left for the worker to price")
            .flatten()
            .map(|entry| std::fs::read_to_string(entry.path()).expect("the paths"))
            .collect::<String>();
        let paths: Vec<&str> = priced.lines().collect();
        assert_eq!(
            paths.len(),
            28,
            "the twelve the screen refused are not in the pool: {paths:?}"
        );
        assert!(
            !priced.contains("internal/"),
            "and their names were never written down: {priced}"
        );
    }

    /// ★ **A path this repository calls sensitive is neither named in an
    /// answer nor priced into what the answer claims — including one the
    /// saved index still holds.**
    ///
    /// The index is built without the sensitive paths, which closes the case
    /// where the list was already there. It does not close the other one:
    /// adding a glob to `.rift/config.toml` moves no directory modification
    /// time and no HEAD, so the saved index stays fresh by its own key and
    /// still holds a path that has since become sensitive. The screen that
    /// answers that runs at this call site, before the price and before the
    /// frame — so the path is left out of the pool as well as out of the
    /// sentence, and is never written into this member's run state on the way
    /// to the worker.
    #[cfg(feature = "search")]
    #[test]
    fn a_path_that_became_sensitive_is_neither_named_nor_priced() {
        let mut files: Vec<(String, String)> = (0..28)
            .map(|n| {
                (
                    format!("src/module{n}/handler.rs"),
                    "fn a() {}\n".to_owned(),
                )
            })
            .collect();
        files
            .extend((0..12).map(|n| (format!("vault/plan{n}/notes.rs"), "fn b() {}\n".to_owned())));
        // The policy file exists before the walk, so that adding the glob
        // below writes into it rather than creating it: creating a file would
        // move `.rift`'s own modification time, which the index IS keyed by,
        // and the test would then be measuring a stale index instead of a
        // stale sensitive list.
        let plain = "schema_version = 1\n\n[project]\nname = \"t\"\n";
        files.push((".rift/config.toml".to_owned(), plain.to_owned()));
        let borrowed: Vec<(&str, &str)> = files
            .iter()
            .map(|(p, b)| (p.as_str(), b.as_str()))
            .collect();
        let tmp = indexed(&borrowed);
        // Written AFTER the index, which is the whole point: the index's own
        // key takes no sensitive glob, so it is still fresh and still holds
        // every one of the forty paths.
        std::fs::write(
            tmp.path().join(".rift/config.toml"),
            format!("{plain}\n[paths]\nsensitive_paths = [\"vault/**\"]\n"),
        )
        .expect("the repository's own policy");

        let outcome = gate(
            &hook("PreToolUse", 1, glob("**/*.rs")),
            tmp.path(),
            &levers(),
        );
        let frame = frame_of(&outcome);
        assert!(
            !frame.contains("vault/"),
            "a sensitive path is not a name an answer says: {frame}"
        );
        assert!(
            frame.starts_with("[rift] Search answered: 8 of 40 paths matching"),
            "and the total is still what matched: {frame}"
        );

        let priced = std::fs::read_dir(tmp.path().join(".rift/state/search/answers"))
            .expect("one answer was left for the worker to price")
            .flatten()
            .map(|entry| std::fs::read_to_string(entry.path()).expect("the paths"))
            .collect::<String>();
        let paths: Vec<&str> = priced.lines().collect();
        assert_eq!(
            paths.len(),
            28,
            "the twelve the screen refused are not in the pool: {paths:?}"
        );
        assert!(
            !priced.contains("vault/"),
            "and their names were never written down: {priced}"
        );
    }

    /// **An answer carries names and a count, and never a byte of a file.**
    #[cfg(feature = "search")]
    #[test]
    fn a_listing_answer_never_carries_file_contents() {
        let secretish = "SUPERSECRETBODYTEXT";
        let body = format!("fn a() {{ \"{secretish}\" }}\n");
        let mut files: Vec<(String, String)> = (0..40)
            .map(|n| (format!("src/module{n}/handler.rs"), body.clone()))
            .collect();
        files.push(("src/module0/named.rs".to_owned(), body.clone()));
        let borrowed: Vec<(&str, &str)> = files
            .iter()
            .map(|(p, b)| (p.as_str(), b.as_str()))
            .collect();
        let tmp = indexed(&borrowed);
        let frame = frame_of(&gate(
            &hook("PreToolUse", 1, glob("**/*.rs")),
            tmp.path(),
            &levers(),
        ));
        assert!(!frame.contains(secretish), "no file content: {frame}");
        assert!(!frame.contains("fn a()"), "and no line of one: {frame}");
        assert_eq!(
            frame.lines().filter(|l| l.starts_with("- ")).count(),
            8,
            "one line per path and nothing else: {frame}"
        );
        for line in frame.lines().filter(|l| l.starts_with("- ")) {
            assert!(
                line.ends_with("handler.rs") || line.ends_with("named.rs"),
                "a path, whole, and nothing after it: {line}"
            );
        }
    }

    /// **A listing is answered inside the directory it named, and a listing
    /// that named no pattern is not answered at all.**
    ///
    /// A directory listing enumerates directories too, and an index of files
    /// cannot: answering `ls` from one would drop every subdirectory name and
    /// state a total that counted none of them.
    #[cfg(feature = "search")]
    #[test]
    fn a_listing_is_answered_within_its_own_directory_and_only_with_a_pattern() {
        let mut files: Vec<(String, String)> = (0..30)
            .map(|n| {
                (
                    format!("src/deep/nested{n}/leaf.rs"),
                    "fn leaf() {}\n".to_owned(),
                )
            })
            .collect();
        files.push(("tests/c.rs".to_owned(), "fn c() {}\n".to_owned()));
        let borrowed: Vec<(&str, &str)> = files
            .iter()
            .map(|(p, b)| (p.as_str(), b.as_str()))
            .collect();
        let tmp = indexed(&borrowed);
        let cwd = tmp.path().canonicalize().expect("a real path");
        let shell = |command: &str| {
            json!({
                "tool_name": "Bash",
                "tool_input": { "command": command },
                "cwd": cwd.to_str().expect("a utf-8 path"),
            })
        };
        let frame = frame_of(&gate(
            &hook("PreToolUse", 1, shell("find src -name '*.rs'")),
            tmp.path(),
            &levers(),
        ));
        assert!(
            frame.starts_with("[rift] Search answered: 8 of 30 paths matching"),
            "the subtree it named, and nothing outside it: {frame}"
        );
        assert!(
            frame.contains("src/deep/nested0/leaf.rs"),
            "every depth: {frame}"
        );
        assert!(!frame.contains("tests/c.rs"), "and no other: {frame}");

        // The other two families, on the shapes their own pinned rows name:
        // one nests its arguments and takes the pattern as an argument, one
        // spells the same search as a command line.
        let elsewhere = [
            (
                rift_core::AgentId::CODEX,
                json!({
                    "tool_name": "Bash",
                    "tool_input": { "command": "find src -name '*.rs'" },
                    "cwd": cwd.to_str().expect("a utf-8 path"),
                }),
            ),
            (
                rift_core::AgentId::ANTIGRAVITY,
                json!({
                    "toolCall": {
                        "name": "find_by_name",
                        "args": {
                            "SearchDirectory": cwd.join("src").to_str().expect("a utf-8 path"),
                            "Pattern": "*.rs",
                        },
                    },
                }),
            ),
        ];
        for (agent, payload) in elsewhere {
            let call = RawHook {
                agent: agent.clone(),
                session: format!("s-{}", agent.as_str()),
                ..hook("PreToolUse", 1, payload)
            };
            let levers = rift_recall_policy::resolve(
                &rift_recall_policy::ServePolicy::default(),
                agent.as_str(),
                crate::recall::family_capability(&agent),
            );
            let frame = frame_of(&gate(&call, tmp.path(), &levers));
            assert!(
                frame.starts_with("[rift] Search answered: 8 of 30 paths matching"),
                "{} names the same search another way: {frame}",
                agent.as_str()
            );
        }

        for left_alone in ["ls src", "ls -la src", "rg --files src"] {
            let outcome = gate(
                &hook("PreToolUse", 2, shell(left_alone)),
                tmp.path(),
                &levers(),
            );
            assert_eq!(
                outcome.answer, None,
                "a directory's contents are not this index's to state: {left_alone}"
            );
        }
    }

    /// **A listing an answer could not beat is not answered.**
    ///
    /// The frame carries a sentence of its own, so a handful of paths costs
    /// less as a listing than as an answer to one — and a lever that fired
    /// there would spend tokens to save none.
    #[cfg(feature = "search")]
    #[test]
    fn an_answer_that_would_cost_more_than_the_listing_is_not_made() {
        let tmp = indexed(&[("src/a.rs", "fn a() {}\n"), ("src/b.rs", "fn b() {}\n")]);
        let outcome = gate(
            &hook("PreToolUse", 1, glob("**/*.rs")),
            tmp.path(),
            &levers(),
        );
        assert_eq!(
            outcome.answer, None,
            "two paths are cheaper said than framed"
        );
        assert!(
            !outcome.journaled,
            "and nothing is claimed for not saying it"
        );
        assert_eq!(markers(tmp.path(), "g-"), 0, "no answer was spent either");
    }

    /// **A glob that scopes itself with an argument no artifact pins is not a
    /// glob this gate understands**, and runs exactly as it was written.
    #[cfg(feature = "search")]
    #[test]
    fn a_glob_carrying_an_unpinned_argument_is_left_alone() {
        let tmp = indexed(&[("src/a.rs", "fn a() {}\n")]);
        let scoped = json!({
            "tool_name": "Glob",
            "tool_input": { "pattern": "**/*.rs", "path": "/somewhere/else" },
        });
        let outcome = gate(&hook("PreToolUse", 1, scoped), tmp.path(), &levers());
        assert_eq!(outcome.answer, None, "the call this gate cannot read");
        assert_eq!(markers(tmp.path(), "g-"), 0);
    }

    /// **What an ended conversation left behind is collected by its own age,
    /// and a live one keeps what it needs.**
    #[cfg(feature = "search")]
    #[test]
    fn clean_removes_expired_search_windows() {
        let files: Vec<(String, String)> = (0..40)
            .map(|n| {
                (
                    format!("src/module{n}/handler.rs"),
                    "fn f() {}\n".to_owned(),
                )
            })
            .collect();
        let borrowed: Vec<(&str, &str)> = files
            .iter()
            .map(|(p, b)| (p.as_str(), b.as_str()))
            .collect();
        let tmp = indexed(&borrowed);
        let root = tmp.path();
        assert!(matches!(
            gate(&hook("PreToolUse", 1, glob("**/*.rs")), root, &levers()).answer,
            Some(SearchAnswer::Listing(_))
        ));
        let answers = root.join(".rift/state/search/answers");
        assert_eq!(markers(root, "g-"), 1);
        assert_eq!(answers.read_dir().expect("the paths priced").count(), 1);

        let now = 1_800_000_000_000_i64;
        let day = 86_400_000_i64;
        // A conversation younger than the window keeps everything: dropping a
        // live marker costs a second answer in a conversation still open.
        assert_eq!(super::sweep(root, now, now, false), (0, 0));
        assert_eq!(markers(root, "g-"), 1);

        // One that ended before the cutoff loses both, and a dry run says so
        // without doing it.
        assert_eq!(super::sweep(root, now, day, true), (1, 1));
        assert_eq!(markers(root, "g-"), 1, "a dry run removes nothing");
        assert_eq!(super::sweep(root, now, day, false), (1, 1));
        assert_eq!(markers(root, "g-"), 0);
        assert_eq!(answers.read_dir().expect("the directory stays").count(), 0);

        // Nothing this module did not write is touched, however it got there.
        let windows = root.join(".rift/state/search/w");
        std::fs::create_dir_all(windows.join("not-a-window")).expect("a directory");
        std::fs::write(answers.join("not-an-answer"), b"x").expect("a file");
        assert_eq!(super::sweep(root, now, day, false), (0, 0));
        assert!(windows.join("not-a-window").is_dir());
        assert!(answers.join("not-an-answer").is_file());
    }
}
