//! The artifacts a checkpoint archive renders: `metadata.json`, the
//! redaction report, the jsonl documents, the digest documents, the artifact
//! map and `content_hash.txt`, plus the canonical serializers they share.
//! Separate from the archived types because every byte here is pinned by a
//! golden, and the pinned derivations read better beside each other than
//! beside the fields they read.

use std::collections::BTreeMap;

use rift_core::Sha256Id;

use super::leaves::{DigestLeafBytes, SessionMetaLeaf, ShardLeafBytes, TaskLeafBytes};
use super::{
    session_worktrees_jsonl_bytes, sessions_jsonl_bytes, shard_merkle_root, tool_calls_jsonl_bytes,
    tool_results_jsonl_bytes, usage_jsonl_bytes, window_journal_jsonl_bytes, ArchivedTask,
    ArchivedTranscript, CheckpointArchive, NATIVE_CANON_VERSION, TRANSCRIPT_COMPACT_NAME,
    TRANSCRIPT_FULL_NAME,
};
use crate::sha256_hex;

/// The shard name of the frozen serve envelopes. It sorts after
/// `redaction_report.json` (`r` < `s`) and before `sessions/` (`r` < `s` at the
/// third byte), which is where its leaf sits.
pub(super) const SERVE_AUDIT_NAME: &str = "serve_audit.jsonl";

/// The shard name of the archived sessions' agents. It sorts after
/// `serve_audit.jsonl` and before the `sessions/` tree (`.` < `/`), which is
/// where its leaf sits.
pub(super) const SESSIONS_NAME: &str = "sessions.jsonl";

/// The shard name of the frozen worktree observations. It sorts after
/// `serve_audit.jsonl` and before `sessions.jsonl` (`_` < `s` at the eighth
/// byte), which is where its leaf sits.
pub(super) const SESSION_WORKTREES_NAME: &str = "session_worktrees.jsonl";

/// The shard name of the frozen settled usage. It sorts after
/// `transcript.jsonl` (`t` < `u`), after every other top-level name, so its
/// leaf trails the sequence.
pub(super) const USAGE_NAME: &str = "usage.jsonl";

/// The shard name of the frozen tool results. It sorts after
/// `tool_calls.jsonl` (`c` < `r` at the sixth byte) and before
/// `transcript.compact.jsonl` (`o` < `r` at the second), which is where its
/// leaf sits.
pub(super) const TOOL_RESULTS_NAME: &str = "tool_results.jsonl";

/// The shard name of the frozen window journals. It sorts after `usage.jsonl`
/// (`u` < `w`), after every other top-level name, so its leaf is the last.
pub(super) const WINDOW_JOURNAL_NAME: &str = "window_journal.jsonl";

impl CheckpointArchive {
    /// This shard's Merkle root — **derived from the bytes, never supplied.**
    ///
    /// It used to be a field, and every production caller filled it with
    /// `checkpoint_merkle_root(&[])` because no event stream was threaded: one
    /// constant across every archived shard, so the recorded root committed to
    /// nothing and `rift verify` had nothing to catch. A method that reads the
    /// archive's own artifact bytes is why no caller can record a root that
    /// disagrees with what it wrote — the disagreement is not reachable rather
    /// than merely tested for.
    #[must_use]
    pub fn merkle_root(&self) -> Sha256Id {
        let full = self.full_jsonl();
        let report = self.redaction_report_json();
        let tool_calls = self.tool_calls_jsonl();
        let tool_results = self.tool_results_jsonl();
        let usage = self.usage_jsonl();
        let sessions = self.sessions_jsonl();
        let session_worktrees = self.session_worktrees_jsonl();
        let window_journal = self.window_journal_jsonl();
        let digest_failures = self.digest_failures_json();
        let digest_intent = self.digest_intent_json();
        let digest_log = self.digest_log_json();
        let digest_shape = self.digest_shape_json();
        // Session/task metadata are rendered by the same functions
        // `write_shard` commits, so the root's leaves and the tree's bytes
        // cannot drift apart.
        let session_meta_docs = self.session_meta_docs();
        let session_meta_leaves: Vec<SessionMetaLeaf<'_>> = session_meta_docs
            .iter()
            .map(|(name, bytes)| SessionMetaLeaf {
                index_name: name,
                metadata: bytes,
            })
            .collect();
        let task_metas: Vec<Vec<u8>> = self.tasks.iter().map(ArchivedTask::metadata_json).collect();
        let task_leaves: Vec<TaskLeafBytes<'_>> = self
            .tasks
            .iter()
            .zip(&task_metas)
            .map(|(t, meta)| TaskLeafBytes {
                tool_use_id: &t.tool_use_id,
                metadata: meta,
                transcript_compact: t.transcript.as_ref().map(|tr| tr.compact.as_slice()),
                transcript_full: t.transcript.as_ref().map(|tr| tr.full.as_slice()),
            })
            .collect();
        let spill_leaves = self.spill_leaves();
        let original_leaves = self.original_leaves();
        shard_merkle_root(&ShardLeafBytes {
            boundary_gaps: self
                .boundary_gaps
                .as_ref()
                .map(super::FrozenBoundaryGaps::as_bytes),
            content_redacted: self.content.as_slice(),
            full_jsonl: full.as_deref(),
            redaction_report: &report,
            serve_audit: self.serve_audit_jsonl(),
            session_worktrees: session_worktrees.as_deref(),
            sessions: sessions.as_deref(),
            tool_calls: tool_calls.as_deref(),
            tool_results: tool_results.as_deref(),
            transcript_compact: self.transcript.as_ref().map(|t| t.compact.as_slice()),
            transcript_full: self.transcript.as_ref().map(|t| t.full.as_slice()),
            usage: usage.as_deref(),
            window_journal: window_journal.as_deref(),
            tasks: &task_leaves,
            spills: &spill_leaves,
            originals: &original_leaves,
            session_metas: &session_meta_leaves,
            digests: DigestLeafBytes {
                failures: digest_failures.as_deref(),
                intent: digest_intent.as_deref(),
                log: digest_log.as_deref(),
                shape: digest_shape.as_deref(),
            },
        })
    }

    /// The `(index_name, bytes)` pairs of this checkpoint's
    /// `sessions/<index>/metadata.json` documents — **empty below two
    /// sessions**, which is the compatibility contract: a single-session
    /// shard writes no session dirs and its tree, leaves, root and id are
    /// exactly what they always were.
    #[must_use]
    pub fn session_meta_docs(&self) -> Vec<(String, Vec<u8>)> {
        if self.sessions.len() < 2 {
            return Vec::new();
        }
        self.sessions
            .iter()
            .enumerate()
            .map(|(i, sid)| (i.to_string(), session_meta_json(i, sid)))
            .collect()
    }

    /// **The checkpoint id these artifacts address** — the single writer of a new
    /// id, derived from this archive's own Merkle root.
    ///
    /// It is a method on the archive rather than a function beside it so the id
    /// and the bytes cannot be assembled from two different field sets: whatever
    /// [`Self::merkle_root`] commits to is exactly what the id commits to,
    /// because it is the same call.
    ///
    /// `None` for a checkpoint whose session set the rule refuses: an empty
    /// list, or a session id carrying NUL (the frame byte). N ≥ 2 is DEFINED
    /// — [`rift_core::checkpoint_id::from_root_sessions`],
    /// whose N = 1 arm is byte-identical to the shipped epoch-3 rule, which is
    /// why defining it needed no era bump.
    #[must_use]
    pub fn derived_checkpoint_id(&self) -> Option<String> {
        let sessions: Vec<&str> = self.sessions.iter().map(String::as_str).collect();
        rift_core::checkpoint_id::from_root_sessions(&sessions, self.merkle_root())
    }

    /// **The task-token rollup**: the sum of every archived
    /// task's agent-reported token counts, or `None` when no task carries one —
    /// so a token-less task set adds no metadata field and a taskless shard's
    /// metadata is untouched.
    ///
    /// One writer: `metadata_json` records exactly this value, and `rift
    /// verify` re-sums the task subtrees' own metadata (Merkle leaves) against
    /// the recorded field — so a rollup that lies about the leaves it sums is
    /// tamper-evident even though the parent metadata itself is not a leaf.
    ///
    /// Deliberately NOT gated on when a task started: a subagent that began
    /// before this checkpoint's boundary still rolls up to the checkpoint that
    /// saw it finish. The pin for that clause lives in `condense::tests`.
    #[must_use]
    pub fn task_token_rollup(&self) -> Option<(u64, u64)> {
        if self
            .tasks
            .iter()
            .all(|t| t.input_tokens.is_none() && t.output_tokens.is_none())
        {
            return None;
        }
        Some(self.tasks.iter().fold((0u64, 0u64), |(i, o), t| {
            (
                i.saturating_add(t.input_tokens.unwrap_or(0)),
                o.saturating_add(t.output_tokens.unwrap_or(0)),
            )
        }))
    }

    /// The `<id[:2]>/<id[2:]>` shard path this checkpoint lands at (LAYOUT).
    #[must_use]
    pub fn shard_path(&self) -> String {
        let id = &self.checkpoint_id;
        match (id.get(..2), id.get(2..)) {
            (Some(hi), Some(rest)) => format!("{hi}/{rest}"),
            _ => id.clone(),
        }
    }

    /// The canonical `metadata.json` bytes (deterministic key order). Carries the
    /// `canon_version` epoch ([`NATIVE_CANON_VERSION`]) so the redaction→leaf
    /// migration is explicit: old (legacy backend) checkpoints lack it and read as epoch 0,
    /// and the vacuous-Merkle era is epoch 1.
    ///
    /// Every optional member is absent-when-empty, so an archive that carries
    /// none of them writes the document every committed anchor and golden
    /// froze; the section inserters below each state that contract for the
    /// member they own.
    #[must_use]
    pub fn metadata_json(&self) -> Vec<u8> {
        let mut obj = serde_json::Map::new();
        obj.insert("canon_version".into(), NATIVE_CANON_VERSION.into());
        obj.insert("strategy".into(), self.strategy.clone().into());
        obj.insert("merkle_root".into(), self.merkle_root().to_hex().into());
        self.insert_created_at(&mut obj);
        if let Some(prev) = &self.prev_hash {
            obj.insert("prev_hash".into(), prev.clone().into());
        }
        // The compact path + boundary metadata is required to carry. Present
        // only when the shard carries the artifacts, so a transcript-less
        // shard's metadata is byte-identical to what it was before the pair
        // existed (the anchors' compatibility claim).
        if let Some(t) = &self.transcript {
            insert_compact_boundary(&mut obj, t);
        }
        self.insert_task_members(&mut obj);
        obj.insert(
            "sessions".into(),
            serde_json::Value::from(self.sessions.clone()),
        );
        // Absent-when-empty like every optional member: an anchorless shard
        // (unborn HEAD) writes no key rather than a sentinel a reader would
        // have to screen and name.
        if let Some(commit) = &self.commit {
            obj.insert("commit".into(), commit.clone().into());
        }
        self.insert_worktree_members(&mut obj);
        if let Some(oid) = &self.observed_head {
            obj.insert("observed_head".into(), oid.clone().into());
        }
        canonical_json(&serde_json::Value::Object(obj))
    }

    /// The canonical `redaction_report.json` bytes (valid for the strict bridge gate).
    #[must_use]
    pub fn redaction_report_json(&self) -> Vec<u8> {
        let r = &self.report;
        let mut obj = serde_json::json!({
            "total_spans": r.total_spans,
            "bytes_masked": r.bytes_masked,
            "allowlisted": r.allowlisted,
            "rule_pack_versions": r.rule_pack_versions,
            "input_sha256": r.input_sha256,
            "by_detector": r.by_detector,
            "by_class": r.by_class,
        });
        if let Some(fields) = obj.as_object_mut() {
            if let Some(transcript) = &self.transcript {
                insert_fallback_counts(fields, transcript.redaction_fallback);
            }
        }
        canonical_json(&obj)
    }

    /// Render `tool_calls.jsonl` — one line per captured read fact, in the
    /// field spelling the recall parser's `ToolCallRecord` aliases accept.
    /// `None` when the session produced no read facts (the artifact is omitted,
    /// never written empty).
    #[must_use]
    pub fn tool_calls_jsonl(&self) -> Option<Vec<u8>> {
        tool_calls_jsonl_bytes(&self.read_facts)
    }

    /// Render `sessions.jsonl` — one line per archived session naming its
    /// agent. `None` when the archive names no agent (the artifact is
    /// omitted, never written empty).
    #[must_use]
    pub fn sessions_jsonl(&self) -> Option<Vec<u8>> {
        sessions_jsonl_bytes(&self.session_agents)
    }

    /// Render `session_worktrees.jsonl` — one line per observation of the
    /// member a session was seen in. `None` when there is none it can freeze
    /// (the artifact is omitted, never written empty).
    #[must_use]
    pub fn session_worktrees_jsonl(&self) -> Option<Vec<u8>> {
        session_worktrees_jsonl_bytes(&self.worktree_observations)
    }

    /// Render `tool_results.jsonl` — one line per measured tool result.
    /// `None` when there is none it can freeze (the artifact is omitted,
    /// never written empty).
    #[must_use]
    pub fn tool_results_jsonl(&self) -> Option<Vec<u8>> {
        tool_results_jsonl_bytes(&self.tool_results)
    }

    /// Render `usage.jsonl` — one line per settled model call. `None` when
    /// the session settled none (the artifact is omitted, never written empty).
    #[must_use]
    pub fn usage_jsonl(&self) -> Option<Vec<u8>> {
        usage_jsonl_bytes(&self.usage)
    }

    /// Render `window_journal.jsonl` — one line per row the session's context
    /// windows held. `None` when they held none it can freeze (the artifact is
    /// omitted, never written empty).
    #[must_use]
    pub fn window_journal_jsonl(&self) -> Option<Vec<u8>> {
        window_journal_jsonl_bytes(&self.window_journal)
    }

    /// `serve_audit.jsonl` — the session's frozen serve envelopes, or `None`
    /// when it froze none. An empty buffer reads as absent, so the artifact
    /// is never written empty and a serve-less shard keeps the leaf sequence
    /// it had before the artifact existed.
    #[must_use]
    pub fn serve_audit_jsonl(&self) -> Option<&[u8]> {
        match self.serve_audit.as_deref() {
            Some(bytes) if !bytes.is_empty() => Some(bytes),
            _ => None,
        }
    }

    /// The `full.jsonl` bytes (one canonical event per line), or `None` if empty.
    #[must_use]
    pub fn full_jsonl(&self) -> Option<Vec<u8>> {
        if self.events.is_empty() {
            return None;
        }
        let mut out = Vec::new();
        for (i, e) in self.events.iter().enumerate() {
            if i > 0 {
                out.push(b'\n');
            }
            let line = serde_json::json!({
                "session": e.session,
                "turn": e.turn_ordinal,
                "event": e.event_ordinal,
                "kind": e.kind,
                "verb": e.verb,
                "agent": e.agent,
                "occurred_at_ms": e.occurred_at_ms,
                "payload_sha256": e.payload_sha256,
                "payload": e.payload,
            });
            out.extend_from_slice(&canonical_json(&line));
        }
        Some(out)
    }

    /// `digest.failures.json`, or `None` without a
    /// transcript or without a failure — one derivation shared by condense,
    /// the trailer prediction and the shard writer, because it is a method of
    /// the same archive all three hold.
    #[must_use]
    pub fn digest_failures_json(&self) -> Option<Vec<u8>> {
        self.transcript
            .as_ref()
            .and_then(|t| crate::digest::failures_json(t.full.as_slice()))
    }

    /// `digest.intent.json` — present whenever the shard carries a
    /// transcript: the intent/outcome slot, filled by the redacted prompt's
    /// LOCATOR plus mechanical outcome facts, no model.
    #[must_use]
    pub fn digest_intent_json(&self) -> Option<Vec<u8>> {
        self.transcript
            .as_ref()
            .map(|t| crate::digest::intent_json(t.full.as_slice(), self.read_facts.len()))
    }

    /// `digest.log.json` — present whenever the shard carries a transcript.
    #[must_use]
    pub fn digest_log_json(&self) -> Option<Vec<u8>> {
        self.transcript.as_ref().map(|t| {
            crate::digest::log_json(
                self.content.as_slice().len(),
                t.full.as_slice(),
                t.compact.as_slice(),
                t.compact_start,
            )
        })
    }

    /// `digest.shape.json` — present whenever the session captured read facts.
    #[must_use]
    pub fn digest_shape_json(&self) -> Option<Vec<u8>> {
        crate::digest::shape_json(&self.read_facts)
    }

    /// **Every artifact this checkpoint writes, keyed by its shard-relative
    /// path** — the same set, the same bytes and the same paths
    /// `NativeBackend::write_shard` commits to the tree.
    ///
    /// It exists because the artifact NAME SET was unenumerated. The parity
    /// anchors compare four names, so `content.redacted`, the transcript pair,
    /// the four `digest.*` documents, the `tasks/<id>/` subtrees and the
    /// `sessions/<index>/` metadata could be added, changed or **planted**
    /// without moving a compared byte. [`crate::artifact_registry`] declares
    /// which names may exist; this says which ones a given shard carries; and
    /// `the_committed_shard_tree_is_exactly_the_declared_artifact_set`
    /// (`tests/native_archive_roundtrip.rs`) binds both to what git actually
    /// holds, so this cannot drift from `write_shard` unnoticed.
    ///
    /// The durable artifacts' bytes are COPIED out of their [`RedactedBytes`](rift_core::RedactedBytes).
    /// That is sound because this is a READ surface — goldens, oracles,
    /// assertions — and never a write path: `write_shard` still hands the typed
    /// value to `write_durable_blob`, which is the only door those bytes fit
    /// through, and nothing here can mint a [`RedactedBytes`](rift_core::RedactedBytes).
    #[must_use]
    pub fn artifact_map(&self) -> BTreeMap<String, Vec<u8>> {
        let mut out = BTreeMap::new();
        out.insert(
            "content.redacted".to_owned(),
            self.content.as_slice().to_vec(),
        );
        out.insert("content_hash.txt".to_owned(), self.content_hash_txt());
        out.insert("metadata.json".to_owned(), self.metadata_json());
        out.insert(
            "redaction_report.json".to_owned(),
            self.redaction_report_json(),
        );
        if let Some(full) = self.full_jsonl() {
            out.insert("full.jsonl".to_owned(), full);
        }
        if let Some(tool_calls) = self.tool_calls_jsonl() {
            out.insert("tool_calls.jsonl".to_owned(), tool_calls);
        }
        if let Some(results) = self.tool_results_jsonl() {
            out.insert(TOOL_RESULTS_NAME.to_owned(), results);
        }
        if let Some(serve_audit) = self.serve_audit_jsonl() {
            out.insert(SERVE_AUDIT_NAME.to_owned(), serve_audit.to_vec());
        }
        if let Some(t) = &self.transcript {
            insert_pair_artifacts(&mut out, "", t);
        }
        self.insert_boundary_gap_artifact(&mut out);
        if let Some(usage) = self.usage_jsonl() {
            out.insert(USAGE_NAME.to_owned(), usage);
        }
        if let Some(sessions) = self.sessions_jsonl() {
            out.insert(SESSIONS_NAME.to_owned(), sessions);
        }
        if let Some(observations) = self.session_worktrees_jsonl() {
            out.insert(SESSION_WORKTREES_NAME.to_owned(), observations);
        }
        if let Some(window_journal) = self.window_journal_jsonl() {
            out.insert(WINDOW_JOURNAL_NAME.to_owned(), window_journal);
        }
        for (name, bytes) in [
            (
                crate::digest::DIGEST_FAILURES_NAME,
                self.digest_failures_json(),
            ),
            (crate::digest::DIGEST_INTENT_NAME, self.digest_intent_json()),
            (crate::digest::DIGEST_LOG_NAME, self.digest_log_json()),
            (crate::digest::DIGEST_SHAPE_NAME, self.digest_shape_json()),
        ] {
            if let Some(bytes) = bytes {
                out.insert(name.to_owned(), bytes);
            }
        }
        for (index_name, bytes) in self.session_meta_docs() {
            out.insert(format!("sessions/{index_name}/metadata.json"), bytes);
        }
        for task in &self.tasks {
            let id = &task.tool_use_id;
            out.insert(format!("tasks/{id}/metadata.json"), task.metadata_json());
            if let Some(t) = &task.transcript {
                insert_pair_artifacts(&mut out, &format!("tasks/{id}/"), t);
            }
        }
        self.insert_spill_artifacts(&mut out);
        self.insert_original_artifacts(&mut out);
        out
    }

    /// The `content_hash.txt` bytes: the shard's content address over its ordered
    /// artifacts (metadata ‖ report ‖ full), lowercase hex + trailing newline.
    #[must_use]
    pub fn content_hash_txt(&self) -> Vec<u8> {
        let mut parts: Vec<Vec<u8>> = vec![self.metadata_json(), self.redaction_report_json()];
        if let Some(full) = self.full_jsonl() {
            parts.push(full);
        }
        let refs: Vec<&[u8]> = parts.iter().map(Vec::as_slice).collect();
        let mut hex = sha256_hex(&refs);
        hex.push('\n');
        hex.into_bytes()
    }

    /// The capture event's own recorded time. Present only when the
    /// archive carries one, so a time-less archive's metadata — including
    /// every committed anchor and golden — is byte-identical to what it
    /// always was (the same absent-when-empty rule as every optional member).
    /// Written in exactly the shape `created_at_for`'s lenient parse reads
    /// back, so the projection's `checkpoint.created_at` stops being the
    /// sentinel on native shards and the squash tie-break
    /// ranks by real creation time instead of degrading to trailer order.
    /// Not a Merkle leaf (`metadata.json` never is — it RECORDS the root),
    /// but inside `content_hash.txt`'s preimage like the rest of this
    /// document. No `canon_version` bump: the field changes no id
    /// derivation and no verification rule, and the reader has parsed it
    /// leniently since the bridge landed.
    fn insert_created_at(&self, obj: &mut serde_json::Map<String, serde_json::Value>) {
        if let Some(ms) = self.created_at_ms.filter(|ms| *ms > 0) {
            obj.insert("created_at".into(), rfc3339_utc_ms(ms).into());
        }
    }

    /// The task list — the reader's ENUMERATION of `tasks/<id>/` subtrees
    /// (`SubstrateReader` has no tree listing, and does not need one: hiding
    /// a listed task changes the leaves `rift verify` re-derives, so the
    /// root and the id both refuse). Bytewise id order = leaf order = tree
    /// order — one order, three places. Absent when no task, so a taskless
    /// shard's metadata is byte-identical to what it always was.
    ///
    /// Beside it the rollup — the parent-side sum of the tasks' own
    /// (leaf-bound) token counts, present only when at least one task
    /// reported any.
    fn insert_task_members(&self, obj: &mut serde_json::Map<String, serde_json::Value>) {
        if !self.tasks.is_empty() {
            let mut ids: Vec<&str> = self.tasks.iter().map(|t| t.tool_use_id.as_str()).collect();
            ids.sort_unstable_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
            obj.insert("tasks".into(), serde_json::Value::from(ids));
        }
        if let Some((input, output)) = self.task_token_rollup() {
            obj.insert(
                "task_tokens".into(),
                serde_json::json!({"input_tokens": input, "output_tokens": output}),
            );
        }
    }

    /// WHICH WORKTREE ARCHIVED THIS SHARD. Name, token,
    /// linked-ness and branch — no absolute path and no username, because
    /// this document crosses machines inside a transfer bundle. Present
    /// only when the archiving worktree is known, so a label-less archive's
    /// metadata — every committed anchor and golden among them — is
    /// byte-identical to what it always was; the per-session labels of a
    /// multi-session shard follow the same rule, and an empty map writes no
    /// key at all.
    ///
    /// Not a Merkle leaf (`metadata.json` never is — it RECORDS the root),
    /// but inside `content_hash.txt`'s preimage like the rest of this
    /// document. **No `canon_version` bump**: the member changes no id
    /// derivation and no verification rule — the same reasoning, in the
    /// same words, that `created_at` was added under.
    fn insert_worktree_members(&self, obj: &mut serde_json::Map<String, serde_json::Value>) {
        if let Some(label) = &self.worktree {
            obj.insert("worktree".into(), label.to_json());
        }
        if !self.session_worktrees.is_empty() {
            obj.insert(
                "session_worktrees".into(),
                serde_json::Value::Object(
                    self.session_worktrees
                        .iter()
                        .map(|(session, label)| (session.clone(), label.to_json()))
                        .collect(),
                ),
            );
        }
    }
}

/// The canonical `sessions/<index>/metadata.json` bytes of one session of a
/// multi-session checkpoint: the session's **stable 0-based
/// index** (its position in the parent metadata's `sessions` array — assigned
/// once, never re-ordered by a later session joining) and its id. A new
/// serialized artifact, so it carries `schema_version`.
#[must_use]
pub fn session_meta_json(index: usize, session_id: &str) -> Vec<u8> {
    let mut obj = serde_json::Map::new();
    obj.insert("schema_version".into(), 1u32.into());
    obj.insert("index".into(), index.into());
    obj.insert("session_id".into(), session_id.to_owned().into());
    canonical_json(&serde_json::Value::Object(obj))
}

/// Render a positive epoch-ms instant as `YYYY-MM-DDTHH:MM:SS.mmmZ` — the
/// exact subset [`crate::artifact_bridge`]'s lenient view parses back, so
/// `parse(format(ms)) == ms` for every value this writer can emit (the
/// round-trip is pinned in the bridge's tests). Civil-from-days is Howard
/// Hinnant's algorithm over the proleptic Gregorian calendar — the inverse of
/// the parser's days-from-civil, and the same one-field-no-date-dependency
/// call.
pub(crate) fn rfc3339_utc_ms(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let millis = ms.rem_euclid(1000);
    let days = secs.div_euclid(86_400);
    let tod = secs.rem_euclid(86_400);
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{millis:03}Z",
        tod / 3600,
        (tod % 3600) / 60,
        tod % 60
    )
}

/// Record a transcript pair's compact name and boundary on a metadata
/// document — the parent's and every task's, in one spelling, so the two
/// documents cannot record the pair two ways.
/// A transcript pair's artifact-map entries under `dir`: each half's blob, or
/// its segments, named as `write_shard` names them.
fn insert_pair_artifacts(out: &mut BTreeMap<String, Vec<u8>>, dir: &str, t: &ArchivedTranscript) {
    super::segments::insert_body_artifacts(out, dir, TRANSCRIPT_COMPACT_NAME, &t.compact);
    super::segments::insert_body_artifacts(out, dir, TRANSCRIPT_FULL_NAME, &t.full);
}

pub(super) fn insert_compact_boundary(
    obj: &mut serde_json::Map<String, serde_json::Value>,
    transcript: &ArchivedTranscript,
) {
    obj.insert("compact_transcript".into(), TRANSCRIPT_COMPACT_NAME.into());
    obj.insert(
        "compact_transcript_start".into(),
        transcript.compact_start.into(),
    );
    super::spills::insert_spill_manifest(obj, &transcript.spills);
    super::originals::insert_original_manifest(obj, &transcript.originals);
    insert_fallback_counts(obj, transcript.redaction_fallback);
    super::segments::insert_segment_members(obj, &transcript.full);
    super::segments::insert_segment_members(obj, &transcript.compact);
}

/// The same optional paired grammar in the bound report and metadata mirrors.
fn insert_fallback_counts(
    obj: &mut serde_json::Map<String, serde_json::Value>,
    fallback: super::RedactionFallback,
) {
    if fallback.lines > 0 || fallback.placeholders > 0 {
        obj.insert("redaction_flat_fallback".into(), fallback.lines.into());
        obj.insert(
            "redaction_fallback_placeholders".into(),
            fallback.placeholders.into(),
        );
    }
}

/// Serialize a JSON value to canonical bytes: `serde_json`'s default `Map` is a
/// `BTreeMap` (keys sorted), so this is deterministic byte-for-byte.
pub(super) fn canonical_json(v: &serde_json::Value) -> Vec<u8> {
    serde_json::to_vec(v).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::{archive, task, transcript_of};
    use super::super::{
        shard_merkle_leaves, ArchivedEvent, ArchivedReadFact, NativeBackend, ShadowBackend,
        WorktreeLabel, ROOT_BOUND_ID_MIN_CANON_VERSION, STRICT_MERKLE_MIN_CANON_VERSION,
    };
    use super::*;
    use rift_core::SecretBytes;

    /// **The sequence `content_hash.txt` digests, pinned by arithmetic rather
    /// than by prose.** Two descriptions of this one artifact lived a screen
    /// apart in this file and disagreed: the writer said
    /// `metadata ‖ report ‖ full`, the leaf-spec docstring said "a digest over
    /// `metadata.json`". The second was inherited by a published format
    /// registry, where a blind second implementation would have written a shard
    /// nothing could address. Measured on the reference branch, the
    /// single-artifact reading matches **0 of 104** shards.
    #[test]
    fn content_hash_covers_metadata_report_and_full_in_that_order() {
        let expect = |a: &CheckpointArchive| -> Vec<u8> {
            let mut parts: Vec<Vec<u8>> = vec![a.metadata_json(), a.redaction_report_json()];
            if let Some(full) = a.full_jsonl() {
                parts.push(full);
            }
            let refs: Vec<&[u8]> = parts.iter().map(Vec::as_slice).collect();
            let mut hex = sha256_hex(&refs);
            hex.push('\n');
            hex.into_bytes()
        };

        // (a) WITHOUT `full.jsonl` — the shape 103 of the 104 live shards have,
        //     and the shape under which the false sentence is least visible.
        let bare = archive();
        assert!(bare.full_jsonl().is_none(), "the control carries no events");
        assert_eq!(bare.content_hash_txt(), expect(&bare));
        // The single-artifact reading is a DIFFERENT value, so the test would
        // pass under it only if the two readings coincided.
        let metadata_only = {
            let mut hex = sha256_hex(&[&bare.metadata_json()]);
            hex.push('\n');
            hex.into_bytes()
        };
        assert_ne!(
            bare.content_hash_txt(),
            metadata_only,
            "a digest over `metadata.json` alone is not this artifact"
        );

        // (b) WITH `full.jsonl` — the third member of the sequence, which is
        //     what makes it a sequence at all.
        let mut with_full = archive();
        with_full.events = vec![ArchivedEvent {
            session: "native-1".into(),
            turn_ordinal: 0,
            event_ordinal: 0,
            kind: "session_start".into(),
            verb: "SessionStart".into(),
            agent: "claude-code".into(),
            occurred_at_ms: 1,
            payload_sha256: "a".repeat(64),
            payload: serde_json::json!({}),
        }];
        assert!(with_full.full_jsonl().is_some());
        assert_eq!(with_full.content_hash_txt(), expect(&with_full));
        assert_ne!(
            with_full.content_hash_txt(),
            bare.content_hash_txt(),
            "the third artifact must be inside the digest"
        );

        // …and ORDER is part of it: report-then-metadata is a different value.
        let swapped = {
            let mut hex = sha256_hex(&[&bare.redaction_report_json(), &bare.metadata_json()]);
            hex.push('\n');
            hex.into_bytes()
        };
        assert_ne!(bare.content_hash_txt(), swapped);

        // The trailing newline is part of the artifact — a reader that trims it
        // computes a different file.
        assert_eq!(bare.content_hash_txt().last(), Some(&b'\n'));
        assert_eq!(bare.content_hash_txt().len(), 65);
    }

    /// **`metadata.json` records the capture event's own time,
    /// absent-when-empty**: `Some` emits `created_at` in exactly the
    /// shape the bridge's lenient view parses back into
    /// `checkpoint.created_at`; `None` — and the hook's 0 for an unreadable
    /// clock — omits the key, so a time-less archive's metadata is
    /// byte-identical to what every pre-`created_at` shard wrote. That
    /// absent-when-empty rule is what every optional member of this document
    /// follows, and it is the reason no committed anchor or golden moved when
    /// the field landed.
    #[test]
    fn metadata_records_the_capture_time_and_omits_it_when_unknown() {
        let dated = archive();
        let doc: serde_json::Value = serde_json::from_slice(&dated.metadata_json()).unwrap();
        assert_eq!(doc["created_at"], "2023-11-14T22:13:20.000Z");

        // Byte-level, not just key-level: the undated document is the dated
        // one minus exactly the `created_at` member.
        let mut undated = archive();
        undated.created_at_ms = None;
        let undoc: serde_json::Value = serde_json::from_slice(&undated.metadata_json()).unwrap();
        assert!(undoc.get("created_at").is_none(), "{undoc}");
        let dated_bytes = String::from_utf8(dated.metadata_json()).unwrap();
        let undated_bytes = String::from_utf8(undated.metadata_json()).unwrap();
        assert_eq!(
            dated_bytes.replace("\"created_at\":\"2023-11-14T22:13:20.000Z\",", ""),
            undated_bytes,
        );

        // An unreadable clock (`now_ms`'s 0) and a negative instant both read
        // as "no recorded time", never as an epoch date.
        for ms in [Some(0), Some(-5)] {
            let mut a = archive();
            a.created_at_ms = ms;
            let v: serde_json::Value = serde_json::from_slice(&a.metadata_json()).unwrap();
            assert!(v.get("created_at").is_none(), "{ms:?}");
        }

        // The time is NOT part of the identity — the anchoring-commit rule,
        // applied to the clock: same capture at a different instant keeps the
        // root and the id, while the document and `content_hash.txt` both
        // record that the instant differed.
        let mut later = archive();
        later.created_at_ms = Some(1_700_000_000_000 + 60_000);
        assert_eq!(later.merkle_root(), dated.merkle_root());
        assert_eq!(later.derived_checkpoint_id(), dated.derived_checkpoint_id());
        assert_ne!(later.metadata_json(), dated.metadata_json());
        assert_ne!(later.content_hash_txt(), dated.content_hash_txt());
    }

    /// The rollup's arithmetic and its absence contract: sums treat a missing
    /// count as zero, and a task set with NO reported counts records no field
    /// at all — so token-less shards' metadata bytes never moved.
    #[test]
    fn the_task_token_rollup_sums_reported_counts_and_omits_when_none() {
        let mut a = archive();
        a.tasks = vec![task("toolu_a"), task("toolu_b")];
        assert_eq!(a.task_token_rollup(), None, "no task reported tokens");
        assert!(
            !String::from_utf8(a.metadata_json())
                .unwrap()
                .contains("task_tokens"),
            "no field when nothing was reported"
        );

        a.tasks[0].input_tokens = Some(100);
        a.tasks[0].output_tokens = Some(7);
        a.tasks[1].input_tokens = Some(20);
        // tasks[1].output_tokens stays None — sums as zero.
        assert_eq!(a.task_token_rollup(), Some((120, 7)));
        let meta = String::from_utf8(a.metadata_json()).unwrap();
        assert!(
            meta.contains(r#""task_tokens":{"input_tokens":120,"output_tokens":7}"#),
            "{meta}"
        );
    }

    /// **The multi-session layout + order rules, in one place.** A
    /// multi-session checkpoint writes `sessions/<index>/metadata.json` per
    /// session (stable 0-based indexes = positions in `sessions`), those
    /// documents are leaves between `redaction_report.json` and the task
    /// subtrees, their leaf order is git's BYTEWISE name order — `"10"` sorts
    /// before `"2"`, pinned so nobody "fixes" it to numeric — and a
    /// single-session shard writes none of it, so every existing shard's tree
    /// and leaf sequence are untouched.
    #[test]
    fn a_multi_session_shard_writes_indexed_session_dirs_and_orders_leaves_bytewise() {
        use rift_hashchain::leaf_hash;

        // Single-session: no dirs, no leaves — the compat half.
        let single = archive();
        assert!(single.session_meta_docs().is_empty());

        let mut multi = archive();
        multi.sessions = (0..11).map(|i| format!("01JSESSION{i:02}")).collect();
        let docs = multi.session_meta_docs();
        assert_eq!(docs.len(), 11);
        assert_eq!(docs[0].0, "0");
        assert_eq!(docs[10].0, "10");
        // The document binds index and id both.
        assert_eq!(
            docs[10].1,
            session_meta_json(10, "01JSESSION10"),
            "one renderer for the writer and the tests"
        );

        // The leaf order is BYTEWISE on the decimal name: 0,1,10,2,…
        let metas: Vec<SessionMetaLeaf<'_>> = docs
            .iter()
            .map(|(name, bytes)| SessionMetaLeaf {
                index_name: name,
                metadata: bytes,
            })
            .collect();
        let leaves = shard_merkle_leaves(&ShardLeafBytes {
            boundary_gaps: None,
            content_redacted: b"c",
            redaction_report: b"r",
            session_metas: &metas,
            ..ShardLeafBytes::default()
        });
        assert_eq!(leaves[0], leaf_hash(b"c"));
        assert_eq!(leaves[1], leaf_hash(b"r"));
        assert_eq!(leaves[2], leaf_hash(&docs[0].1), "index 0 first");
        assert_eq!(leaves[3], leaf_hash(&docs[1].1), "then 1");
        assert_eq!(leaves[4], leaf_hash(&docs[10].1), "then 10 — BYTEWISE");
        assert_eq!(leaves[5], leaf_hash(&docs[2].1), "then 2");

        // The writer writes the dirs (2 sessions ⇒ sessions/0 + sessions/1).
        let n = NativeBackend::in_memory();
        let mut a = archive();
        a.sessions = vec!["01JFIRST".to_owned(), "01JSECOND".to_owned()];
        a.checkpoint_id = a.derived_checkpoint_id().unwrap();
        n.archive_checkpoint(&a).unwrap();
        // content_hash + metadata + report + 2 session metas = 5 structural.
        assert_eq!(n.writer().structural_count(), 5);
        // leaf tree + sessions/ tree + 2 index trees = 4 trees.
        assert_eq!(n.writer().tree_count(), 4);
    }

    /// **The digest artifacts' binding signal test, the same shape as the task subtree's.**
    /// The digest artifacts are Merkle leaves: their bytes sit in the leaf
    /// sequence right after `content.redacted` (their bytewise name position),
    /// they move the checkpoint id through the root, and the writer commits
    /// them to the tree — so an edited stored digest stops the shard
    /// addressing its own path exactly like any other leaf.
    #[test]
    fn a_digest_artifact_joins_the_leaf_spec_and_moves_the_id() {
        use rift_hashchain::leaf_hash;

        let mut with_facts = archive();
        with_facts.read_facts = vec![ArchivedReadFact {
            tool_use_id: "t1".into(),
            tool_name: "Read".into(),
            path: "src/a.rs".into(),
            content_sha256: "aa".into(),
            input_tokens: None,
            at: None,
            turn: None,
            blob_oid: None,
            tokenizer: None,
        }];
        let shape = with_facts
            .digest_shape_json()
            .expect("facts => shape digest");
        let tool_calls = with_facts.tool_calls_jsonl().unwrap();
        let report = with_facts.redaction_report_json();

        // The leaf sequence, hand-built: content, then the digest (its
        // bytewise position — after content_hash.txt, before full.jsonl),
        // then report, then tool_calls.
        let leaves = shard_merkle_leaves(&ShardLeafBytes {
            boundary_gaps: None,
            content_redacted: with_facts.content.as_slice(),
            redaction_report: &report,
            tool_calls: Some(&tool_calls),
            digests: DigestLeafBytes {
                shape: Some(&shape),
                ..DigestLeafBytes::default()
            },
            ..ShardLeafBytes::default()
        });
        assert_eq!(
            leaves,
            vec![
                leaf_hash(with_facts.content.as_slice()),
                leaf_hash(&shape),
                leaf_hash(&report),
                leaf_hash(&tool_calls),
            ],
            "digest leaves sit right after content.redacted"
        );
        // …and the archive derives its id THROUGH that sequence: the same
        // artifacts minus the digest leaf address a different path.
        let with_digest_root = with_facts.merkle_root();
        let without_digest_root = shard_merkle_root(&ShardLeafBytes {
            boundary_gaps: None,
            content_redacted: with_facts.content.as_slice(),
            redaction_report: &report,
            tool_calls: Some(&tool_calls),
            ..ShardLeafBytes::default()
        });
        assert_ne!(with_digest_root, without_digest_root, "the digest is bound");

        // The writer commits it: content_hash + metadata + report +
        // tool_calls + digest.shape = 5 structural blobs.
        let n = NativeBackend::in_memory();
        let mut a = with_facts.clone();
        a.checkpoint_id = a.derived_checkpoint_id().unwrap();
        n.archive_checkpoint(&a).unwrap();
        assert_eq!(n.writer().structural_count(), 5);
    }

    /// **The compat twin, stated just as plainly:** a shard
    /// carrying no digest artifact derives, leaf for leaf, the sequence it
    /// derived before digests existed — and a digest-less archive is exactly
    /// one with neither transcript nor read facts, which is every
    /// pre-digest-era test fixture in this file (the anchors' claim).
    #[test]
    fn a_digestless_shard_derives_the_same_root_it_did_before_digests_existed() {
        use rift_hashchain::leaf_hash;

        let bare = archive();
        assert_eq!(bare.digest_failures_json(), None);
        assert_eq!(bare.digest_intent_json(), None);
        assert_eq!(bare.digest_log_json(), None);
        assert_eq!(bare.digest_shape_json(), None);
        let report = bare.redaction_report_json();
        assert_eq!(
            shard_merkle_leaves(&ShardLeafBytes {
                boundary_gaps: None,
                content_redacted: bare.content.as_slice(),
                redaction_report: &report,
                ..ShardLeafBytes::default()
            }),
            vec![leaf_hash(bare.content.as_slice()), leaf_hash(&report)],
            "no digest, no leaf — the pre-digest sequence verbatim"
        );
    }

    /// A transcript-carrying archive derives all three transcript-side
    /// digests, and they are functions of the archive's own fields — the
    /// drift-proofing that lets condense, the trailer prediction and the
    /// shard writer share one derivation.
    #[test]
    fn a_transcript_carrying_archive_derives_its_digest_documents() {
        let mut a = archive();
        a.transcript = Some(transcript_of(
            br#"{"type":"user","message":"do the thing"}
{"type":"user","message":[{"type":"tool_result","tool_use_id":"t","is_error":true}]}"#,
        ));
        let failures = a.digest_failures_json().expect("one failure");
        let intent = a.digest_intent_json().expect("transcript => intent");
        let log = a.digest_log_json().expect("transcript => log");
        let v: serde_json::Value = serde_json::from_slice(&failures).unwrap();
        assert_eq!(v.get("count").and_then(serde_json::Value::as_u64), Some(1));
        let v: serde_json::Value = serde_json::from_slice(&intent).unwrap();
        assert_eq!(
            v.pointer("/intent/prompt_line")
                .and_then(serde_json::Value::as_u64),
            Some(0)
        );
        let v: serde_json::Value = serde_json::from_slice(&log).unwrap();
        assert_eq!(
            v.get("transcript_lines")
                .and_then(serde_json::Value::as_u64),
            Some(2)
        );
        // All durable + structural writes still land (wall intact): content +
        // 2 transcript blobs durable; content_hash/metadata/report + 3 digest
        // docs structural.
        let n = NativeBackend::in_memory();
        a.checkpoint_id = a.derived_checkpoint_id().unwrap();
        n.archive_checkpoint(&a).unwrap();
        assert_eq!(n.writer().durable_count(), 3);
        assert_eq!(n.writer().structural_count(), 6);
    }

    /// The anchor is the document's one optional member a live seal may lack:
    /// a checkpoint minted on an unborn HEAD names no commit, and the member
    /// is then absent — never `""`, which a reader would have to screen as a
    /// malformed oid and name as an issue. A present anchor is written as it
    /// always was, so every committed shard's bytes stand.
    #[test]
    fn an_absent_anchor_is_omitted_from_metadata_and_a_present_one_is_written() {
        let mut anchorless = archive();
        anchorless.commit = None;
        let meta: serde_json::Value = serde_json::from_slice(&anchorless.metadata_json()).unwrap();
        assert!(
            meta.get("commit").is_none(),
            "an absent anchor writes no member: {meta}"
        );
        let anchored: serde_json::Value =
            serde_json::from_slice(&archive().metadata_json()).unwrap();
        assert_eq!(anchored["commit"], "abc123");
    }

    #[test]
    fn metadata_and_report_are_canonical_and_valid() {
        use rift_recall_types::checkpoint::{CheckpointMetadata, RedactionReportFacts};
        let a = archive();
        let meta: CheckpointMetadata = serde_json::from_slice(&a.metadata_json()).unwrap();
        assert_eq!(
            meta.merkle_root.as_deref(),
            Some(a.merkle_root().to_hex()).as_deref()
        );
        assert_eq!(meta.sessions, a.sessions);
        assert_eq!(meta.commit, a.commit);
        let report: RedactionReportFacts =
            serde_json::from_slice(&a.redaction_report_json()).unwrap();
        assert!(
            report.is_valid(),
            "the archived report passes the strict bridge gate"
        );
        // Canonical (byte-stable) serialization: identical archive → identical bytes.
        assert_eq!(archive().metadata_json(), a.metadata_json());
    }

    /// The defect this construction exists to kill: every native shard recorded
    /// `merkle_root(&[])`, so 100+ archived checkpoints published the identical
    /// constant and the Merkle half of tamper-evidence committed to nothing.
    ///
    /// The root must (a) not be the empty-tree root and (b) move when ANY of the
    /// leaf artifacts moves. Each arm below is the mutation that would have to
    /// go undetected for the defect to be back.
    #[test]
    fn the_shard_root_is_not_vacuous_and_moves_with_every_leaf_artifact() {
        let base = archive();
        let root = base.merkle_root();
        assert_ne!(
            root,
            rift_hashchain::empty_root(),
            "a shard root equal to the empty-tree root is the vacuous era returning"
        );

        // (1) content.redacted
        let mut other_content = archive();
        other_content.content = rift_shadow_redact::redact(&SecretBytes::new(b"other".to_vec()))
            .unwrap()
            .redacted;
        assert_ne!(other_content.merkle_root(), root, "content.redacted");

        // (2) redaction_report.json
        let mut other_report = archive();
        other_report.report.total_spans += 1;
        assert_ne!(other_report.merkle_root(), root, "redaction_report.json");

        // (3) full.jsonl (an event stream appears)
        let mut with_events = archive();
        with_events.events = vec![ArchivedEvent {
            session: "native-1".into(),
            turn_ordinal: 0,
            event_ordinal: 0,
            kind: "session_start".into(),
            verb: "SessionStart".into(),
            agent: "claude-code".into(),
            occurred_at_ms: 1,
            payload_sha256: "a".repeat(64),
            payload: serde_json::json!({}),
        }];
        assert_ne!(with_events.merkle_root(), root, "full.jsonl");

        // (4) tool_calls.jsonl
        let mut with_facts = archive();
        with_facts.read_facts = vec![ArchivedReadFact {
            tool_use_id: "t1".into(),
            tool_name: "Read".into(),
            path: "a.rs".into(),
            content_sha256: "aa".into(),
            input_tokens: None,
            at: None,
            turn: None,
            blob_oid: None,
            tokenizer: None,
        }];
        assert_ne!(with_facts.merkle_root(), root, "tool_calls.jsonl");

        // …and the anchoring commit — which is NOT a leaf artifact — must not
        // move it, or a second commit in one session would mint a new root for
        // unchanged capture and the shard's idempotence would break.
        let mut later = archive();
        later.commit = Some("def456".to_owned());
        later.created_at_ms = later.created_at_ms.map(|ms| ms + 60_000);
        assert_eq!(
            later.merkle_root(),
            root,
            "anchor drift is not capture drift"
        );
    }

    /// One event line per leaf: two archived events produce two more leaves than
    /// none, in file order, and reordering them changes the root.
    #[test]
    fn full_jsonl_contributes_one_leaf_per_event_line() {
        let ev = |ordinal: u32, payload: u8| ArchivedEvent {
            session: "native-1".into(),
            turn_ordinal: 0,
            event_ordinal: ordinal,
            kind: "session_start".into(),
            verb: "SessionStart".into(),
            agent: "claude-code".into(),
            occurred_at_ms: 1,
            payload_sha256: format!("{payload:02x}").repeat(32),
            payload: serde_json::json!({}),
        };
        let mut a = archive();
        a.events = vec![ev(0, 0xaa), ev(1, 0xbb)];
        let full = a.full_jsonl().unwrap();
        let report = a.redaction_report_json();
        let leaves = shard_merkle_leaves(&ShardLeafBytes {
            boundary_gaps: None,
            content_redacted: a.content.as_slice(),
            full_jsonl: Some(&full),
            redaction_report: &report,
            serve_audit: None,
            session_worktrees: None,
            tool_results: None,
            sessions: None,
            usage: None,
            window_journal: None,
            tool_calls: None,
            transcript_compact: None,
            transcript_full: None,
            tasks: &[],
            spills: &[],
            originals: &[],
            session_metas: &[],
            digests: DigestLeafBytes::default(),
        });
        assert_eq!(
            leaves.len(),
            4,
            "content.redacted + two event lines + redaction_report.json"
        );

        let mut swapped = archive();
        swapped.events = vec![ev(1, 0xbb), ev(0, 0xaa)];
        assert_ne!(
            swapped.merkle_root(),
            a.merkle_root(),
            "order is part of the commitment"
        );

        // A trailing newline must not mint a phantom leaf: the same stream with
        // and without one derives the same root.
        let mut terminated = full.clone();
        terminated.push(b'\n');
        assert_eq!(
            shard_merkle_root(&ShardLeafBytes {
                boundary_gaps: None,
                content_redacted: a.content.as_slice(),
                full_jsonl: Some(&terminated),
                redaction_report: &report,
                serve_audit: None,
                session_worktrees: None,
                tool_results: None,
                sessions: None,
                usage: None,
                window_journal: None,
                tool_calls: None,
                transcript_compact: None,
                transcript_full: None,
                tasks: &[],
                spills: &[],
                originals: &[],
                session_metas: &[],
                digests: DigestLeafBytes::default(),
            }),
            a.merkle_root(),
            "trailing newline is not a record"
        );
    }

    /// The metadata half of the requirement: the compact path and its
    /// boundary are recorded when — and only when — the shard carries the
    /// artifacts, so a transcript-less shard's metadata is byte-identical to
    /// what it was before the pair existed.
    #[test]
    fn metadata_records_the_compact_name_and_start_only_when_present() {
        let bare = archive();
        let meta: serde_json::Value = serde_json::from_slice(&bare.metadata_json()).unwrap();
        assert!(meta.get("compact_transcript").is_none());
        assert!(meta.get("compact_transcript_start").is_none());

        let mut with = archive();
        let mut t = transcript_of(br#"{"type":"user","message":{"role":"user","content":"hi"}}"#);
        t.compact_start = 7;
        with.transcript = Some(t);
        let meta: serde_json::Value = serde_json::from_slice(&with.metadata_json()).unwrap();
        assert_eq!(
            meta.get("compact_transcript")
                .and_then(serde_json::Value::as_str),
            Some(TRANSCRIPT_COMPACT_NAME),
            "'compact path recorded in metadata' means the artifact NAME"
        );
        assert_eq!(
            meta.get("compact_transcript_start")
                .and_then(serde_json::Value::as_u64),
            Some(7),
        );
    }

    /// The era is stamped, and it is the field `rift verify` reads to decide
    /// which claims a shard is making about itself.
    ///
    /// Both thresholds are asserted against what a shard written NOW carries,
    /// because the two are separate claims: the recorded root must be strictly
    /// checkable (epoch ≥ 2) and the id must be root-bound (epoch ≥ 3). A stamp
    /// that satisfied one and not the other would put shards into an era whose
    /// rules no writer implements.
    #[test]
    fn the_archive_stamps_an_era_that_is_both_strict_and_root_bound() {
        let a = archive();
        let meta: serde_json::Value = serde_json::from_slice(&a.metadata_json()).unwrap();
        let stamped = meta
            .get("canon_version")
            .and_then(serde_json::Value::as_u64)
            .unwrap();
        assert_eq!(stamped, u64::from(NATIVE_CANON_VERSION));
        assert!(
            stamped >= u64::from(STRICT_MERKLE_MIN_CANON_VERSION),
            "a shard written now must be strictly verifiable"
        );
        assert!(
            stamped >= u64::from(ROOT_BOUND_ID_MIN_CANON_VERSION),
            "…and its id must address its own root"
        );
        // The stamp is a claim about this shard, so the shard must satisfy it:
        // the id the archive derives is the one that era's rule accepts.
        let ids = crate::condense::checkpoint_ids_for_shard(
            &a.sessions,
            NATIVE_CANON_VERSION,
            &ShardLeafBytes {
                boundary_gaps: None,
                content_redacted: a.content.as_slice(),
                full_jsonl: None,
                redaction_report: &a.redaction_report_json(),
                serve_audit: None,
                session_worktrees: None,
                tool_results: None,
                sessions: None,
                usage: None,
                window_journal: None,
                tool_calls: None,
                transcript_compact: None,
                transcript_full: None,
                tasks: &[],
                spills: &[],
                originals: &[],
                session_metas: &[],
                digests: DigestLeafBytes::default(),
            },
        );
        assert_eq!(
            ids.first().map(String::as_str),
            a.derived_checkpoint_id().as_deref(),
            "the writer's id must be what this era's checker accepts"
        );
    }

    /// The central byte claim: the label rides in `metadata.json`,
    /// which is NOT a Merkle leaf — so an archive written from a linked
    /// worktree names it, and addresses **the same checkpoint id** as the
    /// byte-identical capture archived from nowhere in particular.
    #[test]
    fn an_archive_from_a_linked_worktree_names_it_and_keeps_the_checkpoint_id() {
        let unlabelled = archive();
        let mut labelled = archive();
        labelled.worktree = Some(WorktreeLabel {
            name: "swt-a".to_owned(),
            token: "0123456789ab".to_owned(),
            linked: true,
            branch: Some("swt/a-foundations".to_owned()),
        });

        assert_eq!(
            labelled.merkle_root(),
            unlabelled.merkle_root(),
            "metadata.json is not a leaf: the root cannot see the member"
        );
        assert_eq!(
            labelled.derived_checkpoint_id(),
            unlabelled.derived_checkpoint_id(),
            "the id is root-bound, so the label cannot move it"
        );

        let text = String::from_utf8(labelled.metadata_json()).unwrap();
        assert!(
            text.contains(
                r#""worktree":{"branch":"swt/a-foundations","linked":true,"name":"swt-a","token":"0123456789ab"}"#
            ),
            "{text}"
        );
        // No absolute path and no username: this document crosses machines in
        // a bundle, and a checkout's path is a local address.
        assert!(!text.contains('/') || !text.contains("Users"), "{text}");

        // …and the absence is exactly the bytes that were there before, which
        // is the whole compatibility claim for the committed anchors.
        let before = String::from_utf8(unlabelled.metadata_json()).unwrap();
        assert!(!before.contains("worktree"), "{before}");
        assert_eq!(
            before.len() + r#","worktree":{"branch":"swt/a-foundations","linked":true,"name":"swt-a","token":"0123456789ab"}"#.len(),
            text.len(),
            "the member is the ONLY difference"
        );

        // A detached archive omits `branch` rather than writing null: absent is
        // this document's spelling of unknown, everywhere.
        let mut detached = archive();
        detached.worktree = Some(WorktreeLabel {
            name: "main".to_owned(),
            token: "0123456789ab".to_owned(),
            linked: false,
            branch: None,
        });
        let detached_text = String::from_utf8(detached.metadata_json()).unwrap();
        assert!(
            detached_text.contains(r#""worktree":{"linked":false,"#),
            "{detached_text}"
        );
        assert!(!detached_text.contains("branch"), "{detached_text}");
    }

    /// `session_worktrees` is for the sessions of a MULTI-session
    /// shard whose last observation differs from the archiving worktree — and
    /// for nothing else. A single-session shard, and a multi-session shard
    /// whose sessions all sat where the archive ran, write no key at all, so
    /// their bytes are what they always were.
    #[test]
    fn a_multi_session_shard_records_session_worktrees_only_where_they_differ() {
        let here = WorktreeLabel {
            name: "main".to_owned(),
            token: "aaaaaaaaaaaa".to_owned(),
            linked: false,
            branch: Some("main".to_owned()),
        };
        let elsewhere = WorktreeLabel {
            name: "swt-d".to_owned(),
            token: "bbbbbbbbbbbb".to_owned(),
            linked: true,
            branch: Some("swt/d-surfaces".to_owned()),
        };

        // Every session sat where the archive ran → nothing to record.
        let mut agreed = archive();
        agreed.sessions = vec!["s-a".to_owned(), "s-b".to_owned()];
        agreed.worktree = Some(here.clone());
        let agreed_text = String::from_utf8(agreed.metadata_json()).unwrap();
        assert!(!agreed_text.contains("session_worktrees"), "{agreed_text}");

        // One session moved → exactly that session is named, and the archiving
        // label stays the shard's own.
        let mut moved = archive();
        moved.sessions = vec!["s-a".to_owned(), "s-b".to_owned()];
        moved.worktree = Some(here);
        moved
            .session_worktrees
            .insert("s-b".to_owned(), elsewhere.clone());
        let moved_text = String::from_utf8(moved.metadata_json()).unwrap();
        assert!(
            moved_text.contains(
                r#""session_worktrees":{"s-b":{"branch":"swt/d-surfaces","linked":true,"name":"swt-d","token":"bbbbbbbbbbbb"}}"#
            ),
            "{moved_text}"
        );
        assert!(
            !moved_text.contains(r#""s-a":{"#),
            "the agreeing session is not restated"
        );

        // Neither member touches the id or the root: both are metadata.
        assert_eq!(
            moved.derived_checkpoint_id(),
            agreed.derived_checkpoint_id()
        );
        assert_eq!(moved.merkle_root(), agreed.merkle_root());
        // …nor the per-session LEAF, which is a Merkle leaf and must not move.
        assert_eq!(moved.session_meta_docs(), agreed.session_meta_docs());
    }
    #[test]
    fn parent_transcript_fallback_counts_are_bound_by_the_report() {
        let mut a = archive();
        let normal_report = a.redaction_report_json();
        let packs = rift_shadow_redact::RulePackSet::builtin()
            .with_vendor_pattern("osc", "WEDGE|REDACTED:vendor", "acme@1")
            .unwrap();
        let (pair, _) =
            crate::archived_transcript(&SecretBytes::from("WEDGE ssn=\n123456789"), 0, &packs)
                .unwrap();
        let counts = pair.redaction_fallback;
        assert!(counts.lines > 0);
        a.transcript = Some(pair);
        let report: serde_json::Value = serde_json::from_slice(&a.redaction_report_json()).unwrap();
        assert_eq!(report["redaction_flat_fallback"], counts.lines);
        assert_eq!(
            report["redaction_fallback_placeholders"],
            counts.placeholders
        );
        let meta: serde_json::Value = serde_json::from_slice(&a.metadata_json()).unwrap();
        assert_eq!(
            meta["redaction_flat_fallback"],
            report["redaction_flat_fallback"]
        );
        let mut task = super::super::fixtures::task("fallback_task");
        task.transcript = a.transcript.clone();
        a.tasks.push(task);
        a.transcript.as_mut().unwrap().redaction_fallback =
            super::super::RedactionFallback::default();
        assert_eq!(
            a.redaction_report_json(),
            normal_report,
            "zero-fallback bytes stay unchanged"
        );
        let task: serde_json::Value =
            serde_json::from_slice(&a.tasks.last().unwrap().metadata_json()).unwrap();
        assert_eq!(
            task["redaction_flat_fallback"], counts.lines,
            "tasks are not added to parent report"
        );
        let mut fields = serde_json::Map::new();
        insert_fallback_counts(
            &mut fields,
            super::super::RedactionFallback {
                lines: 0,
                placeholders: 1,
            },
        );
        assert_eq!(fields["redaction_fallback_placeholders"], 1);
        assert_eq!(fields["redaction_flat_fallback"], 0);
    }
}
