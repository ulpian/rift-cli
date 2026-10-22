//! Typed, validated policy-as-code loaded from the committed `.rift/` directory.
//!
//! The only Rift state in the working tree is a thin committed `.rift/`;
//! everything else lives under the machine's cache root. This crate turns it into a typed
//! [`EffectiveConfig`] downstream crates consult for security decisions: which
//! models are allowed, which paths are sensitive, what is captured, how long
//! things are retained.
//!
//! ## Format note (deliberate deviation from the step's illustrative YAML)
//! `toml` is maintained and `cargo-deny`-clean, while `serde_yaml` is
//! unmaintained. So Rift loads `.rift/config.toml` rather than
//! `project.yaml` — same typed model, same `deny_unknown_fields` strictness,
//! same secure defaults, supply-chain-clean.
//!
//! Loading is **strict**: unknown keys are hard errors (a typo cannot silently
//! disable a redactor), a `raw_transcripts = true` written out requires an
//! explicit `acknowledge_risk`, built-in sensitive paths are unioned in and
//! cannot be dropped, and validation **aggregates** every problem at once.
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

mod edit;
mod effective;
mod glob;
pub mod redactor_packs;
mod schema;
mod source;

pub use edit::{
    load_project_policy, patch_project_policy, set_checkpoint_remote, PolicyEditError,
    PolicyRevision, ProjectPolicyPatch, ProjectPolicySnapshot,
};
pub use effective::{idle_sweep_ms, EffectiveConfig, BUILTIN_SENSITIVE_PATHS};
pub use glob::{glob_match, wildcard_count, MAX_PATTERN_LEN, MAX_WILDCARDS};
pub use redactor_packs::{
    load_redactor_packs, redactor_pack_dir, LoadedPacks, PackDiagnostic, RedactorPack,
    RedactorRule, RedactorSample, REDACTORS_DIR_NAME, REDACTOR_PACK_SCHEMA_VERSION,
};
pub use schema::{
    CapturePolicy, CheckpointPolicy, FeaturePolicy, ModelPolicy, PathPolicy, ProjectMeta,
    RedactionPolicy, RetentionPolicy, RiftConfig, CHECKPOINT_STRATEGIES,
};
pub use source::{checkpoint_remote, PolicyRefusal, PolicySource};

#[cfg(test)]
mod tests {
    use rift_core::{Result, RiftError};

    use super::*;

    fn parse(toml_src: &str) -> Result<EffectiveConfig> {
        EffectiveConfig::from_toml_str(toml_src)
    }

    proptest::proptest! {
        /// The untrusted config parser must never panic/hang/OOM on arbitrary
        /// bytes — it returns a `Result` for any input. Runs in the
        /// normal `cargo test` job, so it gates without nightly/libFuzzer.
        #[test]
        fn parser_never_panics_on_arbitrary_bytes(
            bytes in proptest::collection::vec(proptest::num::u8::ANY, 0..4096)
        ) {
            let _ = EffectiveConfig::from_bytes(&bytes);
        }

        /// Same contract over arbitrary UTF-8 text (exercises the TOML grammar).
        ///
        /// `(?s)` is load-bearing: TOML is line-oriented -- tables, key/value
        /// pairs and comments all end at a newline -- so a generator whose `.`
        /// can never produce one exercises the grammar's least interesting half.
        #[test]
        fn parser_never_panics_on_arbitrary_text(s in "(?s).*") {
            let _ = EffectiveConfig::from_toml_str(&s);
        }
    }

    #[test]
    fn loads_minimal_config() {
        let eff = parse("schema_version = 1\n[project]\nname = \"demo\"\n").unwrap();
        assert_eq!(eff.project_name(), "demo");
        assert!(
            eff.raw_transcripts_enabled(),
            "raw transcripts are on by default"
        );
        assert!(eff.captures_tool_calls());
    }

    /// Writing a checkpoint remote must not cost a repository its policy.
    ///
    /// `[checkpoint]` was an unknown key under `deny_unknown_fields`, so a
    /// repository that had ever run `rift configure --checkpoint-remote` made
    /// [`EffectiveConfig::load`] return `Err` — every declared sensitive path,
    /// model rule and retention class went with it. Delete
    /// `RiftConfig::checkpoint` and this reddens on the first assertion.
    #[test]
    fn a_configured_checkpoint_remote_does_not_break_the_policy_load() {
        let src = "schema_version = 1\n\
                   [project]\nname = \"demo\"\n\
                   [paths]\nsensitive_paths = [\"secrets/**\"]\n\
                   [checkpoint]\nremote = \"rift-origin\"\n";
        let eff = parse(src).expect("a configured checkpoint remote must still load as policy");
        assert!(
            eff.is_sensitive_path("secrets/db.pem"),
            "the project's declared policy must survive the presence of a checkpoint remote"
        );
    }

    /// The destination is not the policy: naming a remote — or changing which
    /// one — must not move `policy_hash`, or every checkpoint captured after a
    /// remote change would look as though it were captured under a different
    /// policy. This is the same exclusion `policy_source` gets, for the same
    /// reason.
    #[test]
    fn the_checkpoint_remote_is_not_part_of_the_policy_hash() {
        let base = "schema_version = 1\n[project]\nname = \"demo\"\n";
        let none = parse(base).unwrap();
        let one = parse(&format!("{base}[checkpoint]\nremote = \"a\"\n")).unwrap();
        let two = parse(&format!("{base}[checkpoint]\nremote = \"b\"\n")).unwrap();
        assert_eq!(none.policy_hash(), one.policy_hash());
        assert_eq!(one.policy_hash(), two.policy_hash());
    }

    /// The single resolver: absent means **not configured**, and every caller
    /// must read that as "do not ship". There is no `origin` fallback anywhere
    /// in this function, which is the whole point of it existing.
    #[test]
    fn an_unconfigured_repository_resolves_to_no_checkpoint_remote() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(checkpoint_remote(tmp.path()).unwrap(), None);

        std::fs::create_dir_all(tmp.path().join(".rift")).unwrap();
        std::fs::write(
            tmp.path().join(".rift/config.toml"),
            "schema_version = 1\n[project]\nname = \"demo\"\n",
        )
        .unwrap();
        assert_eq!(
            checkpoint_remote(tmp.path()).unwrap(),
            None,
            "a config with no [checkpoint] table names no remote"
        );

        std::fs::write(
            tmp.path().join(".rift/config.toml"),
            "schema_version = 1\n[project]\nname = \"demo\"\n[checkpoint]\nremote = \"rift-origin\"\n",
        )
        .unwrap();
        assert_eq!(
            checkpoint_remote(tmp.path()).unwrap().as_deref(),
            Some("rift-origin")
        );
    }

    /// The value becomes an argv word in a `git` invocation, so the shapes that
    /// stop being a remote name are refusals rather than silent `None`s: a typo
    /// must be reported, not disguised as "not configured".
    #[test]
    fn an_unsafe_checkpoint_remote_is_refused_by_reason_not_by_value() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(".rift")).unwrap();
        let head = "schema_version = 1\n[project]\nname = \"demo\"\n[checkpoint]\n";
        for (value, needle) in [
            ("\"\"", "empty"),
            ("\"--upload-pack=evil\"", "flag"),
            ("\"a b\"", "whitespace"),
            ("\"a\\tb\"", "whitespace"),
            ("7", "string"),
        ] {
            std::fs::write(
                tmp.path().join(".rift/config.toml"),
                format!("{head}remote = {value}\n"),
            )
            .unwrap();
            let err = checkpoint_remote(tmp.path())
                .expect_err("an unusable remote token must be an error");
            let text = err.to_string();
            assert!(
                text.contains(needle),
                "refusal must name the reason ({needle}), got: {text}"
            );
            assert!(
                !text.contains("evil"),
                "a refusal names the reason, never the value: {text}"
            );
        }
    }

    #[test]
    fn missing_rift_dir_yields_secure_defaults() {
        let dir = std::env::temp_dir().join("rift-config-absent-xyz");
        let eff = EffectiveConfig::load(&dir).unwrap();
        assert!(eff.raw_transcripts_enabled());
        assert!(eff.is_sensitive_path("project/.env"));
        assert!(eff.is_sensitive_path("a/b/id_rsa"));
    }

    /// A policy file the work tree does not own is not policy. The value read
    /// through the link would be wrong at best; the error raised by failing to
    /// parse it carries a line of the target, which is how a credentials file
    /// ends up on a terminal.
    ///
    /// The control leg is the same bytes in a *regular* file: that still errors,
    /// and it still quotes the line, because there the line is the repository's
    /// own content and the operator needs to see it. The difference between the
    /// two legs is the whole guard.
    #[cfg(unix)]
    #[test]
    fn a_linked_policy_file_is_not_read() {
        let tmp = tempfile::tempdir().unwrap();
        let outside = tmp.path().join("credentials");
        std::fs::write(
            &outside,
            "[default]\naws_access_key_id = AKIAIOSFODNN7EXAMPLE\n",
        )
        .unwrap();

        let rift_dir = tmp.path().join("repo/.rift");
        std::fs::create_dir_all(&rift_dir).unwrap();
        std::os::unix::fs::symlink(&outside, rift_dir.join("config.toml")).unwrap();

        let eff = EffectiveConfig::load(&rift_dir).expect("a link degrades, it does not fail");
        assert_eq!(eff.project_name(), "unnamed", "secure defaults expected");
        assert!(eff.is_sensitive_path("project/.env"));

        // control: the same bytes as a real file are read, and the parse error
        // quotes the line — which is exactly what must not happen to a target.
        let owned = tmp.path().join("owned/.rift");
        std::fs::create_dir_all(&owned).unwrap();
        std::fs::copy(&outside, owned.join("config.toml")).unwrap();
        let err = EffectiveConfig::load(&owned).expect_err("a real file still parses or errors");
        assert!(
            err.to_string().contains("AKIAIOSFODNN7EXAMPLE"),
            "control leg must show the mechanism the guard defeats: {err}"
        );
    }

    /// The same for a linked `.rift` itself: the directory name is repository
    /// content too, and `<link>/config.toml` resolves through it just as well.
    #[cfg(unix)]
    #[test]
    fn a_linked_rift_directory_is_not_read() {
        let tmp = tempfile::tempdir().unwrap();
        let elsewhere = tmp.path().join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::fs::write(
            elsewhere.join("config.toml"),
            "schema_version = 1\n[project]\nname = \"leaked\"\n",
        )
        .unwrap();

        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let rift_dir = repo.join(".rift");
        std::os::unix::fs::symlink(&elsewhere, &rift_dir).unwrap();

        let eff = EffectiveConfig::load(&rift_dir).unwrap();
        assert_eq!(
            eff.project_name(),
            "unnamed",
            "a linked .rift must not supply policy"
        );
    }

    /// A directory where the policy file belongs used to reach `read_to_string`
    /// and surface an IO error; it degrades like every other unreadable shape.
    #[test]
    fn a_directory_in_the_policy_files_place_degrades() {
        let tmp = tempfile::tempdir().unwrap();
        let rift_dir = tmp.path().join(".rift");
        std::fs::create_dir_all(rift_dir.join("config.toml")).unwrap();
        let eff = EffectiveConfig::load(&rift_dir).expect("degrades, never fails");
        assert_eq!(eff.project_name(), "unnamed");
    }

    /// The guard does not cost the ordinary case: a real file in a real `.rift`
    /// is still read and still applied.
    #[test]
    fn an_owned_policy_file_is_still_read() {
        let tmp = tempfile::tempdir().unwrap();
        let rift_dir = tmp.path().join(".rift");
        std::fs::create_dir_all(&rift_dir).unwrap();
        std::fs::write(
            rift_dir.join("config.toml"),
            "schema_version = 1\n[project]\nname = \"demo\"\n",
        )
        .unwrap();
        let eff = EffectiveConfig::load(&rift_dir).unwrap();
        assert_eq!(eff.project_name(), "demo");
    }

    /// Refusing is right; refusing **silently** is the half that was missing.
    ///
    /// Every degraded shape produces the same secure defaults, so no caller can
    /// tell "this project declared no policy" from "this project's policy was
    /// refused and none of it is running" — the built-in floor holds either way,
    /// and what is lost is exactly what the developer wrote. `policy_source`
    /// answers it, absent and refused are different answers, and each refusal
    /// carries the reason `symlink_metadata` actually saw.
    #[cfg(unix)]
    #[test]
    fn a_refused_policy_file_says_so() {
        let tmp = tempfile::tempdir().unwrap();
        let outside = tmp.path().join("elsewhere");
        std::fs::create_dir_all(&outside).unwrap();
        let declared = "schema_version = 1\n[project]\nname = \"declared\"\n\
                        [paths]\nsensitive_paths = [\"**/*.ledger\"]\n";
        std::fs::write(outside.join("config.toml"), declared).unwrap();

        // 1. no .rift at all -> Default, no diagnostic
        let absent = tmp.path().join("none/.rift");
        let eff = EffectiveConfig::load(&absent).unwrap();
        assert_eq!(eff.policy_source(), PolicySource::Default);
        assert!(eff.policy_source().diagnostic().is_none());

        // 2. a real .rift with a real config -> Declared
        let owned = tmp.path().join("owned/.rift");
        std::fs::create_dir_all(&owned).unwrap();
        std::fs::write(owned.join("config.toml"), declared).unwrap();
        let applied = EffectiveConfig::load(&owned).unwrap();
        assert_eq!(applied.policy_source(), PolicySource::Declared);
        assert!(applied.policy_source().is_declared_policy_applied());
        assert!(applied.is_sensitive_path("book.ledger"));

        // 3. a linked config.toml -> Refused, and the reason names the link
        let linked_file = tmp.path().join("lf/.rift");
        std::fs::create_dir_all(&linked_file).unwrap();
        std::os::unix::fs::symlink(outside.join("config.toml"), linked_file.join("config.toml"))
            .unwrap();
        // 4. a linked .rift directory -> Refused, different reason
        let linked_dir_repo = tmp.path().join("ld");
        std::fs::create_dir_all(&linked_dir_repo).unwrap();
        let linked_dir = linked_dir_repo.join(".rift");
        std::os::unix::fs::symlink(&outside, &linked_dir).unwrap();
        // 5. a directory where the file belongs -> Refused, third reason
        let dir_in_place = tmp.path().join("dip/.rift");
        std::fs::create_dir_all(dir_in_place.join("config.toml")).unwrap();

        for (dir, reason) in [
            (&linked_file, PolicyRefusal::ConfigIsASymlink),
            (&linked_dir, PolicyRefusal::RiftDirIsNotADirectory),
            (&dir_in_place, PolicyRefusal::ConfigIsNotARegularFile),
        ] {
            let eff = EffectiveConfig::load(dir).expect("a refusal degrades, it does not fail");
            assert_eq!(
                eff.policy_source(),
                PolicySource::Refused(reason),
                "{}",
                dir.display()
            );
            assert!(!eff.policy_source().is_declared_policy_applied());
            let diagnostic = eff
                .policy_source()
                .diagnostic()
                .expect("a refusal must carry a diagnostic");
            assert!(diagnostic.contains("declared policy is not applied"));
            // The built-in floor is identical to the ordinary default load --
            // which is precisely why nothing else can notice the refusal.
            let defaults = EffectiveConfig::secure_default().unwrap();
            assert_eq!(eff.sensitive_paths(), defaults.sensitive_paths());
            assert_eq!(eff.policy_hash(), defaults.policy_hash());
            // ...and the declared policy is gone.
            assert!(!eff.is_sensitive_path("book.ledger"));
        }
    }

    /// The provenance is not part of the policy's identity: two projects with
    /// the same policy hash the same, however each of them got there.
    #[test]
    fn policy_source_is_not_part_of_the_policy_hash() {
        let a = EffectiveConfig::secure_default().unwrap();
        let b = EffectiveConfig::from_config(RiftConfig::secure_default()).unwrap();
        assert_ne!(a.policy_source(), b.policy_source());
        assert_eq!(a.policy_hash(), b.policy_hash());
    }

    #[test]
    fn unknown_policy_key_is_rejected() {
        let err =
            parse("schema_version = 1\n[project]\nname=\"x\"\n[capture]\nraw_transcript = true\n");
        assert!(matches!(err, Err(RiftError::Config(_))));
    }

    #[test]
    fn model_in_allow_and_deny_is_invalid() {
        let err = parse(
            "schema_version=1\n[project]\nname=\"x\"\n[models]\nallow=[\"claude-*\"]\ndeny=[\"claude-*\"]\n",
        );
        match err {
            Err(RiftError::Config(m)) => assert!(m.contains("both allow and deny")),
            other => panic!("expected config error, got {other:?}"),
        }
    }

    /// Contract snapshots: a representative effective
    /// config's Debug view and a multi-problem aggregated validation report,
    /// pinned so a shape change is a reviewable diff.
    #[test]
    fn effective_config_and_error_report_snapshots() {
        let cfg = parse(
            "schema_version = 1\n\
             [project]\nname = \"demo\"\nrepo_id = \"repo-demo\"\n\
             [models]\ndeny = [\"gpt-3*\"]\n\
             [paths]\nsensitive_paths = [\"secrets/**\"]\n\
             [capture]\ntool_calls = true\n",
        )
        .expect("valid config");
        insta::assert_snapshot!("effective_config_debug", format!("{cfg:#?}"));

        let err = parse(
            "schema_version = 99\n[project]\nname = \"x\"\n\
             [paths]\nsensitive_paths = [\"\"]\n\
             [models]\nallow = [\"m1\"]\ndeny = [\"m1\"]\n",
        )
        .expect_err("three validation problems");
        insta::assert_snapshot!("aggregated_validation_report", err.to_string());
    }

    #[test]
    fn bad_glob_and_schema_aggregate() {
        let err =
            parse("schema_version=99\n[project]\nname=\"x\"\n[paths]\nsensitive_paths=[\"\"]\n");
        match err {
            Err(RiftError::Config(m)) => {
                assert!(m.contains("unsupported schema_version"));
                assert!(m.contains("empty glob"));
            }
            other => panic!("expected aggregated config error, got {other:?}"),
        }
    }

    /// The refusal message of a config whose `[redaction]` table holds `body`.
    fn redaction_refusal(body: &str) -> String {
        let src = format!("schema_version = 1\n[project]\nname = \"x\"\n[redaction]\n{body}\n");
        match parse(&src) {
            Err(RiftError::Config(m)) => m,
            other => panic!("expected the [redaction] key to be refused, got {other:?}"),
        }
    }

    #[test]
    fn an_entropy_threshold_is_refused_and_the_floor_is_said_to_be_fixed() {
        let m = redaction_refusal("entropy_threshold = 3.0");
        assert!(m.contains("redaction.entropy_threshold"), "{m}");
        assert!(m.contains("fixed"), "{m}");
        assert!(m.contains(".rift/policies/redactors"), "{m}");
    }

    #[test]
    fn extra_patterns_are_refused_and_pointed_at_a_rule_pack() {
        let m = redaction_refusal("extra_patterns = [\"acme-[a-z0-9]{20}\"]");
        assert!(m.contains("redaction.extra_patterns"), "{m}");
        assert!(m.contains(".rift/policies/redactors"), "{m}");
    }

    #[test]
    fn an_empty_redaction_table_loads_with_the_absent_tables_fingerprint() {
        let absent = parse("schema_version = 1\n[project]\nname = \"x\"\n").unwrap();
        let empty = parse("schema_version = 1\n[project]\nname = \"x\"\n[redaction]\n").unwrap();
        assert_eq!(absent.policy_hash(), empty.policy_hash());
    }

    /// Fingerprints already stamped into events and summary bindings must keep
    /// matching, so the preimage keeps the fixed entropy line the retired key
    /// once contributed. The pinned value moves only with a real policy
    /// change: it last moved when raw transcripts became on by default.
    #[test]
    fn the_default_policy_fingerprint_is_unchanged_by_the_retired_redaction_keys() {
        let default = EffectiveConfig::secure_default().unwrap();
        assert_eq!(
            default.policy_hash().to_string(),
            "b700dc637f25407f23c5e3f378fceb2350fe01fe0482e2300c8381e0cd7b79b2"
        );
    }

    #[test]
    fn raw_transcripts_requires_acknowledge() {
        let err =
            parse("schema_version=1\n[project]\nname=\"x\"\n[capture]\nraw_transcripts=true\n");
        assert!(matches!(err, Err(RiftError::Config(_))));
        let ok = parse(
            "schema_version=1\n[project]\nname=\"x\"\n[capture]\nraw_transcripts=true\nacknowledge_risk=true\n",
        )
        .unwrap();
        assert!(ok.raw_transcripts_enabled());
    }

    #[test]
    fn a_config_that_writes_raw_transcripts_off_keeps_them_off() {
        let off = parse(
            "schema_version=1\n[project]\nname=\"x\"\n[capture]\nraw_transcripts=false\nacknowledge_risk=false\n",
        )
        .unwrap();
        assert!(!off.raw_transcripts_enabled());
        let bare =
            parse("schema_version=1\n[project]\nname=\"x\"\n[capture]\nraw_transcripts=false\n")
                .unwrap();
        assert!(!bare.raw_transcripts_enabled());
    }

    /// The acknowledgement guards a file that writes the option on, not the
    /// shipped default: a file silent on the option is on and valid whatever
    /// it says about the acknowledgement.
    #[test]
    fn the_default_raw_transcripts_need_no_acknowledgement() {
        let silent =
            parse("schema_version=1\n[project]\nname=\"x\"\n[capture]\nacknowledge_risk=false\n")
                .unwrap();
        assert!(silent.raw_transcripts_enabled());
        assert!(EffectiveConfig::secure_default()
            .unwrap()
            .raw_transcripts_enabled());
    }

    #[test]
    fn builtin_sensitive_paths_cannot_be_dropped() {
        let eff = parse(
            "schema_version=1\n[project]\nname=\"x\"\n[paths]\nsensitive_paths=[\"**/custom_secret_dir/**\"]\n",
        )
        .unwrap();
        assert!(eff.is_sensitive_path("a/.env"));
        assert!(eff.is_sensitive_path("x/custom_secret_dir/y"));
    }

    #[test]
    fn model_allow_deny_semantics() {
        let eff = parse(
            "schema_version=1\n[project]\nname=\"x\"\n[models]\nallow=[\"claude-*\"]\ndeny=[\"*-experimental\"]\n",
        )
        .unwrap();
        assert!(eff.is_model_allowed("claude-opus-4-8"));
        assert!(!eff.is_model_allowed("claude-experimental"));
        assert!(!eff.is_model_allowed("gpt-4"));
    }

    #[test]
    fn policy_hash_is_stable_across_key_order() {
        let a = parse("schema_version=1\n[project]\nname=\"x\"\n[models]\nallow=[\"a\",\"b\"]\n")
            .unwrap();
        let b = parse("schema_version=1\n[models]\nallow=[\"b\",\"a\"]\n[project]\nname=\"x\"\n")
            .unwrap();
        assert_eq!(a.policy_hash(), b.policy_hash());
    }

    /// Declaring the search key — under either spelling, at either value —
    /// leaves the policy hash byte-identical to a config that never mentions
    /// it. That is what makes the rename safe to ship: `policy_ver` is stamped
    /// into served summaries, so a key that fed the hash would invalidate every
    /// repository's derived memory the day this landed.
    #[test]
    fn the_search_feature_key_is_hash_inert_under_both_spellings() {
        let plain = parse("schema_version=1\n[project]\nname=\"x\"\n").unwrap();
        for src in [
            "schema_version=1\n[project]\nname=\"x\"\n[features]\nsearch=true\n",
            "schema_version=1\n[project]\nname=\"x\"\n[features]\nsearch=false\n",
            "schema_version=1\n[project]\nname=\"x\"\n[features]\nfff=true\n",
            "schema_version=1\n[project]\nname=\"x\"\n[features]\nfff=false\n",
        ] {
            let cfg = parse(src).unwrap();
            assert_eq!(plain.policy_hash(), cfg.policy_hash(), "hashed: {src:?}");
            assert!(cfg.recall_enabled(), "recall follows its own key: {src:?}");
        }
    }

    /// Recall is on unless the repository declares it off: an absent
    /// `[features]` table, a table without the key, and an explicit `true`
    /// all enable it; only `total_recall = false` turns it off. The absent
    /// and explicit-on forms hash identically, so a repository that writes
    /// out the default does not invalidate its derived memory.
    #[test]
    fn recall_is_on_unless_declared_off() {
        let absent = parse("schema_version=1\n[project]\nname=\"x\"\n").unwrap();
        assert!(absent.recall_enabled(), "no [features] table: on");
        let empty_table =
            parse("schema_version=1\n[project]\nname=\"x\"\n[features]\nsearch=true\n").unwrap();
        assert!(empty_table.recall_enabled(), "a table without the key: on");
        let explicit_on =
            parse("schema_version=1\n[project]\nname=\"x\"\n[features]\ntotal_recall=true\n")
                .unwrap();
        assert!(explicit_on.recall_enabled());
        assert_eq!(absent.policy_hash(), explicit_on.policy_hash());
        let declared_off =
            parse("schema_version=1\n[project]\nname=\"x\"\n[features]\ntotal_recall=false\n")
                .unwrap();
        assert!(
            !declared_off.recall_enabled(),
            "only a written false is off"
        );
        assert_ne!(absent.policy_hash(), declared_off.policy_hash());
        assert!(
            EffectiveConfig::secure_default().unwrap().recall_enabled(),
            "the no-.rift default agrees with the absent-key default"
        );
        assert!(FeaturePolicy::default().total_recall);
    }

    /// The retired spelling loads and asks to be renamed exactly once; the
    /// current spelling and an absent `[features]` say nothing. A repository
    /// configured before the rename therefore keeps working for this release
    /// and still finds out.
    #[test]
    fn the_retired_search_spelling_asks_for_the_rename_and_the_current_one_is_silent() {
        let notice = parse("schema_version=1\n[project]\nname=\"x\"\n[features]\nfff=false\n")
            .unwrap()
            .retired_feature_key_notice()
            .expect("the retired spelling is present, so it is reported");
        assert!(notice.contains("fff"), "names the key to rename: {notice}");
        assert!(notice.contains("search"), "names the new key: {notice}");
        assert!(
            notice.contains(".rift/config.toml"),
            "names the file to edit: {notice}"
        );
        for quiet in [
            "schema_version=1\n[project]\nname=\"x\"\n[features]\nsearch=true\n",
            "schema_version=1\n[project]\nname=\"x\"\n[features]\ntotal_recall=true\n",
            "schema_version=1\n[project]\nname=\"x\"\n",
        ] {
            assert!(
                parse(quiet).unwrap().retired_feature_key_notice().is_none(),
                "nothing to rename: {quiet:?}"
            );
        }
    }

    /// The graph-analysis feature key is not renamed, it is refused: the
    /// packages behind it were deleted by owner decision, so accepting policy
    /// for it would be accepting policy for something that cannot exist. The
    /// error names the offending key, which is what makes "delete the line" a
    /// fix a reader can perform without this crate's source.
    /// The idle sweep's floor: five minutes when absent, what the file says
    /// when present, and zero refused at load, because a zero floor would end
    /// every session between two of its own hooks. It leaves the policy hash
    /// alone: it moves when a session is finalized, never what is captured.
    #[test]
    fn the_idle_sweep_floor_defaults_to_five_minutes_and_refuses_zero() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(
            idle_sweep_ms(tmp.path()),
            5 * 60_000,
            "no file: the default"
        );
        let head = "schema_version = 1\n[project]\nname = \"x\"\n";
        let one = format!("{head}[capture]\nidle_sweep_minutes = 1\n");
        std::fs::write(tmp.path().join("config.toml"), &one).unwrap();
        assert_eq!(idle_sweep_ms(tmp.path()), 60_000);
        assert_eq!(
            parse(&one).unwrap().policy_hash(),
            parse(head).unwrap().policy_hash()
        );
        let zero = format!("{head}[capture]\nidle_sweep_minutes = 0\n");
        assert!(matches!(parse(&zero), Err(RiftError::Config(_))));
        std::fs::write(tmp.path().join("config.toml"), &zero).unwrap();
        assert_eq!(
            idle_sweep_ms(tmp.path()),
            5 * 60_000,
            "a refused file: the default"
        );
    }

    #[test]
    fn the_deleted_graph_feature_key_no_longer_parses() {
        let err = parse("schema_version=1\n[project]\nname=\"x\"\n[features]\ncodegraph=true\n")
            .expect_err("the key was removed from the accepted set")
            .to_string();
        assert!(err.contains("codegraph"), "names the refused key: {err}");
    }

    proptest::proptest! {
        /// Same logical config (any vec order) → identical policy hash.
        #[test]
        fn effective_policy_is_deterministic(a in "[a-z]{1,6}", b in "[a-z]{1,6}") {
            proptest::prop_assume!(a != b);
            let one = parse(&format!(
                "schema_version=1\n[project]\nname=\"p\"\n[models]\nallow=[\"{a}\",\"{b}\"]\n"
            )).unwrap();
            let two = parse(&format!(
                "schema_version=1\n[project]\nname=\"p\"\n[models]\nallow=[\"{b}\",\"{a}\"]\n"
            )).unwrap();
            proptest::prop_assert_eq!(one.policy_hash(), two.policy_hash());
        }
    }
}
