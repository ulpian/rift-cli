//! P3 S1 — commit-message trailers (the durable commit→checkpoint linkage).
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

/// Split a message into its body and any existing Rift trailer values (last value
/// of each key wins — dedup). Non-Rift lines are preserved verbatim in the body.
fn parse(message: &str) -> (String, Option<String>, Option<String>) {
    let mut body_lines: Vec<&str> = Vec::new();
    let mut checkpoint: Option<String> = None;
    let mut attribution: Option<String> = None;
    for line in message.lines() {
        if let Some(v) = line.strip_prefix(KEY_CHECKPOINT).and_then(strip_colon) {
            checkpoint = Some(v.trim().to_owned());
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
    (body_lines.join("\n"), checkpoint, attribution)
}

/// Match a `: ` (or `:`) separator immediately after a key.
fn strip_colon(rest: &str) -> Option<&str> {
    rest.strip_prefix(':')
}

/// Render a body plus optional Rift trailers into a canonical message: body, a
/// blank separator line, then `Rift-Checkpoint` before `Rift-Attribution`.
fn render(body: &str, checkpoint: Option<&str>, attribution: Option<&str>) -> String {
    let mut trailers: Vec<String> = Vec::new();
    if let Some(c) = checkpoint {
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

/// Inject Rift trailers when a session is active. Provided values override any
/// existing trailer of the same key; an empty [`Trailers`] (inactive session)
/// leaves the message **untouched**.
#[must_use]
pub fn inject(message: &str, t: &Trailers) -> String {
    if t.is_empty() {
        return message.to_owned();
    }
    let (body, existing_ckpt, existing_attr) = parse(message);
    let checkpoint = t.checkpoint_id.clone().or(existing_ckpt);
    let attribution = t
        .attribution
        .map(AttributionSummary::to_value)
        .or(existing_attr);
    render(&body, checkpoint.as_deref(), attribution.as_deref())
}

/// Normalize existing Rift trailers idempotently: dedup (keep last), canonical
/// order, single trailing block. A message with no Rift trailers round-trips to
/// itself (modulo a trailing newline).
#[must_use]
pub fn normalize(message: &str) -> String {
    let (body, ckpt, attr) = parse(message);
    render(&body, ckpt.as_deref(), attr.as_deref())
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
    fn normalize_is_idempotent_and_dedups() {
        // duplicate + out-of-order trailers
        let msg = "Title\n\nRift-Attribution: agent=1% human=2% confidence=3\n\
                   Rift-Checkpoint: old\nRift-Checkpoint: new\n";
        let once = normalize(msg);
        let twice = normalize(&once);
        assert_eq!(once, twice, "normalize must be idempotent");
        // last value of each key wins; canonical order = Checkpoint then Attribution
        let ckpt = once.find("Rift-Checkpoint: new").unwrap();
        let att = once.find("Rift-Attribution:").unwrap();
        assert!(ckpt < att, "Checkpoint precedes Attribution");
        assert!(
            !once.contains("Rift-Checkpoint: old"),
            "stale value dropped"
        );
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
        assert!(!low.contains("entire"));
    }
}
