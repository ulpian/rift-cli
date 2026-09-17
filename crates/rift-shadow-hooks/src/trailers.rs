//! Commit-message trailers (the durable commit→checkpoint linkage).
//!
//! `prepare-commit-msg` injects `Rift-Checkpoint`/`Rift-Attribution` trailers when
//! a session is active; `commit-msg` normalizes them idempotently. Trailers are
//! plain RFC-822-style `Key: value` lines at the message foot (parseable by
//! `git interpret-trailers`) — never executed, only data. These functions are pure
//! string transforms; the "warn, don't reject" on a malformed message happens at
//! the hook boundary (the bin), which always exits 0 on the commit path.

/// The checkpoint-linkage trailer key.
const KEY_CHECKPOINT: &str = "Rift-Checkpoint";
/// The one-line attribution-summary trailer key.
const KEY_ATTRIBUTION: &str = "Rift-Attribution";

/// A one-line attribution summary rendered into the `Rift-Attribution` trailer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AttributionSummary {
    /// Percent of lines attributed to the agent (0–100).
    pub agent_pct: u8,
    /// Percent of lines attributed to the human (0–100).
    pub human_pct: u8,
    /// Confidence percent (0–100).
    pub confidence_pct: u8,
}

impl AttributionSummary {
    /// The trailer value: `agent=<a>% human=<h>% confidence=<c>`.
    #[must_use]
    pub fn to_value(self) -> String {
        format!(
            "agent={}% human={}% confidence={}",
            self.agent_pct, self.human_pct, self.confidence_pct
        )
    }
}

/// The Rift trailers to inject (each optional — `None` leaves that trailer alone).
#[derive(Clone, Debug, Default)]
pub struct Trailers {
    /// The checkpoint id this commit links to.
    pub checkpoint_id: Option<String>,
    /// The attribution summary.
    pub attribution: Option<AttributionSummary>,
}

impl Trailers {
    /// Whether there is anything to inject (a `None`/`None` set means "session
    /// inactive → leave the message untouched").
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.checkpoint_id.is_none() && self.attribution.is_none()
    }
}

/// Split a message into its body and the Rift trailer values it already carries.
///
/// **Every** `Rift-Checkpoint` is kept, in first-seen order and deduplicated. A
/// commit can legitimately claim several checkpoints — a forge's
/// squash-and-merge fuses one message per constituent commit — and keeping only
/// the last one *deleted* the others from the message the next time it passed
/// through `commit-msg`. `Rift-Attribution` stays single-valued: it is one
/// summary of one commit, so the last write is the current one.
///
/// The match stays anchored at column 0 **on purpose**. `git merge --squash`
/// writes `SQUASH_MSG` as an indented transcript, so a locally squashed commit's
/// constituent trailers arrive with leading whitespace — they are quoted history
/// belonging to the body, not claims by the commit being written, and they must
/// survive verbatim. Trimming before the match would promote quoted text into
/// this commit's own trailer block. Reading indented claims is the *read* side's
/// job (`rift_shadow_attribution::provenance`), which counts them separately.
fn parse(message: &str) -> (String, Vec<String>, Option<String>) {
    let mut body_lines: Vec<&str> = Vec::new();
    let mut checkpoints: Vec<String> = Vec::new();
    let mut attribution: Option<String> = None;
    for line in message.lines() {
        if let Some(v) = line.strip_prefix(KEY_CHECKPOINT).and_then(strip_colon) {
            push_unique(&mut checkpoints, v.trim());
        } else if let Some(v) = line.strip_prefix(KEY_ATTRIBUTION).and_then(strip_colon) {
            attribution = Some(v.trim().to_owned());
        } else {
            body_lines.push(line);
        }
    }
    // Trim trailing blank lines from the body so the trailer block sits cleanly.
    while body_lines.last().is_some_and(|l| l.trim().is_empty()) {
        body_lines.pop();
    }
    (body_lines.join("\n"), checkpoints, attribution)
}

/// Append `value` unless it is empty or already present (order-preserving dedup).
fn push_unique(values: &mut Vec<String>, value: &str) {
    if !value.is_empty() && !values.iter().any(|v| v == value) {
        values.push(value.to_owned());
    }
}

/// Match a `: ` (or `:`) separator immediately after a key.
fn strip_colon(rest: &str) -> Option<&str> {
    rest.strip_prefix(':')
}

/// Render a body plus its Rift trailers into a canonical message: body, a blank
/// separator line, then every `Rift-Checkpoint` in order, then
/// `Rift-Attribution`.
fn render(body: &str, checkpoints: &[String], attribution: Option<&str>) -> String {
    let mut trailers: Vec<String> = Vec::new();
    for c in checkpoints {
        trailers.push(format!("{KEY_CHECKPOINT}: {c}"));
    }
    if let Some(a) = attribution {
        trailers.push(format!("{KEY_ATTRIBUTION}: {a}"));
    }
    if trailers.is_empty() {
        return format!("{body}\n");
    }
    let block = trailers.join("\n");
    if body.is_empty() {
        format!("{block}\n")
    } else {
        format!("{body}\n\n{block}\n")
    }
}

/// Inject Rift trailers when a session is active. An empty [`Trailers`]
/// (inactive session) leaves the message **untouched**.
///
/// The checkpoint id **joins** the claims already on the message rather than
/// replacing them: the message may be a squash carrying its constituents'
/// claims, and this commit's claim is one more true statement about it, not a
/// correction of theirs. Injection is idempotent (the same id twice adds one
/// line), but note the consequence on `git commit --amend` while a session is
/// still running: the predicted id folds in the read facts captured so far, so
/// amending after further agent work leaves both predictions on the message.
/// Both name the same session, and the read side resolves whichever was
/// actually archived — an unrealized prediction resolves to nothing rather than
/// mis-attributing.
///
/// Every trailer value is a **claim**, permanently: a transcript-carrying
/// session's prediction never lands as
/// written, and the reader's trailer/anchor union — not a heavier hook — is
/// the permanent resolution.
#[must_use]
pub fn inject(message: &str, t: &Trailers) -> String {
    if t.is_empty() {
        return message.to_owned();
    }
    let (body, mut checkpoints, existing_attr) = parse(message);
    if let Some(id) = t.checkpoint_id.as_deref() {
        push_unique(&mut checkpoints, id.trim());
    }
    let attribution = t
        .attribution
        .map(AttributionSummary::to_value)
        .or(existing_attr);
    render(&body, &checkpoints, attribution.as_deref())
}

/// Normalize existing Rift trailers idempotently: canonical order, a single
/// trailing block, every distinct checkpoint claim preserved in first-seen
/// order. A message with no Rift trailers round-trips to itself (modulo a
/// trailing newline).
#[must_use]
pub fn normalize(message: &str) -> String {
    let (body, checkpoints, attr) = parse(message);
    render(&body, &checkpoints, attr.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attr() -> AttributionSummary {
        AttributionSummary {
            agent_pct: 70,
            human_pct: 30,
            confidence_pct: 82,
        }
    }

    #[test]
    fn inject_appends_trailers_when_active() {
        let msg = "Fix the parser\n";
        let out = inject(
            msg,
            &Trailers {
                checkpoint_id: Some("ckpt-abc".into()),
                attribution: Some(attr()),
            },
        );
        assert!(out.contains("Rift-Checkpoint: ckpt-abc"));
        assert!(out.contains("Rift-Attribution: agent=70% human=30% confidence=82"));
        // body preserved, trailer block separated by a blank line
        assert!(out.starts_with("Fix the parser\n\nRift-Checkpoint:"));
    }

    #[test]
    fn inject_leaves_message_untouched_when_inactive() {
        let msg = "Just a commit\n";
        assert_eq!(inject(msg, &Trailers::default()), msg);
    }

    #[test]
    fn normalize_is_idempotent_and_keeps_every_checkpoint_claim() {
        // Two claims + an out-of-order attribution — the shape a forge's
        // squash-and-merge lands, which normalize used to strip down to one.
        let msg = "Title\n\nRift-Attribution: agent=1% human=2% confidence=3\n\
                   Rift-Checkpoint: first\nRift-Checkpoint: second\n";
        let once = normalize(msg);
        let twice = normalize(&once);
        assert_eq!(once, twice, "normalize must be idempotent");
        // Canonical order = every Checkpoint, in first-seen order, then Attribution.
        let first = once.find("Rift-Checkpoint: first").unwrap();
        let second = once.find("Rift-Checkpoint: second").unwrap();
        let att = once.find("Rift-Attribution:").unwrap();
        assert!(first < second, "claim order is preserved");
        assert!(second < att, "Checkpoints precede Attribution");
    }

    #[test]
    fn a_repeated_claim_collapses_to_one_line() {
        let msg = "Title\n\nRift-Checkpoint: same\nRift-Checkpoint: same\n";
        assert_eq!(normalize(msg).matches("Rift-Checkpoint: same").count(), 1);
    }

    #[test]
    fn an_indented_trailer_is_body_text_and_survives_verbatim() {
        // `git merge --squash` writes SQUASH_MSG as an INDENTED transcript, so
        // these are quoted history, not this commit's claims. They must not be
        // hoisted into the trailer block — and they must not be lost either.
        let msg = "Squashed commit\n\n    Rift-Checkpoint: quoted-a\n    \
                   Rift-Checkpoint: quoted-b\n";
        let out = normalize(msg);
        assert!(out.contains("    Rift-Checkpoint: quoted-a"));
        assert!(out.contains("    Rift-Checkpoint: quoted-b"));
        assert!(
            !out.contains("\nRift-Checkpoint:"),
            "indented claims are never promoted to trailers: {out}"
        );
        assert_eq!(normalize(&out), out, "still idempotent");
    }

    #[test]
    fn inject_joins_existing_claims_instead_of_deleting_them() {
        // Amending a pulled forge squash: the message already claims two
        // checkpoints and this commit adds its own.
        let msg = "Squash merge\n\nRift-Checkpoint: forge-a\nRift-Checkpoint: forge-b\n";
        let out = inject(
            msg,
            &Trailers {
                checkpoint_id: Some("mine".into()),
                attribution: None,
            },
        );
        for id in ["forge-a", "forge-b", "mine"] {
            assert!(
                out.contains(&format!("Rift-Checkpoint: {id}")),
                "{id}: {out}"
            );
        }
        // Idempotent: re-injecting the same id adds nothing.
        let again = inject(
            &out,
            &Trailers {
                checkpoint_id: Some("mine".into()),
                attribution: None,
            },
        );
        assert_eq!(again, out);
    }

    #[test]
    fn normalize_preserves_body_without_trailers() {
        let msg = "Subject line\n\nA body paragraph.\n";
        assert_eq!(normalize(msg), "Subject line\n\nA body paragraph.\n");
    }

    #[test]
    fn inject_then_normalize_is_stable() {
        let injected = inject(
            "Subject\n",
            &Trailers {
                checkpoint_id: Some("c1".into()),
                attribution: Some(attr()),
            },
        );
        assert_eq!(normalize(&injected), injected);
    }

    #[test]
    fn no_forbidden_brand_in_trailer_keys() {
        let out = inject(
            "x\n",
            &Trailers {
                checkpoint_id: Some("c".into()),
                attribution: None,
            },
        );
        let low = out.to_ascii_lowercase();
        assert!(!low.contains(concat!("ent", "ire")));
    }
}
