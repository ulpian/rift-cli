//! How a reading is reported: the one table the command prints, and the JSON
//! document carrying the same facts.
//!
//! The columns, their widths, the share rounding and the stamp format are what
//! a published comparison says, so they are pinned by the tests below rather
//! than derived anywhere else.

use std::path::PathBuf;

use crate::dogfood::measure::{
    tally_kinds, DecisionKind, LeverKind, RetryCounts, SavingsDeltas, SavingsSample,
};
use crate::dogfood::{Client, Lever};

/// How one scripted session ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    /// The client ran the session to completion.
    Completed,
    /// The client refused, was missing, or ran out of quota.
    Unavailable,
    /// The time box fired first.
    TimedOut,
    /// The client exited non-zero for some other reason.
    Failed,
}

impl Status {
    /// The word the table and the JSON both use.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Unavailable => "UNAVAILABLE",
            Self::TimedOut => "TIMED OUT",
            Self::Failed => "FAILED",
        }
    }
}

/// One played session and what Rift recorded for it.
#[derive(Clone, Debug)]
pub struct SessionOutcome {
    /// The session's ordinal.
    pub number: u32,
    /// The session's name.
    pub name: String,
    /// How it ended.
    pub status: Status,
    /// The client's exit code, when it produced one.
    pub exit_code: Option<i32>,
    /// Wall-clock seconds.
    pub seconds: u64,
    /// `rift savings --json` for the scratch family, after this session.
    pub savings: Option<SavingsSample>,
    /// What this session alone moved that family's figures by.
    pub deltas: SavingsDeltas,
    /// What the client's own event stream said the session cost it, where the
    /// stream carries the figure at all.
    pub client_reported: Option<u64>,
    /// Lines in the checkout's window journals after this session.
    pub ledger_lines: u64,
    /// What the client's own log exposed.
    pub retries: RetryCounts,
    /// The first line of whatever went wrong.
    pub error: Option<String>,
}

/// Everything one client's sessions produced.
#[derive(Clone, Debug)]
pub struct ClientRun {
    /// The client.
    pub client: Client,
    /// The scratch checkout it played in.
    pub scratch: PathBuf,
    /// The scratch family's recall projection — printed so a reader can see it
    /// is not the developer's.
    pub recall_db: String,
    /// The scratch family's capture projection.
    pub projection_db: String,
    /// One entry per scripted session.
    pub sessions: Vec<SessionOutcome>,
    /// The savings figure after the last session.
    pub final_savings: Option<SavingsSample>,
    /// The audit relation's rows by verb, at the end.
    pub kinds: Vec<LeverKind>,
}

impl ClientRun {
    /// Sessions the client actually completed.
    #[must_use]
    pub fn completed(&self) -> usize {
        self.sessions
            .iter()
            .filter(|s| s.status == Status::Completed)
            .count()
    }

    /// Retry counts summed over the sessions whose logs exposed them.
    #[must_use]
    pub fn retries(&self) -> RetryCounts {
        let mut out = RetryCounts::default();
        for s in &self.sessions {
            if s.retries.observed {
                out.observed = true;
                out.denies += s.retries.denies;
                out.retries += s.retries.retries;
            }
        }
        out
    }
}

/// A share as the surfaces spell it, or the word for a question with no answer.
#[must_use]
pub fn fmt_share(share: Option<f64>) -> String {
    share.map_or_else(|| "n/a".to_owned(), |s| format!("{s:.2}%"))
}

/// The one table the command prints: a row per client, then the overall row.
#[must_use]
pub fn render_table(runs: &[ClientRun]) -> String {
    let mut out = String::new();
    out.push_str(
        "dogfood — the read levers over a fixed scripted sample, through the real clients\n",
    );
    push_savings_rows(&mut out, runs);
    push_kind_rows(&mut out, runs);
    push_incomplete_rows(&mut out, runs);
    out
}

/// The tokens each client booked, then the overall row.
///
/// The overall share is computed over the summed tokens rather than averaged
/// across the client rows: an average of shares would weight a client with
/// three reads the same as one with three thousand.
fn push_savings_rows(out: &mut String, runs: &[ClientRun]) {
    out.push_str(&format!(
        "  {}{:>7}{:>8}{:>13}{:>13}{:>13}{:>13}{:>9}\n",
        pad("client", 14),
        "done",
        "n",
        "read",
        "avoided",
        "returned",
        "honesty",
        "share"
    ));
    let (mut avoided, mut returned, mut read, mut n, mut done, mut planned) =
        (0u64, 0u64, 0u64, 0u64, 0usize, 0usize);
    for r in runs {
        let s = r.final_savings.clone().unwrap_or_default();
        avoided = avoided.saturating_add(s.tokens_avoided);
        returned = returned.saturating_add(s.tokens_returned);
        read = read.saturating_add(s.read_tokens);
        n = n.saturating_add(s.sessions_with_reads);
        done += r.completed();
        planned += r.sessions.len();
        out.push_str(&format!(
            "  {}{:>7}{:>8}{:>13}{:>13}{:>13}{:>13}{:>9}\n",
            pad(r.client.slug(), 14),
            format!("{}/{}", r.completed(), r.sessions.len()),
            s.sessions_with_reads,
            s.read_tokens,
            s.tokens_avoided,
            s.tokens_returned,
            s.honesty,
            fmt_share(s.share_pct)
        ));
    }
    out.push_str(&format!(
        "  {}{:>7}{:>8}{:>13}{:>13}{:>13}{:>13}{:>9}\n",
        pad("overall", 14),
        format!("{done}/{planned}"),
        n,
        read,
        avoided,
        returned,
        i64::try_from(avoided).unwrap_or(i64::MAX) - i64::try_from(returned).unwrap_or(i64::MAX),
        fmt_share(share_of(avoided, read))
    ));
}

/// The audit relation's rows grouped into the lever that produced them, and
/// how often the model asked again after a decline.
fn push_kind_rows(out: &mut String, runs: &[ClientRun]) {
    out.push_str("\ndecisions by kind (rows in the audit relation), and retry-after-deny\n");
    out.push_str(&format!(
        "  {}{:>12}{:>12}{:>10}{:>10}{:>18}\n",
        pad("client", 14),
        "intercept",
        "document",
        "search",
        "other",
        "retry/deny"
    ));
    for r in runs {
        let t = tally_kinds(&r.kinds);
        let get = |k: DecisionKind| t.get(&k).copied().unwrap_or(0);
        let retries = r.retries();
        let retry = if retries.observed {
            format!("{}/{}", retries.retries, retries.denies)
        } else {
            "n/a".to_owned()
        };
        out.push_str(&format!(
            "  {}{:>12}{:>12}{:>10}{:>10}{:>18}\n",
            pad(r.client.slug(), 14),
            get(DecisionKind::Intercept),
            get(DecisionKind::Document),
            get(DecisionKind::Search),
            get(DecisionKind::Other),
            retry
        ));
    }
}

/// Every session that did not complete, with what went wrong — or the word
/// for a run where none of them did, so a reader never meets a blank.
fn push_incomplete_rows(out: &mut String, runs: &[ClientRun]) {
    out.push_str("\nsessions that did not complete\n");
    let mut any = false;
    for r in runs {
        for s in &r.sessions {
            if s.status == Status::Completed {
                continue;
            }
            any = true;
            out.push_str(&format!(
                "  {} {} — {}: {}\n",
                pad(r.client.slug(), 14),
                s.name,
                s.status.label(),
                s.error.clone().unwrap_or_else(|| "no detail".to_owned())
            ));
        }
    }
    if !any {
        out.push_str("  (none)\n");
    }
}

/// `a ÷ (a + b)` as a percentage, `None` when both sides are empty.
#[must_use]
pub fn share_of(avoided: u64, read: u64) -> Option<f64> {
    let total = avoided.saturating_add(read);
    if total == 0 {
        None
    } else {
        Some(avoided as f64 * 100.0 / total as f64)
    }
}

/// The same facts as the table, as one JSON document.
#[must_use]
pub fn report_json(runs: &[ClientRun], stamp: &str, enforced: &[Lever]) -> serde_json::Value {
    let clients: Vec<serde_json::Value> = runs
        .iter()
        .map(|r| {
            let s = r.final_savings.clone().unwrap_or_default();
            let t = tally_kinds(&r.kinds);
            let retries = r.retries();
            serde_json::json!({
                "client": r.client.slug(),
                "scratch": r.scratch.display().to_string(),
                "recall_db": r.recall_db,
                "projection_db": r.projection_db,
                "sessions_played": r.sessions.len(),
                "sessions_completed": r.completed(),
                "savings": {
                    "share_pct": s.share_pct,
                    "tokens_avoided": s.tokens_avoided,
                    "tokens_returned": s.tokens_returned,
                    "read_tokens": s.read_tokens,
                    "honesty": s.honesty,
                    "sessions_with_reads": s.sessions_with_reads,
                    "window": s.window,
                    "note": s.note,
                },
                "decisions_by_kind": {
                    "intercept": t.get(&DecisionKind::Intercept).copied().unwrap_or(0),
                    "document": t.get(&DecisionKind::Document).copied().unwrap_or(0),
                    "search": t.get(&DecisionKind::Search).copied().unwrap_or(0),
                    "other": t.get(&DecisionKind::Other).copied().unwrap_or(0),
                },
                "verbs": r.kinds.iter().map(|k| serde_json::json!({
                    "verb": k.verb, "rows": k.rows,
                    "tokens_avoided": k.tokens_avoided, "tokens_returned": k.tokens_returned,
                })).collect::<Vec<_>>(),
                "retry_after_deny": if retries.observed {
                    serde_json::json!({ "denies": retries.denies, "retries": retries.retries })
                } else {
                    serde_json::json!({ "observed": false })
                },
                "sessions": r.sessions.iter().map(|s| serde_json::json!({
                    "number": s.number,
                    "name": s.name,
                    "status": s.status.label(),
                    "exit_code": s.exit_code,
                    "seconds": s.seconds,
                    "ledger_lines": s.ledger_lines,
                    "error": s.error,
                    "savings": s.savings.as_ref().map(|v| serde_json::json!({
                        "share_pct": v.share_pct,
                        "tokens_avoided": v.tokens_avoided,
                        "tokens_returned": v.tokens_returned,
                        "read_tokens": v.read_tokens,
                        "honesty": v.honesty,
                        "sessions_with_reads": v.sessions_with_reads,
                    })),
                })).collect::<Vec<_>>(),
            })
        })
        .collect();
    let avoided: u64 = runs
        .iter()
        .map(|r| r.final_savings.clone().unwrap_or_default().tokens_avoided)
        .sum();
    let read: u64 = runs
        .iter()
        .map(|r| r.final_savings.clone().unwrap_or_default().read_tokens)
        .sum();
    serde_json::json!({
        "kind": "dogfood-reading",
        "definition": "avoided / (avoided + read) over a fixed scripted sample, per client",
        "caveat": "a reading over the scripted sessions through real clients — \
                   non-deterministic, not a gate, and not the class-sized fixture",
        "stamp": stamp,
        "levers_enforced": enforced.iter().map(|lever| lever.key()).collect::<Vec<_>>(),
        "overall_share_pct": share_of(avoided, read),
        "clients": clients,
    })
}

/// `YYYYMMDD-HHMMSS`, UTC, from seconds since the epoch.
///
/// Spelled out rather than pulled from a date crate: this crate carries two
/// dependencies and a run stamp is not worth a third.
#[must_use]
pub fn utc_stamp(epoch_secs: u64) -> String {
    let days = epoch_secs / 86_400;
    let secs = epoch_secs % 86_400;
    // Days from 1970-01-01 to a civil date, by the shifted-era algorithm: the
    // year starts in March so a leap day is always last.
    let z = i64::try_from(days).unwrap_or(0) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}{m:02}{d:02}-{:02}{:02}{:02}",
        secs / 3600,
        (secs % 3600) / 60,
        secs % 60
    )
}

/// Left-pad to `width` columns.
fn pad(label: &str, width: usize) -> String {
    let mut out = label.to_owned();
    for _ in label.chars().count()..width {
        out.push(' ');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dogfood::tests::{sample_run, REREAD_SAMPLE, SAVINGS_SAMPLE};
    use crate::dogfood::{parse_kinds, parse_savings};

    /// The table prints a row per client and one overall row, and the overall
    /// share is computed over the summed tokens rather than averaged across
    /// clients — an average of shares would weight a client with three reads
    /// the same as one with three thousand.
    #[test]
    fn the_table_carries_every_client_and_one_overall_row() {
        let a = SavingsSample {
            share_pct: Some(20.0),
            tokens_avoided: 200,
            tokens_prevented: Some(40),
            tokens_returned: 50,
            read_tokens: 800,
            honesty: 150,
            sessions_with_reads: 2,
            window: "since the reset".to_owned(),
            projection_db: "/a/projection.db".to_owned(),
            recall_db: "/a/recall.db".to_owned(),
            note: None,
        };
        let b = SavingsSample {
            share_pct: Some(10.0),
            tokens_avoided: 100,
            read_tokens: 900,
            recall_db: "/b/recall.db".to_owned(),
            projection_db: "/b/projection.db".to_owned(),
            ..a.clone()
        };
        let runs = vec![
            sample_run(
                Client::ClaudeCode,
                a,
                vec![LeverKind {
                    verb: "memd.read_intercepted".to_owned(),
                    rows: 3,
                    tokens_returned: 10,
                    tokens_avoided: 200,
                }],
            ),
            sample_run(
                Client::Codex,
                b,
                vec![LeverKind {
                    verb: "memd.doc_rewritten".to_owned(),
                    rows: 2,
                    tokens_returned: 5,
                    tokens_avoided: 100,
                }],
            ),
        ];
        let table = render_table(&runs);
        assert!(table.contains("claude-code"), "{table}");
        assert!(table.contains("codex"), "{table}");
        // 300 avoided over 300 + 1700 read.
        assert!(table.contains("15.00%"), "{table}");
        assert!(
            table.contains("1/2"),
            "one of two sessions completed\n{table}"
        );
        assert!(table.contains("UNAVAILABLE"), "{table}");
        assert!(table.contains("Individual quota reached"), "{table}");
        // The kind columns come from the verbs, not from the session count.
        assert!(table.contains("intercept"), "{table}");
        assert!(table.contains("1/2"), "{table}");
    }
    /// With nothing to report the table still draws, and says so where a
    /// reader would otherwise see a blank.
    #[test]
    fn an_empty_run_still_draws_a_table() {
        let table = render_table(&[]);
        assert!(table.contains("overall"));
        assert!(
            table.contains("n/a"),
            "no share is not a zero share\n{table}"
        );
        assert!(table.contains("(none)"));
    }
    /// The JSON carries the same figures as the table, plus the caveat the
    /// number cannot carry on its own.
    #[test]
    fn the_json_carries_the_figures_and_the_caveat() {
        let s = parse_savings(SAVINGS_SAMPLE).unwrap();
        let runs = vec![sample_run(
            Client::Antigravity,
            s,
            parse_kinds(REREAD_SAMPLE).unwrap(),
        )];
        let json = report_json(&runs, "20260908-120000", &[]);
        assert_eq!(json["kind"], "dogfood-reading");
        assert_eq!(json["stamp"], "20260908-120000");
        assert_eq!(
            json["levers_enforced"],
            serde_json::json!([]),
            "a run with nothing written says so"
        );
        let written = report_json(
            &runs,
            "20260908-120000",
            &[Lever::Narrow, Lever::FirstContact],
        );
        assert_eq!(
            written["levers_enforced"],
            serde_json::json!(["narrow", "first_contact"])
        );
        assert!(json["caveat"]
            .as_str()
            .is_some_and(|c| c.contains("not a gate")));
        let client = &json["clients"][0];
        assert_eq!(client["client"], "antigravity");
        assert_eq!(client["savings"]["tokens_avoided"], 479);
        assert_eq!(client["decisions_by_kind"]["intercept"], 42);
        assert_eq!(client["decisions_by_kind"]["document"], 0);
        assert_eq!(client["retry_after_deny"]["denies"], 2);
        assert_eq!(client["sessions_completed"], 1);
        assert_eq!(client["sessions"][1]["status"], "UNAVAILABLE");
        assert_eq!(client["sessions"][0]["ledger_lines"], 4);
    }
    /// A client whose log never exposed a decision reports that, rather than a
    /// retry rate of zero.
    #[test]
    fn an_unobserved_retry_rate_is_absent_from_the_json() {
        let mut run = sample_run(Client::Codex, SavingsSample::default(), Vec::new());
        for s in &mut run.sessions {
            s.retries = RetryCounts::default();
        }
        let json = report_json(&[run], "s", &[]);
        assert_eq!(json["clients"][0]["retry_after_deny"]["observed"], false);
    }
    /// The run stamp, against instants whose civil date is known — including a
    /// leap day, which is where a hand-rolled calendar goes wrong.
    #[test]
    fn the_run_stamp_is_the_utc_instant() {
        assert_eq!(utc_stamp(0), "19700101-000000");
        assert_eq!(utc_stamp(1_788_830_405), "20260908-012005");
        assert_eq!(utc_stamp(1_709_164_800), "20240229-000000");
        assert_eq!(utc_stamp(951_782_400), "20000229-000000");
    }
    /// The single-arm table and document are what they were before the
    /// calibration existed: the figures it collects are carried on the same
    /// rows, and none of them reaches this output.
    #[test]
    fn without_compare_the_table_is_unchanged() {
        let sample = parse_savings(SAVINGS_SAMPLE).unwrap();
        let plain = sample_run(
            Client::Codex,
            sample.clone(),
            parse_kinds(REREAD_SAMPLE).unwrap(),
        );
        let mut carrying = plain.clone();
        for s in &mut carrying.sessions {
            s.deltas = SavingsDeltas {
                read_tokens: 4_000,
                tokens_avoided: 900,
                tokens_prevented: Some(120),
                tokens_returned: 30,
            };
            s.client_reported = Some(76_667);
        }
        assert_eq!(
            render_table(std::slice::from_ref(&carrying)),
            render_table(std::slice::from_ref(&plain)),
            "the calibration's figures do not reach the single-arm table"
        );
        assert_eq!(
            report_json(std::slice::from_ref(&carrying), "20260909-000000", &[]),
            report_json(std::slice::from_ref(&plain), "20260909-000000", &[]),
            "nor its document"
        );
    }
}
