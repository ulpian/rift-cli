//! Deciding which bytes each layer's proposals seal: precedence, the
//! per-byte marks that make a pass linear in the document, the coalescing
//! rule that never publishes a losing span's remainder, the settling rounds
//! and the mask. This is the span arithmetic the pipeline's containment
//! guarantee rests on, kept apart from the detectors so that adding a
//! detector cannot touch how an overlap is resolved.

use crate::builtin_tables::builtin_shape_tables;
use crate::entropy::entropy_spans;
use crate::shapes::shape_spans;
use crate::span::{MatchedAgainst, Span};
use crate::{is_known_placeholder, placeholder_for, DetectorClass, RedactionReport, RulePackSet};

/// Bits per word of a [`ByteMarks`].
const MARK_BITS: usize = u64::BITS as usize;

/// **A per-byte marker over one document**, one bit per byte, packed 64 to a
/// word.
///
/// [`winners`] and [`coalesce`] ask their chosen set exactly two questions —
/// *does this span touch a byte I have already taken?* and *which runs inside it
/// are still free?* — and both used to answer by scanning the whole chosen set,
/// once per span, plus a `subtract` of the same set out of the loser. That is
/// O(|chosen|) per span and therefore O(spans²) per pass; span count is linear
/// in document length, so it was O(bytes²) — measured at exponent **1.94**, and
/// **8.5 s for one pass over 1 MiB** of a dense 3-3-4 phone column, against
/// 0.03 s for the same bytes with nothing in them a detector claims. Asking a
/// bitmap instead answers both questions in time proportional to the *span's
/// own* length, which makes a pass linear in the document.
///
/// **Two markers, not one, and the second one is the whole correctness
/// argument** — a single coverage mark answers *is this byte taken* where
/// [`coalesce`]'s exception needs *what took it*. See `coalesce`.
///
/// One marker costs one bit per byte of document, so the pair costs two: under
/// 1 MB at the largest document the capture path can hand this crate.
///
/// **Precondition, and it is why this can be a bitmap at all:** a bitmap cannot
/// express a zero-width interval, so a zero-width span is not representable
/// here. No layer proposes one — `shape_spans` rejects `start >= end` after its
/// gate, `entropy_spans` needs [`ENTROPY_MIN_LEN`] bytes, and a carved piece is
/// a non-empty free run — and `no_layer_ever_proposes_an_empty_span` is what
/// says so. Both callers skip one rather than pretend, which seals no fewer
/// bytes: an empty span covers none.
pub(crate) struct ByteMarks {
    words: Vec<u64>,
    len: usize,
}

impl ByteMarks {
    pub(crate) fn new(len: usize) -> Self {
        Self {
            words: vec![0; len.div_ceil(MARK_BITS)],
            len,
        }
    }

    /// Bits `[lo, hi)` of one word, where `hi <= MARK_BITS`.
    fn word_mask(lo: usize, hi: usize) -> u64 {
        if lo >= hi || lo >= MARK_BITS {
            return 0;
        }
        let high = if hi >= MARK_BITS {
            u64::MAX
        } else {
            (1u64 << hi) - 1
        };
        (u64::MAX << lo) & high
    }

    /// Mark every byte of `[start, end)`.
    pub(crate) fn mark(&mut self, start: usize, end: usize) {
        let end = end.min(self.len);
        let mut i = start.min(end);
        while i < end {
            let base = (i / MARK_BITS) * MARK_BITS;
            let hi = (end - base).min(MARK_BITS);
            let m = Self::word_mask(i - base, hi);
            if let Some(word) = self.words.get_mut(i / MARK_BITS) {
                *word |= m;
            }
            i = base + hi;
        }
    }

    /// Whether any byte of `[start, end)` is marked. This is the `overlaps`
    /// scan: a marked byte inside the span is a byte some chosen span holds,
    /// and — because the chosen set is pairwise disjoint and non-empty — a
    /// chosen span sharing a byte with this one is exactly a chosen span this
    /// one overlaps.
    pub(crate) fn any(&self, start: usize, end: usize) -> bool {
        let end = end.min(self.len);
        let mut i = start.min(end);
        while i < end {
            let base = (i / MARK_BITS) * MARK_BITS;
            let hi = (end - base).min(MARK_BITS);
            let m = Self::word_mask(i - base, hi);
            if self.words.get(i / MARK_BITS).is_some_and(|w| w & m != 0) {
                return true;
            }
            i = base + hi;
        }
        false
    }

    /// The maximal runs of **unmarked** bytes inside `[start, end)`, in order.
    ///
    /// This is `subtract`: the pieces a losing span keeps are the parts of it no
    /// winner claimed, and those are the connected components of `[start, end)`
    /// minus the marked set — the same list, computed from the marks instead of
    /// from a copy of every chosen range. Every offset it returns is either a
    /// bound of `[start, end)` or a bound of a span that was marked, so all of
    /// them are already `char` boundaries.
    pub(crate) fn free_runs(&self, start: usize, end: usize) -> Vec<(usize, usize)> {
        let end = end.min(self.len);
        let mut runs = Vec::new();
        let mut open: Option<usize> = None;
        let mut i = start.min(end);
        while i < end {
            let base = (i / MARK_BITS) * MARK_BITS;
            let hi = (end - base).min(MARK_BITS);
            let lo = i - base;
            let m = Self::word_mask(lo, hi);
            let marked = self.words.get(i / MARK_BITS).copied().unwrap_or(0) & m;
            if marked == 0 {
                if open.is_none() {
                    open = Some(i);
                }
            } else if marked == m {
                if let Some(s) = open.take() {
                    runs.push((s, i));
                }
            } else {
                for b in lo..hi {
                    let pos = base + b;
                    if marked & (1u64 << b) == 0 {
                        if open.is_none() {
                            open = Some(pos);
                        }
                    } else if let Some(s) = open.take() {
                        runs.push((s, pos));
                    }
                }
            }
            i = base + hi;
        }
        if let Some(s) = open {
            runs.push((s, end));
        }
        runs
    }
}

/// Rounds of winner-settling inside a single detection pass. Each round either
/// puts a new span on the table or stops; the cap is the fail-safe.
pub(crate) const MAX_SETTLE: usize = 4;

/// One detection pass: every layer proposes, the winner set **settles**, then
/// precedence decides who *attributes* each byte and nobody's bytes are dropped.
/// The single place the pipeline's span set is decided, so the invariant
/// property and the masking loop cannot drift apart.
///
/// Settling is what keeps carving a remainder from costing coverage. A detector
/// anchored on a word boundary cannot see a shape that abuts a token —
/// `ghp_…h5wg161.214.203.186` hides the IPv4's leading `\b` behind the vendor
/// token — and the old pipeline recovered it on the *next* mask pass, once the
/// placeholder supplied that boundary. Carving takes that second chance away:
/// the entropy span's remainder is `161`, masking it leaves `.214.203.186`, and
/// three octets are not an address. So each round hides the current winners
/// (losers stay visible, because a hidden shape is made of their bytes) and asks
/// every layer again; anything new joins the same pass's proposals. Measured on
/// the differential corpus this is the whole remainder: the three inputs where
/// carving was weaker than dropping are exactly this shape.
///
/// **Settling moved bytes in both directions, and only one of them was written
/// down.** A round proposes against text it has *changed*, so it can find a
/// structure the input does not contain — a `://user:…@` whose password region
/// only stops holding a space because the space was blanked — and until that
/// span was marked ([`MatchedAgainst`]) it could displace a competitor in
/// [`coalesce`] and *unmask* what the competitor covered — 743 records in a
/// 30 000-record differential against `main`, 2.5 % of them, and 3 047 in
/// 120 000 over four seeds. Both signs are pinned now rather than asserted:
/// `a_settling_round_finds_a_shape_the_first_pass_could_not` is the one this
/// doc always claimed, and `settling_never_takes_a_masked_byte_away` is the one
/// it did not.
pub(crate) fn select_spans(text: &str, packs: &RulePackSet) -> Vec<Span> {
    let mut spans = propose(text, packs);
    for _ in 0..MAX_SETTLE {
        let hidden = winners(spans.clone());
        if hidden.is_empty() {
            break;
        }
        let mut found = propose(&blank(text, &hidden), packs);
        // The round handed every detector a text with holes in it, so nothing it
        // returns is a reading of the input. `coalesce` is where that matters.
        for f in &mut found {
            f.matched = MatchedAgainst::BlankedText;
        }
        found.retain(|f| {
            !spans
                .iter()
                .any(|s| s.start == f.start && s.end == f.end && s.id == f.id)
        });
        if found.is_empty() {
            break;
        }
        spans.extend(found);
    }
    coalesce(spans)
}

/// Every layer's proposal on one text, before any overlap is resolved.
///
/// The emission order is load-bearing and is not a listing order:
/// [`by_precedence`] sorts *stably*, so two spans of equal precedence, equal
/// width and equal start are attributed — and therefore placeheld — in the
/// order they were proposed here.
pub(crate) fn propose(text: &str, packs: &RulePackSet) -> Vec<Span> {
    let [pass1, provider, credential, pii] = builtin_shape_tables();
    let mut spans = shape_spans(text, pass1);
    spans.extend(shape_spans(text, provider));
    spans.extend(shape_spans(text, credential));
    spans.extend(shape_spans(text, &packs.extra));
    spans.extend(entropy_spans(text));
    spans.extend(shape_spans(text, pii));
    spans
}

/// The spans that win their overlaps outright, every loser dropped whole.
///
/// This is **not** the published set — dropping a loser whole is the defect
/// [`coalesce`] exists to close. It is only what to *hide* while the winner set
/// settles ([`select_spans`]), and the losers must stay visible there, because
/// their bytes are exactly the ones a hidden shape is still made of.
///
/// The chosen set is pairwise disjoint by construction — nothing joins it that
/// touches a byte already in it — so "overlaps something chosen" and "covers a
/// marked byte" are the same question, and [`ByteMarks`] answers the second one
/// in the span's own length instead of the chosen set's size. Held to the
/// implementation it replaced by `the_marks_choose_the_winners_the_scan_chose`.
pub(crate) fn winners(mut spans: Vec<Span>) -> Vec<Span> {
    by_precedence(&mut spans);
    let mut taken = ByteMarks::new(extent_of(&spans));
    let mut chosen: Vec<Span> = Vec::new();
    for s in spans {
        if s.start >= s.end || taken.any(s.start, s.end) {
            continue;
        }
        taken.mark(s.start, s.end);
        chosen.push(s);
    }
    chosen
}

/// **Two marks, and the second one is not an optimization.**
///
/// `taken` answers *is this byte already claimed* — that replaces both
/// `chosen.iter().any(overlaps)` scans and the `subtract` between them, since
/// the free runs inside a loser *are* the pieces it keeps. `parsed` answers the
/// question the exception actually asks, which a coverage mark cannot:
/// **did a member of one fixed subset of the winners claim this byte** — the
/// subset being the winners carrying a [`Span::parse`] read out of the input's
/// own bytes. Not *which* winner claimed it. The predicate is an existential
/// over a subset that is fixed before the loop reaches this span, so one bit per
/// byte is exactly enough; it would take more only if the exception depended on
/// some varying property of the covering span, and it does not.
///
/// One mark cannot answer both questions, which is why there are two.
/// Collapsing them makes `displaced_by_a_parse` true at every overlap, so the
/// loser goes whole against a winner with no parse to justify it — the
/// published-remainder class, rejected in review three times. A carved piece
/// never marks
/// `parsed`, because a piece carries no parse.
///
/// **The two mutations are not interchangeable and neither are their
/// witnesses**, re-derived on this tree with the saved regressions moved aside
/// so each search ran fresh:
///
/// * *collapse the two marks into one* shrinks to
///   `https://alice:001 01 0001@h.com` and reddens seven tests, among them
///   `no_losing_span_publishes_its_remainder` and
///   `nothing_survives_here_that_dropping_the_loser_masked`;
/// * *never mark `parsed` at all* shrinks to
///   `https://alice:AAAAAAAA@cache.internal` and reddens five, among them
///   `masks_credentialed_uri_keeps_host`.
///
/// The third saved regression is the second of those (`@h.com` rather than
/// `@cache.internal` — the strategy picks the host), and `9ba4dfa`'s message
/// attributes it to the collapse, which is wrong; both are real calibration and
/// the seed is fine, but they came from different mutations.
/// `a_parse_that_only_a_blanked_copy_can_see_displaces_nothing` and
/// `the_marks_coalesce_the_way_the_scan_did` are what hold it.
pub(crate) fn coalesce(mut spans: Vec<Span>) -> Vec<Span> {
    by_precedence(&mut spans);
    let extent = extent_of(&spans);
    let mut taken = ByteMarks::new(extent);
    let mut parsed = ByteMarks::new(extent);
    let mut chosen: Vec<Span> = Vec::new();
    for s in spans {
        if s.start >= s.end {
            continue;
        }
        if !taken.any(s.start, s.end) {
            taken.mark(s.start, s.end);
            if s.parse.is_some() && s.matched == MatchedAgainst::Input {
                parsed.mark(s.start, s.end);
            }
            chosen.push(s);
            continue;
        }
        let displaced_by_a_parse = parsed.any(s.start, s.end);
        if displaced_by_a_parse && s.class != DetectorClass::Entropy {
            continue;
        }
        for (start, end) in taken.free_runs(s.start, s.end) {
            chosen.push(Span {
                start,
                end,
                class: s.class,
                id: s.id.clone(),
                parse: None,
                matched: s.matched,
            });
        }
        // The pieces cover exactly the free runs, so the loser's whole range is
        // claimed now: one mark instead of one per piece.
        taken.mark(s.start, s.end);
    }
    chosen.sort_by_key(|s| s.start);
    chosen
}

/// Resolve overlaps by precedence **without ever publishing a losing span's
/// bytes**: the loser keeps whatever of itself no winner claimed, under its own
/// class, instead of being dropped whole.
///
/// Order is precedence, then width, then position, and that order is only about
/// *attribution* — the highest-precedence span claims its bytes first, so a
/// credentialed URI's password is attributed to the URI detector and a vendor
/// token to its vendor rather than to entropy. What losing costs a span is the
/// bytes it shares with a winner, never the bytes it does not.
///
/// **The chosen set's union is therefore the union of every span every layer
/// proposed, but for the one exception below**, which is the pipeline's
/// containment guarantee stated as arithmetic rather than as a special case.
/// The unqualified form of that sentence — "adding a detector can only ever
/// seal *more*" — is the premise the published-remainder reject falsified, and
/// it is false in exactly one
/// place, so the exception carries the qualification and the measurement rather
/// than this paragraph carrying an absolute. Dropping the loser whole was the
/// opposite — a new span that won
/// an overlap took the loser's remainder out with it, and the detect→mask
/// fixpoint could not recover a remainder that fell under the entropy layer's
/// [`ENTROPY_MIN_LEN`]/[`ENTROPY_MIN_BITS`] floor or was a bare domain no layer
/// claims:
///
/// * a numeric-PII span inside a longer high-entropy token published both ends
///   (`Zx9Qw3eR7tY2uI8oPa-378282246310005` →
///   `Zx9Qw3eR7tY2uI8oPa-[REDACTED:pii]`, 18 characters of the token published);
/// * `tax_id 325531976+eSorngoNobNA7WT07TOWk`, whole `[REDACTED:entropy]` before
///   `pii.us_taxpayer_id_labelled` existed, published 22 characters of the token
///   once it did;
/// * a pack rule `alice@ex` turned `alice@example.com` into
///   `[REDACTED:vendor]ample.com`.
///
/// Every `[REDACTED:pii]` rendered in this doc — here and in the drop-the-loser
/// template below — is the output **as it was measured**, before the PII
/// family gained a label per detector. A re-run reads
/// `[REDACTED:pii.card_pan]` and `[REDACTED:pii.phone_nanp]` in those places.
/// Only the label moved: which bytes each variant publishes, which is what every
/// one of these examples is about, is untouched, and the counts beside them were
/// measured on the pipeline of the day and are not re-derived here.
///
/// **The one exception:** a losing **shape** span overlapping a winning
/// capture-group detector's span is dropped whole rather than carved. That
/// detector parsed the structure those bytes sit in ([`Span::parse`] is the rest
/// of its match), so a shape span crossing its group is the same bytes read a
/// second way — the `pii.email` detector reads `s3cr3tpw@db.example.com` out of
/// `postgres://admin:s3cr3tpw@db.example.com`, and honouring its remainder would
/// destroy the host the URI detector exists to keep (`uri.host_preserved`).
///
/// **The exception is restricted to a parse of the input's own bytes
/// ([`MatchedAgainst::Input`]), and that restriction is the whole of it.** The
/// premise it was first written on — that dropping a loser here can never
/// publish a byte the drop-the-loser pipeline did not already publish, since
/// that pipeline dropped *every* loser whole — is false whenever the winning
/// parse is one only this pipeline can see. Measured against `main` over
/// 120 000 records (four seeds), the unrestricted exception published a byte
/// `main` masks in **3 047** of them, and the class is one template:
/// `http://user:123 45 6789@h.co/path` is `http://user:123 45 [REDACTED:pii]/path`
/// under drop-the-loser, because `pii.email` claims `6789@h.co` whole. Here the
/// numeric-PII span over `123 45 6789` won its round, settling blanked it, and
/// `uri.credentials` — whose `[^/\s@]+` cannot cross a space in the input —
/// matched the blanked copy. That parse displaced the email span, and `@h.co`
/// shipped. A parse read out of blanked text is a claim about bytes the input
/// does not carry, so it no longer displaces anything; the intended case is
/// untouched, because `postgres://admin:s3cr3tpw@db.example.com` parses in the
/// input as written. Pinned by
/// `a_parse_that_only_a_blanked_copy_can_see_displaces_nothing`.
///
/// The entropy layer is never dropped this way: it is not a competing parse but
/// the guess of last resort, and its remainder is what the reject was about. So
/// a key name the entropy layer swallowed into its token goes with it —
/// `password=Zx9…` seals whole rather than keeping `password=` — which is
/// over-redaction in the direction the legacy backend's own connection-string regions
/// already take (`entropy.overlapped_group_detector_context`).
pub(crate) fn by_precedence(spans: &mut [Span]) {
    spans.sort_by(|a, b| {
        precedence(b.class)
            .cmp(&precedence(a.class))
            .then((b.end - b.start).cmp(&(a.end - a.start)))
            .then(a.start.cmp(&b.start))
    });
}

/// Detector precedence: a precise shape detector beats PII, which beats the
/// generic entropy detector. When spans overlap, the higher-precedence span
/// wins and the overlapping lower-precedence span is dropped (not merged-and-
/// extended) — so a credentialed-URI password is masked without swallowing the
/// host, and a vendor token is attributed to its vendor, not to entropy.
pub(crate) fn precedence(class: DetectorClass) -> u8 {
    match class {
        DetectorClass::PrivateKey => 5,
        DetectorClass::CredentialedUri | DetectorClass::JdbcDsn | DetectorClass::VendorSecret => 4,
        DetectorClass::Pii => 2,
        DetectorClass::Entropy => 1,
    }
}

/// `text` with every byte of `hidden` replaced by NUL, byte offsets preserved.
///
/// NUL belongs to no detector's character class and is not a word character, so
/// a blanked span reads to every regex the way a masked one does: it hides its
/// bytes *and* it supplies the `\b` a neighbouring shape needs. Span bounds are
/// `char` boundaries and NUL is one byte, so the result is still valid UTF-8 at
/// the same offsets.
pub(crate) fn blank(text: &str, hidden: &[Span]) -> String {
    let mut out = text.as_bytes().to_vec();
    for s in hidden {
        if let Some(slot) = out.get_mut(s.start..s.end) {
            slot.fill(0);
        }
    }
    String::from_utf8(out).unwrap_or_else(|_| text.to_owned())
}

pub(crate) fn mask(text: &str, spans: &[Span], report: &mut RedactionReport) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for s in spans {
        out.push_str(&text[last..s.start]);
        let covered = &text[s.start..s.end];
        // Re-masking an already-placed placeholder (e.g. `password=[REDACTED:jdbc]`
        // matches the DSN detector again on a later pass) is a no-op, not a new
        // redaction — keep it out of the counts. Membership in the WHOLE emitted
        // set, not an equality and not the span's own family: any placeholder
        // the pipeline writes suppresses both the count and the rewrite, so
        // neither a sibling PII detector nor a cross-family one (the DSN
        // detector over `password=[REDACTED:pii.…]`) can relabel prior output
        // (see `is_known_placeholder`). Echoing `covered` rather than the
        // placeholder is what makes the *rewrite* half true; for the equal
        // case the two are the same bytes.
        if is_known_placeholder(covered) {
            out.push_str(covered);
        } else {
            report.total_spans = report.total_spans.saturating_add(1);
            report.bytes_masked += (s.end - s.start) as u64;
            *report.by_detector.entry(s.id.clone()).or_insert(0) += 1;
            *report.by_class.entry(s.class).or_insert(0) += 1;
            out.push_str(placeholder_for(s.class, &s.id));
        }
        last = s.end;
    }
    out.push_str(&text[last..]);
    out
}

/// One past the last byte any of `spans` reaches — the document length as far as
/// the marks are concerned.
fn extent_of(spans: &[Span]) -> usize {
    spans.iter().map(|s| s.end).max().unwrap_or(0)
}
