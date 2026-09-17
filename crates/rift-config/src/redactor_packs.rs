//! Shareable redactor rule packs — `.rift/policies/redactors/**/*.{toml,json}`.
//!
//! A pack is a versioned bundle of extra secret patterns a team can commit and
//! share; a personal pack drops into the same tree (conventionally `local/`,
//! gitignored) and loads the same way. Packs are **additive only**: they add
//! detectors to the pipeline and can never disable a built-in one, which is why
//! loading them is safe to make this permissive.
//!
//! Nothing here can fail capture. A missing directory, an unreadable file, a
//! malformed pack, a rule with a regex this build cannot compile — each is
//! reported as a [`PackDiagnostic`] and skipped, never raised. The reason is the
//! hook contract: redaction runs on the capture path, and a policy file a user
//! is editing must never be able to stop a commit. The failure mode is *fewer
//! extra rules*, and the built-in pipeline is untouched underneath.
//!
//! This module parses and validates; it does **not** compile regexes or run the
//! samples. That happens in `rift-shadow-redact`, which owns redaction
//! semantics — so a pack's rules are proven against the real pipeline rather
//! than against a second copy of it here.
//!
//! Diagnostics carry paths, ids, and lengths. They never carry a rule's sample
//! input: a sample is exactly the shape of a secret, and a diagnostic is a log
//! line.
//!
//! The wire contract this implements is the **Redactor rule pack** entry in
//! `docs/sync-format.md`, which is the spec and not a description — where the
//! two disagreed at port time the code was changed. One such disagreement was
//! found and is worth naming, because it made a cap that reads like a guard not
//! be one: `MAX_PACK_FILES` used to bound the packs that *loaded*, so a
//! directory of a hundred thousand malformed files was parsed in full and the
//! cap never fired (`the_file_cap_counts_files_and_not_successful_packs`).

use std::path::{Path, PathBuf};

/// The pack schema version this build understands. A pack declaring anything
/// else is skipped with a diagnostic rather than guessed at.
pub const REDACTOR_PACK_SCHEMA_VERSION: u32 = 1;

/// The directory under `.rift/policies/` that packs are read from.
pub const REDACTORS_DIR_NAME: &str = "redactors";

/// Per-file size cap. Not a security barrier — the trust boundary is "the user
/// owns the repo" — but a runaway-input guard that keeps an accidentally huge
/// file out of the capture path.
const MAX_PACK_FILE_BYTES: u64 = 1 << 20;

/// How many pack **files** one tree may contribute. Counted over candidate files
/// and not over successful packs, so the guard bounds the work that is actually
/// expensive — reading and parsing — rather than the outcome of it. The walk
/// still gathers and sorts every candidate path first, because which 256 files
/// load has to be the same answer on two machines and directory order is not.
const MAX_PACK_FILES: usize = 256;

/// How deep the walk descends below the pack root before it stops looking. The
/// root itself is level 0, so eight levels of subdirectory are read and the
/// ninth is refused with a diagnostic.
const MAX_DEPTH: usize = 8;

/// Length cap for a pack name / rule id, in characters. Both flow into
/// diagnostics and into detector ids in the redaction report; bounding them
/// keeps those readable.
const MAX_IDENTIFIER_LEN: usize = 64;

/// A versioned bundle of redaction rules, loaded from one file.
#[derive(Debug, Clone, ::serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RedactorPack {
    /// Schema version of the pack file itself.
    pub schema_version: u32,
    /// Pack name. Must equal the file's stem, so a rule id in a report names a
    /// file you can open.
    pub name: String,
    /// Free-form version of the *rules*, surfaced in `report.rule_pack_versions`.
    pub version: String,
    /// What this pack is for (diagnostics only).
    #[serde(default)]
    pub description: String,
    /// At least one rule; a pack with no rules is a mistake worth reporting.
    pub rules: Vec<RedactorRule>,
    /// The file this pack came from. Filled in by the loader, never parsed:
    /// `deny_unknown_fields` means a file that tries to set it is rejected.
    #[serde(skip)]
    pub source: PathBuf,
}

/// One rule: an id, a pattern, and the samples that prove the pattern does what
/// its author thinks.
#[derive(Debug, Clone, ::serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RedactorRule {
    /// Unique within the pack. Namespaced as `<pack>.<id>` in the report.
    pub id: String,
    /// What the rule catches (diagnostics only).
    #[serde(default)]
    pub description: String,
    /// The pattern, compiled by `rift-shadow-redact` — not here.
    pub regex: String,
    /// Self-tests. A failing sample warns; it never drops the rule and never
    /// fails the load.
    #[serde(default)]
    pub samples: Vec<RedactorSample>,
}

/// A rule's self-test: does this rule match this input, yes or no?
#[derive(Debug, Clone, ::serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RedactorSample {
    /// The text to run the rule against.
    pub input: String,
    /// Whether the rule is expected to match it.
    pub redacted: bool,
}

/// Something the loader chose to skip, kept as data instead of raised.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackDiagnostic {
    /// The file (or directory) the decision was made about.
    pub path: PathBuf,
    /// Why it was skipped. Never contains sample text.
    pub reason: String,
}

/// What one directory yielded: the packs that loaded, and every decision that
/// was made about the ones that did not.
#[derive(Debug, Clone, Default)]
pub struct LoadedPacks {
    /// Packs in a deterministic (path-sorted) order, so two machines reading the
    /// same tree build the same pipeline.
    pub packs: Vec<RedactorPack>,
    /// One entry per skipped file, in the order the decisions were made.
    pub diagnostics: Vec<PackDiagnostic>,
}

/// Where packs live under a project's `.rift/` directory, so no caller has to
/// spell the path out and drift from it.
#[must_use]
pub fn redactor_pack_dir(rift_dir: &Path) -> PathBuf {
    rift_dir.join("policies").join(REDACTORS_DIR_NAME)
}

/// Load every pack under `dir`, recursively.
///
/// Infallible by construction: a missing directory is "no packs configured",
/// and everything else that goes wrong becomes a [`PackDiagnostic`]. See the
/// module docs for why the capture path cannot afford any other contract.
#[must_use]
pub fn load_redactor_packs(dir: &Path) -> LoadedPacks {
    let mut out = LoadedPacks::default();
    if !dir.exists() {
        return out;
    }
    let mut files = Vec::new();
    collect_files(dir, 0, &mut files, &mut out.diagnostics);
    files.sort();
    let total = files.len();
    let mut names: Vec<String> = Vec::new();
    for (index, path) in files.into_iter().enumerate() {
        if index >= MAX_PACK_FILES {
            // One diagnostic, not one per file: a runaway directory must not
            // produce a runaway diagnostic list either.
            out.diagnostics.push(PackDiagnostic {
                path: dir.to_path_buf(),
                reason: format!(
                    "{} pack files past the {MAX_PACK_FILES}-file cap were not read",
                    total - index
                ),
            });
            break;
        }
        match parse_pack(&path) {
            Ok(pack) => {
                if names.iter().any(|n| n == &pack.name) {
                    // Required by the report contract, not merely tidy: two packs
                    // under one name would mint the same `<pack>.<rule>` detector
                    // id twice, and an id in a report has to name exactly one
                    // detector.
                    out.diagnostics.push(PackDiagnostic {
                        path,
                        reason: format!("duplicate pack name `{}`", pack.name),
                    });
                    continue;
                }
                names.push(pack.name.clone());
                out.packs.push(pack);
            }
            Err(reason) => out.diagnostics.push(PackDiagnostic { path, reason }),
        }
    }
    out
}

/// Gather candidate pack files, deepest-first order irrelevant (the caller
/// sorts). Symlinks are never followed: a pack tree is repo content, and a
/// symlink out of it is a surprise, not a feature.
fn collect_files(
    dir: &Path,
    depth: usize,
    files: &mut Vec<PathBuf>,
    diagnostics: &mut Vec<PackDiagnostic>,
) {
    if depth > MAX_DEPTH {
        diagnostics.push(PackDiagnostic {
            path: dir.to_path_buf(),
            reason: format!("directory nested deeper than {MAX_DEPTH} levels"),
        });
        return;
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) => {
            diagnostics.push(PackDiagnostic {
                path: dir.to_path_buf(),
                reason: format!("unreadable directory: {e}"),
            });
            return;
        }
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = entry.file_type() else {
            diagnostics.push(PackDiagnostic {
                path,
                reason: "unreadable directory entry".to_string(),
            });
            continue;
        };
        if meta.is_symlink() {
            diagnostics.push(PackDiagnostic {
                path,
                reason: "symlinked pack path skipped".to_string(),
            });
            continue;
        }
        if meta.is_dir() {
            collect_files(&path, depth + 1, files, diagnostics);
            continue;
        }
        match extension(&path).as_deref() {
            Some("toml" | "json") => files.push(path),
            Some("yaml" | "yml") => diagnostics.push(PackDiagnostic {
                path,
                reason: "yaml packs are not supported by this build (the workspace pins no yaml \
                         parser; use .toml or .json)"
                    .to_string(),
            }),
            _ => {}
        }
    }
}

fn extension(path: &Path) -> Option<String> {
    path.extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
}

/// Read, decode and validate one pack file. The error is a reason string
/// because every caller turns it into a diagnostic — there is no path on which
/// a bad pack becomes an error the process has to handle.
fn parse_pack(path: &Path) -> Result<RedactorPack, String> {
    let size = std::fs::metadata(path)
        .map_err(|e| format!("stat failed: {e}"))?
        .len();
    if size > MAX_PACK_FILE_BYTES {
        return Err(format!(
            "file is {size} bytes, over the {MAX_PACK_FILE_BYTES}-byte cap"
        ));
    }
    let text = std::fs::read_to_string(path).map_err(|e| format!("unreadable: {e}"))?;
    let mut pack: RedactorPack = match extension(path).as_deref() {
        Some("json") => serde_json::from_str(&text).map_err(|e| parse_reason(&e.to_string()))?,
        _ => toml::from_str(&text).map_err(|e| parse_reason(&e.to_string()))?,
    };
    pack.source = path.to_path_buf();
    validate(&pack, path)?;
    Ok(pack)
}

/// Longest a diagnostic reason may be.
const MAX_REASON_LEN: usize = 200;

/// Turn a parser message into a bounded diagnostic.
///
/// TOML's `Display` quotes the offending source line, and a pack line can hold a
/// `samples[].input` written to look exactly like a secret — so the quoted
/// snippet (the lines carrying the `|` gutter) is dropped and what remains,
/// position and reason, is length-bounded. What this does *not* claim: a
/// parser's own message may still name a value it choked on. That is accepted
/// deliberately — a pack is policy the user authored and its bytes already sit
/// in the working tree unredacted — and it is why the loader's own messages
/// never quote a sample.
fn parse_reason(text: &str) -> String {
    let kept: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.contains('|') && !l.starts_with('^'))
        .take(4)
        .collect();
    let reason = format!("parse: {}", kept.join("; "));
    if reason.chars().count() > MAX_REASON_LEN {
        return reason.chars().take(MAX_REASON_LEN).collect::<String>() + "…";
    }
    reason
}

fn validate(pack: &RedactorPack, path: &Path) -> Result<(), String> {
    if pack.schema_version != REDACTOR_PACK_SCHEMA_VERSION {
        return Err(format!(
            "schema_version {} is not {REDACTOR_PACK_SCHEMA_VERSION}",
            pack.schema_version
        ));
    }
    validate_identifier("name", &pack.name)?;
    if pack.version.is_empty() {
        return Err("missing `version`".to_string());
    }
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default();
    if stem != pack.name {
        return Err(format!(
            "pack name `{}` does not match the filename stem `{stem}`",
            pack.name
        ));
    }
    if pack.rules.is_empty() {
        return Err("`rules` must hold at least one rule".to_string());
    }
    let mut seen: Vec<&str> = Vec::with_capacity(pack.rules.len());
    for rule in &pack.rules {
        validate_identifier("rule id", &rule.id)?;
        if rule.regex.is_empty() {
            return Err(format!("rule `{}` has an empty `regex`", rule.id));
        }
        if seen.contains(&rule.id.as_str()) {
            return Err(format!("duplicate rule id `{}`", rule.id));
        }
        seen.push(&rule.id);
    }
    Ok(())
}

/// Names and ids end up in detector ids and diagnostics, so they are held to a
/// conservative character set: no whitespace, no control characters, nothing
/// that could forge a second field in a log line.
///
/// Charset before length, so a value outside the set is always reported as
/// such — the character count and the byte count only agree once the set is
/// known to be ASCII, and a message about the wrong one is a message that sends
/// its reader looking in the wrong place.
fn validate_identifier(field: &str, value: &str) -> Result<(), String> {
    if value.is_empty() {
        return Err(format!("missing `{field}`"));
    }
    if !value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    {
        return Err(format!(
            "{field} `{value}` has characters outside [A-Za-z0-9._-]"
        ));
    }
    if value.chars().count() > MAX_IDENTIFIER_LEN {
        return Err(format!(
            "{field} `{value}` is longer than {MAX_IDENTIFIER_LEN} characters"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, body: &str) {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("mkdir");
        }
        std::fs::write(path, body).expect("write pack");
    }

    fn acme_toml(name: &str) -> String {
        format!(
            r#"
schema_version = 1
name = "{name}"
version = "2026.08.0"
description = "internal token shapes"

[[rules]]
id = "internal_token"
description = "ACME-XXXXXXXX"
regex = "ACME-[A-Z0-9]{{8}}"

[[rules.samples]]
input = "ACME-AB12CD34"
redacted = true
"#
        )
    }

    #[test]
    fn loads_toml_and_json_packs_in_a_deterministic_order() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path();
        write(dir, "acme.toml", &acme_toml("acme"));
        write(
            dir,
            "beta.json",
            r#"{"schema_version":1,"name":"beta","version":"1","rules":[{"id":"r1","regex":"BETA-[0-9]+"}]}"#,
        );
        let loaded = load_redactor_packs(dir);
        assert!(loaded.diagnostics.is_empty(), "{:?}", loaded.diagnostics);
        let names: Vec<&str> = loaded.packs.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["acme", "beta"]);
        assert!(loaded.packs[0].rules[0].samples[0].redacted);
        assert_eq!(loaded.packs[0].source, dir.join("acme.toml"));
    }

    /// A personal, gitignored pack drops into a subdirectory and loads the same
    /// way a shared one does.
    #[test]
    fn personal_subdirectory_packs_are_discovered() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write(tmp.path(), "local/mine.toml", &acme_toml("mine"));
        let loaded = load_redactor_packs(tmp.path());
        assert_eq!(loaded.packs.len(), 1);
        assert_eq!(loaded.packs[0].name, "mine");
    }

    #[test]
    fn packs_live_under_the_policies_directory() {
        assert!(redactor_pack_dir(Path::new("/repo/.rift")).ends_with("policies/redactors"));
    }

    #[test]
    fn a_missing_directory_is_not_an_error() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let loaded = load_redactor_packs(&tmp.path().join("absent"));
        assert!(loaded.packs.is_empty());
        assert!(loaded.diagnostics.is_empty());
    }

    /// Every rejection is data, and one bad pack never silences the good ones.
    #[test]
    fn a_bad_pack_is_skipped_and_the_rest_still_load() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write(tmp.path(), "acme.toml", &acme_toml("acme"));
        write(
            tmp.path(),
            "broken.toml",
            "schema_version = 1\nname = \"broken\"\n",
        );
        let loaded = load_redactor_packs(tmp.path());
        assert_eq!(loaded.packs.len(), 1, "{:?}", loaded.diagnostics);
        assert_eq!(loaded.diagnostics.len(), 1);
        assert!(loaded.diagnostics[0].path.ends_with("broken.toml"));
    }

    #[test]
    fn unknown_fields_are_rejected() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let body = format!("{}\nextra_key = true\n", acme_toml("acme"));
        write(tmp.path(), "acme.toml", &body);
        let loaded = load_redactor_packs(tmp.path());
        assert!(loaded.packs.is_empty());
        assert_eq!(loaded.diagnostics.len(), 1);
    }

    #[test]
    fn the_pack_name_must_match_the_filename_stem() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write(tmp.path(), "other.toml", &acme_toml("acme"));
        let loaded = load_redactor_packs(tmp.path());
        assert!(loaded.packs.is_empty());
        assert!(loaded.diagnostics[0].reason.contains("filename stem"));
    }

    #[test]
    fn duplicate_pack_names_and_rule_ids_are_rejected() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write(tmp.path(), "acme.toml", &acme_toml("acme"));
        write(tmp.path(), "sub/acme.toml", &acme_toml("acme"));
        let loaded = load_redactor_packs(tmp.path());
        assert_eq!(loaded.packs.len(), 1);
        assert!(loaded.diagnostics[0].reason.contains("duplicate pack name"));

        let tmp2 = tempfile::tempdir().expect("tempdir");
        write(
            tmp2.path(),
            "dup.json",
            r#"{"schema_version":1,"name":"dup","version":"1","rules":[{"id":"r","regex":"A"},{"id":"r","regex":"B"}]}"#,
        );
        let loaded2 = load_redactor_packs(tmp2.path());
        assert!(loaded2.packs.is_empty());
        assert!(loaded2.diagnostics[0].reason.contains("duplicate rule id"));
    }

    #[test]
    fn identifiers_are_held_to_a_conservative_charset_and_length() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write(
            tmp.path(),
            "bad.json",
            r#"{"schema_version":1,"name":"bad","version":"1","rules":[{"id":"has space","regex":"A"}]}"#,
        );
        let loaded = load_redactor_packs(tmp.path());
        assert!(loaded.packs.is_empty());
        assert!(loaded.diagnostics[0].reason.contains("outside"));
        // The length cap is in characters, and the charset is checked first, so
        // a long-and-illegal id is reported for the reason that names the fix.
        assert!(validate_identifier("rule id", &"a".repeat(MAX_IDENTIFIER_LEN)).is_ok());
        let too_long = validate_identifier("rule id", &"a".repeat(MAX_IDENTIFIER_LEN + 1));
        assert!(too_long.is_err_and(|e| e.contains("longer than")));
        let both = validate_identifier("rule id", &"é".repeat(MAX_IDENTIFIER_LEN + 1));
        assert!(both.is_err_and(|e| e.contains("outside")));
        assert!(validate_identifier("name", "").is_err_and(|e| e.contains("missing")));
    }

    #[test]
    fn an_unknown_schema_version_is_skipped_not_guessed_at() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write(
            tmp.path(),
            "future.json",
            r#"{"schema_version":99,"name":"future","version":"1","rules":[{"id":"r","regex":"A"}]}"#,
        );
        let loaded = load_redactor_packs(tmp.path());
        assert!(loaded.packs.is_empty());
        assert!(loaded.diagnostics[0].reason.contains("schema_version"));
    }

    /// A format this build cannot parse is *reported*, never silently ignored —
    /// a pack a user believes is loaded but is not is the worst outcome here.
    #[test]
    fn yaml_packs_are_reported_rather_than_ignored() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write(tmp.path(), "team.yaml", "name: team\n");
        write(tmp.path(), "other.YML", "name: other\n");
        let loaded = load_redactor_packs(tmp.path());
        assert!(loaded.packs.is_empty());
        assert_eq!(loaded.diagnostics.len(), 2, "{:?}", loaded.diagnostics);
        assert!(loaded.diagnostics.iter().all(|d| d.reason.contains("yaml")));
    }

    #[test]
    fn oversized_files_are_skipped() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut body = acme_toml("big");
        body.push_str(&format!(
            "\n# {}\n",
            "x".repeat(MAX_PACK_FILE_BYTES as usize)
        ));
        write(tmp.path(), "big.toml", &body);
        let loaded = load_redactor_packs(tmp.path());
        assert!(loaded.packs.is_empty());
        assert!(loaded.diagnostics[0].reason.contains("cap"));
    }

    /// **The cap counts candidate files, not packs that loaded, and it reports
    /// itself once.** Bounding the outcome instead of the work let a directory
    /// of malformed files be parsed in full — the guard never fired, because
    /// `packs.len()` never grew.
    ///
    /// The fixture puts `MAX_PACK_FILES` unparsable files ahead of **three**
    /// valid packs, and the three are the load-bearing part. With one file past
    /// the cap, `break` and `continue` are indistinguishable — both emit a
    /// single diagnostic — so a one-file fixture cannot tell a bounded report
    /// from a per-file one however the count is asserted. Three makes the two
    /// readings differ: `break` says it once and names the three it skipped,
    /// `continue` says it three times, which is the runaway diagnostic list the
    /// cap exists to refuse.
    #[test]
    fn the_file_cap_counts_files_and_not_successful_packs() {
        let tmp = tempfile::tempdir().expect("tempdir");
        for i in 0..MAX_PACK_FILES {
            write(tmp.path(), &format!("{i:04}.toml"), "not a pack = [\n");
        }
        // Sorted after every numeric name, so all three sit past the cap.
        for name in ["zza", "zzb", "zzc"] {
            write(tmp.path(), &format!("{name}.toml"), &acme_toml(name));
        }
        let loaded = load_redactor_packs(tmp.path());
        assert!(
            loaded.packs.is_empty(),
            "a file past the cap was read: {:?}",
            loaded.packs.iter().map(|p| &p.name).collect::<Vec<_>>()
        );
        // **Exactly one**, not merely at least one. `any(…)` could not tell the
        // `break` from a `continue`; a verifier proved that by planting the
        // `continue` and watching the whole crate stay green at 46/0.
        let capped: Vec<&PackDiagnostic> = loaded
            .diagnostics
            .iter()
            .filter(|d| d.reason.contains("past the"))
            .collect();
        assert_eq!(
            capped.len(),
            1,
            "the cap must report itself exactly once, not once per file: {capped:?}"
        );
        // …and the count it names is the whole remainder, not what is left at
        // the moment it stopped. Nothing else pins that arithmetic.
        assert!(
            capped[0].reason.contains('3'),
            "the diagnostic must name the 3 files it skipped: {}",
            capped[0].reason
        );
    }

    /// The depth cap, pinned at the boundary so "8 levels" means one thing. The
    /// root is level 0: a pack eight directories down loads, and the ninth is
    /// refused with a diagnostic rather than silently missed.
    #[test]
    fn the_depth_cap_reads_eight_levels_and_refuses_the_ninth() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let deep = (1..=MAX_DEPTH).fold(String::new(), |mut a, i| {
            use std::fmt::Write as _;
            let _ = write!(a, "d{i}/");
            a
        });
        write(tmp.path(), &format!("{deep}ok.toml"), &acme_toml("ok"));
        let too_deep = (1..=MAX_DEPTH + 1).fold(String::new(), |mut a, i| {
            use std::fmt::Write as _;
            let _ = write!(a, "e{i}/");
            a
        });
        write(
            tmp.path(),
            &format!("{too_deep}nope.toml"),
            &acme_toml("nope"),
        );
        let loaded = load_redactor_packs(tmp.path());
        let names: Vec<&str> = loaded.packs.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["ok"], "{:?}", loaded.diagnostics);
        assert!(loaded
            .diagnostics
            .iter()
            .any(|d| d.reason.contains("nested deeper")));
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_paths_are_skipped() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write(tmp.path(), "real/acme.toml", &acme_toml("acme"));
        std::os::unix::fs::symlink(
            tmp.path().join("real/acme.toml"),
            tmp.path().join("link.toml"),
        )
        .expect("symlink");
        // A symlinked *directory* too: following one would read policy from
        // outside the work tree, which is injection and not a byte echo.
        std::os::unix::fs::symlink(tmp.path().join("real"), tmp.path().join("linkdir"))
            .expect("symlink");
        let loaded = load_redactor_packs(tmp.path());
        assert_eq!(loaded.packs.len(), 1);
        assert_eq!(
            loaded
                .diagnostics
                .iter()
                .filter(|d| d.reason.contains("symlink"))
                .count(),
            2
        );
    }

    /// A sample is shaped exactly like a secret, so no diagnostic may quote one.
    #[test]
    fn diagnostics_never_quote_sample_text() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let secret = "AKIAIOSFODNN7EXAMPLE";
        let body = format!(
            "schema_version = 1\nname = \"wrongstem\"\nversion = \"1\"\n\n[[rules]]\nid = \
             \"r\"\nregex = \"A\"\n\n[[rules.samples]]\ninput = \"{secret}\"\nredacted = true\n"
        );
        write(tmp.path(), "mismatch.toml", &body);
        // …and a parse failure on the same file, where TOML's own message quotes
        // the offending line.
        write(
            tmp.path(),
            "torn.toml",
            &format!("{body}\n[[rules.samples]\n"),
        );
        let loaded = load_redactor_packs(tmp.path());
        assert!(loaded.packs.is_empty());
        assert_eq!(loaded.diagnostics.len(), 2);
        for d in &loaded.diagnostics {
            assert!(
                !d.reason.contains(secret),
                "diagnostic quoted a sample: {d:?}"
            );
            assert!(d.reason.chars().count() <= MAX_REASON_LEN + 1);
        }
    }
}
