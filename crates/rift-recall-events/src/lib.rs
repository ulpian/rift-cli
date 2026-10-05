//! `memd.*` serving events and the `rift hooks memd` handoff.
//!
//! Every serving decision emits a canonical event carrying
//! `estimated_tokens_returned` / `estimated_tokens_avoided`, so over-retrieval is
//! visible and Rift Cloud can attribute *measured* tokens avoided. The Rust side
//! does **not** write the hash chain: it hands the event to the native `rift`
//! capture CLI via `rift hooks memd <verb>` with JSON on stdin, which redacts,
//! chains, and stores it like any other event.
#![forbid(unsafe_code)]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]

mod claim;

use sha2::{Digest, Sha256};

pub use claim::{PullClaim, PullClaimBasis};

/// The opaque per-connection identity carried by the retired schema-2
/// envelope, retained so that envelope stays constructible and decodable. It
/// is deliberately unrelated to capture-session ids, and no production path
/// mints new values — the live axis is [`ServeChannel`].
///
/// The hash domain strings below are **wire bytes**: they decide the derived
/// ids a schema-2 envelope carries, so they are frozen at their historical
/// spelling even though the surrounding names moved to the serve vocabulary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct Schema2ThreadId(rift_core::Blake3Id);

impl Schema2ThreadId {
    /// Derive a process identity from injected entropy. Production supplies OS
    /// randomness; tests supply fixed bytes.
    #[must_use]
    pub fn from_seed(seed: &[u8]) -> Self {
        let mut h = blake3::Hasher::new();
        h.update(b"rift:memd:mcp-thread:v1\0");
        h.update(seed);
        Self(rift_core::Blake3Id::from_bytes(*h.finalize().as_bytes()))
    }

    /// The stable lowercase representation stored in `recall.db`.
    #[must_use]
    pub fn to_hex(self) -> String {
        self.0.to_hex()
    }
}

impl core::fmt::Display for Schema2ThreadId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        self.0.fmt(f)
    }
}

/// One decoded invocation within a [`Schema2ThreadId`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct Schema2CallId(rift_core::Blake3Id);

impl Schema2CallId {
    /// Derive a unique, replay-stable call id from the process id and its
    /// monotonic decoded-call ordinal.
    #[must_use]
    pub fn derive(thread: Schema2ThreadId, ordinal: u64) -> Self {
        let mut h = blake3::Hasher::new();
        h.update(b"rift:memd:mcp-call:v1\0");
        h.update(thread.0.as_bytes());
        h.update(&ordinal.to_be_bytes());
        Self(rift_core::Blake3Id::from_bytes(*h.finalize().as_bytes()))
    }

    /// The stable lowercase representation used as the audit idempotency key.
    #[must_use]
    pub fn to_hex(self) -> String {
        self.0.to_hex()
    }
}

impl core::fmt::Display for Schema2CallId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        self.0.fmt(f)
    }
}

/// Neutral identity attached to one decoded schema-2 invocation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Schema2CallContext {
    /// Per-process connection identity.
    pub thread_id: Schema2ThreadId,
    /// Per-invocation identity and downstream idempotency key.
    pub call_id: Schema2CallId,
    /// Decoded wire tool name (never its arguments).
    pub tool_name: String,
}

/// A decoded invocation's terminal result. Internal failures after decoding
/// are counted without exposing error text; failures before decoding never
/// construct one of these.
#[derive(Clone, Copy, Debug)]
pub enum Schema2Terminal<'a> {
    /// The policy/serve layer produced a canonical `memd.*` decision.
    Event(&'a MemdEvent),
    /// Dispatch failed after the tool name and parameters decoded.
    InternalError,
}

/// The neutral tag on a decision the window journal proved: the window was
/// handed these lines already.
pub const TAG_ALREADY_HELD: &str = "already_held";

/// The neutral tag on a row whose avoided figure could not be measured: the
/// bytes the decision named are in neither the working file nor the object
/// database any more, so the honest figure is zero and this says why.
pub const TAG_BASIS_UNAVAILABLE: &str = "basis_unavailable";

/// The neutral tag on a row this build could not count with the real
/// tokenizer. Measured or absent: the figures stay at zero rather than
/// carrying an estimate no one can check.
pub const TAG_TOKENS_UNMEASURED: &str = "tokens_unmeasured";

/// The neutral tag on an avoided figure that is a **declared** per-format
/// baseline rather than a measurement (document conversion has no second
/// reading of the native cost to measure).
pub const TAG_DECLARED_BASELINE: &str = "declared_baseline";

/// The neutral tag on a document read that ran untouched because its
/// conversion did not finish inside the deadline.
pub const TAG_CONVERT_DEADLINE: &str = "convert_deadline";

/// The neutral tag on a read Rift **prevented**: it had told this context
/// window it already held the file, and the window then acted on the file —
/// changed it, or searched inside it — without reading it again.
pub const TAG_PREVENTED: &str = "prevented";

/// The same decision **recorded and not acted on**: the lever is in observe
/// for this family, so the row sizes the class and claims none of it. The
/// avoided figure on such a row is zero, and this is what says why.
pub const TAG_PREVENTED_OBSERVED: &str = "prevented_observed";

/// The neutral tag on a decline whose slice a **prevented** row of the same
/// window already booked.
///
/// The two levers can reach one slice in either order, and the slice is worth
/// one read whichever of them reached it first. When the booking came first,
/// the decline is still made — the model is declined and pointed at what it
/// holds, and the reason it was sent is still charged — and its avoided figure
/// is a zero this tag explains. The mirror case needs no tag: a decline that
/// came first answers the announcement, so no booking follows it at all.
pub const TAG_PREVENTED_FIRST: &str = "prevented_first";

/// The identity of one revision of one file: `file:<path>@sha256:<hash>`, the
/// grammar the serve path has always written into `memd_audit_event.handle`.
#[must_use]
pub fn file_handle(path: &str, content_sha256: &str) -> String {
    format!("file:{path}@sha256:{content_sha256}")
}

/// The identity of one **run of lines** of one revision:
/// `file:<path>@sha256:<hash>:L<lo>-<hi>`, [`file_handle`] with the range the
/// decision was about, so a repeat pool can be counted per slice and not only
/// per file.
#[must_use]
pub fn read_handle(path: &str, content_sha256: &str, lo: u32, hi: u32) -> String {
    format!("{}:L{lo}-{hi}", file_handle(path, content_sha256))
}

/// The identity of a capped content search: `grep:matches=<total>`.
///
/// The total the tool itself reported is the only fact a cap produces, and
/// the audit relation has no column for a count. It is carried here, in the
/// identity column, because that column already holds identities this crate
/// composes (never agent text), and because the alternative — a new column —
/// is a migration on an append-only canonical table that this work is not
/// allowed to make. [`grep_cap_matches`] reads it back.
#[must_use]
pub fn grep_cap_handle(matches_total: u32) -> String {
    format!("grep:matches={matches_total}")
}

/// The total a [`grep_cap_handle`] carries, or `None` for anything else — the
/// reader's half, so the count survives the round trip through the relation
/// as a number rather than as a string a report re-parses by eye.
#[must_use]
pub fn grep_cap_matches(handle: &str) -> Option<u32> {
    handle.strip_prefix("grep:matches=")?.parse().ok()
}

/// A serving decision event.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MemdEvent {
    /// A manifest (no content) was served — the **pull** side: the agent asked,
    /// so the response is both a cost and a claim about what it saved.
    ManifestServed {
        /// Tokens the manifest itself cost.
        estimated_tokens_returned: u32,
        /// Tokens avoided vs a full re-read.
        estimated_tokens_avoided: u32,
        /// What the caller claimed to hold, on a pull that stated revisions;
        /// absent otherwise, and then absent from the payload too, so every
        /// other serve's bytes and key are what they always were.
        #[serde(skip_serializing_if = "Option::is_none")]
        pull_claim: Option<PullClaim>,
    },
    /// A manifest was **injected** — the push side: Rift put it in
    /// the agent's context unasked, so it is pure spend.
    ///
    /// This is a distinct kind rather than a [`ManifestServed`](Self::ManifestServed)
    /// with `estimated_tokens_avoided: 0`, and the difference is the whole point:
    /// injected spend and served credit share one relation, and while they also
    /// shared one *verb* an audit row held nothing that told them apart. The verb
    /// was the only discriminating column either side ever set — see
    /// `an_injection_is_its_own_verb_and_carries_no_credit`, which pins that an
    /// injection sets no handle, no reason and no avoided figure — so separating
    /// them meant inferring the side from *outside* the row (a missing call-event
    /// sibling, avoided = 0). A separate verb makes the netting a query instead of
    /// an inference. Carrying no `avoided` field at all is what makes "an
    /// injection never claims credit" a fact about the type: a caller cannot pass
    /// a figure that is not there.
    ManifestInjected {
        /// Tokens the injected manifest cost the turn, measured by the real
        /// tokenizer on the exact string injected.
        estimated_tokens_returned: u32,
    },
    /// A validity-checked summary was served.
    SummaryServed {
        /// The summary handle.
        handle: String,
        /// Tokens the summary cost.
        estimated_tokens_returned: u32,
        /// Tokens avoided vs the full artifact.
        estimated_tokens_avoided: u32,
        /// What the caller claimed to hold, on a pull that stated revisions;
        /// absent otherwise, and then absent from the payload too, so every
        /// other serve's bytes and key are what they always were.
        #[serde(skip_serializing_if = "Option::is_none")]
        pull_claim: Option<PullClaim>,
    },
    /// Exact content was retrieved (policy-gated, reason-required, audited).
    ExactRetrieved {
        /// The retrieved handle.
        handle: String,
        /// The reason supplied (mandatory).
        reason: String,
        /// Tokens the exact content cost.
        estimated_tokens_returned: u32,
    },
    /// A context pack was built.
    PackBuilt {
        /// Number of sections.
        sections: u32,
        /// Total rendered-pack tokens (the pack compiler's own figure).
        total_tokens: u32,
        /// Tokens the serialized response cost (the accounting seam's measure,
        /// the full payload, not just the rendered pack).
        estimated_tokens_returned: u32,
        /// Tokens avoided vs full re-reads.
        estimated_tokens_avoided: u32,
        /// What the caller claimed to hold, on a pull that stated revisions;
        /// absent otherwise, and then absent from the payload too, so every
        /// other serve's bytes and key are what they always were.
        #[serde(skip_serializing_if = "Option::is_none")]
        pull_claim: Option<PullClaim>,
    },
    /// A summary/handle was invalidated by a source change.
    Invalidated {
        /// The invalidated handle.
        handle: String,
        /// Tokens the invalidation response itself cost (no
        /// response class is exempt from the accounting).
        estimated_tokens_returned: u32,
    },
    /// A serve was refused (denied path/model, missing reason, untrusted-raw,
    /// or transcript-disabled). The `reason` is a neutral, non-leaking tag — it
    /// names *why*, never the withheld content. Audited so every serve decision
    /// (granted or refused) has exactly one terminal event.
    Refused {
        /// The target handle/path the agent asked for.
        target: String,
        /// The neutral refusal reason tag (e.g. `policy_denied_exact`).
        reason: String,
        /// Tokens the refusal response itself cost (measured and
        /// charged like any serve; never the withheld bytes).
        estimated_tokens_returned: u32,
    },
    /// A read the window was proved to already hold was **declined** once,
    /// with a reason the model sees. `estimated_tokens_avoided` is the real
    /// tokenizer count of the slice the decision named — counted by the
    /// detached worker over the bytes the journaled decision identified, never
    /// on the hook path — and `estimated_tokens_returned` is the count of the
    /// reason frame that went back instead. The `reason` is a neutral tag
    /// naming why the figures are what they are (the window already held the
    /// lines; the bytes could no longer be found to count).
    ReadIntercepted {
        /// `file:<path>@sha256:<hash>:L<lo>-<hi>` — see [`read_handle`].
        handle: String,
        /// The neutral tag for the decision (e.g. [`TAG_ALREADY_HELD`]).
        reason: String,
        /// Tokens the reason frame cost.
        estimated_tokens_returned: u32,
        /// Tokens the declined slice would have cost.
        estimated_tokens_avoided: u32,
    },
    /// The same decision **recorded and not acted on** — observe mode, or a
    /// family whose reason channel is not confirmed. Nothing was declined and
    /// nothing was returned, so the row carries neither figure: the pool it
    /// sizes is counted by the rows themselves, and a would-have-saved number
    /// on a read that happened anyway would be a claim about tokens nobody
    /// avoided.
    ReadRepeatObserved {
        /// `file:<path>@sha256:<hash>:L<lo>-<hi>` — see [`read_handle`].
        handle: String,
        /// The neutral tag for the decision (e.g. [`TAG_ALREADY_HELD`]).
        reason: String,
    },
    /// **A read Rift caused not to happen.**
    ///
    /// Rift told this window it holds a file — a manifest entry, a recovery
    /// map entry — and the window then acted on that file without reading it
    /// again: an edit of it, or a content search inside it. No call was
    /// declined, so nothing was returned and there is no field here to return
    /// one; what the row carries is the token count of the announced lines at
    /// the announced revision, counted by the detached worker over exactly
    /// the bytes the decision named.
    ///
    /// It is its own verb rather than a [`ReadIntercepted`](Self::ReadIntercepted)
    /// with a different tag, for the reason the injection split gives: a
    /// figure a reader has to separate is a figure the relation must be able
    /// to separate by itself. The verb does that with no new column and no
    /// migration, and every breakdown reports it beside — never inside — the
    /// measured intercepts.
    ///
    /// The `reason` is [`TAG_PREVENTED`] where the figure is a count, and
    /// [`TAG_PREVENTED_OBSERVED`] where the lever was only observing and the
    /// figure is therefore zero.
    ReadPrevented {
        /// `file:<path>@sha256:<hash>[:L<lo>-<hi>]` — see [`read_handle`] and
        /// [`file_handle`], the latter where the whole file was announced.
        handle: String,
        /// The neutral tag for the decision (e.g. [`TAG_PREVENTED`]).
        reason: String,
        /// Tokens the read that did not happen would have cost.
        estimated_tokens_avoided: u32,
    },
    /// A compacted or resumed context was handed the map of what it had
    /// stopped holding. Pure spend, like [`ManifestInjected`](Self::ManifestInjected)
    /// and for the same reason: the agent did not ask, so the row records the
    /// cost and claims no credit.
    RecoveryInjected {
        /// Tokens the injected map cost, measured on the exact string.
        estimated_tokens_returned: u32,
    },
    /// A listing-shaped search was answered from Rift's own index instead of
    /// being run. No handle: a query is the agent's own text, and this
    /// relation's identity columns are Rift's.
    ///
    /// The `reason` is the neutral tag that says what the avoided figure is —
    /// [`TAG_DECLARED_BASELINE`] on an answer that was made, and the
    /// decision's own tag on one that was recorded and not acted on. Without
    /// it the two are told apart only by an avoided figure of zero, which is
    /// an inference about a row rather than a fact in it.
    SearchAnswered {
        /// The neutral tag for the decision (e.g. [`TAG_DECLARED_BASELINE`]).
        reason: String,
        /// Tokens the answer frame cost.
        estimated_tokens_returned: u32,
        /// Tokens the full listing would have cost.
        estimated_tokens_avoided: u32,
    },
    /// A content search was given the result cap it lacked. Nothing was
    /// avoided — the search ran, and Rift added nothing to its output — so the
    /// row books no figures at all and carries the total the tool reported, in
    /// the handle, through [`grep_cap_handle`]: the count is the only fact
    /// this decision produces, and the audit relation's identity column is
    /// where a fact of Rift's own making can be stated without a new column.
    GrepCapped {
        /// `grep:matches=<total>` — see [`grep_cap_handle`].
        handle: String,
    },
    /// A document read was pointed at Rift's Markdown sidecar. The avoided
    /// figure is a **declared** per-format baseline, not a measurement, which
    /// is what the [`TAG_DECLARED_BASELINE`] reason says out loud.
    DocRewritten {
        /// `file:<path>@sha256:<hash>` — see [`file_handle`].
        handle: String,
        /// The neutral tag, [`TAG_DECLARED_BASELINE`] when the figure is one.
        reason: String,
        /// Tokens the sidecar the agent read instead cost.
        estimated_tokens_returned: u32,
        /// The declared native cost of the document it replaced.
        estimated_tokens_avoided: u32,
    },
    /// A document read Rift did not answer with Markdown: the conversion did
    /// not finish inside its deadline and the read ran untouched, or the verb
    /// was asked for a document the family holds no sidecar for. Nothing was
    /// avoided either way — but the verb's answer is itself a response the
    /// caller paid for, so what it cost is carried rather than assumed away.
    DocDeferred {
        /// `file:<path>@sha256:<hash>` — see [`file_handle`].
        handle: String,
        /// The neutral tag naming why (e.g. [`TAG_CONVERT_DEADLINE`]).
        reason: String,
        /// What the answer cost the caller: zero on the hook path, where the
        /// read simply ran, and the payload's own count on the verb.
        estimated_tokens_returned: u32,
    },
    /// The search verb served a ranked answer through the chokepoint — the
    /// pull side of [`SearchAnswered`](Self::SearchAnswered).
    SearchServed {
        /// Tokens the answer cost.
        estimated_tokens_returned: u32,
        /// Tokens the full listing would have cost.
        estimated_tokens_avoided: u32,
    },
    /// The document verb served a sidecar through the chokepoint — the pull
    /// side of [`DocRewritten`](Self::DocRewritten), with the same declared
    /// baseline and the same tag.
    DocServed {
        /// `file:<path>@sha256:<hash>` — see [`file_handle`].
        handle: String,
        /// The neutral tag, [`TAG_DECLARED_BASELINE`] when the figure is one.
        reason: String,
        /// Tokens the sidecar cost.
        estimated_tokens_returned: u32,
        /// The declared native cost of the document.
        estimated_tokens_avoided: u32,
    },
}

impl MemdEvent {
    /// The hook verb for this event (`memd.<verb>`).
    #[must_use]
    pub fn verb(&self) -> &'static str {
        match self {
            Self::ManifestServed { .. } => "manifest_served",
            Self::ManifestInjected { .. } => "manifest_injected",
            Self::SummaryServed { .. } => "summary_served",
            Self::ExactRetrieved { .. } => "exact_retrieved",
            Self::PackBuilt { .. } => "pack_built",
            Self::Invalidated { .. } => "invalidated",
            Self::Refused { .. } => "refused",
            Self::ReadIntercepted { .. } => "read_intercepted",
            Self::ReadRepeatObserved { .. } => "read_repeat_observed",
            Self::ReadPrevented { .. } => "read_prevented",
            Self::RecoveryInjected { .. } => "recovery_injected",
            Self::SearchAnswered { .. } => "search_answered",
            Self::GrepCapped { .. } => "grep_capped",
            Self::DocRewritten { .. } => "doc_rewritten",
            Self::DocDeferred { .. } => "doc_deferred",
            Self::SearchServed { .. } => "search_served",
            Self::DocServed { .. } => "doc_served",
        }
    }

    /// Tokens avoided by this decision (0 for exact/invalidated).
    #[must_use]
    pub fn tokens_avoided(&self) -> u32 {
        match self {
            Self::ManifestServed {
                estimated_tokens_avoided,
                ..
            }
            | Self::SummaryServed {
                estimated_tokens_avoided,
                ..
            }
            | Self::PackBuilt {
                estimated_tokens_avoided,
                ..
            }
            | Self::ReadIntercepted {
                estimated_tokens_avoided,
                ..
            }
            | Self::ReadPrevented {
                estimated_tokens_avoided,
                ..
            }
            | Self::SearchAnswered {
                estimated_tokens_avoided,
                ..
            }
            | Self::DocRewritten {
                estimated_tokens_avoided,
                ..
            }
            | Self::SearchServed {
                estimated_tokens_avoided,
                ..
            }
            | Self::DocServed {
                estimated_tokens_avoided,
                ..
            } => *estimated_tokens_avoided,
            // An injection claims nothing: it is spend the agent did not ask for,
            // and there is no field here to say otherwise.
            //
            // The three decisions that acted on nobody's behalf are the same
            // way: an observed repeat was not declined, a capped search still
            // ran, and a deferred document was read as it was. None of them
            // avoided a token, and none of them has a field that could say so.
            Self::ManifestInjected { .. }
            | Self::ExactRetrieved { .. }
            | Self::Invalidated { .. }
            | Self::Refused { .. }
            | Self::ReadRepeatObserved { .. }
            | Self::RecoveryInjected { .. }
            | Self::GrepCapped { .. }
            | Self::DocDeferred { .. } => 0,
        }
    }

    /// Tokens returned by this decision — the accounting seam's measure of the
    /// serialized response, on every variant: a refusal or
    /// invalidation costs its own (small) payload, never the withheld bytes.
    #[must_use]
    pub fn tokens_returned(&self) -> u32 {
        match self {
            Self::ManifestServed {
                estimated_tokens_returned,
                ..
            }
            | Self::ManifestInjected {
                estimated_tokens_returned,
            }
            | Self::SummaryServed {
                estimated_tokens_returned,
                ..
            }
            | Self::ExactRetrieved {
                estimated_tokens_returned,
                ..
            }
            | Self::PackBuilt {
                estimated_tokens_returned,
                ..
            }
            | Self::Invalidated {
                estimated_tokens_returned,
                ..
            }
            | Self::Refused {
                estimated_tokens_returned,
                ..
            }
            | Self::ReadIntercepted {
                estimated_tokens_returned,
                ..
            }
            | Self::RecoveryInjected {
                estimated_tokens_returned,
            }
            | Self::SearchAnswered {
                estimated_tokens_returned,
                ..
            }
            | Self::DocRewritten {
                estimated_tokens_returned,
                ..
            }
            | Self::SearchServed {
                estimated_tokens_returned,
                ..
            }
            | Self::DocServed {
                estimated_tokens_returned,
                ..
            }
            | Self::DocDeferred {
                estimated_tokens_returned,
                ..
            } => *estimated_tokens_returned,
            // A decision that put nothing in front of the model returns
            // nothing, and says so by having no field to return.
            Self::ReadRepeatObserved { .. }
            | Self::GrepCapped { .. }
            // A read that never ran put nothing in front of the model. The
            // frame that made it unnecessary was already charged where it was
            // injected, and charging it twice would net a saving against a
            // cost some other row is already carrying.
            | Self::ReadPrevented { .. } => 0,
        }
    }

    /// Build the JSON envelope to pipe to `rift hooks memd <verb>` on stdin.
    /// Carries a content-addressed idempotency key so a retried handoff is a
    /// no-op downstream.
    #[must_use]
    pub fn to_hook_json(&self) -> String {
        self.to_hook_json_keyed("")
    }

    /// [`to_hook_json`](Self::to_hook_json) with an extra `occurrence`
    /// discriminator folded into the idempotency key.
    ///
    /// The plain key is the payload's hash, which is exactly right when the
    /// payload varies with the serve: a retried handoff collapses, distinct
    /// serves do not. Some serves are byte-identical every time they happen —
    /// an injected session pointer costs the same tokens in every session —
    /// and would otherwise be counted once for all time, quietly understating
    /// spend. Passing a discriminator that is stable *within* one occurrence
    /// (the session, say) and distinct *across* occurrences keeps both
    /// properties: retries still collapse, separate occurrences still count.
    #[must_use]
    pub fn to_hook_json_keyed(&self, occurrence: &str) -> String {
        let payload = serde_json::to_value(self).unwrap_or(serde_json::Value::Null);
        let payload_bytes = serde_json::to_vec(&payload).unwrap_or_default();
        let mut h = Sha256::new();
        h.update(&payload_bytes);
        if !occurrence.is_empty() {
            h.update([0]);
            h.update(occurrence.as_bytes());
        }
        let mut key = String::with_capacity(64);
        use core::fmt::Write as _;
        for b in h.finalize() {
            let _ = write!(key, "{b:02x}");
        }
        let envelope = serde_json::json!({
            "schema": 1,
            "verb": format!("memd.{}", self.verb()),
            "idempotency_key": key,
            "payload": payload,
        });
        serde_json::to_string(&envelope).unwrap_or_default()
    }

    /// Build the schema-4 channel-attributed envelope: a
    /// `serve_call_event` row, discriminated by [`ServeChannel`] — the hook+
    /// skill successor to the schema-2 [`Schema2Terminal`] envelope, whose
    /// one producer was the retired stdio transport. Nothing writes
    /// `memd_call_event` anymore; that append-only table and
    /// [`Schema2Terminal`] both remain as compatibility infrastructure —
    /// historical rows stay readable and the schema-2 decode path stays
    /// exercised — rather than being deleted.
    /// `call_id` is the caller's own identity (a fresh one per invocation),
    /// used directly as the idempotency key —
    /// there is no payload-hash collapse here because, unlike the plain
    /// audit relation, two invocations that return identical figures are
    /// still two distinct calls (the same reasoning `Schema2CallId` uses).
    #[must_use]
    pub fn to_serve_call_json(
        &self,
        channel: ServeChannel,
        call_id: &str,
        tool_name: &str,
        recorded_at: rift_core::Timestamp,
    ) -> String {
        let payload = serde_json::to_value(self).unwrap_or(serde_json::Value::Null);
        let envelope = serde_json::json!({
            "schema": 4,
            "verb": format!("memd.{}", self.verb()),
            "idempotency_key": call_id,
            "serve_call": {
                "call_id": call_id,
                "channel": channel.token(),
                "tool_name": tool_name,
                "recorded_at": recorded_at.as_millis(),
                "tokens_returned": self.tokens_returned(),
                "tokens_avoided": self.tokens_avoided(),
            },
            "payload": payload,
        });
        serde_json::to_string(&envelope).unwrap_or_default()
    }
}

/// The two consumption paths a serve happens through, replacing the
/// per-connection `thread_id`/`client` axis the historical
/// `memd_call_event`/`mcp_connection_event` relations were built around
/// (neither hook injection nor a skill CLI invocation has a persistent
/// connection to multiplex over).
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServeChannel {
    /// Pushed into the agent's context unasked, at a turn boundary.
    HookInject,
    /// Pulled by the agent shelling out to a `rift recall …` skill verb.
    SkillCli,
}

impl ServeChannel {
    /// The stable, brand-free token stored in `serve_call_event.channel` /
    /// `serve_session_event.channel`.
    #[must_use]
    pub fn token(self) -> &'static str {
        match self {
            Self::HookInject => "hook_inject",
            Self::SkillCli => "skill_cli",
        }
    }

    /// Parse a stored channel token, or `None` for anything else — the
    /// reader's half of [`Self::token`], with no lenient arm.
    #[must_use]
    pub fn parse(token: &str) -> Option<Self> {
        match token {
            "hook_inject" => Some(Self::HookInject),
            "skill_cli" => Some(Self::SkillCli),
            _ => None,
        }
    }
}

impl core::fmt::Display for ServeChannel {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.token())
    }
}

impl Schema2Terminal<'_> {
    /// The neutral hook verb for this terminal.
    #[must_use]
    pub fn verb(self) -> &'static str {
        match self {
            Self::Event(event) => event.verb(),
            Self::InternalError => "internal_error",
        }
    }

    /// Build the schema-v2 terminal envelope. The call id, rather than outcome
    /// content, is the idempotency key: identical invocations remain distinct,
    /// while replaying this exact handoff remains a no-op.
    #[must_use]
    pub fn to_hook_json(
        &self,
        call: &Schema2CallContext,
        recorded_at: rift_core::Timestamp,
    ) -> String {
        let (verb, returned, avoided, payload) = match self {
            Self::Event(event) => (
                format!("memd.{}", event.verb()),
                event.tokens_returned(),
                event.tokens_avoided(),
                serde_json::to_value(event).unwrap_or(serde_json::Value::Null),
            ),
            Self::InternalError => (
                "memd.internal_error".to_string(),
                0,
                0,
                serde_json::Value::Null,
            ),
        };
        let call_id = call.call_id.to_hex();
        let envelope = serde_json::json!({
            "schema": 2,
            "verb": verb,
            "idempotency_key": call_id,
            "call": {
                "call_id": call_id,
                "thread_id": call.thread_id.to_hex(),
                "tool_name": call.tool_name,
                "recorded_at": recorded_at.as_millis(),
                "tokens_returned": returned,
                "tokens_avoided": avoided,
            },
            "payload": payload,
        });
        serde_json::to_string(&envelope).unwrap_or_default()
    }
}

/// A serve whose payload repeats verbatim must still be counted per
/// occurrence: keying only by payload would book the first injected pointer
/// and silently discard every later one, understating spend and so inflating
/// net savings. Retries of the *same* occurrence must still collapse.
#[cfg(test)]
mod keyed_tests {
    use super::MemdEvent;

    fn key(json: &str) -> String {
        let v: serde_json::Value = serde_json::from_str(json).unwrap();
        v["idempotency_key"].as_str().unwrap().to_owned()
    }

    #[test]
    fn an_occurrence_key_separates_identical_payloads_but_not_retries() {
        // The real caller of the keyed spelling is the injection path, so the
        // property is stated on the kind that path writes.
        let event = MemdEvent::ManifestInjected {
            estimated_tokens_returned: 86,
        };
        let a1 = key(&event.to_hook_json_keyed("session-a"));
        let a2 = key(&event.to_hook_json_keyed("session-a"));
        let b = key(&event.to_hook_json_keyed("session-b"));
        let plain = key(&event.to_hook_json());
        assert_eq!(a1, a2, "a retry of one occurrence must collapse");
        assert_ne!(a1, b, "two occurrences of one payload must both count");
        assert_ne!(a1, plain, "the discriminator must reach the key");
        // The un-keyed spelling is unchanged: existing callers keep their keys.
        assert_eq!(plain, key(&event.to_hook_json_keyed("")));
        // Only the key moves; the audited figures are the payload's own.
        let doc: serde_json::Value =
            serde_json::from_str(&event.to_hook_json_keyed("session-a")).unwrap();
        assert_eq!(doc["verb"], "memd.manifest_injected");
        assert_eq!(doc["payload"]["estimated_tokens_returned"], 86);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One value of every kind, with the same figures throughout so the table
    /// below reads as a table: 1 returned and 2 avoided wherever the kind can
    /// carry them. Built in three groups, by the path that books each kind, so
    /// every group stays short enough to check against the table by eye.
    fn one_of_every_kind() -> Vec<MemdEvent> {
        let sha = "ab".repeat(32);
        let handle = read_handle("src/a.rs", &sha, 1, 40);
        let mut all = fixture_serving_kinds(&handle);
        all.extend(fixture_read_hook_kinds(&handle));
        all.extend(fixture_search_and_document_kinds(&handle));
        all
    }

    /// The kinds the serving path books: manifests, summaries, exact reads,
    /// packs, invalidations and refusals.
    fn fixture_serving_kinds(handle: &str) -> Vec<MemdEvent> {
        vec![
            MemdEvent::ManifestServed {
                estimated_tokens_returned: 1,
                estimated_tokens_avoided: 2,
                pull_claim: None,
            },
            MemdEvent::ManifestInjected {
                estimated_tokens_returned: 1,
            },
            MemdEvent::SummaryServed {
                handle: handle.to_owned(),
                estimated_tokens_returned: 1,
                estimated_tokens_avoided: 2,
                pull_claim: None,
            },
            MemdEvent::ExactRetrieved {
                handle: handle.to_owned(),
                reason: "debugging".into(),
                estimated_tokens_returned: 1,
            },
            MemdEvent::PackBuilt {
                sections: 1,
                total_tokens: 1,
                estimated_tokens_returned: 1,
                estimated_tokens_avoided: 2,
                pull_claim: None,
            },
            MemdEvent::Invalidated {
                handle: handle.to_owned(),
                estimated_tokens_returned: 1,
            },
            MemdEvent::Refused {
                target: handle.to_owned(),
                reason: "policy_denied_exact".into(),
                estimated_tokens_returned: 1,
            },
        ]
    }

    /// The kinds the read hook books after a decision, and the recovery
    /// injection that follows a compaction.
    fn fixture_read_hook_kinds(handle: &str) -> Vec<MemdEvent> {
        vec![
            MemdEvent::ReadIntercepted {
                handle: handle.to_owned(),
                reason: TAG_ALREADY_HELD.into(),
                estimated_tokens_returned: 1,
                estimated_tokens_avoided: 2,
            },
            MemdEvent::ReadRepeatObserved {
                handle: handle.to_owned(),
                reason: TAG_ALREADY_HELD.into(),
            },
            MemdEvent::ReadPrevented {
                handle: handle.to_owned(),
                reason: TAG_PREVENTED.into(),
                estimated_tokens_avoided: 2,
            },
            MemdEvent::RecoveryInjected {
                estimated_tokens_returned: 1,
            },
        ]
    }

    /// The kinds the search and document paths book.
    fn fixture_search_and_document_kinds(handle: &str) -> Vec<MemdEvent> {
        vec![
            MemdEvent::SearchAnswered {
                reason: TAG_DECLARED_BASELINE.into(),
                estimated_tokens_returned: 1,
                estimated_tokens_avoided: 2,
            },
            MemdEvent::GrepCapped {
                handle: grep_cap_handle(40),
            },
            MemdEvent::DocRewritten {
                handle: handle.to_owned(),
                reason: TAG_DECLARED_BASELINE.into(),
                estimated_tokens_returned: 1,
                estimated_tokens_avoided: 2,
            },
            MemdEvent::DocDeferred {
                handle: handle.to_owned(),
                reason: TAG_CONVERT_DEADLINE.into(),
                estimated_tokens_returned: 1,
            },
            MemdEvent::SearchServed {
                estimated_tokens_returned: 1,
                estimated_tokens_avoided: 2,
            },
            MemdEvent::DocServed {
                handle: handle.to_owned(),
                reason: TAG_DECLARED_BASELINE.into(),
                estimated_tokens_returned: 1,
                estimated_tokens_avoided: 2,
            },
        ]
    }

    /// Every kind's verb, in one table, and the figures each kind is
    /// **structurally** able to carry.
    ///
    /// The match below has no wildcard arm, so a kind added without a row here
    /// does not compile: a verb is what a later report groups by and what an
    /// honesty gate nets over, and a kind that quietly picked its own spelling
    /// would land rows no query is looking for. The second column is the same
    /// claim about the figures — where a `0` stands in this table, the type
    /// has no field for that number at all, which is what makes "an observed
    /// repeat claims nothing" and "a cap avoids nothing" facts rather than
    /// conventions.
    #[test]
    fn every_kind_has_a_stable_verb_and_only_the_figures_it_may_carry() {
        let all = one_of_every_kind();
        for event in &all {
            let (verb, returned, avoided) = match event {
                MemdEvent::ManifestServed { .. } => ("manifest_served", 1, 2),
                MemdEvent::ManifestInjected { .. } => ("manifest_injected", 1, 0),
                MemdEvent::SummaryServed { .. } => ("summary_served", 1, 2),
                MemdEvent::ExactRetrieved { .. } => ("exact_retrieved", 1, 0),
                MemdEvent::PackBuilt { .. } => ("pack_built", 1, 2),
                MemdEvent::Invalidated { .. } => ("invalidated", 1, 0),
                MemdEvent::Refused { .. } => ("refused", 1, 0),
                MemdEvent::ReadIntercepted { .. } => ("read_intercepted", 1, 2),
                MemdEvent::ReadRepeatObserved { .. } => ("read_repeat_observed", 0, 0),
                MemdEvent::ReadPrevented { .. } => ("read_prevented", 0, 2),
                MemdEvent::RecoveryInjected { .. } => ("recovery_injected", 1, 0),
                MemdEvent::SearchAnswered { .. } => ("search_answered", 1, 2),
                MemdEvent::GrepCapped { .. } => ("grep_capped", 0, 0),
                MemdEvent::DocRewritten { .. } => ("doc_rewritten", 1, 2),
                MemdEvent::DocDeferred { .. } => ("doc_deferred", 1, 0),
                MemdEvent::SearchServed { .. } => ("search_served", 1, 2),
                MemdEvent::DocServed { .. } => ("doc_served", 1, 2),
            };
            assert_eq!(event.verb(), verb);
            assert_eq!(event.tokens_returned(), returned, "{verb} returned");
            assert_eq!(event.tokens_avoided(), avoided, "{verb} avoided");
            let v: serde_json::Value = serde_json::from_str(&event.to_hook_json()).unwrap();
            assert_eq!(v["verb"], format!("memd.{verb}"));
            assert_eq!(v["payload"]["kind"], verb, "the tag is the verb");
        }
        // No two kinds share a verb: the whole point of the table.
        let mut verbs: Vec<&str> = all.iter().map(MemdEvent::verb).collect();
        verbs.sort_unstable();
        let count = verbs.len();
        verbs.dedup();
        assert_eq!(verbs.len(), count, "two kinds share a verb");
    }

    /// The three kinds the hook path books after a decision it did NOT act on
    /// carry neither figure in their payload — not a zero someone can raise,
    /// no field at all — and the two identity spellings survive the round
    /// trip a report reads them back through.
    #[test]
    fn a_decision_that_acted_on_nothing_books_no_figures() {
        let sha = "cd".repeat(32);
        let observed = MemdEvent::ReadRepeatObserved {
            handle: read_handle("src/a.rs", &sha, 3, 9),
            reason: TAG_ALREADY_HELD.into(),
        };
        let v: serde_json::Value = serde_json::from_str(&observed.to_hook_json()).unwrap();
        for absent in ["estimated_tokens_returned", "estimated_tokens_avoided"] {
            assert!(v["payload"].get(absent).is_none(), "{absent}: {v}");
        }
        assert_eq!(
            v["payload"]["handle"],
            format!("file:src/a.rs@sha256:{sha}:L3-9")
        );
        assert_eq!(v["payload"]["reason"], TAG_ALREADY_HELD);

        let capped = MemdEvent::GrepCapped {
            handle: grep_cap_handle(137),
        };
        let c: serde_json::Value = serde_json::from_str(&capped.to_hook_json()).unwrap();
        assert_eq!(capped.tokens_avoided(), 0);
        assert_eq!(capped.tokens_returned(), 0);
        assert_eq!(c["payload"]["handle"], "grep:matches=137");
        assert_eq!(grep_cap_matches("grep:matches=137"), Some(137));
        for not_a_cap in ["grep:matches=", "grep:matches=-1", "file:a@sha256:x", ""] {
            assert_eq!(grep_cap_matches(not_a_cap), None, "{not_a_cap}");
        }
    }

    #[test]
    fn verbs_and_tokens_avoided() {
        let m = MemdEvent::ManifestServed {
            estimated_tokens_returned: 50,
            estimated_tokens_avoided: 900,
            pull_claim: None,
        };
        assert_eq!(m.verb(), "manifest_served");
        assert_eq!(m.tokens_avoided(), 900);
        let e = MemdEvent::ExactRetrieved {
            handle: "file:a@sha256:x".into(),
            reason: "debug".into(),
            estimated_tokens_returned: 200,
        };
        assert_eq!(e.verb(), "exact_retrieved");
        assert_eq!(e.tokens_avoided(), 0);
    }

    /// The push/pull split, stated from the side that has to stay true no
    /// matter who calls it: an injected manifest has its own verb, and the
    /// figure that would let it claim credit is not a field it has.
    ///
    /// It also pins the claim the split rests on — that while the two sides
    /// shared a verb the row held nothing else that separated them. An audit
    /// row's discriminating columns are the verb, the token figures, a handle
    /// and a reason; an injection sets none of the last three, so under a
    /// shared verb it was a pull row with nothing avoided and no query could
    /// say otherwise. Give the injection payload a handle or a reason and that
    /// sentence stops being true, which is what this half would catch.
    #[test]
    fn an_injection_is_its_own_verb_and_carries_no_credit() {
        let injected = MemdEvent::ManifestInjected {
            estimated_tokens_returned: 86,
        };
        let served = MemdEvent::ManifestServed {
            estimated_tokens_returned: 86,
            estimated_tokens_avoided: 0,
            pull_claim: None,
        };
        assert_eq!(injected.verb(), "manifest_injected");
        assert_eq!(served.verb(), "manifest_served");
        assert_ne!(injected.verb(), served.verb());
        // Spend is carried; credit is structurally absent, not merely zero.
        assert_eq!(injected.tokens_returned(), 86);
        assert_eq!(injected.tokens_avoided(), 0);

        let v: serde_json::Value = serde_json::from_str(&injected.to_hook_json()).unwrap();
        assert_eq!(v["verb"], "memd.manifest_injected");
        assert_eq!(v["payload"]["kind"], "manifest_injected");
        assert_eq!(v["payload"]["estimated_tokens_returned"], 86);
        assert!(
            v["payload"].get("estimated_tokens_avoided").is_none(),
            "an injection payload must not carry an avoided figure at all"
        );
        // …nor either of the other two columns a row could have been told
        // apart by. With the verb shared, an injection was a pull row with
        // nothing avoided — identical in every column that carries meaning,
        // which is why the ambiguity of pre-split rows is by construction and
        // not an accident of any particular database.
        for absent in ["handle", "reason"] {
            assert!(
                v["payload"].get(absent).is_none(),
                "an injection payload must not carry `{absent}`: {v}"
            );
        }
        let s: serde_json::Value = serde_json::from_str(&served.to_hook_json()).unwrap();
        assert_eq!(
            s["payload"]["estimated_tokens_returned"], v["payload"]["estimated_tokens_returned"],
            "the figures a shared verb would have left visible are the same figures"
        );
        // Two rows that cost the same are still two different kinds of fact, so
        // the two envelopes must not collide on the idempotency key either.
        assert_ne!(v["idempotency_key"], s["idempotency_key"]);
    }

    /// A prevented read is separable from a measured intercept **in the
    /// relation**, not by a reader's eye: a different verb, an avoided figure
    /// of its own, and no cost — the frame that made the read unnecessary is
    /// charged where it was injected, and charging it again here would net
    /// one spend against two savings.
    #[test]
    fn a_prevented_read_is_its_own_verb_and_claims_no_cost() {
        let prevented = MemdEvent::ReadPrevented {
            handle: read_handle("src/lib.rs", &"c".repeat(64), 1, 40),
            reason: TAG_PREVENTED.into(),
            estimated_tokens_avoided: 900,
        };
        let intercepted = MemdEvent::ReadIntercepted {
            handle: read_handle("src/lib.rs", &"c".repeat(64), 1, 40),
            reason: TAG_ALREADY_HELD.into(),
            estimated_tokens_returned: 88,
            estimated_tokens_avoided: 900,
        };
        assert_ne!(prevented.verb(), intercepted.verb());
        assert_eq!(prevented.tokens_avoided(), 900);
        assert_eq!(prevented.tokens_returned(), 0);
        let v: serde_json::Value =
            serde_json::from_str(&prevented.to_hook_json()).expect("valid json");
        assert_eq!(v["verb"], "memd.read_prevented");
        assert_eq!(v["payload"]["reason"], TAG_PREVENTED);
        assert_eq!(v["payload"]["estimated_tokens_avoided"], 900);
        assert!(
            v["payload"].get("estimated_tokens_returned").is_none(),
            "a prevented read has no cost field to fill in: {v}"
        );
    }

    #[test]
    fn hook_json_is_well_formed_and_namespaced() {
        let m = MemdEvent::SummaryServed {
            handle: "summary:k".into(),
            estimated_tokens_returned: 30,
            estimated_tokens_avoided: 400,
            pull_claim: None,
        };
        let json = m.to_hook_json();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["verb"], "memd.summary_served");
        assert_eq!(v["payload"]["estimated_tokens_avoided"], 400);
        assert!(v["idempotency_key"].as_str().unwrap().len() == 64);
    }

    #[test]
    fn exact_event_carries_reason() {
        let e = MemdEvent::ExactRetrieved {
            handle: "file:a@sha256:x".into(),
            reason: "investigating regression".into(),
            estimated_tokens_returned: 10,
        };
        let json = e.to_hook_json();
        assert!(json.contains("investigating regression"));
    }

    #[test]
    fn idempotency_key_is_stable_for_same_event() {
        let a = MemdEvent::Invalidated {
            handle: "summary:k".into(),
            estimated_tokens_returned: 9,
        };
        let b = MemdEvent::Invalidated {
            handle: "summary:k".into(),
            estimated_tokens_returned: 9,
        };
        assert_eq!(a.to_hook_json(), b.to_hook_json());
    }

    #[test]
    fn call_identity_is_thread_scoped_and_ordinal_sensitive() {
        let a = Schema2ThreadId::from_seed(b"process-a");
        let a_again = Schema2ThreadId::from_seed(b"process-a");
        let b = Schema2ThreadId::from_seed(b"process-b");
        assert_eq!(a, a_again);
        assert_ne!(a, b);
        assert_eq!(Schema2CallId::derive(a, 1), Schema2CallId::derive(a, 1));
        assert_ne!(Schema2CallId::derive(a, 1), Schema2CallId::derive(a, 2));
        assert_ne!(Schema2CallId::derive(a, 1), Schema2CallId::derive(b, 1));
    }

    #[test]
    fn v2_terminal_envelope_is_invocation_addressed_and_neutral() {
        let thread = Schema2ThreadId::from_seed(b"process");
        let call = Schema2CallContext {
            thread_id: thread,
            call_id: Schema2CallId::derive(thread, 7),
            tool_name: "rift_read_summary".into(),
        };
        let event = MemdEvent::SummaryServed {
            handle: "summary:neutral".into(),
            estimated_tokens_returned: 12,
            estimated_tokens_avoided: 90,
            pull_claim: None,
        };
        let json = Schema2Terminal::Event(&event)
            .to_hook_json(&call, rift_core::Timestamp::from_millis(123));
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["schema"], 2);
        assert_eq!(value["idempotency_key"], call.call_id.to_hex());
        assert_eq!(value["call"]["thread_id"], thread.to_hex());
        assert_eq!(value["call"]["tool_name"], "rift_read_summary");
        assert_eq!(value["call"]["recorded_at"], 123);
        assert_eq!(value["call"]["tokens_returned"], 12);
        assert_eq!(value["call"]["tokens_avoided"], 90);
        assert!(!json.contains("arguments"));
        assert!(!json.contains("transcript"));

        let internal = Schema2Terminal::InternalError
            .to_hook_json(&call, rift_core::Timestamp::from_millis(124));
        assert!(internal.contains("memd.internal_error"));
        assert!(!internal.contains("error message"));
    }

    #[test]
    fn pack_built_carries_the_returned_figure() {
        // The accessor reads the seam's measure, never `total_tokens`,
        // and both hook envelopes (v1 payload, v2 call) carry it.
        let e = MemdEvent::PackBuilt {
            sections: 3,
            total_tokens: 120,
            estimated_tokens_returned: 480,
            estimated_tokens_avoided: 900,
            pull_claim: None,
        };
        assert_eq!(e.tokens_returned(), 480);
        let v: serde_json::Value = serde_json::from_str(&e.to_hook_json()).unwrap();
        assert_eq!(v["payload"]["estimated_tokens_returned"], 480);
        assert_eq!(v["payload"]["total_tokens"], 120);
        let thread = Schema2ThreadId::from_seed(b"process");
        let call = Schema2CallContext {
            thread_id: thread,
            call_id: Schema2CallId::derive(thread, 1),
            tool_name: "rift_context_pack".into(),
        };
        let v2: serde_json::Value = serde_json::from_str(
            &Schema2Terminal::Event(&e).to_hook_json(&call, rift_core::Timestamp::from_millis(1)),
        )
        .unwrap();
        assert_eq!(v2["call"]["tokens_returned"], 480);
        assert_eq!(v2["call"]["tokens_avoided"], 900);
    }

    #[test]
    fn refused_event_is_namespaced_and_leaks_no_content() {
        let e = MemdEvent::Refused {
            target: "file:secrets/.env@sha256:ab".into(),
            reason: "policy_denied_exact".into(),
            estimated_tokens_returned: 40,
        };
        assert_eq!(e.verb(), "refused");
        assert_eq!(e.tokens_avoided(), 0);
        // The refusal's own payload cost is audited, never the
        // withheld bytes.
        assert_eq!(e.tokens_returned(), 40);
        let v: serde_json::Value = serde_json::from_str(&e.to_hook_json()).unwrap();
        assert_eq!(v["verb"], "memd.refused");
        // the reason names *why*, never the withheld bytes
        assert_eq!(v["payload"]["reason"], "policy_denied_exact");
    }

    #[test]
    fn serve_channel_tokens_round_trip_and_refuse_everything_else() {
        for c in [ServeChannel::HookInject, ServeChannel::SkillCli] {
            assert_eq!(ServeChannel::parse(c.token()), Some(c));
            assert_eq!(c.to_string(), c.token());
        }
        assert_ne!(
            ServeChannel::HookInject.token(),
            ServeChannel::SkillCli.token()
        );
        for junk in ["skill_cli2", "hook-inject", "HookInject", ""] {
            assert_eq!(ServeChannel::parse(junk), None, "{junk}");
        }
    }

    /// The schema-4 envelope carries the channel identity
    /// AND the same figures the schema-2/`Schema2Terminal` path carries — the
    /// successor relation must not lose accounting fidelity to gain the axis.
    #[test]
    fn schema_4_envelope_carries_the_channel_and_the_call_identity() {
        let e = MemdEvent::SummaryServed {
            handle: "summary:neutral".into(),
            estimated_tokens_returned: 12,
            estimated_tokens_avoided: 90,
            pull_claim: None,
        };
        let json = e.to_serve_call_json(
            ServeChannel::SkillCli,
            "call-1",
            "rift_read_summary",
            rift_core::Timestamp::from_millis(123),
        );
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["schema"], 4);
        assert_eq!(v["verb"], "memd.summary_served");
        assert_eq!(v["idempotency_key"], "call-1");
        assert_eq!(v["serve_call"]["call_id"], "call-1");
        assert_eq!(v["serve_call"]["channel"], "skill_cli");
        assert_eq!(v["serve_call"]["tool_name"], "rift_read_summary");
        assert_eq!(v["serve_call"]["recorded_at"], 123);
        assert_eq!(v["serve_call"]["tokens_returned"], 12);
        assert_eq!(v["serve_call"]["tokens_avoided"], 90);
        // The two channels are two different envelopes, not two labels on one.
        let hook = e.to_serve_call_json(
            ServeChannel::HookInject,
            "call-1",
            "rift_read_summary",
            rift_core::Timestamp::from_millis(123),
        );
        assert_ne!(json, hook);
    }
}
