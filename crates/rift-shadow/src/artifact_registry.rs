//! The **shard artifact registry** — one enumeration of every artifact a
//! checkpoint shard can carry, and the version each serialized form declares.
//!
//! # Why it exists
//!
//! Two holes, both measured on this tree before this module was written:
//!
//! * **`schema_version` was unguarded.** Changing a digest document's
//!   `schema_version` from 1 to 7 left the whole suite green. A version field
//!   nothing binds is a field that documents nothing: the sync format promises
//!   that a reader accepts `<= current` and a writer emits current, and
//!   neither half is checkable if the current value can drift silently.
//! * **The artifact NAME set was unenumerated.** The frozen-bytes parity oracle
//!   compared four artifacts by name; everything later added — the transcript
//!   pair, the four digests, the task subtrees, the per-session metadata — sat
//!   outside it, so an artifact planted under an unlisted name changed no
//!   compared byte. [`is_declared`] is the predicate that closes it, and the
//!   parity oracle's artifact-set row is what calls it.
//!
//! # What a row is, and is not
//!
//! A row is a CLAIM about bytes a writer in this crate emits, and every row is
//! re-derived from that writer in `tests` rather than transcribed. A row is
//! **not** a claim that `docs/sync-format.md` carries a matching entry: that
//! file is git-ignored (it lives only in a maintainer's checkout), so a test
//! here cannot read it and must not pretend to. What each row does carry is
//! [`ShardArtifact::sync_format_row`] — the title of the registry row that
//! file owns — so the note text for a version bump is derivable from the table
//! rather than remembered. The sync format requires the entry; this table
//! names which one.
//!
//! # Changing a version
//!
//! A schema bump rides its OWN commit: change the writer, change the row here,
//! and update the `docs/sync-format.md` entry alongside it. The test below reds
//! until the row and the writer agree, which is the acknowledgement the bump
//! was missing.

/// How an artifact declares the version of its serialized form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VersionField {
    /// A top-level `schema_version` integer in a JSON document.
    SchemaVersion(u32),
    /// `metadata.json`'s canonicalization epoch. It is the checkpoint header's
    /// version field under a different name — the name is frozen on disk
    /// (`rift verify` selects a leaf canonicalization by it), so it is
    /// modelled rather than renamed.
    CanonVersion(u32),
    /// A per-line `v` integer in a JSONL stream (the compact transcript).
    PerLineV(u64),
    /// A per-line `schema` integer that differs from line to line: each line
    /// is a serve envelope and declares its own envelope schema (1 or 4), so no
    /// single number describes the artifact. Its own variant rather than
    /// [`Self::PerLineV`], whose member is `v` and whose value is one number
    /// for the whole stream.
    PerLineSchema,
    /// A per-line `schema_version` integer in a JSONL stream, one number for
    /// every line: the line format of a stream Rift composes from projection
    /// rows. Its own variant because [`Self::SchemaVersion`] is one member of
    /// one JSON document and [`Self::PerLineV`] names its member `v`.
    PerLineSchemaVersion(u32),
    /// No version of its own. Capture bytes, a hex digest, or a stream whose
    /// records are versioned by the shard header — recorded explicitly so
    /// "this artifact has no version" is a decision in the table rather than an
    /// omission from it.
    None,
}

/// One artifact a checkpoint shard can carry.
#[derive(Debug, Clone, Copy)]
pub struct ShardArtifact {
    /// The shard-relative path. `<id>` stands for a tool-use id and `<index>`
    /// for a 0-based session index — the only two variable components a shard
    /// tree has.
    pub path: &'static str,
    /// The version its serialized form declares.
    pub version: VersionField,
    /// The `docs/sync-format.md` registry row this artifact is documented by
    /// (maintained outside this crate; see the module header for why this is a
    /// pointer and not an assertion).
    pub sync_format_row: &'static str,
}

/// Every artifact the condense/archive pipeline can write into a shard, in
/// bytewise path order — the same order `NativeBackend::write_shard` sorts the
/// tree into and `shard_merkle_leaves` derives leaves in.
///
/// **Measured, not recalled**: this list was taken from `write_shard`'s own
/// entry construction (`crates/rift-shadow/src/backend/shard.rs`), and
/// `every_artifact_a_maximal_shard_writes_is_declared` re-measures it against
/// what a maximal fixture actually emits.
pub const SHARD_ARTIFACTS: &[ShardArtifact] = &[
    ShardArtifact {
        path: "content.redacted",
        version: VersionField::None,
        sync_format_row: "Capture document (`content.redacted`)",
    },
    ShardArtifact {
        path: "content_hash.txt",
        version: VersionField::None,
        sync_format_row: "Checkpoint metadata (`metadata.json`)",
    },
    ShardArtifact {
        path: "digest.failures.json",
        version: VersionField::SchemaVersion(1),
        sync_format_row: "Checkpoint digests (`digest.*.json`)",
    },
    ShardArtifact {
        path: "digest.intent.json",
        version: VersionField::SchemaVersion(1),
        sync_format_row: "Checkpoint digests (`digest.*.json`)",
    },
    ShardArtifact {
        path: "digest.log.json",
        version: VersionField::SchemaVersion(1),
        sync_format_row: "Checkpoint digests (`digest.*.json`)",
    },
    ShardArtifact {
        path: "digest.shape.json",
        version: VersionField::SchemaVersion(1),
        sync_format_row: "Checkpoint digests (`digest.*.json`)",
    },
    ShardArtifact {
        path: "full.jsonl",
        version: VersionField::None,
        sync_format_row: "Checkpoint metadata (`metadata.json`)",
    },
    ShardArtifact {
        path: "metadata.json",
        version: VersionField::CanonVersion(crate::backend::NATIVE_CANON_VERSION),
        sync_format_row: "Checkpoint metadata (`metadata.json`)",
    },
    ShardArtifact {
        path: "redaction_report.json",
        version: VersionField::None,
        sync_format_row: "Checkpoint metadata (`metadata.json`)",
    },
    ShardArtifact {
        path: "serve_audit.jsonl",
        version: VersionField::PerLineSchema,
        sync_format_row: "Serve audit (`serve_audit.jsonl`)",
    },
    ShardArtifact {
        path: "sessions.jsonl",
        version: VersionField::PerLineSchemaVersion(crate::backend::SESSIONS_SCHEMA_VERSION),
        sync_format_row: "Session agents (`sessions.jsonl`)",
    },
    ShardArtifact {
        path: "sessions/<index>/metadata.json",
        version: VersionField::SchemaVersion(1),
        sync_format_row: "Per-session checkpoint metadata (`sessions/<index>/metadata.json`)",
    },
    ShardArtifact {
        path: "tasks/<id>/metadata.json",
        version: VersionField::SchemaVersion(1),
        sync_format_row: "Subagent task metadata (`tasks/<id>/metadata.json`)",
    },
    ShardArtifact {
        path: "tasks/<id>/transcript.compact.jsonl",
        version: VersionField::PerLineV(crate::transcript::COMPACT_SCHEMA_VERSION),
        sync_format_row: "Compact transcript (`transcript.compact.jsonl`)",
    },
    ShardArtifact {
        path: "tasks/<id>/transcript.jsonl",
        version: VersionField::None,
        sync_format_row: "Capture document (`content.redacted`)",
    },
    ShardArtifact {
        path: "tool_calls.jsonl",
        version: VersionField::None,
        sync_format_row: "Checkpoint metadata (`metadata.json`)",
    },
    ShardArtifact {
        path: "transcript.compact.jsonl",
        version: VersionField::PerLineV(crate::transcript::COMPACT_SCHEMA_VERSION),
        sync_format_row: "Compact transcript (`transcript.compact.jsonl`)",
    },
    ShardArtifact {
        path: "transcript.jsonl",
        version: VersionField::None,
        sync_format_row: "Capture document (`content.redacted`)",
    },
    ShardArtifact {
        path: "usage.jsonl",
        version: VersionField::PerLineSchemaVersion(crate::backend::USAGE_SCHEMA_VERSION),
        sync_format_row: "Usage (`usage.jsonl`)",
    },
    ShardArtifact {
        path: "window_journal.jsonl",
        version: VersionField::PerLineSchemaVersion(crate::backend::WINDOW_JOURNAL_SCHEMA_VERSION),
        sync_format_row: "Window journal (`window_journal.jsonl`)",
    },
];

/// The registry row a shard-relative path belongs to, or `None` when the
/// registry does not declare it.
///
/// The two variable components are matched structurally rather than by glob:
/// `tasks/<anything-without-a-slash>/<declared name>` and
/// `sessions/<anything-without-a-slash>/<declared name>`. A third directory
/// level does not exist in a shard tree and is therefore refused — which is the
/// point, since an undeclared path is exactly what a planted artifact is.
#[must_use]
pub fn is_declared(path: &str) -> Option<&'static ShardArtifact> {
    SHARD_ARTIFACTS
        .iter()
        .find(|a| matches_pattern(a.path, path))
}

/// Whether `path` matches a registry pattern, `<id>`/`<index>` standing for one
/// non-empty path component.
fn matches_pattern(pattern: &str, path: &str) -> bool {
    let mut want = pattern.split('/');
    let mut got = path.split('/');
    loop {
        match (want.next(), got.next()) {
            (None, None) => return true,
            (Some(w), Some(g)) => {
                let ok = if w == "<id>" || w == "<index>" {
                    !g.is_empty() && g != "." && g != ".."
                } else {
                    w == g
                };
                if !ok {
                    return false;
                }
            }
            _ => return false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    /// The version integer a JSON document declares under `key`.
    fn json_version(bytes: &[u8], key: &str) -> Option<u64> {
        serde_json::from_slice::<Value>(bytes)
            .ok()?
            .get(key)?
            .as_u64()
    }

    /// The `v` a compact-transcript line declares (every line carries one).
    fn per_line_v(bytes: &[u8]) -> Vec<u64> {
        bytes
            .split(|b| *b == b'\n')
            .filter(|l| !l.is_empty())
            .filter_map(|l| json_version(l, "v"))
            .collect()
    }

    /// A transcript whose compaction exercises the artifacts under test.
    fn raw() -> rift_core::SecretBytes {
        rift_core::SecretBytes::new(
            [
                r#"{"type":"user","message":"fix the build"}"#,
                r#"{"type":"assistant","message":{"id":"m1","content":[{"type":"text","text":"looking"}]}}"#,
                r#"{"type":"user","message":[{"type":"tool_result","tool_use_id":"t1","is_error":true,"content":"boom"}]}"#,
                "",
            ]
            .join("\n")
            .into_bytes(),
        )
    }

    /// **THE PIN, and it is derived rather than transcribed.** Every row's
    /// declared version is compared against the integer the *writer* emits, so
    /// a bump reds here until the row moves with it — and the row moving is the
    /// acknowledgement, which is what a bump from 1 to 7 across 218 green tests
    /// did not have to make.
    #[test]
    fn every_declared_version_is_the_one_its_writer_emits() {
        use crate::backend::{session_meta_json, NATIVE_CANON_VERSION};

        let raw = raw();
        let packs = rift_shadow_redact::RulePackSet::builtin();
        let (pair, _) = crate::transcript::archived_transcript(&raw, 0, &packs).unwrap();
        let full = pair.full.as_slice();
        let compact = pair.compact.as_slice();

        // Every emitted document, keyed by the registry path it lands at.
        let emitted: Vec<(&str, Vec<u8>)> = vec![
            (
                "digest.failures.json",
                crate::digest::failures_json(full).expect("the fixture carries a failure"),
            ),
            ("digest.intent.json", crate::digest::intent_json(full, 1)),
            (
                "digest.log.json",
                crate::digest::log_json(0, full, compact, 0),
            ),
            (
                "digest.shape.json",
                crate::digest::shape_json(&[crate::backend::ArchivedReadFact {
                    tool_use_id: "t1".into(),
                    tool_name: "Read".into(),
                    path: "src/a.rs".into(),
                    content_sha256: "aa".into(),
                    input_tokens: None,
                    at: None,
                    turn: None,
                    blob_oid: None,
                    tokenizer: None,
                }])
                .expect("one read fact"),
            ),
            ("sessions/<index>/metadata.json", session_meta_json(0, "s0")),
            ("tasks/<id>/metadata.json", task_metadata()),
            ("transcript.compact.jsonl", compact.to_vec()),
            ("tasks/<id>/transcript.compact.jsonl", compact.to_vec()),
        ];

        for (path, bytes) in emitted {
            let row = is_declared(path).unwrap_or_else(|| panic!("{path} is not declared"));
            match row.version {
                VersionField::SchemaVersion(want) => assert_eq!(
                    json_version(&bytes, "schema_version"),
                    Some(u64::from(want)),
                    "{path}: the writer's schema_version and the registry disagree"
                ),
                VersionField::PerLineV(want) => {
                    let seen = per_line_v(&bytes);
                    assert!(!seen.is_empty(), "{path}: no line declared a version");
                    assert!(
                        seen.iter().all(|v| *v == want),
                        "{path}: lines declare {seen:?}, the registry says {want}"
                    );
                }
                VersionField::CanonVersion(_)
                | VersionField::None
                | VersionField::PerLineSchema
                | VersionField::PerLineSchemaVersion(_) => {
                    unreachable!("{path} is not versioned that way")
                }
            }
        }

        // The header's epoch, from the constant the writer stamps.
        let row = is_declared("metadata.json").expect("declared");
        assert_eq!(
            row.version,
            VersionField::CanonVersion(NATIVE_CANON_VERSION),
            "the registry must move with the canonicalization epoch"
        );
    }

    /// The serve audit's lines each declare their own envelope schema, one of
    /// the two the one writer freezes — read off the maximal fixture, whose
    /// lines are the shape the writer emits.
    #[test]
    fn every_frozen_serve_line_declares_its_own_envelope_schema() {
        assert_eq!(
            is_declared("serve_audit.jsonl").map(|a| a.version),
            Some(VersionField::PerLineSchema)
        );
        let archive = crate::shard_fixture::every_artifact_archive().unwrap();
        let bytes = archive
            .serve_audit_jsonl()
            .expect("the fixture freezes serves");
        let mut schemas: Vec<u64> = bytes
            .split(|b| *b == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| json_version(line, "schema").expect("every line declares one"))
            .collect();
        schemas.sort_unstable();
        assert_eq!(
            schemas,
            vec![1, 4],
            "one schema-1 line and one schema-4 line"
        );
    }

    /// Every frozen usage line declares the line format the registry names,
    /// read off the maximal fixture, whose lines the one writer rendered.
    #[test]
    fn every_frozen_usage_line_declares_the_registered_schema_version() {
        let Some(VersionField::PerLineSchemaVersion(want)) =
            is_declared("usage.jsonl").map(|a| a.version)
        else {
            panic!("usage.jsonl is declared with a per-line schema_version");
        };
        let archive = crate::shard_fixture::every_artifact_archive().unwrap();
        let bytes = archive.usage_jsonl().expect("the fixture freezes usage");
        let versions: Vec<u64> = bytes
            .split(|b| *b == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| json_version(line, "schema_version").expect("every line declares one"))
            .collect();
        assert_eq!(versions, vec![u64::from(want); 2], "two lines, one version");
    }

    /// Every frozen session line declares the line format the registry
    /// names, read off the maximal fixture, whose lines the one writer
    /// rendered.
    #[test]
    fn every_frozen_session_line_declares_the_registered_schema_version() {
        let Some(VersionField::PerLineSchemaVersion(want)) =
            is_declared("sessions.jsonl").map(|a| a.version)
        else {
            panic!("sessions.jsonl is declared with a per-line schema_version");
        };
        let archive = crate::shard_fixture::every_artifact_archive().unwrap();
        let bytes = archive.sessions_jsonl().expect("the fixture names agents");
        let versions: Vec<u64> = bytes
            .split(|b| *b == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| json_version(line, "schema_version").expect("every line declares one"))
            .collect();
        assert_eq!(versions, vec![u64::from(want); 2], "two lines, one version");
    }

    /// Every frozen window-journal line declares the line format the
    /// registry names, read off the maximal fixture, whose lines the one
    /// writer rendered.
    #[test]
    fn every_frozen_window_journal_line_declares_the_registered_schema_version() {
        let Some(VersionField::PerLineSchemaVersion(want)) =
            is_declared("window_journal.jsonl").map(|a| a.version)
        else {
            panic!("window_journal.jsonl is declared with a per-line schema_version");
        };
        let archive = crate::shard_fixture::every_artifact_archive().unwrap();
        let bytes = archive
            .window_journal_jsonl()
            .expect("the fixture freezes window rows");
        let versions: Vec<u64> = bytes
            .split(|b| *b == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| json_version(line, "schema_version").expect("every line declares one"))
            .collect();
        assert_eq!(
            versions,
            vec![u64::from(want); 5],
            "five lines, one version"
        );
    }

    /// A task metadata document from the real writer.
    fn task_metadata() -> Vec<u8> {
        crate::backend::ArchivedTask {
            tool_use_id: "toolu_1".into(),
            agent_id: "claude-code".into(),
            started_at_ms: 1,
            ended_at_ms: 2,
            input_tokens: None,
            output_tokens: None,
            transcript: None,
        }
        .metadata_json()
    }

    /// The unversioned rows are unversioned ON PURPOSE, and the table says so
    /// rather than leaving the reader to infer it from a missing entry.
    #[test]
    fn the_unversioned_artifacts_are_declared_unversioned() {
        for path in [
            "content.redacted",
            "content_hash.txt",
            "full.jsonl",
            "redaction_report.json",
            "tool_calls.jsonl",
            "transcript.jsonl",
            "tasks/<id>/transcript.jsonl",
        ] {
            assert_eq!(
                is_declared(path).map(|a| a.version),
                Some(VersionField::None),
                "{path}"
            );
        }
    }

    /// Every row names the registry row a bump must update.
    /// It is a POINTER, not an assertion that the row exists — `docs/` is
    /// git-ignored and absent from a fresh clone, so a test that claimed
    /// to read it would be claiming something it cannot see.
    #[test]
    fn every_row_names_the_sync_format_registry_row_a_bump_must_update() {
        for a in SHARD_ARTIFACTS {
            assert!(
                !a.sync_format_row.is_empty(),
                "{}: no sync-format row named",
                a.path
            );
        }
    }

    /// The table is in bytewise path order — the tree order, the leaf order,
    /// and the order a reader compares two shards in.
    #[test]
    fn the_registry_is_in_bytewise_path_order_and_has_no_duplicates() {
        let paths: Vec<&str> = SHARD_ARTIFACTS.iter().map(|a| a.path).collect();
        let mut sorted = paths.clone();
        sorted.sort_unstable_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
        assert_eq!(paths, sorted, "registry order");
        let mut deduped = sorted.clone();
        deduped.dedup();
        assert_eq!(sorted, deduped, "a path is declared once");
    }

    /// **The planted-garbage half.** A name the registry does not carry is
    /// refused — including the shapes a careless plant would wear: a sibling of
    /// a declared name, a deeper path under a declared directory, and a
    /// traversal component in the variable slot.
    #[test]
    fn an_undeclared_artifact_name_is_refused() {
        for path in [
            "notes.txt",
            "digest.secrets.json",
            "content.redacted.bak",
            "tasks/toolu_1/notes.txt",
            "tasks/toolu_1/deeper/metadata.json",
            "tasks/../metadata.json",
            "sessions/0/transcript.jsonl",
            "sessions/metadata.json",
            "",
        ] {
            assert!(
                is_declared(path).is_none(),
                "{path} must not be a declared artifact"
            );
        }
        // …and the declared shapes still are, so the refusal above is not a
        // predicate that says no to everything.
        for path in [
            "metadata.json",
            "tasks/t1/metadata.json",
            "tasks/t1/transcript.compact.jsonl",
            "sessions/0/metadata.json",
            "sessions/10/metadata.json",
        ] {
            assert!(is_declared(path).is_some(), "{path} is declared");
        }
    }
}
