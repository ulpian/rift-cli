//! What a detector is and what it proposes: the [`ShapeDetector`] a table
//! row compiles to, the [`SpanGate`] that disposes of its match, and the
//! [`Span`] every layer hands to the marks. A module of its own because the
//! shape layer, the entropy layer and the marks all speak in these types and
//! none of them owns the others.

use std::borrow::Cow;

use regex::Regex;

use crate::DetectorClass;

/// A post-match gate. The detector's match becomes a redaction span only when
/// this returns a range, given the whole text and the match's byte range.
///
/// The regex carries the *shape*; the gate carries what a linear-time regex
/// cannot express — a checksum (Luhn), an issuing authority's allocation rules
/// (SSA/IRS, NANP), and the surrounding context that decides whether the match
/// is a whole number or a fragment of a longer one. A gate can only ever
/// *remove* or *narrow* a candidate span; the range it returns is always inside
/// the range it was given (fail-safe by construction, pinned by
/// `a_gate_never_widens_the_span_it_was_given`). Narrowing exists because a
/// greedy regex cannot tell where one number ends and the next begins: the
/// alternative to trimming an over-long candidate is publishing it whole.
///
/// That is a statement about the **detector**. The matching statement about the
/// **pipeline** — that adding a span never makes it seal less — is [`coalesce`]'s,
/// not this type's: a losing span keeps its non-overlapping remainder, so the
/// sealed set is the union of every proposal. The invariant that checks it is
/// `every_byte_the_entropy_layer_proposes_is_masked`.
pub(crate) type SpanGate = fn(text: &str, start: usize, end: usize) -> Option<(usize, usize)>;

pub(crate) struct ShapeDetector {
    /// The detector's id, verbatim in `report.by_detector`.
    ///
    /// `Cow` and not `&'static str`, and that is the whole reason a rule pack
    /// read from a file can be a detector at all: a built-in borrows a literal,
    /// a pack rule owns the `<pack>.<rule>` string it was namespaced into.
    pub(crate) id: Cow<'static, str>,
    pub(crate) class: DetectorClass,
    pub(crate) re: Regex,
    /// Capture group to mask (`0` = the whole match).
    pub(crate) group: usize,
    /// Optional validity gate; `None` means every match is a span.
    pub(crate) gate: Option<SpanGate>,
}

/// Which text a span's match was read out of.
///
/// Settling ([`select_spans`]) re-proposes against a copy of the input whose
/// current winners are NUL-blanked ([`blank`]), so a span found in that round
/// matched *that* text and not the input. Its own bytes may well be untouched
/// — `pii.ipv4` finding `161.214.203.186` once the token in front of it is
/// blanked is the case settling exists for — but the surrounding structure it
/// read is not the structure the input carries. The distinction is spent in
/// exactly one place, [`coalesce`]'s parse exception, because a *parse* is a
/// claim about the bytes around the ones it seals.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MatchedAgainst {
    /// The input as written.
    Input,
    /// A settling round's blanked copy of it.
    BlankedText,
}

#[derive(Clone, Debug)]
pub(crate) struct Span {
    pub(crate) start: usize,
    pub(crate) end: usize,
    pub(crate) class: DetectorClass,
    pub(crate) id: String,
    /// For a detector that masks a capture **group**, the range of its *whole*
    /// match — the bytes it read and deliberately kept around the ones it seals
    /// (a URI's `://`…`@`, a DSN's `password=`, a labelled id's key). `None`
    /// when the detector masks its whole match, which keeps nothing.
    ///
    /// This is the one thing that survives an overlap besides the winner, and
    /// [`coalesce`] is where it is spent.
    pub(crate) parse: Option<(usize, usize)>,
    /// Whether the match was read out of the input or out of a settling round's
    /// blanked copy of it. Only a `parse` read out of the input may displace a
    /// competitor — see [`coalesce`].
    pub(crate) matched: MatchedAgainst,
}
