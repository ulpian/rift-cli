//! The serving-policy half of `rift recall status`: the three tables as the
//! repository wrote them, what each family's pinned channels allow, and the
//! savings share the whole plane is judged by.
//!
//! The capability bridge lives here rather than in the policy crate, which
//! stays a leaf: a family's levers are read from one committed table, and a
//! reader chasing a lever finds the table and its reason in one place.

use std::fmt::Write as _;

use serde_json::{json, Value};

/// The families this program implements to the full extent of their published
/// surface, in the order status prints them.
const ALPHA_FAMILIES: [rift_core::AgentId; 3] = [
    rift_core::AgentId::CLAUDE_CODE,
    rift_core::AgentId::CODEX,
    rift_core::AgentId::ANTIGRAVITY,
];

/// The families whose edit tool's own result is pinned to tell the model that
/// the file it just wrote is still current in its context, each beside the
/// sentence its committed artifact quotes.
///
/// A row here is the whole of the answer to "may a decline rest on this
/// window's own edit?", and **absence is the answer for everyone else**: Antigravity's pinned page states in as many words that nothing says
/// what `replace_file_content` or `write_to_file` returns, Codex's pinned
/// source carries no result text at all, and a family nobody has pinned yet
/// gets the safe answer without anyone remembering to add it.
///
/// The excerpts are not decoration: the test below holds each one against the
/// artifact it claims to come from, so a row cannot be added here by
/// conviction alone.
const PINNED_EDIT_ECHO: &[(rift_core::AgentId, &str)] = &[(
    rift_core::AgentId::CLAUDE_CODE,
    "file state is current in your context — no need to Read it back",
)];

/// What one family's pinned rows allow, read from the single table that
/// asserts a channel from a committed pin.
///
/// This is the bridge the policy crate deliberately does not have: it stays a
/// leaf, and the capability arrives as four facts. The hook path calls this
/// beside [`rift_recall_policy::resolve`], both of which read a file and
/// nothing else — no store is opened to decide what a lever may do.
pub(crate) fn family_capability(
    agent: &rift_core::AgentId,
) -> rift_recall_policy::FamilyCapability {
    use rift_agent::respond::{Channel, RewriteForm, CAPABILITIES};
    let mut capability = rift_recall_policy::FamilyCapability::default();
    for (family, _verb, channel) in CAPABILITIES {
        if family != agent {
            continue;
        }
        match channel {
            Channel::DenyWithReason { confirmed, .. } => {
                capability.confirmed_reason_channel |= *confirmed;
            }
            Channel::UpdatedInput { forms } => {
                capability.document_sidecar_rewrite |=
                    forms.contains(&RewriteForm::DocumentSidecarPath);
                capability.grep_head_limit_rewrite |= forms.contains(&RewriteForm::GrepHeadLimit);
            }
            Channel::AdditionalContext { .. } | Channel::InjectSteps { .. } | Channel::Neutral => {}
        }
    }
    capability.pinned_edit_echo = PINNED_EDIT_ECHO.iter().any(|(family, _)| family == agent);
    capability
}

/// The first reason a family's levers sit below what the file asked for. One
/// note per row: the causes are the same handful, and a reader chasing a lever
/// that is not doing what the file says needs the first one, not a list.
fn cap_note(effective: &rift_recall_policy::Effective) -> &'static str {
    effective
        .read_deny_cap
        .or(effective.search_answer_cap)
        .or(effective.doc_redirect_cap)
        .or(effective.grep_cap_off)
        .or(effective.doc_rewrite_off)
        .map_or("-", rift_recall_policy::CapReason::as_str)
}

fn source_name(source: rift_recall_policy::ServePolicySource) -> &'static str {
    match source {
        rift_recall_policy::ServePolicySource::Declared => "declared",
        rift_recall_policy::ServePolicySource::Default => "absent; defaults apply",
        rift_recall_policy::ServePolicySource::Invalid => "invalid; every lever off",
        rift_recall_policy::ServePolicySource::Refused => "refused; every lever off",
    }
}

/// The three tables as they were read, then what each family's levers may
/// actually do. Both halves are printed because they answer different
/// questions: the first is what the repository asked for, the second is what
/// its agents' pinned channels allow — and the second is the one that runs.
pub(super) fn render_policy(out: &mut String, snapshot: &rift_recall_policy::ServePolicySnapshot) {
    let p = &snapshot.policy;
    let _ = writeln!(
        out,
        "\nServing policy  .rift/policies/serve.toml: {}",
        source_name(snapshot.source)
    );
    let _ = writeln!(
        out,
        "  interception  mode {}  max_slice_bytes {}  min_slice_lines {}  max_inline_hash_bytes {}  recovery_budget {}  inherit_by_name {}",
        p.interception.mode.as_str(),
        p.interception.max_slice_bytes,
        p.interception.min_slice_lines,
        p.interception.max_inline_hash_bytes,
        p.interception.recovery_budget,
        p.interception.inherit_by_name.as_str(),
    );
    // Its own line rather than more columns on the one above, so a reader
    // matching that line by its text still finds it.
    let _ = writeln!(
        out,
        "  first contact first_contact {}  first_contact_min_lines {}  first_contact_min_bytes {}",
        p.interception.first_contact.as_str(),
        p.interception.first_contact_min_lines,
        p.interception.first_contact_min_bytes,
    );
    let _ = writeln!(
        out,
        "  search        mode {}  grep_head_limit {}  glob_top_n {}  answer_budget_tokens {}",
        p.search.mode.as_str(),
        p.search.grep_head_limit,
        p.search.glob_top_n,
        p.search.answer_budget_tokens,
    );
    let _ = writeln!(
        out,
        "  doc           mode {}  channel {}  formats {}  table_rows {}  max_input_bytes {}",
        p.doc.mode.as_str(),
        p.doc.channel.as_str(),
        p.doc.formats.as_str(),
        p.doc.table_rows,
        p.doc.max_input_bytes,
    );
    let _ = writeln!(
        out,
        "                pdf_min_chars_per_page {}  convert_budget_ms {}  ttl_days {}",
        p.doc.pdf_min_chars_per_page, p.doc.convert_budget_ms, p.doc.ttl_days,
    );
    let _ = writeln!(
        out,
        "  {:<13} {:<9} {:<9} {:<9} {:<9} {:<12} note",
        "agent", "re-read", "listing", "grep cap", "doc read", "doc decline"
    );
    for agent in &ALPHA_FAMILIES {
        let e = rift_recall_policy::resolve(p, agent.as_str(), family_capability(agent));
        let _ = writeln!(
            out,
            "  {:<13} {:<9} {:<9} {:<9} {:<9} {:<12} {}",
            agent.as_str(),
            e.read_deny.as_str(),
            e.search_answer.as_str(),
            e.grep_cap.as_str(),
            e.doc_rewrite.as_str(),
            e.doc_redirect.as_str(),
            cap_note(&e),
        );
    }
}

/// One line per family with its savings share, under the overall figure.
///
/// Every number here was computed by the one shared function the reread
/// report, the `savings` verb and the dashboard also call — this renders, it
/// does not calculate, so the status table cannot drift from the card.
pub(super) fn render_savings_shares(out: &mut String, r: &crate::memory::SavingsReport) {
    let s = &r.savings;
    let _ = writeln!(
        out,
        "\nSavings share  avoided ÷ (avoided + read), {}",
        r.window.label()
    );
    if let Some(note) = &r.note {
        let _ = writeln!(out, "  not measured  {note}");
        return;
    }
    let _ = writeln!(
        out,
        "  {:<13} {:<9} avoided {} ÷ {}  n {} session(s) with reads",
        "overall",
        crate::memory::fmt_share(s.share()),
        s.tokens_avoided,
        s.tokens_avoided.saturating_add(s.read_tokens),
        s.sessions_with_reads,
    );
    if s.families.is_empty() {
        let _ = writeln!(out, "  (no family has a decision or a read yet)");
    }
    for f in &s.families {
        let note = if f.decided_without_reads() {
            "  decided, no captured reads"
        } else {
            ""
        };
        let _ = writeln!(
            out,
            "  {:<13} {:<9} honesty {:+}  n {}{note}",
            f.family,
            crate::memory::fmt_share(f.share()),
            f.honesty(),
            f.sessions_with_reads,
        );
    }
    let _ = writeln!(
        out,
        "  {:<13} {:<9} excluding families that decided with no captured reads",
        "adjusted",
        crate::memory::fmt_share(s.share_excluding_undenominated()),
    );
}

/// The same two halves as JSON, for a caller that would rather read the
/// numbers than the table.
pub(super) fn policy_json(snapshot: &rift_recall_policy::ServePolicySnapshot) -> Value {
    let p = &snapshot.policy;
    let agents: Vec<Value> = ALPHA_FAMILIES
        .iter()
        .map(|agent| {
            let e = rift_recall_policy::resolve(p, agent.as_str(), family_capability(agent));
            json!({
                "agent": agent.as_str(),
                "interception_requested": e.read_deny_requested.as_str(),
                "interception": e.read_deny.as_str(),
                "inherit_by_name": e.inherit_by_name.as_str(),
                "first_contact": e.first_contact.as_str(),
                "search_answer": e.search_answer.as_str(),
                "grep_cap": e.grep_cap.as_str(),
                "doc_rewrite": e.doc_rewrite.as_str(),
                "doc_redirect": e.doc_redirect.as_str(),
                "note": cap_note(&e),
            })
        })
        .collect();
    json!({
        "source": source_name(snapshot.source),
        "fail_closed": p.fail_closed,
        "interception": {
            "mode": p.interception.mode.as_str(),
            "max_slice_bytes": p.interception.max_slice_bytes,
            "min_slice_lines": p.interception.min_slice_lines,
            "max_inline_hash_bytes": p.interception.max_inline_hash_bytes,
            "recovery_budget": p.interception.recovery_budget,
            "inherit_by_name": p.interception.inherit_by_name.as_str(),
            "first_contact": p.interception.first_contact.as_str(),
            "first_contact_min_lines": p.interception.first_contact_min_lines,
            "first_contact_min_bytes": p.interception.first_contact_min_bytes,
        },
        "search": {
            "mode": p.search.mode.as_str(),
            "grep_head_limit": p.search.grep_head_limit,
            "glob_top_n": p.search.glob_top_n,
            "answer_budget_tokens": p.search.answer_budget_tokens,
        },
        "doc": {
            "mode": p.doc.mode.as_str(),
            "channel": p.doc.channel.as_str(),
            "formats": p.doc.formats.as_str(),
            "table_rows": p.doc.table_rows,
            "max_input_bytes": p.doc.max_input_bytes,
            "pdf_min_chars_per_page": p.doc.pdf_min_chars_per_page,
            "convert_budget_ms": p.doc.convert_budget_ms,
            "ttl_days": p.doc.ttl_days,
        },
        "agents": agents,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The committed pins, by `include_str!` rather than a runtime read, so a
    /// row that outlives the artifact it quotes fails to compile rather than
    /// failing on someone's machine.
    const CLAUDE_CODE_TOOL_FIELDS: &str = include_str!(
        "../../../../crates/rift-agent-claude-code/fixtures/claude-code-tool-field-surface.md"
    );

    const ANTIGRAVITY_TOOL_FIELDS: &str = include_str!(
        "../../../../crates/rift-agent-antigravity/fixtures/antigravity-tool-field-surface.md"
    );

    /// ★ Every edit-echo row quotes the artifact it names, and the two
    /// families whose artifacts say nothing about what an edit returns have no
    /// row — the fact that keeps a decline off an agent's only sight of its
    /// own edit is a pin, not an opinion.
    #[test]
    fn an_edit_echo_row_is_quoted_by_the_pinned_artifact_it_names() {
        let claude_fields = CLAUDE_CODE_TOOL_FIELDS.replace("\r\n", "\n");
        let antigravity_fields = ANTIGRAVITY_TOOL_FIELDS.replace("\r\n", "\n");
        assert_eq!(
            PINNED_EDIT_ECHO.len(),
            1,
            "one family is pinned today; a new row needs its artifact quoted here too"
        );
        for (family, excerpt) in PINNED_EDIT_ECHO {
            assert_eq!(family, &rift_core::AgentId::CLAUDE_CODE);
            assert!(
                claude_fields.contains(excerpt),
                "the pinned artifact does not contain {excerpt:?}"
            );
        }

        assert!(
            !family_capability(&rift_core::AgentId::ANTIGRAVITY).pinned_edit_echo,
            "the page says nothing about what this family's edit tools return"
        );
        assert!(
            antigravity_fields.contains(
                "Nothing on the page says what\n`replace_file_content` or `write_to_file` returns."
            ),
            "and its artifact is where that is written down"
        );
        assert!(!family_capability(&rift_core::AgentId::CODEX).pinned_edit_echo);
        assert!(family_capability(&rift_core::AgentId::CLAUDE_CODE).pinned_edit_echo);
    }

    /// Render the policy half of `rift recall status` over a repository whose
    /// `.rift/policies/serve.toml` is `src` (absent when `None`).
    fn status_policy(src: Option<&str>) -> (String, Value) {
        let dir = tempfile::tempdir().unwrap();
        let rift_dir = dir.path().join(".rift");
        if let Some(src) = src {
            let pol = rift_dir.join("policies");
            std::fs::create_dir_all(&pol).unwrap();
            std::fs::write(pol.join("serve.toml"), src).unwrap();
        }
        let snapshot = rift_recall_policy::load_serve_policy(&rift_dir);
        let mut out = String::new();
        render_policy(&mut out, &snapshot);
        (out, policy_json(&snapshot))
    }

    /// The status table carries the figure this plane is judged by, one line
    /// per family, and names the family that decided with no captured reads
    /// rather than folding it silently into the overall number.
    #[test]
    fn status_prints_a_savings_share_for_every_family() {
        use rift_recall_ledger::savings::{FamilyShare, Savings};
        let report = crate::memory::SavingsReport {
            projection_db: "p".to_owned(),
            recall_db: "r".to_owned(),
            note: None,
            savings: Savings {
                tokens_avoided: 1_000,
                tokens_prevented: 0,
                tokens_returned: 160,
                claimed_by_pulls: rift_recall_ledger::savings::ClaimedByPulls::default(),
                read_tokens: 9_000,
                channels: rift_recall_ledger::savings::ChannelTokens::default(),
                reads_unmeasured: 0,
                sessions_with_reads: 3,
                families: vec![
                    FamilyShare {
                        family: "claude-code".to_owned(),
                        tokens_avoided: 500,
                        tokens_prevented: 0,
                        tokens_returned: 160,
                        decisions: 4,
                        read_tokens: 9_000,
                        channels: rift_recall_ledger::savings::ChannelTokens::default(),
                        reads_unmeasured: 0,
                        sessions_with_reads: 3,
                    },
                    FamilyShare {
                        family: "codex".to_owned(),
                        tokens_avoided: 500,
                        tokens_prevented: 0,
                        tokens_returned: 0,
                        decisions: 3,
                        read_tokens: 0,
                        channels: rift_recall_ledger::savings::ChannelTokens::default(),
                        reads_unmeasured: 0,
                        sessions_with_reads: 0,
                    },
                ],
            },
            window: rift_recall_ledger::savings_window::Boundary::SinceReset,
            enforce_cut_ms: None,
            enforce_cut_source: "none".to_owned(),
            observe_sessions: 0,
            enforce_sessions: 0,
        };
        let mut out = String::new();
        render_savings_shares(&mut out, &report);
        assert!(out.contains("overall       10.0%"), "{out}");
        assert!(out.contains("n 3 session(s) with reads"), "{out}");
        assert!(out.contains("claude-code   5.3%"), "{out}");
        assert!(
            out.contains("codex         100.0%") && out.contains("decided, no captured reads"),
            "{out}"
        );
        assert!(out.contains("adjusted      5.3%"), "{out}");
    }

    /// An unresolvable family says so, on the same table, instead of printing
    /// a percentage nobody measured.
    #[test]
    fn an_unmeasured_family_prints_the_reason_not_a_percentage() {
        let report = crate::memory::SavingsReport {
            projection_db: "(unresolved)".to_owned(),
            recall_db: "(unresolved)".to_owned(),
            note: Some("the family's stores did not resolve".to_owned()),
            savings: rift_recall_ledger::savings::Savings::default(),
            window: rift_recall_ledger::savings_window::Boundary::SinceReset,
            enforce_cut_ms: None,
            enforce_cut_source: "not resolved".to_owned(),
            observe_sessions: 0,
            enforce_sessions: 0,
        };
        let mut out = String::new();
        render_savings_shares(&mut out, &report);
        assert!(out.contains("not measured"), "{out}");
        assert!(!out.contains('%'), "{out}");
    }

    /// **The two floors are one measurement.**
    ///
    /// `rift-agent` ships the SDK's default and `rift-recall-policy` the
    /// policy's; they are separate constants because the two crates are used
    /// separately, and neither depends on the other, so nothing inside either
    /// can hold them together. This binary depends on both, so this is where
    /// the reconciliation can live at all — and it matters, because the floor
    /// is a consequence of the frame's measured cost and a frame that grew must
    /// move both or neither.
    #[test]
    fn the_sdk_and_the_policy_ship_the_same_slice_floor() {
        assert_eq!(
            rift_agent::DEFAULT_MIN_SLICE_LINES,
            rift_recall_policy::DEFAULT_MIN_SLICE_LINES,
            "the two floors drifted apart; both follow the reason frame's \
             measured cost (`the_reason_frame_pays_for_itself_at_the_floor`)"
        );
    }

    /// What an operator is owed by a status line: the three tables as the file
    /// was read, and then — for every family this program implements — what
    /// each of that family's levers may actually do. The two halves differ,
    /// which is the whole reason both are printed.
    #[test]
    fn status_prints_the_three_tables_and_a_mode_for_every_family() {
        let (out, json) = status_policy(None);
        assert!(out.contains(".rift/policies/serve.toml: absent; defaults apply"));
        assert!(out.contains(
            "interception  mode enforce  max_slice_bytes 98304  min_slice_lines 28  max_inline_hash_bytes 1048576  recovery_budget 800  inherit_by_name observe"
        ));
        assert!(out.contains(
            "first contact first_contact observe  first_contact_min_lines 400  first_contact_min_bytes 16384"
        ));
        assert!(out.contains(
            "search        mode enforce  grep_head_limit 40  glob_top_n 8  answer_budget_tokens 160"
        ));
        assert!(out.contains(
            "doc           mode enforce  channel rewrite  formats all  table_rows 200  max_input_bytes 16777216"
        ));
        assert!(out.contains("pdf_min_chars_per_page 200  convert_budget_ms 250  ttl_days 30"));
        assert!(
            out.contains("agent         re-read   listing   grep cap  doc read  doc decline  note")
        );
        assert!(
            out.contains("claude-code   enforce   enforce   enforce   enforce   enforce      -")
        );
        for family in ["codex", "antigravity"] {
            assert!(
                out.contains(&format!(
                    "{family:<13} enforce   enforce   off       off       enforce"
                )),
                "{family} uses native declines where it has no document/search amendment channel:\n{out}"
            );
        }
        assert_eq!(json["source"], "absent; defaults apply");
        assert_eq!(json["fail_closed"], false);
        assert_eq!(json["doc"]["ttl_days"], 30);
        assert_eq!(json["agents"][0]["agent"], "claude-code");
        assert_eq!(json["agents"][0]["grep_cap"], "enforce");
    }

    /// Retry telemetry is visible in metrics, never a support-tier or mode
    /// gate. Repository-wide enforcement therefore reaches all three alpha
    /// families without an agent-specific override.
    #[test]
    fn retry_telemetry_never_demotes_a_supported_family_on_the_status_line() {
        let (out, json) = status_policy(Some("[interception]\nmode = \"enforce\"\n"));
        assert!(
            out.contains("interception  mode enforce"),
            "the file asked for it:\n{out}"
        );
        for family in ["claude-code", "codex", "antigravity"] {
            let row = out
                .lines()
                .find(|line| line.trim_start().starts_with(family))
                .unwrap_or_else(|| panic!("{family} has no row:\n{out}"));
            assert!(row.contains("enforce"), "{family} is not active: {row}");
            assert!(
                !row.contains("measured retry"),
                "{family} has a stale retry caveat: {row}"
            );
        }
        let agents = json["agents"].as_array().expect("one row per family");
        let codex = agents
            .iter()
            .find(|row| row["agent"] == "codex")
            .expect("a codex row");
        assert_eq!(codex["interception"], "enforce");
        assert_eq!(codex["interception_requested"], "enforce");
        assert_eq!(codex["note"], "no native document/search amendment channel");
    }

    /// A file Rift could not read is not a file it works around. The status
    /// line says so and shows every lever off — including the two that decline
    /// nothing and would otherwise act.
    #[test]
    fn an_unreadable_policy_shows_every_lever_off_and_says_why() {
        let (out, json) = status_policy(Some("[doc]\nttl_days = 0\n"));
        assert!(out.contains(".rift/policies/serve.toml: invalid; every lever off"));
        for family in ["claude-code", "codex", "antigravity"] {
            assert!(
                out.contains(&format!(
                    "{family:<13} off       off       off       off       off"
                )),
                "{family} still has a live lever:\n{out}"
            );
        }
        assert_eq!(json["fail_closed"], true);
        assert_eq!(json["interception"]["mode"], "off");
    }

    /// What a family may be asked for is read from the one table that asserts
    /// a channel from a committed pin, never from a list kept here. The three
    /// families' pins differ in exactly the ways the levers care about.
    #[test]
    fn the_capability_bridge_reads_the_pinned_table() {
        let claude = family_capability(&rift_core::AgentId::CLAUDE_CODE);
        assert!(claude.confirmed_reason_channel);
        assert!(claude.document_sidecar_rewrite && claude.grep_head_limit_rewrite);

        let codex = family_capability(&rift_core::AgentId::CODEX);
        assert!(
            codex.confirmed_reason_channel,
            "a recorded run on the client that has the surface confirmed it"
        );
        assert!(!codex.document_sidecar_rewrite && !codex.grep_head_limit_rewrite);
        assert!(
            !codex.pinned_edit_echo,
            "nothing pins what this family's edit tool tells the model it wrote"
        );

        let antigravity = family_capability(&rift_core::AgentId::ANTIGRAVITY);
        assert!(antigravity.confirmed_reason_channel);
        assert!(!antigravity.document_sidecar_rewrite && !antigravity.grep_head_limit_rewrite);

        // A family with no rows at all is a family with no channel, not a
        // family that inherits somebody else's.
        let unknown = family_capability(&rift_core::AgentId::new("some-editor"));
        assert_eq!(unknown, rift_recall_policy::FamilyCapability::default());
    }

    /// Naming one supported family explicitly does not change the other
    /// supported families' repository-wide enforcement.
    #[test]
    fn an_agent_override_does_not_demote_other_supported_families() {
        let (out, json) = status_policy(Some(
            "[interception]\nmode = \"enforce\"\n\n[interception.agents]\nantigravity = \"enforce\"\n",
        ));
        assert!(out.contains("claude-code   enforce"));
        assert!(out.contains("codex         enforce"), "{out}");
        assert!(out.contains("antigravity   enforce"));
        assert_eq!(json["agents"][1]["interception_requested"], "enforce");
        assert_eq!(json["agents"][1]["interception"], "enforce");
    }
}
