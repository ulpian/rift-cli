//! Rendering a sweep for a reader.
//!
//! Every generated sentence that names a cap is **formatted from the
//! constant**, never restated beside it: a cap spelled in prose is a second
//! declaration of it, and the two went out of step once already.

use super::{
    arm::{CORPUS_DIR, MAX_TOTAL_BYTES},
    phase::{exclusive_phases, PhaseAudit},
    sweep::HookLatencyReport,
};
use crate::leg::LegStatus;

/// The sentence every absolute in this project's benchmarks travels with.
///
/// It is one sentence and it retires two mistakes: an absolute here moves about
/// **2x with machine load**, which is why every arm is quoted against a control
/// measured in the same run. Fresh Unix installs dispatch each unchained hook
/// through a direct executable link; a preserved user-hook chain and non-Unix
/// installs retain the shell shim. Process-launch cost is therefore an observed
/// implementation term, not an immutable hardware floor.
pub const MACHINE_CLASS_NOTE: &str = "**Machine class.** Every absolute here is one machine's \
     measurement and moves about 2x with load, which is why each arm is quoted against a \
     bare-commit control measured in the SAME run and the ratio is the portable number. Fresh \
     Unix installs invoke unchained hooks through direct executable links; preserved user-hook \
     chains and non-Unix installs retain shell shims. Process launch is part of the measured \
     implementation, and nothing here may be generalised into a product claim about another \
     machine.";

impl HookLatencyReport {
    /// Render the sweep.
    #[must_use]
    pub fn to_markdown(&self) -> String {
        let mut out = format!(
            "# Hook latency — git-latency sweep\n\n\
             - harness `{}`, artifact schema {}, generated {}\n\
             - host: {} / {} ({} logical CPUs)\n- control git: {}\n\
             - protocol: {} warmup + {} measured commits per arm\n\
             - budget: **{} ms** of git latency (owner-set, hard)\n\n",
            self.harness_version,
            self.schema_version,
            self.generated_at_unix,
            self.host.os,
            self.host.arch,
            self.host
                .logical_cpus
                .map_or_else(|| "?".to_owned(), |n| n.to_string()),
            self.git_version,
            self.protocol.warmup,
            self.protocol.iterations,
            self.budget_ms,
        );
        self.subject_section(&mut out);
        self.arms_section(&mut out);
        self.corpus_section(&mut out);
        self.anatomy_section(&mut out);
        self.oracles_section(&mut out);
        self.phases_section(&mut out);
        out.push_str(&format!("\n{MACHINE_CLASS_NOTE}\n"));
        out
    }

    fn subject_section(&self, out: &mut String) {
        out.push_str("## Subject\n\n");
        match &self.subject {
            LegStatus::Present {
                binary,
                version,
                source,
                binary_mtime_unix,
                git_describe,
                dirty,
            } => out.push_str(&format!(
                "`{}` — {version}, resolved via {source}, built {}\n\ncommit `{}`{}\n",
                binary.display(),
                binary_mtime_unix.map_or_else(|| "?".to_owned(), |t| t.to_string()),
                git_describe.as_deref().unwrap_or("UNKNOWN"),
                match dirty {
                    // Printed as a sentence rather than a flag: a dirty subject
                    // names a tree that exists on no other machine, and a
                    // reader has to meet that fact beside the number, not two
                    // sections away in a floors file.
                    Some(true) =>
                        " — **DIRTY working tree**: this run is a record and may not \
                                   derive a floor",
                    Some(false) => "",
                    None => " — provenance unknown, so this run may not derive a floor",
                },
            )),
            LegStatus::Absent { reason } => {
                out.push_str(&format!("**no subject binary**: {reason}\n"));
            }
        }
        if let Some(clock) = &self.source_clock {
            out.push_str(&format!(
                "\nNewest source in the checkout: `{}` at {} ({} files).\n",
                clock.newest_path.display(),
                clock.newest_unix,
                clock.files
            ));
        }
    }

    fn arms_section(&self, out: &mut String) {
        out.push_str(
            "\n## Arms\n\n`hook latency` is this arm's p50 minus the control's, in the same \
             run; `× bare` is the ratio, and the ratio is the portable number.\n\n\
 | arm | p50 | min | hook latency | × bare | verdict |\n|---|---|---|---|---|---|\n",
        );
        let control = self.control().copied();
        for a in &self.arms {
            let Some(stats) = a.stats else {
                out.push_str(&format!(
                    "| `{}` | — | — | — | — | FAILED — {} |\n",
                    a.arm.id,
                    a.error.as_deref().unwrap_or("no samples")
                ));
                continue;
            };
            let load = a
                .attestation
                .map_or_else(|| "load1 unknown".to_owned(), |x| x.describe());
            let (latency, ratio, verdict) = match control {
                Some(_) if a.arm.is_control() => {
                    ("—".to_owned(), "1.00×".to_owned(), "control".to_owned())
                }
                Some(c) => {
                    let ns = a.latency_ns(&c).unwrap_or_default();
                    let over = ns > self.budget_ms.saturating_mul(1_000_000);
                    (
                        crate::report::ms(ns),
                        a.ratio(&c)
                            .map_or_else(|| "—".to_owned(), |r| format!("{r:.2}×")),
                        if over {
                            "**BREACH**".to_owned()
                        } else {
                            "meets".to_owned()
                        },
                    )
                }
                None => (
                    "—".to_owned(),
                    "—".to_owned(),
                    "**no control** — nothing to quote against".to_owned(),
                ),
            };
            out.push_str(&format!(
                "| `{}` | {} | {} | {latency} | {ratio} | {verdict} · {load} |\n",
                a.arm.id,
                crate::report::ms(stats.p50_ns),
                crate::report::ms(stats.min_ns),
            ));
        }
        for a in &self.arms {
            out.push_str(&format!("\n- `{}` — {}\n", a.arm.id, a.arm.title));
        }
    }

    /// What each opted-in arm actually handed capture, in both units.
    fn corpus_section(&self, out: &mut String) {
        let rows: Vec<_> = self
            .arms
            .iter()
            .filter_map(|a| a.arm.include.map(|c| (a, c)))
            .collect();
        if rows.is_empty() {
            return;
        }
        out.push_str(&format!(
            "\n## Opted-in corpus\n\n`MAX_TOTAL_BYTES` is {cap} and is charged on bytes \
                 **read**; `Walk::file` prefixes every content line with one space, so N bytes \
                 contribute up to **2N + 2** to the document the redactor is handed. A full \
                 budget therefore hands on just over {doubled}, not {cap}. Both numbers are \
                 published, because an arm described by one figure alone misstates the ceiling \
                 in one direction or the other.\n\n\
 | arm | files | bytes read | budget | document | factor | files read (explain) \
 |\n|---|---|---|---|---|---|---|\n",
            cap = human_bytes(MAX_TOTAL_BYTES),
            doubled = human_bytes(MAX_TOTAL_BYTES * 2),
        ));
        for (result, corpus) in rows {
            let read = corpus.bytes();
            out.push_str(&format!(
                "| `{}` | {} | {read} | {} | {} | {:.2}× | {} |\n",
                result.arm.id,
                corpus.files,
                if read >= MAX_TOTAL_BYTES {
                    format!(
                        "**at/over the {} read ceiling**",
                        human_bytes(MAX_TOTAL_BYTES)
                    )
                } else {
                    "under".to_owned()
                },
                corpus.document_bytes(CORPUS_DIR),
                corpus.document_factor(CORPUS_DIR),
                result
                    .oracles
                    .explain_files_read
                    .map_or_else(|| "—".to_owned(), |n| n.to_string()),
            ));
        }
        // **The corroboration rule, turned on itself.** `explain_files_read` was `null` in
        // 96 of 96 opted-in arm-runs across 18 artifacts and the column rendered
        // `—` every time while saying nothing about it. So every "bytes read"
        // figure in this report — including the ceiling claim that anchored the
        // budget argument — was harness-side arithmetic confirmed by nothing the
        // product said, and the one probe built to catch exactly that never
        // answered. It is also the check that would have caught the
        // `MAX_TOTAL_BYTES` cut on its own. Silence is now stated where the
        // numbers are.
        let unconfirmed: Vec<String> = self
            .arms
            .iter()
            .filter(|a| a.arm.include.is_some() && a.oracles.explain_files_read.is_none())
            .map(|a| format!("`{}`", a.arm.id))
            .collect();
        if !unconfirmed.is_empty() {
            out.push_str(&format!(
                "\n**UNCONFIRMED BY THE PRODUCT: {}.** `files read (explain)` is `—` for these \
                 arms, so every byte figure in their rows is the harness's own arithmetic over \
                 the corpus it wrote — nothing the product reported back agrees with it. That \
                 is the corroboration probe failing *silently*, which is worse than no probe \
                 because it occupies the slot where a check would go. Read those byte columns \
                 as declarations, not measurements, until it answers.\n",
                unconfirmed.join(", ")
            ));
        }
        out.push_str(
            "\nThe factor here is near 1.0 because these lines are long. The 2× is the              pathological end — a file that is all newlines, where the one-space prefix is the              whole line — so a full budget hands on nearly twice the read ceiling. Neither end is the other, and this table says which one each arm is.\n\n`files read (explain)` is `rift checkpoint explain`'s **own** label, not a span              count: the inclusion walk's record count is on no CLI surface today, so the              harness publishes the figure the product publishes rather than relabelling it as              something it has not established.\n",
        );
    }

    /// Curve, units, and attribution in one table — what a decision needs.
    fn anatomy_section(&self, out: &mut String) {
        let rows = self.anatomy();
        if rows.is_empty() {
            return;
        }
        let breached: Vec<&str> = rows
            .iter()
            .filter(|r| r.breaches)
            .map(|r| r.arm.as_str())
            .collect();
        out.push_str(&format!(
            "\n## Anatomy — the curve, the units, and the phase that owns it\n\n{}\n\n**Magnitude and attribution come from different commits.** `git latency` is the \
             median of the timed commits minus the control's; every phase column comes from \
             the arm's one **extra untimed** commit with the timer on. Read the residue as an \
             anatomy of a representative commit beside the magnitude of the median, never as \
             an exact decomposition of one commit.\n\n`predict_id` here is **exclusive** — `redact` subtracted — so the columns are \
             disjoint and may be added.\n\n| arm | bytes read | handed on | ×read | git latency | hook bodies | outside \
             bodies | `redact` | `predict_id` less `redact` | verdict |\n|---|---|---|---|---|---|---|---|---|---|\n",
            if breached.is_empty() {
                "No arm is over the budget in this run.".to_owned()
            } else {
                format!(
                    "**BREACH: {}.** The budget is {} ms of git latency and is HARD (owner-set). \
                     The corpus was not adjusted in response — the arms are the \
                     sizes they were declared at.",
                    breached
                        .iter()
                        .map(|a| format!("`{a}`"))
                        .collect::<Vec<_>>()
                        .join(", "),
                    self.budget_ms
                )
            }
        ));
        for r in &rows {
            let us = |v: u64| format!("{:.2} ms", v as f64 / 1_000.0);
            out.push_str(&format!(
                "| `{}` | {} | {} | {} | {} | {} | {} | {} | {} | {} |\n",
                r.arm,
                r.bytes_read
                    .map_or_else(|| "—".to_owned(), |b| b.to_string()),
                r.document_bytes
                    .map_or_else(|| "—".to_owned(), |b| b.to_string()),
                r.document_factor
                    .map_or_else(|| "—".to_owned(), |f| format!("{f:.2}×")),
                us(r.git_latency_us),
                us(r.hook_bodies_us),
                us(r.outside_hook_bodies_us),
                us(r.redact_us),
                us(r.predict_id_excl_us),
                if r.breaches { "**BREACH**" } else { "meets" },
            ));
        }
        if rows.iter().any(|r| r.bodies_exceed_latency) {
            out.push_str(
                "\n**One arm's hook bodies measured larger than its median git latency.** \
                 That is possible across two populations — a slow phase commit against a fast \
                 median — and it is reported rather than clamped, because the alternative is a \
                 zero in the `outside bodies` column that would read as \"process creation is \
                 free\".\n",
            );
        }
        out.push_str(
            "\n`outside bodies` is measured rather than assumed: process creation, hook \
             dispatch, and git's own cost of running the three hooks. Fresh Unix installs use \
             direct executable links for unchained hooks; preserved user-hook chains and \
             non-Unix installs retain shell shims.\n",
        );
    }

    fn oracles_section(&self, out: &mut String) {
        out.push_str(
            "\n## Oracles — what each arm exercised\n\n\
             Reported for every arm including the control, because a check that only ever \
             confirms is not a check: the idle arm's **absent** trailer is what makes the live \
             arm's present one mean something.\n\n\
 | arm | projections | trailer | verify | events |\n|---|---|---|---|---|\n",
        );
        for a in &self.arms {
            let projections = if a.oracles.projections.is_empty() {
                "none".to_owned()
            } else {
                a.oracles
                    .projections
                    .iter()
                    .map(|(name, bytes)| format!("{name} {bytes} B"))
                    .collect::<Vec<_>>()
                    .join("; ")
            };
            let verify = match (a.oracles.verify_checkpoints, a.oracles.verify_shards) {
                (Some(c), Some(s)) => format!("{c} checkpoint(s), {s} shard(s)"),
                _ => "—".to_owned(),
            };
            out.push_str(&format!(
                "| `{}` | {projections} | {} | {verify} | {} |\n",
                a.arm.id,
                if a.oracles.trailer_present {
                    "present"
                } else {
                    "absent"
                },
                a.oracles
                    .captured_events
                    .map_or_else(|| "—".to_owned(), |n| n.to_string()),
            ));
        }
        if self.arms.iter().any(|a| a.oracles.projections.len() > 1) {
            out.push_str(
                "\n**More than one projection under a single arm's cache.** That is the shape \
                 of the defect this table exists for: the number may be about a store rift \
                 never opened.\n",
            );
        }
    }

    fn phases_section(&self, out: &mut String) {
        let any = self.arms.iter().any(|a| !a.phases.is_empty());
        if !any {
            return;
        }
        out.push_str(
            "\n## Phases\n\nFrom one extra **untimed** commit per arm with \
             `RIFT_HOOK_TIMING=1`, straight out of the shipped binary. Untimed because the \
             timer takes clock readings the shipped default never takes.\n\n\
             **Read the `exclusive` column, and never sum `recorded`.** The timer records on \
             completion, so a nested phase prints before its parent and the parent's figure \
             already contains it: `predict_id` INCLUDES `redact`. `recorded` is what the \
             binary printed; `exclusive` has every declared child subtracted, so those values \
             are disjoint and may be added. Summing `recorded` would double-count the \
             redaction — the one quantity the opted-in arms exist to separate.\n\n\
 | arm | hook | phase | recorded µs | exclusive µs |\n|---|---|---|---|---|\n",
        );
        for a in &self.arms {
            for (hook, phases) in &a.phases {
                let exclusive = exclusive_phases(phases);
                for (phase, us) in phases {
                    let excl = exclusive.get(phase).copied().unwrap_or(*us);
                    let note = if excl == *us {
                        String::new()
                    } else {
                        " (parent, `redact` subtracted)".to_owned()
                    };
                    out.push_str(&format!(
                        "| `{}` | {hook} | {phase} | {us} | {excl}{note} |\n",
                        a.arm.id
                    ));
                }
            }
        }
        self.phase_audit_section(out);
    }

    /// The free oracle on the phase table: `total_us` is measured independently
    /// of the phases, so disjoint phases can never exceed it.
    fn phase_audit_section(&self, out: &mut String) {
        out.push_str(
            "\n### Phase audit\n\n`total` is measured independently of the phases, so \
             disjoint phases can never exceed it. `over-attributed` is therefore a proof that \
             something is nested which the harness does not declare — not a rounding \
             remark.\n\n| arm | hook | total µs | attributed µs | unattributed µs | verdict \
 |\n|---|---|---|---|---|---|\n",
        );
        for a in &self.arms {
            for (hook, phases) in &a.phases {
                let audit = PhaseAudit::of(phases);
                out.push_str(&format!(
                    "| `{}` | {hook} | {} | {} | {} | {} |\n",
                    a.arm.id,
                    audit.total_us,
                    audit.attributed_us,
                    audit.unattributed_us,
                    if audit.over_attributed {
                        "**OVER-ATTRIBUTED — an undeclared nested phase**"
                    } else {
                        "adds up"
                    },
                ));
            }
        }
    }
}

/// A byte count as prose (`8 KiB`, `1 MiB`, `2,048 B`).
///
/// Exists because three generated sentences hard-coded `1 MiB` and went on
/// saying it after the constant became `8 * 1024` — one of them contradicting
/// itself 48 lines later in the same generated section. A cap that appears in
/// prose must be **formatted from the constant**, or the prose is a second
/// declaration of it that nothing keeps in step.
#[must_use]
pub fn human_bytes(n: usize) -> String {
    if n >= 1024 * 1024 && n.is_multiple_of(1024 * 1024) {
        format!("{} MiB", n / (1024 * 1024))
    } else if n >= 1024 && n.is_multiple_of(1024) {
        format!("{} KiB", n / 1024)
    } else {
        format!("{} B", thousands(n))
    }
}

/// `8192` -> `"8,192"`. Correct for every magnitude, which
/// `format!("{},{:03}", n / 1000, n % 1000)` was not: it rendered 512 as
/// `"0,512"`, and a guard comparing against that silently never matched.
#[must_use]
pub fn thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::hooklat::{parse_timings, tests_support};

    /// The probe was `null` in 96 of 96 opted-in arm-runs and the report said
    /// nothing. A probe that fails silently is worse than no probe: it occupies
    /// the slot where a check would go.
    #[test]
    fn an_unanswered_product_probe_is_stated_loudly_rather_than_rendered_as_a_dash() {
        let mut report = tests_support::report_with(&[("A_bare", 37), ("H3_newline_8k", 200)]);
        let md = report.to_markdown();
        assert!(md.contains("UNCONFIRMED BY THE PRODUCT"), "{md}");
        assert!(md.contains("`H3_newline_8k`"), "{md}");
        assert!(
            md.contains("declarations, not measurements"),
            "it must say what the byte columns are worth: {md}"
        );

        // When the product does answer, the warning goes away — a notice that
        // is always printed is wallpaper.
        report
            .arms
            .iter_mut()
            .find(|a| a.arm.id == "H3_newline_8k")
            .unwrap()
            .oracles
            .explain_files_read = Some(8);
        let md = report.to_markdown();
        assert!(!md.contains("UNCONFIRMED BY THE PRODUCT"), "{md}");
    }

    /// The generated ceiling sentence must be **formatted from the constant**,
    /// not restated beside it.
    ///
    /// It was restated, and it went on saying `1 MiB` after the constant became
    /// `8 * 1024` — contradicting itself 48 lines later in the same generated
    /// section. A cap that appears in prose is a second declaration of it, and
    /// nothing was keeping the two in step.
    #[test]
    fn the_generated_ceiling_prose_is_derived_from_the_constant() {
        let report = tests_support::report_with(&[("A_bare", 37), ("H3_newline_8k", 200)]);
        let md = report.to_markdown();
        let cap = human_bytes(MAX_TOTAL_BYTES);
        assert!(
            md.contains(&format!("`MAX_TOTAL_BYTES` is {cap}")),
            "the corpus header must state the live cap ({cap}):\n{md}"
        );
        assert!(
            md.contains(&format!("just over {}", human_bytes(MAX_TOTAL_BYTES * 2))),
            "and the doubled figure with it:\n{md}"
        );
        // No stale magnitude may survive anywhere in a generated report.
        for stale in ["1 MiB", "2 MiB", "512 KiB"] {
            assert!(
                !md.contains(stale),
                "a generated report still says {stale} while the cap is {cap}:\n{md}"
            );
        }
    }

    /// **No line of THIS renderer's markdown may be indented.**
    ///
    /// `report::Report` is a different renderer with its own tables and has its
    /// own copy of this guard — found by planting an indented header in *that*
    /// file and watching this test pass. CommonMark renders 4+
    /// leading spaces as a code block, so a table emitted that way is not a
    /// table — 140 lines across the 18 committed artifacts were rendering as
    /// code. The generator was fixed once for the ratchet banner and the tables
    /// were missed, which is why this asserts over *every* section rather than
    /// over the one that was noticed.
    #[test]
    fn no_generated_line_is_indented_into_a_code_block() {
        let mut report = tests_support::report_with(&[
            ("A_bare", 37),
            ("B0_idle", 90),
            ("G3_line_8k", 150),
            ("H3_newline_8k", 200),
        ]);
        report
            .arms
            .iter_mut()
            .find(|a| a.arm.id == "H3_newline_8k")
            .unwrap()
            .phases = parse_timings(
            "rift-timing hook=prepare-commit-msg total_us=90000 predict_id_us=70000 \
             redact_us=66000\n",
        );
        for (n, line) in report.to_markdown().lines().enumerate() {
            assert!(
                !line.starts_with("    ") || line.trim().is_empty(),
                "line {n} is indented and will render as a code block: {line:?}"
            );
        }
    }

    /// **B2: the committed artifacts stay indentation-clean.**
    ///
    /// I reflowed 145 leading-space lines down to 5 and then *wrote* that the
    /// property was "asserted, not claimed" — while no assertion existed. The
    /// substance was true and the sentence was not, which is the pattern this
    /// suite exists to break. Here is the assertion.
    ///
    /// It also closes the other direction the verifier named: without it the
    /// 140 lines could silently return, since fixing the generator only governs
    /// files that do not exist yet.
    ///
    /// **There is no exception, because none was needed.** An earlier version
    /// carved out `hook-latency-1787120100.md` for "5 survivors", and the
    /// carve-out was both wrong and dead weight: it skipped *every* line in
    /// that file not starting with `"| "` — a whole-file pattern scoped by
    /// filename, while its own text claimed the opposite — and deleting it
    /// leaves the guard green, because **zero 4-space-indented lines exist in
    /// any of the 18 artifacts**. The 5 survivors are ` |` single-space
    /// continuations and unindented `Usage:` text, which the 4-space rule never
    /// covered in the first place. An exception protecting nothing is a hole
    /// with a reason attached.
    #[test]
    fn the_committed_artifacts_are_indentation_clean() {
        let dir = crate::leg::default_repo_root().join("benchmarks/hook-latency");
        let Ok(entries) = fs::read_dir(&dir) else {
            assert!(
                !crate::leg::default_repo_root().join("benchmarks").is_dir(),
                "benchmarks/ exists but {} does not — the guard would skip itself",
                dir.display()
            );
            return;
        };
        let mut checked = 0usize;
        for path in entries.flatten().map(|e| e.path()) {
            if path.extension().and_then(|e| e.to_str()) != Some("md") {
                continue;
            }
            let Ok(text) = fs::read_to_string(&path) else {
                continue;
            };
            checked += 1;
            for (n, line) in text.lines().enumerate() {
                if line.trim().is_empty() {
                    continue;
                }
                assert!(
                    !line.starts_with("    "),
                    "{}:{} is indented and renders as a code block, not a table: {line:?}",
                    path.display(),
                    n + 1
                );
            }
        }
        assert!(checked >= 18, "only {checked} artifacts were checked");
    }

    #[test]
    fn byte_prose_is_correct_at_every_magnitude() {
        assert_eq!(human_bytes(8 * 1024), "8 KiB");
        assert_eq!(human_bytes(16 * 1024), "16 KiB");
        assert_eq!(human_bytes(1024 * 1024), "1 MiB");
        // The formatter that rendered 512 as "0,512" and so never matched.
        assert_eq!(thousands(512), "512");
        assert_eq!(thousands(2_048), "2,048");
        assert_eq!(thousands(8_192), "8,192");
        assert_eq!(thousands(1_048_576), "1,048,576");
        assert_eq!(human_bytes(2_048), "2 KiB");
        assert_eq!(human_bytes(999), "999 B");
    }
}
