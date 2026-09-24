//! The screen that keeps every fuzz target on disk running in CI, and the
//! three list parsers it needs — one per spelling the hand-maintained lists
//! happen to use.
use std::path::Path;

use super::{list_violations, read};

/// The fuzz targets that actually exist, as `fuzz/fuzz_targets/<name>.rs` stems.
fn fuzz_targets_on_disk(root: &Path) -> Result<Vec<String>, String> {
    let rel = "fuzz/fuzz_targets";
    let mut names = Vec::new();
    for entry in std::fs::read_dir(root.join(rel)).map_err(|e| format!("read {rel}: {e}"))? {
        let path = entry.map_err(|e| format!("read {rel}: {e}"))?.path();
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
            names.push(stem.to_owned());
        }
    }
    names.sort();
    Ok(names)
}

/// The comma-separated names inside the first `[...]` after `key` (a YAML flow
/// sequence, which is how the nightly matrix spells its target list).
fn bracketed_list_after(text: &str, key: &str) -> Vec<String> {
    let Some((_, rest)) = text.split_once(key) else {
        return Vec::new();
    };
    let (Some(open), Some(close)) = (rest.find('['), rest.find(']')) else {
        return Vec::new();
    };
    let Some(inner) = open.checked_add(1).and_then(|from| rest.get(from..close)) else {
        return Vec::new();
    };
    inner
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

/// The words a `for t in … ; do` loop iterates — the smoke job's target list.
fn shell_loop_targets(text: &str) -> Vec<String> {
    text.lines()
        .find_map(|line| {
            let list = line
                .trim()
                .strip_prefix("for t in ")?
                .strip_suffix("; do")?;
            Some(list.split_whitespace().map(ToOwned::to_owned).collect())
        })
        .unwrap_or_default()
}

/// The `name = "…"` of every `[[bin]]` table in a manifest.
fn cargo_bin_names(text: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut in_bin = false;
    for raw in text.lines() {
        let line = raw.trim();
        if line.starts_with('[') {
            in_bin = line == "[[bin]]";
            continue;
        }
        if let (true, Some((key, value))) = (in_bin, line.split_once('=')) {
            if key.trim() == "name" {
                names.push(value.trim().trim_matches('"').to_owned());
            }
        }
    }
    names
}

/// Every fuzz target on disk is declared in `fuzz/Cargo.toml` and named by both
/// the PR smoke job and the nightly matrix — and neither workflow names one that
/// does not exist.
///
/// Both lists are hand-maintained, and `required_jobs_present` only asserts that
/// the *job* exists, not what it runs. So a new target used to land, pass review,
/// and then quietly never execute in CI: a fuzz target nobody runs is not a gate,
/// it is a file. This closes that by construction, in the direction that hides
/// cost — adding a target now fails CI until the lists catch up.
///
/// # Errors
/// If the fuzz target directory or either workflow cannot be read.
pub fn fuzz_targets_all_run_in_ci(root: &Path) -> Result<Vec<String>, String> {
    let on_disk = fuzz_targets_on_disk(root)?;
    let manifest = cargo_bin_names(&read(root, "fuzz/Cargo.toml")?);
    let smoke = shell_loop_targets(&read(root, ".github/workflows/ci.yml")?);
    let nightly = bracketed_list_after(
        &read(root, ".github/workflows/fuzz-nightly.yml")?,
        "target:",
    );
    let mut v = Vec::new();
    for (label, listed) in [
        ("fuzz/Cargo.toml declares no [[bin]] for", &manifest),
        ("ci.yml fuzz-smoke does not run", &smoke),
        ("fuzz-nightly.yml matrix does not run", &nightly),
    ] {
        v.extend(list_violations(label, &on_disk, listed));
    }
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ci_meta::tests::scratch_dir;
    use crate::workspace_root;

    /// The three hand-maintained lists agree with the directory, spelled as the
    /// equality it is — so a failure names the drifting list, not just "green".
    #[test]
    fn every_fuzz_list_matches_the_targets_on_disk() {
        let root = workspace_root();
        let on_disk = fuzz_targets_on_disk(&root).expect("fuzz targets read");
        assert!(
            !on_disk.is_empty(),
            "no fuzz targets found — check the path"
        );
        let sorted = |mut names: Vec<String>| {
            names.sort();
            names
        };
        let read_at = |rel: &str| std::fs::read_to_string(root.join(rel)).expect("read");
        assert_eq!(
            sorted(cargo_bin_names(&read_at("fuzz/Cargo.toml"))),
            on_disk,
            "fuzz/Cargo.toml [[bin]] tables"
        );
        assert_eq!(
            sorted(shell_loop_targets(&read_at(".github/workflows/ci.yml"))),
            on_disk,
            "ci.yml fuzz-smoke loop"
        );
        assert_eq!(
            sorted(bracketed_list_after(
                &read_at(".github/workflows/fuzz-nightly.yml"),
                "target:"
            )),
            on_disk,
            "fuzz-nightly.yml matrix"
        );
    }

    /// Non-vacuity, in both directions: the check must catch a target that no
    /// list runs *and* a list naming a target that does not exist. Without this
    /// the gate could pass by parsing nothing at all.
    #[test]
    fn an_unrun_target_and_a_phantom_entry_are_both_violations() {
        let on_disk = vec!["diff".to_owned(), "redact".to_owned()];
        assert!(list_violations("l", &on_disk, &on_disk).is_empty());

        let missing = list_violations("l", &on_disk, &["diff".to_owned()]);
        assert_eq!(missing.len(), 1, "{missing:?}");
        assert!(
            missing.iter().any(|m| m.contains("`redact`")),
            "{missing:?}"
        );

        let phantom = list_violations(
            "l",
            &on_disk,
            &["diff".to_owned(), "redact".to_owned(), "gone".to_owned()],
        );
        assert_eq!(phantom.len(), 1, "{phantom:?}");
        assert!(phantom.iter().any(|p| p.contains("does not exist")));
    }

    /// Each list has its own spelling; a parser that silently returns nothing
    /// would make the gate vacuous, so pin all three shapes.
    #[test]
    fn each_fuzz_list_spelling_parses() {
        assert_eq!(
            shell_loop_targets("  run: |\n    for t in a b c; do\n      cargo fuzz\n"),
            vec!["a", "b", "c"]
        );
        assert!(shell_loop_targets("for t in a b c\n").is_empty());
        assert_eq!(
            bracketed_list_after(
                "matrix:\n  target:\n    [\n      a,\n      b,\n    ]\n",
                "target:"
            ),
            vec!["a", "b"]
        );
        assert!(bracketed_list_after("nothing here", "target:").is_empty());
        assert_eq!(
            cargo_bin_names(
                "[package]\nname = \"skip-me\"\n\n[[bin]]\nname = \"a\"\ntest = false\n"
            ),
            vec!["a"]
        );
    }

    /// The screen itself over a planted tree, not only its list comparison: a
    /// target the nightly matrix leaves out is reported against that list, and
    /// a smoke loop naming a target with no source file is reported as a
    /// phantom.
    #[test]
    fn a_target_the_nightly_matrix_skips_and_a_phantom_smoke_entry_are_reported() {
        let root = scratch_dir("fuzz-lists");
        let targets = root.join("fuzz").join("fuzz_targets");
        let workflows = root.join(".github").join("workflows");
        std::fs::create_dir_all(&targets).unwrap();
        std::fs::create_dir_all(&workflows).unwrap();
        for name in ["diff", "redact"] {
            std::fs::write(targets.join(format!("{name}.rs")), "").unwrap();
        }
        std::fs::write(
            root.join("fuzz").join("Cargo.toml"),
            "[[bin]]\nname = \"diff\"\n\n[[bin]]\nname = \"redact\"\n",
        )
        .unwrap();
        std::fs::write(
            workflows.join("ci.yml"),
            "      for t in diff redact gone; do\n",
        )
        .unwrap();
        std::fs::write(
            workflows.join("fuzz-nightly.yml"),
            "matrix:\n  target: [diff]\n",
        )
        .unwrap();

        let violations = fuzz_targets_all_run_in_ci(&root).expect("the screen runs");
        assert_eq!(
            violations,
            vec![
                "a fuzz list names `gone`, but fuzz/fuzz_targets/gone.rs does not exist".to_owned(),
                "fuzz-nightly.yml matrix does not run fuzz target `redact` — a target nobody runs \
                 is not a gate"
                    .to_owned(),
            ]
        );
        std::fs::remove_dir_all(&root).unwrap();
    }
}
