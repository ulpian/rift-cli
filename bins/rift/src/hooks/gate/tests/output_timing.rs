//! The hook-path cost of matching a terminal result, timed at the four
//! output sizes the budget is stated at: a measurement instrument, run by
//! hand in a release build, never a verdict.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Instant;

use rift_shadow::hooks::AdapterRegistry;

use super::fixtures::{hook, observing};
use crate::hooks::gate::output_match::output_match_record;

/// The output sizes, in characters, and how many samples each gets.
const BANDS: [usize; 4] = [1_000, 4_000, 16_000, 29_000];
const SAMPLES_PER_SHAPE: usize = 60;

/// Every `.rs` file under `dir`, as paths relative to `root`, sorted.
fn sources(root: &Path, dir: &Path, out: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            sources(root, &path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            let rel = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            out.push(rel);
        }
    }
}

/// A copy of a slice of this repository's own sources in a fresh checkout.
fn corpus_checkout() -> (tempfile::TempDir, Vec<String>) {
    let tmp = tempfile::tempdir().unwrap();
    rift_git::test_support::fake_repo(tmp.path()).unwrap();
    let here = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../crates/rift-agent");
    let mut files = Vec::new();
    sources(&here, &here.join("src"), &mut files);
    for rel in &files {
        let to = tmp.path().join(rel);
        std::fs::create_dir_all(to.parent().unwrap()).unwrap();
        std::fs::copy(here.join(rel), to).unwrap();
    }
    (tmp, files)
}

/// One sample of `shape` at about `chars` characters: the command and the
/// output it printed.
fn sample(
    root: &Path,
    files: &[String],
    shape: usize,
    chars: usize,
    seed: usize,
) -> (String, String) {
    let mut out = String::new();
    let mut named = Vec::new();
    let mut at = seed;
    while out.len() < chars {
        let rel = &files[at % files.len()];
        at += 7;
        let text = std::fs::read_to_string(root.join(rel)).unwrap();
        named.push(rel.clone());
        for (n, line) in text.lines().enumerate() {
            if out.len() >= chars {
                break;
            }
            match shape {
                0 => writeln!(out, "{:>6}\t{line}", n + 1).unwrap(),
                1 => writeln!(out, "{rel}:{}:{line}", n + 1).unwrap(),
                _ => writeln!(out, "{line}").unwrap(),
            }
        }
    }
    let command = match shape {
        0 => format!("cat -n {} | head -2000", named.join(" ")),
        1 => "grep -rn . src/ | head -2000".to_owned(),
        _ => named
            .iter()
            .map(|f| format!("cat {f} | head -400"))
            .collect::<Vec<_>>()
            .join(" && "),
    };
    (command, out)
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    let at = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted.get(at).copied().unwrap_or_default()
}

/// The p50/p95/max of the whole `PostToolUse` match step per band: the
/// files named or printed hashed as the gate hashes them, line hashes from
/// the bytes (no cache), the match, the alignment and the appends.
#[test]
#[ignore = "measurement instrument: times the output match on this machine's own sources"]
fn the_output_match_fits_the_hook_budget() {
    let (tmp, files) = corpus_checkout();
    let root = tmp.path();
    let registry = AdapterRegistry::with_builtin();
    let mut report = String::from("| band (chars) | n | p50 ms | p95 ms | max ms | rows written |\n|---|---|---|---|---|---|\n");
    for chars in BANDS {
        let mut times = Vec::new();
        let mut written = 0usize;
        for shape in 0..3 {
            for seed in 0..SAMPLES_PER_SHAPE {
                let (command, stdout) = sample(root, &files, shape, chars, seed * 13 + shape);
                let payload = serde_json::json!({
                    "tool_name": "Bash",
                    "cwd": root.display().to_string(),
                    "tool_input": { "command": command },
                    "tool_response": { "stdout": stdout, "stderr": "", "interrupted": false },
                });
                let raw = hook(
                    "claude-code",
                    "PostToolUse",
                    &format!("t-{chars}-{shape}-{seed}"),
                    payload,
                );
                let start = Instant::now();
                written += usize::from(output_match_record(
                    &registry,
                    &raw,
                    &root.join(".git"),
                    root,
                    &observing(),
                ));
                times.push(start.elapsed().as_secs_f64() * 1000.0);
            }
        }
        times.sort_by(f64::total_cmp);
        writeln!(
            report,
            "| {chars} | {} | {:.2} | {:.2} | {:.2} | {written} |",
            times.len(),
            percentile(&times, 0.5),
            percentile(&times, 0.95),
            percentile(&times, 1.0),
        )
        .unwrap();
    }
    eprintln!("{report}");
    if let Ok(out) = std::env::var("RIFT_OUTPUT_TIMING_OUT") {
        std::fs::write(out, &report).unwrap();
    }
}
