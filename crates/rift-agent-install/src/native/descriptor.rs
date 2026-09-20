//! Rift's project-local capture descriptor: where it lives, the bytes Rift
//! writes, and the three shapes that prove a file is Rift's own without a
//! manifest entry to vouch for it.

use std::path::{Path, PathBuf};

use rift_shadow_hooks::AgentAdapter;

use super::manifest::{fingerprint, OwnershipManifest};

pub(super) fn descriptor_path(repo: &Path, token: &str) -> PathBuf {
    repo.join(".rift/agents").join(format!("{token}.json"))
}

pub(super) fn descriptor_bytes(
    rift_bin: &Path,
    token: &str,
    verbs: &[&str],
    notes: &rift_shadow_hooks::CompatNotes,
) -> Vec<u8> {
    let hooks: Vec<serde_json::Value> = verbs
        .iter()
        .map(|verb| {
            serde_json::json!({
                "verb": verb,
                "command": format!("{} hooks {token} {verb}", rift_bin.display()),
            })
        })
        .collect();
    let value = serde_json::json!({
        "schema_version": 1,
        "managed_by": "rift",
        "agent": token,
        "rift_bin": rift_bin.display().to_string(),
        "hooks": hooks,
        "compat": {
            "best_effort": notes.best_effort,
            "surfaces_token_usage": notes.surfaces_token_usage,
            "lifecycle_gaps": notes.lifecycle_gaps,
        },
    });
    let mut bytes = serde_json::to_vec_pretty(&value).unwrap_or_default();
    bytes.push(b'\n');
    bytes
}

pub(super) fn descriptor_rift_bin(repo: &Path, token: &str) -> Option<PathBuf> {
    let value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(descriptor_path(repo, token)).ok()?).ok()?;
    let path = PathBuf::from(value.get("rift_bin")?.as_str()?);
    crate::absolute_on_any_platform(&path).then_some(path)
}

/// Whether `bytes` at the descriptor path are Rift's own: self-identifying
/// and fingerprinted by the manifest, or — when the adapter is known — one of
/// the shapes [`legacy_owned_descriptor`] recognizes. The one predicate
/// `inspect_wiring`, `enable` and `disable` all read, so the three cannot
/// disagree about a file.
pub(super) fn rift_owns_descriptor(
    bytes: &[u8],
    token: &str,
    manifest: &OwnershipManifest,
    adapter: Option<&dyn AgentAdapter>,
) -> bool {
    (owned_descriptor(bytes, token) && manifest.owns_descriptor(token, &fingerprint(bytes)))
        || adapter.is_some_and(|adapter| legacy_owned_descriptor(bytes, token, adapter))
}

fn owned_descriptor(bytes: &[u8], token: &str) -> bool {
    serde_json::from_slice::<serde_json::Value>(bytes)
        .ok()
        .is_some_and(|value| {
            value.get("managed_by").and_then(serde_json::Value::as_str) == Some("rift")
                && value.get("agent").and_then(serde_json::Value::as_str) == Some(token)
        })
}

/// Recognize a descriptor written by `rift enable --agent` whose ownership the
/// manifest cannot vouch for.
///
/// **Three arms. The second is the migration the verb-table correction re-owed;
/// the third is the same migration one schema later.**
///
/// Arm 1 is the original: the whole parsed document equals the value derived
/// from TODAY's `verbs()` + `compat_notes()`. That arm is exact and cheap, and
/// it is also the reason a corrected verb table unrecognizes a pre-manifest
/// install — the document was derived from the table of its day, so a moved
/// table stops it matching and `inspect_wiring` reports
/// [`NativeWiring::Conflict`] on a file Rift itself wrote. That breakage is
/// PLANTED, not argued, by
/// [`a_moved_verb_table_conflicts_the_pre_manifest_install_without_arm_two`].
///
/// **MEASURED ON THIS MACHINE BEFORE THE ARM WAS WRITTEN**, because the
/// original sizing of this population and the tree disagreed:
/// the main checkout's `.rift/agents/codex.json` carries `managed_by`
/// and is listed in the manifest, so it takes the manifest path and is merely
/// rewritten; `.rift/agents/claude-code.json` has NO `managed_by` and is absent
/// from the manifest, so it is a genuine pre-manifest descriptor and arm 1 is
/// the only thing recognizing it. Its table did not move then, so
/// nothing on this machine broke — but the population is real, and the next
/// table correction breaks it.
///
/// Arm 2 therefore recognizes the SHAPE rather than the CONTENT: a document
/// whose key set, hook-command spelling and self-consistency are unmistakably
/// Rift's own pre-manifest output, whatever verb table it was written from.
/// This is version-proof where a list of superseded tables would grow forever
/// and be wrong the first time someone forgot to append to it.
///
/// What it does NOT relax: `agent` must be this token, `rift_bin` must be
/// absolute, every `hooks[i].command` must be exactly
/// `"<rift_bin> hooks <token> <verbs[i]>"` in order, and `compat` must carry
/// exactly the three keys Rift writes. A merely similar user file still fails
/// and is never adopted — and the only consequence of a true positive is that
/// `enable` rewrites the file to the current shape and takes ownership in the
/// manifest, which is what running `rift enable` asks for.
fn legacy_owned_descriptor(bytes: &[u8], token: &str, adapter: &dyn AgentAdapter) -> bool {
    let Ok(actual) = serde_json::from_slice::<serde_json::Value>(bytes) else {
        return false;
    };
    let Some(rift_bin) = actual.get("rift_bin").and_then(serde_json::Value::as_str) else {
        return false;
    };
    if !crate::absolute_on_any_platform(Path::new(rift_bin)) {
        return false;
    }
    actual == legacy_descriptor_value(Path::new(rift_bin), token, adapter)
        || pre_manifest_descriptor_shape(&actual, token, rift_bin)
        || current_descriptor_shape(&actual, token, rift_bin)
}

/// Whether `actual` is structurally one of Rift's own pre-manifest descriptors
/// for `token`, derived from ANY verb table. See [`legacy_owned_descriptor`].
fn pre_manifest_descriptor_shape(actual: &serde_json::Value, token: &str, rift_bin: &str) -> bool {
    let Some(root) = actual.as_object() else {
        return false;
    };
    // The exact key set Rift wrote before `managed_by`/`schema_version` existed.
    // A superset is a user file with extra fields; a subset is not ours.
    let keys: std::collections::BTreeSet<&str> = root.keys().map(String::as_str).collect();
    if keys
        != ["agent", "compat", "hooks", "rift_bin", "verbs"]
            .into_iter()
            .collect()
    {
        return false;
    }
    if root.get("agent").and_then(serde_json::Value::as_str) != Some(token) {
        return false;
    }
    let compat_keys: Option<std::collections::BTreeSet<&str>> = root
        .get("compat")
        .and_then(serde_json::Value::as_object)
        .map(|c| c.keys().map(String::as_str).collect());
    if compat_keys
        != Some(
            ["best_effort", "lifecycle_gaps", "surfaces_token_usage"]
                .into_iter()
                .collect(),
        )
    {
        return false;
    }
    let Some(verbs) = root.get("verbs").and_then(serde_json::Value::as_array) else {
        return false;
    };
    let Some(hooks) = root.get("hooks").and_then(serde_json::Value::as_array) else {
        return false;
    };
    if verbs.is_empty() || verbs.len() != hooks.len() {
        return false;
    }
    verbs.iter().zip(hooks).all(|(verb, hook)| {
        let Some(verb) = verb.as_str() else {
            return false;
        };
        rift_shaped_descriptor_hook(hook, token, rift_bin, verb)
    })
}

/// Whether `actual` is structurally one of Rift's own CURRENT-schema
/// descriptors for `token`, derived from ANY verb table — the third arm of
/// [`legacy_owned_descriptor`], for a descriptor whose manifest entry is gone.
///
/// The population is real and one `git clone` away: descriptors have been
/// committed to repositories while `.rift/state/` (the ownership manifest) is
/// gitignored, so a fresh checkout — or a state wipe — holds a file Rift wrote
/// in today's schema that the manifest cannot vouch for. Before this arm that
/// file matched nothing (arm 1 derives the pre-manifest key set, arm 2 demands
/// it) and read as `Conflict`, with both `enable` and `disable` refusing —
/// the descriptor-level twin of the hooks-side stranding the same correction
/// named.
///
/// Same trust model as arm 2: the exact key set Rift writes today
/// (`managed_by` + `schema_version`, no `verbs`), `agent` equal to this token,
/// `schema_version` 1, `compat` carrying exactly its three keys, and every
/// hook exactly `{verb, command}` with the UNQUOTED command spelling of
/// [`descriptor_bytes`] — deliberately distinct from the quoted hook-file
/// spelling. Verb-set free, so a table move cannot unrecognize it; a merely
/// similar user file fails on the first wrong byte and is never adopted.
fn current_descriptor_shape(actual: &serde_json::Value, token: &str, rift_bin: &str) -> bool {
    let Some(root) = actual.as_object() else {
        return false;
    };
    let keys: std::collections::BTreeSet<&str> = root.keys().map(String::as_str).collect();
    if keys
        != [
            "agent",
            "compat",
            "hooks",
            "managed_by",
            "rift_bin",
            "schema_version",
        ]
        .into_iter()
        .collect()
    {
        return false;
    }
    if root.get("managed_by").and_then(serde_json::Value::as_str) != Some("rift")
        || root.get("agent").and_then(serde_json::Value::as_str) != Some(token)
        || root
            .get("schema_version")
            .and_then(serde_json::Value::as_u64)
            != Some(1)
    {
        return false;
    }
    let compat_keys: Option<std::collections::BTreeSet<&str>> = root
        .get("compat")
        .and_then(serde_json::Value::as_object)
        .map(|c| c.keys().map(String::as_str).collect());
    if compat_keys
        != Some(
            ["best_effort", "lifecycle_gaps", "surfaces_token_usage"]
                .into_iter()
                .collect(),
        )
    {
        return false;
    }
    let Some(hooks) = root.get("hooks").and_then(serde_json::Value::as_array) else {
        return false;
    };
    if hooks.is_empty() {
        return false;
    }
    hooks.iter().all(|hook| {
        let Some(verb) = hook.get("verb").and_then(serde_json::Value::as_str) else {
            return false;
        };
        rift_shaped_descriptor_hook(hook, token, rift_bin, verb)
    })
}

/// One descriptor hook entry in Rift's own spelling: exactly `{verb, command}`
/// with `command == "<rift_bin> hooks <token> <verb>"` (unquoted binary).
fn rift_shaped_descriptor_hook(
    hook: &serde_json::Value,
    token: &str,
    rift_bin: &str,
    verb: &str,
) -> bool {
    let Some(hook) = hook.as_object() else {
        return false;
    };
    let named = hook.get("verb").and_then(serde_json::Value::as_str) == Some(verb);
    let commanded = hook.get("command").and_then(serde_json::Value::as_str)
        == Some(format!("{rift_bin} hooks {token} {verb}").as_str());
    named && commanded && hook.len() == 2
}

pub(super) fn legacy_descriptor_value(
    rift_bin: &Path,
    token: &str,
    adapter: &dyn AgentAdapter,
) -> serde_json::Value {
    let verbs = adapter.verbs();
    let notes = adapter.compat_notes();
    let hooks = verbs
        .iter()
        .map(|verb| {
            serde_json::json!({
                "verb": verb,
                "command": format!("{} hooks {token} {verb}", rift_bin.display()),
            })
        })
        .collect::<Vec<_>>();
    serde_json::json!({
        "agent": token,
        "rift_bin": rift_bin.display().to_string(),
        "verbs": verbs,
        "hooks": hooks,
        "compat": {
            "best_effort": notes.best_effort,
            "surfaces_token_usage": notes.surfaces_token_usage,
            "lifecycle_gaps": notes.lifecycle_gaps,
        },
    })
}

#[cfg(test)]
mod tests {
    use rift_core::AgentId;
    use rift_shadow_hooks::AdapterRegistry;

    use crate::InstallError;

    use super::super::lifecycle::enable;
    use super::super::manifest::{read_manifest, write_manifest};
    use super::super::{inspect_wiring, NativeWiring};
    use super::*;

    /// One agent's verb table frozen at a point in time, which is exactly what
    /// an installed descriptor is: a document derived from `verbs()` and
    /// `compat_notes()` on the day it was written.
    struct FrozenCursor {
        verbs: &'static [&'static str],
    }

    /// Cursor's declared table today — the one every existing install's
    /// descriptor was derived from.
    const CURSOR_AS_INSTALLED: &[&str] = &["session_start", "edit", "session_end"];

    /// Cursor's real hook surface, cited to the pinned tag by
    /// `rift-shadow-hooks/tests/adapter_sweep.rs` (`cursor.verb_surface`). This
    /// is what the table becomes when the sweep's Gap row is closed.
    const CURSOR_AS_PUBLISHED: &[&str] = &[
        "sessionStart",
        "sessionEnd",
        "beforeSubmitPrompt",
        "stop",
        "preCompact",
        "subagentStart",
        "subagentStop",
    ];

    impl AgentAdapter for FrozenCursor {
        fn agent_id(&self) -> AgentId {
            AgentId::new("cursor")
        }

        fn map(
            &self,
            verb: &str,
            _native: &serde_json::Value,
        ) -> Result<Vec<rift_shadow_hooks::CanonicalEvent>, rift_shadow_hooks::AdapterError>
        {
            Err(rift_shadow_hooks::AdapterError::UnknownVerb(
                verb.to_owned(),
            ))
        }

        fn verbs(&self) -> &'static [&'static str] {
            self.verbs
        }

        fn compat_notes(&self) -> rift_shadow_hooks::CompatNotes {
            rift_shadow_hooks::CompatNotes {
                agent: AgentId::new("cursor"),
                lifecycle_gaps: &[],
                surfaces_token_usage: false,
                best_effort: true,
                note: "frozen table, test double",
            }
        }
    }

    /// The descriptor an older `rift enable` left behind for Cursor, derived
    /// from the table of its day, at the path today's `enable` reads.
    fn write_pre_manifest_cursor_descriptor(dir: &Path) -> PathBuf {
        let path = descriptor_path(dir, "cursor");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let as_installed = FrozenCursor {
            verbs: CURSOR_AS_INSTALLED,
        };
        let legacy = serde_json::to_vec_pretty(&legacy_descriptor_value(
            Path::new("/opt/rift"),
            "cursor",
            &as_installed,
        ))
        .unwrap();
        std::fs::write(&path, &legacy).unwrap();
        path
    }

    /// **MEASURED, NOT REASONED: correcting a verb table breaks the
    /// PRE-MANIFEST population and spares the manifest-owned one.**
    ///
    /// It was first sized as "every pre-existing install"; it is smaller
    /// than that, and the difference is the whole shape of the migration owed.
    /// Two recognizers run over a descriptor, and only one of them reads the
    /// verb table:
    ///
    /// * `legacy_owned_descriptor` compares the WHOLE document against a value
    ///   derived from `verbs()` + `compat_notes()`, so a moved table stops it
    ///   matching — `NativeWiring::Conflict` in doctor and Project Settings,
    ///   and `InstallError::Config` out of `enable`;
    /// * `owned_descriptor` + the manifest checks `managed_by`/`agent` and
    ///   compares the manifest hash against the hash of the bytes ALREADY ON
    ///   DISK — never against a freshly derived value — so a moved table cannot
    ///   unmatch it. It is simply rewritten.
    ///
    /// This is the pre-manifest half: the plant, and arm 2 closing it. The
    /// forged-file arm and the manifest-owned half follow as their own tests.
    #[test]
    fn a_moved_verb_table_conflicts_the_pre_manifest_install_without_arm_two() {
        let dir = tempfile::tempdir().unwrap();
        let rift_bin = Path::new("/opt/rift");
        let as_installed = FrozenCursor {
            verbs: CURSOR_AS_INSTALLED,
        };
        let as_published = FrozenCursor {
            verbs: CURSOR_AS_PUBLISHED,
        };
        write_pre_manifest_cursor_descriptor(dir.path());
        assert_eq!(
            inspect_wiring(dir.path(), "cursor", &as_installed),
            NativeWiring::Managed,
            "the adapter that wrote it must still recognize it"
        );
        // THE BREAKAGE, still planted rather than argued: arm 1 alone does not
        // recognize it. Asserted on the predicate `inspect_wiring` reads, so the
        // plant survives the migration that closes it — deleting this line would
        // leave the migration with nothing to be a migration FROM.
        assert!(
            !(serde_json::to_vec_pretty(&legacy_descriptor_value(
                rift_bin,
                "cursor",
                &as_installed
            ))
            .map(|bytes| {
                let actual: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
                actual == legacy_descriptor_value(rift_bin, "cursor", &as_published)
            })
            .unwrap()),
            "THE BREAKAGE: a corrected verb table unrecognizes a pre-manifest \
             install through arm 1 (whole-document equality)"
        );
        // …AND ARM 2 CLOSES IT. The same file, the same moved table, now
        // recognized by shape: `enable` migrates it instead of refusing, and
        // the migrated document carries the manifest marker.
        assert_eq!(
            inspect_wiring(dir.path(), "cursor", &as_published),
            NativeWiring::Managed,
            "the migration must adopt a descriptor Rift itself wrote from an \
             older table"
        );
    }

    /// The negative arm, which is what stops the migration being a hole: a
    /// merely similar user file is STILL refused. One field is wrong — the
    /// hook command names a different agent than the descriptor does.
    #[test]
    fn a_forged_pre_manifest_descriptor_stays_a_conflict_under_the_moved_table() {
        let dir = tempfile::tempdir().unwrap();
        let rift_bin = Path::new("/opt/rift");
        let path = write_pre_manifest_cursor_descriptor(dir.path());
        let as_installed = FrozenCursor {
            verbs: CURSOR_AS_INSTALLED,
        };
        let as_published = FrozenCursor {
            verbs: CURSOR_AS_PUBLISHED,
        };
        let mut forged = legacy_descriptor_value(rift_bin, "cursor", &as_installed);
        forged["hooks"][0]["command"] = serde_json::json!(format!(
            "{} hooks not-cursor session_start",
            rift_bin.display()
        ));
        std::fs::write(&path, serde_json::to_vec_pretty(&forged).unwrap()).unwrap();
        assert_eq!(
            inspect_wiring(dir.path(), "cursor", &as_published),
            NativeWiring::Conflict,
            "a file that merely resembles a Rift descriptor is never adopted"
        );
        let refused = enable(dir.path(), rift_bin, "cursor").unwrap_err();
        assert!(
            matches!(refused, InstallError::Config { .. }),
            "expected a Config refusal, got {refused:?}"
        );
    }

    /// The manifest-owned population survives the identical change: the
    /// manifest hash is compared against the bytes already on disk, never
    /// against a freshly derived value, so a moved table cannot unmatch it.
    #[test]
    fn a_manifest_owned_install_survives_the_moved_verb_table() {
        let dir = tempfile::tempdir().unwrap();
        let as_installed = FrozenCursor {
            verbs: CURSOR_AS_INSTALLED,
        };
        let as_published = FrozenCursor {
            verbs: CURSOR_AS_PUBLISHED,
        };
        let path = descriptor_path(dir.path(), "cursor");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let current = descriptor_bytes(
            Path::new("/opt/rift"),
            "cursor",
            as_installed.verbs(),
            &as_installed.compat_notes(),
        );
        std::fs::write(&path, &current).unwrap();
        let mut manifest = OwnershipManifest::default();
        manifest
            .descriptors
            .insert("cursor".to_owned(), fingerprint(&current));
        write_manifest(dir.path(), &manifest).unwrap();
        assert_eq!(
            inspect_wiring(dir.path(), "cursor", &as_published),
            NativeWiring::Managed,
            "a manifest-owned install must survive the correction, not conflict"
        );
    }

    /// **The clone landmine, closed: a CURRENT-schema descriptor whose
    /// manifest never travelled is still Rift's own file.**
    ///
    /// Descriptors have been committed to repositories while `.rift/state/`
    /// is gitignored, so a fresh checkout — or a state wipe — holds a
    /// descriptor Rift wrote in today's schema with no manifest entry to
    /// vouch for it. Arm 1 derives the pre-manifest key set and arm 2 demands
    /// it, so before the third arm this file matched NOTHING and read
    /// `Conflict`, with `enable` and `disable` both refusing — the
    /// descriptor-level twin of the hooks-side stranding. The moved-table
    /// half drives the arm through a frozen adapter so it stays version-proof,
    /// not merely schema-proof.
    #[test]
    fn a_cloned_descriptor_without_local_state_is_still_rift_managed() {
        // Half 1: today's schema, today's table, no manifest → Managed, and
        // `enable` re-adopts it into a fresh manifest.
        let dir = tempfile::tempdir().unwrap();
        let registry = AdapterRegistry::with_builtin();
        let adapter = registry.get(&AgentId::new("codex")).unwrap();
        let path = descriptor_path(dir.path(), "codex");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            descriptor_bytes(
                Path::new("/opt/rift"),
                "codex",
                adapter.verbs(),
                &adapter.compat_notes(),
            ),
        )
        .unwrap();
        assert_eq!(
            inspect_wiring(dir.path(), "codex", adapter),
            NativeWiring::Managed,
            "a descriptor Rift wrote must not conflict because the manifest \
             stayed behind on another machine"
        );
        enable(dir.path(), Path::new("/opt/rift"), "codex").unwrap();
        assert!(
            read_manifest(dir.path()).descriptors.contains_key("codex"),
            "enable must re-adopt the descriptor into the manifest"
        );

        // Half 2: the same recognition survives a MOVED verb table — the file
        // was written from the installed table, inspected under the published
        // one.
        let dir = tempfile::tempdir().unwrap();
        let as_installed = FrozenCursor {
            verbs: CURSOR_AS_INSTALLED,
        };
        let as_published = FrozenCursor {
            verbs: CURSOR_AS_PUBLISHED,
        };
        let path = descriptor_path(dir.path(), "cursor");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            descriptor_bytes(
                Path::new("/opt/rift"),
                "cursor",
                as_installed.verbs(),
                &as_installed.compat_notes(),
            ),
        )
        .unwrap();
        assert_eq!(
            inspect_wiring(dir.path(), "cursor", &as_published),
            NativeWiring::Managed,
            "the arm is verb-set free: a table move cannot unrecognize it"
        );
    }

    /// One byte-level deviation from Rift's current descriptor emission.
    type DescriptorMutation = fn(&mut serde_json::Value);

    /// The negative arms for the third recognizer: one wrong byte-level fact
    /// each, all `Conflict`, all refused by `enable`, bytes untouched.
    #[test]
    fn a_file_that_merely_resembles_a_current_descriptor_is_never_adopted() {
        let registry = AdapterRegistry::with_builtin();
        let adapter = registry.get(&AgentId::new("codex")).unwrap();
        let pristine: serde_json::Value = serde_json::from_slice(&descriptor_bytes(
            Path::new("/opt/rift"),
            "codex",
            adapter.verbs(),
            &adapter.compat_notes(),
        ))
        .unwrap();
        let cases: [(&str, DescriptorMutation); 6] = [
            ("a command naming another agent", |v| {
                v["hooks"][0]["command"] =
                    serde_json::json!("/opt/rift hooks not-codex SessionStart");
            }),
            ("an extra top-level key", |v| {
                v["note"] = serde_json::json!("mine");
            }),
            ("a foreign managed_by", |v| {
                v["managed_by"] = serde_json::json!("other");
            }),
            ("a relative rift_bin", |v| {
                v["rift_bin"] = serde_json::json!("target/debug/rift");
            }),
            ("a three-key hook entry", |v| {
                v["hooks"][0]["timeout"] = serde_json::json!(10);
            }),
            ("an unknown schema_version", |v| {
                v["schema_version"] = serde_json::json!(2);
            }),
        ];
        for (why, mutate) in cases {
            let dir = tempfile::tempdir().unwrap();
            let path = descriptor_path(dir.path(), "codex");
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let mut value = pristine.clone();
            mutate(&mut value);
            let mut bytes = serde_json::to_vec_pretty(&value).unwrap();
            bytes.push(b'\n');
            std::fs::write(&path, &bytes).unwrap();
            assert_eq!(
                inspect_wiring(dir.path(), "codex", adapter),
                NativeWiring::Conflict,
                "{why}: must stay a conflict"
            );
            let error = enable(dir.path(), Path::new("/opt/rift"), "codex").unwrap_err();
            assert!(
                error.to_string().contains("ownership fingerprint"),
                "{why}: {error}"
            );
            assert_eq!(
                std::fs::read(&path).unwrap(),
                bytes,
                "{why}: bytes must be untouched"
            );
        }
    }
}
