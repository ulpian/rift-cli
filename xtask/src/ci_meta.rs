//! CI/lint meta-checks: guards on the guards. Each check returns the list of
//! violations (empty ⇒ pass); the `ci-meta` command fails if any check
//! reports one.
//!
//! * `required_jobs_present` — the workflow still declares every gate job
//!   (including `docs`, the zero-baseline rustdoc gate, and
//!   `dialect-conformance`).
//! * `workflow_permissions_read_only` — `permissions: contents: read`, no writes.
//! * `lint_table_matches_baseline` — `[workspace.lints.*]` vs
//!   `ci/lint-baseline.txt`, so a silently dropped or downgraded lint fails
//!   CI (the drift this guard was written for was `indexing_slicing` quietly
//!   losing its deny).
//! * `unsafe_only_in_sanctioned_modules` — no `unsafe` outside an allowlist
//!   (empty today; the workspace is `#![forbid(unsafe_code)]`).
//! * `no_blanket_allow_at_crate_root` — crate roots carry no `#![allow(...)]`
//!   of workspace-managed lints; narrow, justified allows live at the site.
//! * `no_schema_fastpath_in_bins` — production code opens projections through
//!   the checksum-locked migration engine, never `create_canonical_schema`.
//! * `fuzz_targets_all_run_in_ci` — every `fuzz/fuzz_targets/*.rs` is declared
//!   in the fuzz manifest and named by both fuzz workflows.
//! * `dialect_conformance_job_runs_every_suite` — the `dialect-conformance`
//!   job's `--test` set equals `DIALECT_SUITES`, so a dialect suite cannot be
//!   dropped from the named job while the job stays green.
//! * `fuzzy_matcher_reads_within_bounds` — the fuzzy matcher's workspace pin
//!   still carries the feature that keeps its SIMD backends inside the bytes
//!   they were given.
//! * `real_model_is_opt_in` — the paid model transport is a compile-time
//!   opt-in: no manifest enables `real-model` through a dependency's
//!   `features` array or a `default` feature.
//! * `retired_vocabulary_screen` — no tracked path or tracked byte carries a
//!   retired program identifier; citations name their rationale in plain
//!   words instead of pointing at a document that no longer exists.
//! * `no_network_crate_in_the_resolved_graph` — the default-resolved
//!   dependency graph of both shipped binaries names no HTTP client; the
//!   in-workspace manifest screen cannot see a client hidden behind an
//!   external intermediary, this one can (fails closed on empty output).
//!
//! `REQUIRED_JOBS` is why a job cannot be quietly dropped from the workflow:
//! the jobs enforce the invariants, and this list enforces the jobs.

use std::path::Path;

/// The jobs the CI workflow must always declare.
const REQUIRED_JOBS: &[&str] = &[
    "fmt:",
    "clippy:",
    "test:",
    "msrv:",
    "coverage:",
    "security:",
    "miri:",
    "fuzz-smoke:",
    "red-team:",
    "dialect-conformance:",
    "docs:",
    "ci-meta:",
    "gate-intercept:",
    "bench-harness:",
];

fn read(root: &Path, rel: &str) -> Result<String, String> {
    std::fs::read_to_string(root.join(rel)).map_err(|e| format!("read {rel}: {e}"))
}

/// Every required job name appears in `.github/workflows/ci.yml`.
///
/// # Errors
/// If the workflow file cannot be read.
pub fn required_jobs_present(root: &Path) -> Result<Vec<String>, String> {
    let ci = read(root, ".github/workflows/ci.yml")?;
    Ok(REQUIRED_JOBS
        .iter()
        .filter(|j| !ci.contains(&format!("\n  {j}")))
        .map(|j| {
            format!(
                "ci.yml: required job `{}` is missing",
                j.trim_end_matches(':')
            )
        })
        .collect())
}

/// The workflow grants read-only permissions and never requests a write scope.
///
/// # Errors
/// If the workflow file cannot be read.
pub fn workflow_permissions_read_only(root: &Path) -> Result<Vec<String>, String> {
    let ci = read(root, ".github/workflows/ci.yml")?;
    let mut v = Vec::new();
    if !ci.contains("permissions:\n  contents: read") {
        v.push("ci.yml: top-level `permissions: contents: read` is missing".to_string());
    }
    if ci.contains(": write") || ci.contains("write-all") {
        v.push("ci.yml: a write permission scope crept in".to_string());
    }
    Ok(v)
}

/// Normalize a lint-table body: keep `[section]` headers and `key = value`
/// lines, dropping comments and blanks.
fn normalized_lint_lines(text: &str, sections: &[&str]) -> Vec<String> {
    let mut out = Vec::new();
    let mut keep = false;
    for raw in text.lines() {
        let line = raw.trim();
        if line.starts_with('[') {
            keep = sections.iter().any(|s| line == format!("[{s}]"));
            if keep {
                out.push(line.to_string());
            }
            continue;
        }
        if keep && !line.is_empty() && !line.starts_with('#') {
            out.push(line.to_string());
        }
    }
    out
}

/// `[workspace.lints.rust]` + `[workspace.lints.clippy]` in the root manifest
/// match `ci/lint-baseline.txt` exactly (order-sensitive: the table is policy).
///
/// # Errors
/// If either file cannot be read.
pub fn lint_table_matches_baseline(root: &Path) -> Result<Vec<String>, String> {
    let sections = ["workspace.lints.rust", "workspace.lints.clippy"];
    let manifest = normalized_lint_lines(&read(root, "Cargo.toml")?, &sections);
    let baseline = normalized_lint_lines(
        &read(root, "ci/lint-baseline.txt")?,
        &["workspace.lints.rust", "workspace.lints.clippy"],
    );
    if manifest == baseline {
        return Ok(Vec::new());
    }
    let mut v = vec![
        "workspace lint table diverges from ci/lint-baseline.txt (change both, with rationale):"
            .to_string(),
    ];
    for line in &manifest {
        if !baseline.contains(line) {
            v.push(format!("  manifest has: {line}"));
        }
    }
    for line in &baseline {
        if !manifest.contains(line) {
            v.push(format!("  baseline has: {line}"));
        }
    }
    Ok(v)
}

/// Walk all first-party `.rs` files under `crates/`, `bins/`, `xtask/`.
///
/// `pub(crate)` so that every guard here shares ONE definition of
/// "first-party": two walkers with two definitions is how a file ends up
/// guarded by neither.
pub(crate) fn source_files(root: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let mut stack: Vec<std::path::PathBuf> = ["crates", "bins", "xtask"]
        .iter()
        .map(|d| root.join(d))
        .collect();
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                if p.file_name().is_some_and(|n| n == "target") {
                    continue;
                }
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push(p);
            }
        }
    }
    out
}

/// Every path Git tracks under `root`, relative, in `git ls-files -z` order.
///
/// ONE walker for every "all tracked files" screen, so a root document
/// (`README.md`, `.gitignore`, a workflow) is either visible to all of them
/// or to none — never to one screen and not another. The walk is the index,
/// not the filesystem: an untracked scratch file is not scanned, a tracked
/// file that has been deleted on disk still is (and the caller's read reds).
///
/// # Errors
/// If `git ls-files` fails or returns a non-UTF-8 path.
pub(crate) fn tracked_paths(root: &Path) -> Result<Vec<String>, String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["ls-files", "-z"])
        .output()
        .map_err(|e| format!("git ls-files (tracked-path walk): {e}"))?;
    if !output.status.success() {
        return Err("git ls-files (tracked-path walk) failed".to_owned());
    }
    output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|p| !p.is_empty())
        .map(|raw| {
            String::from_utf8(raw.to_vec())
                .map_err(|_| "git ls-files returned a non-UTF-8 path".to_owned())
        })
        .collect()
}

/// No `unsafe` code outside the (currently empty) sanctioned-module allowlist.
///
/// # Errors
/// If a source file cannot be read.
pub fn unsafe_only_in_sanctioned_modules(root: &Path) -> Result<Vec<String>, String> {
    const SANCTIONED: &[&str] = &[]; // extend with FFI seams as they land
    let mut v = Vec::new();
    for f in source_files(root) {
        let rel = f.strip_prefix(root).unwrap_or(&f).display().to_string();
        if SANCTIONED.iter().any(|s| rel.starts_with(s)) {
            continue;
        }
        let text = std::fs::read_to_string(&f).map_err(|e| format!("read {rel}: {e}"))?;
        // needles assembled via concat! so this scanner never matches itself
        const NEEDLES: [&str; 3] = [
            concat!("unsafe", " fn"),
            concat!("unsafe", " impl"),
            concat!("unsafe", " {"),
        ];
        for (i, line) in text.lines().enumerate() {
            let code = line.split("//").next().unwrap_or("");
            if NEEDLES.iter().any(|n| code.contains(n)) {
                v.push(format!(
                    "{rel}:{}: `unsafe` outside a sanctioned module",
                    i + 1
                ));
            }
        }
    }
    Ok(v)
}

/// One-based line numbers of blanket clippy `#![allow(...)]` inner attributes
/// in `text`, reported at the attribute's opening line.
///
/// Catches both the single-line `#![allow(clippy::foo)]` form and the
/// multi-line form
///
/// ```text
/// #![allow(
///     clippy::foo,
///     clippy::bar
/// )]
/// ```
///
/// which the earlier line-prefix check missed, letting a crate-root blanket
/// evade the gate simply by wrapping. The test-only
/// `#![cfg_attr(test, allow(...))]` form is never matched — it opens with
/// `#![cfg_attr`, not `#![allow(` — and blocks that name no `clippy::` lint
/// (e.g. `#![allow(dead_code)]`) are left to the rust-lint policy.
fn blanket_clippy_allow_lines(text: &str) -> Vec<usize> {
    // Drop a trailing line comment before inspecting attribute syntax, so a
    // `clippy::` mentioned only in a comment can neither trigger nor mask a
    // match (mirrors the `unsafe` scanner above). `str::lines` already strips a
    // trailing `\r`, so CRLF sources are handled.
    fn code(line: &str) -> &str {
        line.split("//").next().unwrap_or("")
    }
    let mut hits = Vec::new();
    let mut it = text.lines().enumerate().peekable();
    while let Some((start, first)) = it.next() {
        // An `#![allow (...)]` inner attribute — whitespace between `allow` and
        // `(` is legal Rust. The test-only `#![cfg_attr(test, allow(...))]` form
        // and item attributes (`#[allow(...)]`, a single `#[`) are excluded:
        // neither starts with `#![allow`.
        let is_opener = code(first)
            .trim_start()
            .strip_prefix("#![allow")
            .is_some_and(|rest| rest.trim_start().starts_with('('));
        if !is_opener {
            continue;
        }
        // Accumulate comment-stripped code from the opener to the closing `)]`,
        // so a wrapped list reads as one attribute. Stop before a fresh
        // inner-attr opener so a malformed, never-closed block cannot swallow
        // the next one.
        let mut block = code(first).trim().to_string();
        while !block.contains(")]") {
            let Some((_, next)) = it.peek() else { break };
            if next.trim_start().starts_with("#![") {
                break;
            }
            if let Some((_, line)) = it.next() {
                block.push_str(code(line).trim());
            }
        }
        if block.contains("clippy::") {
            hits.push(start + 1);
        }
    }
    hits
}

/// Crate roots carry no blanket `#![allow(clippy::...)]` of workspace-managed
/// lints — the workspace table is the single policy point; site allows must be
/// narrow and justified. (`#![cfg_attr(test, allow(...))]` is fine: test-only.)
///
/// # Errors
/// If a crate-root file cannot be read.
pub fn no_blanket_allow_at_crate_root(root: &Path) -> Result<Vec<String>, String> {
    let mut v = Vec::new();
    for f in source_files(root) {
        let name = f.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let is_crate_root = (name == "lib.rs" || name == "main.rs")
            && f.parent().is_some_and(|p| p.ends_with("src"));
        if !is_crate_root {
            continue;
        }
        let rel = f.strip_prefix(root).unwrap_or(&f).display().to_string();
        let text = std::fs::read_to_string(&f).map_err(|e| format!("read {rel}: {e}"))?;
        for line in blanket_clippy_allow_lines(&text) {
            v.push(format!(
                "{rel}:{line}: blanket crate-root `#![allow(clippy::…)]` — manage the lint \
                 in [workspace.lints] or allow narrowly at the site"
            ));
        }
    }
    Ok(v)
}

/// The production half of a source file: everything before its trailing test
/// module, located by the joined column-zero anchor described on
/// [`no_schema_fastpath_in_bins`].
///
/// Split out from the walk so the horizon itself is testable on strings — the
/// gate it feeds reads the filesystem, and an untested horizon is how the
/// previous spelling went blind over 64% of a real file unnoticed.
pub(crate) fn production_half(text: &str) -> String {
    // Assembled so this scanner never matches itself.
    let anchor = ["\n#[cfg(", "test)]\nmod "].concat();
    // Lead with a newline so an anchor on line 1 still reads as column-zero.
    let padded = format!("\n{text}");
    padded.split(&anchor).next().unwrap_or("").to_owned()
}

/// `bins/` production code never uses the `create_canonical_schema` fast-path
/// (it bypasses the checksum-locked migration ledger).
///
/// The exempt region is the file's trailing test module, located by the JOINED
/// anchor `\n#[cfg(test)]\nmod ` — a column-zero `#[cfg(test)]` whose very next
/// line opens a module. Anything the anchor does not match is SCREENED, not
/// exempted, so an ambiguous marker becomes a finding rather than a silent
/// hole.
///
/// That is deliberate, and it is not hypothetical. The previous spelling cut at
/// the bare token, which an item-level `#[cfg(test)]` truncates just as
/// effectively as a test module: `bins/rift/src/features.rs` gates a test-only
/// helper `fn` at line 92 of 260, so the gate exempted 64% of the file — and
/// two production functions with it. Four further shapes (an indented marker,
/// and the token inside a string, a `//` comment, or a `//!` doc comment) each
/// disabled the gate for a whole file with no test module involved at all.
///
/// Consequence worth knowing before you hit it: a `#[cfg(test)]`-gated helper
/// that legitimately calls the fast-path IS now reported. Move it inside the
/// test module. That direction is fail-closed, which is the correct posture for
/// a gate of last resort — and by this workspace's own precedent, a gate of
/// last resort must not depend on another gate to be correct.
///
/// Deliberately NOT a uniqueness rule (`count != 1` → report): a file may
/// legitimately carry several test modules (`crates/rift-canopy/src/suggest.rs`
/// carries three), and a gate that cries wolf on a large file gets silenced.
///
/// Residual, stated rather than papered over: a string or block comment holding
/// the exact sequence `\n#[cfg(test)]\nmod ` at column zero still truncates. No
/// `bins/` file holds one today. Narrower than what it replaces, not absent.
///
/// # Errors
/// If a source file cannot be read.
pub fn no_schema_fastpath_in_bins(root: &Path) -> Result<Vec<String>, String> {
    let mut v = Vec::new();
    for f in source_files(root) {
        let rel = f.strip_prefix(root).unwrap_or(&f).display().to_string();
        if !rel.starts_with("bins/") || rel.contains("/tests/") {
            continue;
        }
        let text = std::fs::read_to_string(&f).map_err(|e| format!("read {rel}: {e}"))?;
        if production_half(&text).contains("create_canonical_schema") {
            v.push(format!(
                "{rel}: `create_canonical_schema` in production bin code — open the \
                 projection via `rift_store::open_migrated`/`open_capture_projection`"
            ));
        }
    }
    Ok(v)
}

/// `path::test_name` for every proptest string generator in `text` whose regex
/// uses `.` without the `(?s)` flag — i.e. one that can never generate a newline.
///
/// The needle is assembled with `concat!` so this scanner never matches itself.
fn newline_blind_generators(text: &str) -> Vec<String> {
    const GENERATOR: &str = concat!("in", " \"");
    const DOT_FORMS: [&str; 3] = [concat!(".", "{"), concat!(".", "*"), concat!(".", "+")];
    let mut out = Vec::new();
    let mut current_fn = String::new();
    for raw in text.lines() {
        let line = raw.split("//").next().unwrap_or("");
        if let Some(after_fn) = line.split("fn ").nth(1) {
            if let Some(name) = after_fn.split(['(', '<', ' ']).next() {
                if !name.is_empty() {
                    name.clone_into(&mut current_fn);
                }
            }
        }
        let Some(after) = line.split(GENERATOR).nth(1) else {
            continue;
        };
        let Some(pattern) = after.split('"').next() else {
            continue;
        };
        if pattern.contains("(?s)") || !DOT_FORMS.iter().any(|d| pattern.contains(d)) {
            continue;
        }
        if !current_fn.is_empty() && !out.contains(&current_fn) {
            out.push(current_fn.clone());
        }
    }
    out
}

/// No NEW proptest generator can be newline-blind; the known ones are listed in
/// `ci/proptest-newline-baseline.txt` and that list may only shrink.
///
/// `proptest::string::string_regex` parses with `ParserBuilder::new().build()`,
/// whose `dot_matches_new_line` defaults to false, so a `.`-based generator
/// never produces `\n` — measured here at 0 newline-bearing samples in 5000,
/// against 4237/5000 once `(?s)` is added. Every such property is therefore a
/// single-line property, while newline is the character this codebase turns on:
/// the capture document is line-oriented, pack lines are role-marker-anchored,
/// trailers are column-0-anchored, and TOML is line-oriented. A test that reads
/// as "arbitrary input" and has never seen a newline is the most expensive kind
/// of green.
///
/// A baseline entry that no longer matches is also a violation — that is what
/// makes the list shrink-only rather than a place to park debt.
///
/// # Errors
/// If the baseline or a source file cannot be read.
pub fn proptest_generators_reach_newlines(root: &Path) -> Result<Vec<String>, String> {
    let baseline_rel = "ci/proptest-newline-baseline.txt";
    let baseline_text = read(root, baseline_rel)?;
    let baseline: Vec<&str> = baseline_text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect();
    let mut found: Vec<String> = Vec::new();
    for f in source_files(root) {
        let rel = f
            .strip_prefix(root)
            .unwrap_or(&f)
            .display()
            .to_string()
            .replace('\\', "/");
        let text = std::fs::read_to_string(&f).map_err(|e| format!("read {rel}: {e}"))?;
        for name in newline_blind_generators(&text) {
            found.push(format!("{rel}::{name}"));
        }
    }
    found.sort();
    let mut v = Vec::new();
    for site in &found {
        if !baseline.contains(&site.as_str()) {
            v.push(format!(
                "{site}: proptest generator uses `.` without `(?s)`, so it never \
                 generates a newline — prefix the regex with `(?s)`"
            ));
        }
    }
    for site in &baseline {
        if !found.iter().any(|f| f == site) {
            v.push(format!(
                "{baseline_rel} names `{site}`, which is no longer newline-blind — \
                 delete the line (this baseline only shrinks)"
            ));
        }
    }
    Ok(v)
}

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

/// One fuzz list against the targets on disk, in both directions.
fn list_violations(label: &str, on_disk: &[String], listed: &[String]) -> Vec<String> {
    let mut v = Vec::new();
    for target in on_disk {
        if !listed.contains(target) {
            v.push(format!(
                "{label} fuzz target `{target}` — a target nobody runs is not a gate"
            ));
        }
    }
    for target in listed {
        if !on_disk.contains(target) {
            v.push(format!(
                "a fuzz list names `{target}`, but fuzz/fuzz_targets/{target}.rs does not exist"
            ));
        }
    }
    v
}

/// The paths an owner decision deleted stay deleted.
///
/// This machine's iCloud sync RESURRECTS them on disk: the first four were
/// observed untracked-present in a sibling worktree on 2026-08-18, with the
/// wrapper crate carrying `Cargo.toml`, `pinned-substrate.toml` and
/// `src/{fetch,lib,pin}.rs` — the closure commit `94a1426` deleted.
/// `.gitignore` stops a bulk `git add -A` re-adding them, but ignoring ALONE
/// would only hide the resurrection, so this asserts BOTH halves: every path
/// is ignored, and no path is tracked. A `git add -f` that re-adds one reds
/// here.
///
/// `crates/rift-fff/` and `crates/rift-codegraph/` join the list under a
/// second owner decision (2026-08-20), and they are the reason the list is not
/// wrapper-specific: both had been ruled deleted once already, and both came
/// BACK as manifest-only scaffolds with an empty `src/` — staged, not merely
/// present. Nothing in `crates/`, `bins/`, `xtask/`, `ci/`, `.github/` or the
/// root manifest ever referenced them, so no compile would have complained.
/// The one thing that WOULD have complained is
/// [`crate::check_members_match_dirs`], which walks the FILESYSTEM — and it
/// was already red in the primary checkout while every linked worktree passed,
/// because an untracked directory does not exist in a linked worktree. That
/// asymmetry is exactly why the ignore half and the absence half are both
/// needed: the ignore stops the re-add, the absence check is what a gate run
/// from a linked worktree can still see.
///
/// A declared weakness: the ignore half looks for the literal line, so a later
/// negation (`!/crates/rift-fff/`) would re-enable the bulk add while this
/// check still saw its entry. The pair survives it — the ABSENCE half is what
/// actually reds on a re-add, and a negation cannot help there — but the
/// ignore half alone is weaker than it reads.
///
/// Returns `(paths examined, violations)`. The count is not decoration: a
/// caller can refuse a screen that examined nothing, so emptying `DELETED`
/// is visible instead of quietly turning this check into a no-op.
///
/// # Errors
/// If `.gitignore` cannot be read or `git ls-files` cannot be run.
pub fn deleted_paths_stay_deleted(root: &Path) -> Result<(usize, Vec<String>), String> {
    // The boolean says whether the path must also be ignored. The two retired
    // branded paths stay visible because joining their token in `.gitignore`
    // would violate the zero-token tree contract; this guard constructs them
    // from pieces and still rejects either tracked or untracked resurrection.
    const DELETED: &[(&str, bool)] = &[
        (concat!("crates/rift-shadow-", "ent", "ire", "/"), false),
        ("xtask/src/pin_cut.rs", true),
        (".rift/rewrite.toml", true),
        (concat!("tests/fixtures/fake-", "ent", "ire", "/"), false),
        ("crates/rift-fff/", true),
        ("crates/rift-codegraph/", true),
        (".mcp.json", true),
        ("bins/rift-memd/src/mcp_serve.rs", true),
        // The document helper binary: its parsers moved into `rift`, which
        // re-spawns itself on `hooks doc` instead of looking for a sibling.
        ("bins/rift-doc/", true),
    ];
    let ignore = read(root, ".gitignore")?;
    let mut v = Vec::new();
    for &(path, must_be_ignored) in DELETED {
        let ignored = ignore
            .lines()
            .any(|l| l.trim().strip_prefix('/') == Some(path));
        if must_be_ignored && !ignored {
            v.push(format!(
                ".gitignore: `/{path}` is missing — a bulk `git add -A` would re-add a deleted path"
            ));
        }
        if root.join(path.trim_end_matches('/')).exists() {
            v.push(format!(
                "present: `{path}` was deleted by owner decision and exists again"
            ));
        }
    }
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .arg("ls-files")
        .arg("--")
        .args(DELETED.iter().map(|(path, _)| path))
        .output()
        .map_err(|e| format!("git ls-files (deleted-path absence check): {e}"))?;
    let tracked = String::from_utf8_lossy(&out.stdout);
    for line in tracked.lines().map(str::trim).filter(|l| !l.is_empty()) {
        v.push(format!(
            "tracked: `{line}` was deleted by owner decision and is tracked again"
        ));
    }
    Ok((DELETED.len(), v))
}

/// One retired lexeme: the literal bytes it starts with, plus the shape that
/// must follow for the match to count.
///
/// Every lead is assembled with `concat!`, so this file spells no retired
/// identifier and needs no exemption from its own screen.
struct Retired {
    /// The literal bytes the lexeme opens with.
    lead: &'static str,
    /// Compare the lead ASCII-case-insensitively.
    fold_case: bool,
    /// The byte before the lead must be non-alphanumeric — the guard that
    /// keeps a short token from firing inside a hex or base64 run.
    boundary_before: bool,
    /// The shape after the lead. Returns the extra bytes it consumes, or
    /// `None` when this is not the lexeme after all.
    tail: fn(&[u8]) -> Option<usize>,
}

/// The lead is the whole lexeme.
fn nothing(_: &[u8]) -> Option<usize> {
    Some(0)
}

/// The lexeme ends here: the next byte, if any, is non-alphanumeric.
fn ends_here(rest: &[u8]) -> Option<usize> {
    if rest.first().is_none_or(|b| !b.is_ascii_alphanumeric()) {
        Some(0)
    } else {
        None
    }
}

/// One decimal digit.
fn digit(rest: &[u8]) -> Option<usize> {
    rest.first().filter(|b| b.is_ascii_digit()).map(|_| 1)
}

/// Four decimal digits — the decision-record numbering.
fn four_digits(rest: &[u8]) -> Option<usize> {
    rest.get(..4)
        .filter(|w| w.iter().all(u8::is_ascii_digit))
        .map(|_| 4)
}

/// One upper-case letter or digit — the requirement-document numbering.
fn upper_or_digit(rest: &[u8]) -> Option<usize> {
    rest.first()
        .filter(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
        .map(|_| 1)
}

/// An optional plural `s`, an optional space or hyphen, then a digit — so the
/// singular, the plural, the spaced and the hyphenated citation all count.
fn numbered_tail(rest: &[u8]) -> Option<usize> {
    let mut at = 0;
    if rest.get(at).is_some_and(|b| b.eq_ignore_ascii_case(&b's')) {
        at += 1;
    }
    if rest.get(at).is_some_and(|b| *b == b' ' || *b == b'-') {
        at += 1;
    }
    if rest.get(at).is_some_and(u8::is_ascii_digit) {
        Some(at + 1)
    } else {
        None
    }
}

/// The second half of the retired master-plan filename, after an optional
/// backslash — Markdown escapes the separator, and an escaped citation is the
/// same citation.
fn master_tail(rest: &[u8]) -> Option<usize> {
    const SECOND_HALF: &str = concat!("_MAS", "TER");
    let escaped = usize::from(rest.first() == Some(&b'\\'));
    rest.get(escaped..escaped + SECOND_HALF.len())
        .filter(|w| *w == SECOND_HALF.as_bytes())
        .map(|_| escaped + SECOND_HALF.len())
}

/// The retired ticket numbering: a milestone digit, an optional point
/// release, a hyphen and a two-digit ticket, bounded on the right.
fn ticket_tail(rest: &[u8]) -> Option<usize> {
    let mut at = 0;
    if !rest.get(at).is_some_and(u8::is_ascii_digit) {
        return None;
    }
    at += 1;
    if rest.get(at) == Some(&b'.') {
        at += 1;
        let first_fraction_digit = at;
        while rest.get(at).is_some_and(u8::is_ascii_digit) {
            at += 1;
        }
        if at == first_fraction_digit {
            return None;
        }
    }
    if rest.get(at) != Some(&b'-') {
        return None;
    }
    at += 1;
    if !rest.get(at).is_some_and(u8::is_ascii_digit)
        || !rest.get(at + 1).is_some_and(u8::is_ascii_digit)
    {
        return None;
    }
    at += 2;
    ends_here(rest.get(at..).unwrap_or_default()).map(|_| at)
}

/// The retired phase-and-step citation: a phase digit, a space, and a step,
/// bounded on the right.
fn step_tail(rest: &[u8]) -> Option<usize> {
    if !rest.first().is_some_and(|b| (b'1'..=b'7').contains(b))
        || rest.get(1) != Some(&b' ')
        || rest.get(2) != Some(&b'S')
        || !rest.get(3).is_some_and(u8::is_ascii_digit)
    {
        return None;
    }
    ends_here(rest.get(4..).unwrap_or_default()).map(|_| 4)
}

/// The retired program vocabulary, one entry per lexeme shape.
///
/// Deliberately absent: the bare phase, milestone and gate letters. Two or
/// three characters wide, they collide constantly with hex digests, base64
/// payloads and benchmark arm names (a letter-digit arm prefix is an arm, not a gate), so
/// they were swept once by eye rather than screened forever — a screen that
/// cries wolf gets suppressed, and then it guards nothing.
const RETIRED: &[Retired] = &[
    Retired {
        lead: concat!("AD", "R-"),
        fold_case: false,
        boundary_before: false,
        tail: four_digits,
    },
    Retired {
        lead: concat!("PR", "D-"),
        fold_case: false,
        boundary_before: false,
        tail: upper_or_digit,
    },
    Retired {
        lead: concat!("rul", "ing"),
        fold_case: true,
        boundary_before: false,
        tail: numbered_tail,
    },
    Retired {
        lead: concat!("SW", "T-"),
        fold_case: false,
        boundary_before: false,
        tail: digit,
    },
    Retired {
        lead: concat!("pre-", "SW", "T"),
        fold_case: false,
        boundary_before: false,
        tail: nothing,
    },
    Retired {
        lead: concat!("RE", "C-"),
        fold_case: false,
        boundary_before: true,
        tail: digit,
    },
    Retired {
        lead: concat!("gate-", "p8"),
        fold_case: false,
        boundary_before: false,
        tail: nothing,
    },
    Retired {
        lead: concat!("gate-", "swt"),
        fold_case: false,
        boundary_before: false,
        tail: nothing,
    },
    Retired {
        lead: concat!("gate-", "m"),
        fold_case: false,
        boundary_before: false,
        tail: digit,
    },
    Retired {
        lead: concat!("GO", "AL"),
        fold_case: false,
        boundary_before: false,
        tail: master_tail,
    },
    Retired {
        lead: concat!("CHAR", "TER"),
        fold_case: false,
        boundary_before: false,
        tail: nothing,
    },
    Retired {
        lead: concat!("RESI", "DUE"),
        fold_case: false,
        boundary_before: false,
        tail: nothing,
    },
    Retired {
        lead: concat!("Sta", "ge "),
        fold_case: false,
        boundary_before: false,
        tail: digit,
    },
    Retired {
        lead: concat!("G", "7C"),
        fold_case: false,
        boundary_before: true,
        tail: ends_here,
    },
    Retired {
        lead: "M",
        fold_case: false,
        boundary_before: true,
        tail: ticket_tail,
    },
    Retired {
        lead: "P",
        fold_case: false,
        boundary_before: true,
        tail: step_tail,
    },
    Retired {
        lead: concat!("SW", "T"),
        fold_case: false,
        boundary_before: true,
        tail: ends_here,
    },
    Retired {
        lead: concat!("ent", "ire"),
        fold_case: true,
        boundary_before: false,
        tail: nothing,
    },
];

/// The bytes any retired lexeme can open with — a prefilter, so the common
/// case (a byte no lexeme starts with) costs one table lookup instead of a
/// walk over every entry. Unknown bytes fail OPEN: a lookup that cannot
/// answer runs the full match rather than skipping it.
fn lead_bytes() -> [bool; 256] {
    let mut table = [false; 256];
    for needle in RETIRED {
        let Some(&first) = needle.lead.as_bytes().first() else {
            continue;
        };
        for byte in [first.to_ascii_lowercase(), first.to_ascii_uppercase()] {
            if !needle.fold_case && byte != first {
                continue;
            }
            if let Some(slot) = table.get_mut(usize::from(byte)) {
                *slot = true;
            }
        }
    }
    table
}

/// The lexeme starting at `at`, if one does. First entry wins, so the more
/// specific shapes are listed before the bare ones.
fn lexeme_at(hay: &[u8], at: usize) -> Option<usize> {
    for needle in RETIRED {
        let lead = needle.lead.as_bytes();
        let Some(window) = hay.get(at..at + lead.len()) else {
            continue;
        };
        let opens = if needle.fold_case {
            window.eq_ignore_ascii_case(lead)
        } else {
            window == lead
        };
        if !opens {
            continue;
        }
        if needle.boundary_before
            && at > 0
            && hay.get(at - 1).is_some_and(u8::is_ascii_alphanumeric)
        {
            continue;
        }
        if let Some(extra) = (needle.tail)(hay.get(at + lead.len()..).unwrap_or_default()) {
            return Some(lead.len() + extra);
        }
    }
    None
}

/// Every retired lexeme in `hay`, as it is spelled there.
///
/// A match consumes its own bytes, so one site is reported once even when a
/// wider shape contains a narrower one.
fn retired_lexemes(hay: &[u8], table: &[bool; 256]) -> Vec<String> {
    let mut found = Vec::new();
    let mut at = 0;
    while at < hay.len() {
        let Some(&byte) = hay.get(at) else { break };
        if table.get(usize::from(byte)).copied().unwrap_or(true) {
            if let Some(len) = lexeme_at(hay, at) {
                found.push(
                    String::from_utf8_lossy(hay.get(at..at + len).unwrap_or_default()).into_owned(),
                );
                at += len.max(1);
                continue;
            }
        }
        at += 1;
    }
    found
}

/// No tracked path or tracked byte carries the retired program vocabulary.
///
/// The corpus is `tracked_paths` — the git index, not the filesystem — with
/// NO exclusions: lockfiles, snapshots, fixtures, corpora and the written
/// guide are all in it, because the vocabulary this screen retires is exactly
/// the vocabulary a reader of any of those would trip over. Both halves of a
/// tracked file are scanned, its path bytes and its content bytes.
///
/// A symlink is scanned as its LINK TEXT and never followed. Following one
/// can land on a directory (the committed skill links do), and reading a
/// target inspects bytes git does not track while skipping the bytes it does.
///
/// Text files are reported by line; a file holding a NUL byte is reported
/// whole, since it has no lines to name.
///
/// Returns `(files examined, violations)`. The count is not decoration: the
/// caller refuses a screen that examined nothing, so a corpus that silently
/// empties out is visible instead of passing.
///
/// # Errors
/// If `git ls-files` fails, a tracked file cannot be read, or the walk
/// returned no files at all.
pub fn retired_vocabulary_screen(root: &Path) -> Result<(usize, Vec<String>), String> {
    screen_tracked(root, &tracked_paths(root)?)
}

/// The screen over an explicit path list — the seam the planted-hit fixture
/// uses, so the shape checks need no repository of their own.
fn screen_tracked(root: &Path, paths: &[String]) -> Result<(usize, Vec<String>), String> {
    const ADVICE: &str = "rewrite to plain-language rationale";
    if paths.is_empty() {
        return Err("retired-vocabulary screen: the tracked-path walk returned nothing".to_owned());
    }
    let table = lead_bytes();
    let mut v = Vec::new();
    for rel in paths {
        for lexeme in retired_lexemes(rel.as_bytes(), &table) {
            v.push(format!(
                "{rel}: retired program vocabulary '{lexeme}' in the tracked path — {ADVICE}"
            ));
        }
        let full = root.join(rel);
        let metadata = std::fs::symlink_metadata(&full)
            .map_err(|e| format!("inspect tracked file {rel}: {e}"))?;
        let bytes = if metadata.file_type().is_symlink() {
            std::fs::read_link(&full)
                .map_err(|e| format!("read tracked symlink {rel}: {e}"))?
                .as_os_str()
                .to_string_lossy()
                .into_owned()
                .into_bytes()
        } else {
            std::fs::read(&full).map_err(|e| format!("read tracked file {rel}: {e}"))?
        };
        if bytes.contains(&0) {
            for lexeme in retired_lexemes(&bytes, &table) {
                v.push(format!(
                    "{rel}: retired program vocabulary '{lexeme}' in file bytes (binary)"
                ));
            }
        } else {
            for (index, line) in bytes.split(|b| *b == b'\n').enumerate() {
                let number = index + 1;
                for lexeme in retired_lexemes(line, &table) {
                    v.push(format!(
                        "{rel}:{number}: retired program vocabulary '{lexeme}' — {ADVICE}"
                    ));
                }
            }
        }
    }
    Ok((paths.len(), v))
}

/// Run every meta-check, returning the retired-vocabulary screen's scanned
/// count and all violations.
///
/// The count travels with the result so the caller can print it: a screen
/// over an empty corpus is a screen that guards nothing, and the number on
/// the ok line is what makes that visible without reading this code.
///
/// # Errors
/// If any input file cannot be read.
pub fn run_all(root: &Path) -> Result<(usize, Vec<String>), String> {
    let mut v = Vec::new();
    v.extend(required_jobs_present(root)?);
    v.extend(workflow_permissions_read_only(root)?);
    v.extend(lint_table_matches_baseline(root)?);
    v.extend(unsafe_only_in_sanctioned_modules(root)?);
    v.extend(no_blanket_allow_at_crate_root(root)?);
    v.extend(no_schema_fastpath_in_bins(root)?);
    v.extend(fuzz_targets_all_run_in_ci(root)?);
    v.extend(proptest_generators_reach_newlines(root)?);
    v.extend(no_network_deps_in_capture_crates(root)?);
    v.extend(dialect_conformance_job_runs_every_suite(root)?);
    v.extend(fuzzy_matcher_reads_within_bounds(root)?);
    v.extend(real_model_is_opt_in(root)?);
    v.extend(no_network_crate_in_the_resolved_graph(root)?);
    v.extend(deleted_paths_stay_deleted(root)?.1);
    let (scanned, retired) = retired_vocabulary_screen(root)?;
    v.extend(retired);
    Ok((scanned, v))
}

/// Every dependency-section KEY a manifest declares, parsed as TOML — so
/// `name = "1"`, `name = { … }` and `[dependencies.name]` are all the same
/// declaration (a mutation round's first bypass was the table form slipping a
/// line matcher). Sections covered: dependencies, dev-dependencies,
/// build-dependencies, and every `[target.'…'.<section>]` variant.
fn declared_dep_keys(manifest: &toml::Value) -> Vec<String> {
    let mut out = Vec::new();
    let sections = ["dependencies", "dev-dependencies", "build-dependencies"];
    for section in sections {
        if let Some(tbl) = manifest.get(section).and_then(toml::Value::as_table) {
            out.extend(tbl.keys().cloned());
        }
    }
    if let Some(targets) = manifest.get("target").and_then(toml::Value::as_table) {
        for target_tbl in targets.values() {
            for section in sections {
                if let Some(tbl) = target_tbl.get(section).and_then(toml::Value::as_table) {
                    out.extend(tbl.keys().cloned());
                }
            }
        }
    }
    out
}

/// The network clients no shipped binary may resolve. Shared by the manifest
/// screen below and the resolved-graph screen, so the two cannot disagree
/// about what "a network client" is.
const NETWORK_CRATES: [&str; 8] = [
    "ureq",
    "reqwest",
    "hyper",
    "curl",
    "isahc",
    "attohttpc",
    "surf",
    "tokio-tungstenite",
];

/// No crate the `rift` binary links may declare a network client — invariant
/// §1.4 ("no network in capture"), previously asserted only by argument.
///
/// SCOPE, stated exactly (a mutation round rejected the universal claim this
/// docstring used to make): the screen walks the IN-WORKSPACE dependency
/// closure of `rift` (the same `dependency_graph` the adjacency snapshot
/// pins) and checks every reachable member's manifest — all dependency
/// sections, all declaration spellings, TOML-parsed. What it does NOT see:
/// a network client hidden behind an EXTERNAL intermediary crate — that is
/// what [`no_network_crate_in_the_resolved_graph`] sees, over the
/// default-resolved `cargo tree` of both binaries. Both of that round's
/// bypasses — the `[dependencies.ureq]` table form, and a plain dep
/// in `rift-config`, which the hook links but a hardcoded list omitted —
/// are covered by construction and pinned in the calibration test.
///
/// # Errors
/// If the workspace graph or a member manifest cannot be read.
pub fn no_network_deps_in_capture_crates(root: &Path) -> Result<Vec<String>, String> {
    let graph = crate::dependency_graph(root)?;
    // BFS the in-workspace closure of the binary that hosts the hooks.
    let mut reach = std::collections::BTreeSet::new();
    let mut queue = vec!["rift".to_string()];
    while let Some(n) = queue.pop() {
        if reach.insert(n.clone()) {
            if let Some(edges) = graph.get(&n) {
                queue.extend(edges.iter().cloned());
            }
        }
    }
    let members = crate::declared_members(root)?;
    let mut v = Vec::new();
    for m in &members {
        let rel = format!("{m}/Cargo.toml");
        let manifest = crate::read_toml(&root.join(&rel))?;
        let Some(name) = crate::package_name(&manifest) else {
            continue;
        };
        if !reach.contains(&name) {
            continue;
        }
        for key in declared_dep_keys(&manifest) {
            if NETWORK_CRATES.contains(&key.as_str()) {
                v.push(format!(
                    "{rel}: crate `{name}` is in the rift binary's closure and declares \
                     network client `{key}` — invariant §1.4 (no network in capture)"
                ));
            }
        }
    }
    Ok(v)
}

/// The dialect-conformance suites: every
/// integration-test target the `dialect-conformance` job must run, by
/// `--test` name. Grow this when a dialect suite lands; the job's `--test`
/// words must equal it exactly.
const DIALECT_SUITES: [&str; 5] = [
    "legacy_import_golden",
    "dual_source",
    "shard_goldens",
    "parity_anchors",
    "gix_walker",
];

/// The body of one top-level job in the workflow: from its `\n  <job>:` line
/// to the next two-space-indented key. Empty when the job is absent.
fn job_body<'a>(ci: &'a str, job: &str) -> &'a str {
    let Some((_, rest)) = ci.split_once(&format!("\n  {job}:")) else {
        return "";
    };
    let end = rest
        .match_indices("\n  ")
        .find(|(i, _)| {
            rest.get(i + 3..)
                .is_some_and(|after| after.starts_with(|c: char| c.is_ascii_alphanumeric()))
        })
        .map_or(rest.len(), |(i, _)| i);
    rest.get(..end).unwrap_or("")
}

/// Every `--test <name>` word in a job body.
fn test_targets(body: &str) -> Vec<String> {
    let mut words = body.split_whitespace().peekable();
    let mut out = Vec::new();
    while let Some(w) = words.next() {
        if w == "--test" {
            if let Some(name) = words.next() {
                out.push(name.to_owned());
            }
        }
    }
    out
}

/// The `dialect-conformance` job runs exactly `DIALECT_SUITES` — set
/// equality both ways, mirroring [`fuzz_targets_all_run_in_ci`]: a suite the
/// job dropped and a suite this list does not know are both violations.
///
/// # Errors
/// If the workflow file cannot be read.
pub fn dialect_conformance_job_runs_every_suite(root: &Path) -> Result<Vec<String>, String> {
    let ci = read(root, ".github/workflows/ci.yml")?;
    let body = job_body(&ci, "dialect-conformance");
    if body.is_empty() {
        return Ok(vec![
            "ci.yml: the `dialect-conformance` job is absent".to_owned()
        ]);
    }
    let listed = test_targets(body);
    let expected: Vec<String> = DIALECT_SUITES.iter().map(|s| (*s).to_owned()).collect();
    let mut v = list_violations(
        "ci.yml dialect-conformance does not run",
        &expected,
        &listed,
    );
    v.extend(list_violations(
        "DIALECT_SUITES does not declare (add it or drop it from the job)",
        &listed,
        &expected,
    ));
    Ok(v)
}

/// The fuzzy matcher, and the feature that decides whether it reads past what
/// it was given.
///
/// `neo_frizbee`'s vectorised backends load a whole register at a time and
/// rely on the load staying inside the same page — an over-read that is
/// undefined behaviour in Rust however well it behaves in practice, and one
/// this workspace's own fuzz target reached. `safe_read` is the published
/// feature that turns those loads into bounded ones.
const FUZZY_MATCHER: (&str, &str) = ("neo_frizbee", "safe_read");

/// The one violation this check can report, over a manifest already parsed.
///
/// Split from the file read for the reason `real_model_enabled_by_default` is:
/// the claim is about a declaration, and a test that states the declaration
/// directly proves the check on every spelling of it rather than on a
/// temporary file.
fn matcher_pin_carries_bounds(manifest: &toml::Value) -> Vec<String> {
    let (crate_name, feature) = FUZZY_MATCHER;
    let Some(pin) = manifest
        .get("workspace")
        .and_then(|w| w.get("dependencies"))
        .and_then(|d| d.get(crate_name))
    else {
        // Not depended on at all is not a violation: a workspace that stopped
        // using the matcher has nothing to hold to a feature of it.
        return Vec::new();
    };
    if pin
        .get("features")
        .and_then(toml::Value::as_array)
        .is_some_and(|features| features.iter().any(|f| f.as_str() == Some(feature)))
    {
        return Vec::new();
    }
    vec![format!(
        "Cargo.toml: the workspace pin of `{crate_name}` no longer enables `{feature}` — its \
         vectorised backends then read past the end of what they were given, which is \
         undefined behaviour on a matcher this tree calls from a hook with an agent's own \
         query; restore the feature rather than the pin"
    )]
}

/// **The fuzzy matcher stays bounds-checked.**
///
/// The feature is a one-word entry in an array in the root manifest: it is
/// dropped by deleting eleven characters, nothing fails to compile when it
/// goes, and what it costs is a class of bug no test in this repository can
/// see. The pin is the whole safety argument for calling a SIMD matcher on
/// paths that come out of an agent's own query, and it is now called from a
/// hook — so it is asserted here rather than remembered.
///
/// Read as TOML rather than as text, so every spelling of the same dependency
/// declaration is the same fact: an inline table, a
/// `[workspace.dependencies.x]` section, `features` written on one line or
/// several.
///
/// # Errors
/// If the root manifest cannot be read or parsed.
pub fn fuzzy_matcher_reads_within_bounds(root: &Path) -> Result<Vec<String>, String> {
    Ok(matcher_pin_carries_bounds(&crate::read_toml(
        &root.join("Cargo.toml"),
    )?))
}

/// The manifests that could switch the paid model transport on without a
/// flag: the binary that hosts `eval`, the crate that owns the feature, and
/// the capture binary (which must never grow it at all).
const REAL_MODEL_MANIFESTS: [&str; 3] = [
    "bins/rift-memd/Cargo.toml",
    "crates/rift-recall-eval/Cargo.toml",
    "bins/rift/Cargo.toml",
];

/// The two spellings that would enable `real-model` in a default build,
/// TOML-parsed: a dependency entry's `features = [...]` array naming it, or
/// a `[features] default` naming it. A dependency section is any of the
/// forms `declared_dep_keys` reads.
fn real_model_enabled_by_default(manifest: &toml::Value, rel: &str) -> Vec<String> {
    let mut v = Vec::new();
    let names_it = |arr: Option<&toml::Value>| {
        arr.and_then(toml::Value::as_array)
            .is_some_and(|a| a.iter().any(|f| f.as_str() == Some("real-model")))
    };
    let sections = ["dependencies", "dev-dependencies", "build-dependencies"];
    let mut tables: Vec<&toml::Value> = vec![manifest];
    if let Some(targets) = manifest.get("target").and_then(toml::Value::as_table) {
        tables.extend(targets.values());
    }
    for table in tables {
        for section in sections {
            let Some(deps) = table.get(section).and_then(toml::Value::as_table) else {
                continue;
            };
            for (dep, value) in deps {
                if names_it(value.get("features")) {
                    v.push(format!(
                        "{rel}: dependency `{dep}` enables `real-model` unconditionally — \
                         the paid transport is an opt-in feature of the binary, never \
                         a dependency default"
                    ));
                }
            }
        }
    }
    if names_it(manifest.get("features").and_then(|f| f.get("default"))) {
        v.push(format!(
            "{rel}: `[features] default` enables `real-model` — the paid transport must \
             stay off in the default build"
        ));
    }
    v
}

/// `real-model` is a compile-time opt-in: none of `REAL_MODEL_MANIFESTS`
/// enables it through a dependency's `features` array or a `default` feature.
///
/// # Errors
/// If a manifest cannot be read or parsed.
pub fn real_model_is_opt_in(root: &Path) -> Result<Vec<String>, String> {
    let mut v = Vec::new();
    for rel in REAL_MODEL_MANIFESTS {
        let manifest = crate::read_toml(&root.join(rel))?;
        v.extend(real_model_enabled_by_default(&manifest, rel));
    }
    Ok(v)
}

/// The network crates named in a `cargo tree --prefix none --format {p}`
/// listing (first word of each line is the package name). An EMPTY listing
/// is an error, not a pass: a screen that reads nothing must not report
/// "nothing found".
fn network_crates_in_tree(listing: &str) -> Result<Vec<String>, String> {
    let names: Vec<&str> = listing
        .lines()
        .filter_map(|l| l.split_whitespace().next())
        .collect();
    if names.is_empty() {
        return Err("cargo tree returned no packages — the resolved-graph screen read nothing and fails closed".to_owned());
    }
    let mut found: Vec<String> = names
        .iter()
        .filter(|n| NETWORK_CRATES.contains(n))
        .map(|n| (*n).to_owned())
        .collect();
    found.sort_unstable();
    found.dedup();
    Ok(found)
}

/// The DEFAULT-resolved normal-dependency graph of the two shipped binaries
/// (`rift`, `rift-memd`) names no `NETWORK_CRATES` member — the external
/// half of invariant §1.4 that the manifest screen cannot see, and the
/// executable form of the same rule (`ureq` reaches `rift-memd` only through
/// its opt-in `real-model` feature). `--all-features` builds still compile
/// the client; reachability stays the explicit CLI flag, and no test calls it.
///
/// # Errors
/// If `cargo tree` cannot run, fails, or returns no packages (fail closed).
pub fn no_network_crate_in_the_resolved_graph(root: &Path) -> Result<Vec<String>, String> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    let output = std::process::Command::new(cargo)
        .current_dir(root)
        .args([
            "tree",
            "-p",
            "rift",
            "-p",
            "rift-memd",
            "-e",
            "normal",
            "--locked",
            "--prefix",
            "none",
            "--format",
            "{p}",
        ])
        .output()
        .map_err(|e| format!("cargo tree (resolved-graph screen): {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "cargo tree (resolved-graph screen) failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let listing = String::from_utf8_lossy(&output.stdout);
    Ok(network_crates_in_tree(&listing)?
        .into_iter()
        .map(|c| {
            format!(
                "resolved graph: `{c}` is reachable from `rift` or `rift-memd` in the \
                 default build — invariant §1.4 (no network in capture); a network \
                 client belongs behind an opt-in feature only"
            )
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace_root;

    /// A deterministic, ignored scratch directory for the fixtures below —
    /// under the build directory, named for the test, no clock and no
    /// randomness, so a re-run reuses the same path.
    fn scratch_dir(name: &str) -> std::path::PathBuf {
        let root = workspace_root()
            .join("target")
            .join("xtask-fixtures")
            .join(name);
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    /// The mutation round's two bypasses, pinned: the TOML table form is the
    /// same declaration as the inline form once parsed, and the screen's
    /// crate SET is the binary's graph closure, not a hardcoded list.
    #[test]
    fn the_network_screen_sees_every_declaration_spelling() {
        let inline: toml::Value = toml::from_str("[dependencies]\nureq = \"3\"").unwrap();
        let table: toml::Value = toml::from_str("[dependencies.ureq]\nversion = \"3\"").unwrap();
        let dev: toml::Value = toml::from_str("[dev-dependencies]\nreqwest = \"0.12\"").unwrap();
        let target: toml::Value =
            toml::from_str("[target.'cfg(unix)'.dependencies.curl]\nversion = \"0.4\"").unwrap();
        for v in [&inline, &table, &dev, &target] {
            assert!(
                !declared_dep_keys(v).is_empty(),
                "every spelling is a declaration"
            );
        }
        assert!(declared_dep_keys(&table).contains(&"ureq".to_string()));
        let mention: toml::Value = toml::from_str("[package]\nname = \"x\"").unwrap();
        assert!(declared_dep_keys(&mention).is_empty());
    }

    /// Bypass 2's shape: the screened set must include every workspace crate
    /// the rift binary reaches — rift-config was the planted escape, linked by
    /// the hook on every commit yet absent from the old hardcoded list.
    /// rift-shadow-redact is planted too: its own manifest test
    /// (`heavy_model_pass_has_no_dependency_to_stand_on`) is an 11-name
    /// denylist over one file, and the structural claim that crate used to
    /// make for itself is held HERE, by closure membership.
    #[test]
    fn the_screened_set_is_the_binary_closure_and_covers_the_planted_escape() {
        let root = workspace_root();
        let graph = crate::dependency_graph(&root).unwrap();
        let mut reach = std::collections::BTreeSet::new();
        let mut queue = vec!["rift".to_string()];
        while let Some(n) = queue.pop() {
            if reach.insert(n.clone()) {
                if let Some(edges) = graph.get(&n) {
                    queue.extend(edges.iter().cloned());
                }
            }
        }
        for planted in [
            "rift-config",
            "rift-git",
            "rift-shadow",
            "rift-store",
            "rift-shadow-redact",
        ] {
            assert!(reach.contains(planted), "{planted} must be screened");
        }
    }

    /// The meta-checks pass on the real repository — this is the actual gate,
    /// run both here (cargo test -p xtask) and by the `ci-meta` CI job.
    #[test]
    fn repository_passes_all_meta_checks() {
        let (scanned, violations) = run_all(&workspace_root()).expect("meta checks run");
        assert!(
            violations.is_empty(),
            "ci-meta violations:\n{}",
            violations.join("\n")
        );
        assert!(scanned > 400, "the screen examined only {scanned} files");
    }

    /// The live repository carries none of the retired vocabulary, and the
    /// corpus it was screened over is the whole index — not a handful of
    /// files that happened to survive a filter.
    #[test]
    fn the_repository_carries_no_retired_vocabulary() {
        let (scanned, violations) =
            retired_vocabulary_screen(&workspace_root()).expect("the screen runs");
        assert!(
            violations.is_empty(),
            "retired vocabulary:\n{}",
            violations.join("\n")
        );
        assert!(scanned > 400, "the screen examined only {scanned} files");
    }

    /// A planted citation is caught, named, and located — the proof the
    /// screen's silence on the live tree means absence and not blindness.
    #[test]
    fn a_planted_citation_is_one_located_violation() {
        let root = scratch_dir("retired-vocabulary");
        let planted = format!("a first line\nsee {}0001 for why\n", concat!("AD", "R-"));
        std::fs::write(root.join("notes.md"), planted).unwrap();

        let (scanned, violations) =
            screen_tracked(&root, &["notes.md".to_string()]).expect("the screen runs");
        assert_eq!(scanned, 1);
        assert_eq!(violations.len(), 1, "{violations:?}");
        let only = violations.first().unwrap();
        assert!(
            only.starts_with("notes.md:2: retired program vocabulary "),
            "{only}"
        );
        assert!(
            only.ends_with("rewrite to plain-language rationale"),
            "{only}"
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// An empty corpus is refused rather than reported as clean.
    #[test]
    fn a_screen_over_nothing_is_an_error() {
        assert!(screen_tracked(&workspace_root(), &[]).is_err());
    }

    /// Each shape, and the near-misses that must stay quiet: the benchmark
    /// arm names (letter-digit prefixes followed by an underscore) live in the tracked corpus
    /// and are not citations, and a bare milestone letter inside a digest is
    /// the reason the short tokens were left out of the table.
    #[test]
    fn every_shape_matches_itself_and_nothing_near_it() {
        let table = lead_bytes();
        let hit = |s: &str| !retired_lexemes(s.as_bytes(), &table).is_empty();

        for spelled in [
            concat!("AD", "R-", "0069"),
            concat!("PR", "D-", "M6"),
            concat!("rul", "ing 52"),
            concat!("Rul", "ings-3"),
            concat!("RUL", "ING7"),
            concat!("SW", "T-", "06"),
            concat!("pre-", "SW", "T"),
            concat!(" ", "RE", "C-", "2"),
            concat!("gate-", "p8"),
            concat!("gate-", "swt"),
            concat!("gate-", "m5"),
            concat!("GO", "AL", "_MAS", "TER"),
            concat!("GO", "AL", "\\", "_MAS", "TER"),
            concat!("CHAR", "TER"),
            concat!("RESI", "DUE"),
            concat!("Sta", "ge 7"),
            concat!(" ", "G", "7C", " "),
            concat!(" M", "5-01 "),
            concat!(" M", "5.15", "-01 "),
            concat!(" P", "6 S", "3 "),
            concat!(" ", "SW", "T", " "),
            concat!("the ", "ENT", "IRE", " thing"),
        ] {
            assert!(hit(spelled), "missed {spelled}");
        }

        for quiet in [
            concat!("G", "1_line_8k"),
            "F1_newline_8k",
            "a rule of thumb",
            "the gateway",
            "record the outcome",
            concat!("no", "RE", "C-", "2"),
            concat!("x", "G", "7C"),
            concat!("G", "7Cx"),
            " M5-0123 ",
            "aM5-01 ",
            concat!(" P", "8 "),
            " S3 ",
            "M5.-01 ",
            concat!("GO", "AL posts"),
            concat!("chart", "er"),
        ] {
            assert!(!hit(quiet), "false positive on {quiet}");
        }
    }

    /// One site is reported once: the wider shape consumes its own bytes, so
    /// the bare token inside it does not fire a second time.
    #[test]
    fn a_wider_shape_swallows_the_bare_one_inside_it() {
        let table = lead_bytes();
        let found = retired_lexemes(concat!(" ", "SW", "T-", "06 ").as_bytes(), &table);
        assert_eq!(found.len(), 1, "{found:?}");
    }

    /// A NUL byte means the file has no lines worth naming, so the whole
    /// file is the location.
    #[test]
    fn binary_content_is_reported_without_a_line_number() {
        let root = scratch_dir("retired-vocabulary-bin");
        let mut planted = b"\0\0".to_vec();
        planted.extend_from_slice(concat!("gate-", "p8").as_bytes());
        std::fs::write(root.join("blob.bin"), planted).unwrap();

        let (_, violations) =
            screen_tracked(&root, &["blob.bin".to_string()]).expect("the screen runs");
        assert_eq!(violations.len(), 1, "{violations:?}");
        assert!(
            violations
                .first()
                .unwrap()
                .ends_with("in file bytes (binary)"),
            "{violations:?}"
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// The path half of the corpus is screened too — a filename can carry a
    /// citation with no byte of the file mentioning it.
    #[test]
    fn a_citation_in_the_pathname_is_caught() {
        let root = scratch_dir("retired-vocabulary-path");
        let named = format!("{}0007-notes.md", concat!("AD", "R-"));
        std::fs::write(root.join(&named), "clean text\n").unwrap();

        let (_, violations) = screen_tracked(&root, &[named]).expect("the screen runs");
        assert_eq!(violations.len(), 1, "{violations:?}");
        assert!(
            violations.first().unwrap().contains("in the tracked path"),
            "{violations:?}"
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn detects_single_line_blanket_clippy_allow() {
        let src = "#![allow(clippy::pedantic)]\nfn f() {}\n";
        assert_eq!(blanket_clippy_allow_lines(src), vec![1]);
    }

    /// ★ The clippy job builds the binary's feature-off configuration.
    ///
    /// `bins/rift`'s manifest advertises `--no-default-features` — no terminal
    /// backend, no search seam, no document seam — and every other leg in CI
    /// builds the binary with its features ON, so an item that exists only for
    /// one of them, or a `use` of an optional crate that is not behind its
    /// feature, is invisible everywhere else. Both have happened here. The
    /// step is one line and deleting it would be one line, hence this: the
    /// job has to carry a leg that names the configuration, the package and
    /// the denial that makes it a gate rather than a report.
    #[test]
    fn the_clippy_job_checks_the_feature_off_configuration() {
        let ci = std::fs::read_to_string(workspace_root().join(".github/workflows/ci.yml"))
            .expect("read ci.yml");
        let body = job_body(&ci, "clippy");
        assert!(!body.is_empty(), "ci.yml: the `clippy` job is absent");
        let leg = body
            .lines()
            .map(str::trim)
            .filter(|line| line.starts_with("- run:") || line.starts_with("cargo "))
            .find(|line| line.contains("--no-default-features"))
            .expect("ci.yml: the clippy job runs no `--no-default-features` leg");
        for word in ["-p rift", "--all-targets", "-D warnings"] {
            assert!(
                leg.contains(word),
                "ci.yml: the feature-off leg no longer says `{word}`: {leg}"
            );
        }
    }

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

    /// The scanner finds a `.`-based generator, ignores one already carrying
    /// `(?s)`, and attributes each to its enclosing test — including when the
    /// signature wraps onto a later line.
    #[test]
    fn newline_blind_generators_are_found_and_attributed() {
        let blind = "proptest! {\n    #[test]\n    fn takes_any(s in \".{0,40}\") {\n";
        assert_eq!(newline_blind_generators(blind), vec!["takes_any"]);

        let fixed = "fn takes_any(s in \"(?s).{0,40}\") {\n";
        assert!(newline_blind_generators(fixed).is_empty());

        let star = "fn splat(m in \".*\") {\n";
        assert_eq!(newline_blind_generators(star), vec!["splat"]);

        // A generator on a later line still belongs to the fn above it.
        let wrapped = "fn wrapped(\n    a in \".{0,8}\",\n) {\n";
        assert_eq!(newline_blind_generators(wrapped), vec!["wrapped"]);

        // No dot metacharacter: a literal alternation is not newline-blind.
        assert!(newline_blind_generators("fn lit(s in \"a|b\") {\n").is_empty());
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

    #[test]
    fn detects_multiline_blanket_clippy_allow() {
        // The wrapped form that evaded the old line-prefix check.
        let src = "//! doc\n#![allow(\n    clippy::doc_markdown,\n    \
                   clippy::cast_precision_loss\n)]\n\npub fn g() {}\n";
        assert_eq!(blanket_clippy_allow_lines(src), vec![2]);
    }

    #[test]
    fn ignores_cfg_attr_test_allow() {
        // Test-only allows are sanctioned and must never be flagged.
        let src = "#![cfg_attr(\n    test,\n    allow(\n        \
                   clippy::unwrap_used,\n        clippy::cast_possible_wrap\n    )\n)]\n";
        assert!(blanket_clippy_allow_lines(src).is_empty());
    }

    #[test]
    fn ignores_non_clippy_blanket_allow() {
        // The gate governs clippy workspace lints; a rust-lint allow is out of scope.
        let src = "#![allow(dead_code)]\n#![allow(\n    unused_variables\n)]\n";
        assert!(blanket_clippy_allow_lines(src).is_empty());
    }

    #[test]
    fn ignores_clippy_named_only_in_a_comment() {
        // A clippy lint mentioned in a trailing comment must not flag a block
        // whose attribute names no clippy lint.
        let src = "#![allow(dead_code)] // TODO: consider clippy::pedantic here\nfn f() {}\n";
        assert!(blanket_clippy_allow_lines(src).is_empty());
    }

    /// The shape that was LIVE on main: `bins/rift/src/features.rs` gates a
    /// test-only helper `fn` at line 92 of 260, and the old bare-token cut
    /// exempted everything below it — 64% of the file, two production
    /// functions included. An item-level attribute is not a module boundary.
    ///
    /// The call sits AFTER the marker on purpose. That is what `features.rs`
    /// looks like, and it is the only arrangement that exercises the
    /// truncation: with the call ABOVE the marker this test passes under the
    /// old spelling too, which is how the first draft of it was vacuous.
    #[test]
    fn a_test_only_helper_fn_does_not_truncate_the_screen() {
        let src = "pub fn a() {}\n#[cfg(test)]\nfn helper() {}\n\npub fn p() { create_canonical_schema(); }\n";
        assert!(production_half(src).contains("create_canonical_schema"));
    }

    /// The other direction, which is what stops this being a broken exemption
    /// rather than a tightened one.
    #[test]
    fn a_real_test_module_is_still_exempt() {
        let src = "pub fn p() {}\n#[cfg(test)]\nmod tests {\n    fn t() { create_canonical_schema(); }\n}\n";
        assert!(!production_half(src).contains("create_canonical_schema"));
    }

    /// Four shapes that each disabled the gate for a whole file under the old
    /// spelling, with no test module involved at all.
    #[test]
    fn a_marker_that_is_not_a_module_boundary_never_truncates() {
        for src in [
            // indented — a nested test helper
            "pub fn p() {\n    #[cfg(test)]\n    fn n() {}\n    create_canonical_schema();\n}\n",
            // inside a string literal
            "const S: &str = \"#[cfg(test)]\";\npub fn p() { create_canonical_schema(); }\n",
            // inside a line comment
            "// #[cfg(test)]\npub fn p() { create_canonical_schema(); }\n",
            // inside a doc comment
            "//! #[cfg(test)]\npub fn p() { create_canonical_schema(); }\n",
        ] {
            assert!(
                production_half(src).contains("create_canonical_schema"),
                "truncated at a non-boundary marker: {src:?}"
            );
        }
    }

    /// A file may carry several test modules, which is why this is not a
    /// uniqueness rule: the cut is at the FIRST module boundary, and everything
    /// after it is test code.
    #[test]
    fn several_test_modules_cut_at_the_first() {
        let src = "pub fn p() {}\n#[cfg(test)]\nmod a { fn t() { create_canonical_schema(); } }\n#[cfg(test)]\nmod b {}\n";
        assert!(!production_half(src).contains("create_canonical_schema"));
        assert!(production_half(src).contains("pub fn p"));
    }

    /// A file whose test module opens on line 1 still has it recognised — the
    /// anchor is column-zero, and column zero includes the first column of the
    /// first line.
    #[test]
    fn an_anchor_on_the_first_line_is_still_column_zero() {
        let src = "#[cfg(test)]\nmod tests { fn t() { create_canonical_schema(); } }\n";
        assert!(!production_half(src).contains("create_canonical_schema"));
    }

    /// The declared residual, asserted so it cannot silently widen: the joined
    /// sequence inside a string still truncates. No `bins/` file holds one.
    /// If this test ever fails, the gate got STRONGER — delete it and say so.
    #[test]
    fn the_declared_residual_is_exactly_as_wide_as_documented() {
        let src = "const S: &str = \"\n#[cfg(test)]\nmod x {\";\npub fn p() { create_canonical_schema(); }\n";
        assert!(!production_half(src).contains("create_canonical_schema"));
    }

    #[test]
    fn catches_whitespace_before_the_paren() {
        // `#![allow (clippy::all)]` is legal Rust and must not evade the gate.
        let src = "#![allow (clippy::all)]\nfn main() {}\n";
        assert_eq!(blanket_clippy_allow_lines(src), vec![1]);
    }

    #[test]
    fn unclosed_block_does_not_swallow_the_next_opener() {
        // A malformed never-closed opener must not absorb a following real blanket.
        let src = "#![allow(dead_code\n#![allow(clippy::pedantic)]\nfn f() {}\n";
        assert_eq!(blanket_clippy_allow_lines(src), vec![2]);
    }

    #[test]
    fn lint_baseline_drift_is_detected() {
        // A manifest whose clippy table dropped indexing_slicing ≠ the baseline.
        let manifest = "[workspace.lints.clippy]\nunwrap_used = \"warn\"\n";
        let baseline =
            "[workspace.lints.clippy]\nunwrap_used = \"warn\"\nindexing_slicing = \"warn\"\n";
        let m = normalized_lint_lines(manifest, &["workspace.lints.clippy"]);
        let b = normalized_lint_lines(baseline, &["workspace.lints.clippy"]);
        assert_ne!(m, b, "drift must be visible after normalization");
    }

    /// The no-paid-transport-by-default rule, both spellings: a dependency `features`
    /// array and a `default` feature each enable the paid transport in a
    /// default build, and each is refused; the clean shape passes.
    #[test]
    fn a_manifest_that_enables_real_model_by_default_is_refused() {
        let via_dep: toml::Value = toml::from_str(
            "[dependencies]\nrift-recall-eval = { path = \"x\", features = [\"real-model\"] }",
        )
        .unwrap();
        let via_table: toml::Value = toml::from_str(
            "[dependencies.rift-recall-eval]\npath = \"x\"\nfeatures = [\"real-model\"]",
        )
        .unwrap();
        let via_default: toml::Value =
            toml::from_str("[features]\ndefault = [\"real-model\"]\nreal-model = []").unwrap();
        for (label, m) in [
            ("dep", &via_dep),
            ("table", &via_table),
            ("default", &via_default),
        ] {
            assert_eq!(
                real_model_enabled_by_default(m, "t/Cargo.toml").len(),
                1,
                "{label} must be refused"
            );
        }
        let clean: toml::Value = toml::from_str(
            "[dependencies]\nrift-recall-eval = { path = \"x\" }\n[features]\nreal-model = [\"rift-recall-eval/real-model\"]",
        )
        .unwrap();
        assert!(real_model_enabled_by_default(&clean, "t/Cargo.toml").is_empty());
        assert!(real_model_is_opt_in(&workspace_root()).unwrap().is_empty());
    }

    /// The matcher's bounds-checking feature is asserted on this workspace,
    /// and its absence is a violation however the pin is spelled.
    #[test]
    fn a_fuzzy_matcher_pin_without_bounds_checked_reads_is_refused() {
        assert!(fuzzy_matcher_reads_within_bounds(&workspace_root())
            .unwrap()
            .is_empty());
        let (crate_name, feature) = FUZZY_MATCHER;
        let parsed = |body: &str| body.parse::<toml::Value>().unwrap();
        let refused = [
            format!("[workspace.dependencies]\n{crate_name} = \"=0.11.0\"\n"),
            format!(
                "[workspace.dependencies]\n{crate_name} = {{ version = \"=0.11.0\", features = [\"other\"] }}\n"
            ),
            format!("[workspace.dependencies.{crate_name}]\nversion = \"=0.11.0\"\n"),
        ];
        for body in &refused {
            assert_eq!(
                matcher_pin_carries_bounds(&parsed(body)).len(),
                1,
                "a pin without the feature must be refused: {body}"
            );
        }
        let kept = [
            format!(
                "[workspace.dependencies]\n{crate_name} = {{ version = \"=0.11.0\", features = [\"{feature}\"] }}\n"
            ),
            format!(
                "[workspace.dependencies.{crate_name}]\nversion = \"=0.11.0\"\nfeatures = [\n  \"{feature}\",\n]\n"
            ),
            // A workspace that does not use the matcher is held to nothing.
            "[workspace.dependencies]\nserde = \"1\"\n".to_owned(),
        ];
        for body in &kept {
            assert!(
                matcher_pin_carries_bounds(&parsed(body)).is_empty(),
                "the same fact, spelled another way: {body}"
            );
        }
    }

    /// The resolved-graph screen names a network crate by its first word and
    /// fails closed on an empty listing rather than passing it.
    #[test]
    fn a_resolved_graph_naming_a_network_crate_is_refused_and_an_empty_one_fails_closed() {
        let clean =
            "rift v0.0.1 (/w/bins/rift)\nserde v1.0.0\nrift-memd v0.0.1 (/w/bins/rift-memd)\n";
        assert!(network_crates_in_tree(clean).unwrap().is_empty());
        let dirty = format!("{clean}ureq v3.0.0\nhyper v1.0.0\nureq v3.0.0\n");
        assert_eq!(
            network_crates_in_tree(&dirty).unwrap(),
            vec!["hyper", "ureq"]
        );
        assert!(
            network_crates_in_tree("").is_err(),
            "empty output must fail closed"
        );
        assert!(network_crates_in_tree("\n\n").is_err());
        assert!(no_network_crate_in_the_resolved_graph(&workspace_root())
            .unwrap()
            .is_empty());
    }

    /// The dialect job's suite list is held by set equality: a dropped
    /// `--test`, an unknown one, and an absent job are each a violation; the
    /// real workflow has none.
    #[test]
    fn the_dialect_job_is_held_to_the_suite_list_both_ways() {
        let job = |run: &str| {
            format!("jobs:\n  red-team:\n    steps:\n      - run: x\n  dialect-conformance:\n    runs-on: ubuntu-latest\n    steps:\n      - run: {run}\n  miri:\n    steps:\n      - run: cargo test --test not_ours\n")
        };
        let full = DIALECT_SUITES
            .iter()
            .map(|s| format!("--test {s}"))
            .collect::<Vec<_>>()
            .join(" ");
        let real = job(&format!("cargo test -p rift-shadow {full}"));
        let body = job_body(&real, "dialect-conformance");
        assert!(!body.contains("not_ours"), "the body stops at the next job");
        let mut got = test_targets(body);
        got.sort_unstable();
        let mut want: Vec<String> = DIALECT_SUITES.iter().map(|s| (*s).to_owned()).collect();
        want.sort_unstable();
        assert_eq!(got, want);
        let dropped = job("cargo test -p rift-shadow --test legacy_import_golden");
        assert!(!list_violations(
            "x",
            &want,
            &test_targets(job_body(&dropped, "dialect-conformance"))
        )
        .is_empty());
        let unknown = job(&format!("cargo test {full} --test stranger"));
        assert!(!list_violations(
            "x",
            &test_targets(job_body(&unknown, "dialect-conformance")),
            &want
        )
        .is_empty());
        assert!(job_body("jobs:\n  miri:\n    steps: []\n", "dialect-conformance").is_empty());
        assert!(dialect_conformance_job_runs_every_suite(&workspace_root())
            .unwrap()
            .is_empty());
    }
}
