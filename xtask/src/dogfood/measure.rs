//! What a dogfood run measures: the figures Rift reported, the levers that
//! produced them, and the two things a client's own log exposes.
//!
//! Every parser here refuses output that is not the command it thinks it ran:
//! a table that silently reads nothing is the one failure mode a savings
//! harness must not have.

use std::collections::BTreeMap;

/// The figures one `rift savings --json` call reported.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SavingsSample {
    /// `avoided ÷ (avoided + read)`, or `None` when neither side has a token.
    pub share_pct: Option<f64>,
    /// Σ avoided over the audit relation.
    pub tokens_avoided: u64,
    /// The prevented part of `tokens_avoided`, when the reporting surface
    /// carries it — never a second figure beside it, so nothing adds the two.
    /// `None` means the surface did not report it, which prints as `-` rather
    /// than as a zero the harness cannot defend.
    pub tokens_prevented: Option<u64>,
    /// Σ returned over the same relation.
    pub tokens_returned: u64,
    /// Every read token the capture projection holds for this family.
    pub read_tokens: u64,
    /// `avoided − returned`, signed.
    pub honesty: i64,
    /// The headline's `n`.
    pub sessions_with_reads: u64,
    /// The window the figures cover, as the command labelled it.
    pub window: String,
    /// The capture projection the read side came from.
    pub projection_db: String,
    /// The recall projection the avoided side came from.
    pub recall_db: String,
    /// Why the figures are absent, when they are.
    pub note: Option<String>,
}

/// Read a `rift savings --json` document.
///
/// # Errors
/// Output that is not JSON, or JSON without the `overall` block — either of
/// which means the harness is reading something other than the command it
/// thinks it ran, and a silently-zero table is worse than a refusal.
pub fn parse_savings(json: &str) -> Result<SavingsSample, String> {
    let v: serde_json::Value =
        serde_json::from_str(json).map_err(|e| format!("dogfood: savings is not JSON: {e}"))?;
    let overall = v
        .get("overall")
        .ok_or_else(|| "dogfood: savings JSON has no `overall` block".to_owned())?;
    let u = |k: &str| {
        overall
            .get(k)
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0)
    };
    Ok(SavingsSample {
        share_pct: overall.get("share_pct").and_then(serde_json::Value::as_f64),
        tokens_avoided: u("tokens_avoided"),
        // Absent and zero are different answers: a surface that does not
        // report the prevented part must not be read as one that reports none.
        tokens_prevented: overall
            .get("tokens_prevented")
            .and_then(serde_json::Value::as_u64),
        tokens_returned: u("tokens_returned"),
        read_tokens: u("read_tokens"),
        honesty: overall
            .get("honesty")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(0),
        sessions_with_reads: u("sessions_with_reads"),
        window: v
            .pointer("/window/boundary_source")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("(unlabelled)")
            .to_owned(),
        projection_db: text_at(&v, "projection_db"),
        recall_db: text_at(&v, "recall_db"),
        note: v
            .get("note")
            .and_then(serde_json::Value::as_str)
            .map(ToOwned::to_owned),
    })
}

/// One top-level string field, or the empty string.
fn text_at(v: &serde_json::Value, key: &str) -> String {
    v.get(key)
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

/// What one session moved a family's figures by: the reading after it, minus
/// the reading before it.
///
/// Every axis is signed. The figures are cumulative counters over an
/// append-only relation, so a session normally moves each of them up — but a
/// superseding correction can move one down, and a delta that clamped at zero
/// would hide exactly the case a calibration is run to find.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SavingsDeltas {
    /// Read tokens booked across the session.
    pub read_tokens: i64,
    /// Avoided tokens booked across the session.
    pub tokens_avoided: i64,
    /// The prevented part of those, when both readings carried it.
    pub tokens_prevented: Option<i64>,
    /// Returned tokens booked across the session.
    pub tokens_returned: i64,
}

impl SavingsDeltas {
    /// `after − before`, on every axis.
    ///
    /// The prevented axis is `None` unless **both** readings carried the
    /// figure: a delta against an absent side would be the surface's silence
    /// read as a number.
    #[must_use]
    pub fn between(before: &SavingsSample, after: &SavingsSample) -> Self {
        let d = |after: u64, before: u64| {
            i64::try_from(after).unwrap_or(i64::MAX) - i64::try_from(before).unwrap_or(i64::MAX)
        };
        Self {
            read_tokens: d(after.read_tokens, before.read_tokens),
            tokens_avoided: d(after.tokens_avoided, before.tokens_avoided),
            tokens_prevented: after
                .tokens_prevented
                .zip(before.tokens_prevented)
                .map(|(a, b)| d(a, b)),
            tokens_returned: d(after.tokens_returned, before.tokens_returned),
        }
    }
}

/// What a decision was about, as the table groups them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum DecisionKind {
    /// A read the gate answered from what the window already holds.
    Intercept,
    /// A document read pointed at the Markdown beside it.
    Document,
    /// A content search bounded or answered.
    Search,
    /// Everything else the audit relation records.
    Other,
}

impl DecisionKind {
    /// The column label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Intercept => "intercept",
            Self::Document => "document",
            Self::Search => "search",
            Self::Other => "other",
        }
    }
}

/// Which column a `memd.*` verb belongs in.
///
/// Grouped by the lever that produced the row rather than by the verb's exact
/// spelling, so a new verb inside an existing lever lands in the right column
/// without a table edit; anything genuinely new shows up under `other` rather
/// than being folded into a lever it does not belong to.
#[must_use]
pub fn classify_verb(verb: &str) -> DecisionKind {
    let bare = verb.strip_prefix("memd.").unwrap_or(verb);
    if bare.starts_with("read_")
        || bare.starts_with("recovery_")
        || bare.starts_with("manifest_")
        || bare.starts_with("summary_")
        || bare == "known_state"
    {
        DecisionKind::Intercept
    } else if bare.starts_with("doc_") {
        DecisionKind::Document
    } else if bare.starts_with("search") || bare.starts_with("grep_") {
        DecisionKind::Search
    } else {
        DecisionKind::Other
    }
}

/// One row of the audit relation's `GROUP BY verb`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LeverKind {
    /// The verb the rows carry.
    pub verb: String,
    /// How many rows there are.
    pub rows: u64,
    /// What those decisions cost.
    pub tokens_returned: u64,
    /// What those decisions avoided.
    pub tokens_avoided: u64,
}

/// Read the `levers.kinds` block of a `rift memory reread-report --json`.
///
/// # Errors
/// Output that is not JSON. A report with no `levers.kinds` array is not an
/// error — a family that has decided nothing has no kinds — and comes back
/// empty.
pub fn parse_kinds(json: &str) -> Result<Vec<LeverKind>, String> {
    let v: serde_json::Value = serde_json::from_str(json)
        .map_err(|e| format!("dogfood: the reread report is not JSON: {e}"))?;
    let Some(rows) = v
        .pointer("/levers/kinds")
        .and_then(serde_json::Value::as_array)
    else {
        return Ok(Vec::new());
    };
    Ok(rows
        .iter()
        .filter_map(|r| {
            Some(LeverKind {
                verb: r.get("verb")?.as_str()?.to_owned(),
                rows: r
                    .get("rows")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0),
                tokens_returned: r
                    .get("tokens_returned")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0),
                tokens_avoided: r
                    .get("tokens_avoided")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0),
            })
        })
        .collect())
}

/// Rows per decision kind, in the table's column order.
#[must_use]
pub fn tally_kinds(kinds: &[LeverKind]) -> BTreeMap<DecisionKind, u64> {
    let mut out = BTreeMap::new();
    for k in kinds {
        *out.entry(classify_verb(&k.verb)).or_insert(0) += k.rows;
    }
    out
}

// --- what the client's own log exposes ---------------------------------------

/// The marker every line Rift writes into a model's context opens with. It is
/// the one token the three clients pass through unchanged, so it is what a log
/// is searched for.
const RIFT_MARKER: &str = "[rift]";

/// The tool-call fields a client's event stream names its target in.
const TARGET_KEYS: [&str; 4] = ["file_path", "AbsolutePath", "command", "pattern"];

/// Denials seen in one client's log, and how often the model asked again.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RetryCounts {
    /// Lines carrying a Rift decision the model could read.
    pub denies: u64,
    /// Of those, the ones whose target the model named again afterwards.
    pub retries: u64,
    /// Whether the log named tool targets at all. A client whose headless
    /// output does not expose them reports `n/a` rather than a zero it cannot
    /// defend.
    pub observed: bool,
}

/// Count retry-after-deny over one client's structured log.
///
/// The heuristic, stated so a reader can disagree with it: a log line carrying
/// the marker is a decision the model was shown, and every tool target named
/// on that line is a target that was declined; the next line that names the
/// same target without the marker is the model asking again. It is a lower
/// bound — a client that never names its targets reports nothing rather than
/// zero — and it is the same shape the archived spikes counted by hand.
#[must_use]
pub fn retries_after_deny(log: &str) -> RetryCounts {
    let mut out = RetryCounts::default();
    let mut pending: Vec<String> = Vec::new();
    for line in log.lines() {
        let targets = targets_on(line);
        if !targets.is_empty() {
            out.observed = true;
        }
        if line.contains(RIFT_MARKER) {
            for t in targets {
                if !pending.contains(&t) {
                    pending.push(t);
                    out.denies += 1;
                }
            }
            continue;
        }
        for t in targets {
            if let Some(at) = pending.iter().position(|p| *p == t) {
                pending.remove(at);
                out.retries += 1;
            }
        }
    }
    out
}

/// Every tool target named on one line of a client's event stream.
///
/// A scan over the raw text rather than a parse, because the three clients
/// nest these fields differently and at different depths, and the question
/// here is only "which file or command does this line name".
fn targets_on(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    for key in TARGET_KEYS {
        let needle = format!("\"{key}\":");
        let mut from = 0usize;
        while let Some(at) = line.get(from..).and_then(|s| s.find(&needle)) {
            let after = from + at + needle.len();
            from = after;
            let Some(rest) = line.get(after..) else { break };
            let rest = rest.trim_start();
            let Some(body) = rest.strip_prefix('"') else {
                continue;
            };
            let mut value = String::new();
            let mut escaped = false;
            for ch in body.chars() {
                if escaped {
                    value.push(ch);
                    escaped = false;
                } else if ch == '\\' {
                    escaped = true;
                } else if ch == '"' {
                    break;
                } else {
                    value.push(ch);
                }
            }
            if !value.is_empty() && !out.contains(&value) {
                out.push(value);
            }
        }
    }
    out
}

/// Whether a client's own output says it could not run at all.
///
/// Quota and authentication are the two refusals worth telling apart from a
/// bad session: they mean the reading is short by that many sessions, not that
/// the sample said something. The phrases are the ones the three clients
/// actually print.
#[must_use]
pub fn unavailability(log: &str) -> Option<String> {
    // Deliberately NOT here: "command not found". A client whose executable is
    // missing fails to launch and is reported through that path; the phrase in
    // a log is the MODEL's own shell command failing, which is an ordinary
    // thing to happen inside a session and cost a whole run's Codex leg the
    // first time this ran.
    const PHRASES: [&str; 7] = [
        "quota reached",
        "quota exceeded",
        "insufficient_quota",
        "usage limit",
        "rate limit",
        "Please run /login",
        "not authenticated",
    ];
    let lower = log.to_lowercase();
    PHRASES
        .iter()
        .find(|p| lower.contains(&p.to_lowercase()))
        .map(|p| {
            let at = lower.find(&p.to_lowercase()).unwrap_or(0);
            let line = log
                .get(at.saturating_sub(120)..(at + 200).min(log.len()))
                .unwrap_or(p)
                .lines()
                .find(|l| l.to_lowercase().contains(&p.to_lowercase()))
                .unwrap_or(p);
            line.trim().chars().take(200).collect::<String>()
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dogfood::fmt_share;
    use crate::dogfood::tests::{REREAD_SAMPLE, SAVINGS_SAMPLE};

    /// The parser against real output, field for field.
    #[test]
    fn a_captured_savings_document_parses() {
        let s = parse_savings(SAVINGS_SAMPLE).unwrap();
        assert_eq!(s.tokens_avoided, 479);
        assert_eq!(s.tokens_returned, 101);
        assert_eq!(s.read_tokens, 472_420);
        assert_eq!(s.honesty, 378);
        assert_eq!(s.sessions_with_reads, 1);
        // The captured document predates the prevented figure, and an absent
        // figure is absent rather than zero.
        assert_eq!(s.tokens_prevented, None);
        assert!(s.note.is_none());
        assert!(s.share_pct.is_some_and(|v| (v - 0.101_290_1).abs() < 1e-6));
        assert!(s.window.starts_with("since the boundary marker"));
        assert!(s.recall_db.ends_with("recall.db"), "{}", s.recall_db);
        assert!(s.projection_db.ends_with("projection.db"));
    }
    /// A document without the block the figures come from is a refusal, not a
    /// row of zeros: a table that silently reads nothing is the one failure
    /// mode a savings harness must not have.
    #[test]
    fn savings_output_that_is_not_the_command_is_refused() {
        assert!(parse_savings("not json").is_err());
        assert!(parse_savings("{\"window\":{}}").is_err());
    }
    /// A family that has served nothing has no figures, and says so.
    #[test]
    fn an_unmeasured_family_carries_its_note() {
        let s = parse_savings(
            "{\"note\":\"nothing has been captured yet\",\"overall\":{\"share_pct\":null}}",
        )
        .unwrap();
        assert_eq!(s.note.as_deref(), Some("nothing has been captured yet"));
        assert_eq!(s.share_pct, None);
        assert_eq!(fmt_share(s.share_pct), "n/a");
    }
    /// The verbs group into the three levers the table names, and a verb this
    /// table has never seen lands under `other` rather than in a lever it does
    /// not belong to.
    #[test]
    fn verbs_group_by_the_lever_that_produced_them() {
        assert_eq!(
            classify_verb("memd.read_intercepted"),
            DecisionKind::Intercept
        );
        assert_eq!(
            classify_verb("memd.recovery_injected"),
            DecisionKind::Intercept
        );
        assert_eq!(
            classify_verb("memd.manifest_served"),
            DecisionKind::Intercept
        );
        assert_eq!(classify_verb("memd.known_state"), DecisionKind::Intercept);
        assert_eq!(classify_verb("memd.doc_rewritten"), DecisionKind::Document);
        assert_eq!(classify_verb("memd.doc_deferred"), DecisionKind::Document);
        assert_eq!(classify_verb("memd.search_answered"), DecisionKind::Search);
        assert_eq!(classify_verb("memd.grep_capped"), DecisionKind::Search);
        assert_eq!(classify_verb("memd.pack_built"), DecisionKind::Other);
    }
    /// The reread report's kinds, out of live output, and the tally the table
    /// draws from them.
    #[test]
    fn a_captured_reread_report_yields_its_kinds() {
        let kinds = parse_kinds(REREAD_SAMPLE).unwrap();
        assert_eq!(kinds.len(), 4);
        let intercepted = kinds.iter().find(|k| k.verb == "memd.read_intercepted");
        assert_eq!(intercepted.map(|k| k.tokens_avoided), Some(14_467));
        let tally = tally_kinds(&kinds);
        assert_eq!(tally.get(&DecisionKind::Intercept), Some(&42));
        assert_eq!(tally.get(&DecisionKind::Document), None);
        assert!(parse_kinds("{}").unwrap().is_empty(), "no levers, no kinds");
        assert!(parse_kinds("nope").is_err());
    }
    // --- what the client's own log exposes ---
    /// A decline the model was shown, then the same call again: one deny, one
    /// retry. A second file that was never declined is not counted.
    #[test]
    fn a_repeated_call_after_a_decline_is_a_retry() {
        let log = "{\"tool_use\":{\"file_path\":\"a.rs\"}}\n\
                   {\"result\":\"[rift] Read skipped\",\"file_path\":\"a.rs\"}\n\
                   {\"tool_use\":{\"file_path\":\"a.rs\"}}\n\
                   {\"tool_use\":{\"file_path\":\"b.rs\"}}\n";
        let r = retries_after_deny(log);
        assert!(r.observed);
        assert_eq!(r.denies, 1);
        assert_eq!(r.retries, 1);
    }
    /// A decline nobody acted on is a deny with no retry, and a log that names
    /// no target at all reports nothing rather than a zero it cannot defend.
    #[test]
    fn a_log_that_names_no_target_reports_nothing() {
        let declined = retries_after_deny(
            "{\"result\":\"[rift] Read redirected\",\"command\":\"cat spec.docx\"}\n",
        );
        assert_eq!((declined.denies, declined.retries), (1, 0));
        let silent = retries_after_deny("{\"type\":\"assistant\"}\nplain text\n");
        assert!(!silent.observed);
        assert_eq!((silent.denies, silent.retries), (0, 0));
    }
    /// The scan reads the fields the three clients actually name their targets
    /// in, and reads an escaped value without ending it early.
    #[test]
    fn the_scan_reads_every_clients_target_field() {
        assert_eq!(targets_on("{\"file_path\":\"a.rs\"}"), vec!["a.rs"]);
        assert_eq!(
            targets_on("{\"AbsolutePath\":\"/x/b.rs\"}"),
            vec!["/x/b.rs"]
        );
        assert_eq!(targets_on("{\"command\":\"cat c.md\"}"), vec!["cat c.md"]);
        assert_eq!(targets_on("{\"pattern\":\"needle\"}"), vec!["needle"]);
        assert_eq!(targets_on("{\"file_path\":\"a\\\"b\"}"), vec!["a\"b"]);
        assert!(targets_on("{\"other\":\"a.rs\"}").is_empty());
    }
    /// A client that has run out of quota, or is not logged in, is a session
    /// the reading is short by — not a measured zero.
    #[test]
    fn a_client_that_cannot_run_is_named_as_unavailable() {
        assert!(unavailability("Individual quota reached. Resets in 56h.")
            .is_some_and(|r| r.contains("quota reached")));
        assert!(unavailability("error: rate limit exceeded").is_some());
        // The model's own shell command failing is ordinary work inside a
        // session, not a client that could not run — this cost a whole run's
        // Codex leg the first time the harness ran.
        assert!(
            unavailability("zsh:1: command not found: rift").is_none(),
            "a shell command the model ran is not the client refusing"
        );
        assert!(unavailability("{\"type\":\"result\",\"subtype\":\"success\"}").is_none());
    }
    /// The prevented part of the avoided figure is read where the reporting
    /// surface carries it, and stays absent where it does not — the two are
    /// different answers, and only one of them is a number.
    #[test]
    fn the_prevented_part_is_read_where_it_is_reported() {
        let with = parse_savings(
            "{\"overall\":{\"tokens_avoided\":800,\"tokens_prevented\":300,\"read_tokens\":9200}}",
        )
        .unwrap();
        assert_eq!(with.tokens_avoided, 800);
        assert_eq!(with.tokens_prevented, Some(300));
        let without =
            parse_savings("{\"overall\":{\"tokens_avoided\":800,\"read_tokens\":9200}}").unwrap();
        assert_eq!(without.tokens_prevented, None);
    }
    /// A session carries what it alone moved, signed on every axis, and the
    /// prevented axis stays absent unless both readings carried it.
    #[test]
    fn a_session_carries_the_move_it_made() {
        let before = SavingsSample {
            read_tokens: 1_000,
            tokens_avoided: 100,
            tokens_prevented: Some(20),
            tokens_returned: 10,
            ..SavingsSample::default()
        };
        let after = SavingsSample {
            read_tokens: 4_000,
            tokens_avoided: 900,
            tokens_prevented: Some(300),
            tokens_returned: 25,
            ..SavingsSample::default()
        };
        let d = SavingsDeltas::between(&before, &after);
        assert_eq!(d.read_tokens, 3_000);
        assert_eq!(d.tokens_avoided, 800);
        assert_eq!(d.tokens_prevented, Some(280));
        assert_eq!(d.tokens_returned, 15);
        // A correction that lowers a counter is reported as the negative it
        // is; a clamp here would hide the one case worth finding.
        let back = SavingsDeltas::between(&after, &before);
        assert_eq!(back.read_tokens, -3_000);
        assert_eq!(back.tokens_avoided, -800);
        // One side without the figure is not a delta of zero.
        let silent = SavingsSample {
            tokens_prevented: None,
            ..after.clone()
        };
        assert_eq!(
            SavingsDeltas::between(&before, &silent).tokens_prevented,
            None
        );
    }
}
