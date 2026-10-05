//! The Merkle leaf specification: the one input type both the writer and
//! `rift verify` build, and the leaf/root derivation over it. Separate from
//! the archive so the derivation reads as a spec over bytes, with no archive
//! type in sight — the verifier feeds it bytes read back out of git.

use rift_core::Sha256Id;

/// The shard bytes a checkpoint's Merkle leaves are derived from.
///
/// **The one input type both sides build.** The writer fills it with the bytes
/// it is about to commit; `rift verify` fills it with the bytes it reads back
/// out of the shard. The fields are named rather than positional precisely so
/// the two sides cannot quietly feed the same function a different set in a
/// different order — the failure this program has now paid for three times is
/// two writers of one fact, not a wrong hash function.
#[derive(Debug, Clone, Copy, Default)]
pub struct ShardLeafBytes<'a> {
    /// `content.redacted` — the redacted capture. Always present; may be empty
    /// (an empty capture still contributes a leaf, and `leaf_hash(&[])` is a
    /// real digest, not the empty-tree root).
    pub content_redacted: &'a [u8],
    /// `full.jsonl` — the archived event stream, or `None` when the shard is
    /// header-only.
    pub full_jsonl: Option<&'a [u8]>,
    /// `redaction_report.json` — the counts-only redaction facts. Always present.
    pub redaction_report: &'a [u8],
    /// `serve_audit.jsonl` — the session's frozen serve envelopes, or `None`
    /// when it froze none.
    pub serve_audit: Option<&'a [u8]>,
    /// `sessions.jsonl` — each archived session's agent, or `None` on a shard
    /// written without it.
    pub sessions: Option<&'a [u8]>,
    /// `tool_calls.jsonl` — the archived read facts, or `None` when the session
    /// captured none.
    pub tool_calls: Option<&'a [u8]>,
    /// `transcript.compact.jsonl` — the redacted deterministic compaction, or
    /// `None` when no transcript reached the archive.
    pub transcript_compact: Option<&'a [u8]>,
    /// `transcript.jsonl` — the redacted full transcript, or `None` alongside
    /// [`Self::transcript_compact`]: the two travel together.
    pub transcript_full: Option<&'a [u8]>,
    /// `usage.jsonl` — the session's settled model calls, or `None` when it
    /// settled none.
    pub usage: Option<&'a [u8]>,
    /// The `tasks/<tool-use-id>/` subtrees' artifact bytes, or empty when the
    /// checkpoint carries no subagent task. Order here is
    /// irrelevant: [`shard_merkle_leaves`] orders by tool-use id itself, so
    /// the writer and the verifier cannot feed one function two orders.
    pub tasks: &'a [TaskLeafBytes<'a>],
    /// The `sessions/<index>/metadata.json` bytes of a **multi-session**
    /// checkpoint, or empty for the single-session shape —
    /// a single-session shard writes NO session dirs, so every existing
    /// shard's leaf sequence is untouched. Order here is irrelevant:
    /// [`shard_merkle_leaves`] orders by the decimal index name itself.
    pub session_metas: &'a [SessionMetaLeaf<'a>],
    /// The condense-time digest artifacts, each `None`
    /// when the shard does not carry it — absence is the compatibility
    /// contract, as everywhere in this struct.
    pub digests: DigestLeafBytes<'a>,
}

/// One per-session metadata document of a multi-session checkpoint — the
/// `sessions/<index>/metadata.json` slice of [`ShardLeafBytes`].
#[derive(Debug, Clone, Copy)]
pub struct SessionMetaLeaf<'a> {
    /// The decimal index-dir name (`"0"`, `"1"`, …) — the leaf *order* key
    /// (bytewise, matching git's tree order: `"10"` sorts before `"2"`).
    pub index_name: &'a str,
    /// `sessions/<index>/metadata.json` ([`session_meta_json`](super::session_meta_json)).
    pub metadata: &'a [u8],
}

/// The four digest artifacts' bytes — a sub-struct so a
/// pre-digest call site names one field, not four. Leaf order: the `digest.*`
/// names sort between `content_hash.txt` (not a leaf) and `full.jsonl`, so
/// these are the leaves immediately after `content.redacted`, in the field
/// order below (`failures` < `intent` < `log` < `shape`, their bytewise
/// name order).
#[derive(Debug, Clone, Copy, Default)]
pub struct DigestLeafBytes<'a> {
    /// `digest.failures.json` — mechanical failure signatures.
    pub failures: Option<&'a [u8]>,
    /// `digest.intent.json` — the model-free intent/outcome slot.
    pub intent: Option<&'a [u8]>,
    /// `digest.log.json` — the log-shape digest.
    pub log: Option<&'a [u8]>,
    /// `digest.shape.json` — the file-shape digest.
    pub shape: Option<&'a [u8]>,
}

/// One task subtree's leaf bytes — the `tasks/<tool-use-id>/` slice of
/// [`ShardLeafBytes`], built by the writer from what it is about to commit and
/// by `rift verify` from what it read back (task ids come from the parent
/// `metadata.json`'s `tasks` list; a listed-but-missing artifact reads as
/// empty bytes, which cannot reproduce the writer's leaf, so the root — and
/// therefore the id — refuses for free).
#[derive(Debug, Clone, Copy)]
pub struct TaskLeafBytes<'a> {
    /// The subtree name — the leaf *order* key, never leaf content.
    pub tool_use_id: &'a str,
    /// `tasks/<id>/metadata.json` — always present ([`ArchivedTask::metadata_json`](super::ArchivedTask::metadata_json)).
    pub metadata: &'a [u8],
    /// `tasks/<id>/transcript.compact.jsonl`, when the task carries its pair.
    pub transcript_compact: Option<&'a [u8]>,
    /// `tasks/<id>/transcript.jsonl`, alongside the compact half.
    pub transcript_full: Option<&'a [u8]>,
}

/// A checkpoint's ordered Merkle leaves: **the shard's content artifacts in
/// canonical (git-tree) name order, with `full.jsonl` expanded to one leaf per
/// archived event line.**
///
/// So the sequence is `content.redacted`, then the `digest.*` documents, then
/// each `full.jsonl` line, then `redaction_report.json`, then
/// `serve_audit.jsonl`, then `sessions.jsonl`, then each
/// `sessions/<index>/metadata.json`, then each
/// `tasks/<tool-use-id>/` subtree's artifacts (`metadata.json`, then the
/// transcript pair when present — the task dir's own bytewise name order —
/// tasks ordered by tool-use id), then `tool_calls.jsonl`, then
/// `transcript.compact.jsonl`, then `transcript.jsonl`, then `usage.jsonl` —
/// which is the artifacts' bytewise **full-path** order (`serve_audit.jsonl`
/// sits between `redaction_report.json` and `sessions/` because `r` < `s` at
/// the first byte and again at the third; `tasks/…` sits between `sessions/`
/// and `tool_calls.jsonl` because `'t','a' < 't','o'`; `usage.jsonl` trails
/// because `t` < `u`, so it is appended to every sequence rather than
/// inserted into one), the same order
/// `NativeBackend::write_shard` sorts the tree into. One rule, and it makes
/// "the checkpoint's event leaves" literally true the moment an event stream is
/// threaded through condense: today a shard carries no events and the leaf set
/// is its other artifacts; tomorrow the events are simply more leaves in the
/// same sequence.
///
/// **New artifacts are optional leaves, and ABSENCE is the compatibility
/// contract** — a shard that does not carry the artifact derives, leaf for
/// leaf, the sequence it derived before the artifact existed, so its root and
/// its root-bound id never move. That is why neither the transcript pair (a
/// trailing pair) nor the task subtrees (a MIDDLE insertion — the honest
/// statement, since `tasks/` does not sort last) needed an era bump or a new
/// id rule: the sequence only ever changes for shards that actually carry the
/// new artifact, and those are new shards by construction. What a future
/// artifact's name must still respect is the one rule above — leaves sit in
/// bytewise full-path order, so pick the name knowing where it lands.
///
/// **Two artifacts are deliberately excluded.** `metadata.json` carries the root
/// itself, so a root over it is not computable; `content_hash.txt` is a digest
/// over a SEQUENCE that includes `metadata.json` — see
/// [`CheckpointArchive::content_hash_txt`](super::CheckpointArchive::content_hash_txt), which digests
/// `metadata.json ‖ redaction_report.json ‖ full.jsonl` (the last only when the
/// shard carries one) — and inherits that circularity. The single-artifact
/// reading of that sentence was false, and was inherited by a published format
/// registry before anyone measured it: `sha256(metadata.json)` alone matches
/// **zero** live shards. `content_hash_covers_metadata_report_and_full_in_that_order`
/// pins the sequence by arithmetic so the two descriptions cannot drift again.
///
/// Blank segments are dropped, so a `full.jsonl` written with a trailing newline
/// and one written without produce the same leaves.
///
/// Order is part of the commitment (see [`rift_hashchain::merkle_root`]), so two
/// artifacts swapping bytes changes the root even though the leaf multiset does
/// not. That is an argument about this construction, not a guarantee about an
/// adversary: what is *tested* is that changing any single artifact's bytes
/// changes the root.
#[must_use]
pub fn shard_merkle_leaves(bytes: &ShardLeafBytes<'_>) -> Vec<Sha256Id> {
    let mut leaves = vec![rift_hashchain::leaf_hash(bytes.content_redacted)];
    // The digest artifacts: `digest.*` sorts after
    // `content_hash.txt` (excluded) and before `full.jsonl`, so their leaves
    // come here, in bytewise name order — failures, intent, log, shape.
    for digest in [
        bytes.digests.failures,
        bytes.digests.intent,
        bytes.digests.log,
        bytes.digests.shape,
    ]
    .into_iter()
    .flatten()
    {
        leaves.push(rift_hashchain::leaf_hash(digest));
    }
    if let Some(full) = bytes.full_jsonl {
        leaves.extend(
            full.split(|b| *b == b'\n')
                .filter(|line| !line.is_empty())
                .map(rift_hashchain::leaf_hash),
        );
    }
    leaves.push(rift_hashchain::leaf_hash(bytes.redaction_report));
    // The frozen serve envelopes: `serve_audit.jsonl` sorts after
    // `redaction_report.json` and before `sessions/`, so one leaf here.
    if let Some(serve_audit) = bytes.serve_audit {
        leaves.push(rift_hashchain::leaf_hash(serve_audit));
    }
    // The sessions' agents: `sessions.jsonl` sorts after `serve_audit.jsonl`
    // (`r` < `s` at the third byte) and before `sessions/` (`.` < `/`), so one
    // leaf here, ahead of the per-session metadata.
    if let Some(sessions) = bytes.sessions {
        leaves.push(rift_hashchain::leaf_hash(sessions));
    }
    // The per-session metadata of a multi-session checkpoint, in bytewise
    // index-name order (`sessions/` sorts between `redaction_report.json`
    // and `tasks/`; within it git orders decimal names bytewise — "10"
    // before "2" — and the leaf order matches the tree, not the index).
    let mut session_metas: Vec<&SessionMetaLeaf<'_>> = bytes.session_metas.iter().collect();
    session_metas.sort_by(|a, b| a.index_name.as_bytes().cmp(b.index_name.as_bytes()));
    for m in session_metas {
        leaves.push(rift_hashchain::leaf_hash(m.metadata));
    }
    // The task subtrees, in bytewise tool-use-id order — ordered HERE, so the
    // writer and the verifier cannot hand one function two orders. The order
    // key is not itself a leaf, but it is not unbound either: every task's
    // metadata.json (a leaf) carries its own `tool_use_id`, so a renamed
    // subtree is a name/bytes disagreement `rift verify` flags rather than a
    // byte change the root would miss.
    let mut tasks: Vec<&TaskLeafBytes<'_>> = bytes.tasks.iter().collect();
    tasks.sort_by(|a, b| a.tool_use_id.as_bytes().cmp(b.tool_use_id.as_bytes()));
    for t in tasks {
        leaves.push(rift_hashchain::leaf_hash(t.metadata));
        if let Some(compact) = t.transcript_compact {
            leaves.push(rift_hashchain::leaf_hash(compact));
        }
        if let Some(full_transcript) = t.transcript_full {
            leaves.push(rift_hashchain::leaf_hash(full_transcript));
        }
    }
    if let Some(tool_calls) = bytes.tool_calls {
        leaves.push(rift_hashchain::leaf_hash(tool_calls));
    }
    if let Some(compact) = bytes.transcript_compact {
        leaves.push(rift_hashchain::leaf_hash(compact));
    }
    if let Some(full_transcript) = bytes.transcript_full {
        leaves.push(rift_hashchain::leaf_hash(full_transcript));
    }
    // The settled usage: `usage.jsonl` sorts after every other top-level
    // name, so its one leaf is the last.
    if let Some(usage) = bytes.usage {
        leaves.push(rift_hashchain::leaf_hash(usage));
    }
    leaves
}

/// The Merkle root over [`shard_merkle_leaves`] — the value a
/// `canon_version >= `[`STRICT_MERKLE_MIN_CANON_VERSION`](super::STRICT_MERKLE_MIN_CANON_VERSION) shard records in
/// `metadata.json` and `rift verify` re-derives.
#[must_use]
pub fn shard_merkle_root(bytes: &ShardLeafBytes<'_>) -> Sha256Id {
    rift_hashchain::merkle_root(&shard_merkle_leaves(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The compat twin, stated the honest way.** `tasks/` sorts BETWEEN
    /// `redaction_report.json` and `tool_calls.jsonl` (`'t','a' < 't','o'`),
    /// so the compatibility argument is NOT "new leaves trail" — it is that a
    /// taskless shard's leaf sequence is UNCHANGED, leaf for leaf, and a
    /// task-carrying one inserts its leaves at exactly the position the name
    /// order dictates. Both halves asserted against hand-built sequences.
    #[test]
    fn a_taskless_shard_derives_the_same_root_it_did_before_tasks_existed() {
        use rift_hashchain::leaf_hash;

        let (content, report, tool_calls) = (b"c".as_slice(), b"r".as_slice(), b"t".as_slice());
        let taskless = ShardLeafBytes {
            content_redacted: content,
            redaction_report: report,
            tool_calls: Some(tool_calls),
            ..ShardLeafBytes::default()
        };
        // The pre-task sequence, built by hand: content, report, tool_calls.
        assert_eq!(
            shard_merkle_leaves(&taskless),
            vec![leaf_hash(content), leaf_hash(report), leaf_hash(tool_calls)],
            "a taskless shard's leaf sequence is unchanged"
        );

        // Two tasks, deliberately handed over UNSORTED: their leaves land
        // between the report and the read facts, in bytewise id order.
        let (meta_a, meta_b) = (b"task a meta".as_slice(), b"task b meta".as_slice());
        let with_tasks = ShardLeafBytes {
            tasks: &[
                TaskLeafBytes {
                    tool_use_id: "toolu_b",
                    metadata: meta_b,
                    transcript_compact: None,
                    transcript_full: Some(b"task b transcript"),
                },
                TaskLeafBytes {
                    tool_use_id: "toolu_a",
                    metadata: meta_a,
                    transcript_compact: None,
                    transcript_full: None,
                },
            ],
            ..taskless
        };
        assert_eq!(
            shard_merkle_leaves(&with_tasks),
            vec![
                leaf_hash(content),
                leaf_hash(report),
                leaf_hash(meta_a),
                leaf_hash(meta_b),
                leaf_hash(b"task b transcript"),
                leaf_hash(tool_calls),
            ],
            "task leaves insert between redaction_report.json and tool_calls.jsonl, id-sorted"
        );
    }

    /// **The transcript's signal test — the prefix claim.** A shard carrying no
    /// transcript derives the identical root it derived before the transcript
    /// pair existed, because the pair's names sort after every earlier
    /// artifact and absent artifacts contribute no leaf. The committed parity
    /// anchors assert the same fact against frozen bytes; this asserts the
    /// structural reason.
    #[test]
    fn a_shard_without_a_transcript_derives_the_same_root_it_did_before() {
        let report = br#"{"x":1}"#.as_slice();
        let without = ShardLeafBytes {
            content_redacted: b"capture",
            full_jsonl: Some(br#"{"e":0}"#),
            redaction_report: report,
            serve_audit: None,
            sessions: None,
            usage: None,
            tool_calls: Some(br#"{"id":"t1"}"#),
            transcript_compact: None,
            transcript_full: None,
            tasks: &[],
            session_metas: &[],
            digests: DigestLeafBytes::default(),
        };
        let with = ShardLeafBytes {
            transcript_compact: Some(b"compact line"),
            transcript_full: Some(b"full line"),
            ..without
        };
        let base = shard_merkle_leaves(&without);
        let grown = shard_merkle_leaves(&with);
        assert_eq!(
            grown.get(..base.len()),
            Some(base.as_slice()),
            "a transcript-less leaf sequence must be a strict PREFIX of a transcript-carrying one"
        );
        assert_eq!(
            grown.len(),
            base.len() + 2,
            "one leaf per transcript artifact"
        );
        assert_ne!(
            shard_merkle_root(&without),
            shard_merkle_root(&with),
            "…and the added leaves genuinely move the root"
        );
    }

    /// **The serve audit's place in the sequence, and its absence contract.**
    /// A shard without `serve_audit.jsonl` derives, leaf for leaf, what it
    /// derived before the artifact existed; one that carries it gains exactly
    /// one leaf, after `redaction_report.json` and before the `sessions/`
    /// metadata, which is the name order the tree is written in.
    #[test]
    fn a_serve_audit_leaf_sits_between_the_report_and_the_sessions_and_absence_changes_nothing() {
        use rift_hashchain::leaf_hash;

        let (content, report, tool_calls) = (b"c".as_slice(), b"r".as_slice(), b"t".as_slice());
        let sessions = [SessionMetaLeaf {
            index_name: "0",
            metadata: b"session zero",
        }];
        let without = ShardLeafBytes {
            content_redacted: content,
            redaction_report: report,
            tool_calls: Some(tool_calls),
            session_metas: &sessions,
            ..ShardLeafBytes::default()
        };
        assert_eq!(
            shard_merkle_leaves(&without),
            vec![
                leaf_hash(content),
                leaf_hash(report),
                leaf_hash(b"session zero"),
                leaf_hash(tool_calls),
            ],
            "a shard without the artifact keeps its old sequence"
        );
        let with = ShardLeafBytes {
            serve_audit: Some(b"serve line"),
            ..without
        };
        assert_eq!(
            shard_merkle_leaves(&with),
            vec![
                leaf_hash(content),
                leaf_hash(report),
                leaf_hash(b"serve line"),
                leaf_hash(b"session zero"),
                leaf_hash(tool_calls),
            ],
            "the serve audit's one leaf sits after the report and before sessions/"
        );
    }

    /// **The usage leaf trails, and its absence changes nothing.** A shard
    /// without `usage.jsonl` derives the sequence it derived before the
    /// artifact existed; one that carries it gains exactly one leaf, after the
    /// transcript pair, because `usage.jsonl` sorts after every other name.
    #[test]
    fn a_usage_leaf_trails_the_sequence_and_absence_changes_nothing() {
        use rift_hashchain::leaf_hash;

        let (content, report, compact, full) = (
            b"c".as_slice(),
            b"r".as_slice(),
            b"compact".as_slice(),
            b"full".as_slice(),
        );
        let without = ShardLeafBytes {
            content_redacted: content,
            redaction_report: report,
            transcript_compact: Some(compact),
            transcript_full: Some(full),
            ..ShardLeafBytes::default()
        };
        let old = vec![
            leaf_hash(content),
            leaf_hash(report),
            leaf_hash(compact),
            leaf_hash(full),
        ];
        assert_eq!(
            shard_merkle_leaves(&without),
            old,
            "no artifact, old sequence"
        );
        let with = ShardLeafBytes {
            usage: Some(b"usage line"),
            ..without
        };
        let mut appended = old;
        appended.push(leaf_hash(b"usage line"));
        assert_eq!(shard_merkle_leaves(&with), appended, "one trailing leaf");
    }

    /// **The sessions leaf sits between the serve audit and the per-session
    /// metadata, and its absence changes nothing.** `sessions.jsonl` sorts
    /// after `serve_audit.jsonl` and before the `sessions/` tree (`.` < `/`).
    #[test]
    fn a_sessions_leaf_sits_before_the_session_dirs_and_absence_changes_nothing() {
        use rift_hashchain::leaf_hash;

        let metas = [SessionMetaLeaf {
            index_name: "0",
            metadata: b"meta",
        }];
        let without = ShardLeafBytes {
            content_redacted: b"c",
            redaction_report: b"r",
            serve_audit: Some(b"serves"),
            session_metas: &metas,
            ..ShardLeafBytes::default()
        };
        let old = vec![
            leaf_hash(b"c"),
            leaf_hash(b"r"),
            leaf_hash(b"serves"),
            leaf_hash(b"meta"),
        ];
        assert_eq!(
            shard_merkle_leaves(&without),
            old,
            "no artifact, old sequence"
        );
        let with = ShardLeafBytes {
            sessions: Some(b"agents"),
            ..without
        };
        let mut inserted = old;
        inserted.insert(3, leaf_hash(b"agents"));
        assert_eq!(shard_merkle_leaves(&with), inserted);
    }
}
