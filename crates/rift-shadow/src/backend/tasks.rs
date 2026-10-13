//! Where a sealed subagent transcript came from: the provenance members a
//! `tasks/<tool-use-id>/metadata.json` carries beside its identifiers.
//!
//! Separate from [`ArchivedTask`](super::ArchivedTask) because a task built
//! before subagent transcripts were collected has none of it, and its metadata
//! must stay byte-identical: every member here is written only when the
//! archive collected the task from the agent's own files.
//!
//! The size and digest are derived here from the sealed `transcript.jsonl`
//! bytes rather than handed in, so the metadata cannot record a hash of bytes
//! the shard does not hold, and `rift verify` can recompute both from the leaf
//! beside it.

use super::ArchivedTranscript;

/// How a task's transcript reached the archive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskTranscriptState {
    /// Read and sealed: the subtree carries the transcript pair.
    Sealed,
    /// The file the agent named, or the listing found, was gone at archive.
    Missing,
    /// The file was there and could not be read.
    Unreadable,
    /// The file was read and the redaction gate refused to seal it.
    Unsealed,
}

impl TaskTranscriptState {
    /// The closed word `metadata.json` records, `None` for a sealed transcript
    /// (whose presence is the subtree's own `transcript.jsonl`).
    #[must_use]
    pub fn word(self) -> Option<&'static str> {
        match self {
            Self::Sealed => None,
            Self::Missing => Some("missing"),
            Self::Unreadable => Some("unreadable"),
            Self::Unsealed => Some("unsealed"),
        }
    }
}

/// The member a subagent ran in, from the worktree observation its own hook
/// recorded — the same three facts `session_worktrees.jsonl` freezes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskWorktree {
    /// `RepoPaths::worktree_token()` — 12 lowercase hex.
    pub token: String,
    /// The member's administrative name, or `main`.
    pub name: String,
    /// Whether the member is a linked worktree.
    pub linked: bool,
}

/// The provenance of one collected subagent transcript.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskProvenance {
    /// The dispatched subagent type, from the spawning tool call or, failing
    /// that, from the agent's own stop hook.
    pub subagent_type: Option<String>,
    /// The native id of the session that spawned the subagent.
    pub parent_session: String,
    /// Where the subagent ran, when one of its hooks said.
    pub worktree: Option<TaskWorktree>,
    /// Whether a spawning tool call was found for the agent id. An unmatched
    /// task is named by its agent id instead of a tool-use id.
    pub spawn_matched: bool,
    /// How the transcript reached the archive.
    pub transcript: TaskTranscriptState,
}

impl TaskProvenance {
    /// Add the provenance members to a task's metadata document.
    pub(super) fn insert(
        &self,
        obj: &mut serde_json::Map<String, serde_json::Value>,
        transcript: Option<&ArchivedTranscript>,
    ) {
        if let Some(kind) = &self.subagent_type {
            obj.insert("subagent_type".into(), kind.clone().into());
        }
        obj.insert("parent_session".into(), self.parent_session.clone().into());
        if let Some(w) = &self.worktree {
            obj.insert(
                "worktree".into(),
                serde_json::json!({ "linked": w.linked, "name": w.name, "token": w.token }),
            );
        }
        let spawn = if self.spawn_matched {
            "matched"
        } else {
            "unmatched"
        };
        obj.insert("spawn".into(), spawn.into());
        if let Some(word) = self.transcript.word() {
            obj.insert("task_transcript".into(), word.into());
        }
        if let Some(t) = transcript {
            let full = t.full.as_slice();
            obj.insert("bytes".into(), full.len().into());
            obj.insert("sha256".into(), crate::sha256_hex(&[full]).into());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::{task, transcript_of};
    use super::*;

    fn provenance(transcript: TaskTranscriptState) -> TaskProvenance {
        TaskProvenance {
            subagent_type: Some("Explore".into()),
            parent_session: "s-1".into(),
            worktree: Some(TaskWorktree {
                token: "0123456789ab".into(),
                name: "wt".into(),
                linked: true,
            }),
            spawn_matched: false,
            transcript,
        }
    }

    fn meta(t: &super::super::ArchivedTask) -> serde_json::Value {
        serde_json::from_slice(&t.metadata_json()).unwrap()
    }

    /// A collected task records its provenance and the size and digest of
    /// exactly the transcript bytes it seals; a task without provenance keeps
    /// the metadata it always had.
    #[test]
    fn a_collected_task_records_the_digest_of_the_bytes_it_seals() {
        let bare = meta(&task("toolu_1"));
        assert!(bare.get("parent_session").is_none() && bare.get("sha256").is_none());

        let mut sealed = task("toolu_1");
        sealed.transcript = Some(transcript_of(b"{\"a\":1}\n"));
        sealed.provenance = Some(provenance(TaskTranscriptState::Sealed));
        let full = sealed.transcript.as_ref().unwrap().full.as_slice().to_vec();
        let m = meta(&sealed);
        assert_eq!(m["sha256"], crate::sha256_hex(&[&full]));
        assert_eq!(m["bytes"], full.len());
        assert_eq!(m["spawn"], "unmatched");
        assert_eq!(m["subagent_type"], "Explore");
        assert_eq!(m["worktree"]["linked"], true);
        assert!(m.get("task_transcript").is_none());
    }

    /// Every way a transcript can fail to reach the shard has its own closed
    /// word, and none of them records a digest.
    #[test]
    fn a_transcript_that_did_not_reach_the_shard_is_named() {
        for (state, word) in [
            (TaskTranscriptState::Missing, "missing"),
            (TaskTranscriptState::Unreadable, "unreadable"),
            (TaskTranscriptState::Unsealed, "unsealed"),
        ] {
            let mut lost = task("toolu_1");
            let mut p = provenance(state);
            p.spawn_matched = true;
            p.worktree = None;
            p.subagent_type = None;
            lost.provenance = Some(p);
            let m = meta(&lost);
            assert_eq!(m["task_transcript"], word);
            assert_eq!(m["spawn"], "matched");
            assert!(m.get("sha256").is_none() && m.get("worktree").is_none());
        }
        assert_eq!(TaskTranscriptState::Sealed.word(), None);
    }
}
