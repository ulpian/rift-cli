//! The pull verb: the changed lines of one file against the revision the
//! caller holds — and, for a caller with no basis, the older ranked shape.
//!
//! The pull is a pipeline of named stages, each with its own way of not
//! having an answer: resolve the two revisions, fetch their bytes, diff them,
//! window the changed places, resolve the story, compose. An early exit is an
//! answer rather than a failure, and it travels as the `Err` half of a
//! `Result<_, Answer>` so the stages compose with `?` and are joined in one
//! place — the one failure this verb must never have is serving one
//! revision's lines under another revision's numbers, so every non-answer is
//! **named** and none is silence or a guess.

use std::collections::BTreeMap;

use rift_recall_events::MemdEvent;
use rift_recall_ledger::candidates::{load_candidates_with_scope, LoadedCandidate};
use rift_recall_ledger::provenance::{commit_oids, hunk_origins, origin_of, HunkOrigin};
use rift_recall_policy::TurnBudget;
use rift_recall_substrate::SubstrateReader;
use rift_recall_types::{OmitReason, ProvenanceOracle, Trust};
use rift_recall_validity::trust::serialize_untrusted;
use rift_recall_validity::{
    envelope_for, resolve, Budget, MemoryDigestStore, SeenStatus, SummaryStore, Validity,
    WorktreeSnapshot,
};
use rift_store::{CanonicalStore, Row};
use serde_json::{json, Value};

use super::{fit_with_provenance, internal, manifest_cap, targeted_pull, HUNK_RANGES_SQL};
use crate::envelope::{envelope_view, manifest_event, omit_tag, provenance_trust};
use crate::held_diff::{self, Basis, HeldDiff, RedactedDiff};
use crate::window::{self, Window};
use crate::{
    basis, provenance, RecallContext, ServeDispatcher, ToolError, ToolName, ToolRequest,
    ToolResponse,
};

/// The per-tool changed-hunk cap (the default ceiling).
pub const MAX_CHANGED_HUNKS: u32 = 8;

/// One answer of the pull verb, before the seam prices it: the document and
/// the gross baseline it stands in for.
struct Answer {
    doc: Value,
    baseline: u32,
}

/// What the handler settled before the pipeline runs: the path the caller
/// named, whether its lines may be served at all, the containment facts every
/// exit owes, and where a delta's story comes from.
struct PullGate<'a> {
    path: &'a str,
    /// The sensitivity rule the pack path applies, on the verb that serves
    /// bytes with no reason attached. A sensitive path's exact source is
    /// reached through the audited `retrieve_exact` — which demands a reason
    /// and books a grant — or not at all; here it degrades to the handle
    /// state the sealed constructor gives it, so the answer still says the
    /// file changed and where, and the lines stay behind.
    exact_allowed: bool,
    containment: Option<&'a Value>,
    oracle: &'a dyn ProvenanceOracle,
}

/// The file the caller named, resolved to the two revisions the verb diffs.
struct Revisions<'a> {
    /// The current revision's row — the most recently touched one.
    row: &'a LoadedCandidate,
    /// What the file is now: the checkout's hash, or the projection's.
    current_hash: String,
    /// The checkout's bytes, when a checkout was there to read.
    current_bytes: Option<Vec<u8>>,
    /// The caller's basis, resolved through the path-bound resolver.
    basis_hash: String,
}

/// What every exit past resolution answers in: the path and its two
/// addresses, the handle and the trust derived from it, and the sealed
/// constructor's inputs.
///
/// The four-state envelope is the only per-artifact result this handler
/// builds, here as everywhere else: no summary store is bound, so the verdict
/// is `Degraded` and the constructor decides between the bytes and a handle
/// on the budget alone.
struct Pull<'a> {
    gate: &'a PullGate<'a>,
    handle: String,
    /// Provenance, never the caller: a tracked file's blob is a code fact, and
    /// anything whose address does not parse as one fails closed to untrusted
    /// — which is served as counts, never as lines.
    trust: Trust,
    validity: Validity,
    basis_hash: String,
    current_hash: String,
}

/// The changed places of the current file, laid out for a reader.
struct Shown {
    lines: Vec<String>,
    windows: Vec<Window>,
    /// THIS ONE FILE, counted by the real tokenizer over the bytes the caller
    /// would otherwise have read. The seam subtracts what was served, so the
    /// claim is literally full minus served.
    baseline: u32,
}

/// The story of THIS change: the checkpoint the caller's revision was
/// recorded in, the one the current revision was, the commit that anchored
/// whichever end the capture facts come from, and — per window — the recorded
/// hunk that established it, when exactly one did.
struct PullStory {
    from: Option<String>,
    to: Option<String>,
    commit: Option<String>,
    origins: Vec<(HunkOrigin, String)>,
    /// The line the story is about is the first CHANGED one, not the first
    /// line shown: a window opens on context, and `rift why` asked about a
    /// context line answers for a line that did not move.
    first_line: Option<u32>,
}

impl ServeDispatcher<'_> {
    /// The pull verb, in its two shapes.
    ///
    /// A caller that names a path and the revision it holds is asking the
    /// question this verb exists for — *what changed in this file since the
    /// bytes I have* — and is answered with the changed lines themselves. A
    /// caller that names neither is asking the older question, which ranks the
    /// candidates and reports the line ranges the projection recorded for the
    /// top one; that shape is untouched, because a caller with no basis has
    /// nothing to diff against and an answer computed against something else
    /// would be an answer to a question nobody asked.
    pub(crate) async fn changed_hunks<S: CanonicalStore, R: SubstrateReader>(
        &self,
        req: &ToolRequest,
        ctx: &RecallContext<'_, S, R>,
        budget: &mut TurnBudget,
    ) -> Result<ToolResponse, ToolError> {
        if targeted_pull(ToolName::ChangedHunks, req) {
            return self.hunks_against_basis(req, ctx, budget).await;
        }
        Self::ranked_hunks(req, ctx, budget).await
    }

    /// The changed lines of one file against the revision the caller holds.
    ///
    /// Every exit that is not the content one is a **named** non-answer:
    /// `unknown_basis` when the caller's hash names no single revision,
    /// `basis_unavailable` when a revision's bytes cannot be reached,
    /// `binary`, `diffuse` when the file moved in more places than a windowed
    /// answer would be smaller than the file for, and `known_unchanged` when
    /// the caller already holds the current bytes.
    ///
    /// The policy screen runs first, before a candidate is loaded or a byte is
    /// read — the same screen every other serve applies. A policy Rift could
    /// not read is not a permissive one: the engine's fail-closed state
    /// carries an EMPTY deny list — it says "nothing is known", not "nothing
    /// is denied" — so a verb that consulted only `is_denied_path` would read
    /// an unparseable `serve.toml` as consent, and this content-bearing verb
    /// would then be the cheapest way around a gate `retrieve_exact` already
    /// closes.
    async fn hunks_against_basis<S: CanonicalStore, R: SubstrateReader>(
        &self,
        req: &ToolRequest,
        ctx: &RecallContext<'_, S, R>,
        budget: &mut TurnBudget,
    ) -> Result<ToolResponse, ToolError> {
        let path = req.target.clone().unwrap_or_default();
        if self.policy.is_fail_closed() || self.policy.is_denied_path(&path) {
            return Ok(Self::refuse_hunks(
                &path,
                OmitReason::PolicyDeniedExact,
                budget,
            ));
        }
        let (loaded, in_scope) =
            load_candidates_with_scope(ctx.store, &ctx.scope, ctx.now, ctx.ancestry)
                .await
                .map_err(internal)?;
        let containment = Self::containment_view(&in_scope, ctx);
        let gate = PullGate {
            path: &path,
            exact_allowed: !self.policy.exact_source_required(&path),
            containment: containment.as_ref(),
            oracle: ctx.provenance,
        };
        let Answer { doc, baseline } = match pull(ctx, req, &gate, &loaded, budget).await {
            Ok(answer) | Err(answer) => answer,
        };
        Ok(Self::respond(
            ToolName::ChangedHunks,
            doc,
            baseline,
            budget,
            manifest_event,
        ))
    }

    /// A refusal on this verb, audited and measured like every other serve. It
    /// names the path the CALLER named — it is the caller's own word, and a
    /// refusal that cannot say which request it refused is not usable.
    fn refuse_hunks(path: &str, reason: OmitReason, budget: &mut TurnBudget) -> ToolResponse {
        let payload = json!({
            "tool": "changed_hunks",
            "path": path,
            "state": "refused",
            "reason": omit_tag(reason),
        });
        let target = path.to_owned();
        let tag = omit_tag(reason).to_owned();
        Self::respond(
            ToolName::ChangedHunks,
            payload,
            0,
            budget,
            move |returned, _| MemdEvent::Refused {
                target,
                reason: tag,
                estimated_tokens_returned: returned,
            },
        )
    }

    /// The ranked shape: the candidates restrict which paths are in play, and
    /// the hunk bytes come from the persisted changed-hunk rows for the top
    /// focus path.
    ///
    /// Containment decides which paths are in play at all, so this response
    /// owes the same self-declaration `known_state` and `diff_since` make: an
    /// empty hunk list because the family's other line was held back reads
    /// exactly like an empty hunk list because nothing changed, and the caller
    /// cannot tell the two apart from the payload. The block is attached to
    /// BOTH exits — the one that found no path is the one that most needs it.
    async fn ranked_hunks<S: CanonicalStore, R: SubstrateReader>(
        req: &ToolRequest,
        ctx: &RecallContext<'_, S, R>,
        budget: &mut TurnBudget,
    ) -> Result<ToolResponse, ToolError> {
        let (ranked, in_scope) = Self::ranked_candidates(req, ctx).await?;
        let containment = Self::containment_view(&in_scope, ctx);
        let Some(top) = ranked.first() else {
            let payload = Self::with_containment(
                json!({ "tool": "changed_hunks", "hunks": [], "omitted": 0 }),
                containment.as_ref(),
            );
            return Ok(Self::respond(
                ToolName::ChangedHunks,
                payload,
                0,
                budget,
                manifest_event,
            ));
        };
        let rows = ctx
            .store
            .query(
                HUNK_RANGES_SQL,
                &[rift_store::Value::text(top.candidate.id.as_str())],
            )
            .await
            .map_err(internal)?;
        let (hunks, omitted) = ranges_within(&rows, budget.tokens_remaining());
        let payload = Self::with_containment(
            json!({
                "tool": "changed_hunks",
                "path": top.candidate.path,
                "hunks": hunks,
                "omitted": omitted,
                "source_hash": top.content_sha256,
            }),
            containment.as_ref(),
        );
        let baseline = top.candidate.est_tokens;
        Ok(Self::respond(
            ToolName::ChangedHunks,
            payload,
            baseline,
            budget,
            manifest_event,
        ))
    }
}

impl<'a> Pull<'a> {
    fn new(gate: &'a PullGate<'a>, revisions: &Revisions<'_>) -> Self {
        let handle = format!("file:{}@sha256:{}", gate.path, revisions.current_hash);
        let trust = provenance_trust(&handle);
        let validity = resolve(
            &SummaryStore::new(),
            &MemoryDigestStore::default(),
            &WorktreeSnapshot::new(),
            &format!("summary:{}", gate.path),
        );
        Self {
            gate,
            handle,
            trust,
            validity,
            basis_hash: revisions.basis_hash.clone(),
            current_hash: revisions.current_hash.clone(),
        }
    }

    /// An answer of this pull, carrying the containment facts every exit owes.
    fn answer(&self, doc: Value, baseline: u32) -> Answer {
        Answer {
            doc: ServeDispatcher::with_containment(doc, self.gate.containment),
            baseline,
        }
    }

    /// A named non-answer that carries both addresses and claims nothing.
    fn addressed(&self, state: &str) -> Answer {
        self.answer(
            json!({
                "tool": "changed_hunks",
                "path": self.gate.path,
                "state": state,
                "current_hash": self.current_hash,
                "basis_sha256": self.basis_hash,
            }),
            0,
        )
    }

    /// The state the sealed constructor puts the artifact in.
    ///
    /// The constructor decides; this handler reads its verdict through the
    /// same read-only projection every other response uses, and names no
    /// state of its own. What it gets back is the state's own spelling — so a
    /// response can only ever claim the state the constructor put the
    /// artifact in.
    fn constructor_state(&self, seen: SeenStatus, exact: Option<&str>, tokens: u32) -> String {
        let mut vbudget = Budget::new(tokens);
        let env = envelope_for(
            self.gate.path,
            self.trust,
            &self.handle,
            &self.current_hash,
            seen,
            &self.validity,
            exact,
            self.gate.exact_allowed,
            &mut vbudget,
        );
        envelope_view(&env)
            .get("state")
            .and_then(Value::as_str)
            .unwrap_or("available_by_handle")
            .to_owned()
    }

    /// The caller holds the current bytes. The whole file is what it did not
    /// have to read, and the sealed constructor is what says so — this handler
    /// never spells `known_unchanged` itself.
    fn unchanged(&self, est_tokens: u32) -> Answer {
        let doc = json!({
            "tool": "changed_hunks",
            "path": self.gate.path,
            "state": self.constructor_state(SeenStatus::SeenSameHash { last_seen_turn: None }, None, 0),
            "basis_sha256": self.basis_hash,
            "hunks": [],
            "omitted": 0,
            "truncated": false,
        });
        self.answer(doc, est_tokens)
    }

    /// More changed places than a windowed answer stays smaller than the file
    /// for. The count is the honest whole of what is known, and re-reading is
    /// the cheaper of the two remaining options.
    fn diffuse(&self, places: usize, shift: i64) -> Answer {
        self.answer(
            json!({
                "tool": "changed_hunks",
                "path": self.gate.path,
                "state": "diffuse",
                "places": places,
                "shift": shift,
                "current_hash": self.current_hash,
                "basis_sha256": self.basis_hash,
            }),
            0,
        )
    }

    /// **A changed place that rests on a mask is served as counts, never as
    /// lines.** The basis passed the redaction gate on its way to disk, so a
    /// line the gate masked there differs from the live file whether or not
    /// the repository changed it — and the lines this verb would serve for it
    /// are the *live* bytes of exactly the span that was redacted. The digest
    /// is the same one-way serializer untrusted provenance leaves by, under
    /// the tag the vocabulary already has for a source that was redacted away.
    fn masked(&self, current: &[u8]) -> Answer {
        let digest = serialize_untrusted("file_content", current, &self.handle);
        let doc = json!({
            "tool": "changed_hunks",
            "path": self.gate.path,
            "state": "refused",
            "reason": omit_tag(OmitReason::SourceDeletedOrRedacted),
            "basis_sha256": self.basis_hash,
            "current_hash": self.current_hash,
            "neutral_digest": serde_json::to_value(&digest).unwrap_or(Value::Null),
        });
        self.answer(doc, 0)
    }

    /// Untrusted provenance never reaches a line of the payload: what the
    /// caller gets is the digest's counts, and the bytes stay here.
    fn untrusted(&self, shown: &Shown) -> Answer {
        let withheld = window::render(&shown.lines, &shown.windows);
        let digest = serialize_untrusted("file_content", withheld.as_bytes(), &self.handle);
        let doc = json!({
            "tool": "changed_hunks",
            "path": self.gate.path,
            "state": "refused",
            "reason": omit_tag(OmitReason::PolicyDeniedExact),
            "neutral_digest": serde_json::to_value(&digest).unwrap_or(Value::Null),
        });
        self.answer(doc, 0)
    }

    /// The changed places of the current file against the basis — or the
    /// named non-answer for a diff that has none to serve.
    fn changed_places(
        &self,
        basis: &Basis,
        current: &[u8],
        est_tokens: u32,
    ) -> Result<(Vec<(u32, u32)>, i64), Answer> {
        let RedactedDiff { diff, tainted } = held_diff::held_diff_of(basis, current);
        if tainted {
            return Err(self.masked(current));
        }
        match diff {
            HeldDiff::Changed { ranges, shift } => Ok((ranges, shift)),
            HeldDiff::Unchanged => Err(self.unchanged(est_tokens)),
            HeldDiff::Binary => Err(self.addressed("binary")),
            HeldDiff::Unavailable => Err(self.addressed("basis_unavailable")),
            HeldDiff::Diffuse { places, shift } => Err(self.diffuse(places, shift)),
        }
    }

    /// Nothing fits, so nothing is claimed: an all-elision text would read as
    /// "this file did not change".
    fn omitted(&self, omitted: usize) -> Value {
        ServeDispatcher::with_containment(
            json!({
                "tool": "changed_hunks",
                "path": self.gate.path,
                "state": "omitted",
                "reason": omit_tag(OmitReason::BudgetExhausted),
                "basis_sha256": self.basis_hash,
                "current_hash": self.current_hash,
                "hunks": [],
                "omitted": omitted,
                "truncated": true,
            }),
            self.gate.containment,
        )
    }

    /// The content answer, fitted to the cap with the story sacrificed before
    /// any changed line.
    fn compose(
        &self,
        req: &ToolRequest,
        budget: &TurnBudget,
        shown: &Shown,
        shift: i64,
        story: &PullStory,
    ) -> Answer {
        let n = shown.windows.len();
        let build = |kept: usize, truncated: bool, detail: bool| {
            let windows = shown.windows.get(..kept).unwrap_or(&[]);
            let omitted = n.saturating_sub(kept);
            if kept == 0 {
                return self.omitted(omitted);
            }
            let text = window::render(&shown.lines, windows);
            let state = self.constructor_state(
                SeenStatus::NotSeenOrChanged,
                Some(text.as_str()),
                budget.tokens_remaining(),
            );
            // The lines ride only on the state that admits them. The
            // constructor decided which one that is; this reads the decision
            // rather than repeating the reasoning behind it.
            let content = if state == "exact_included" {
                Some(text)
            } else {
                None
            };
            let hunks: Vec<Value> = windows
                .iter()
                .map(|w| hunk_view(w, detail, &story.origins))
                .collect();
            let mut doc = json!({
                "tool": "changed_hunks",
                "path": self.gate.path,
                "state": state,
                "basis_sha256": self.basis_hash,
                "current_hash": self.current_hash,
                "shift": shift,
                "hunks": hunks,
                "content": content,
                "omitted": omitted,
                "truncated": truncated,
            });
            self.attach_story(&mut doc, detail, story);
            self.attach_gate(&mut doc);
            ServeDispatcher::with_containment(doc, self.gate.containment)
        };
        let (doc, _kept) = fit_with_provenance(manifest_cap(req, budget), n, &build);
        Answer {
            doc,
            baseline: shown.baseline,
        }
    }

    /// The delta's own provenance, once for the file. It downgrades to the
    /// tag and the command before a single changed line is dropped: the lines
    /// are the answer, and the story is what rides with it.
    fn attach_story(&self, doc: &mut Value, detail: bool, story: &PullStory) {
        let Some(obj) = doc.as_object_mut() else {
            return;
        };
        let bracket = story.bracket();
        let full = if detail {
            provenance::object(self.gate.oracle, &bracket, self.gate.path, story.first_line)
        } else {
            None
        };
        match full {
            Some(value) => {
                obj.insert("provenance".to_owned(), value);
            }
            None if bracket.is_anchored() => {
                let (tag, why) = provenance::downgraded(self.gate.path, story.first_line);
                obj.insert("provenance".to_owned(), tag);
                obj.insert("why".to_owned(), why);
            }
            None => (),
        }
    }

    /// A sensitive path never reaches the content state, and the answer says
    /// which gate held the lines back rather than leaving a caller to infer it
    /// from a field that is simply absent.
    fn attach_gate(&self, doc: &mut Value) {
        if self.gate.exact_allowed {
            return;
        }
        let Some(obj) = doc.as_object_mut() else {
            return;
        };
        obj.insert(
            "reason".to_owned(),
            json!(omit_tag(OmitReason::SensitivePathRequiresReason)),
        );
    }
}

impl Shown {
    fn of(current: &[u8], ranges: &[(u32, u32)]) -> Self {
        let lines = window::text_lines(current);
        let windows = window::windows(&lines, ranges);
        let baseline = rift_tokens::count(&String::from_utf8_lossy(current));
        Self {
            lines,
            windows,
            baseline,
        }
    }
}

impl PullStory {
    /// Both ends are looked up **by revision**, not taken from the candidate
    /// row the handler picked to answer `current_hash`. That row is the most
    /// recently touched revision of the path, which is the right answer to
    /// "what is this file" and the wrong one to "which checkpoint recorded
    /// these bytes": a file edited in the work tree and never read again has
    /// no checkpoint for its current revision at all, and naming the one that
    /// recorded an older revision would put a change in a turn that did not
    /// make it. Absent is the honest answer, and the object then carries the
    /// end it does know.
    async fn resolve<S: CanonicalStore, R: SubstrateReader>(
        ctx: &RecallContext<'_, S, R>,
        pull: &Pull<'_>,
        loaded: &[LoadedCandidate],
        ranges: &[(u32, u32)],
    ) -> Self {
        let anchored = |hash: &str| {
            loaded
                .iter()
                .find(|l| l.candidate.path == pull.gate.path && l.content_sha256 == hash)
                .map(|l| l.checkpoint.clone())
        };
        let (from, to) = (anchored(&pull.basis_hash), anchored(&pull.current_hash));
        // The commit belongs to whichever end the capture facts will come from.
        let anchor = to.clone().or_else(|| from.clone());
        let commits = match &anchor {
            Some(id) => commit_oids(ctx.store, std::slice::from_ref(id))
                .await
                .unwrap_or_default(),
            None => BTreeMap::new(),
        };
        let commit = anchor.as_ref().and_then(|id| commits.get(id)).cloned();
        let origins = hunk_origins(ctx.store, pull.gate.path, &pull.current_hash)
            .await
            .unwrap_or_default();
        Self {
            from,
            to,
            commit,
            origins,
            first_line: ranges.first().map(|(lo, _)| *lo),
        }
    }

    fn bracket(&self) -> provenance::Bracket<'_> {
        provenance::Bracket {
            from: self.from.as_deref(),
            to: self.to.as_deref(),
            commit: self.commit.as_deref(),
        }
    }
}

/// The pull, stage by stage. Every early exit is an answer, not a failure:
/// it travels as `Err` so the stages compose with `?`, and the caller joins
/// the two sides.
async fn pull<S: CanonicalStore, R: SubstrateReader>(
    ctx: &RecallContext<'_, S, R>,
    req: &ToolRequest,
    gate: &PullGate<'_>,
    loaded: &[LoadedCandidate],
    budget: &TurnBudget,
) -> Result<Answer, Answer> {
    let revisions = resolve_revisions(ctx, gate.path, &req.known_hashes, loaded)
        .await
        .map_err(|doc| Answer {
            doc: ServeDispatcher::with_containment(doc, gate.containment),
            baseline: 0,
        })?;
    let pull = Pull::new(gate, &revisions);
    let est_tokens = revisions.row.candidate.est_tokens;
    if revisions.basis_hash == revisions.current_hash {
        return Ok(pull.unchanged(est_tokens));
    }
    let (basis, current) = fetch_bytes(ctx, &pull, revisions.current_bytes).await?;
    let (ranges, shift) = pull.changed_places(&basis, &current, est_tokens)?;
    let shown = Shown::of(&current, &ranges);
    if pull.trust.is_untrusted() {
        return Ok(pull.untrusted(&shown));
    }
    let story = PullStory::resolve(ctx, &pull, loaded, &ranges).await;
    Ok(pull.compose(req, budget, &shown, shift, &story))
}

/// The current revision of the path and the caller's basis for it, or the
/// named non-answer — `invalid` for a path this scope holds no revision of,
/// `unknown_basis` for a hash that names no revision of THIS file.
///
/// What the file IS, not what a checkpoint last carried: the caller named a
/// path in a checkout, and the answer it needs is about the bytes a reader
/// would get from that path now. A projection's newest revision trails every
/// edit made since the last checkpoint, and answering from it is how a
/// changed file gets reported as one the caller already holds. The projection
/// stays the fallback for a reader with no checkout to look in.
async fn resolve_revisions<'a, S: CanonicalStore, R: SubstrateReader>(
    ctx: &RecallContext<'_, S, R>,
    path: &str,
    known_hashes: &[String],
    loaded: &'a [LoadedCandidate],
) -> Result<Revisions<'a>, Value> {
    let Some(row) = current_revision(loaded, path) else {
        return Err(json!({
            "tool": "changed_hunks",
            "path": path,
            "state": "invalid",
            "reason": "path_not_found",
        }));
    };
    let now = held_diff::current_worktree_bytes(ctx.reader, path);
    let current_hash = now
        .as_ref()
        .map_or_else(|| row.content_sha256.clone(), |(hash, _)| hash.clone());
    let Some(basis_hash) = held_basis(ctx.store, known_hashes, path).await else {
        return Err(json!({
            "tool": "changed_hunks",
            "path": path,
            "state": "unknown_basis",
            "current_hash": current_hash,
        }));
    };
    Ok(Revisions {
        row,
        current_hash,
        current_bytes: now.map(|(_, bytes)| bytes),
        basis_hash,
    })
}

/// The row of the path's current revision.
///
/// Candidates are keyed by FILE REVISION, so one path has a row per revision
/// anyone read and `find` would take whichever the map yielded first — on a
/// real projection that is an arbitrary revision answering as `current_hash`,
/// which is how a caller holding the current bytes was told
/// `basis_unavailable` and a caller holding old ones was told
/// `known_unchanged`. The current revision is the most recently touched one;
/// the id breaks a tie so the choice is deterministic rather than whatever
/// order the rows arrived in.
fn current_revision<'a>(loaded: &'a [LoadedCandidate], path: &str) -> Option<&'a LoadedCandidate> {
    loaded
        .iter()
        .filter(|l| l.candidate.path == path)
        .max_by(|a, b| {
            a.candidate
                .last_touched
                .cmp(&b.candidate.last_touched)
                .then_with(|| a.candidate.id.cmp(&b.candidate.id))
        })
}

/// The caller's basis, through the PATH-BOUND resolver.
///
/// The first hash that names a revision OF THIS FILE wins; a caller sending
/// several is holding several, and any one of them is a basis this file can
/// be diffed against. An address the family knows for some other file is not
/// one of them: resolving it here would let a caller pair any address it has
/// seen with any path it may name and receive that path's whole content as a
/// single changed place.
async fn held_basis(
    store: &impl CanonicalStore,
    known_hashes: &[String],
    path: &str,
) -> Option<String> {
    for given in known_hashes {
        if let Ok(basis::Basis::Resolved(hash)) =
            basis::resolve_basis_for_path(store, given, path).await
        {
            return Some(hash);
        }
    }
    None
}

/// Both sides' bytes, or `basis_unavailable`.
///
/// The basis travels the one route a revision nobody kept can travel: content
/// address, the bridge ingest writes, the read-only substrate — so a revision
/// that is not a tracked blob has no bytes here at all. It is fetched WITH THE
/// PATH BOUND, so it cannot be another file's bytes wearing this file's line
/// numbers; the current side is the named file itself, read from the
/// checkout, which is bound to the path by construction.
///
/// Down the whole ladder: the blob first, then the family's read-time
/// snapshot. The second rung is what gives an **uncommitted** revision a basis
/// at all — and it is redacted, so what it licenses is checked by the diff
/// rather than assumed.
async fn fetch_bytes<S: CanonicalStore, R: SubstrateReader>(
    ctx: &RecallContext<'_, S, R>,
    pull: &Pull<'_>,
    current_bytes: Option<Vec<u8>>,
) -> Result<(Basis, Vec<u8>), Answer> {
    let path = pull.gate.path;
    let current = match current_bytes {
        Some(bytes) => Some(bytes),
        None => {
            held_diff::basis_bytes_for_path(ctx.store, ctx.reader, path, &pull.current_hash).await
        }
    };
    let basis = held_diff::resolve_basis_for_path(
        ctx.store,
        ctx.reader,
        ctx.snapshots,
        path,
        &pull.basis_hash,
    )
    .await;
    match (current, basis) {
        (Some(current), Some(basis)) => Ok((basis, current)),
        _ => Err(pull.addressed("basis_unavailable")),
    }
}

/// One window's view. Per hunk only where the hunk's own revision is known:
/// one recorded hunk covering this window, or nothing.
fn hunk_view(w: &Window, detail: bool, origins: &[(HunkOrigin, String)]) -> Value {
    let mut view = json!({
        "lo": w.lo,
        "hi": w.hi,
        "window": w.rule.name(),
    });
    if !detail {
        return view;
    }
    if let (Some(ck), Some(obj)) = (origin_of(origins, w.lo, w.hi), view.as_object_mut()) {
        obj.insert("checkpoint".to_owned(), json!(ck));
    }
    view
}

/// The recorded ranges that fit, and how many did not.
///
/// Capped at the configured max-hunks and the per-turn token allowance. The
/// per-hunk omission is a NON-CHARGING trial against a local mirror of the
/// remaining budget — the single real charge happens at the respond seam.
fn ranges_within(rows: &[Row], mut trial: u32) -> (Vec<Value>, u32) {
    let max_hunks = MAX_CHANGED_HUNKS as usize;
    let mut hunks = Vec::new();
    let mut omitted = 0u32;
    for (i, r) in rows.iter().enumerate() {
        if i >= max_hunks {
            omitted += 1;
            continue;
        }
        let lo = r.get_i64(0).unwrap_or(0).max(0);
        let len = r.get_i64(1).unwrap_or(0).max(0);
        let view = json!({ "lo": lo, "hi": lo + len });
        let cost = rift_tokens::count(&view.to_string());
        if cost > trial {
            omitted += 1;
            continue;
        }
        trial -= cost;
        hunks.push(view);
    }
    (hunks, omitted)
}
