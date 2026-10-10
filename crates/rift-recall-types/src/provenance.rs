//! The capture-side half of a delta's provenance, behind one seam.
//!
//! A delta names a path, a hash, the changed sites and the shift. **When** it
//! happened, **who** made it and **what it cost** are recorded in the capture
//! projection and in git, and the recall projection deliberately joins neither:
//! the two databases rebuild independently, so a predicate that reached across
//! them would stop working the moment either one did. So the serving handlers
//! ask this trait instead, and the binary that owns both stores answers it.
//!
//! Every method may answer `None`, and `None` is **unknown** — it is never an
//! invented zero, an epoch, or an empty author. A field nobody can prove is
//! left out of the payload altogether, which is what keeps a provenance object a
//! set of recorded facts rather than a shape with holes in it.
//!
//! [`NoProvenance`] is what a caller with no capture projection passes, and
//! naming the absence is more honest than an `Option` parameter a serving
//! caller could pass `None` to by accident — the same argument
//! [`NoAncestry`](crate::NoAncestry) is built on.

/// One turn's four recorded token classes, as the capture projection holds
/// them. Signed because the store's columns are, and a negative figure is a
/// corrupt row worth surfacing rather than clamping into a plausible one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct TokenClasses {
    /// Input tokens billed at the uncached rate.
    pub uncached_input: i64,
    /// Input tokens served from the provider's cache.
    pub cached_input: i64,
    /// Input tokens written into the provider's cache.
    pub cache_creation: i64,
    /// Output tokens produced.
    pub output: i64,
}

/// What the capture projection records about one checkpoint — the unit a
/// recorded revision belongs to.
///
/// Every field is optional and independently absent: a checkpoint can be dated
/// with no session bound to it, or carry a session whose model calls were never
/// priced. Nothing here is derived from anything else here.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CheckpointFacts {
    /// The capture event's recorded time (epoch ms). `None` for both honest
    /// absences: no row locally, and a row carrying the unknown-time sentinel.
    pub recorded_at_ms: Option<i64>,
    /// The row id of the first session bound to the checkpoint.
    pub session: Option<String>,
    /// That session's agent family (`claude-code`, `codex`, …), filtered to a
    /// label charset by whoever answers — this crate never prints it.
    pub agent: Option<String>,
    /// The four token classes of the model calls this checkpoint froze: its
    /// first session's calls since that session's previous checkpoint, up to
    /// this one.
    pub tokens: Option<TokenClasses>,
    /// Micro-USD of those same calls under the current catalog quote. `None`
    /// when no call in the window is priced — tokens without a priced bill
    /// stay tokens.
    pub cost_micros: Option<i64>,
}

impl CheckpointFacts {
    /// Whether anything at all is known. An all-absent value is not rendered:
    /// an object of five nulls tells a reader nothing and costs it tokens.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.recorded_at_ms.is_none()
            && self.session.is_none()
            && self.agent.is_none()
            && self.tokens.is_none()
            && self.cost_micros.is_none()
    }
}

/// The capture-and-git facts a delta's provenance rides on.
///
/// Both questions are answered from state the serving handler cannot reach:
/// the capture projection, and this checkout's commit history. Both are
/// **read-only** and both may answer *unknown*.
pub trait ProvenanceOracle {
    /// What the capture projection records about `checkpoint_id`.
    fn checkpoint(&self, checkpoint_id: &str) -> Option<CheckpointFacts>;

    /// The subject line of the commit `oid` names, as a git fact — `None` when
    /// the revision is not committed, is outside whatever history the answerer
    /// resolved, or carries a subject it will not render.
    fn commit_subject(&self, oid: &str) -> Option<String>;
}

/// An oracle that knows nothing — every question answers *unknown*.
///
/// What a caller with no capture projection passes: the push tier, the eval
/// harness, and every unit test whose subject is the delta rather than its
/// story. A payload built against it simply carries no provenance object.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NoProvenance;

impl ProvenanceOracle for NoProvenance {
    fn checkpoint(&self, _checkpoint_id: &str) -> Option<CheckpointFacts> {
        None
    }

    fn commit_subject(&self, _oid: &str) -> Option<String> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The empty oracle answers unknown, and unknown is not a zero.
    #[test]
    fn nothing_is_known_and_nothing_is_invented() {
        assert_eq!(NoProvenance.checkpoint("ck-1"), None);
        assert_eq!(NoProvenance.commit_subject("deadbeef"), None);
        assert!(CheckpointFacts::default().is_empty());
    }

    /// One recorded field is enough to be worth rendering; each on its own.
    #[test]
    fn any_single_recorded_fact_makes_the_object_non_empty() {
        let each = [
            CheckpointFacts {
                recorded_at_ms: Some(1),
                ..CheckpointFacts::default()
            },
            CheckpointFacts {
                session: Some("s".to_owned()),
                ..CheckpointFacts::default()
            },
            CheckpointFacts {
                agent: Some("claude-code".to_owned()),
                ..CheckpointFacts::default()
            },
            CheckpointFacts {
                tokens: Some(TokenClasses::default()),
                ..CheckpointFacts::default()
            },
            CheckpointFacts {
                cost_micros: Some(0),
                ..CheckpointFacts::default()
            },
        ];
        for facts in each {
            assert!(!facts.is_empty(), "{facts:?}");
        }
    }
}
