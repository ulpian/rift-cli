//! The helpers every test file reaches for: redact a string and read the
//! bytes back, build a one-rule pack, and walk the workspace's text files.

use rift_core::SecretBytes;

use crate::{redact, redact_with, PackRule, PackSample, PackSpec, RedactionOutput, RulePackSet};

pub(super) fn out(s: &str) -> RedactionOutput {
    redact(&SecretBytes::from(s)).unwrap()
}

pub(super) fn masked(s: &str) -> String {
    String::from_utf8(out(s).redacted.as_slice().to_vec()).unwrap()
}

/// `masked`, through an explicit pack set instead of the builtin one.
pub(super) fn masked_with(s: &str, packs: &RulePackSet) -> String {
    let o = redact_with(&SecretBytes::from(s), packs).unwrap();
    String::from_utf8(o.redacted.as_slice().to_vec()).unwrap()
}

pub(super) fn pack(id: &str, regex: &str, samples: Vec<(&str, bool)>) -> PackSpec {
    PackSpec {
        name: "acme".to_string(),
        version: "2026.08.0".to_string(),
        rules: vec![PackRule {
            id: id.to_string(),
            regex: regex.to_string(),
            samples: samples
                .into_iter()
                .map(|(input, redacted)| PackSample {
                    input: input.to_string(),
                    redacted,
                })
                .collect(),
        }],
    }
}

pub(super) fn workspace_root() -> std::path::PathBuf {
    // crates/rift-shadow-redact → crates → the workspace root
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("workspace root")
        .to_path_buf()
}

/// Walk every tracked or unignored text file under `dir`.
///
/// `--others --exclude-standard` keeps a newly-created source file inside
/// the audit while respecting the repository and per-checkout ignore rules.
/// That distinction matters for this source-tree probe: ignored local PRDs
/// and alternate Cargo target directories are not repository content and
/// can legitimately contain the detector fixtures or numeric build ids the
/// probe is designed to find.
pub(super) fn visit_text_files(dir: &std::path::Path, f: &mut impl FnMut(&std::path::Path, &str)) {
    const TEXT: [&str; 11] = [
        "rs", "toml", "lock", "md", "json", "snap", "txt", "yml", "yaml", "sh", "sql",
    ];
    let tracked = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ])
        .output();
    if let Ok(output) = tracked {
        if output.status.success() {
            for relative in output.stdout.split(|byte| *byte == 0) {
                if relative.is_empty() {
                    continue;
                }
                let path = dir.join(String::from_utf8_lossy(relative).as_ref());
                if path
                    .extension()
                    .is_some_and(|e| TEXT.iter().any(|t| e == *t))
                {
                    if let Ok(contents) = std::fs::read_to_string(&path) {
                        f(&path, &contents);
                    }
                }
            }
            return;
        }
    }

    // A packaged source tree may not have Git metadata. Preserve the old
    // filesystem walk as a conservative fallback for that environment.
    visit_text_files_unfiltered(dir, f);
}

fn visit_text_files_unfiltered(dir: &std::path::Path, f: &mut impl FnMut(&std::path::Path, &str)) {
    const TEXT: [&str; 11] = [
        "rs", "toml", "lock", "md", "json", "snap", "txt", "yml", "yaml", "sh", "sql",
    ];
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path
                .file_name()
                .is_some_and(|n| n == "target" || n == ".git" || n == "node_modules")
            {
                continue;
            }
            visit_text_files_unfiltered(&path, f);
        } else if path
            .extension()
            .is_some_and(|e| TEXT.iter().any(|t| e == *t))
        {
            if let Ok(contents) = std::fs::read_to_string(&path) {
                f(&path, &contents);
            }
        }
    }
}
