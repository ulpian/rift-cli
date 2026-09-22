//! The `rift blame` document: the human lines an operator reads and the
//! `--json` envelope a script parses.
//!
//! A module of its own because it is PURE — everything it needs is already
//! resolved — so the locked shapes can be driven by tests without a store, a
//! repository or a shard.

use std::collections::{BTreeMap, HashMap};

use rift_shadow::attribution::provenance::ClaimSet;

use super::blame::{is_uncommitted, LineRange};
use super::resolve::{CheckpointFacts, Reads, Resolved, Tag};
use super::safe_text::{plural, safe_path, short};
use super::worktree::{spread_text, worktree_label};
use crate::cli::Outcome;

/// Envelope version for `rift blame --json`. Bump on any breaking shape change.
const SCHEMA_VERSION: u32 = 1;

/// What a whole document's spans add up to, accumulated as they are rendered.
///
/// Every count here is derived from the SPANS, never from the caches the
/// resolver filled: a cache is an optimisation, and a summary that counts
/// cache entries reports how hard the resolver worked rather than what the
/// file is made of. (Found by a test that passed an empty cache and got
/// "0 commits" for a two-commit file.)
#[derive(Default)]
struct Tally<'a> {
    /// Total lines across every span.
    total: usize,
    /// Each commit's claims, serialized once per commit rather than per span.
    commits: BTreeMap<&'a str, &'a ClaimSet>,
    /// Each answering checkpoint's metadata, likewise once.
    checkpoints: BTreeMap<&'a str, serde_json::Value>,
    /// The commits that really are commits — an uncommitted span is not one.
    distinct_commits: std::collections::BTreeSet<&'a str>,
    /// The checkpoints that answered for at least one span.
    distinct_checkpoints: std::collections::BTreeSet<&'a str>,
    /// Lines whose answer rests on recorded file evidence.
    evidence_lines: usize,
}

impl<'a> Tally<'a> {
    /// Fold one span in.
    fn record_span(
        &mut self,
        r: &'a Resolved,
        claims: &'a HashMap<String, ClaimSet>,
        facts: &'a HashMap<String, CheckpointFacts>,
    ) {
        let width = r.span.end - r.span.start + 1;
        self.total += width;
        if !is_uncommitted(&r.span.commit) {
            self.distinct_commits.insert(r.span.commit.as_str());
        }
        if let Some(set) = claims.get(&r.span.commit) {
            self.commits.insert(r.span.commit.as_str(), set);
        }
        let Some(sel) = &r.selection else {
            return;
        };
        self.distinct_checkpoints.insert(sel.checkpoint_id.as_str());
        if sel.basis.is_evidence() {
            self.evidence_lines += width;
        }
        if let Some(f) = facts.get(sel.checkpoint_id.as_str()) {
            self.checkpoints
                .insert(sel.checkpoint_id.as_str(), checkpoint_json(f));
        }
    }

    /// The closing human line.
    ///
    /// The rendering clause for this verb is the members the answering
    /// checkpoints were ARCHIVED IN (the exact word, not "worked in" — the
    /// moving session's question is `session_worktree`'s). One clause rather
    /// than a column per span: a file's spans routinely share one checkpoint,
    /// and a repeated label per row would push the evidence count off a narrow
    /// terminal to say the same thing n times.
    fn summary_line(&self, facts: &HashMap<String, CheckpointFacts>) -> String {
        let archived_in: Vec<String> = self
            .distinct_checkpoints
            .iter()
            .map(|id| worktree_label(facts.get(*id).and_then(|f| f.worktree.as_ref())))
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        format!(
            "  {} across {}; {} claimed; {} backed by recorded file evidence; archived_in: {}",
            plural(self.total, "line"),
            plural(self.distinct_commits.len(), "commit"),
            plural(self.distinct_checkpoints.len(), "checkpoint"),
            self.evidence_lines,
            spread_text(&archived_in),
        )
    }
}

/// Render the human and JSON halves. Pure: everything it needs is resolved.
///
/// `reads` is carried into `--json` rather than kept as a test-only counter, so
/// the module's own cost claim is visible in the artifact and not merely
/// assertable about it — and so the counters live on the production path they
/// describe rather than beside it.
pub(super) fn render(
    file: &str,
    range: Option<LineRange>,
    resolved: &[Resolved],
    claims: &HashMap<String, ClaimSet>,
    facts: &HashMap<String, CheckpointFacts>,
    reads: Reads,
) -> Outcome {
    let mut tally = Tally::default();
    let mut human = blame_header(file, range);
    let mut spans_json = Vec::with_capacity(resolved.len());
    for r in resolved {
        let tag = Tag::of(r.counts.as_ref());
        tally.record_span(r, claims, facts);
        human.push_str(&format!(
            "  {:>11}  {}  {}{}\n",
            format!("{}-{}", r.span.start, r.span.end),
            short(&r.span.commit),
            answer(r, tag),
            import_note(r, facts),
        ));
        spans_json.push(span_json(r, tag));
    }
    human.push_str(&tally.summary_line(facts));
    let mut json = serde_json::json!({
        "schema_version": SCHEMA_VERSION,
        "file": file,
        "lines": tally.total,
        "spans": spans_json,
        "commits": tally.commits,
        "checkpoints": tally.checkpoints,
        "summary": {
            "commits": tally.distinct_commits.len(),
            "checkpoints": tally.distinct_checkpoints.len(),
            "lines_with_file_evidence": tally.evidence_lines,
        },
        // What answering cost, in reads — not in cache entries. Additive to
        // the envelope, so `schema_version` does not move.
        "reads": reads,
    });
    if let (Some(r), Some(obj)) = (range, json.as_object_mut()) {
        // Present exactly when `--line` was asked — absent-when-whole-file, so
        // the pre-range envelope byte-stays and `schema_version` does not move.
        obj.insert("range".into(), serde_json::json!(r));
    }
    Outcome::ok(human, json)
}

/// The document's first line.
///
/// Escaped, not raw: see `safe_path`. Raw, this header is one newline away
/// from printing a forged blame row above the real ones.
fn blame_header(file: &str, range: Option<LineRange>) -> String {
    match range {
        Some(r) => format!(
            "rift blame {} --line {}-{}\n",
            safe_path(file),
            r.start,
            r.end
        ),
        None => format!("rift blame {}\n", safe_path(file)),
    }
}

/// One span's `--json` object.
fn span_json(r: &Resolved, tag: Tag) -> serde_json::Value {
    serde_json::json!({
        "start": r.span.start,
        "end": r.span.end,
        "lines": r.span.end - r.span.start + 1,
        "commit": r.span.commit,
        "path": r.span.path,
        "tag": tag,
        "selection": r.selection,
        "attribution": r.counts,
        "sessions": r.sessions,
    })
}

/// One answering checkpoint's `--json` object.
fn checkpoint_json(f: &CheckpointFacts) -> serde_json::Value {
    serde_json::json!({
        "created_at_ms": f.created_at_ms,
        "files_touched": f.files_touched.len(),
        "sessions": f.sessions,
        "strategy": f.strategy,
        // Worktree provenance: beside `strategy`, the same place the `import`
        // marker went. Null is UNKNOWN, and the human line below spells it out.
        "worktree": f.worktree,
    })
}

/// The human-readable answer for one span.
pub(super) fn answer(r: &Resolved, tag: Tag) -> String {
    let Some(sel) = &r.selection else {
        return if is_uncommitted(&r.span.commit) {
            "not committed".to_owned()
        } else {
            "no checkpoint claimed".to_owned()
        };
    };
    let who = r.sessions.first().map_or_else(
        || "(no session recorded)".to_owned(),
        |s| format!("{}/{}", s.agent, s.model),
    );
    let basis = match sel.basis {
        rift_shadow::attribution::SelectionBasis::FileTouched => "file-touched",
        rift_shadow::attribution::SelectionBasis::NoPathEvidence => "no-path-evidence",
        rift_shadow::attribution::SelectionBasis::PathExcluded => "path-excluded (guess)",
    };
    let conf = r.counts.map_or_else(
        || "conf n/a".to_owned(),
        |c| {
            format!(
                "conf {}.{:03}",
                c.confidence_milli / 1000,
                c.confidence_milli.rem_euclid(1000)
            )
        },
    );
    format!(
        "ckpt {}{}  {}  {who}  {conf}  {basis}{}",
        short(&sel.checkpoint_id),
        // The id was claimed by the commit but no local row answers for it, so
        // everything after it is absence rather than fact. Saying so in the
        // human render matters more than in the JSON: `resolved: false` is a
        // field a script reads, while a printed id reads as knowledge.
        if sel.resolved {
            ""
        } else {
            " (not known locally)"
        },
        tag.as_str(),
        if sel.considered > 1 {
            format!("  ({} claimed)", sel.considered)
        } else {
            String::new()
        }
    )
}

/// The human-render suffix for a checkpoint `rift import` landed: an imported
/// transcript is read-only provenance, not a live archive, and the line says
/// so. Empty for every other strategy and for an unresolved
/// id — `explain` already prints `metadata.strategy` verbatim.
fn import_note(r: &Resolved, facts: &HashMap<String, CheckpointFacts>) -> &'static str {
    let imported = r
        .selection
        .as_ref()
        .and_then(|sel| facts.get(&sel.checkpoint_id))
        .is_some_and(|f| f.strategy.as_deref() == Some("import"));
    if imported {
        " (imported transcript, read-only)"
    } else {
        ""
    }
}

#[cfg(test)]
mod tests {
    use rift_shadow::attribution::provenance::{parse_checkpoint_claims, Selection};
    use rift_shadow::attribution::SelectionBasis;

    use super::*;
    use crate::cli::exit;
    use crate::provenance::blame::Span;
    use crate::provenance::resolve::{AttrCounts, SessionRef};
    use crate::provenance::safe_text::{Label, MAX_PATH};
    use crate::provenance::tests::{oid, render_whole};

    /// With a range, the document carries `range` and the spans carry TRUE
    /// file line numbers (the porcelain numbered the excerpt from 1; the
    /// shift is what makes `--line 40-41` say 40, not 1). Without one, the
    /// envelope byte-stays: no `range` key at all.
    #[test]
    fn a_ranged_answer_carries_true_line_numbers_and_names_its_range() {
        let rows = [resolved_row(40, 41, None)];
        let out = render(
            "src/lib.rs",
            Some(LineRange { start: 40, end: 41 }),
            &rows,
            &HashMap::new(),
            &HashMap::new(),
            Reads::default(),
        );
        assert_eq!(out.json["range"]["start"], 40);
        assert_eq!(out.json["range"]["end"], 41);
        assert_eq!(out.json["spans"][0]["start"], 40);
        assert_eq!(out.json["lines"], 2);
        assert!(out.human.contains("--line 40-41"), "{}", out.human);

        let whole = render_whole(
            "src/lib.rs",
            &rows,
            &HashMap::new(),
            &HashMap::new(),
            Reads::default(),
        );
        assert!(
            whole.json.get("range").is_none(),
            "absent-when-whole-file keeps the pre-range envelope byte-stable"
        );
    }

    /// The filter bound where it is *used*, not only where it is defined.
    ///
    /// `Label::of` being correct says nothing about the renderer unless the
    /// renderer can only print `Label`s — which is now a type property, so this
    /// pins the other half: what a hostile agent/model actually looks like on
    /// the terminal after passing through `answer`.
    #[test]
    fn the_rendered_who_is_the_filtered_label_not_the_recorded_one() {
        let mut row = resolved_row(1, 2, Some(a_selection(SelectionBasis::FileTouched, 1)));
        row.sessions = vec![SessionRef {
            session_id: "s1".to_owned(),
            agent: Label::of("claude\u{1b}[2Jcode"),
            model: Label::of("opus\n5"),
        }];
        let out = render_whole(
            "src/lib.rs",
            &[row],
            &HashMap::new(),
            &HashMap::new(),
            Reads::default(),
        );
        assert!(!out.human.contains('\u{1b}'), "{}", out.human);
        assert!(out.human.contains("claude..2Jcode/opus.5"), "{}", out.human);
        assert_eq!(out.human.lines().count(), 3, "{}", out.human);
    }

    /// The defect that rejected the first cut of this verb, stated as a test.
    ///
    /// A filename is attacker-controlled in any repository you clone. The
    /// header used to interpolate it raw, so a file *named* a newline followed
    /// by a plausible blame row printed that row on the operator's terminal,
    /// byte-indistinguishable from a real one and asserting `ai` authorship on
    /// `file-touched` — the one basis this module reserves for recorded
    /// evidence. The assertion is deliberately on the SHAPE OF THE OUTPUT
    /// rather than on `safe_path` in isolation: a forged row can only exist as
    /// an extra line, so counting lines is what actually refutes the forgery.
    #[test]
    fn a_hostile_filename_cannot_forge_a_row_or_reach_the_terminal_raw() {
        let forged = "a.rs\n          1-9  0123456789ab  ckpt cafecafecafe  \
                      ai  trusted-model  conf 1.000  file-touched";
        let rows = [
            resolved_row(1, 4, Some(a_selection(SelectionBasis::FileTouched, 1))),
            resolved_row(5, 6, None),
        ];
        let out = render_whole(
            forged,
            &rows,
            &HashMap::new(),
            &HashMap::new(),
            Reads::default(),
        );
        assert_eq!(
            out.human.lines().count(),
            1 + rows.len() + 1,
            "one header, one line per span, one summary: {}",
            out.human
        );
        // The only control character in the whole render is the newline this
        // renderer itself writes between rows — and the count above already
        // says how many of those there may be.
        assert!(
            !out.human.chars().any(|c| c.is_control() && c != '\n'),
            "{:?}",
            out.human.replace('\n', "|")
        );
        // Escape sequences and bidi overrides are text, not terminal control.
        let ansi = render_whole(
            "x\u{1b}[31m\r\nFORGED.rs\u{202e}",
            &rows,
            &HashMap::new(),
            &HashMap::new(),
            Reads::default(),
        );
        assert!(!ansi.human.contains('\u{1b}'), "{}", ansi.human);
        assert!(!ansi.human.contains('\u{202e}'), "{}", ansi.human);
        assert_eq!(ansi.human.lines().count(), 1 + rows.len() + 1);
        // A pathological length cannot fill a scrollback; the marker says so.
        let long = render_whole(
            &"p".repeat(9_000),
            &rows,
            &HashMap::new(),
            &HashMap::new(),
            Reads::default(),
        );
        let header = long.human.lines().next().expect("header");
        assert!(header.ends_with("\"…"), "{header}");
        // Exact, and it is the arithmetic the module doc quotes for the one
        // channel this module cannot close: `rift blame ` (11) + the opening
        // quote + MAX_PATH + the closing quote + the cut marker. 174 > 80, so
        // an 80-column terminal wraps it and the continuation is a line this
        // module did not write. If someone lowers MAX_PATH to stop that, this
        // assertion is where the prose gets corrected.
        assert_eq!(header.chars().count(), 11 + 1 + MAX_PATH + 2, "{header}");
        assert_eq!(header.chars().count(), 174, "{header}");
    }

    /// What `--json` actually guarantees — which is **not** "no control byte".
    ///
    /// The first cut of this test asserted that the serialized document holds no
    /// raw control byte at all, over a corpus that stopped at `0x1B`. The
    /// property is false. `serde_json` escapes what JSON requires — every code
    /// point below `0x20`, plus `"` and `\` — and `U+007F` and `U+0085` are both
    /// `Cc`, both `char::is_control`, and both perfectly legal raw inside a JSON
    /// string, so both survive. `U+0085` survives *because* this verb asks git
    /// for `core.quotePath=false`: the fix and the false claim shipped in the
    /// same delta, and the one thing that would have caught it — a corpus that
    /// reached past `0x1B` — was the thing missing. A test whose corpus cannot
    /// reach the failing case is a green light for a property nobody holds.
    ///
    /// The guarantee that does hold is the one this module needs: **an
    /// attacker's bytes stay inside the string they were put in.** They cannot
    /// add a key, close a value, or break the document into lines; the shape is
    /// the renderer's alone, and the value round-trips exactly, which is the
    /// contract for a caller that must get its own path back. What is left
    /// over is named and asserted below rather than left to be discovered — and
    /// it is left over in a document that is *parsed*: every one of those code points is
    /// escaped by `safe_path`, which is the only way a path reaches a terminal.
    ///
    /// If a future `serde_json` starts escaping them, those assertions are
    /// what will say so. Update them and the doc; do not delete them.
    #[test]
    fn the_json_document_cannot_be_given_a_shape_the_renderer_did_not_build() {
        // Every escape class in one name: a C0 run, DEL, two C1 controls (one of
        // them the CSI introducer), both Unicode line separators, and the JSON
        // metacharacters that would be needed to forge a sibling span.
        let forged = "a.rs\n  1-9  0123456789ab  ckpt cafecafecafe  ai\r\u{1b}[31m\
                      \u{7f}\u{85}\u{9b}\u{2028}\u{2029}\"},{\"file\":\"innocent.rs\\";
        let mut rows = [resolved_row(
            1,
            2,
            Some(a_selection(SelectionBasis::FileTouched, 1)),
        )];
        rows[0].span.path = forged.to_owned();
        let out = render_whole(
            forged,
            &rows,
            &HashMap::new(),
            &HashMap::new(),
            Reads::default(),
        );
        let text = serde_json::to_string(&out.json).expect("serialize");

        // 1. Nothing below 0x20 survives, so the document is one line and no C0
        //    control reaches whatever echoes it.
        assert!(
            !text.chars().any(|c| (c as u32) < 0x20),
            "raw C0 byte in --json: {text:?}"
        );
        // 2. The shape is the renderer's. The corpus carries `"`, `}`, `{` and a
        //    trailing backslash — the exact material for a forged sibling span —
        //    and the document still parses to the keys `render` built, with one
        //    span, and no `innocent.rs` anywhere but inside a value.
        let back: serde_json::Value = serde_json::from_str(&text).expect("parses back");
        let mut keys: Vec<&str> = back
            .as_object()
            .expect("object")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "checkpoints",
                "commits",
                "file",
                "lines",
                "reads",
                "schema_version",
                "spans",
                "summary"
            ]
        );
        assert_eq!(back["spans"].as_array().expect("spans").len(), 1);
        // 3. …and it round-trips exactly, because a caller that asked for this
        //    path must get it back — which is also why the values are not
        //    sanitized in place: that would be a lossy transcription of the
        //    thing the caller named.
        assert_eq!(back["file"], forged);
        assert_eq!(back["spans"][0]["path"], forged);

        // 4. What is left over, named. These are legal raw in a JSON string, and this
        //    is what the old assertion could not see.
        for c in ['\u{7f}', '\u{85}', '\u{9b}', '\u{2028}', '\u{2029}'] {
            assert!(
                text.contains(c),
                "U+{:04X} is expected raw in --json today",
                c as u32
            );
            // The human render is the surface that must not carry them, and it
            // does not: `safe_path` escapes every one.
            assert!(
                !out.human.contains(c),
                "U+{:04X} reached the human render",
                c as u32
            );
        }
    }

    fn resolved_row(start: usize, end: usize, sel: Option<Selection>) -> Resolved {
        Resolved {
            span: Span {
                start,
                end,
                commit: oid('a'),
                path: "src/lib.rs".to_owned(),
            },
            selection: sel,
            counts: Some(AttrCounts {
                agent_added: 9,
                human_added: 0,
                human_modified_agent: 0,
                confidence_milli: 920,
            }),
            sessions: vec![SessionRef {
                session_id: "s1".to_owned(),
                agent: Label::of("claude-code"),
                model: Label::of("opus"),
            }],
        }
    }

    fn a_selection(basis: SelectionBasis, considered: usize) -> Selection {
        Selection {
            checkpoint_id: "abc1234def567".to_owned(),
            basis,
            considered,
            resolved: true,
        }
    }

    #[test]
    fn a_claimed_but_unknown_checkpoint_says_so_where_a_human_reads_it() {
        // Observed in real use: a commit claiming an id this machine has never seen is the
        // ordinary case in a fresh clone, and printing a bare id there reads as
        // knowledge. `resolved: false` in the JSON is not enough on its own.
        let mut sel = a_selection(SelectionBasis::NoPathEvidence, 1);
        sel.resolved = false;
        let rows = [Resolved {
            counts: None,
            ..resolved_row(1, 4, Some(sel))
        }];
        let out = render_whole(
            "src/lib.rs",
            &rows,
            &HashMap::new(),
            &HashMap::new(),
            Reads::default(),
        );
        assert!(out.human.contains("not known locally"), "{}", out.human);
        assert_eq!(out.json["spans"][0]["selection"]["resolved"], false);
        // And the resolved case does not carry the caveat.
        let ok = [resolved_row(
            1,
            4,
            Some(a_selection(SelectionBasis::FileTouched, 1)),
        )];
        let out = render_whole(
            "src/lib.rs",
            &ok,
            &HashMap::new(),
            &HashMap::new(),
            Reads::default(),
        );
        assert!(!out.human.contains("not known locally"), "{}", out.human);
    }

    /// **An imported checkpoint names its strategy in the documents.**
    /// The JSON carries the row's `strategy` verbatim under
    /// `checkpoints.{id}` and the human line says "(imported transcript,
    /// read-only)" for the `import` marker only — a live `manual-commit` row
    /// carries the key and no note.
    #[test]
    fn an_imported_checkpoint_names_its_strategy_in_the_documents() {
        let rows = [resolved_row(
            1,
            3,
            Some(a_selection(SelectionBasis::FileTouched, 1)),
        )];
        let id = rows[0].selection.as_ref().unwrap().checkpoint_id.clone();
        let facts_with = |strategy: &str| {
            let mut facts = HashMap::new();
            facts.insert(
                id.clone(),
                CheckpointFacts {
                    created_at_ms: Some(42),
                    files_touched: vec!["src/lib.rs".to_owned()],
                    sessions: Vec::new(),
                    strategy: Some(strategy.to_owned()),
                    worktree: None,
                },
            );
            facts
        };
        let imported = render_whole(
            "src/lib.rs",
            &rows,
            &HashMap::new(),
            &facts_with("import"),
            Reads::default(),
        );
        assert_eq!(imported.json["checkpoints"][&id]["strategy"], "import");
        assert!(
            imported.human.contains("(imported transcript, read-only)"),
            "{}",
            imported.human
        );
        let live = render_whole(
            "src/lib.rs",
            &rows,
            &HashMap::new(),
            &facts_with("manual-commit"),
            Reads::default(),
        );
        assert_eq!(live.json["checkpoints"][&id]["strategy"], "manual-commit");
        assert!(
            !live.human.contains("imported transcript"),
            "{}",
            live.human
        );
    }

    #[test]
    fn the_summary_counts_agree_with_the_spans() {
        let rows = [
            resolved_row(1, 8, Some(a_selection(SelectionBasis::FileTouched, 1))),
            resolved_row(9, 10, None),
        ];
        let out = render_whole(
            "src/lib.rs",
            &rows,
            &HashMap::new(),
            &HashMap::new(),
            Reads::default(),
        );
        assert!(
            out.human.contains("10 lines across 1 commit;"),
            "{}",
            out.human
        );
        assert_eq!(out.json["lines"], 10);
    }

    /// The locked document: two spans, one of them answered by a checkpoint
    /// with a session, a strategy and a member, the other answered by nothing.
    fn locked_envelope() -> Outcome {
        let rows = [
            resolved_row(1, 8, Some(a_selection(SelectionBasis::FileTouched, 3))),
            resolved_row(9, 9, None),
        ];
        let mut claims = HashMap::new();
        claims.insert(
            oid('a'),
            parse_checkpoint_claims("s\n\nRift-Checkpoint: abc1234def567\n"),
        );
        let mut facts = HashMap::new();
        facts.insert(
            "abc1234def567".to_owned(),
            CheckpointFacts {
                created_at_ms: Some(42),
                files_touched: vec!["src/lib.rs".to_owned()],
                sessions: vec![SessionRef {
                    session_id: "s1".to_owned(),
                    agent: Label::of("claude-code"),
                    model: Label::of("opus"),
                }],
                strategy: Some("manual-commit".to_owned()),
                worktree: Some(Label::of("swt-d")),
            },
        );
        render_whole("src/lib.rs", &rows, &claims, &facts, Reads::default())
    }

    #[test]
    fn the_json_envelope_is_the_contract() {
        let out = locked_envelope();
        let v = &out.json;
        assert_eq!(v["schema_version"], 1);
        assert_eq!(v["file"], "src/lib.rs");
        assert_eq!(v["lines"], 9);
        let spans = v["spans"].as_array().expect("spans array");
        assert_eq!(spans.len(), 2);
        for key in [
            "start",
            "end",
            "lines",
            "commit",
            "path",
            "tag",
            "selection",
            "attribution",
            "sessions",
        ] {
            assert!(spans[0].get(key).is_some(), "span key {key}");
        }
        assert_eq!(spans[0]["tag"], "ai");
    }

    /// `Selection` is composed, not re-spelled: its own field names and its
    /// snake_case basis rename are the contract.
    #[test]
    fn a_composed_selection_keeps_its_own_field_names() {
        let out = locked_envelope();
        let spans = out.json["spans"].as_array().expect("spans array");
        assert_eq!(spans[0]["selection"]["basis"], "file_touched");
        assert_eq!(spans[0]["selection"]["considered"], 3);
        assert_eq!(spans[0]["selection"]["checkpoint_id"], "abc1234def567");
        assert_eq!(spans[0]["selection"]["resolved"], true);
        assert!(spans[1]["selection"].is_null());
    }

    /// `ClaimSet` appears once per commit, not once per span.
    #[test]
    fn a_claim_set_is_serialized_once_per_commit() {
        let v = locked_envelope().json;
        assert_eq!(v["commits"][oid('a')]["claims"][0]["ordinal"], 0);
        assert_eq!(v["commits"][oid('a')]["rejected"], 0);
        assert_eq!(v["commits"][oid('a')]["claims"][0]["dialect"], "rift");
        assert_eq!(
            v["commits"][oid('a')]["claims"][0]["checkpoint_id"],
            "abc1234def567"
        );
        assert_eq!(v["commits"][oid('a')]["truncated"], false);
    }

    /// The NESTED leaves too. Locking only the span-level keys left every
    /// composed type free to rename a field without a test noticing, which for
    /// a contract an agent parses is the same exposure one level down.
    #[test]
    fn the_nested_leaves_carry_the_names_a_parser_reads() {
        let out = locked_envelope();
        let v = &out.json;
        let spans = v["spans"].as_array().expect("spans array");
        for (path, want) in [
            (["sessions", "agent"], serde_json::json!("claude-code")),
            (["sessions", "model"], serde_json::json!("opus")),
            (["sessions", "session_id"], serde_json::json!("s1")),
        ] {
            assert_eq!(spans[0][path[0]][0][path[1]], want, "span {path:?}");
            assert_eq!(
                v["checkpoints"]["abc1234def567"][path[0]][0][path[1]], want,
                "checkpoint {path:?}"
            );
        }
        for (key, want) in [
            ("agent_added", 9),
            ("human_added", 0),
            ("human_modified_agent", 0),
            ("confidence_milli", 920),
        ] {
            assert_eq!(spans[0]["attribution"][key], want, "attribution {key}");
        }
        assert_eq!(v["checkpoints"]["abc1234def567"]["created_at_ms"], 42);
        assert_eq!(v["checkpoints"]["abc1234def567"]["files_touched"], 1);
    }

    /// The summary counts, and what answering cost in reads.
    #[test]
    fn the_summary_and_the_reads_ride_in_the_envelope() {
        let v = locked_envelope().json;
        assert_eq!(v["summary"]["lines_with_file_evidence"], 8);
        assert_eq!(v["summary"]["commits"], 1);
        for key in ["messages", "facts", "counts"] {
            assert_eq!(v["reads"][key], 0, "reads {key}");
        }
    }

    #[test]
    fn the_human_render_says_guess_when_the_basis_is_a_guess() {
        let excluded = [resolved_row(
            1,
            2,
            Some(a_selection(SelectionBasis::PathExcluded, 2)),
        )];
        let out = render_whole(
            "src/lib.rs",
            &excluded,
            &HashMap::new(),
            &HashMap::new(),
            Reads::default(),
        );
        assert!(out.human.contains("path-excluded (guess)"), "{}", out.human);
        assert!(out.human.contains("(2 claimed)"), "{}", out.human);
        // Ignorance is not dressed up as a rename caveat.
        let silent = [resolved_row(
            1,
            2,
            Some(a_selection(SelectionBasis::NoPathEvidence, 1)),
        )];
        let out = render_whole(
            "src/lib.rs",
            &silent,
            &HashMap::new(),
            &HashMap::new(),
            Reads::default(),
        );
        assert!(out.human.contains("no-path-evidence"), "{}", out.human);
        assert!(!out.human.contains("guess"), "{}", out.human);
    }

    #[test]
    fn a_commit_that_claims_nothing_is_an_answer_not_an_error() {
        let rows = [resolved_row(1, 3, None)];
        let out = render_whole(
            "src/lib.rs",
            &rows,
            &HashMap::new(),
            &HashMap::new(),
            Reads::default(),
        );
        assert_eq!(out.code, exit::OK);
        assert!(out.human.contains("no checkpoint claimed"), "{}", out.human);
        assert_eq!(out.json["summary"]["lines_with_file_evidence"], 0);
    }
}
