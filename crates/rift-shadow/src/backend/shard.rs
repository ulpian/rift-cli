//! The shard writer: the checkpoint archive laid out as a git tree and
//! grafted onto the orphan checkpoint branch. Separate from the seam so the
//! plumbing — which blob takes the durable path, which the structural one,
//! how the subtrees nest — sits in one file the redaction wall can be read
//! off.

use rift_core::{RedactedBytes, Result};
use rift_git::{GitFileMode, NativeGitWrite, OrphanCommit, TreeWriteEntry};

use super::{
    ArchivedTask, ArchivedTranscript, CheckpointArchive, NativeBackend, NATIVE_CHECKPOINT_REF,
    TRANSCRIPT_COMPACT_NAME, TRANSCRIPT_FULL_NAME,
};

impl<W: NativeGitWrite> NativeBackend<W> {
    /// Write the checkpoint shard: structural artifacts + the durable content blob,
    /// assembled into a leaf tree grafted onto the orphan branch. Idempotent (the
    /// underlying `commit_on_orphan` no-ops an already-present shard).
    pub(super) fn write_shard(&self, archive: &CheckpointArchive) -> Result<()> {
        // Refuse malformed task sets BEFORE any object write: a tool-use id is
        // a git tree name, and a duplicate would be two entries with one name.
        validate_task_set(&archive.tasks)?;
        let mut entries = self.write_top_level_blobs(archive)?;
        if let Some(sessions_entry) = self.write_session_trees(archive)? {
            entries.push(sessions_entry);
        }
        if let Some(tasks_entry) = self.write_task_trees(&archive.tasks)? {
            entries.push(tasks_entry);
        }
        // Canonical git tree order (bytewise by name) so the leaf tree is stable.
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        let leaf = self.writer.write_tree(&entries)?;
        self.graft_shard(archive, &leaf)
    }

    /// The blobs at the shard's top level, in the order the writer has always
    /// written them: the redacted capture first — the one content path through
    /// the redaction wall — then the Rift-authored structural artifacts, then
    /// the optional ones a shard carries only when it has them.
    fn write_top_level_blobs(&self, archive: &CheckpointArchive) -> Result<Vec<TreeWriteEntry>> {
        let mut entries = vec![
            self.durable_entry("content.redacted", &archive.content)?,
            self.structural_entry("content_hash.txt", &archive.content_hash_txt())?,
            self.structural_entry("metadata.json", &archive.metadata_json())?,
            self.structural_entry("redaction_report.json", &archive.redaction_report_json())?,
        ];
        if let Some(full) = archive.full_jsonl() {
            entries.push(self.structural_entry("full.jsonl", &full)?);
        }
        // The read facts the recall plane ingests: ids/paths/hashes
        // only — structural by construction, like metadata.json.
        if let Some(tool_calls) = archive.tool_calls_jsonl() {
            entries.push(self.structural_entry("tool_calls.jsonl", &tool_calls)?);
        }
        // The frozen serve envelopes: verbs, closed tags, handles, counts, ids
        // and timestamps, canonicalised by the one writer before they reach
        // here — structural, like the read facts beside them.
        if let Some(serve_audit) = archive.serve_audit_jsonl() {
            entries.push(self.structural_entry(super::artifacts::SERVE_AUDIT_NAME, serve_audit)?);
        }
        if let Some(transcript) = &archive.transcript {
            self.push_transcript_pair(transcript, &mut entries)?;
        }
        // The settled usage: token counts, a counting method, a model id,
        // a call key and a time, each identifier screened by the one writer —
        // structural, like the read facts.
        if let Some(usage) = archive.usage_jsonl() {
            entries.push(self.structural_entry(super::artifacts::USAGE_NAME, &usage)?);
        }
        self.push_digest_entries(archive, &mut entries)?;
        Ok(entries)
    }

    /// Write the present `digest.*` artifacts: counts,
    /// indexes and content addresses only — structural by the containment pin
    /// (`a_digest_never_quotes_the_prompt_it_locates`).
    fn push_digest_entries(
        &self,
        archive: &CheckpointArchive,
        entries: &mut Vec<TreeWriteEntry>,
    ) -> Result<()> {
        for (name, bytes) in [
            (
                crate::digest::DIGEST_FAILURES_NAME,
                archive.digest_failures_json(),
            ),
            (
                crate::digest::DIGEST_INTENT_NAME,
                archive.digest_intent_json(),
            ),
            (crate::digest::DIGEST_LOG_NAME, archive.digest_log_json()),
            (
                crate::digest::DIGEST_SHAPE_NAME,
                archive.digest_shape_json(),
            ),
        ] {
            if let Some(bytes) = bytes {
                entries.push(self.structural_entry(name, &bytes)?);
            }
        }
        Ok(())
    }

    /// Write a transcript pair. CAPTURE, not structure: both
    /// are `RedactedBytes` and take the durable path like `content.redacted`
    /// — the type is the proof they passed the gate, and `write_durable_blob`
    /// is the only door bytes of that type fit through. One writer for the
    /// parent's pair and every task's, so the two cannot take different doors.
    fn push_transcript_pair(
        &self,
        transcript: &ArchivedTranscript,
        entries: &mut Vec<TreeWriteEntry>,
    ) -> Result<()> {
        entries.push(self.durable_entry(TRANSCRIPT_COMPACT_NAME, &transcript.compact)?);
        entries.push(self.durable_entry(TRANSCRIPT_FULL_NAME, &transcript.full)?);
        Ok(())
    }

    /// Write the per-session dirs of a multi-session checkpoint —
    /// `sessions/<index>/metadata.json`, structural — and return the top-level
    /// `sessions` tree entry, or `None` below two sessions, so a
    /// single-session shard's tree never moved.
    fn write_session_trees(&self, archive: &CheckpointArchive) -> Result<Option<TreeWriteEntry>> {
        let session_docs = archive.session_meta_docs();
        if session_docs.is_empty() {
            return Ok(None);
        }
        let mut dir_entries = Vec::with_capacity(session_docs.len());
        for (index_name, bytes) in &session_docs {
            let inner = vec![self.structural_entry("metadata.json", bytes)?];
            dir_entries.push(self.tree_entry(index_name.clone(), &inner)?);
        }
        dir_entries.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(Some(self.tree_entry("sessions".into(), &dir_entries)?))
    }

    /// Write the `tasks/<tool-use-id>/` subtrees and return
    /// the top-level `tasks` tree entry, or `None` when the checkpoint carries
    /// no task — so a taskless shard's tree is exactly what it always was.
    ///
    /// Each subtree holds the task's own metadata (structural — identifiers,
    /// capture-time stamps, agent-reported counts) and, when present, its
    /// transcript pair (capture — the durable path, same wall as the parent's).
    fn write_task_trees(&self, tasks: &[ArchivedTask]) -> Result<Option<TreeWriteEntry>> {
        if tasks.is_empty() {
            return Ok(None);
        }
        let mut sorted: Vec<&ArchivedTask> = tasks.iter().collect();
        sorted.sort_by(|a, b| a.tool_use_id.as_bytes().cmp(b.tool_use_id.as_bytes()));
        let mut task_entries = Vec::with_capacity(sorted.len());
        for task in sorted {
            let mut inner = vec![self.structural_entry("metadata.json", &task.metadata_json())?];
            if let Some(transcript) = &task.transcript {
                self.push_transcript_pair(transcript, &mut inner)?;
            }
            inner.sort_by(|a, b| a.name.cmp(&b.name));
            task_entries.push(self.tree_entry(task.tool_use_id.clone(), &inner)?);
        }
        Ok(Some(self.tree_entry("tasks".into(), &task_entries)?))
    }

    /// Graft the finished leaf tree onto the orphan branch at the shard path.
    fn graft_shard(&self, archive: &CheckpointArchive, leaf: &str) -> Result<()> {
        let shard_path = archive.shard_path();
        // Epoch seconds for the commit date (derived from content, never wall
        // clock); a time-less archive dates its commit at epoch, exactly as
        // every pre-`created_at` shard did.
        let date_epoch = archive.created_at_ms.unwrap_or(0).div_euclid(1000);
        self.writer.commit_on_orphan(&OrphanCommit {
            branch: NATIVE_CHECKPOINT_REF,
            shard_path: &shard_path,
            tree_oid: leaf,
            message: "rift: checkpoint",
            ident: &self.ident,
            date_epoch,
        })?;
        Ok(())
    }

    /// A blob entry for redacted CAPTURE: the durable path, which only a
    /// [`RedactedBytes`] fits through.
    fn durable_entry(&self, name: &str, body: &RedactedBytes) -> Result<TreeWriteEntry> {
        Ok(TreeWriteEntry {
            mode: GitFileMode::Blob,
            name: name.into(),
            oid: self.writer.write_durable_blob(body)?,
        })
    }

    /// A blob entry for a Rift-authored STRUCTURAL artifact: counts, ids and
    /// content addresses that never touched a secret.
    fn structural_entry(&self, name: &str, bytes: &[u8]) -> Result<TreeWriteEntry> {
        Ok(TreeWriteEntry {
            mode: GitFileMode::Blob,
            name: name.into(),
            oid: self.writer.write_structural_blob(bytes)?,
        })
    }

    /// A subtree entry over already-written entries.
    fn tree_entry(&self, name: String, inner: &[TreeWriteEntry]) -> Result<TreeWriteEntry> {
        Ok(TreeWriteEntry {
            mode: GitFileMode::Tree,
            name,
            oid: self.writer.write_tree(inner)?,
        })
    }
}

/// Refuse a task set the tree cannot faithfully hold: a tool-use id that is
/// not a safe single tree component (it names the `tasks/<id>/` subtree), or
/// two tasks sharing one id (one name cannot carry two subtrees, and silently
/// keeping either would drop capture). `rift-git`'s `mktree` screens names
/// again downstream; this refusal is the typed one that fires before any
/// object write, so a bad dispatch costs nothing durable.
fn validate_task_set(tasks: &[ArchivedTask]) -> Result<()> {
    let mut ids: Vec<&str> = Vec::with_capacity(tasks.len());
    for t in tasks {
        let id = t.tool_use_id.as_str();
        if id.is_empty() || id == "." || id == ".." || id.contains('/') || id.contains('\0') {
            return Err(rift_core::RiftError::validity(
                "a task tool-use id must be a safe git tree name",
            ));
        }
        ids.push(id);
    }
    ids.sort_unstable();
    let distinct = ids.len();
    ids.dedup();
    if ids.len() != distinct {
        return Err(rift_core::RiftError::validity(
            "two archived tasks share one tool-use id",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::{archive, task, transcript_of};
    use super::super::{ArchivedEvent, ShadowBackend, NATIVE_CANON_VERSION};
    use super::*;
    use rift_core::SecretBytes;

    /// **The task subtree's signal test.** A
    /// `tasks/<tool-use-id>/` subtree is part of the leaf spec: adding one
    /// MOVES the checkpoint id (so the shard path binds it), the second
    /// archive appends a new shard instead of colliding, and re-archiving the
    /// identical task-carrying checkpoint stays a no-op — the same three facts
    /// `an_id_bound_artifact_mints_a_new_shard_instead_of_refusing` pinned for
    /// `full.jsonl`, now for a nested artifact.
    #[test]
    fn a_task_subtree_joins_the_leaf_spec_and_moves_the_id() {
        let n = NativeBackend::in_memory();
        let mut taskless = archive();
        taskless.checkpoint_id = taskless.derived_checkpoint_id().unwrap();
        n.archive_checkpoint(&taskless).unwrap();

        let mut with_task = archive();
        with_task.tasks = vec![task("toolu_01")];
        with_task.checkpoint_id = with_task.derived_checkpoint_id().unwrap();
        assert_ne!(
            with_task.checkpoint_id, taskless.checkpoint_id,
            "the task subtree is bound, so the id — and the shard path — moved"
        );
        n.archive_checkpoint(&with_task)
            .expect("a bound task appends a new shard rather than colliding");
        assert_eq!(
            n.writer().shard_paths().len(),
            2,
            "both shards on the branch"
        );

        // The parent metadata enumerates the subtree for the reader.
        let meta = String::from_utf8(with_task.metadata_json()).unwrap();
        assert!(
            meta.contains(r#""tasks":["toolu_01"]"#),
            "metadata lists the task ids: {meta}"
        );

        // Idempotence survives the subtree: same bytes, same path, no-op.
        let tip = n.writer().branch_tip();
        n.archive_checkpoint(&with_task).unwrap();
        assert_eq!(n.writer().branch_tip(), tip, "re-archive is a no-op");
        assert_eq!(n.writer().shard_paths().len(), 2);
    }

    /// A tool-use id names a git tree entry, so the writer refuses the ids a
    /// tree cannot faithfully hold — BEFORE any object write, so a bad
    /// dispatch costs nothing durable.
    #[test]
    fn an_unsafe_or_duplicate_task_id_is_refused_before_any_write() {
        for bad in ["", ".", "..", "a/b", "a\0b"] {
            let n = NativeBackend::in_memory();
            let mut a = archive();
            a.tasks = vec![task(bad)];
            let err = n.archive_checkpoint(&a).expect_err("unsafe id must refuse");
            assert!(err.to_string().contains("safe git tree name"), "{err}");
            assert_eq!(n.writer().durable_count(), 0, "refused before any write");
            assert_eq!(n.writer().structural_count(), 0);
        }
        let n = NativeBackend::in_memory();
        let mut dup = archive();
        dup.tasks = vec![task("toolu_01"), task("toolu_01")];
        let err = n
            .archive_checkpoint(&dup)
            .expect_err("duplicate ids refuse");
        assert!(err.to_string().contains("share one tool-use id"), "{err}");
        assert_eq!(n.writer().durable_count(), 0);
    }

    /// **The empty-transcript class — legacy backend defect 0x0011 — covered by
    /// name.** A session whose transcript file exists but is EMPTY archives
    /// cleanly, and the resulting shard is a DIFFERENT identity from the same
    /// capture with no transcript at all: empty is a value, absent is a
    /// shape, and collapsing them is the bug class. (leaf_hash(&[]) is a real
    /// digest, not the empty-tree root — the leaf spec's own doc contract.)
    #[test]
    fn an_empty_transcript_archives_distinct_from_no_transcript_the_0x0011_class() {
        let empty_pair = transcript_of(b"");
        assert_eq!(empty_pair.compact_start, 0);

        let mut without = archive();
        without.checkpoint_id = without.derived_checkpoint_id().unwrap();
        let mut with_empty = archive();
        with_empty.transcript = Some(empty_pair);
        with_empty.checkpoint_id = with_empty.derived_checkpoint_id().unwrap();
        assert_ne!(
            with_empty.checkpoint_id, without.checkpoint_id,
            "an empty transcript is capture content, not absence"
        );

        // Both archive cleanly — the class does not crash the writer — and
        // both shards coexist on the branch.
        let n = NativeBackend::in_memory();
        n.archive_checkpoint(&without).unwrap();
        n.archive_checkpoint(&with_empty).unwrap();
        assert_eq!(n.writer().shard_paths().len(), 2);
        // The empty pair still records its boundary in metadata.
        let meta = String::from_utf8(with_empty.metadata_json()).unwrap();
        assert!(meta.contains(r#""compact_transcript_start":0"#), "{meta}");
    }

    #[test]
    fn the_archive_writes_the_shard_the_wall_and_the_branch() {
        let n = NativeBackend::in_memory();
        n.archive_checkpoint(&archive()).unwrap();
        let w = n.writer();
        assert_eq!(w.durable_count(), 1, "the redacted content blob (the wall)");
        // content_hash.txt + metadata.json + redaction_report.json (no full.jsonl).
        assert_eq!(w.structural_count(), 3);
        assert_eq!(w.shard_paths().len(), 1, "one shard grafted");
        assert!(w.branch_tip().is_some(), "the branch advanced");
    }

    #[test]
    fn native_archive_is_idempotent() {
        let n = NativeBackend::in_memory();
        n.archive_checkpoint(&archive()).unwrap();
        let tip1 = n.writer().branch_tip();
        // Re-archiving the identical checkpoint is a no-op (same shard path).
        n.archive_checkpoint(&archive()).unwrap();
        assert_eq!(n.writer().branch_tip(), tip1, "idempotent re-archive");
        assert_eq!(n.writer().shard_paths().len(), 1);
    }

    #[test]
    fn native_archive_metadata_drift_is_a_noop_keeping_the_first_anchor() {
        // The checkpoint id excludes the anchoring commit (trailer prediction),
        // so a later commit in the same session re-archives the SAME shard path
        // with only metadata.json differing. That must be an idempotent no-op —
        // the regression this closed: the second commit of a session once failed
        // with 'shard path already holds different content'.
        let n = NativeBackend::in_memory();
        n.archive_checkpoint(&archive()).unwrap();
        let tip1 = n.writer().branch_tip();
        let mut later = archive();
        later.commit = "def456".to_owned();
        later.created_at_ms = later.created_at_ms.map(|ms| ms + 60_000);
        n.archive_checkpoint(&later).unwrap();
        assert_eq!(n.writer().branch_tip(), tip1, "first anchor kept");
        assert_eq!(n.writer().shard_paths().len(), 1);
    }

    #[test]
    fn native_archive_divergent_capture_content_still_refuses() {
        // Same shard path with DIFFERENT capture entries stays an integrity
        // refusal — only metadata.json may drift.
        let n = NativeBackend::in_memory();
        n.archive_checkpoint(&archive()).unwrap();
        let mut diverged = archive();
        diverged.content = rift_shadow_redact::redact(&SecretBytes::new(b"other".to_vec()))
            .unwrap()
            .redacted;
        let err = n
            .archive_checkpoint(&diverged)
            .expect_err("divergent capture content at the same shard path");
        assert!(
            err.to_string().contains("different content"),
            "refusal names the conflict: {err}"
        );
    }

    /// **The positive form of the id-unbound-artifact defect, which this replaced by closing it.**
    ///
    /// It was planted here as an `expect_err`: `full.jsonl` was written when
    /// `events` was non-empty and `events` was NOT in the checkpoint id, so the
    /// id — and the shard path — did not move while the tree did, and
    /// `rift_git::ANCHOR_DEPENDENT_SHARD_ENTRIES` (`["metadata.json",
    /// "content_hash.txt"]`) made the second archive of a live session take a
    /// hard `Integrity` refusal. The only reason it had never fired is that
    /// `events` is empty at every production call site — which is exactly
    /// what the archived transcript changes.
    ///
    /// The fix was NOT to add names to that list. That list means "ignore this
    /// entry when comparing", so a transcript on it would let the FIRST one win
    /// and drop every later one in silence — a class an earlier commit was
    /// spent removing. The fix is that the id is derived from the shard's
    /// Merkle root,
    /// which commits to every content artifact: a changed artifact mints a NEW
    /// checkpoint, cleanly appended, exactly as a grown read-fact set already did.
    ///
    /// So this asserts the three things that must now all hold at once: the id
    /// MOVES when an artifact appears, the second archive SUCCEEDS, and both
    /// shards survive — because appending is the whole point of not colliding.
    #[test]
    fn an_id_bound_artifact_mints_a_new_shard_instead_of_refusing() {
        let n = NativeBackend::in_memory();
        let mut first = archive();
        first.checkpoint_id = first.derived_checkpoint_id().unwrap();
        n.archive_checkpoint(&first).unwrap();

        // The same session archiving again after a turn produced events.
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
        with_events.checkpoint_id = with_events.derived_checkpoint_id().unwrap();
        assert_ne!(
            with_events.checkpoint_id, first.checkpoint_id,
            "the artifact is bound, so the shard path moved with it"
        );
        n.archive_checkpoint(&with_events)
            .expect("a bound artifact appends a new shard rather than colliding");
        assert_eq!(
            n.writer().shard_paths().len(),
            2,
            "both checkpoints are on the branch — appended, not overwritten"
        );

        // And the refusal is still there for what it is FOR: the same id
        // presented with different bytes. That is a forged path, not a new turn.
        let mut forged = with_events.clone();
        forged.checkpoint_id = first.checkpoint_id.clone();
        let err = n
            .archive_checkpoint(&forged)
            .expect_err("a shard path holding different content must still refuse");
        assert!(
            err.to_string().contains("different content"),
            "the refusal names the conflict: {err}"
        );
    }

    /// **The transcript's signal test.** A transcript is a content artifact
    /// like any other: it moves the root, so it moves the id, so a session
    /// whose transcript changed mints a NEW shard that APPENDS — and none of
    /// that needed an era bump, which is precisely what binding the id to the
    /// root bought (the pre-root eras needed a new formula per artifact).
    #[test]
    fn a_transcript_artifact_is_bound_without_an_era_bump() {
        let n = NativeBackend::in_memory();
        let mut bare = archive();
        bare.checkpoint_id = bare.derived_checkpoint_id().unwrap();
        n.archive_checkpoint(&bare).unwrap();

        let mut with_transcript = archive();
        with_transcript.transcript = Some(transcript_of(
            br#"{"type":"user","message":{"role":"user","content":"hi"}}"#,
        ));
        with_transcript.checkpoint_id = with_transcript.derived_checkpoint_id().unwrap();
        assert_ne!(
            with_transcript.checkpoint_id, bare.checkpoint_id,
            "the transcript must move the id, or it sits outside the binding"
        );
        n.archive_checkpoint(&with_transcript).unwrap();
        assert_eq!(
            n.writer().shard_paths().len(),
            2,
            "a changed capture APPENDS a new shard; nothing is overwritten or refused"
        );
        // Both transcript artifacts are CAPTURE and take the durable path:
        // content.redacted ×2 shards + compact + full = 4 durable writes.
        assert_eq!(
            n.writer().durable_count(),
            4,
            "transcripts go through write_durable_blob, like every redacted capture"
        );
        assert_eq!(
            NATIVE_CANON_VERSION, 3,
            "and the era did NOT move: a new artifact extends the leaf spec, never the id rule"
        );
    }
}
