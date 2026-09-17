//! The ```bench-facts fence: the only gate-bearing numbers in a benchmark
//! report (the guard's fence arm).
//!
//! # Why prose stopped being allowed to carry a number
//!
//! `benchmarks/hook-latency.md` is the document a hard budget is read from, and
//! it is twelve sections of hand-written tables. Every one of those numbers was
//! typed. Some were typed wrong and stayed wrong for days, and the file has a
//! section about correcting itself — twice about numbers, once about a sentence
//! claiming reproducibility from scripts that were never committed. Nothing
//! reddened, because prose reddens nothing.
//!
//! So one fence per report, **generated from the `.json` artifacts**, and the
//! guard recomputes it and reds on disagreement. Prose beside the fence is
//! commentary: it may say what a number means, what produced it and what it
//! retires, and it stays exactly as valuable as it was — but it is no longer
//! where the gate looks. A report carrying two fences is an error rather than a
//! merge of both, because "which one is the gate reading?" has to have one
//! answer.
//!
//! # Integers on purpose
//!
//! Every gate-bearing value here is a `u64` of nanoseconds or a count. A float
//! that survives a decimal round trip through markdown and back is a float
//! nobody should be comparing for equality, and the alternative — a tolerance —
//! is a place for a real disagreement to hide. Milliseconds appear only in the
//! human column the fence prints beside itself.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{hooklat::HookLatencyReport, measure::nearest_rank};

/// The fence's language tag.
pub const FENCE_TAG: &str = "bench-facts";

/// Schema of the fence body.
pub const FACTS_SCHEMA: u32 = 1;

/// One arm's gate-bearing summary, reduced across every artifact that measured
/// it.
///
/// **Median of per-run medians**, which is the reduction the publication rules
/// name and the one this document's own tables already used: a single run cannot tell a
/// slow arm from a busy machine, and a mean lets one outlier carry the row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArmFacts {
    /// The arm id.
    pub arm: String,
    /// How many artifacts measured it.
    pub runs: usize,
    /// Median across runs of (arm p50 - control p50), nanoseconds.
    pub git_latency_ns_median: u64,
    /// How many of those runs were over the budget.
    pub over_budget_runs: usize,
}

impl ArmFacts {
    /// The median as the human column prints it.
    #[must_use]
    pub fn latency_ms(&self) -> String {
        format!("{:.2}", self.git_latency_ns_median as f64 / 1_000_000.0)
    }
}

/// Everything a report may be gated on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BenchFacts {
    /// Fence schema.
    pub schema_version: u32,
    /// The budget these arms are judged against, milliseconds (owner-set).
    pub budget_ms: u64,
    /// The artifact family these stamps belong to: `<family>-<stamp>.json`
    /// under `benchmarks/`.
    ///
    /// Named once instead of repeated into every entry — which is not only
    /// tidier. The workspace's redaction false-positive corpus walks every
    /// tracked file, and a family-plus-stamp stem runs to 23 characters, just
    /// over the entropy detector's 20-byte / 4.0-bit floor: listing the family
    /// thirty-six times pushed a **security crate's** recorded ceiling over, on
    /// this repository's own committed text. The stamps below are ten digits and
    /// cannot.
    pub family: String,
    /// The artifact stamps these facts were generated from, sorted. A reader
    /// can re-derive every number below from exactly `<family>-<stamp>.json`.
    pub artifacts: Vec<u64>,
    /// The artifacts behind this fence that are **records** rather than current
    /// measurements, by stem.
    ///
    /// This is the field that gives the staleness arm something to say about
    /// the committed runs: they are all real, they all predate the shipped
    /// caps, and none of them may derive a floor. Marking them is not a way of
    /// dismissing them — the pre-cap evidence is what produced those caps — it
    /// is a way of stopping them being quoted as current.
    ///
    /// **Derived, never declared.** It is computed from each artifact's own
    /// subject provenance (absent, or dirty), so no caller can mark a current
    /// run superseded to duck a check, and none can mark a record current to
    /// borrow its authority.
    pub superseded_pending_rerun: Vec<u64>,
    /// Why, in the words the report shows.
    pub superseded_reason: String,
    /// Median across runs of the bare-commit control's own p50, nanoseconds.
    /// Published because every latency here is relative to it, and it moved 40
    /// -> 62 ms across four runs of one sitting.
    pub control_p50_ns_median: u64,
    /// One row per arm, in arm order.
    pub arms: Vec<ArmFacts>,
}

impl BenchFacts {
    /// Reduce a set of loaded artifacts into the gate-bearing set.
    ///
    /// `reports` is `(stem, report)`; an artifact with no control contributes
    /// nothing, because a git latency without its own run's control is the
    /// error the same-run control rule exists to prevent.
    #[must_use]
    pub fn of(reports: &[(String, HookLatencyReport)], superseded_reason: String) -> Self {
        let family = reports
            .first()
            .and_then(|(stem, _)| stem.rsplit_once('-').map(|(head, _)| head.to_owned()))
            .unwrap_or_else(|| "hook-latency".to_owned());
        let budget_ms = reports.first().map_or(100, |(_, r)| r.budget_ms);
        let ceiling_ns = budget_ms.saturating_mul(1_000_000);
        let mut per_arm: BTreeMap<String, Vec<u64>> = BTreeMap::new();
        let mut controls: Vec<u64> = Vec::new();
        let mut used: Vec<u64> = Vec::new();
        let mut superseded: Vec<u64> = Vec::new();

        for (stem, report) in reports {
            let Some(control) = report.control().copied() else {
                continue;
            };
            // A stem that does not end in this family's stamp is skipped
            // rather than folded in under a name that would not resolve.
            let Some(stamp) = stamp_of(&family, stem) else {
                continue;
            };
            used.push(stamp);
            if is_record(&report.subject) {
                superseded.push(stamp);
            }
            controls.push(control.p50_ns);
            for arm in &report.arms {
                if arm.arm.is_control() {
                    continue;
                }
                if let Some(ns) = arm.latency_ns(&control) {
                    per_arm.entry(arm.arm.id.clone()).or_default().push(ns);
                }
            }
        }

        used.sort_unstable();
        used.dedup();
        superseded.sort_unstable();
        superseded.dedup();
        Self {
            schema_version: FACTS_SCHEMA,
            budget_ms,
            family,
            artifacts: used,
            superseded_pending_rerun: superseded,
            superseded_reason,
            control_p50_ns_median: median(&controls),
            arms: per_arm
                .into_iter()
                .map(|(arm, mut samples)| {
                    let over = samples.iter().filter(|ns| **ns > ceiling_ns).count();
                    samples.sort_unstable();
                    ArmFacts {
                        arm,
                        runs: samples.len(),
                        git_latency_ns_median: median(&samples),
                        over_budget_runs: over,
                    }
                })
                .collect(),
        }
    }

    /// The fenced block, ready to splice into a report.
    #[must_use]
    pub fn render_fence(&self) -> String {
        render_fence_tagged(FENCE_TAG, self)
    }

    /// The human table the fence is printed beside — commentary, never the
    /// gate. Rendered from the same struct so it cannot drift from it.
    #[must_use]
    pub fn render_table(&self) -> String {
        let mut out = String::from(
            "| arm | runs | git latency (median of run medians) | over the budget |\n\
             |---|---|---|---|\n",
        );
        for arm in &self.arms {
            out.push_str(&format!(
                "| `{}` | {} | {} ms | {} / {} |\n",
                arm.arm,
                arm.runs,
                arm.latency_ms(),
                arm.over_budget_runs,
                arm.runs
            ));
        }
        out
    }
}

/// The stamp inside `<family>-<stamp>`, when the stem belongs to `family`.
#[must_use]
pub fn stamp_of(family: &str, stem: &str) -> Option<u64> {
    stem.strip_prefix(family)?.strip_prefix('-')?.parse().ok()
}

/// The stem a stamp names in this family.
#[must_use]
pub fn stem_of(family: &str, stamp: u64) -> String {
    format!("{family}-{stamp}")
}

/// Whether an artifact's subject makes it a **record** rather than a current
/// measurement: no provenance at all, or a dirty tree.
///
/// Ancestry is deliberately not decided here — that needs a repository and the
/// scenario's declared paths, which is the guard's job. This is the half that
/// is visible in the artifact alone, and it is the half that classifies all
/// committed runs.
#[must_use]
pub fn is_record(subject: &crate::leg::LegStatus) -> bool {
    match subject {
        crate::leg::LegStatus::Present {
            git_describe,
            dirty,
            ..
        } => git_describe.is_none() || *dirty == Some(true),
        // A sweep with no subject binary measured nothing to be current about.
        crate::leg::LegStatus::Absent { .. } => true,
    }
}

/// Nearest-rank median over an unsorted slice; `0` for nothing.
fn median(values: &[u64]) -> u64 {
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    nearest_rank(&sorted, 50)
}

/// A fenced block under `tag`, ready to splice into a report: the value as
/// pretty JSON between ```` ```<tag> ```` and ```` ``` ````.
#[must_use]
pub fn render_fence_tagged<T: Serialize>(tag: &str, value: &T) -> String {
    let body =
        serde_json::to_string_pretty(value).unwrap_or_else(|_| "{\"schema_version\":0}".to_owned());
    format!("```{tag}\n{body}\n```\n")
}

/// Pull the single ```` ```bench-facts ```` fence body out of a markdown
/// document.
///
/// # Errors
/// More than one fence (the gate would have to choose), or an unterminated one.
pub fn extract(markdown: &str) -> std::result::Result<Option<String>, String> {
    extract_tagged(markdown, FENCE_TAG)
}

/// Pull the single fence body under `tag` out of a markdown document. Fences
/// under other tags are prose to this reader: one report may carry one
/// gate-bearing fence per family, each with one home.
///
/// # Errors
/// More than one fence under `tag` (the gate would have to choose), or an
/// unterminated one.
pub fn extract_tagged(markdown: &str, tag: &str) -> std::result::Result<Option<String>, String> {
    let open = format!("```{tag}");
    let mut bodies: Vec<String> = Vec::new();
    let mut current: Option<Vec<&str>> = None;
    for line in markdown.lines() {
        match &mut current {
            None if line.trim_end() == open => current = Some(Vec::new()),
            None => {}
            Some(_) if line.trim_end() == "```" => {
                let body = current.take().unwrap_or_default();
                bodies.push(body.join("\n"));
            }
            Some(lines) => lines.push(line),
        }
    }
    if current.is_some() {
        return Err(format!("unterminated ```{tag} fence"));
    }
    match bodies.len() {
        0 => Ok(None),
        1 => Ok(bodies.pop()),
        n => Err(format!(
            "{n} ```{tag} fences in one report: the gate-bearing numbers must have one home"
        )),
    }
}

/// Parse the ```` ```bench-facts ```` fence body.
///
/// # Errors
/// No fence, or a body that is not this schema.
pub fn parse(markdown: &str) -> std::result::Result<BenchFacts, String> {
    parse_tagged(markdown, FENCE_TAG, FACTS_SCHEMA)
}

/// Parse the fence body under `tag` as `T`, refusing any `schema_version`
/// but `schema`.
///
/// # Errors
/// No fence under `tag`, or a body that is not this schema.
pub fn parse_tagged<T: serde::de::DeserializeOwned>(
    markdown: &str,
    tag: &str,
    schema: u32,
) -> std::result::Result<T, String> {
    let body = extract_tagged(markdown, tag)?.ok_or_else(|| format!("no ```{tag} fence"))?;
    // The version is read **before** the shape, so a fence from a later build
    // says which schema it is rather than complaining about a field this build
    // happens not to know. A version mismatch and a corrupt body send a reader
    // to different places.
    let value: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| format!("the fence body is not valid: {e}"))?;
    let version = value
        .get("schema_version")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| "the fence body states no schema_version".to_owned())?;
    if version != u64::from(schema) {
        return Err(format!(
            "fence schema {version}, this build understands {schema}"
        ));
    }
    serde_json::from_value(value).map_err(|e| format!("the fence body is not valid: {e}"))
}

/// Replace the ```` ```bench-facts ```` fence in `markdown`, or append one
/// under `heading` if absent.
#[must_use]
pub fn splice(markdown: &str, facts: &BenchFacts, heading: &str) -> String {
    splice_tagged(markdown, FENCE_TAG, &facts.render_fence(), heading)
}

/// Replace the fence under `tag` in `markdown` with `fence` (a rendered
/// block, see [`render_fence_tagged`]), or append it under `heading` if
/// absent. Fences under other tags are left exactly where they are.
#[must_use]
pub fn splice_tagged(markdown: &str, tag: &str, fence: &str, heading: &str) -> String {
    let open = format!("```{tag}");
    let mut out = String::new();
    let mut skipping = false;
    let mut replaced = false;
    for line in markdown.lines() {
        if skipping {
            if line.trim_end() == "```" {
                skipping = false;
                out.push_str(fence);
                replaced = true;
            }
            continue;
        }
        if line.trim_end() == open {
            skipping = true;
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    if !replaced {
        out.push_str(heading);
        out.push_str(fence);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        hooklat::{Arm, ArmResult, Capture, Oracles},
        leg::LegStatus,
        measure::{Protocol, Sample, Stats},
        report::Host,
    };
    use std::collections::BTreeMap;

    fn stats(p50_ns: u64) -> Stats {
        Stats::of(&[Sample {
            wall_ns: p50_ns,
            exit_code: Some(0),
        }])
        .unwrap()
    }

    fn arm(id: &str, p50_ns: u64, control: bool) -> ArmResult {
        ArmResult {
            arm: Arm {
                id: id.to_owned(),
                title: id.to_owned(),
                capture: if control { Capture::Off } else { Capture::Live },
                include: None,
            },
            stats: Some(stats(p50_ns)),
            error: None,
            attestation: Some(crate::attest::Attestation {
                load1: Some(1.65),
                void_above: crate::attest::VOID_ABOVE_LOAD1,
            }),
            oracles: Oracles::default(),
            phases: BTreeMap::new(),
        }
    }

    fn report(control_ns: u64, live_ns: u64) -> HookLatencyReport {
        HookLatencyReport {
            schema_version: crate::hooklat::HOOK_LATENCY_SCHEMA,
            harness_version: "0.0.1".to_owned(),
            generated_at_unix: 1_787_120_100,
            host: Host::detect("n/a".to_owned()),
            subject: LegStatus::Absent {
                reason: "fixture".to_owned(),
            },
            source_clock: None,
            git_version: "git version 2.50.1".to_owned(),
            protocol: Protocol::new(3, 11),
            budget_ms: 100,
            arms: vec![
                arm("A_bare", control_ns, true),
                arm("B1_live", live_ns, false),
            ],
        }
    }

    #[test]
    fn the_facts_are_the_median_of_per_run_medians_and_count_the_breaches() {
        // Three runs: latencies 50, 150 and 120 ms over their own controls.
        let reports = vec![
            ("run-1".to_owned(), report(40_000_000, 90_000_000)),
            ("run-2".to_owned(), report(40_000_000, 190_000_000)),
            ("run-3".to_owned(), report(40_000_000, 160_000_000)),
        ];
        let facts = BenchFacts::of(&reports, "predates the caps".to_owned());
        assert_eq!(facts.family, "run");
        assert_eq!(facts.artifacts, [1, 2, 3]);
        assert_eq!(facts.arms.len(), 1, "the control is not an arm of itself");
        let live = &facts.arms[0];
        assert_eq!(live.arm, "B1_live");
        assert_eq!(live.runs, 3);
        // Sorted 50/120/150 -> nearest-rank p50 is 120 ms.
        assert_eq!(live.git_latency_ns_median, 120_000_000);
        assert_eq!(live.over_budget_runs, 2, "150 and 120 are over 100");
        assert_eq!(live.latency_ms(), "120.00");
        assert_eq!(facts.control_p50_ns_median, 40_000_000);
        // Derived, not declared: this fixture's subject is Absent, so every run
        // behind the fence is a record and says so by name.
        assert_eq!(
            facts.superseded_pending_rerun,
            [1, 2, 3],
            "a run with no provenance is a record"
        );
    }

    /// A run with no control contributes nothing at all: an arm quoted against
    /// another run's control is the error the same-run control rule exists to prevent, and
    /// silently borrowing one would be worse than dropping the run.
    #[test]
    fn a_run_that_lost_its_control_contributes_nothing() {
        let mut orphan = report(40_000_000, 90_000_000);
        orphan.arms.retain(|a| !a.arm.is_control());
        let facts = BenchFacts::of(
            &[
                ("run-7".to_owned(), report(40_000_000, 90_000_000)),
                ("run-8".to_owned(), orphan),
            ],
            String::new(),
        );
        assert_eq!(facts.artifacts, [7]);
        assert_eq!(facts.arms[0].runs, 1);
    }

    #[test]
    fn a_fence_round_trips_through_a_document() {
        let facts = BenchFacts::of(
            &[("run-1".to_owned(), report(40_000_000, 90_000_000))],
            "records only".to_owned(),
        );
        let doc = format!("# Title\n\nprose\n\n{}\nmore prose\n", facts.render_fence());
        assert_eq!(parse(&doc).unwrap(), facts);

        // Splicing replaces in place rather than appending a second one.
        let mut newer = facts.clone();
        newer.budget_ms = 42;
        let doc2 = splice(&doc, &newer, "\n## Facts\n\n");
        assert_eq!(parse(&doc2).unwrap(), newer);
        assert!(doc2.contains("more prose"), "prose survives: {doc2}");
        assert_eq!(doc2.matches("```bench-facts").count(), 1);
    }

    #[test]
    fn a_document_with_no_fence_grows_one_under_its_heading() {
        let facts = BenchFacts::of(
            &[("run-1".to_owned(), report(40_000_000, 90_000_000))],
            String::new(),
        );
        let doc = splice("# Title\n\nprose\n", &facts, "\n## Facts\n\n");
        assert!(doc.contains("## Facts"), "{doc}");
        assert_eq!(parse(&doc).unwrap(), facts);
    }

    /// Two fences is an error, not a merge: "which one is the gate reading?"
    /// has to have one answer.
    #[test]
    fn two_fences_in_one_report_are_an_error() {
        let facts = BenchFacts::of(
            &[("run-1".to_owned(), report(40_000_000, 90_000_000))],
            String::new(),
        );
        let two = format!("{}\n{}", facts.render_fence(), facts.render_fence());
        let err = extract(&two).unwrap_err();
        assert!(err.contains("one home"), "{err}");

        assert!(extract("no fence here").unwrap().is_none());
        assert!(parse("no fence here").unwrap_err().contains("no ```"));
        let unterminated = format!("```{FENCE_TAG}\n{{}}\n");
        assert!(extract(&unterminated).unwrap_err().contains("unterminated"));
    }

    /// The stem is `<family>-<stamp>` and both directions round-trip; a stem
    /// from another family is skipped rather than folded in under a name that
    /// would not resolve.
    #[test]
    fn a_stamp_and_its_stem_round_trip_and_a_foreign_family_does_not() {
        assert_eq!(
            stamp_of("hook-latency", "hook-latency-1787120100"),
            Some(1_787_120_100)
        );
        assert_eq!(
            stem_of("hook-latency", 1_787_120_100),
            "hook-latency-1787120100"
        );
        assert_eq!(stamp_of("hook-latency", "other-family-1787120100"), None);
        assert_eq!(stamp_of("hook-latency", "hook-latency-nope"), None);
        assert_eq!(stamp_of("hook-latency", "hook-latency"), None);
    }

    /// Two families, two tags, one document: each tag's reader sees only
    /// its own fence, splicing one leaves the other byte-for-byte alone, and
    /// the untagged names are exactly the `bench-facts` case.
    #[test]
    fn fences_under_different_tags_share_a_document_without_touching_each_other() {
        #[derive(Debug, PartialEq, Serialize, Deserialize)]
        struct Other {
            schema_version: u32,
            count: u64,
        }
        let facts = BenchFacts::of(
            &[("run-1".to_owned(), report(40_000_000, 90_000_000))],
            String::new(),
        );
        let other = Other {
            schema_version: 7,
            count: 3,
        };
        let doc = format!(
            "# Title\n\n{}\nprose\n\n{}",
            facts.render_fence(),
            render_fence_tagged("other-report", &other)
        );
        assert_eq!(parse(&doc).unwrap(), facts);
        assert_eq!(
            parse_tagged::<Other>(&doc, "other-report", 7).unwrap(),
            other
        );
        assert!(parse_tagged::<Other>(&doc, "other-report", 8)
            .unwrap_err()
            .contains("this build understands 8"));
        assert!(extract_tagged(&doc, "absent").unwrap().is_none());

        let newer = Other {
            schema_version: 7,
            count: 4,
        };
        let spliced = splice_tagged(
            &doc,
            "other-report",
            &render_fence_tagged("other-report", &newer),
            "\n## Other\n\n",
        );
        assert_eq!(
            parse(&spliced).unwrap(),
            facts,
            "the other family's fence is untouched"
        );
        assert_eq!(
            parse_tagged::<Other>(&spliced, "other-report", 7).unwrap(),
            newer
        );
        assert!(
            !spliced.contains("## Other"),
            "replaced in place, not appended"
        );
        assert_eq!(spliced.matches("```other-report").count(), 1);
    }

    #[test]
    fn a_fence_from_another_schema_is_refused_rather_than_read() {
        let doc = format!("```{FENCE_TAG}\n{{\"schema_version\": 99}}\n```\n");
        let err = parse(&doc).unwrap_err();
        assert!(err.contains("this build understands"), "{err}");
        let broken = format!("```{FENCE_TAG}\nnot json\n```\n");
        assert!(parse(&broken).unwrap_err().contains("not valid"));
    }

    #[test]
    fn the_human_table_is_rendered_from_the_same_struct_it_cannot_drift_from() {
        let facts = BenchFacts::of(
            &[("run-1".to_owned(), report(40_000_000, 190_000_000))],
            String::new(),
        );
        let table = facts.render_table();
        assert!(
            table.contains("| `B1_live` | 1 | 150.00 ms | 1 / 1 |"),
            "{table}"
        );
    }
}
