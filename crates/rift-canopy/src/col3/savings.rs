//! The Recall panel's savings figures and the breakdown modal's body: the
//! priced rows, the per-family share table and the worktree block, assembled
//! from strings the money authority and the ledger already decided.

use super::format::fmt_count;
use super::worktree::{savings_worktree_lines, WorktreeAxis};

/// One per-model row composing the USD savings estimate (the priced estimate):
/// an attributed slice of the avoided tokens priced at that model's
/// uncached-input rate. The rows are what the breakdown modal renders, so the
/// headline is explained by exactly the numbers that produced it.
///
/// Labels are pre-rendered by the one money authority (`rift-shadow-cost`,
/// via the data plane) — the render layer draws strings and never does money
/// math or formatting of its own.
#[derive(Clone, Debug, PartialEq)]
pub struct UsdRow {
    /// The model id (e.g. `claude-opus-4-8`).
    pub model: String,
    /// Avoided tokens attributed to this model (a largest-remainder slice;
    /// all rows' tokens sum to exactly the avoided total).
    pub tokens: u64,
    /// The uncached-input rate applied, micro-USD per MTok. `None` = no
    /// seeded price: the row contributes no dollars, honestly.
    pub price_per_mtok_micros: Option<i64>,
    /// `tokens` valued at the rate, micro-USD. `None` iff unpriced.
    pub subtotal_micros: Option<i64>,
    /// Catalog rate label (`"$5.00/MTok"`, no tilde — a recorded fact).
    pub rate_label: Option<String>,
    /// Estimate label (`"~$39.31"`, tilde mandatory). `None` iff unpriced.
    pub subtotal_label: Option<String>,
}

/// One family's row in the savings-share block of the breakdown modal.
///
/// The share pairs the serving relation's avoided tokens with the capture
/// projection's read tokens, joined on the family NAME and nothing else — the
/// modal says so above the rows rather than implying a key that does not
/// exist.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FamilyShareRow {
    /// The family, as the relation that named it spells it.
    pub family: String,
    /// `avoided ÷ (avoided + read)`, `None` when neither side holds a token.
    pub share: Option<f64>,
    /// Avoided minus returned, for this family. Signed: an observe window
    /// that injected and declined nothing is legitimately negative.
    pub honesty: i64,
    /// Sessions of this family that read a file — the `n` behind its share.
    pub sessions_with_reads: u64,
    /// It decided and its reads were never captured, so it has a numerator
    /// and no denominator of its own. Named, never silently dropped.
    pub decided_without_reads: bool,
}

/// The savings share: what fraction of the tokens this repository's agents
/// would have read the recall plane avoided, over the card's window.
///
/// **It is the card's headline**, not a second opinion beside one. Every figure
/// here was computed by `rift_recall_ledger::savings`, which the CLI report,
/// the `savings` verb and the recall status table also call — one computation,
/// so the dashboard and the terminal cannot show the owner two numbers. The
/// card used to publish a second percentage of its own, `avoided ÷ (avoided +
/// returned)` over the audit trail, which is a different question with a
/// different denominator and looked exactly like this one.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ShareData {
    /// The window the figures cover, when a boundary marker narrows it to less
    /// than the whole record. `None` is the whole record — the wording the
    /// card has always used — so an absent field reads as it always did.
    pub window: Option<String>,
    /// The headline share, over the audit relation. `None` is unanswerable —
    /// nothing read and nothing avoided — and the headline draws no percentage.
    pub overall: Option<f64>,
    /// Avoided minus returned over the same relation.
    pub honesty: i64,
    /// The numerator: Σ avoided over the audit relation, in tokens.
    ///
    /// It is carried beside [`Self::read_tokens`] rather than re-derived from
    /// [`Self::overall`] and the read side, because the share is a rounded
    /// percentage and inverting it would put a token count on the card that no
    /// relation ever held. Both come out of one `savings::compose` call, so the
    /// two bars and the headline above them describe one window.
    pub avoided_tokens: u64,
    /// The read side of the denominator.
    pub read_tokens: u64,
    /// Sessions that read a file, over every family.
    pub sessions_with_reads: u64,
    /// Reads in the window that carry no token count at all.
    ///
    /// They sit in the denominator's row set and weigh nothing in it, so the
    /// share is over fewer measured reads than it looks. Published beside the
    /// figure rather than imputed into it — a count nobody froze is not a
    /// count this dashboard will invent.
    pub reads_unmeasured: u64,
    /// The same share over the family rows alone — the comparable twin of
    /// [`Self::excluding`], summed from the same rows.
    pub by_family: Option<f64>,
    /// [`Self::by_family`] with every family that decided and whose reads were
    /// never captured left out of both sides.
    pub excluding: Option<f64>,
    /// Those families, named so the exclusion is auditable.
    pub excluded: Vec<String>,
    /// The per-family rows the modal draws.
    pub families: Vec<FamilyShareRow>,
}

/// Recall savings numbers.
///
/// Every figure here describes **one** window, resolved once by the data plane
/// (the card's period button composed with the family's boundary marker), so
/// the headline, the bars, the money and the modal cannot cover two periods.
/// Nothing on this struct is tweened: they are headlines, and a measurement
/// mid-animation is a number no window ever held.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SavingsData {
    /// Estimated USD saved, integer micro-USD: the sum of the
    /// priced [`UsdRow`] subtotals. `None` is honest absence — no resolvable
    /// pricing basis — and the headline draws no dollar segment.
    pub usd_saved_micros: Option<i64>,
    /// The headline's dollar segment, pre-rendered by the money authority
    /// (`"~$105.90"`).
    /// `Some` exactly when [`Self::usd_saved_micros`] is.
    pub usd_label: Option<String>,
    /// The per-model rows whose priced subtotals compose the headline
    /// (empty exactly when [`Self::usd_saved_micros`] is `None`).
    pub usd_rows: Vec<UsdRow>,
    /// [`ShareData::read_tokens`] valued through the **same** model mix and
    /// the same uncached-input rates as [`Self::usd_saved_micros`], in
    /// micro-USD. `None` is honest absence.
    ///
    /// One basis, read twice — never a second price table: the read side and
    /// the saved side are comparable in dollars only because their tokens
    /// were partitioned across the same models at the same moment.
    pub usd_read_micros: Option<i64>,
    /// The read side's dollar label, pre-rendered by the money authority
    /// (`"~$12.40"`, tilde mandatory). `Some` exactly when [`Self::usd_read_micros`] is.
    pub usd_read_label: Option<String>,
    /// The avoided total decomposed by worktree (the family view). Today every
    /// avoided token lands in [`WorktreeAxis::family_only`], because no serve
    /// relation records the session that was served; the breakdown modal
    /// discloses that rather than attributing a serve to a guess.
    pub by_worktree: WorktreeAxis,
    /// The savings share the owner judges this plane by, overall and per
    /// family — **the card's headline figure**. Absent figures draw nothing.
    ///
    /// **Boxed**, like `DetailView::project_settings` and for the same
    /// reason: this is much the largest part of the struct, and
    /// `FeedPayload::Recall` carries the struct through the update channel.
    /// Unboxed it made the Recall variant more than twice the next largest
    /// and every other feed paid for it.
    pub share: Box<ShareData>,
}

/// How the per-family rows join, said under the table so a family with
/// decisions and no captured read is read as what it is.
const SHARE_JOIN_NOTE: [&str; 4] = [
    "a row is a name one relation used: the serving side's",
    "decisions and the capture projection's reads are joined",
    "by that name alone, so a family with decisions and no",
    "captured read has no read side of its own.",
];

/// The breakdown modal's body (the priced savings estimate) — the full disclosed basis:
/// net avoided tokens, the class assumption, one row per model (attributed
/// slice × catalog rate = subtotal), the composing total, the unpriced
/// coverage when any, and the estimate-of-an-estimate statement. Pure over
/// the savings view-model; every money string was pre-rendered by the one
/// money authority, so this function formats counts and assembles lines —
/// it never computes or formats money.
#[must_use]
pub fn usd_breakdown_lines(sd: &SavingsData) -> Vec<String> {
    let Some(total_label) = sd.usd_label.as_deref() else {
        // Reachable, and not rarely: the headline opens this modal on every
        // repository whose models carry no seeded price. The dollar half says
        // it has nothing to say, and the share half below still answers.
        let mut lines = vec![
            "no dollar figure: no seeded price applies to this".to_owned(),
            "project's models — the token headline stands alone".to_owned(),
        ];
        lines.extend(savings_share_lines(&sd.share));
        return lines;
    };
    // The rows' own slices, which sum to exactly the avoided total this card's
    // window holds — the same figure the headline and the saved bar draw,
    // because the money is priced from the share rather than from a second
    // read of the audit relation over a second window.
    let avoided: u64 = sd.usd_rows.iter().map(|r| r.tokens).sum();
    let mut lines = vec![
        format!("net tokens avoided   {}", fmt_count(avoided)),
        "priced as uncached input, at the current seeded".to_owned(),
        "rates; split by each model's share of this project's".to_owned(),
        "observed input tokens".to_owned(),
        String::new(),
    ];
    let name_w = sd
        .usd_rows
        .iter()
        .map(|r| r.model.chars().count())
        .max()
        .unwrap_or(0);
    let tok_w = sd
        .usd_rows
        .iter()
        .map(|r| fmt_count(r.tokens).chars().count())
        .max()
        .unwrap_or(0);
    let mut unpriced_tokens: u64 = 0;
    for r in &sd.usd_rows {
        let tokens = fmt_count(r.tokens);
        if let (Some(rate), Some(sub)) = (&r.rate_label, &r.subtotal_label) {
            lines.push(format!(
                "{model:<name_w$}  {tokens:>tok_w$} tok x {rate} = {sub}",
                model = r.model,
            ));
        } else {
            unpriced_tokens = unpriced_tokens.saturating_add(r.tokens);
            lines.push(format!(
                "{model:<name_w$}  {tokens:>tok_w$} tok — no seeded price",
                model = r.model,
            ));
        }
    }
    lines.push(format!("total = {total_label}"));
    if unpriced_tokens > 0 {
        lines.push(String::new());
        lines.push(format!(
            "{} tok carry no seeded price and add nothing to",
            fmt_count(unpriced_tokens)
        ));
        lines.push("the total — the figure covers the priced share only".to_owned());
    }
    lines.extend(savings_worktree_lines(&sd.by_worktree));
    lines.push(String::new());
    lines.push("~ marks an estimate, not a bill: token counts are".to_owned());
    lines.push("cl100k_base (not the provider's billing tokenizer),".to_owned());
    lines.push("and the per-model split is attributed from observed".to_owned());
    lines.push("usage — the serve trail records no per-serve model".to_owned());
    lines.push("(the priced savings estimate).".to_owned());
    lines.extend(savings_share_lines(&sd.share));
    lines
}

/// The savings-share block of the breakdown modal: the headline share, the
/// honesty figure and one row per family (share, honesty, n).
///
/// Every number was computed by `rift_recall_ledger::savings` and arrived on
/// the bus already decided; this function lays out strings. An absent share is
/// drawn as `n/a`, never as `0 %` — the two mean opposite things, and the
/// second is a measurement nobody made.
#[must_use]
pub fn savings_share_lines(share: &ShareData) -> Vec<String> {
    // The window the numbers cover, in the card's own voice: the default is the
    // whole record, and a boundary marker replaces that clause rather than
    // being appended to it — two window sentences on one card is how a reader
    // ends up believing the wrong one.
    let window = share
        .window
        .clone()
        .unwrap_or_else(|| "since the projection was reset".to_owned());
    let Some(overall) = share.overall else {
        return vec![
            String::new(),
            "savings share: unanswerable — nothing has been read".to_owned(),
            format!("and nothing avoided {window}"),
        ];
    };
    let mut lines = vec![
        String::new(),
        "savings share — of the tokens this repository's agents".to_owned(),
        "would have read, the share the recall plane avoided,".to_owned(),
        window,
        format!("overall              {overall:.1}%"),
        format!("honesty              {:+} tok avoided net", share.honesty),
        format!(
            "sessions with reads  {} (the n behind the share)",
            share.sessions_with_reads
        ),
    ];
    // The hole in the denominator, named. A read whose token count was never
    // frozen is a row in the denominator's set worth zero tokens, so the share
    // is over fewer measured reads than its `n` suggests. It is counted here
    // rather than imputed into the figure above it.
    if share.reads_unmeasured > 0 {
        lines.push(format!(
            "reads not measured   {} (in the denominator, worth 0)",
            share.reads_unmeasured
        ));
    }
    lines.push(String::new());
    lines.extend(family_share_table(&share.families));
    lines.extend(excluding_lines(share));
    lines.push(String::new());
    lines.extend(SHARE_JOIN_NOTE.iter().map(|line| (*line).to_owned()));
    lines
}

/// The per-family table: its header, then one row per family — share,
/// honesty, n — flagged where the family decided and its reads were never
/// captured. An empty table says so rather than drawing a header over nothing.
fn family_share_table(families: &[FamilyShareRow]) -> Vec<String> {
    let name_w = families
        .iter()
        .map(|f| f.family.chars().count())
        .max()
        .unwrap_or(0)
        .max(6);
    let mut lines = vec![format!(
        "{family:<name_w$}  {share:>7}  {honesty:>9}  {n:>3}",
        family = "family",
        share = "share",
        honesty = "honesty",
        n = "n"
    )];
    if families.is_empty() {
        lines.push("(no family has a decision or a read yet)".to_owned());
    }
    for f in families {
        let pct = f
            .share
            .map_or_else(|| "n/a".to_owned(), |p| format!("{p:.1}%"));
        let flag = if f.decided_without_reads {
            "  decided, no captured reads"
        } else {
            ""
        };
        lines.push(format!(
            "{family:<name_w$}  {pct:>7}  {honesty:>+9}  {n:>3}{flag}",
            family = f.family,
            honesty = f.honesty,
            n = f.sessions_with_reads
        ));
    }
    lines
}

/// The excluding variant, when the ledger computed one: the share with every
/// family that decided and whose reads were never captured left out of both
/// sides, and the names it left out so the exclusion is auditable.
fn excluding_lines(share: &ShareData) -> Vec<String> {
    let Some(excluding) = share.excluding else {
        return Vec::new();
    };
    let excluded = if share.excluded.is_empty() {
        "none".to_owned()
    } else {
        share.excluded.join(", ")
    };
    vec![
        String::new(),
        "excluding families that decided with no captured".to_owned(),
        format!("reads: {excluding:.1}% (excluded: {excluded})"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    use super::super::fixtures::share_fixture;

    /// the priced savings estimate: the modal discloses the whole basis — rows that visibly
    /// sum, the class assumption, unpriced coverage, and the
    /// estimate-of-an-estimate statement. Every money string is pre-rendered
    /// (this function never formats money).
    #[test]
    fn breakdown_lines_disclose_the_whole_basis() {
        let sd = SavingsData {
            usd_saved_micros: Some(2_500_000),
            usd_label: Some("~$2.50".to_owned()),
            usd_rows: vec![
                UsdRow {
                    model: "claude-opus-4-8".to_owned(),
                    tokens: 500_000,
                    price_per_mtok_micros: Some(5_000_000),
                    subtotal_micros: Some(2_500_000),
                    rate_label: Some("$5.00/MTok".to_owned()),
                    subtotal_label: Some("~$2.50".to_owned()),
                },
                UsdRow {
                    model: "mystery-model".to_owned(),
                    tokens: 500_500,
                    price_per_mtok_micros: None,
                    subtotal_micros: None,
                    rate_label: None,
                    subtotal_label: None,
                },
            ],
            usd_read_micros: None,
            usd_read_label: None,
            by_worktree: WorktreeAxis::default(),
            share: Box::new(ShareData::default()),
        };
        let body = usd_breakdown_lines(&sd).join("\n");
        // The net total is the rows' exact sum, thousands-grouped.
        assert!(body.contains("net tokens avoided   1,000,500"), "{body}");
        assert!(
            body.contains("uncached input"),
            "the class is named: {body}"
        );
        // Priced row: slice × catalog rate = tilde'd subtotal.
        assert!(body.contains("500,000 tok x $5.00/MTok = ~$2.50"), "{body}");
        // Unpriced row: slice kept, marked, no dollars anywhere on it.
        assert!(body.contains("mystery-model"), "{body}");
        assert!(body.contains("no seeded price"), "{body}");
        // The composing total and the unpriced-coverage disclosure.
        assert!(body.contains("total = ~$2.50"), "{body}");
        assert!(
            body.contains("500,500 tok carry no seeded price"),
            "coverage is disclosed: {body}"
        );
        // The honesty statement, verbatim anchors.
        assert!(body.contains("estimate, not a bill"), "{body}");
        assert!(body.contains("cl100k_base"), "{body}");
        assert!(body.contains("attributed from observed"), "{body}");
    }

    /// ★ the family view: a serve carries no session key, so it cannot be
    /// attributed to a member — it counts under the family only, and the
    /// modal SAYS so rather than splitting the figure on a guess. (No serve
    /// relation in `recall.db` holds a session id: `memd_audit_event`,
    /// `serve_call_event`, `serve_session_event`.)
    #[test]
    fn serves_with_no_session_key_count_under_family_only() {
        let sd = SavingsData {
            usd_saved_micros: Some(2_500_000),
            usd_label: Some("~$2.50".to_owned()),
            usd_rows: vec![UsdRow {
                model: "claude-opus-4-8".to_owned(),
                tokens: 1_000_000,
                price_per_mtok_micros: Some(5_000_000),
                subtotal_micros: Some(2_500_000),
                rate_label: Some("$5.00/MTok".to_owned()),
                subtotal_label: Some("~$2.50".to_owned()),
            }],
            usd_read_micros: None,
            usd_read_label: None,
            by_worktree: WorktreeAxis {
                rows: Vec::new(),
                family_only: 1_000_000,
            },
            share: Box::new(ShareData::default()),
        };
        let body = usd_breakdown_lines(&sd).join("\n");
        assert!(body.contains("by worktree"), "the axis is present: {body}");
        assert!(
            body.contains("family-only  1,000,000 tok"),
            "the whole avoided total sits under family-only: {body}"
        );
        assert!(
            body.contains("no serve relation records the session that was"),
            "the modal says WHY, it does not just show a row: {body}"
        );
        assert!(
            body.contains("this figure is the family's"),
            "and what the figure therefore means: {body}"
        );
        // Nothing was invented: no member name, and no `unknown` row either
        // (unknown is a session with no observation; this has no session).
        assert!(!body.contains("unknown"), "{body}");
    }

    /// An axis with nothing in it adds nothing to the modal — the savings body
    /// is byte-identical to what it was before the axis existed, so a project with no
    /// savings basis grows no empty table.
    #[test]
    fn an_empty_axis_adds_no_block_to_the_savings_modal() {
        let sd = SavingsData {
            usd_saved_micros: Some(1),
            usd_label: Some("~$0.01".to_owned()),
            usd_rows: vec![UsdRow {
                model: "m".to_owned(),
                tokens: 10,
                price_per_mtok_micros: Some(1),
                subtotal_micros: Some(1),
                rate_label: Some("$0.00/MTok".to_owned()),
                subtotal_label: Some("~$0.01".to_owned()),
            }],
            usd_read_micros: None,
            usd_read_label: None,
            by_worktree: WorktreeAxis::default(),
            share: Box::new(ShareData::default()),
        };
        assert!(!usd_breakdown_lines(&sd).join("\n").contains("by worktree"));
        assert!(WorktreeAxis::default().is_empty());
        assert_eq!(WorktreeAxis::default().total(), 0);
    }

    /// The breakdown modal carries the per-family rows (share, honesty, n) and
    /// names the family whose reads were never captured — the number the
    /// excluding variant exists for is explained where it is read.
    #[test]
    fn the_breakdown_modal_publishes_the_per_family_shares() {
        let sd = share_fixture(Some(10.0));
        let body = usd_breakdown_lines(&sd).join("\n");
        assert!(body.contains("savings share"), "{body}");
        assert!(body.contains("overall              10.0%"), "{body}");
        assert!(body.contains("sessions with reads  3"), "{body}");
        assert!(body.contains("claude-code"), "{body}");
        assert!(
            body.contains("codex") && body.contains("decided, no captured reads"),
            "{body}"
        );
        assert!(
            body.contains("reads: 5.3% (excluded: codex)"),
            "the excluding variant names what it excluded: {body}"
        );
        // Reachable with no dollar figure at all: the headline is the door
        // on every project whose models carry no seeded price.
        assert!(body.contains("no dollar figure"), "{body}");
    }

    /// **The card names the period it is reporting.** A person who moved the
    /// window with `rift savings reset` and then read a card still captioned
    /// "since the projection was reset" would be reading the right number
    /// under the wrong claim — and would have no way to tell.
    #[test]
    fn the_modal_names_the_window_the_share_covers() {
        let mut sd = share_fixture(Some(10.0));
        let body = usd_breakdown_lines(&sd).join("\n");
        assert!(body.contains("since the projection was reset"), "{body}");

        sd.share.window = Some("since the window was reset 2026-09-07T18:00:00.000Z".to_owned());
        let moved = usd_breakdown_lines(&sd).join("\n");
        assert!(
            moved.contains("since the window was reset 2026-09-07T18:00:00.000Z"),
            "{moved}"
        );
        assert!(
            !moved.contains("since the projection was reset"),
            "one window sentence, not two: {moved}"
        );

        // And with nothing to report, the absence is dated too — "unanswerable
        // since the reset" and "unanswerable since Tuesday" are different
        // claims about the same plane.
        sd.share.overall = None;
        let empty = savings_share_lines(&sd.share).join("\n");
        assert!(empty.contains("unanswerable"), "{empty}");
        assert!(
            empty.contains("since the window was reset 2026-09-07T18:00:00.000Z"),
            "{empty}"
        );
    }
}
