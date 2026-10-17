//! Closed fallback records for redaction that cannot reach a fixpoint.
//!
//! Failed structured records keep their physical line slots as fixed records.
//! Failed flat runs retain only independently safe sub-runs. A bounded replay of
//! the same effective detector selection preserves context dependencies across
//! every pass: lines whose protection depends on neighbours are replaced too.
//! Probe counts are discarded; only masks and records actually sealed count.

use std::ops::Range;

use serde_json::Value;

use crate::{
    effective_spans, redact_fragment, redact_fragment_observed, RedactionReport, RulePackSet, Span,
};

/// The record's discriminating member and its one value.
const KIND_KEY: &str = "rift_redaction";
const KIND: &str = "placeholder";
/// The closed reason vocabulary. One word today: a fixpoint that did not
/// converge is the only way the gate gives a line up.
const REASON_DID_NOT_CONVERGE: &str = "did_not_converge";

/// The record that takes the place of a line no path could seal: its 1-based
/// line number and the reason, and never a byte of the line. JSON, so a
/// transcript reader keeps parsing past it; members sorted, so it has one
/// spelling.
pub(crate) fn placeholder_record(line_number: impl std::fmt::Display) -> String {
    format!(
        r#"{{"line":{line_number},"reason":"{REASON_DID_NOT_CONVERGE}","{KIND_KEY}":"{KIND}"}}"#
    )
}

/// Whether `line` is exactly a record the fallback writes in place of a line: those
/// three members, a line number, a reason from the closed vocabulary, and
/// nothing else, spelled the way the writer spells it.
#[must_use]
pub fn is_fallback_placeholder(line: &str) -> bool {
    let Ok(Value::Object(map)) = serde_json::from_str::<Value>(line) else {
        return false;
    };
    let Some(number) = map.get("line").and_then(Value::as_u64) else {
        return false;
    };
    map.len() == 3 && line == placeholder_record(number)
}

/// How many lines of `bytes` are placeholder records — the recount a verifier
/// holds a recorded count to.
#[must_use]
pub fn count_fallback_placeholders(bytes: &[u8]) -> u32 {
    count(
        String::from_utf8_lossy(bytes)
            .split('\n')
            .filter(|line| is_fallback_placeholder(line))
            .count(),
    )
}

/// Seal one flat-text run. `first` is the 0-based index of its first line in
/// the document. Failed speculative masks never enter the final report.
pub(crate) fn seal_run(
    run: &[&str],
    first: usize,
    forced: Vec<bool>,
    packs: &RulePackSet,
    report: &mut RedactionReport,
) -> String {
    let snapshot = report.clone();
    if !forced.iter().any(|flag| *flag) {
        if let Ok(text) = redact_fragment(&run.join("\n"), packs, report) {
            return text;
        }
    }
    // The refused attempt's counts are not the sealed text's, so they go.
    *report = snapshot;
    split_run(run, first, forced, packs, report)
}

/// The run did not settle as a whole: seal it by the lines that do.
fn split_run(
    run: &[&str],
    first: usize,
    forced: Vec<bool>,
    packs: &RulePackSet,
    report: &mut RedactionReport,
) -> String {
    let mut protection = ContextProtection::new(forced);
    let mut scratch = crate::jsonl::blank_report();
    let _ = redact_fragment_observed(&run.join("\n"), packs, &mut scratch, |text, spans| {
        protection.observe(text, spans, packs);
    });
    let settles: Vec<bool> = run
        .iter()
        .enumerate()
        .map(|(i, l)| protection.forced.get(i) == Some(&false) && settles_alone(l, packs))
        .collect();
    let mut out: Vec<String> = Vec::with_capacity(run.len());
    let mut at = 0usize;
    while let Some(&settled) = settles.get(at) {
        let start = at;
        while settles.get(at) == Some(&settled) {
            at += 1;
        }
        let lines = run.get(start..at).unwrap_or_default();
        if settled {
            out.push(seal_sub_run(lines, first + start, packs, report));
        } else {
            out.push(replace_lines(lines, first + start, report));
        }
    }
    add(&mut report.redaction_flat_fallback, count(run.len()));
    out.join("\n")
}

/// A sub-run of lines that each settle alone, redacted together; replaced
/// whole in the case where the lines settle apart and not together.
fn seal_sub_run(
    lines: &[&str],
    first: usize,
    packs: &RulePackSet,
    report: &mut RedactionReport,
) -> String {
    let snapshot = report.clone();
    if let Ok(text) = redact_fragment(&lines.join("\n"), packs, report) {
        return text;
    }
    *report = snapshot;
    replace_lines(lines, first, report)
}

/// `n` placeholder records for the lines starting at 0-based index `first`.
fn placeholders(first: usize, n: usize, report: &mut RedactionReport) -> String {
    add(&mut report.redaction_fallback_placeholders, count(n));
    (first..first + n)
        .map(|index| placeholder_record(index + 1))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Replace one failed parsed value, retaining blank slots and trailing newline.
pub(crate) fn replace_record(lines: &[&str], first: usize, report: &mut RedactionReport) -> String {
    let nonempty = lines.iter().filter(|line| !line.trim().is_empty()).count();
    add(&mut report.redaction_flat_fallback, count(nonempty));
    replace_lines(lines, first, report)
}

/// Blank slots cannot carry protected payload and retain their exact spelling.
fn replace_lines(lines: &[&str], first: usize, report: &mut RedactionReport) -> String {
    lines
        .iter()
        .enumerate()
        .map(|(i, line)| {
            if line.trim().is_empty() {
                (*line).to_string()
            } else {
                placeholders(first + i, 1, report)
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Current physical lines map to contiguous ranges of original physical lines.
/// Masking never introduces a newline, so origins only merge. Storage is linear
/// in line count; no per-byte provenance or pass snapshots are retained.
struct ContextProtection {
    origins: Vec<Range<usize>>,
    forced: Vec<bool>,
}

impl ContextProtection {
    fn new(forced: Vec<bool>) -> Self {
        Self {
            origins: (0..forced.len()).map(|i| i..i + 1).collect(),
            forced,
        }
    }

    fn force(&mut self, range: Range<usize>) {
        if let Some(flags) = self.forced.get_mut(range) {
            flags.fill(true);
        } else {
            self.forced.fill(true);
        }
    }

    fn observe(&mut self, text: &str, spans: &[Span], packs: &RulePackSet) {
        let lines: Vec<&str> = text.split('\n').collect();
        if lines.len() != self.origins.len() {
            self.forced.fill(true);
            return;
        }
        let mut offset = 0;
        let ranges: Vec<Range<usize>> = lines
            .iter()
            .map(|line| {
                let range = offset..offset + line.len();
                offset = range.end + 1;
                range
            })
            .collect();
        let isolated: Vec<Vec<Span>> = lines
            .iter()
            .map(|line| effective_spans(line, packs).0)
            .collect();
        let mut forced_through = 0;
        for span in spans {
            self.protect_span(span, &ranges, &isolated, &mut forced_through);
        }
        self.merge_masked_newlines(&ranges, spans);
    }

    fn protect_span(
        &mut self,
        span: &Span,
        ranges: &[Range<usize>],
        isolated: &[Vec<Span>],
        forced_through: &mut usize,
    ) {
        let first = ranges.partition_point(|r| r.end < span.start);
        let last = ranges
            .partition_point(|r| r.start < span.end)
            .saturating_sub(1);
        let local = ranges
            .get(first)
            .is_some_and(|r| span.start >= r.start && span.end <= r.end);
        let covered = local
            && isolated
                .get(first)
                .zip(ranges.get(first))
                .is_some_and(|(selected, r)| {
                    let mut through = span.start - r.start;
                    let first_candidate = selected.partition_point(|item| item.end <= through);
                    for item in selected.iter().skip(first_candidate) {
                        if item.start > through {
                            break;
                        }
                        through = through.max(item.end);
                        if through >= span.end - r.start {
                            break;
                        }
                    }
                    through >= span.end - r.start
                });
        if !covered {
            if let Some(origin) = self
                .origins
                .get(first)
                .zip(self.origins.get(last))
                .map(|(a, b)| a.start..b.end)
            {
                let clipped = origin.start.max(*forced_through)..origin.end;
                *forced_through = origin.end;
                self.force(clipped);
            } else {
                self.forced.fill(true);
            }
        }
    }

    fn merge_masked_newlines(&mut self, ranges: &[Range<usize>], spans: &[Span]) {
        let mut merged: Vec<Range<usize>> = Vec::with_capacity(self.origins.len());
        let mut join = false;
        let mut span_at = 0;
        for (i, origin) in self.origins.iter().cloned().enumerate() {
            if join {
                if let Some(previous) = merged.last_mut() {
                    previous.end = origin.end;
                }
            } else {
                merged.push(origin);
            }
            join = i + 1 < ranges.len()
                && ranges.get(i).is_some_and(|r| {
                    while spans.get(span_at).is_some_and(|s| s.end <= r.end) {
                        span_at += 1;
                    }
                    spans
                        .get(span_at)
                        .is_some_and(|s| s.start <= r.end && r.end < s.end)
                });
        }
        // Disjoint accumulated ranges avoid repeatedly filling a growing range.
        for origin in &merged {
            if origin.end.saturating_sub(origin.start) > 1 {
                self.force(origin.clone());
            }
        }
        self.origins = merged;
    }
}

/// Whether `line` reaches a fixpoint on its own. A probe: its counts are
/// thrown away.
fn settles_alone(line: &str, packs: &RulePackSet) -> bool {
    let mut scratch = crate::jsonl::blank_report();
    redact_fragment(line, packs, &mut scratch).is_ok()
}

fn add(counter: &mut u32, n: u32) {
    *counter = counter.saturating_add(n);
}

/// A line count as the report's width; a document past `u32::MAX` lines
/// saturates rather than wraps.
pub(crate) fn count(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}
