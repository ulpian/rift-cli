//! The condense operation (shadow → permanent, redacted checkpoint).
//!
//! Condense is the single point where **raw local** capture becomes **durable**
//! data, so it is the one-way redaction gate in action: the
//! raw bytes pass through [`redact`](rift_shadow_redact) and only the resulting
//! `RedactedBytes` flow onward — a path that skips redaction does not compile.
//! The git write of the checkpoint shard happens behind the
//! [`ShadowBackend`] seam; Rift orchestrates *when*. It is idempotent:
//! identical content → identical `checkpoint_id`.

use std::path::Path;

use rift_core::{Result, SecretBytes};
use rift_shadow_hooks::strategy::{ArchiveReason, StrategyKind};
use rift_shadow_redact::RulePackSet;
use rift_store::{
    record_archive_decision, record_substrate_block_event, ArchiveDecisionRow, CanonicalStore,
    SubstrateBlockEventRow,
};

use crate::backend::{CheckpointArchive, RedactionReportView, ShadowBackend};
use crate::sha256_hex;

/// A content-addressed checkpoint id (lowercase hex).
pub type CheckpointId = String;

/// What a session captured beside its capture document, handed to the archive
/// as one value: the read facts that become `tool_calls.jsonl`, the frozen
/// serve envelopes that become `serve_audit.jsonl`, the settled usage that
/// becomes `usage.jsonl`, the session's agent that becomes `sessions.jsonl`,
/// its context windows' journal rows that become `window_journal.jsonl`, its
/// worktree observations that become `session_worktrees.jsonl` and its
/// measured tool results that become `tool_results.jsonl`.
///
/// One struct rather than one parameter per artifact, because the prediction
/// and the archive must fold exactly the same facts, and every new fact
/// threaded as its own parameter was another signature on every path between
/// the hook and the shard. A new fact is a field here and nowhere else.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionFacts {
    /// Checked repository recovery audit carried by this genuine session.
    pub boundary_gaps: Option<crate::backend::FrozenBoundaryGaps>,
    /// The session's captured read facts (`tool_calls.jsonl`).
    pub read_facts: Vec<crate::backend::ArchivedReadFact>,
    /// The session's frozen serve envelopes, already canonical, validated,
    /// de-duplicated and sorted (`serve_audit.jsonl`); `None` when it froze
    /// none.
    pub serve_audit: Option<Vec<u8>>,
    /// The session's settled model calls with their usage (`usage.jsonl`).
    pub usage: Vec<crate::backend::ArchivedUsageFact>,
    /// The agent token the session was captured from (`sessions.jsonl`);
    /// `None` writes no artifact.
    pub agent: Option<String>,
    /// Every row the session's context windows' journals held
    /// (`window_journal.jsonl`), in any order: the writer sorts them.
    pub window_journal: Vec<crate::backend::ArchivedWindowFact>,
    /// Every observation of the member the session was seen in
    /// (`session_worktrees.jsonl`), in any order: the writer sorts them.
    pub worktree_observations: Vec<crate::backend::ArchivedWorktreeObservation>,
    /// Every measured tool result the session recorded
    /// (`tool_results.jsonl`), in any order: the writer sorts them.
    pub tool_results: Vec<crate::backend::ArchivedToolResult>,
}

/// **The one derivation of a new checkpoint id**: `sha256(session ‖ \0 ‖
/// sha256_hex(shard Merkle root))`, where the root is
/// [`shard_merkle_root`](crate::backend::shard_merkle_root) over the shard's
/// content artifacts in canonical name order.
///
/// The RULE — the byte formula — lives in [`rift_core::checkpoint_id`], which
/// is its single writer; this function is the leaf-spec side's call-through,
/// kept so every caller keeps naming the derivation through the crate that
/// owns *what a shard root is*. What this crate decides is the LEAF SPEC (what
/// bytes feed the root); what `rift-core` decides is the arithmetic.
///
/// **Why the root and not a list of digests.** The id has to bind every artifact
/// the shard stores, or an unbound one makes the shard tree differ at an
/// unchanged path and the second archive of a live session takes a hard
/// `Integrity` refusal (a defect once planted in
/// `an_id_unbound_artifact_hard_fails_the_second_archive_of_a_session` before
/// this closed it). The alternative was to fold one more digest per artifact into
/// the formula — which works, and needs a NEW formula, a new accepted-set entry
/// and a new sync-format paragraph every single time an artifact is added. The
/// root already commits to all of them, in order, and is already DERIVED rather
/// than supplied. Binding it means a future artifact extends the LEAF SPEC and
/// the id rule never moves again.
///
/// The consequences are the ones the archive already relies on:
///
/// * A byte change in any content artifact moves the root, so it moves the id,
///   so it mints a NEW checkpoint — cleanly appended, exactly as a grown
///   read-fact set already did.
/// * `metadata.json` and `content_hash.txt` are NOT leaves (the root lives in the
///   first and the second is a digest over it), so anchor drift — a second commit
///   in one session — leaves the id alone and the re-archive stays an idempotent
///   no-op.
/// * A tamperer who edits an artifact and recomputes the recorded root passes the
///   strict Merkle pass and still fails here, because the id is the shard's PATH
///   and re-deriving it is not something writing a file can fix.
#[must_use]
pub fn derive_checkpoint_id(session: &str, shard_root: rift_core::Sha256Id) -> CheckpointId {
    rift_core::checkpoint_id::from_root(session, shard_root)
}

/// Every checkpoint id a shard of this **declared era** could legitimately
/// address, newest derivation first.
///
/// A checker asks "is the shard's own path in this set?"; the writer never asks
/// at all — it derives from its own artifacts (`CheckpointArchive::
/// derived_checkpoint_id`), so there is exactly one site that decides what a new
/// id is and this function cannot drift into being a second one.
///
/// **The era gate is load-bearing, and it is deliberately not symmetric.**
/// Epochs ≤ 2 accept both pre-root derivations, because being permissive about
/// the past is what stops a checker calling an honest shard tampered — the
/// failure this program has already had once, on the owner's own history. Epoch
/// 3+ accepts ONLY the root binding, because accepting a pre-root derivation
/// there would re-open the hole the era exists to close: a tamperer could edit
/// `full.jsonl`, recompute the recorded root, and keep the path.
///
/// **Relabelling DOWN does not buy the looser rule**, and that is structural
/// rather than lucky: the two eras' rules produce different ids for the same
/// bytes, so a shard's PATH is evidence of the era that minted it and the label
/// cannot be moved out from under it. Measured in `bins/rift`'s
/// `relabelling_a_root_bound_shard_into_the_capture_era_does_not_hide_it`.
///
/// Accepting a superseded derivation costs about one bit and no security
/// property: every entry is SHA-256 over the same content, so forging one from
/// chosen bytes is a preimage attack.
/// An empty set means **no era rule grants this shard an id at all** — a
/// session list the root-bound rule refuses (empty, or NUL-carrying), or a
/// multi-session shard declaring a pre-root era whose rules were
/// single-session by construction (no writer ever minted one; the label is
/// evidence of an edit). A checker maps the empty set to tamper.
#[must_use]
pub fn checkpoint_ids_for_shard(
    sessions: &[String],
    canon_version: u32,
    leaves: &crate::backend::ShardLeafBytes<'_>,
) -> Vec<CheckpointId> {
    if canon_version >= crate::backend::ROOT_BOUND_ID_MIN_CANON_VERSION {
        let refs: Vec<&str> = sessions.iter().map(String::as_str).collect();
        return rift_core::checkpoint_id::from_root_sessions(
            &refs,
            crate::backend::shard_merkle_root(leaves),
        )
        .into_iter()
        .collect();
    }
    let [session] = sessions else {
        return Vec::new();
    };
    vec![
        rift_core::checkpoint_id::from_capture(session, leaves.content_redacted, leaves.tool_calls),
        rift_core::checkpoint_id::from_capture_only(session, leaves.content_redacted),
    ]
}

/// The checkpoint id `condense` will derive for `(session, raw_capture,
/// read_facts)` — computable **before** the archive runs (it depends on the
/// session, the redacted capture, and the already-captured read facts — never
/// the commit oid), which is what lets `prepare-commit-msg` inject a
/// `Rift-Checkpoint` trailer that the post-commit archive then makes true.
///
/// The prediction misses only if one of its inputs changes in the instants
/// between the two hooks: a read fact landing, or — since the inclusion
/// controls put content in the capture — an opted-in working-tree file being
/// rewritten (a build touching
/// an included `.env`, say). Either way the archive still lands under its true
/// id and the trailer is one change stale: a recorded limitation, never a lost
/// checkpoint. The two hooks can only ever disagree about *when* they looked,
/// never about *what* they measured, because both read the caller's single
/// capture definition.
///
/// **Ruled 2026-08-29: the prediction does not see the transcript,
/// permanently.** The archive binds
/// the session transcript into the id, so a transcript-carrying session's
/// trailer does not match its checkpoint — pinned as a fact in
/// `tests::a_condense_with_a_transcript_diverges_from_the_trailer_prediction`
/// (load-bearing). Including it here would mean reading, compacting and
/// redacting the agent's transcript file inside `prepare-commit-msg`, which
/// cannot detach and owns the 100 ms HARD git-latency ceiling — a cost that
/// grows with session length. The decision: the
/// trailer is a CLAIM. It stays exact for transcript-less sessions; for the
/// rest the read side resolves through the trailer/anchor union
/// (`bins/rift/src/provenance/resolve.rs`, the `checkpoint_commit` anchor the
/// shard itself recorded), which is the permanent answer rather than a stopgap.
/// "Predicted id == landed id" is retired as a property.
///
/// # Errors
/// [`rift_core::RiftError`] if redaction fails.
pub fn predicted_checkpoint_id(
    session: &str,
    raw_capture: &SecretBytes,
    facts: &SessionFacts,
    packs: &RulePackSet,
) -> Result<CheckpointId> {
    predicted_checkpoint_id_from(session, predict_redaction(raw_capture, packs)?, facts)
}

/// The REDACTION half of [`predicted_checkpoint_id`], separable so the
/// `prepare-commit-msg` hook can time it on its own.
///
/// **Why this is a function and not an inlined line.** When this seam was cut,
/// the hook's 100 ms budget had never been measured at any opted-in
/// `.worktreeinclude` size and the 1 MiB ceiling was only *predicted* to breach
/// it by 40-220%. It has been measured since, through this very phase: `redact`
/// is **74% of hook-body time** and doubles to **138.50 ms** at the old ceiling,
/// which is what set the new one at 8 KiB
/// ([`crate::inclusion::MAX_TOTAL_BYTES`]). "It is in `predict_id`" was not an
/// actionable finding; "it is in the redaction inside `predict_id`" was, and
/// that is the finding this split produced. The alternatives all corrupt the thing they measure:
/// redacting a second time to time it doubles the dominant cost inside the one
/// hook that cannot detach, and re-deriving the id beside the redaction is the
/// second-implementation defect this module's own comment says has already been
/// paid for three times. So the split is here, where the single implementation
/// lives, and the caller composes.
///
/// # Errors
/// [`rift_core::RiftError`] if redaction fails.
pub fn predict_redaction(
    raw_capture: &SecretBytes,
    packs: &RulePackSet,
) -> Result<rift_shadow_redact::RedactionOutput> {
    rift_shadow_redact::redact_with(raw_capture, packs)
}

/// The ID half of [`predicted_checkpoint_id`], over an already-redacted capture.
///
/// The prediction assembles the ARCHIVE the post-commit hook will write and asks
/// it for its own id, rather than re-deriving one beside it. Two implementations
/// of an id rule is the defect this program has now paid for three times; the
/// anchoring fields inside are placeholders because they are provably not inputs
/// (`anchor drift is not capture drift`, backend tests), and the two hooks would
/// disagree about them anyway — that is the whole reason the id excludes them.
///
/// **This is that single implementation.** [`predicted_checkpoint_id`] is now
/// its composition with [`predict_redaction`] and holds no rule of its own, so
/// splitting the two for timing added a seam and not a second answer.
///
/// # Errors
/// [`rift_core::RiftError::Validity`] if the capture names no NUL-free session.
pub fn predicted_checkpoint_id_from(
    session: &str,
    out: rift_shadow_redact::RedactionOutput,
    facts: &SessionFacts,
) -> Result<CheckpointId> {
    let archive = archive_for_id(
        session,
        out.redacted,
        RedactionReportView::from_redact(&out.report),
        facts.clone(),
    );
    derived_id(&archive)
}

/// A [`CheckpointArchive`] carrying exactly the artifacts that decide an id, with
/// placeholders for the anchoring fields the derivation excludes. [`condense`]
/// writes its anchoring fields over this same assembly, so the prediction and
/// the archive cannot fold two different sets of facts.
fn archive_for_id(
    session: &str,
    content: rift_core::RedactedBytes,
    report: RedactionReportView,
    facts: SessionFacts,
) -> CheckpointArchive {
    CheckpointArchive {
        boundary_gaps: facts.boundary_gaps,
        repo: std::path::PathBuf::new(),
        commit: String::new(),
        checkpoint_id: String::new(),
        sessions: vec![session.to_owned()],
        strategy: String::new(),
        prev_hash: None,
        events: Vec::new(),
        read_facts: facts.read_facts,
        serve_audit: facts.serve_audit,
        usage: facts.usage,
        session_agents: facts
            .agent
            .map(|agent| std::collections::BTreeMap::from([(session.to_owned(), agent)]))
            .unwrap_or_default(),
        window_journal: facts.window_journal,
        worktree_observations: facts.worktree_observations,
        tool_results: facts.tool_results,
        content,
        report,
        transcript: None,
        tasks: Vec::new(),
        // The id derivation never reads the time (metadata is not a Merkle
        // leaf), so the prediction carries none rather than inventing one —
        // and, for the same reason, no worktree label either: the trailer's
        // prediction is about the CAPTURE, and a label would make the
        // prediction depend on where it was computed.
        created_at_ms: None,
        worktree: None,
        session_worktrees: std::collections::BTreeMap::new(),
    }
}

/// Condense raw shadow capture into a durable, redacted checkpoint and record the
/// `archive_decision`. `commit_oid` is the anchoring commit (HEAD) the substrate
/// archives against.
///
/// # Errors
/// [`rift_core::RiftError`] if redaction, the substrate archive, or the audit
/// insert fails.
#[allow(clippy::too_many_arguments)] // one gate, one call shape; a params struct would hide which caller passes what
pub async fn condense(
    backend: &impl ShadowBackend,
    store: &impl CanonicalStore,
    repo: &Path,
    session: &str,
    raw_capture: &SecretBytes,
    strategy: StrategyKind,
    reason: ArchiveReason,
    commit_oid: &str,
    now_ms: i64,
    facts: SessionFacts,
    packs: &RulePackSet,
    transcript: Option<crate::backend::ArchivedTranscript>,
    tasks: Vec<crate::backend::ArchivedTask>,
) -> Result<CheckpointId> {
    // ONE-WAY GATE: redact first. Nothing past this line sees the raw bytes; the
    // checkpoint id is content-addressed over the *redacted* form.
    let out = rift_shadow_redact::redact_with(raw_capture, packs)?;
    let report = RedactionReportView::from_redact(&out.report);

    // The backend writes the shard behind the `ShadowBackend` seam. The archive
    // carries the anchoring `repo`/`commit` plus the full structural content
    // (id, sessions, strategy, redacted capture, redaction report).
    //
    // The Merkle root is NOT passed here. It used to be, and every caller passed
    // `checkpoint_merkle_root(&[])` because no per-turn event stream is threaded
    // yet — one constant on every shard this repository ever archived, so the
    // recorded root committed to nothing. It is now derived from the shard's own
    // artifacts inside `CheckpointArchive::merkle_root`, which is why condense
    // cannot get it wrong: there is nowhere left to supply it.
    let mut archive = CheckpointArchive {
        repo: repo.to_path_buf(),
        commit: commit_oid.to_owned(),
        // Filled from the archive itself on the next line: the id is a function
        // of the artifacts this archive will write, and the only way to be sure
        // it addresses them is to ask the assembled archive rather than to
        // compute an id beside it and hope the two agree.
        checkpoint_id: String::new(),
        strategy: strategy.as_str().to_owned(),
        // Already through the gate: `ArchivedTranscript` is constructible only
        // from `RedactedBytes` (`archived_transcript` is the one writer of the
        // raw→artifact flow, and it redacts each artifact before this sees it).
        transcript,
        // The subagent tasks, HANDED IN — never filtered here. A task that
        // started before this checkpoint's boundary still belongs to the
        // checkpoint that saw it finish (the rollup rule),
        // and the pin for that is the test right below the divergence pin.
        tasks,
        // The capture event's own recorded time — the hook stamped it and
        // passed it down; nothing on this path reads a clock of its own. It
        // lands in `metadata.json` as `created_at`, which is what the bridge
        // reads back into `checkpoint.created_at`: native checkpoints stop
        // carrying the unknown-time sentinel.
        created_at_ms: Some(now_ms),
        // WHICH WORKTREE THIS ARCHIVE RAN IN, derived HERE
        // rather than handed in. `repo` is the checkout the caller is
        // archiving from — the session-end path passes its work tree, the
        // post-commit worker passes `repo.work_tree` — so this is the one
        // place the archiving worktree is known, and deriving it here is what
        // keeps the two archive entry points from disagreeing about it.
        // `None` when `repo` is not in a readable repository, which is what
        // every hand-built fixture is: the member stays absent and their
        // frozen bytes stay frozen.
        worktree: crate::backend::WorktreeLabel::of(repo),
        // Single-session by construction on this path; see the field's doc.
        session_worktrees: std::collections::BTreeMap::new(),
        // The session, the redacted capture, its report and the session's facts
        // come from the one assembly the trailer's prediction folds, so a fact
        // added to `SessionFacts` reaches both archives through one function.
        ..archive_for_id(session, out.redacted, report, facts)
    };
    archive.checkpoint_id = derived_id(&archive)?;
    let checkpoint_id = archive.checkpoint_id.clone();
    archive_or_audit_refusal(backend, store, &archive, now_ms).await?;
    record_archived_decision(store, session, strategy, reason, now_ms, &checkpoint_id).await?;
    Ok(checkpoint_id)
}

/// Hand the archive to the backend. A default-deny refusal is an audited fact,
/// not just an error: the archive root verb is the refused token. Audit
/// best-effort — the refusal itself must still surface even if the audit insert
/// fails.
async fn archive_or_audit_refusal(
    backend: &impl ShadowBackend,
    store: &impl CanonicalStore,
    archive: &CheckpointArchive,
    now_ms: i64,
) -> Result<()> {
    let Err(e) = backend.archive_checkpoint(archive) else {
        return Ok(());
    };
    if matches!(e, rift_core::RiftError::PolicyDenied(_)) {
        let _ = record_block(store, "checkpoint", "screen-refused", now_ms).await;
    }
    Err(e)
}

/// The id an assembled archive derives from its own artifacts; a capture that
/// names no NUL-free session has none, and that is a validity error.
fn derived_id(archive: &CheckpointArchive) -> Result<CheckpointId> {
    archive
        .derived_checkpoint_id()
        .ok_or_else(|| rift_core::RiftError::validity("a checkpoint id needs 1+ NUL-free sessions"))
}

/// The `archived=1` decision row — written only after the substrate write
/// landed, so the audit never claims an archive that does not exist.
async fn record_archived_decision(
    store: &impl CanonicalStore,
    session: &str,
    strategy: StrategyKind,
    reason: ArchiveReason,
    now_ms: i64,
    checkpoint_id: &str,
) -> Result<()> {
    record_archive_decision(
        store,
        &ArchiveDecisionRow {
            decision_id: decision_id(session, strategy, reason, now_ms, Some(checkpoint_id)),
            session_id: Some(session.to_owned()),
            strategy: strategy.as_str().to_owned(),
            reason: reason.as_str().to_owned(),
            archived: true,
            checkpoint_id: Some(checkpoint_id.to_owned()),
            decided_at: now_ms,
            schema_version: 1,
        },
    )
    .await?;
    Ok(())
}

/// Record a default-deny refusal of an archive as an append-only
/// `substrate_block_event`. `verb` is the refused root token only —
/// never key/transcript bytes.
///
/// It lived in a `provision` module that existed to install and audit a foreign
/// capture binary; that module died with the binary on 2026-08-11. This did not,
/// because a backend that refuses is a fact about *this* archive, and the seam
/// still admits one.
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on insert failure.
async fn record_block(
    store: &impl CanonicalStore,
    verb: &str,
    reason: &str,
    now_ms: i64,
) -> Result<()> {
    let row = SubstrateBlockEventRow {
        block_id: sha256_hex(&[
            verb.as_bytes(),
            b"\0",
            reason.as_bytes(),
            b"\0",
            now_ms.to_string().as_bytes(),
        ]),
        verb: verb.to_owned(),
        reason: reason.to_owned(),
        occurred_at: now_ms,
        schema_version: 1,
    };
    record_substrate_block_event(store, &row).await?;
    Ok(())
}

/// Derive an `archive_decision` id. A retried hook at the same `decided_at` is
/// idempotent; a genuinely later decision differs by `decided_at`.
pub(crate) fn decision_id(
    session: &str,
    strategy: StrategyKind,
    reason: ArchiveReason,
    decided_at: i64,
    checkpoint_id: Option<&str>,
) -> String {
    sha256_hex(&[
        session.as_bytes(),
        b"\0",
        strategy.as_str().as_bytes(),
        b"\0",
        reason.as_str().as_bytes(),
        b"\0",
        decided_at.to_string().as_bytes(),
        b"\0",
        checkpoint_id.unwrap_or("").as_bytes(),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use rift_store::SqliteStore;

    fn store() -> SqliteStore {
        let s = SqliteStore::open_in_memory().unwrap();
        s.create_canonical_schema().unwrap();
        s
    }

    /// The session id the id-derivation tests archive under.
    const SESSION: &str = "01JSESSION";

    /// A backend whose write fails the way a plumbing failure does: an error out
    /// of `archive_checkpoint` and no shard on the substrate.
    struct FailingBackend;
    impl ShadowBackend for FailingBackend {
        fn archive_checkpoint(&self, _archive: &CheckpointArchive) -> Result<()> {
            Err(rift_core::RiftError::storage(
                "the shard could not be written",
            ))
        }
    }

    /// A backend that records the id of every archive it is handed. Counting the
    /// calls is the point: `condense` must not short-circuit a repeat.
    #[derive(Default)]
    struct CountingBackend {
        archived: std::cell::RefCell<Vec<String>>,
    }
    impl ShadowBackend for CountingBackend {
        fn archive_checkpoint(&self, archive: &CheckpointArchive) -> Result<()> {
            self.archived
                .borrow_mut()
                .push(archive.checkpoint_id.clone());
            Ok(())
        }
    }

    #[tokio::test]
    async fn failed_backend_archive_records_no_archived_row() {
        // Audit honesty: `archived=1` may exist only when the substrate
        // write landed. A backend that fails must error out of condense BEFORE
        // the decision row is written — no false facts.
        let s = store();
        let backend = FailingBackend;
        let raw = SecretBytes::new(Vec::new());
        let err = condense(
            &backend,
            &s,
            std::path::Path::new("/tmp/repo"),
            "01JSESSION",
            &raw,
            StrategyKind::ManualCommit,
            ArchiveReason::Commit,
            "abc123",
            1,
            SessionFacts::default(),
            &RulePackSet::builtin(),
            None,
            Vec::new(),
        )
        .await
        .expect_err("a failed backend archive fails condense");
        assert!(
            err.to_string().contains("could not be written"),
            "reason surfaced: {err}"
        );
        assert_eq!(
            s.count("archive_decision").await.unwrap(),
            0,
            "no archived=1 row for an archive that never landed"
        );
    }

    #[tokio::test]
    async fn condense_is_idempotent_and_redacts() {
        let s = store();
        let backend = CountingBackend::default();
        let raw = SecretBytes::new(b"secret token=AKIAIOSFODNN7EXAMPLE in capture".to_vec());
        let repo = std::path::Path::new("/tmp/repo");

        let id1 = condense(
            &backend,
            &s,
            repo,
            "01JSESSION",
            &raw,
            StrategyKind::ManualCommit,
            ArchiveReason::Commit,
            "abc123",
            1,
            SessionFacts::default(),
            &RulePackSet::builtin(),
            None,
            Vec::new(),
        )
        .await
        .unwrap();
        // identical content → identical checkpoint id (idempotent)
        let id2 = condense(
            &backend,
            &s,
            repo,
            "01JSESSION",
            &raw,
            StrategyKind::ManualCommit,
            ArchiveReason::Commit,
            "abc123",
            1,
            SessionFacts::default(),
            &RulePackSet::builtin(),
            None,
            Vec::new(),
        )
        .await
        .unwrap();
        assert_eq!(id1, id2);
        // same decided_at + content → one decision row
        assert_eq!(s.count("archive_decision").await.unwrap(), 1);
        // …and the backend really was asked twice for the same id, so the single
        // row is idempotence and not a skipped second archive.
        assert_eq!(backend.archived.borrow().as_slice(), &[id1.clone(), id2]);
    }

    #[tokio::test]
    async fn a_refused_archive_is_audited_as_a_block_event() {
        use crate::backend::ShadowBackend;

        /// A backend that refuses every archive as a policy block (screen refusal).
        struct RefusingBackend;
        impl ShadowBackend for RefusingBackend {
            fn archive_checkpoint(&self, _archive: &CheckpointArchive) -> rift_core::Result<()> {
                Err(rift_core::RiftError::PolicyDenied("refused".into()))
            }
        }

        let s = store();
        let raw = SecretBytes::new(b"capture".to_vec());
        let out = condense(
            &RefusingBackend,
            &s,
            std::path::Path::new("/r"),
            "01JSESSION",
            &raw,
            StrategyKind::ManualCommit,
            ArchiveReason::Commit,
            "abc123",
            1,
            SessionFacts::default(),
            &RulePackSet::builtin(),
            None,
            Vec::new(),
        )
        .await;
        assert!(matches!(out, Err(rift_core::RiftError::PolicyDenied(_))));
        assert_eq!(
            s.count("substrate_block_event").await.unwrap(),
            1,
            "the refusal left an append-only audit row"
        );
    }

    /// A hand-built archive with its id derived from its own artifacts, and
    /// that id. Each test below changes one thing about it; the root is the
    /// archive's own, so the leaf tests are not restatements of one hash call.
    fn archive_with_id() -> (CheckpointArchive, String) {
        use crate::backend::{ArchivedReadFact, RedactionReportView};

        let facts = vec![ArchivedReadFact {
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
        let content = rift_shadow_redact::redact(&SecretBytes::new(b"capture".to_vec()))
            .unwrap()
            .redacted;
        let mut archive = CheckpointArchive {
            boundary_gaps: None,
            repo: std::path::PathBuf::from("/tmp/r"),
            commit: "abc123".to_owned(),
            checkpoint_id: String::new(),
            sessions: vec![SESSION.to_owned()],
            strategy: "manual-commit".to_owned(),
            prev_hash: None,
            events: Vec::new(),
            read_facts: facts,
            serve_audit: None,
            usage: Vec::new(),
            session_agents: std::collections::BTreeMap::new(),
            window_journal: Vec::new(),
            worktree_observations: Vec::new(),
            tool_results: Vec::new(),
            content,
            report: RedactionReportView {
                total_spans: 0,
                bytes_masked: 0,
                allowlisted: 0,
                rule_pack_versions: vec!["builtin@2026.06.0".to_owned()],
                input_sha256: "c".repeat(64),
                by_detector: std::collections::BTreeMap::new(),
                by_class: std::collections::BTreeMap::new(),
            },
            transcript: None,
            tasks: Vec::new(),
            created_at_ms: Some(1),
            // A hand-built archive is in no repository: the member is ABSENT, which
            // is what keeps every frozen anchor and golden byte-identical.
            worktree: None,
            session_worktrees: std::collections::BTreeMap::new(),
        };
        archive.checkpoint_id = archive.derived_checkpoint_id().unwrap();
        let id = archive.checkpoint_id.clone();
        (archive, id)
    }

    /// The writer's id is a function of the bytes the writer archived — asserted
    /// through the archive's own artifact renderers, which is the pair `rift
    /// verify` re-derives from. If the id ever stops addressing the shard it
    /// sits in, the shard path stops being a binding and the last thing a
    /// tamperer cannot recompute is gone.
    #[test]
    fn the_archived_id_is_derivable_from_the_artifacts_the_archive_writes() {
        let (archive, id) = archive_with_id();
        assert_eq!(
            derive_checkpoint_id(&archive.sessions[0], archive.merkle_root()),
            id,
            "the id must address the root over the artifacts the shard carries"
        );
    }

    // Every leaf artifact moves the id, which is the property that makes the
    // shard TREE a function of the shard PATH. Each test below is a separate
    // artifact.

    #[test]
    fn another_session_moves_the_archived_id() {
        let (archive, id) = archive_with_id();
        let mut other_session = archive.clone();
        other_session.sessions = vec!["01JOTHERSESSION".to_owned()];
        assert_ne!(
            other_session.derived_checkpoint_id().unwrap(),
            id,
            "session"
        );
    }

    #[test]
    fn edited_content_moves_the_archived_id() {
        let (archive, id) = archive_with_id();
        let mut edited = archive.clone();
        edited.content = rift_shadow_redact::redact(&SecretBytes::new(b"edited".to_vec()))
            .unwrap()
            .redacted;
        assert_ne!(
            edited.derived_checkpoint_id().unwrap(),
            id,
            "content.redacted"
        );
    }

    #[test]
    fn dropped_read_facts_move_the_archived_id() {
        let (archive, id) = archive_with_id();
        let mut no_facts = archive.clone();
        no_facts.read_facts = Vec::new();
        assert_ne!(
            no_facts.derived_checkpoint_id().unwrap(),
            id,
            "tool_calls.jsonl"
        );
    }

    #[test]
    fn a_changed_redaction_report_moves_the_archived_id() {
        let (archive, id) = archive_with_id();
        let mut with_report = archive.clone();
        with_report.report.total_spans += 1;
        assert_ne!(
            with_report.derived_checkpoint_id().unwrap(),
            id,
            "redaction_report.json — a leaf the pre-root derivation did NOT bind"
        );
    }

    /// The anchoring commit does NOT move the id, or a second commit in one
    /// session would mint a new shard for unchanged capture and idempotence
    /// would go.
    #[test]
    fn anchor_drift_leaves_the_archived_id_alone() {
        let (archive, id) = archive_with_id();
        let mut later = archive.clone();
        later.commit = "def456".to_owned();
        later.created_at_ms = later.created_at_ms.map(|ms| ms + 60_000);
        assert_eq!(
            later.derived_checkpoint_id().unwrap(),
            id,
            "anchor drift is not capture drift"
        );
    }

    /// N ≥ 2 is DEFINED — and it is a different identity from any single
    /// session's, because cardinality and order are part of the full form.
    /// (This assertion used to be `is_none()`: the writer refused to guess
    /// until the rule existed.)
    #[test]
    fn a_second_session_is_a_different_identity() {
        let (archive, id) = archive_with_id();
        let mut two = archive.clone();
        two.sessions.push("01JSECOND".to_owned());
        let two_id = two.derived_checkpoint_id().expect("N >= 2 is defined now");
        assert_ne!(two_id, id, "a second session is a different identity");
    }

    /// A session id carrying the frame byte is refused, not framed.
    #[test]
    fn a_session_id_carrying_the_frame_byte_is_refused() {
        let (archive, _) = archive_with_id();
        let mut nul = archive.clone();
        nul.sessions = vec!["bad\0session".to_owned()];
        assert!(nul.derived_checkpoint_id().is_none(), "NUL is refused");
    }

    /// A session id from this repository's own checkpoint branch. Not a secret —
    /// it is committed in that shard's `metadata.json` — and it is here because
    /// the pin below is only worth anything against the real bytes.
    const MEASURED_SESSION: &str = "ac5515af-4e58-426d-b45c-9e52b7ebd7da";

    /// **The superseded derivation, pinned to the shard that still carries it.**
    ///
    /// `f62a4930…` is a live shard on `refs/heads/rift/checkpoints/v1` in this
    /// repository: `canon_version` 1, one session, an empty capture, no read
    /// facts. Under [`rift_core::checkpoint_id::from_capture`] it re-derives to
    /// `23b75dac…` and would be called tampered; under
    /// [`rift_core::checkpoint_id::from_capture_only`] it re-derives to itself.
    ///
    /// So this test is the reason `checkpoint_ids_for_shard` returns a set. If
    /// the superseded derivation is ever dropped as "dead code", this reds with
    /// the id of the shard that the drop would have condemned.
    ///
    /// It also pins the era gate from the OTHER side: all 103 shards on that ref
    /// are epoch 1, so if epoch 1 ever started demanding the root binding, every
    /// one of them would be called tampered at once.
    #[test]
    fn the_superseded_derivation_still_addresses_a_live_shard() {
        use crate::backend::ShardLeafBytes;
        const MEASURED_ID: &str =
            "f62a493024792e8f4dc540579c3389e2b4f8826be45d3f51ebb336a4424cf960";
        let leaves = ShardLeafBytes {
            boundary_gaps: None,
            content_redacted: b"",
            ..ShardLeafBytes::default()
        };
        let ids = checkpoint_ids_for_shard(&[MEASURED_SESSION.to_owned()], 1, &leaves);
        assert!(
            ids.iter().any(|i| i == MEASURED_ID),
            "the live shard f62a4930… must still address its own bytes: {ids:?}"
        );
        assert_ne!(
            rift_core::checkpoint_id::from_capture(MEASURED_SESSION, b"", None),
            MEASURED_ID,
            "…and it is NOT that era's newer derivation, which is the whole point"
        );
        // The set is newest-first within the era.
        assert_eq!(
            ids.first().map(String::as_str),
            Some(rift_core::checkpoint_id::from_capture(MEASURED_SESSION, b"", None).as_str())
        );
        // …and the ROOT-BOUND era does not accept either of them, or binding the
        // root would buy nothing: a tamperer would just satisfy the older rule.
        let root_era = checkpoint_ids_for_shard(
            &[MEASURED_SESSION.to_owned()],
            crate::backend::ROOT_BOUND_ID_MIN_CANON_VERSION,
            &leaves,
        );
        assert!(
            !root_era.iter().any(|i| ids.contains(i)),
            "epoch 3 must not accept a pre-root derivation: {root_era:?}"
        );
    }

    /// 2c wiring + the recorded trailer divergence, both as facts.
    ///
    /// A condense handed a sealed transcript archives it: the shard gains the
    /// pair through the durable path and the id moves. And the trailer
    /// PREDICTION — transcript-blind by ruling (decision (b), 2026-08-29:
    /// `prepare-commit-msg` owns the 100 ms budget and the read-side union is
    /// the permanent answer) — matches the transcript-less condense exactly
    /// and the transcript-carrying one not at all. If the prediction ever
    /// learns the transcript, the second half of this test reds and the
    /// ruling above must be revisited with it.
    #[tokio::test]
    async fn a_condense_with_a_transcript_diverges_from_the_trailer_prediction() {
        let s = store();
        let backend = crate::backend::NativeBackend::in_memory();
        let raw = SecretBytes::new(b"capture".to_vec());
        let packs = RulePackSet::builtin();
        let predicted =
            predicted_checkpoint_id(SESSION, &raw, &SessionFacts::default(), &packs).unwrap();

        let (pair, raw_lines) = crate::transcript::archived_transcript(
            &SecretBytes::new(br#"{"type":"user","message":"hi"}"#.to_vec()),
            0,
            &packs,
        )
        .unwrap();
        assert_eq!(raw_lines, 1, "one non-empty raw line consumed");
        let with_transcript = condense(
            &backend,
            &s,
            std::path::Path::new("/r"),
            SESSION,
            &raw,
            StrategyKind::ManualCommit,
            ArchiveReason::Commit,
            "abc123",
            1,
            SessionFacts::default(),
            &packs,
            Some(pair),
            Vec::new(),
        )
        .await
        .unwrap();
        assert_ne!(
            with_transcript, predicted,
            "the transcript moves the id; the trailer prediction does not see it (recorded)"
        );
        // The shard really carries the pair, through the durable path:
        // content.redacted + transcript.compact.jsonl + transcript.jsonl.
        assert_eq!(backend.writer().durable_count(), 3);

        let bare = condense(
            &backend,
            &s,
            std::path::Path::new("/r"),
            SESSION,
            &raw,
            StrategyKind::ManualCommit,
            ArchiveReason::Commit,
            "abc123",
            2,
            SessionFacts::default(),
            &packs,
            None,
            Vec::new(),
        )
        .await
        .unwrap();
        assert_eq!(
            bare, predicted,
            "without a transcript the prediction is exact — the divergence is the transcript alone"
        );
    }

    /// **The digest leaf must not widen the trailer divergence.** The shape digest
    /// derives from read facts, which BOTH hooks see — so the trailer
    /// prediction stays EXACT for a fact-carrying, transcript-less session
    /// even though its shard now carries a digest leaf. (The transcript-side
    /// digests ride the one recorded divergence, whose pin is the test
    /// above.) If digest derivation ever splits between the two paths, this
    /// equality reds.
    #[tokio::test]
    async fn the_trailer_prediction_stays_exact_for_a_fact_carrying_session() {
        let s = store();
        let backend = crate::backend::NativeBackend::in_memory();
        let raw = SecretBytes::new(b"capture".to_vec());
        let packs = RulePackSet::builtin();
        let facts = SessionFacts {
            boundary_gaps: None,
            read_facts: vec![crate::backend::ArchivedReadFact {
                tool_use_id: "t1".into(),
                tool_name: "Read".into(),
                path: "src/a.rs".into(),
                content_sha256: "aa".into(),
                input_tokens: None,
                at: None,
                turn: None,
                blob_oid: None,
                tokenizer: None,
            }],
            serve_audit: None,
            usage: Vec::new(),
            agent: None,
            window_journal: Vec::new(),
            worktree_observations: Vec::new(),
            tool_results: Vec::new(),
        };
        let predicted = predicted_checkpoint_id(SESSION, &raw, &facts, &packs).unwrap();
        let archived = condense(
            &backend,
            &s,
            std::path::Path::new("/r"),
            SESSION,
            &raw,
            StrategyKind::ManualCommit,
            ArchiveReason::Commit,
            "abc123",
            1,
            facts,
            &packs,
            None,
            Vec::new(),
        )
        .await
        .unwrap();
        assert_eq!(
            archived, predicted,
            "the shape digest derives from facts both hooks share"
        );
    }

    /// The facts a session with frozen serves hands both hooks: one schema-1
    /// line and one schema-4 line, the shape the one writer freezes.
    fn facts_with_serves(extra: bool) -> SessionFacts {
        let mut lines = String::from(concat!(
            r#"{"idempotency_key":"01JCALLA","schema":4,"verb":"memd.grep_capped"}"#,
            "\n",
            r#"{"idempotency_key":"aaaa","schema":1,"verb":"memd.grep_capped"}"#,
            "\n",
        ));
        if extra {
            lines.push_str(r#"{"idempotency_key":"bbbb","schema":1,"verb":"memd.grep_capped"}"#);
            lines.push('\n');
        }
        SessionFacts {
            boundary_gaps: None,
            read_facts: Vec::new(),
            serve_audit: Some(lines.into_bytes()),
            usage: Vec::new(),
            agent: None,
            window_journal: Vec::new(),
            worktree_observations: Vec::new(),
            tool_results: Vec::new(),
        }
    }

    /// Condense `facts` for [`SESSION`] over a fresh in-memory backend.
    async fn condensed_with(facts: SessionFacts, now_ms: i64) -> CheckpointId {
        condense(
            &crate::backend::NativeBackend::in_memory(),
            &store(),
            std::path::Path::new("/r"),
            SESSION,
            &SecretBytes::new(b"capture".to_vec()),
            StrategyKind::ManualCommit,
            ArchiveReason::Commit,
            "abc123",
            now_ms,
            facts,
            &RulePackSet::builtin(),
            None,
            Vec::new(),
        )
        .await
        .unwrap()
    }

    /// **One id for the prediction and the archive of a session with serves.**
    /// The trailer hook and the worker fold the same frozen envelopes through
    /// the same assembly, so the prediction stays exact once serves are
    /// frozen — and a session that froze none predicts and archives the id it
    /// always did.
    #[tokio::test]
    async fn the_trailer_prediction_and_the_archive_mint_one_id_with_and_without_serves() {
        let raw = SecretBytes::new(b"capture".to_vec());
        let packs = RulePackSet::builtin();
        let with = facts_with_serves(false);
        let predicted = predicted_checkpoint_id(SESSION, &raw, &with, &packs).unwrap();
        assert_eq!(condensed_with(with, 1).await, predicted);

        let bare =
            predicted_checkpoint_id(SESSION, &raw, &SessionFacts::default(), &packs).unwrap();
        assert_eq!(condensed_with(SessionFacts::default(), 1).await, bare);
        assert_ne!(bare, predicted, "the frozen serves are a leaf of the id");
    }

    /// Re-archiving an unchanged session is the same checkpoint, clock and all;
    /// one more frozen serve is a new one.
    #[tokio::test]
    async fn an_unchanged_serve_audit_re_archives_to_the_same_id_and_one_more_serve_moves_it() {
        let first = condensed_with(facts_with_serves(false), 1).await;
        assert_eq!(condensed_with(facts_with_serves(false), 2).await, first);
        assert_ne!(condensed_with(facts_with_serves(true), 1).await, first);
    }

    /// An empty buffer is absence, not an empty artifact: the id is the one a
    /// session that froze nothing derives.
    #[tokio::test]
    async fn an_empty_serve_audit_is_the_same_as_none() {
        let empty = SessionFacts {
            boundary_gaps: None,
            read_facts: Vec::new(),
            serve_audit: Some(Vec::new()),
            usage: Vec::new(),
            agent: None,
            window_journal: Vec::new(),
            worktree_observations: Vec::new(),
            tool_results: Vec::new(),
        };
        assert_eq!(
            condensed_with(empty, 1).await,
            condensed_with(SessionFacts::default(), 1).await
        );
    }

    /// The facts a session with `calls` settled model calls hands both hooks,
    /// one per turn, alternating between two models.
    fn facts_with_usage(calls: i64) -> SessionFacts {
        let usage = (1..=calls)
            .map(|turn| crate::backend::ArchivedUsageFact {
                session: SESSION.to_owned(),
                turn,
                call_id: format!("call-{turn}"),
                model: if turn % 2 == 0 {
                    "claude-opus-5"
                } else {
                    "claude-fable-5"
                }
                .to_owned(),
                at: 100 * turn,
                uncached_input: 20,
                cached_input: 30,
                cache_creation: 0,
                output: 2,
                method: "provider_reported".to_owned(),
            })
            .collect();
        SessionFacts {
            boundary_gaps: None,
            usage,
            ..SessionFacts::default()
        }
    }

    /// **One id for the prediction and the archive of a session with settled
    /// usage**, and the id a session with none always derived: the usage is a
    /// leaf of the id, folded identically by both hooks.
    #[tokio::test]
    async fn the_trailer_prediction_and_the_archive_mint_one_id_with_and_without_usage() {
        let raw = SecretBytes::new(b"capture".to_vec());
        let packs = RulePackSet::builtin();
        let with = facts_with_usage(2);
        let predicted = predicted_checkpoint_id(SESSION, &raw, &with, &packs).unwrap();
        assert_eq!(condensed_with(with, 1).await, predicted);

        let bare =
            predicted_checkpoint_id(SESSION, &raw, &SessionFacts::default(), &packs).unwrap();
        assert_eq!(condensed_with(SessionFacts::default(), 1).await, bare);
        assert_ne!(bare, predicted, "the settled usage is a leaf of the id");
    }

    /// Re-archiving a session whose usage is unchanged is the same checkpoint,
    /// clock and all; one more settled call is a new one.
    #[tokio::test]
    async fn unchanged_usage_re_archives_to_the_same_id_and_one_more_settled_call_moves_it() {
        let first = condensed_with(facts_with_usage(2), 1).await;
        assert_eq!(condensed_with(facts_with_usage(2), 2).await, first);
        assert_ne!(condensed_with(facts_with_usage(3), 1).await, first);
    }

    /// **One id for the prediction and the archive of a session whose agent
    /// is named**, and a different one from the same session with none: the
    /// agent is a leaf of the id, folded identically by both hooks.
    #[tokio::test]
    async fn the_trailer_prediction_and_the_archive_mint_one_id_with_the_agent() {
        let raw = SecretBytes::new(b"capture".to_vec());
        let packs = RulePackSet::builtin();
        let named = SessionFacts {
            boundary_gaps: None,
            agent: Some("codex".to_owned()),
            ..SessionFacts::default()
        };
        let predicted = predicted_checkpoint_id(SESSION, &raw, &named, &packs).unwrap();
        assert_eq!(condensed_with(named.clone(), 1).await, predicted);
        assert_eq!(
            condensed_with(named, 2).await,
            predicted,
            "unchanged, same id"
        );

        let bare =
            predicted_checkpoint_id(SESSION, &raw, &SessionFacts::default(), &packs).unwrap();
        assert_ne!(bare, predicted, "the agent is a leaf of the id");
    }

    /// **One id for the prediction and the archive of a session whose
    /// windows journaled rows**, the same id again when nothing was added,
    /// and a different one from the same session with no rows: the journal is
    /// a leaf of the id, folded identically by both hooks and cumulative.
    #[tokio::test]
    async fn the_trailer_prediction_and_the_archive_mint_one_id_with_window_rows() {
        let raw = SecretBytes::new(b"capture".to_vec());
        let packs = RulePackSet::builtin();
        let evict = crate::backend::ArchivedWindowRow::Evict {
            reason: "pre_compact".to_owned(),
        };
        let row = crate::backend::ArchivedWindowFact {
            window: "c".repeat(64),
            subagent: None,
            turn: 2,
            at: 7,
            row: evict,
        };
        let journaled = SessionFacts {
            boundary_gaps: None,
            window_journal: vec![row],
            ..SessionFacts::default()
        };
        let predicted = predicted_checkpoint_id(SESSION, &raw, &journaled, &packs).unwrap();
        assert_eq!(condensed_with(journaled.clone(), 1).await, predicted);
        assert_eq!(condensed_with(journaled, 2).await, predicted, "unchanged");

        let bare =
            predicted_checkpoint_id(SESSION, &raw, &SessionFacts::default(), &packs).unwrap();
        assert_ne!(bare, predicted, "the journal is a leaf of the id");
    }

    /// **One id for the prediction and the archive of a session that
    /// searched**, and a different one with no measured result: the tool
    /// results are a leaf of the id, folded identically by both hooks.
    #[tokio::test]
    async fn the_trailer_prediction_and_the_archive_mint_one_id_with_tool_results() {
        let raw = SecretBytes::new(b"capture".to_vec());
        let packs = RulePackSet::builtin();
        let searched = SessionFacts {
            boundary_gaps: None,
            tool_results: vec![crate::backend::ArchivedToolResult {
                id: "a".repeat(64),
                session: SESSION.to_owned(),
                tool: "Glob".to_owned(),
                channel: "search".to_owned(),
                output_tokens: 40,
                tokenizer: "cl100k_base".to_owned(),
                basis: "in_process".to_owned(),
                recorded_at: 7,
            }],
            ..SessionFacts::default()
        };
        let predicted = predicted_checkpoint_id(SESSION, &raw, &searched, &packs).unwrap();
        assert_eq!(condensed_with(searched, 1).await, predicted);
        let bare =
            predicted_checkpoint_id(SESSION, &raw, &SessionFacts::default(), &packs).unwrap();
        assert_ne!(bare, predicted, "the tool results are a leaf of the id");
    }

    /// **The rollup rule, pinned at the assembly.** A
    /// subagent that STARTED before this checkpoint's boundary still rolls up
    /// to the checkpoint that saw it finish: condense hands its `tasks` to the
    /// archive verbatim — no window filter on `started_at_ms` — so the id the
    /// backend receives equals the id of an archive constructed with that task
    /// included, and the parent metadata's `task_tokens` sums it. If condense
    /// ever grows a boundary gate on tasks, the id comparison here reds.
    #[tokio::test]
    async fn a_task_started_before_the_boundary_still_rolls_up_to_the_parent() {
        use crate::backend::{ArchivedTask, NativeBackend};

        let s = store();
        let backend = NativeBackend::in_memory();
        let raw = SecretBytes::new(b"capture".to_vec());
        let packs = RulePackSet::builtin();
        // A transcript whose boundary is past line 0: the second line is this
        // checkpoint's slice, so line 0 — and anything that happened during it,
        // including the task's start — precedes the boundary.
        let raw_transcript = br#"{"type":"user","message":"first"}
{"type":"user","message":"second"}"#;
        let (pair, _) = crate::transcript::archived_transcript(
            &SecretBytes::new(raw_transcript.to_vec()),
            1,
            &packs,
        )
        .unwrap();
        assert!(pair.compact_start > 0, "the boundary is real");
        let before_boundary_task = ArchivedTask {
            tool_use_id: "toolu_early".into(),
            agent_id: "explore".into(),
            // Started long before the checkpoint (and its boundary) existed…
            started_at_ms: 1,
            ended_at_ms: 900,
            input_tokens: Some(120),
            output_tokens: Some(34),
            transcript: None,
            provenance: None,
        };

        let archived = condense(
            &backend,
            &s,
            std::path::Path::new("/r"),
            SESSION,
            &raw,
            StrategyKind::ManualCommit,
            ArchiveReason::Commit,
            "abc123",
            1_000,
            SessionFacts::default(),
            &packs,
            Some(pair.clone()),
            vec![before_boundary_task.clone()],
        )
        .await
        .unwrap();

        // The id condense archived equals the id of an archive that INCLUDES
        // the early task — i.e. nothing filtered it out on its start time.
        let out = rift_shadow_redact::redact_with(&raw, &packs).unwrap();
        let mut twin = archive_for_id(
            SESSION,
            out.redacted,
            RedactionReportView::from_redact(&out.report),
            SessionFacts::default(),
        );
        twin.transcript = Some(pair);
        twin.tasks = vec![before_boundary_task];
        assert_eq!(
            archived,
            twin.derived_checkpoint_id().unwrap(),
            "condense must hand tasks to the archive unfiltered"
        );
        // …and the parent-side rollup sums the early task's tokens.
        assert_eq!(twin.task_token_rollup(), Some((120, 34)));
        let meta = String::from_utf8(twin.metadata_json()).unwrap();
        assert!(
            meta.contains(r#""task_tokens":{"input_tokens":120,"output_tokens":34}"#),
            "the rollup rides the parent metadata: {meta}"
        );
    }

    #[tokio::test]
    async fn redaction_survival_seeded_secret_never_in_checkpoint_inputs() {
        // The checkpoint id is derived over the REDACTED bytes; a seeded secret in
        // the raw capture is masked before it can address anything durable.
        let raw_secret = "AKIAIOSFODNN7EXAMPLE";
        let out = rift_shadow_redact::redact(&SecretBytes::new(
            format!("aws key {raw_secret} here").into_bytes(),
        ))
        .unwrap();
        let redacted = String::from_utf8_lossy(out.redacted.as_slice()).into_owned();
        assert!(
            !redacted.contains(raw_secret),
            "seeded secret must not survive"
        );
    }

    /// A backend that keeps the whole archive, so the member can be read.
    #[derive(Default)]
    struct KeepingBackend {
        last: std::cell::RefCell<Option<CheckpointArchive>>,
    }
    impl ShadowBackend for KeepingBackend {
        fn archive_checkpoint(&self, archive: &CheckpointArchive) -> Result<()> {
            *self.last.borrow_mut() = Some(archive.clone());
            Ok(())
        }
    }

    /// A repository with a main tree at `main` and a linked member at `member`
    /// (admin name `swt-a`, on `swt/a-foundations`); `nowhere` is in neither.
    fn repo_with_a_member() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        let main_git = rift_git::test_support::fake_repo(&tmp.path().join("main")).unwrap();
        let member_tree = tmp.path().join("member");
        let admin =
            rift_git::test_support::fake_worktree(&main_git, &member_tree, "swt-a").unwrap();
        std::fs::write(admin.join("HEAD"), "ref: refs/heads/swt/a-foundations\n").unwrap();
        tmp
    }

    /// The archive one condense of an empty capture hands its backend when run
    /// from `repo` — a fresh store and backend each time.
    async fn archive_condensed_in(repo: &std::path::Path) -> CheckpointArchive {
        let backend = KeepingBackend::default();
        let s = store();
        let raw = SecretBytes::new(Vec::new());
        condense(
            &backend,
            &s,
            repo,
            "01JSESSION",
            &raw,
            StrategyKind::ManualCommit,
            ArchiveReason::Commit,
            "abc123",
            1,
            SessionFacts::default(),
            &RulePackSet::builtin(),
            None,
            Vec::new(),
        )
        .await
        .unwrap();
        let kept = backend.last.borrow().clone().unwrap();
        kept
    }

    /// The label is derived from **the worktree the condense ran
    /// in** — `repo`, the checkout the caller is archiving from — and not
    /// handed in by a caller who could get it wrong.
    #[tokio::test]
    async fn a_condense_labels_the_shard_with_the_worktree_it_ran_in() {
        let tmp = repo_with_a_member();
        let from_member = archive_condensed_in(&tmp.path().join("member")).await;
        let label = from_member.worktree.clone().expect("a member is labelled");
        assert_eq!(label.name, "swt-a", "the ADMIN name, not the checkout's");
        assert!(label.linked);
        assert_eq!(label.branch.as_deref(), Some("swt/a-foundations"));
        assert_eq!(label.token.len(), 12);
        assert!(
            from_member.session_worktrees.is_empty(),
            "one session, no map"
        );
    }

    #[tokio::test]
    async fn a_condense_in_the_main_tree_labels_it_main_with_its_own_token() {
        let tmp = repo_with_a_member();
        let from_member = archive_condensed_in(&tmp.path().join("member")).await;
        let label = from_member.worktree.clone().expect("a member is labelled");
        let from_main = archive_condensed_in(&tmp.path().join("main")).await;
        let main_label = from_main
            .worktree
            .clone()
            .expect("the main tree is labelled");
        assert_eq!(main_label.name, "main");
        assert!(!main_label.linked);
        assert_ne!(main_label.token, label.token, "two members, two tokens");
    }

    /// A condense outside any repository (which every hand-built fixture is)
    /// carries no member at all — unknown, never `main` — which is what keeps
    /// the frozen anchors and goldens frozen.
    #[tokio::test]
    async fn a_condense_outside_any_repository_writes_no_worktree_member() {
        let tmp = repo_with_a_member();
        let nowhere = archive_condensed_in(&tmp.path().join("nowhere")).await;
        assert_eq!(nowhere.worktree, None);
        assert!(
            !String::from_utf8(nowhere.metadata_json())
                .unwrap()
                .contains("worktree"),
            "a label-less condense writes the bytes it always wrote"
        );
    }

    /// The label moves no id: the same capture, two worktrees, one checkpoint
    /// id.
    #[tokio::test]
    async fn the_worktree_label_moves_no_checkpoint_id() {
        let tmp = repo_with_a_member();
        let from_member = archive_condensed_in(&tmp.path().join("member")).await;
        let from_main = archive_condensed_in(&tmp.path().join("main")).await;
        let nowhere = archive_condensed_in(&tmp.path().join("nowhere")).await;
        assert_eq!(from_member.checkpoint_id, from_main.checkpoint_id);
        assert_eq!(from_member.checkpoint_id, nowhere.checkpoint_id);
    }
}
