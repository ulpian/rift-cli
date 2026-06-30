//! P3 S1 — the `pre-push` egress filter.
//!
//! The backstop for P2 S1's "shadow refs are never pushed" rule: reject **only**
//! `rift/*` refs, and pass every developer-intended ref through untouched. This is
//! the one place a Rift git hook may exit non-zero — and only to keep a `rift/*`
//! ref from leaving the machine.

/// One ref update line from `pre-push` stdin: `<local> <local-sha> <remote> <remote-sha>`.
/// Only the destination (remote) ref matters for containment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PushRef {
    /// The local ref being pushed (informational).
    pub src: String,
    /// The destination (remote) ref — what would leave the machine.
    pub dst: String,
}

impl PushRef {
    /// Parse one `pre-push` stdin line into a [`PushRef`]. Returns `None` for a
    /// blank or malformed line (the caller skips it; a malformed line never makes
    /// a push leak).
    #[must_use]
    pub fn parse_line(line: &str) -> Option<Self> {
        let mut parts = line.split_whitespace();
        let src = parts.next()?.to_owned();
        let _local_sha = parts.next()?;
        let dst = parts.next()?.to_owned();
        Some(Self { src, dst })
    }
}

/// The screening outcome.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PushDecision {
    /// No `rift/*` ref present — let the push proceed.
    Allow,
    /// At least one `rift/*` ref present — reject the whole push (fails closed).
    RejectRiftRefs,
}

/// Whether a destination ref is a Rift-internal ref that must never leave.
fn is_rift_ref(dst: &str) -> bool {
    dst.starts_with("refs/heads/rift/") || dst.starts_with("rift/")
}

/// Screen a set of push refs: reject **iff** any ref is under `rift/*`; otherwise
/// allow. Pure and total.
#[must_use]
pub fn screen_push(refs: &[PushRef]) -> PushDecision {
    if refs.iter().any(|r| is_rift_ref(&r.dst)) {
        PushDecision::RejectRiftRefs
    } else {
        PushDecision::Allow
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(dst: &str) -> PushRef {
        PushRef {
            src: "refs/heads/feature".into(),
            dst: dst.into(),
        }
    }

    #[test]
    fn allows_normal_refs() {
        assert_eq!(
            screen_push(&[r("refs/heads/main"), r("refs/tags/v1")]),
            PushDecision::Allow
        );
    }

    #[test]
    fn rejects_rift_refs_in_either_form() {
        assert_eq!(
            screen_push(&[r("refs/heads/rift/checkpoints")]),
            PushDecision::RejectRiftRefs
        );
        assert_eq!(
            screen_push(&[r("rift/abc-123")]),
            PushDecision::RejectRiftRefs
        );
    }

    #[test]
    fn mixed_push_fails_closed_on_rift_presence() {
        // a normal ref alongside a rift/* ref rejects the WHOLE push
        assert_eq!(
            screen_push(&[r("refs/heads/main"), r("refs/heads/rift/x")]),
            PushDecision::RejectRiftRefs
        );
    }

    #[test]
    fn empty_push_is_allowed() {
        assert_eq!(screen_push(&[]), PushDecision::Allow);
    }

    #[test]
    fn parses_pre_push_line() {
        let pr = PushRef::parse_line("refs/heads/main abc123 refs/heads/main def456").unwrap();
        assert_eq!(pr.dst, "refs/heads/main");
        assert!(PushRef::parse_line("").is_none());
        assert!(
            PushRef::parse_line("one two").is_none(),
            "needs >= 3 fields"
        );
    }

    proptest::proptest! {
        /// `screen_push` allows IFF no ref is under `rift/*` (egress containment).
        #[test]
        fn allows_iff_no_rift_ref(dsts in proptest::collection::vec("[a-z/]{0,20}", 0..6)) {
            let refs: Vec<PushRef> = dsts.iter().map(|d| r(d)).collect();
            let any_rift = dsts.iter().any(|d| d.starts_with("refs/heads/rift/") || d.starts_with("rift/"));
            let decision = screen_push(&refs);
            if any_rift {
                proptest::prop_assert_eq!(decision, PushDecision::RejectRiftRefs);
            } else {
                proptest::prop_assert_eq!(decision, PushDecision::Allow);
            }
        }
    }
}
