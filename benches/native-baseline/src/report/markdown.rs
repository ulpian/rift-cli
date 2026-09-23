//! Rendering a run for a reader.
//!
//! Zero and absent are different claims and never collapse: a measure that ran
//! and moved nothing prints its zero, a measure nobody counted prints a dash,
//! and a rate with no denominator is not invented to fill the cell.

use super::{artifact::Report, outcome::LegOutcome};
use crate::{
    leg::{LegId, LegStatus},
    measure::{BudgetVerdict, Stats},
};

impl Report {
    /// Render the human-readable report.
    #[must_use]
    pub fn to_markdown(&self) -> String {
        let mut out = String::new();
        out.push_str("# rift vs the pinned capture backend\n\n");
        out.push_str(&format!(
            "- harness `{}`, report schema {}\n- host: {} / {} ({} logical CPUs)\n\
             - peak RSS via {}\n\n",
            self.harness_version,
            self.schema_version,
            self.host.os,
            self.host.arch,
            self.host
                .logical_cpus
                .map_or_else(|| "?".to_owned(), |n| n.to_string()),
            self.host.rss_probe,
        ));
        out.push_str(&format!(
            "- machine: {}\n\n",
            self.attestation
                .map_or_else(|| "load1 unknown".to_owned(), |a| a.describe())
        ));
        self.legs_section(&mut out);
        self.fixtures_section(&mut out);
        self.measures_sections(&mut out);
        self.caveats_section(&mut out);
        self.unmeasurable_section(&mut out);
        self.budgets_section(&mut out);
        self.spawns_section(&mut out);
        self.protocol_note(&mut out);
        self.not_measured_section(&mut out);
        out
    }

    /// The sentences that must be read with the numbers above.
    ///
    /// Its own section, immediately after the measures and before the budgets,
    /// because a caveat placed after the verdict is one a reader meets only if
    /// they were not looking for the verdict.
    fn caveats_section(&self, out: &mut String) {
        let rows: Vec<_> = self
            .scenarios
            .iter()
            .filter_map(|s| s.caveat.as_ref().map(|c| (s.id, c)))
            .collect();
        if rows.is_empty() {
            return;
        }
        out.push_str("\n## Read these with the numbers\n\n");
        for (id, caveat) in rows {
            out.push_str(&format!("- **`{}`** — {caveat}\n", id.as_str()));
        }
    }

    /// Rows that ran and moved nothing.
    ///
    /// Its own section rather than a footnote under Budgets, because a row can
    /// be empty without carrying a budget at all — and the first version of
    /// this note lived under Budgets, where exactly that row would have been
    /// silent.
    fn unmeasurable_section(&self, out: &mut String) {
        let empty: Vec<_> = self
            .scenarios
            .iter()
            .filter(|s| {
                s.legs
                    .get(LegId::Rift.as_str())
                    .and_then(LegOutcome::stats)
                    .is_some_and(Stats::moved_no_work)
            })
            .map(|s| format!("`{}`", s.id.as_str()))
            .collect();
        if empty.is_empty() {
            return;
        }
        out.push_str(&format!(
            "\n## Unmeasurable as a rate\n\n{} ran and moved **zero work**, so they have no \
             rate: `0/s` would read as \"it did work, slowly\" and blame the code for a corpus \
             that handed it nothing. The zero is published as the work count rather than \
             hidden — a measure whose denominator is empty has never been about any work, and \
             that is the finding, not a footnote.\n",
            empty.join(", ")
        ));
    }

    /// The declared ceilings next to the numbers that answer them, plus the proof
    /// each number did the work it claims.
    fn budgets_section(&self, out: &mut String) {
        let rows: Vec<_> = self
            .scenarios
            .iter()
            .filter(|s| s.budget.is_some())
            .collect();
        if rows.is_empty() {
            return;
        }
        out.push_str(
            "\n## Budgets\n\n | measure | rift p50 | rift p95 | rate | work/it | budget | verdict | ingest proof |\n\
 |---|---|---|---|---|---|---|---|\n",
        );
        for s in rows {
            let outcome = s.legs.get(LegId::Rift.as_str());
            let stats = outcome.and_then(LegOutcome::stats);
            let cell =
                |pick: fn(&Stats) -> u64| stats.map_or_else(|| "—".to_owned(), |st| ms(pick(st)));
            let budget = s
                .budget
                .map_or_else(String::new, |b| b.describe(s.work_unit.as_deref()));
            let verdict = s.budget_verdict();
            let proof = match outcome {
                Some(LegOutcome::Measured {
                    ingest: Some(check),
                    ..
                }) => format!(
                    "+{} events (≥ {} required)",
                    check.events_after.saturating_sub(check.events_before),
                    check.required_growth
                ),
                Some(LegOutcome::Measured { ingest: None, .. }) => "none available".to_owned(),
                _ => "—".to_owned(),
            };
            out.push_str(&format!(
                "| `{}` | {} | {} | {} | {} | {budget} | {verdict} | {proof} |\n",
                s.id.as_str(),
                cell(|st| st.p50_ns),
                cell(|st| st.p95_ns),
                rate_cell(stats, s.work_unit.as_deref()),
                work_cell(stats, s.work_unit.as_deref()),
            ));
        }
        if self.scenarios.iter().any(|s| {
            matches!(
                s.budget_verdict_of(),
                Some(BudgetVerdict::Latency {
                    p50_met: true,
                    p95_met: false
                })
            )
        }) {
            out.push_str(
                "\n**A row reading `p50 met · p95 OVER` is reported as two facts on purpose, \
                 by owner decision.** Collapsing the pair into one word would either credit the \
                 budget with a p95 it does not have or fail a median that genuinely meets it. \
                 Process creation and hook dispatch remain visible in the same-run control and \
                 the hook phase breakdown.\n",
            );
        }
        let no_ops: Vec<_> = self
            .scenarios
            .iter()
            .filter(|s| s.budget.is_some() && s.is_no_op_path())
            .map(|s| format!("`{}`", s.id.as_str()))
            .collect();
        if !no_ops.is_empty() {
            out.push_str(&format!(
                "\nThe `n/a` rows ({}) re-deliver an event whose ingest is a designed no-op. \
                 Such a row is kept because the dedup short-circuit is a real hook path worth \
                 a number, and it is held to the same ceiling — but it is **not judged** \
                 against it, because passing a budget by not doing the work is not passing \
                 it.\n",
                no_ops.join(", ")
            ));
        }
    }

    /// The child processes a measured verb launches — a cost that is invisible
    /// in a wall time until someone asks what the wall time is made of.
    fn spawns_section(&self, out: &mut String) {
        let rows: Vec<_> = self
            .scenarios
            .iter()
            .filter_map(|s| {
                let counts: Vec<_> = s
                    .legs
                    .iter()
                    .filter_map(|(leg, outcome)| match outcome {
                        LegOutcome::Measured {
                            child_spawns: Some(count),
                            ..
                        } => Some((leg.clone(), count)),
                        _ => None,
                    })
                    .collect();
                (!counts.is_empty()).then_some((s, counts))
            })
            .collect();
        if rows.is_empty() {
            return;
        }
        out.push_str(
            "\n## Child processes\n\n\
             One invocation of the measured verb, counted in a separate untimed pass \
             (the counting shims cost an exec each, so they must never sit inside a \
             published timing).\n\n\
 | measure | leg | child processes |\n|---|---|---|\n",
        );
        for (scenario, counts) in rows {
            for (leg, count) in counts {
                out.push_str(&format!(
                    "| `{}` | `{leg}` | {} |\n",
                    scenario.id.as_str(),
                    count.describe()
                ));
            }
        }
    }

    /// What the percentiles in this report do and do not mean.
    fn protocol_note(&self, out: &mut String) {
        out.push_str(
            "\nPercentiles are nearest-rank over the measured samples: every figure is an \
             observed run, never an interpolation.\n",
        );
        let thin: Vec<_> = self
            .scenarios
            .iter()
            .filter(|s| s.protocol.iterations < 20 && s.legs.values().any(|o| o.stats().is_some()))
            .map(|s| format!("`{}` (n={})", s.id.as_str(), s.protocol.iterations))
            .collect();
        if !thin.is_empty() {
            out.push_str(&format!(
                "With fewer than 20 iterations the nearest-rank p95 **is** the maximum, so it \
                 carries no tail information and no threshold may be set on it: {}.\n",
                thin.join(", ")
            ));
        }
    }

    /// The legs, and the freshness evidence that says a number is about this
    /// tree. `built` and the source clock are printed together on purpose: two
    /// timestamps in different sections are an exercise for the reader, and the
    /// question "is this binary the code?" has to be answerable at a glance.
    fn legs_section(&self, out: &mut String) {
        out.push_str(
            "## Legs\n\n| leg | state | binary | version | size | built | commit |\n |---|---|---|---|---|---|---|\n",
        );
        for leg in &self.legs {
            let size = leg
                .binary_bytes
                .map_or_else(|| "—".to_owned(), |b| format!("{} KiB", b / 1024));
            match &leg.status {
                LegStatus::Present {
                    binary,
                    version,
                    source,
                    binary_mtime_unix,
                    git_describe,
                    dirty,
                } => out.push_str(&format!(
                    "| `{}` | present ({source}) | `{}` | {version} | {size} | {} | {} |\n",
                    leg.id.as_str(),
                    binary.display(),
                    binary_mtime_unix.map_or_else(|| "—".to_owned(), |t| t.to_string()),
                    commit_cell(git_describe.as_deref(), *dirty),
                )),
                LegStatus::Absent { reason } => out.push_str(&format!(
                    "| `{}` | **absent** | — | — | — | — | — |\n| | {reason} | | | | | |\n",
                    leg.id.as_str()
                )),
            }
        }
        if let Some(driver) = &self.driver {
            match driver {
                LegStatus::Present {
                    binary,
                    version,
                    source,
                    binary_mtime_unix,
                    git_describe,
                    dirty,
                } => out.push_str(&format!(
                    "| `vogon` (driver) | present ({source}) | `{}` | {version} | — | {} | {} |\n",
                    binary.display(),
                    binary_mtime_unix.map_or_else(|| "—".to_owned(), |t| t.to_string()),
                    commit_cell(git_describe.as_deref(), *dirty),
                )),
                LegStatus::Absent { reason } => out.push_str(&format!(
                    "| `vogon` (driver) | **absent** | — | — | — | — | — |\n| | {reason} | | | | | |\n"
                )),
            }
        }
        match &self.source_clock {
            Some(clock) => out.push_str(&format!(
                "\nNewest source in the checkout: `{}` at **{}** ({} files scanned). A rift                  binary older than that is refused rather than measured — a timing taken from                  a binary that predates its own sources is real and is about the wrong code.\n",
                clock.newest_path.display(),
                clock.newest_unix,
                clock.files,
            )),
            None => out.push_str(
                "\nNo source clock: the harness found no checkout to compare the binaries                  against, so **freshness was not checked** — which is not the same as checked                  and clean.\n",
            ),
        }
    }

    fn fixtures_section(&self, out: &mut String) {
        out.push_str("\n## Fixtures\n\n| fixture | files | bytes | commits | turns | head |\n|---|---|---|---|---|---|\n");
        for f in &self.fixtures {
            out.push_str(&format!(
                "| `{}` | {} | {} | {} | {} | `{}` |\n",
                f.spec.name,
                f.worktree_files,
                f.worktree_bytes,
                f.spec.commits,
                f.spec.session_turns,
                f.head_oid.get(..12).unwrap_or(&f.head_oid),
            ));
        }
    }

    fn measures_sections(&self, out: &mut String) {
        for (heading, headline) in [("Headline measures", true), ("Other measures", false)] {
            let rows: Vec<_> = self
                .scenarios
                .iter()
                .filter(|s| s.headline == headline)
                .collect();
            if rows.is_empty() {
                continue;
            }
            out.push_str(&format!(
                "\n## {heading}\n\n | measure | rift p50 | rift p95 | rate | work/it | backend p50 | backend p95 |                  speedup | n |\n|---|---|---|---|---|---|---|---|---|\n"
            ));
            for s in rows {
                let cell = |leg: LegId, pick: fn(&Stats) -> u64| {
                    s.legs
                        .get(leg.as_str())
                        .and_then(LegOutcome::stats)
                        .map_or_else(|| "—".to_owned(), |st| ms(pick(st)))
                };
                let n = s
                    .legs
                    .get(LegId::Rift.as_str())
                    .and_then(LegOutcome::stats)
                    .map_or_else(|| "0".to_owned(), |st| st.samples.to_string());
                let speedup = s
                    .comparison
                    .map_or_else(|| "—".to_owned(), |c| format!("{:.2}×", c.p50_speedup));
                let rift = s.legs.get(LegId::Rift.as_str()).and_then(LegOutcome::stats);
                out.push_str(&format!(
                    "| `{}` | {} | {} | {} | {} | {} | {} | {speedup} | {n} |\n",
                    s.id.as_str(),
                    cell(LegId::Rift, |st| st.p50_ns),
                    cell(LegId::Rift, |st| st.p95_ns),
                    rate_cell(rift, s.work_unit.as_deref()),
                    work_cell(rift, s.work_unit.as_deref()),
                    cell(LegId::Reference, |st| st.p50_ns),
                    cell(LegId::Reference, |st| st.p95_ns),
                ));
            }
        }
    }

    fn not_measured_section(&self, out: &mut String) {
        out.push_str("\n## Not measured\n\n");
        let mut any = false;
        // A leg the run did not ask for is absent from the results but present in
        // the legs table; saying so is the difference between "measured nothing"
        // and "was never asked". Said once per leg rather than once per scenario:
        // with the backend off the default set that was sixteen identical lines,
        // and a reader who skims past a repeated line skims past the ones that
        // are not repeated.
        for leg in &self.legs {
            let missing = self
                .scenarios
                .iter()
                .filter(|s| !s.legs.contains_key(leg.id.as_str()))
                .count();
            if missing > 0 {
                any = true;
                out.push_str(&format!(
                    "- `{}`: not requested by this run (`--legs`) — {missing} of {} scenarios\n",
                    leg.id.as_str(),
                    self.scenarios.len()
                ));
            }
        }
        for s in &self.scenarios {
            for (leg, outcome) in &s.legs {
                match outcome {
                    LegOutcome::Skipped { reason } => {
                        any = true;
                        out.push_str(&format!("- `{}` / `{leg}`: {reason}\n", s.id.as_str()));
                    }
                    LegOutcome::Failed { reason, .. } => {
                        any = true;
                        out.push_str(&format!(
                            "- `{}` / `{leg}`: FAILED — {reason}\n",
                            s.id.as_str()
                        ));
                    }
                    LegOutcome::Measured { .. } => {}
                }
            }
        }
        if !any {
            out.push_str("- nothing: every declared measure ran on every leg.\n");
        }
    }
}

/// Format nanoseconds as milliseconds.
#[must_use]
pub fn ms(ns: u64) -> String {
    format!("{:.2} ms", ns as f64 / 1_000_000.0)
}

/// The commit a leg's number is about.
///
/// Three renderings for three different states, and none of them is blank: a
/// clean commit, a **dirty** one (a tree that exists nowhere else, so the run
/// is a record and derives no floor), and *unknown* — which is what a leg whose
/// sources are not in this checkout honestly is, and is not the same claim as
/// clean.
fn commit_cell(describe: Option<&str>, dirty: Option<bool>) -> String {
    match (describe, dirty) {
        (Some(d), Some(true)) => format!("`{d}` **DIRTY**"),
        (Some(d), _) => format!("`{d}`"),
        (None, _) => "unknown".to_owned(),
    }
}

/// The rate cell: work per second at the median iteration, in the row's unit.
///
/// `—` where nothing counted the work. That is not a formatting nicety: a row
/// with no denominator has no rate, and printing one anyway would invent it.
fn rate_cell(stats: Option<&Stats>, unit: Option<&str>) -> String {
    if stats.is_some_and(Stats::moved_no_work) {
        return format!("**unmeasurable** — 0 {}", unit.unwrap_or("units"));
    }
    let Some(rate) = stats.and_then(Stats::rate_per_s) else {
        return "—".to_owned();
    };
    let unit = unit.unwrap_or("units");
    if rate >= 100.0 {
        format!("{rate:.0} {unit}/s")
    } else {
        format!("{rate:.2} {unit}/s")
    }
}

/// The corroboration probe, printed beside the timing it explains: how much work
/// one measured iteration actually performed.
///
/// **`0` and `—` say different things and must never be collapsed.** `0` is a
/// measure that ran and moved nothing — the case where a fast wall time is
/// meaningless and the reader has to be able to see it. `—` is a measure nobody
/// counted, where the timing may be perfectly good and simply has no
/// denominator.
fn work_cell(stats: Option<&Stats>, unit: Option<&str>) -> String {
    let Some(units) = stats.and_then(|st| st.units_per_iteration) else {
        return "—".to_owned();
    };
    format!("{units} {}", unit.unwrap_or("units"))
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, path::PathBuf};

    use super::*;
    use crate::{
        leg::LegStatus,
        measure::{Budget, BudgetVerdict, Protocol},
        report::{
            now_unix,
            tests_support::{measured, report, stats},
            IngestCheck, LegOutcome,
        },
    };

    /// The provenance, rendered where a reader meets the number.
    /// `unknown` and `clean` are different claims and the cell says which.
    #[test]
    fn the_commit_a_number_is_about_is_printed_beside_the_leg() {
        let mut legs = BTreeMap::new();
        legs.insert("rift".to_owned(), measured(2_000_000));
        let md = report(legs, None).to_markdown();
        assert!(
            md.contains(&format!("`ls-597-g{}`", "b".repeat(40))),
            "{md}"
        );
        assert!(md.contains("| commit |"), "the column exists: {md}");

        assert_eq!(commit_cell(None, None), "unknown");
        assert_eq!(commit_cell(None, Some(false)), "unknown");
        assert_eq!(commit_cell(Some("abc"), Some(false)), "`abc`");
        assert_eq!(commit_cell(Some("abc"), None), "`abc`");
        assert_eq!(commit_cell(Some("abc"), Some(true)), "`abc` **DIRTY**");
    }

    #[test]
    fn a_budget_is_published_beside_the_number_that_answers_it() {
        let mut legs = BTreeMap::new();
        legs.insert(
            "rift".to_owned(),
            LegOutcome::Measured {
                command: "rift hooks claude-code Stop".into(),
                stats: stats(40_000_000, 0),
                peak_rss_bytes: None,
                ingest: Some(IngestCheck {
                    events_before: 100,
                    events_after: 190,
                    required_growth: 30,
                }),
                child_spawns: None,
            },
        );
        let mut r = report(legs, None);
        r.scenarios[0].budget = Some(Budget::hook());
        assert_eq!(
            r.scenarios[0].budget_verdict_of(),
            Some(BudgetVerdict::Latency {
                p50_met: true,
                p95_met: true
            })
        );
        let md = r.to_markdown();
        assert!(md.contains("## Budgets"), "{md}");
        assert!(md.contains("p50 ≤ 100 ms, p95 ≤ 500 ms"), "{md}");
        assert!(md.contains("**met**"), "{md}");
        assert!(md.contains("+90 events (≥ 30 required)"), "{md}");

        // p95 is 2× p50 in this fixture, so 300 ms p50 blows both halves.
        let over = LegOutcome::Measured {
            command: "rift hooks claude-code SessionStart".into(),
            stats: stats(300_000_000, 0),
            peak_rss_bytes: None,
            ingest: None,
            child_spawns: None,
        };
        r.scenarios[0].legs.insert("rift".to_owned(), over);
        assert_eq!(
            r.scenarios[0].budget_verdict_of(),
            Some(BudgetVerdict::Latency {
                p50_met: false,
                p95_met: false
            })
        );
        let md = r.to_markdown();
        assert!(md.contains("p50 **OVER** · p95 **OVER**"), "{md}");
        // No counter, no claim.
        assert!(md.contains("none available"), "{md}");
    }

    /// The unratified-floor rule rendered: a throughput row whose floor nobody has set
    /// reports `OPEN`, never green — and the rate and its probed denominator
    /// are printed together, because a rate whose work count is invisible is
    /// the corroboration failure wearing a slash.
    #[test]
    fn an_unratified_throughput_row_reports_open_with_its_rate_and_its_work_count() {
        let mut legs = BTreeMap::new();
        legs.insert(
            "rift".to_owned(),
            LegOutcome::Measured {
                command: "rift verify".into(),
                // 2 s median, 40 checkpoints re-derived: 20 checkpoints/s.
                stats: stats(2_000_000_000, 0).with_units(40),
                peak_rss_bytes: None,
                ingest: None,
                child_spawns: None,
            },
        );
        let mut r = report(legs, None);
        r.scenarios[0].budget = Some(Budget::unratified_throughput());
        r.scenarios[0].work_unit = Some("checkpoints".to_owned());

        assert_eq!(
            r.scenarios[0].budget_verdict_of(),
            Some(BudgetVerdict::Unratified),
            "an unset floor is unjudgeable, and unjudgeable is not passed"
        );
        assert_eq!(
            r.scenarios[0].budget_verdict(),
            "OPEN — awaiting owner ratification"
        );
        let md = r.to_markdown();
        assert!(md.contains("OPEN — awaiting owner ratification"), "{md}");
        assert!(md.contains("checkpoints/s — no floor ratified"), "{md}");
        assert!(md.contains("20.00 checkpoints/s"), "{md}");
        assert!(md.contains("40 checkpoints"), "{md}");
        assert!(!md.contains("**met**"), "never green: {md}");
    }

    /// **The two-facts headline case, which had no test until a plant found
    /// that out.** Deleting the `p50 met · p95 OVER` arm and collapsing it to
    /// `**OVER**` left every test green — the one rendering the rule exists
    /// for was the one nothing covered. This is the measured shape: the
    /// median clears the hook budget while the tail does not.
    #[test]
    fn a_median_that_meets_and_a_tail_that_does_not_are_rendered_as_two_facts() {
        let mut legs = BTreeMap::new();
        legs.insert(
            "rift".to_owned(),
            LegOutcome::Measured {
                command: "git commit".into(),
                // The fixture's p95 is 2x its p50, so 80 ms clears the 100 ms
                // median ceiling while 160 ms misses... the 500 ms p95 ceiling
                // is generous, so the budget is stated explicitly instead.
                stats: stats(80_000_000, 0),
                peak_rss_bytes: None,
                ingest: None,
                child_spawns: None,
            },
        );
        let mut r = report(legs, None);
        r.scenarios[0].budget = Some(Budget::Latency {
            p50_ms: 100,
            p95_ms: 120,
        });

        assert_eq!(
            r.scenarios[0].budget_verdict_of(),
            Some(BudgetVerdict::Latency {
                p50_met: true,
                p95_met: false
            }),
            "the two facts must survive as two"
        );
        assert_eq!(
            r.scenarios[0].budget_verdict(),
            "p50 **met** · p95 **OVER**",
            "collapsing this into one word credits a p95 the budget does not have"
        );

        let md = r.to_markdown();
        assert!(md.contains("p50 **met** · p95 **OVER**"), "{md}");
        assert!(
            md.contains("reported as two facts on purpose, by owner decision"),
            "the reason travels with the verdict: {md}"
        );
        assert!(
            md.contains("Process creation and hook dispatch remain visible"),
            "and it keeps the measurable terms visible: {md}"
        );
    }

    /// The condense row's workload is authored by a binary that is not the
    /// subject, so the report has to name it. A number traceable to the tested
    /// binary but not to the binary that decided what the test *was* is only
    /// half traceable.
    #[test]
    fn the_scripted_session_driver_is_named_in_the_report_or_its_absence_is() {
        let mut r = report(BTreeMap::new(), None);
        assert!(
            !r.to_markdown().contains("vogon"),
            "a run with no driven row must not invent a driver"
        );

        r.driver = Some(LegStatus::Present {
            binary: PathBuf::from("/x/target/release/vogon"),
            version: "unknown".into(),
            source: "repo target/release".into(),
            binary_mtime_unix: Some(1_787_118_443),
            git_describe: Some(format!("ls-597-g{}", "c".repeat(40))),
            dirty: Some(false),
        });
        let md = r.to_markdown();
        assert!(md.contains("`vogon` (driver)"), "{md}");
        assert!(md.contains("/x/target/release/vogon"), "{md}");
        // The mtime is the provenance that matters here: `vogon` answers no
        // `--version`, so freshness is the only thing distinguishing two builds.
        assert!(md.contains("1787118443"), "{md}");

        r.driver = Some(LegStatus::Absent {
            reason: "no vogon binary: no $RIFT_BENCH_VOGON_BIN".into(),
        });
        let md = r.to_markdown();
        assert!(md.contains("**absent**"), "{md}");
        assert!(
            md.contains("RIFT_BENCH_VOGON_BIN"),
            "an absent driver says why: {md}"
        );
    }

    /// The distinction the probe exists for: a measure that ran and moved
    /// nothing prints `0`, a measure nobody counted prints `—`. Collapsing them
    /// is how a fast wall time over an empty repository reads as a good number.
    #[test]
    fn zero_work_and_uncounted_work_render_differently() {
        let mut legs = BTreeMap::new();
        legs.insert(
            "rift".to_owned(),
            LegOutcome::Measured {
                command: "rift verify".into(),
                stats: stats(30_000_000, 0).with_units(0),
                peak_rss_bytes: None,
                ingest: None,
                child_spawns: None,
            },
        );
        let mut r = report(legs, None);
        r.scenarios[0].work_unit = Some("checkpoints".to_owned());
        let md = r.to_markdown();
        assert!(md.contains("0 checkpoints"), "the zero is published: {md}");
        assert!(
            !md.contains("0.00 checkpoints/s"),
            "an empty corpus has no rate; 0/s blames the code: {md}"
        );
        assert!(md.contains("**unmeasurable** — 0 checkpoints"), "{md}");
        assert!(
            md.contains("## Unmeasurable as a rate") && md.contains("`cold-status` ran"),
            "the reason is stated, not left to the reader: {md}"
        );

        let uncounted = LegOutcome::Measured {
            command: "rift verify".into(),
            stats: stats(30_000_000, 0),
            peak_rss_bytes: None,
            ingest: None,
            child_spawns: None,
        };
        r.scenarios[0].legs.insert("rift".to_owned(), uncounted);
        let md = r.to_markdown();
        assert!(!md.contains("0 checkpoints"), "absent is not zero: {md}");
        assert!(
            !md.contains("unmeasurable"),
            "uncounted is not the same claim as empty: {md}"
        );
    }

    #[test]
    fn a_thin_protocol_says_its_p95_is_only_the_maximum() {
        let mut legs = BTreeMap::new();
        legs.insert("rift".to_owned(), measured(2_000_000));
        let mut r = report(legs, None);
        assert!(
            !r.to_markdown().contains("carries no tail information"),
            "30 iterations is a real percentile"
        );
        r.scenarios[0].protocol = Protocol::heavy();
        let md = r.to_markdown();
        assert!(md.contains("nearest-rank"), "{md}");
        assert!(md.contains("carries no tail information"), "{md}");
        assert!(md.contains("`cold-status` (n=3)"), "{md}");
    }

    #[test]
    fn markdown_publishes_absence_and_skip_reasons() {
        let mut legs = BTreeMap::new();
        legs.insert("rift".to_owned(), measured(2_500_000));
        legs.insert(
            "reference".to_owned(),
            LegOutcome::Skipped {
                reason: "backend not provisioned".into(),
            },
        );
        let md = report(legs, None).to_markdown();
        assert!(md.contains("**absent**"));
        assert!(md.contains("the harness never downloads it"));
        assert!(md.contains("backend not provisioned"));
        assert!(md.contains("2.50 ms"), "{md}");
        // No fabricated head-to-head when one leg never ran.
        assert!(md.contains("| — | — | — |"), "{md}");
    }

    #[test]
    fn a_leg_that_was_never_asked_is_not_reported_as_measured() {
        // Found by running the harness on this repository: `--legs rift` left
        // the backend out of the results,
        // and the summary read "every declared measure ran on every leg".
        let mut legs = BTreeMap::new();
        legs.insert("rift".to_owned(), measured(2_000_000));
        let md = report(legs, None).to_markdown();
        assert!(md.contains("not requested by this run"), "{md}");
        assert!(!md.contains("every declared measure ran on every leg"));
    }

    #[test]
    fn failures_are_rendered_as_failures() {
        let mut legs = BTreeMap::new();
        legs.insert(
            "rift".to_owned(),
            LegOutcome::Failed {
                reason: "exit 2: not a rift repository".into(),
                stats: Some(stats(1_000, 3)),
            },
        );
        let md = report(legs, None).to_markdown();
        assert!(md.contains("FAILED — exit 2"));
    }

    /// The twin of `hooklat`'s indentation guard, and it exists because that
    /// one did not cover this renderer: an indented budgets-table header was
    /// planted here and the other test passed. CommonMark renders 4+ leading
    /// spaces as a code block, so an indented table is not a table.
    #[test]
    fn no_generated_line_is_indented_into_a_code_block() {
        let mut legs = BTreeMap::new();
        legs.insert("rift".to_owned(), measured(40_000_000));
        let mut r = report(legs, None);
        r.scenarios[0].budget = Some(Budget::hook());
        r.scenarios[0].work_unit = Some("checkpoints".to_owned());
        for (n, line) in r.to_markdown().lines().enumerate() {
            assert!(
                !line.starts_with("    ") || line.trim().is_empty(),
                "line {n} is indented and will render as a code block: {line:?}"
            );
        }
    }

    #[test]
    fn formatting_helpers_are_stable() {
        assert_eq!(ms(1_500_000), "1.50 ms");
        assert_eq!(ms(0), "0.00 ms");
        assert!(now_unix() > 1_700_000_000);
    }
}
