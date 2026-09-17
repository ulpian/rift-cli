//! `rift blame <file>` — which checkpoint, session and agent explain each line.
//!
//! The chain is: `git blame --line-porcelain` gives a commit per line; the
//! commit's message gives every checkpoint it *claims*
//! (`rift_shadow_attribution::provenance::parse_checkpoint_claims`); the
//! projection resolves each claim into a candidate; and `select_for_file`
//! chooses the one that explains this file, carrying the basis for the choice
//! so a guess is never printed as evidence.
//!
//! # Two things this module is careful about
//!
//! **Untrusted input.** A commit message is free text written by anyone who has
//! ever pushed to the repository, `git blame`'s porcelain hands us the author
//! name, mail and subject alongside the oid, and **a filename is
//! attacker-controlled in any repository you clone** — a path is an argument
//! here, but it is an argument the operator copied from a directory listing.
//! What reaches a terminal is exactly: line numbers, oids, checkpoint ids (all
//! validated lowercase hex by `parse_checkpoint_claims`), counts, agent/model
//! labels — which are [`Label`]s, a type with no constructor that does not
//! sanitize — and the requested path through [`safe_path`]. Author, mail and
//! subject are dropped at the *parse boundary* rather than filtered
//! downstream, and `ClaimSet` is structurally incapable of carrying a rejected
//! value — it counts them.
//!
//! The property behind that list, stated as narrowly as it is true: **no byte
//! an attacker chose reaches an operator as anything but text inside a line
//! this module wrote.** A path holding a newline and a plausible blame row
//! would otherwise print exactly such a row, asserting authorship that only
//! recorded evidence is allowed to assert. `safe_path` and `Label` are the two
//! places that is enforced, and both are pinned by tests that fail on the
//! rendered output rather than on the sanitizer alone.
//!
//! The blanket version of that sentence — "in a form indistinguishable from a
//! fact this module recorded" — is stronger than anything here can deliver, and
//! saying so is the point, because [`safe_path`] already admitted it one level
//! down and the two must not disagree. **A terminal narrower than the header
//! wraps it, and a wrapped continuation is a line this module did not write.**
//! With [`MAX_PATH`] at 160 against the 12-character `rift blame "` prefix, an
//! 80-column terminal leaves 68 attacker-chosen characters on the first line
//! and a full 80 on the second — room for a plausible row. No constant closes
//! that: a bound that defeated wrapping would have to be smaller than a width
//! this program cannot know, and the largest that never wraps at 80 columns
//! (66) would truncate ordinary paths in this workspace, whose deepest is 100
//! characters.
//!
//! `--json` is a different surface with a different guarantee. It carries the
//! caller's path and the repository's spelling **byte-exact**, so what it
//! promises is that an attacker's bytes cannot change the document's shape —
//! not that they are absent from it. What survives raw is named and asserted in
//! `the_json_document_cannot_be_given_a_shape_the_renderer_did_not_build`.
//!
//! **Work per line vs work per commit.** A file has thousands of lines and a
//! handful of commits. Every message parse and every projection read is
//! memoised by commit oid and checkpoint id, and adjacent lines with the same
//! answer collapse into one span before anything is rendered — so the cost is
//! O(distinct commits), not O(lines). Doing it per line is the specific
//! weakness this verb exists to beat.

use std::collections::{BTreeMap, HashMap};

use rift_core::{Result, RiftError};
use rift_recall_types::{CheckpointFacts as ServedFacts, ProvenanceOracle, TokenClasses};
use rift_shadow::attribution::provenance::{
    normalize_path, parse_checkpoint_claims, select_for_file, CheckpointCandidate, ClaimSet,
    Selection,
};
use rift_store::{CanonicalStore, SqliteStore, Value};

use crate::cli::{exit, Outcome};

/// Envelope version for `rift blame --json`. Bump on any breaking shape change.
const SCHEMA_VERSION: u32 = 1;

/// An inclusive 1-based line range, as `--line N` or `--line N-M` spells it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ::serde::Serialize)]
pub(crate) struct LineRange {
    /// First line, 1-based.
    pub(crate) start: usize,
    /// Last line, 1-based and inclusive.
    pub(crate) end: usize,
}

/// Parse `N` or `N-M` (both 1-based, inclusive; the grammar the legacy backend's `--line`
/// used, `^\d+(-\d+)?$`, kept so operators migrating a script keep their
/// spelling). Anything else — a zero, a reversed range, a stray sign or space —
/// is a validity error, not a guess.
pub(crate) fn parse_line_range(spec: &str) -> Result<LineRange> {
    let bad = || RiftError::validity("--line takes N or N-M (1-based, inclusive)");
    let (a, b) = match spec.split_once('-') {
        Some((a, b)) => (a, Some(b)),
        None => (spec, None),
    };
    // `usize::from_str` accepts `+7`; digits-only is the published grammar.
    if a.is_empty()
        || !a.bytes().all(|c| c.is_ascii_digit())
        || b.is_some_and(|b| b.is_empty() || !b.bytes().all(|c| c.is_ascii_digit()))
    {
        return Err(bad());
    }
    let start: usize = a.parse().map_err(|_| bad())?;
    let end: usize = b.map_or(Ok(start), str::parse).map_err(|_| bad())?;
    if start == 0 || end < start {
        return Err(bad());
    }
    Ok(LineRange { start, end })
}

/// Whether an oid is the all-zero one `git blame` reports for a line that is
/// not committed yet.
///
/// A predicate rather than a comparison against a constant because the null oid
/// is 40 zeros in a sha1 repository and 64 in a sha256 one, and this verb
/// answers in both.
fn is_uncommitted(oid: &str) -> bool {
    !oid.is_empty() && oid.bytes().all(|b| b == b'0')
}

/// Longest agent/model label rendered. Long enough for real model ids, short
/// enough that a hostile one cannot reflow a terminal.
const MAX_LABEL: usize = 48;

/// Longest *rendered* path in the human header, in characters after escaping.
///
/// Comfortably above the paths this workspace actually holds — the deepest
/// tracked one is **100 characters**
/// (`crates/rift-recall-eval/src/snapshots/rift_recall_eval__record__tests__seeded_regression_report.snap`,
/// measured, not estimated; git's own ceiling is 4096 bytes) — so the bound only
/// ever bites a pathological name. It is a scrollback bound, **not** the forgery
/// defence: escaping is (see [`safe_path`]), and what a bound cannot defend
/// against is quantified in the module doc.
const MAX_PATH: usize = 160;

/// One line as `git blame --line-porcelain` reported it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BlamedLine {
    /// The commit that last touched the line.
    pub(crate) commit: String,
    /// The path the line had **in that commit** — not the path asked for, so a
    /// rename resolves against the name the checkpoint actually recorded.
    pub(crate) path: String,
}

/// A run of consecutive lines with the same commit and path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Span {
    /// First line of the run, 1-based.
    pub(crate) start: usize,
    /// Last line of the run, 1-based and inclusive.
    pub(crate) end: usize,
    /// The commit the run belongs to.
    pub(crate) commit: String,
    /// The path that commit knew the file by.
    pub(crate) path: String,
}

/// Per-file authorship counts recorded for one checkpoint.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ::serde::Serialize)]
pub(crate) struct AttrCounts {
    /// Lines the agent added.
    pub(crate) agent_added: i64,
    /// Lines the human added.
    pub(crate) human_added: i64,
    /// Agent-authored lines a human later edited.
    pub(crate) human_modified_agent: i64,
    /// Calibrated confidence in milli-units (0..=1000).
    pub(crate) confidence_milli: i64,
}

/// What a span's authorship can honestly be called.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ::serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Tag {
    /// Only agent-added lines were recorded for this file at this checkpoint.
    Ai,
    /// Only human-added lines were recorded.
    Human,
    /// Both were recorded, or a human edited agent lines. The counts are
    /// **per file**, so this cut cannot say which of the two wrote a *single*
    /// line — saying `ai` here would be a claim the data does not support.
    Mixed,
    /// No attribution row, or no checkpoint resolved at all.
    Unknown,
}

impl Tag {
    /// The tag implied by a checkpoint's per-file counts.
    fn of(counts: Option<&AttrCounts>) -> Self {
        let Some(c) = counts else {
            return Self::Unknown;
        };
        match (
            c.agent_added > 0,
            c.human_added > 0 || c.human_modified_agent > 0,
        ) {
            (true, true) => Self::Mixed,
            (true, false) => Self::Ai,
            (false, true) => Self::Human,
            (false, false) => Self::Unknown,
        }
    }

    /// The rendered word.
    fn as_str(self) -> &'static str {
        match self {
            Self::Ai => "ai",
            Self::Human => "human",
            Self::Mixed => "mixed",
            Self::Unknown => "unknown",
        }
    }
}

/// One session that contributed to a checkpoint.
#[derive(Clone, Debug, PartialEq, Eq, ::serde::Serialize)]
pub(crate) struct SessionRef {
    /// The session id. Unlike the two below it is **not** rendered to a
    /// terminal: only `--json` carries it, and there nothing it contains can
    /// change the document's shape (see
    /// `the_json_document_cannot_be_given_a_shape_the_renderer_did_not_build`,
    /// which also names what does survive raw — this field is not claimed to be
    /// control-free). So it is reported exactly as recorded rather than folded
    /// into the label charset, which would mangle the ids of adapters that do
    /// not use ULIDs.
    pub(crate) session_id: String,
    /// The agent that ran it.
    pub(crate) agent: Label,
    /// Its headline model.
    pub(crate) model: Label,
}

/// Everything the projection knows about one claimed checkpoint. Memoised by
/// id: a squash claims a handful, a file has thousands of lines.
#[derive(Clone, Debug, Default)]
pub(crate) struct CheckpointFacts {
    /// The capture event's recorded time from `checkpoint.created_at`.
    /// `None` for BOTH honest absences — the id is claimed but absent
    /// locally, and the row is present but carries the unknown-time sentinel
    /// (`-1`, every native checkpoint archived before the writer fix).
    /// Folding the sentinel in here is what keeps `Some(-1)` from ranking as
    /// "resolved metadata" in the squash tie-break and from rendering as if a
    /// real instant in the `--json` documents. Row presence is not lost:
    /// a sentinel-dated row still loads its file evidence and sessions below,
    /// so the two absences differ by their `files_touched`/`sessions`, which
    /// is the half selection actually ranks on first.
    pub(crate) created_at_ms: Option<i64>,
    /// Every path this checkpoint recorded touching.
    pub(crate) files_touched: Vec<String>,
    /// Its sessions, in `session_in_checkpoint.ordinal` order.
    pub(crate) sessions: Vec<SessionRef>,
    /// The checkpoint's strategy token as the projection recorded it
    /// (`manual-commit` | `on-pr` | `scheduled` | `policy-driven`, or the
    /// `import` marker `rift import` stamps on a read-only imported
    /// transcript). `None` when the row is absent locally.
    /// Imports landed before 2026-08-29 carry `manual-commit` forever: the
    /// table is append-only, so the documents say what the row says.
    pub(crate) strategy: Option<String>,
    /// The member the shard was **archived in**, from `checkpoint_worktree`
    /// `None` is UNKNOWN and stays unknown: every shard written before worktree
    /// provenance existed carries no `worktree` member and so projects no row,
    /// back-filling one is forbidden, and defaulting to `main` would turn
    /// "we do not know" into a claim about where the archive ran. Rendered
    /// through [`worktree_label`], never read raw by a surface.
    pub(crate) worktree: Option<Label>,
}

/// A span with its answer, ready to render.
#[derive(Clone, Debug)]
pub(crate) struct Resolved {
    /// The span this answers.
    pub(crate) span: Span,
    /// The chosen checkpoint and why, `None` when the commit claims nothing.
    pub(crate) selection: Option<Selection>,
    /// Per-file counts for the chosen checkpoint, when it has any.
    pub(crate) counts: Option<AttrCounts>,
    /// The chosen checkpoint's sessions.
    pub(crate) sessions: Vec<SessionRef>,
}

/// A string that has been through the label filter — the only way to have one.
///
/// Agent and model strings come from adapters and from a shard's metadata; they
/// are data, not code, and this verb is one of the few places they are printed
/// beside untrusted git output. Sanitizing them *at the call site* worked but
/// bound nothing: a second construction site would have bypassed the filter in
/// silence and no test would have failed. The guarantee lives in the type
/// instead — [`Label::of`] is the sole constructor and the field is private, so
/// a `SessionRef` carrying an unfiltered agent is unspellable.
#[derive(Clone, Debug, PartialEq, Eq, ::serde::Serialize)]
#[serde(transparent)]
pub(crate) struct Label(String);

impl Label {
    /// Keep only characters that can safely reach a terminal, bounded in
    /// length: `[A-Za-z0-9._/+-]` survives, anything else becomes `.`, and a
    /// label longer than [`MAX_LABEL`] is cut with a `…` so it cannot reflow a
    /// terminal.
    pub(crate) fn of(s: &str) -> Self {
        let mut out: String = s
            .chars()
            .take(MAX_LABEL)
            .map(|c| {
                if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/' | '+') {
                    c
                } else {
                    '.'
                }
            })
            .collect();
        if s.chars().count() > MAX_LABEL {
            out.push('…');
        }
        Self(out)
    }
}

impl std::fmt::Display for Label {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A caller-supplied path as it can safely be shown: escaped, quoted, bounded.
///
/// **The escaping is the load-bearing half.** A filename is attacker-controlled
/// in any repository you clone, so a path that is a newline followed by a
/// plausible blame row would otherwise print a row byte-indistinguishable from
/// a real one — asserting `ai` authorship on `file-touched`, the one basis this
/// module reserves for recorded evidence. `char::escape_debug` is the same
/// rendering `{:?}` gives, which is what the not-blameable arm already used, so
/// the two now agree by construction rather than by coincidence; it escapes
/// every `Cc`/`Cf` code point, so `\n`, `\r`, `\t`, `ESC` and `U+202E` all come
/// out as text. Interior quotes are escaped too, so the closing quote is
/// unambiguous.
///
/// The length bound is the weaker half and is not claimed as a forgery defence:
/// a bound that would defeat *wrapping*-based forgery would have to be smaller
/// than a terminal's width, which this cannot know. It exists so a pathological
/// filename cannot fill a scrollback, and it counts **rendered** characters,
/// since one hostile code point can escape to eight. The module doc carries the
/// same disclaimer with the arithmetic (68 characters on line 1 and 80 on line 2
/// at 80 columns) — deliberately, because a blanket claim up there and an
/// admission down here would be a disagreement about the same fact.
pub(crate) fn safe_path(p: &str) -> String {
    let mut body = String::new();
    let mut width = 0usize;
    let mut cut = false;
    for c in p.chars() {
        let piece: String = c.escape_debug().collect();
        let w = piece.chars().count();
        if width + w > MAX_PATH {
            cut = true;
            break;
        }
        body.push_str(&piece);
        width += w;
    }
    if cut {
        format!("\"{body}\"…")
    } else {
        format!("\"{body}\"")
    }
}

/// Parse `git blame --line-porcelain` output into one entry per line.
///
/// The format repeats every header for every line: a `<oid> <orig> <final>`
/// header, then key/value headers, then the content prefixed by a tab. Only the
/// oid and `filename` are read — author, mail and summary are untrusted free
/// text and are deliberately dropped here rather than filtered downstream.
///
/// # What actually stops a filename from spelling a second header
///
/// **git's quoting — not the check below, and the two are easy to confuse.** To
/// choose the path that file evidence is matched against, a name has to spell
/// `filename hijacked.rs` on a line of its own, which needs a newline; and a
/// newline can never appear *inside* a value this function inspects, because it
/// splits the document into lines first (`str::lines` splits on `\n` alone —
/// `\r`, `U+0085` and `U+2028` all stay inside a line). So if git ever stopped
/// quoting, the porcelain would already carry two ordinary-looking `filename`
/// headers and no per-value check could tell them apart. What stops it is that
/// git c-quotes the whole pathname — `"b.rs\nfilename hijacked.rs"`, one line,
/// one header — and `tests/cli_e2e.rs` asserts *that*, against a name whose
/// post-newline segment is deliberately free of control characters so the
/// assertion depends on the quoting rather than on a fail-safe firing.
///
/// # What the check below is for, and the class it leaves alone
///
/// Measured on git 2.50.1: every control byte **below 0x80** is c-quoted
/// unconditionally (`ESC` → `"s\033.rs"`, `DEL` → `"d\177.rs"`), and
/// `core.quotePath` governs only bytes ≥ 0x80. So an ASCII control character in
/// a value is evidence that the framing promise did not hold at all — and a
/// value framing does not vouch for cannot ground file evidence. It keeps its
/// line, since dropping the entry would renumber every line after it, and loses
/// its path; an empty path matches no recorded file set, so it can never become
/// evidence.
///
/// The C1 controls, `U+0080..=U+009F`, are the opposite case: they are ≥ 0x80,
/// so arriving raw is exactly what `core.quotePath=false` asked for and says
/// nothing about framing. Testing `char::is_control` folded the two classes
/// together and threw away the file evidence of every C1-named path for a
/// reason that did not apply to it — the same silent degradation
/// `core.quotePath=false` was added to end for `café.rs`.
///
/// # The second thing a value has to be, to ground evidence
///
/// Distinguishable from a *different* name. `paths_contain` compares
/// `normalize_path`ed strings, and that normalization **trims**, so a file named
/// `a.rs ` (or `a.rs\u{85}`, or with any of the 25 code points Rust calls
/// whitespace at either end — measured) matches a checkpoint that recorded
/// `a.rs`. That is `file-touched` — the basis reserved for recorded evidence —
/// asserted for a path the attacker chose and a checkpoint that never saw it.
/// So a value that is not its own trimmed self loses its path here, at the
/// boundary where the value arrives, rather than by changing what matching
/// means for every caller of `normalize_path`.
///
/// **Who creates the reachable collisions — measured, because the first
/// version of this note asserted the opposite.** The 25 are exactly
/// `char::is_whitespace`, over a sweep of every scalar value to `0x10FFFF`
/// rather than the BMP. Of them, **20 reach this parser raw, and 19 of those 20
/// only because `blame` passes `-c core.quotePath=false`**: under git's default
/// (`core.quotePath=true`, git 2.50.1) exactly one arrives raw, `U+0020`, and
/// the other 19 arrive c-quoted, matching nothing. The remaining five are the
/// C0 whitespace (`TAB LF VT FF CR`), c-quoted under both settings. So this verb
/// does not inherit those 19 — **it creates them**, as the price of the
/// correctness fix that stopped `café.rs` from losing its evidence — and this
/// guard is what pays it. `tests/cli_e2e.rs` asserts the whole table against
/// real git under both settings, so the price stays measured rather than
/// narrated.
///
/// **What the guard itself costs, since "nothing" would be the convenient
/// answer and is not the true one.** A file whose name really *is* its trimmed
/// self plus whitespace — recorded that way by the projection, delivered that
/// way by git — loses its file evidence here and degrades to a labelled
/// `path-excluded (guess)`. That is unavoidable rather than free: `paths_contain`
/// cannot tell that name from the different, real one, so `file-touched` for it
/// would be a coin flip printed as a fact, and this module's whole rule is that
/// a guess is never printed as evidence. Every name that is its own normal form
/// — which is every path git can produce for an ordinary repository — is
/// untouched, and that is the claim being made, not "nothing changed".
///
/// Two adjacent facts, so they are not discovered as surprises. **Leading**
/// position collides for 27 rather than 25, `/` and `\` joining through the
/// prefix-strip and the fold; neither is reachable from git, which emits
/// repo-relative `/`-separated paths. And the *opposite* class is open and is
/// not closed here: `café.rs` in NFC and in NFD do **not** collide, so a
/// filesystem storing one form while the projection recorded the other silently
/// loses real evidence for a real file. That one costs evidence rather than
/// forging it, and it belongs to the matcher rather than to this boundary.
///
/// **This guard closes one of two entrances, and the other is named rather than
/// fixed here.** `paths_contain` normalizes *both* sides and nothing inspects
/// `files_touched`, so a checkpoint that **recorded** `src/a.rs\u{3000}` — or
/// `./src/a.rs`, or `a\b.rs` — still grounds `file-touched` for the different,
/// real `src/a.rs`, and can outrank a genuine candidate in `rank`. Smaller blast
/// radius, since the attacker must also own the commit that claims the
/// checkpoint, but the same class and the same basis. Closing it means deciding
/// what a *recorded* path that is not its own normal form means, which is the
/// attribution crate's call and not this parser's.
///
/// **The question is asked in `normalize_path`'s own vocabulary.** An earlier
/// cut asked `name.trim() != name`, which is half of what the matcher does: it
/// left the `\` → `/` fold open — so `src/a\b.rs` would still have grounded
/// evidence for the different, real `src/a/b.rs` — along with the leading `/`
/// and `./` strips, and it would have under-covered again the next time
/// `normalize_path` grew a step. `normalize_path(name) != name` is the same
/// question put to the function that actually decides, so every collision the
/// sweep finds falls out of it, including the ones nobody enumerated.
///
/// **The backslash asymmetry, decided.** `"` and `\` are c-quoted whatever
/// `core.quotePath` says (measured, git 2.50.1, both settings, asserted in
/// `tests/cli_e2e.rs`) — exactly the standing this file gives the C0 class. So
/// either of them in a value git delivered *as itself* is the same
/// broken-framing evidence, and now fires the same fail-safe. The rule cannot be
/// "no backslashes", because a c-quoted value is *made* of them; it is **an
/// unquoted value holds none of the bytes git escapes**, which is git's framing
/// contract read as one bit rather than as a model of its quoting. Deciding it
/// the other way — leaving `\` alone because the fold is unreachable from the
/// git side — would have been true and would have left the parser's own rule
/// saying two different things about two classes git treats identically.
///
/// A c-quoted value therefore keeps its spelling, and since this parser does not
/// c-unquote it can only match a projection that stores encodings too. Two
/// different names can still collide *there* (`"a\\b.rs"` and `"a\\/b.rs"` both
/// normalize to `"a//b.rs"`), which is the same routed item as the other
/// entrance: `paths_contain` normalizes **both** sides and nothing inspects
/// `files_touched`.
///
/// One residual limit, stated rather than left to be found: a filename that
/// really does hold a C0 control arrives c-quoted, and the quoted spelling
/// matches nothing the projection recorded, so that class degrades to a
/// labelled `path-excluded (guess)`. This parser does not c-unquote — the
/// evidence gained would be for names that cannot occur without an attacker,
/// and it would mean re-creating, inside a value, the newline that splitting
/// the document into lines is what makes harmless.
pub(crate) fn parse_line_porcelain(out: &[u8]) -> Vec<BlamedLine> {
    let text = String::from_utf8_lossy(out);
    let mut lines = Vec::new();
    let mut commit: Option<String> = None;
    let mut path: Option<String> = None;
    for raw in text.lines() {
        if let Some(rest) = raw.strip_prefix('\t') {
            let _ = rest;
            if let (Some(c), Some(p)) = (commit.take(), path.clone()) {
                lines.push(BlamedLine { commit: c, path: p });
            }
            continue;
        }
        if let Some(name) = raw.strip_prefix("filename ") {
            // git c-quotes a name it had to escape, and only such a name, so a
            // value is either wrapped in `"` — an *encoding* of the name, which
            // this parser does not decode — or it is the name itself.
            let quoted = name.len() >= 2 && name.starts_with('"') && name.ends_with('"');
            // `"` and `\` are escaped whatever `core.quotePath` says, so either
            // of them in a value git delivered as itself is the same
            // broken-framing evidence an ASCII control is.
            let unvouched = name.chars().any(|c| c.is_ascii_control())
                || (!quoted && (name.contains('\\') || name.contains('"')));
            // A value the matcher cannot tell from a *different* name is not
            // evidence either, and the test is the matcher's own: anything else
            // re-derives `normalize_path` and drifts from it.
            let ambiguous = !quoted && normalize_path(name) != name;
            path = Some(if unvouched || ambiguous {
                String::new()
            } else {
                name.to_owned()
            });
            continue;
        }
        let head = raw.split(' ').next().unwrap_or("");
        // 40 or 64: `rift_git::validate_oid` accepts both, and accepting only
        // the sha1 width made a sha256 repository parse to nothing at all —
        // "0 lines across 0 commits" for a file that has both.
        if matches!(head.len(), 40 | 64) && head.bytes().all(|b| b.is_ascii_hexdigit()) {
            commit = Some(head.to_ascii_lowercase());
        }
    }
    lines
}

/// Collapse consecutive lines with the same commit and path into spans.
pub(crate) fn spans(lines: &[BlamedLine]) -> Vec<Span> {
    let mut out: Vec<Span> = Vec::new();
    for (i, l) in lines.iter().enumerate() {
        let n = i + 1;
        match out.last_mut() {
            Some(s) if s.commit == l.commit && s.path == l.path => s.end = n,
            _ => out.push(Span {
                start: n,
                end: n,
                commit: l.commit.clone(),
                path: l.path.clone(),
            }),
        }
    }
    out
}

/// What every provenance surface prints when nothing recorded a worktree
/// recorded a worktree. Borrowed from the dashboard rather than spelled again, so the
/// CLI and the Canopy panels say `unknown` with the same word — a reader who
/// learns it in one place has learned it in both.
pub(crate) const UNKNOWN_WORKTREE: &str = rift_canopy::col3::UNKNOWN_WORKTREE;

/// The label a provenance surface prints for a worktree fact: the member's
/// name, or [`UNKNOWN_WORKTREE`] when there is no fact.
///
/// **Never `main`.** Absence means the shard carries no `worktree` member or
/// the session was never observed — back-filling either is forbidden — and
/// answering `main` would convert an absence into a claim about the main work
/// tree.
pub(crate) fn worktree_label(name: Option<&Label>) -> String {
    name.map_or_else(|| UNKNOWN_WORKTREE.to_owned(), ToString::to_string)
}

/// One `session_worktree` observation, as `rift session info` reports it.
///
/// Every string is a [`Label`]: an administrative worktree name and a branch
/// name are repository data, printed beside the same untrusted git output the
/// rest of this module neutralizes, and the type is the only way to have one.
#[derive(Clone, Debug, PartialEq, Eq, ::serde::Serialize)]
pub(crate) struct WorktreeObservation {
    /// The member's administrative name, or `main`.
    pub(crate) name: Label,
    /// `RepoPaths::worktree_token()` — 12 hex.
    pub(crate) token: Label,
    /// Whether the observed checkout was a linked worktree.
    pub(crate) linked: bool,
    /// `HEAD`'s branch at the observation; `None` for detached or unreadable.
    pub(crate) branch: Option<Label>,
    /// Where the observation came from (`rift_store::schema::SESSION_WORKTREE_BASES`).
    pub(crate) basis: Label,
    /// Epoch milliseconds of the first insert of this state.
    pub(crate) first_observed_at: i64,
}

/// Record the worktree a **non-hook** writer observed a session in —
/// the `attach` and `import` bases for worktree provenance.
///
/// The agent-hook writer lives on the hot path and memoizes through the
/// session cache; this one does not, because these verbs run once, from a
/// terminal, for a session whose cache may not exist at all (`rift import`
/// adopts transcripts the journal never saw). The row's own id still dedups:
/// it is content-addressed over the observed state, so re-running `rift
/// import` over an unchanged transcript store writes no second row, exactly as
/// a redelivered hook does not.
///
/// The observed checkout is **this** worktree — the one the operator ran the
/// verb in — and the basis says so. Nothing is inferred about where the agent
/// originally worked: that would be a claim, and the honest one is that a
/// human adopted the transcript here.
///
/// Returns whether a row was written. Never fails the verb: a projection
/// without the v14 relation costs the observation, not the import.
pub(crate) async fn record_worktree_observation(
    store: &SqliteStore,
    repo: &rift_git::RepoPaths,
    session_row_id: &str,
    basis: &str,
    now: i64,
) -> bool {
    let head = rift_git::head_state(repo);
    let token = repo.worktree_token();
    let row = rift_store::SessionWorktreeRow {
        observation_id: rift_shadow::ids::session_worktree_observation_id(
            session_row_id,
            &token,
            head.branch.as_deref(),
            head.oid.as_deref(),
            None,
        ),
        session_id: session_row_id.to_owned(),
        worktree_token: token,
        worktree_name: repo.member_name(),
        is_linked: i64::from(repo.is_linked()),
        // A LOCAL ADDRESS, canonicalized so one checkout reached through two
        // spellings is one path — the same rule the hook writer holds itself to.
        work_tree: std::fs::canonicalize(&repo.work_tree)
            .unwrap_or_else(|_| repo.work_tree.clone())
            .to_string_lossy()
            .into_owned(),
        branch: head.branch,
        head_oid: head.oid,
        // A subagent cannot run these verbs: both refuse to run from a hook.
        subagent_id: None,
        basis: basis.to_owned(),
        first_observed_at: now,
        schema_version: 1,
    };
    rift_store::record_session_worktree(store, &row)
        .await
        .unwrap_or(false)
}

/// The member a checkpoint was **archived in** — `checkpoint_worktree`'s
/// `worktree_name`, or `None` for every shard that carries no
/// member.
///
/// **Total by construction, and that is deliberate.** A projection still on a
/// pre-v14 schema has no such relation, and a provenance verb must not fail
/// over a label: an unreadable table answers `None`, which is the same answer a
/// v14 projection gives for a shard archived before worktree provenance
/// existed, and both render `unknown`. The
/// query is its own round trip rather than a join onto `checkpoint` for exactly
/// that reason — a join would take the whole verb down with the relation.
pub(crate) async fn checkpoint_worktree(store: &SqliteStore, id: &str) -> Option<Label> {
    let rows = store
        .query(
            "SELECT worktree_name FROM checkpoint_worktree WHERE checkpoint_id = ?",
            &[Value::text(id.to_owned())],
        )
        .await
        .ok()?;
    rows.first()
        .and_then(|r| r.get_text(0).ok())
        .map(|name| Label::of(&name))
}

/// The distinct members a set of checkpoints was archived in, in name order,
/// with [`UNKNOWN_WORKTREE`] present exactly when at least one of them carries
/// no row.
///
/// The window verbs' shape: a report over many checkpoints has many answers,
/// and collapsing them to one would name a worktree the report does not cover.
/// An empty input is an empty spread — the caller renders that as `unknown`,
/// because "which worktree did this window happen in" has no answer when the
/// window holds nothing.
pub(crate) async fn worktree_spread(store: &SqliteStore, ids: &[String]) -> Vec<String> {
    let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for id in ids {
        seen.insert(worktree_label(
            checkpoint_worktree(store, id).await.as_ref(),
        ));
    }
    seen.into_iter().collect()
}

/// Render a spread for a human line: the names, comma-separated, or
/// [`UNKNOWN_WORKTREE`] when there is nothing to name.
pub(crate) fn spread_text(spread: &[String]) -> String {
    if spread.is_empty() {
        return UNKNOWN_WORKTREE.to_owned();
    }
    spread.join(", ")
}

/// A session's **current** worktree: the observation with the greatest
/// `first_observed_at`, ties broken by name so the answer is deterministic.
///
/// `None` for every session captured before worktree provenance existed — and for every session
/// whose projection predates v14, on the same totality rule as
/// [`checkpoint_worktree`].
pub(crate) async fn session_worktree(store: &SqliteStore, row_id: &str) -> Option<Label> {
    session_worktree_history(store, row_id)
        .await
        .last()
        .map(|o| o.name.clone())
}

/// A session's worktree **history**: every observation, oldest first — the
/// ordered set is what an auditor reads.
///
/// Ordered by `first_observed_at` then `observation_id`, which is the same
/// total order the writer's own tests read the relation in, so two runs cannot
/// disagree about which observation is current.
pub(crate) async fn session_worktree_history(
    store: &SqliteStore,
    row_id: &str,
) -> Vec<WorktreeObservation> {
    let Ok(rows) = store
        .query(
            "SELECT worktree_name, worktree_token, is_linked, branch, basis, first_observed_at \
             FROM session_worktree WHERE session_id = ? \
             ORDER BY first_observed_at, observation_id",
            &[Value::text(row_id.to_owned())],
        )
        .await
    else {
        return Vec::new();
    };
    rows.iter()
        .filter_map(|r| {
            Some(WorktreeObservation {
                name: Label::of(&r.get_text(0).ok()?),
                token: Label::of(&r.get_text(1).ok()?),
                linked: r.get_i64(2).ok()? != 0,
                branch: r.get_opt_text(3).ok()?.map(|b| Label::of(&b)),
                basis: Label::of(&r.get_text(4).ok()?),
                first_observed_at: r.get_i64(5).ok()?,
            })
        })
        .collect()
}

/// Read one checkpoint's metadata, file set and sessions.
async fn checkpoint_facts(store: &SqliteStore, id: &str) -> Result<CheckpointFacts> {
    let rows = store
        .query(
            "SELECT created_at, strategy FROM checkpoint WHERE checkpoint_id = ?",
            &[Value::text(id.to_owned())],
        )
        .await?;
    let row = match rows.first() {
        Some(r) => Some(r.get_i64(0)?),
        None => None,
    };
    let strategy = match rows.first() {
        Some(r) => Some(r.get_text(1)?),
        None => None,
    };
    let mut facts = CheckpointFacts {
        // The sentinel is CLASSIFIED here, not passed through: a pre-fix
        // native row's `-1` is "no recorded time", and letting it ride as
        // `Some(-1)` made the tie-break call it resolved metadata and the
        // documents print it as an instant. The row's file evidence still
        // loads below — an undated checkpoint that touched the file must
        // keep beating a dated one that did not (rank criterion 1 outranks
        // criterion 3).
        created_at_ms: row.filter(|&ms| ms != rift_shadow::CREATED_AT_UNKNOWN),
        strategy,
        // Its own round trip, not a join onto the row above: `checkpoint_worktree`
        // is a v14 relation and a projection that predates it must cost the
        // LABEL, never the answer.
        worktree: checkpoint_worktree(store, id).await,
        ..CheckpointFacts::default()
    };
    if row.is_none() {
        return Ok(facts);
    }
    for r in store
        .query(
            "SELECT fr.path FROM attribution a \
             JOIN file_revision fr ON fr.file_revision_id = a.file_revision_id \
             WHERE a.checkpoint_id = ?",
            &[Value::text(id.to_owned())],
        )
        .await?
    {
        facts.files_touched.push(r.get_text(0)?);
    }
    for r in store
        .query(
            "SELECT sic.session_id, COALESCE(s.agent, 'unknown'), COALESCE(s.model_primary, 'unknown') \
             FROM session_in_checkpoint sic \
             LEFT JOIN session s ON s.session_id = sic.session_id \
             WHERE sic.checkpoint_id = ? ORDER BY sic.ordinal",
            &[Value::text(id.to_owned())],
        )
        .await?
    {
        facts.sessions.push(SessionRef {
            session_id: r.get_text(0)?,
            agent: Label::of(&r.get_text(1)?),
            model: Label::of(&r.get_text(2)?),
        });
    }
    Ok(facts)
}

/// The per-file authorship counts one checkpoint recorded for `path`.
async fn file_counts(store: &SqliteStore, id: &str, path: &str) -> Result<Option<AttrCounts>> {
    let rows = store
        .query(
            "SELECT a.agent_added, a.human_added, a.human_modified_agent, a.confidence_milli \
             FROM attribution a \
             JOIN file_revision fr ON fr.file_revision_id = a.file_revision_id \
             WHERE a.checkpoint_id = ? AND fr.path = ?",
            &[Value::text(id.to_owned()), Value::text(path.to_owned())],
        )
        .await?;
    let mut acc: Option<AttrCounts> = None;
    for r in &rows {
        // A path can have several revisions inside one checkpoint; the counts
        // add, and the weakest confidence is the honest one to report.
        let c = AttrCounts {
            agent_added: r.get_i64(0)?,
            human_added: r.get_i64(1)?,
            human_modified_agent: r.get_i64(2)?,
            confidence_milli: r.get_i64(3)?,
        };
        acc = Some(acc.map_or(c, |a| {
            AttrCounts {
                agent_added: a.agent_added.saturating_add(c.agent_added),
                human_added: a.human_added.saturating_add(c.human_added),
                human_modified_agent: a
                    .human_modified_agent
                    .saturating_add(c.human_modified_agent),
                confidence_milli: a.confidence_milli.min(c.confidence_milli),
            }
        }));
    }
    Ok(acc)
}

/// Run `git blame --line-porcelain` (bounded to `range` when given, via git's
/// own `-L`) and return spans carrying TRUE file line numbers. `Ok(None)` is
/// git's refusal — an untracked path, or a range past the end of the file —
/// which each verb words for itself.
fn blamed_spans(
    repo: &rift_git::RepoPaths,
    file: &str,
    range: Option<LineRange>,
) -> Result<Option<Vec<Span>>> {
    let mut args = vec![
        "-c".to_owned(),
        "core.quotePath=false".to_owned(),
        "blame".to_owned(),
        "--line-porcelain".to_owned(),
    ];
    if let Some(r) = range {
        // git does the trimming: `-L N,M` blames only the asked lines, so a
        // one-line question on a ten-thousand-line file costs one line's
        // porcelain, not a post-hoc filter over all of it.
        args.push("-L".to_owned());
        args.push(format!("{},{}", r.start, r.end));
    }
    args.push("--".to_owned());
    args.push(file.to_owned());
    let (ok, out) = rift_git::run_git_status(repo, &args, &[], &[])?;
    if !ok {
        return Ok(None);
    }
    let mut out_spans = spans(&parse_line_porcelain(&out));
    if let Some(r) = range {
        // The porcelain numbers what it was given from 1; shift every span so
        // the rendered numbers are true file lines again.
        let off = r.start - 1;
        for s in &mut out_spans {
            s.start += off;
            s.end += off;
        }
    }
    Ok(Some(out_spans))
}

/// `rift blame <file> [--line N|N-M]`: per-line provenance for one file.
pub(crate) async fn blame(
    store: &SqliteStore,
    root: &std::path::Path,
    file: &str,
    line: Option<&str>,
) -> Result<Outcome> {
    if file.trim().is_empty() {
        return Err(RiftError::validity("blame needs a path"));
    }
    let range = line.map(parse_line_range).transpose()?;
    let repo = rift_git::discover(root)?;
    // `--` before the path: `run_git_status` takes an arbitrary argv with no
    // screening, so without it a filename beginning with `-` is parsed as a
    // flag. Load-bearing only for a path that is really tracked *and* really
    // spelled like one, which is why it is pinned against tracked files named
    // `-w` and `--evil.rs` (`tests/cli_e2e.rs`): git exits 129 for both without
    // it — the first for want of a path after consuming the option, the second
    // as `error: unknown option` — and 0 with it.
    //
    // `core.quotePath=false` is a **correctness** setting, not a safety one.
    // git's default c-quotes every byte ≥ 0x80, so `café.rs` arrives as
    // `"caf\303\251.rs"` and matches nothing the projection recorded — every
    // non-ASCII path silently loses its file evidence and degrades to a guess.
    // Turning it off does not weaken the parse boundary: control characters are
    // c-quoted unconditionally, which `core.quotePath` does not govern
    // (measured; asserted in `tests/cli_e2e.rs`).
    let Some(spans) = blamed_spans(&repo, file, range)? else {
        let (what, line_json) = match range {
            // `-L` past the end of the file fails the same way an untracked
            // path does; the message carries both candidate causes.
            Some(r) => (
                format!(
                    "cannot blame {} at lines {}-{}: path untracked or range past end of file",
                    safe_path(file),
                    r.start,
                    r.end
                ),
                serde_json::json!(format!("{}-{}", r.start, r.end)),
            ),
            None => (
                format!(
                    "cannot blame {}: not a tracked path in this worktree",
                    safe_path(file)
                ),
                serde_json::Value::Null,
            ),
        };
        return Ok(Outcome::coded(
            exit::PRECONDITION,
            what,
            serde_json::json!({ "error": "not_blameable", "file": file, "line": line_json }),
        ));
    };
    let res = resolve(store, spans, |oid| commit_claims(&repo, oid)).await?;
    Ok(render(
        file,
        range,
        &res.rows,
        &res.claims,
        &res.facts,
        res.reads,
    ))
}

/// Envelope version for `rift why --json`. Bump on any breaking shape change.
const WHY_SCHEMA_VERSION: u32 = 1;

/// Split `PATH[:LINE]` (and reconcile the `--line` flag): the tail after the
/// LAST colon is a line spec only when it matches the line grammar, so
/// `C:\repo\a.rs` and a path that merely contains colons stay paths. A range
/// in either position is refused with the sentence the legacy backend used, because `why`
/// answers for one line.
pub(crate) fn parse_why_spec(spec: &str, flag: Option<&str>) -> Result<(String, Option<usize>)> {
    let single = |s: &str| -> Result<usize> {
        let r = parse_line_range(s)?;
        if r.start != r.end {
            return Err(RiftError::validity(
                "why explains a single line; use rift blame for a range",
            ));
        }
        Ok(r.start)
    };
    let (path, colon_line) = match spec.rsplit_once(':') {
        Some((head, tail))
            if !head.is_empty()
                && tail.bytes().any(|b| b.is_ascii_digit())
                && tail.bytes().all(|b| b.is_ascii_digit() || b == b'-') =>
        {
            // Line-LIKE but malformed (`:0`, `:3-`, `:1-2-3`, a range) errors
            // here rather than being blamed as a path that cannot exist.
            (head.to_owned(), Some(single(tail)?))
        }
        _ => (spec.to_owned(), None),
    };
    let flag_line = flag.map(single).transpose()?;
    if colon_line.is_some() && flag_line.is_some() {
        return Err(RiftError::validity(
            "give the line once: PATH:LINE or --line N, not both",
        ));
    }
    Ok((path, colon_line.or(flag_line)))
}

/// A redacted excerpt as it can safely reach a terminal: every char through
/// `escape_debug` (C0/C1, ESC, RTL overrides all become text), bounded to
/// `max` rendered characters with a `…` when cut. Newlines stay literal so the
/// caller can indent each line — they are the one control char a prompt block
/// wants, and the caller prints them only via its own `  | ` prefix.
pub(crate) fn safe_excerpt(s: &str, max: usize) -> String {
    let mut out = String::new();
    let mut width = 0usize;
    for c in s.chars() {
        if width >= max {
            out.push('…');
            break;
        }
        if c == '\n' {
            out.push('\n');
            width += 1;
            continue;
        }
        let piece: String = c.escape_debug().collect();
        width += piece.chars().count();
        out.push_str(&piece);
    }
    out
}

/// Best-effort prompt text out of one archived transcript record. The record
/// shapes across captured dialects: `message` as a string, `message.content`
/// as a string or as `[{type:"text",text}]` blocks, or `payload.text`. When no
/// string is found the raw record line stands in (it is already redacted), and
/// the caller says which happened.
fn extract_prompt_text(v: &serde_json::Value) -> Option<String> {
    if let Some(s) = v.get("message").and_then(serde_json::Value::as_str) {
        return Some(s.to_owned());
    }
    if let Some(c) = v.pointer("/message/content") {
        if let Some(s) = c.as_str() {
            return Some(s.to_owned());
        }
        if let Some(items) = c.as_array() {
            let text: Vec<&str> = items
                .iter()
                .filter_map(|i| i.get("text").and_then(serde_json::Value::as_str))
                .collect();
            if !text.is_empty() {
                return Some(text.join("\n"));
            }
        }
    }
    v.pointer("/payload/text")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
}

/// The redacted prompt as read from the chosen checkpoint's own shard.
pub(crate) struct PromptView {
    /// Extracted (or raw-record) prompt text — already-redacted stored bytes.
    pub(crate) text: String,
    /// True when a text field was extracted; false when the raw record stands in.
    pub(crate) extracted: bool,
    /// 0-based non-empty line index into the shard transcript.
    pub(crate) line: usize,
    /// SHA-256 of the line's bytes — the same address `digest.intent.json`
    /// records, so the claim is checkable against the shard.
    pub(crate) sha256: String,
}

impl PromptView {
    /// The one JSON shape a prompt has, wherever it is served.
    pub(crate) fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "text": self.text,
            "extracted": self.extracted,
            "line": self.line,
            "sha256": self.sha256,
        })
    }

    /// A view for renderer tests: fixed address, extracted text.
    #[cfg(test)]
    pub(crate) fn test_view(text: &str) -> Self {
        Self {
            text: text.to_owned(),
            extracted: true,
            line: 0,
            sha256: "00".repeat(32),
        }
    }
}

/// What the shard answered when asked for its redacted prompt. Three shapes,
/// because they render differently: only [`Self::NoTranscript`] may say
/// "pre-transcript era" — a transcript-carrying shard whose records hold no
/// prompt is a different (and differently-worded) absence. Both absences
/// serialize as `null` on the locked `--json` contracts.
pub(crate) enum ShardPrompt {
    /// The prompt, located and served from the shard's own transcript.
    Found(PromptView),
    /// A transcript artifact exists, but no prompt record was located in it.
    NoPromptRecord,
    /// The shard archives no transcript artifact at all, or the shard/walker
    /// is unavailable.
    NoTranscript,
}

impl ShardPrompt {
    /// The served view, when one was found — the single JSON projection point.
    pub(crate) fn view(&self) -> Option<&PromptView> {
        match self {
            Self::Found(p) => Some(p),
            Self::NoPromptRecord | Self::NoTranscript => None,
        }
    }
}

/// Read the redacted prompt for `checkpoint_id` from its shard: prefer the
/// intent digest's locator, fall back to the first `"type":"user"` record for
/// pre-digest shards.
///
/// Transcript resolution covers both writer eras: `transcript.jsonl` is the
/// redacted full transcript a **canon-3 native shard** archives (the same
/// bytes `digest.intent.json`'s `prompt_line` indexes); `full.jsonl` /
/// `sessions/0/full.jsonl` are the legacy-dialect names, kept for older
/// shards. `transcript.compact.jsonl` is deliberately not a prompt source:
/// the intent locator indexes the full transcript, and the pair always
/// travels together.
pub(crate) fn prompt_from_shard(root: &std::path::Path, checkpoint_id: &str) -> ShardPrompt {
    let Ok(walker) = rift_recall_substrate::ShardWalker::open(root) else {
        return ShardPrompt::NoTranscript;
    };
    let transcript = ["transcript.jsonl", "full.jsonl", "sessions/0/full.jsonl"]
        .into_iter()
        .find_map(|name| walker.read_artifact_any(checkpoint_id, name).ok().flatten());
    let Some(transcript) = transcript else {
        return ShardPrompt::NoTranscript;
    };
    let lines: Vec<&[u8]> = transcript
        .split(|b| *b == b'\n')
        .filter(|l| !l.is_empty())
        .collect();
    let located = walker
        .read_artifact_any(checkpoint_id, "digest.intent.json")
        .ok()
        .flatten()
        .and_then(|d| {
            let v: serde_json::Value = serde_json::from_slice(&d).ok()?;
            usize::try_from(v.pointer("/intent/prompt_line")?.as_u64()?).ok()
        });
    let line_idx = located.or_else(|| {
        lines.iter().position(|l| {
            serde_json::from_slice::<serde_json::Value>(l)
                .is_ok_and(|v| v.get("type").and_then(serde_json::Value::as_str) == Some("user"))
        })
    });
    let Some(line_idx) = line_idx else {
        return ShardPrompt::NoPromptRecord;
    };
    let Some(bytes) = lines.get(line_idx) else {
        return ShardPrompt::NoPromptRecord;
    };
    let record = serde_json::from_slice::<serde_json::Value>(bytes).ok();
    let (text, extracted) = match record.as_ref().and_then(extract_prompt_text) {
        Some(t) => (t, true),
        None => (String::from_utf8_lossy(bytes).into_owned(), false),
    };
    ShardPrompt::Found(PromptView {
        text,
        extracted,
        line: line_idx,
        sha256: rift_shadow::sha256_hex(&[bytes]),
    })
}

/// One session's four-class token totals and its non-superseded cost, summed
/// over `turn → model_call → token_usage / cost_fact`.
#[derive(Clone, Debug, ::serde::Serialize)]
pub(crate) struct CostView {
    pub(crate) session_id: String,
    pub(crate) uncached_input: i64,
    pub(crate) cached_input: i64,
    pub(crate) cache_creation: i64,
    pub(crate) output: i64,
    /// Micro-USD over current (non-superseded, open-valid) cost facts; `None`
    /// when no fact rows exist — tokens without a priced bill stay tokens.
    pub(crate) amount_micros: Option<i64>,
}

/// Sum one session's recorded token classes and cost. `Ok(None)` when the
/// session has no recorded model calls at all.
pub(crate) async fn session_cost(
    store: &SqliteStore,
    session_id: &str,
) -> Result<Option<CostView>> {
    let rows = store
        .query(
            "SELECT COUNT(*), COALESCE(SUM(tu.uncached_input),0), \
             COALESCE(SUM(tu.cached_input),0), COALESCE(SUM(tu.cache_creation),0), \
             COALESCE(SUM(tu.output),0) \
             FROM token_usage tu \
             JOIN model_call mc ON mc.call_id = tu.call_id \
             JOIN turn t ON t.turn_id = mc.turn_id \
             WHERE t.session_id = ?",
            &[Value::text(session_id.to_owned())],
        )
        .await?;
    let Some(r) = rows.first() else {
        return Ok(None);
    };
    if r.get_i64(0)? == 0 {
        return Ok(None);
    }
    let mut view = CostView {
        session_id: session_id.to_owned(),
        uncached_input: r.get_i64(1)?,
        cached_input: r.get_i64(2)?,
        cache_creation: r.get_i64(3)?,
        output: r.get_i64(4)?,
        amount_micros: None,
    };
    let cost = store
        .query(
            "SELECT COUNT(*), COALESCE(SUM(cf.amount_micros),0) \
             FROM cost_fact cf \
             JOIN model_call mc ON mc.call_id = cf.call_id \
             JOIN turn t ON t.turn_id = mc.turn_id \
             WHERE t.session_id = ? AND cf.valid_to IS NULL \
             AND NOT EXISTS (SELECT 1 FROM cost_fact s WHERE s.supersedes_id = cf.cost_fact_id)",
            &[Value::text(session_id.to_owned())],
        )
        .await?;
    if let Some(c) = cost.first() {
        if c.get_i64(0)? > 0 {
            view.amount_micros = Some(c.get_i64(1)?);
        }
    }
    Ok(Some(view))
}

/// `rift why <file>[:LINE]`: one line's full story — attribution, agent,
/// model, checkpoint, session, commit, the REDACTED stored prompt, the
/// originating session's four-class cost, and the policy-gated handle to
/// ladder deeper. Reads only: stored `RedactedBytes` artifacts from the shard
/// (the redaction gate ran at capture; nothing is sealed here) and projection
/// rows.
pub(crate) async fn why(
    store: &SqliteStore,
    root: &std::path::Path,
    spec: &str,
    line_flag: Option<&str>,
) -> Result<Outcome> {
    let (file, line) = parse_why_spec(spec, line_flag)?;
    if file.trim().is_empty() {
        return Err(RiftError::validity("why needs a path"));
    }
    let repo = rift_git::discover(root)?;
    let asked = line.map(|n| LineRange { start: n, end: n });
    let Some(all_spans) = blamed_spans(&repo, &file, asked)? else {
        let what = match line {
            Some(n) => format!(
                "cannot answer for {} line {n}: path untracked or line past end of file",
                safe_path(&file)
            ),
            None => format!(
                "cannot answer for {}: not a tracked path in this worktree",
                safe_path(&file)
            ),
        };
        return Ok(Outcome::coded(
            exit::PRECONDITION,
            what,
            serde_json::json!({
                "error": "not_answerable",
                "file": file,
                "line": line,
            }),
        ));
    };
    let res = resolve(store, all_spans, |oid| commit_claims(&repo, oid)).await?;

    // The answering row: the asked line's span, or — for a whole-file question
    // — the dominant one: most evidence-backed lines, then most lines, then
    // earliest position. Deterministic, and the basis is printed, not implied.
    let chosen = match line {
        Some(n) => res
            .rows
            .iter()
            .find(|r| r.span.start <= n && n <= r.span.end),
        None => res.rows.iter().max_by_key(|r| {
            let width = r.span.end - r.span.start + 1;
            let evidence = r.selection.as_ref().is_some_and(|s| s.basis.is_evidence());
            (
                usize::from(evidence) * width,
                usize::from(r.selection.is_some()) * width,
                std::cmp::Reverse(r.span.start),
            )
        }),
    };
    let Some(row) = chosen else {
        return Ok(Outcome::coded(
            exit::PRECONDITION,
            format!("{} has no blameable lines", safe_path(&file)),
            serde_json::json!({ "error": "not_answerable", "file": file, "line": line }),
        ));
    };

    let session = row.sessions.first();
    let checkpoint = row.selection.as_ref().and_then(|sel| {
        res.facts.get(&sel.checkpoint_id).map(|f| {
            serde_json::json!({
                "id": sel.checkpoint_id,
                "created_at_ms": f.created_at_ms,
                "files_touched": f.files_touched.len(),
                "strategy": f.strategy,
                "worktree": f.worktree,
            })
        })
    });
    let prompt = row
        .selection
        .as_ref()
        .map(|sel| prompt_from_shard(root, &sel.checkpoint_id));
    let cost = match session {
        Some(s) => session_cost(store, &s.session_id).await?,
        None => None,
    };
    let file_handle = std::fs::read(root.join(&file))
        .ok()
        .map(|bytes| format!("file:{file}@sha256:{}", rift_shadow::sha256_hex(&[&bytes])));
    Ok(render_why(&WhyView {
        file: &file,
        line,
        row,
        checkpoint,
        prompt,
        cost,
        file_handle,
        reads: res.reads,
    }))
}

/// Everything [`render_why`] needs, resolved — so the rendering is a pure
/// function a test can drive without a store, a repository, or a shard.
struct WhyView<'a> {
    file: &'a str,
    line: Option<usize>,
    row: &'a Resolved,
    checkpoint: Option<serde_json::Value>,
    prompt: Option<ShardPrompt>,
    cost: Option<CostView>,
    file_handle: Option<String>,
    reads: Reads,
}

/// The human and JSON halves of one `why` answer. Every transcript- or
/// adapter-derived string goes through the existing neutralizers: [`Label`]
/// (agent/model), [`safe_path`] (the path), [`safe_excerpt`] (the prompt
/// block).
fn render_why(v: &WhyView<'_>) -> Outcome {
    let WhyView {
        file,
        line,
        row,
        checkpoint,
        prompt,
        cost,
        file_handle,
        reads,
    } = v;
    let (line, row) = (*line, *row);
    let tag = Tag::of(row.counts.as_ref());
    let session = row.sessions.first();
    let mut human = match line {
        Some(n) => format!("rift why {}:{n}\n", safe_path(file)),
        None => format!("rift why {}\n", safe_path(file)),
    };
    human.push_str(&format!(
        "  {:>11}  {}  {}{}\n",
        format!("{}-{}", row.span.start, row.span.end),
        short(&row.span.commit),
        answer(row, tag),
        import_note_of(checkpoint.as_ref()),
    ));
    // Worktree provenance on the one-line verb: the answering checkpoint's shard
    // member, `unknown` when it carries none — never `main`.
    human.push_str(&format!(
        "  archived_in: {}\n",
        archived_in_of(checkpoint.as_ref())
    ));
    match &prompt {
        Some(ShardPrompt::Found(p)) => {
            human.push_str(&format!(
                "  prompt (redacted, line {} of the checkpoint transcript{}):\n",
                p.line,
                if p.extracted { "" } else { ", raw record" }
            ));
            for l in safe_excerpt(&p.text, 600).lines().take(8) {
                human.push_str(&format!("    | {l}\n"));
            }
        }
        Some(ShardPrompt::NoPromptRecord) => {
            human.push_str(
                "  prompt: the archived transcript holds no prompt record for this checkpoint\n",
            );
        }
        Some(ShardPrompt::NoTranscript) => {
            human.push_str("  prompt: not archived for this checkpoint (pre-transcript era, or shard unavailable)\n");
        }
        None => {}
    }
    match &cost {
        Some(c) => {
            human.push_str(&format!(
                "  cost (session {}): in {} / cached {} / cache-w {} / out {} tokens{}\n",
                Label::of(&c.session_id),
                c.uncached_input,
                c.cached_input,
                c.cache_creation,
                c.output,
                c.amount_micros.map_or(String::new(), |m| format!(
                    "  (${}.{:06})",
                    m / 1_000_000,
                    m.rem_euclid(1_000_000)
                )),
            ));
        }
        None => {
            if session.is_some() {
                human.push_str("  cost: no model calls recorded for the originating session\n");
            }
        }
    }
    if let Some(h) = &file_handle {
        human.push_str(&format!(
            "  ladder deeper (policy-gated): rift recall retrieve-exact \"{h}\" --reason <why>\n"
        ));
    }

    let json = serde_json::json!({
        "schema_version": WHY_SCHEMA_VERSION,
        "file": file,
        "line": line,
        "span": {
            "start": row.span.start,
            "end": row.span.end,
            "commit": row.span.commit,
            "path": row.span.path,
        },
        "tag": tag,
        "selection": row.selection,
        "attribution": row.counts,
        "sessions": row.sessions,
        "checkpoint": checkpoint,
        "prompt": prompt.as_ref().and_then(ShardPrompt::view).map(PromptView::to_json),
        "cost": cost,
        "handles": { "file": file_handle },
        "reads": reads,
    });
    Outcome::ok(human.trim_end().to_owned(), json)
}

/// How much work [`resolve`] actually did.
///
/// Every field counts a **read that happened**, incremented at the call site
/// that performs it. That distinction is the point: the first version of the
/// caching test asserted `facts.len() == 1`, which is the size of the returned
/// map, and a map has one entry per distinct key whether it was filled once or
/// three hundred times — so defeating `fact_cache` or `count_cache` left the
/// suite green while the work grew a hundredfold. It is the same "counts cache
/// entries, not work" defect this module's own tests caught in the summary one
/// level up, recurring inside the test written to fix it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ::serde::Serialize)]
pub(crate) struct Reads {
    /// Commit messages read.
    pub(crate) messages: usize,
    /// `checkpoint` / `attribution` / `session` reads for checkpoint metadata.
    pub(crate) facts: usize,
    /// Per-file attribution reads, one per distinct (checkpoint, path).
    pub(crate) counts: usize,
}

/// Everything [`resolve`] produced.
pub(crate) struct Resolution {
    /// One answer per span, in span order.
    pub(crate) rows: Vec<Resolved>,
    /// Claims by commit oid — the renderer reads this so a `ClaimSet` is
    /// serialized once per commit rather than once per span.
    pub(crate) claims: HashMap<String, ClaimSet>,
    /// Checkpoint metadata by id, likewise serialized once.
    pub(crate) facts: HashMap<String, CheckpointFacts>,
    /// What it cost, in reads.
    pub(crate) reads: Reads,
}

/// Resolve every span against the projection.
///
/// `read_message` is injected rather than called directly so that "one message
/// read per **commit**, not per line" is a testable property of this function
/// instead of a sentence in a doc comment: a test counts the calls. The two
/// projection reads are counted in [`Reads`] for the same reason, and by the
/// same rule — a number that only a *narrower* code path can change.
pub(crate) async fn resolve<F>(
    store: &SqliteStore,
    spans: Vec<Span>,
    mut read_message: F,
) -> Result<Resolution>
where
    F: FnMut(&str) -> Result<ClaimSet>,
{
    let mut reads = Reads::default();
    let mut claim_cache: HashMap<String, ClaimSet> = HashMap::new();
    let mut candidate_cache: HashMap<String, Vec<CheckpointCandidate>> = HashMap::new();
    let mut fact_cache: HashMap<String, CheckpointFacts> = HashMap::new();
    let mut count_cache: HashMap<(String, String), Option<AttrCounts>> = HashMap::new();
    let mut resolved: Vec<Resolved> = Vec::new();

    for span in spans {
        if is_uncommitted(&span.commit) {
            resolved.push(Resolved {
                span,
                selection: None,
                counts: None,
                sessions: Vec::new(),
            });
            continue;
        }
        // Both the claims and the CANDIDATE LIST are memoised per commit. The
        // candidate list is a pure function of the commit — `select_for_file`
        // takes the path as a separate argument — so rebuilding it per span
        // would clone every checkpoint's whole file set once per span, which
        // for a many-span file is the per-line cost this verb is meant to
        // avoid wearing a different hat.
        if !candidate_cache.contains_key(&span.commit) {
            let set = read_message(&span.commit)?;
            reads.messages += 1;
            // The candidate set is the UNION of what the commit SAYS and what
            // the projection RECORDED: trailer claims first (in trailer
            // order), then every checkpoint whose recorded anchor
            // (`checkpoint_commit`, written by the bridge from the shard's
            // own metadata) is this commit. The union exists because the two
            // can diverge and were MEASURED diverging: the predicted trailer
            // is derived before the commit from an archive that carries no
            // transcript, while the landed shard of a transcript-carrying
            // session includes its transcript and digests in the id — so on
            // exactly the sessions with the richest provenance, the trailer
            // names an id that never lands and a trailer-only walk answers
            // `unknown` forever. The anchored checkpoint is not a guess: the
            // shard itself recorded which commit it captured. Where trailer
            // and anchor agree the union is a set-union no-op; where they
            // disagree, rank (file evidence, then resolved metadata, then
            // created-at) picks among REAL rows instead of
            // phantoms. Settled 2026-08-29: this union is the PERMANENT answer, not a stopgap until the trailer
            // learns the transcript — the trailer is a claim by definition,
            // and `prepare-commit-msg` keeps the 100 ms git budget.
            let mut candidates = Vec::with_capacity(set.claims.len());
            let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
            let anchored: Vec<String> = store
                .query(
                    "SELECT checkpoint_id FROM checkpoint_commit \
                     WHERE commit_oid = ? ORDER BY checkpoint_id",
                    &[Value::text(span.commit.clone())],
                )
                .await?
                .iter()
                .filter_map(|r| r.get_text(0).ok())
                .collect();
            let ids = set
                .claims
                .iter()
                .map(|c| &c.checkpoint_id)
                .chain(anchored.iter());
            for (ordinal, id) in ids.enumerate() {
                if !seen.insert(id.clone()) {
                    continue;
                }
                if !fact_cache.contains_key(id) {
                    let f = checkpoint_facts(store, id).await?;
                    reads.facts += 1;
                    fact_cache.insert(id.clone(), f);
                }
                let facts = fact_cache.get(id).cloned().unwrap_or_default();
                candidates.push(CheckpointCandidate {
                    checkpoint_id: id.clone(),
                    ordinal,
                    created_at_ms: facts.created_at_ms,
                    files_touched: facts.files_touched,
                });
            }
            claim_cache.insert(span.commit.clone(), set);
            candidate_cache.insert(span.commit.clone(), candidates);
        }
        let selection = candidate_cache
            .get(&span.commit)
            .and_then(|c| select_for_file(c, &span.path));
        let (counts, sessions) = match &selection {
            Some(s) => {
                let key = (s.checkpoint_id.clone(), span.path.clone());
                if !count_cache.contains_key(&key) {
                    let c = file_counts(store, &s.checkpoint_id, &span.path).await?;
                    reads.counts += 1;
                    count_cache.insert(key.clone(), c);
                }
                (
                    count_cache.get(&key).copied().flatten(),
                    fact_cache
                        .get(&s.checkpoint_id)
                        .map(|f| f.sessions.clone())
                        .unwrap_or_default(),
                )
            }
            None => (None, Vec::new()),
        };
        resolved.push(Resolved {
            span,
            selection,
            counts,
            sessions,
        });
    }
    Ok(Resolution {
        rows: resolved,
        claims: claim_cache,
        facts: fact_cache,
        reads,
    })
}

/// Read one commit's message and parse every checkpoint it claims.
pub(crate) fn commit_claims(repo: &rift_git::RepoPaths, oid: &str) -> Result<ClaimSet> {
    // The oid comes from git's own porcelain, but validating it here is what
    // makes "no argument injection" a property of this call site rather than of
    // git's output format.
    rift_git::validate_oid(oid)?;
    let args = vec![
        "log".to_owned(),
        "-1".to_owned(),
        "--format=%B".to_owned(),
        oid.to_owned(),
        "--".to_owned(),
    ];
    let (ok, out) = rift_git::run_git_status(repo, &args, &[], &[])?;
    if !ok {
        return Ok(ClaimSet::default());
    }
    Ok(parse_checkpoint_claims(&String::from_utf8_lossy(&out)))
}

/// Render the human and JSON halves. Pure: everything it needs is resolved.
///
/// `reads` is carried into `--json` rather than kept as a test-only counter, so
/// the module's own cost claim is visible in the artifact and not merely
/// assertable about it — and so the counters live on the production path they
/// describe rather than beside it.
pub(crate) fn render(
    file: &str,
    range: Option<LineRange>,
    resolved: &[Resolved],
    claims: &HashMap<String, ClaimSet>,
    facts: &HashMap<String, CheckpointFacts>,
    reads: Reads,
) -> Outcome {
    let total: usize = resolved.iter().map(|r| r.span.end - r.span.start + 1).sum();
    let mut commits: BTreeMap<&str, &ClaimSet> = BTreeMap::new();
    let mut checkpoints: BTreeMap<&str, serde_json::Value> = BTreeMap::new();
    // The counts the summary reports are derived from the SPANS, never from the
    // caches above: a cache is an optimisation, and a summary that counts cache
    // entries reports how hard the resolver worked rather than what the file is
    // made of. (Found by a test that passed an empty cache and got "0 commits"
    // for a two-commit file.)
    let mut distinct_commits: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
    let mut distinct_checkpoints: std::collections::BTreeSet<&str> =
        std::collections::BTreeSet::new();
    let mut evidence_lines = 0usize;
    let mut spans_json = Vec::with_capacity(resolved.len());
    // Escaped, not raw: see `safe_path`. Raw, this header is one newline away
    // from printing a forged blame row above the real ones.
    let mut human = match range {
        Some(r) => format!(
            "rift blame {} --line {}-{}\n",
            safe_path(file),
            r.start,
            r.end
        ),
        None => format!("rift blame {}\n", safe_path(file)),
    };

    for r in resolved {
        let width = r.span.end - r.span.start + 1;
        let tag = Tag::of(r.counts.as_ref());
        if !is_uncommitted(&r.span.commit) {
            distinct_commits.insert(r.span.commit.as_str());
        }
        if let Some(set) = claims.get(&r.span.commit) {
            commits.insert(r.span.commit.as_str(), set);
        }
        if let Some(sel) = &r.selection {
            distinct_checkpoints.insert(sel.checkpoint_id.as_str());
            if sel.basis.is_evidence() {
                evidence_lines += width;
            }
            if let Some(f) = facts.get(sel.checkpoint_id.as_str()) {
                checkpoints.insert(
                    sel.checkpoint_id.as_str(),
                    serde_json::json!({
                        "created_at_ms": f.created_at_ms,
                        "files_touched": f.files_touched.len(),
                        "sessions": f.sessions,
                        "strategy": f.strategy,
                        // Worktree provenance: beside `strategy`, the same place the
                        // `import` marker went. Null is UNKNOWN, and the human
                        // line below spells it out.
                        "worktree": f.worktree,
                    }),
                );
            }
        }
        human.push_str(&format!(
            "  {:>11}  {}  {}{}\n",
            format!("{}-{}", r.span.start, r.span.end),
            short(&r.span.commit),
            answer(r, tag),
            import_note(r, facts),
        ));
        spans_json.push(serde_json::json!({
            "start": r.span.start,
            "end": r.span.end,
            "lines": width,
            "commit": r.span.commit,
            "path": r.span.path,
            "tag": tag,
            "selection": r.selection,
            "attribution": r.counts,
            "sessions": r.sessions,
        }));
    }
    // The rendering clause for this verb: the members the answering
    // checkpoints were ARCHIVED IN (the exact word, not "worked in" — the moving
    // session's question is `session_worktree`'s). One clause rather than a
    // column per span: a file's spans routinely share one checkpoint, and a
    // repeated label per row would push the evidence count off a narrow
    // terminal to say the same thing n times.
    let archived_in: Vec<String> = distinct_checkpoints
        .iter()
        .map(|id| worktree_label(facts.get(*id).and_then(|f| f.worktree.as_ref())))
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    human.push_str(&format!(
        "  {} across {}; {} claimed; {evidence_lines} backed by recorded file evidence; \
         archived_in: {}",
        plural(total, "line"),
        plural(distinct_commits.len(), "commit"),
        plural(distinct_checkpoints.len(), "checkpoint"),
        spread_text(&archived_in),
    ));
    let mut json = serde_json::json!({
        "schema_version": SCHEMA_VERSION,
        "file": file,
        "lines": total,
        "spans": spans_json,
        "commits": commits,
        "checkpoints": checkpoints,
        "summary": {
            "commits": distinct_commits.len(),
            "checkpoints": distinct_checkpoints.len(),
            "lines_with_file_evidence": evidence_lines,
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

/// The human-readable answer for one span.
fn answer(r: &Resolved, tag: Tag) -> String {
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

/// [`import_note`] over the `why` document's checkpoint object, which carries
/// the same `strategy` the JSON serves (one source for both halves).
/// The `archived_in` value a rendered checkpoint object carries, as a human
/// line prints it: the member's name, or `unknown`.
///
/// Reads the document rather than the facts map, because the document is what
/// the caller already resolved and two readers of the same fact are two places
/// for it to disagree.
fn archived_in_of(checkpoint: Option<&serde_json::Value>) -> String {
    checkpoint
        .and_then(|c| c.get("worktree"))
        .and_then(serde_json::Value::as_str)
        .map_or_else(|| UNKNOWN_WORKTREE.to_owned(), str::to_owned)
}

fn import_note_of(checkpoint: Option<&serde_json::Value>) -> &'static str {
    let imported = checkpoint
        .and_then(|c| c.get("strategy"))
        .and_then(serde_json::Value::as_str)
        == Some("import");
    if imported {
        " (imported transcript, read-only)"
    } else {
        ""
    }
}

/// `n unit` or `n units`.
fn plural(n: usize, unit: &str) -> String {
    if n == 1 {
        format!("{n} {unit}")
    } else {
        format!("{n} {unit}s")
    }
}

/// The first 12 characters of an id — enough to recognise, short enough to scan.
fn short(id: &str) -> String {
    id.chars().take(12).collect()
}

// --- the capture-and-git half of a delta's provenance ----------------------

/// The facts a pull verb's `provenance` object needs and the recall projection
/// does not hold: when a checkpoint was recorded, which session and agent
/// family took it, what that session's turns cost, and the subject of the
/// commit a checkpoint anchored.
///
/// **Resolved once, up front, and never lazily.** The serving handlers are
/// `async` and the oracle they consult is not, so a lazy implementation would
/// have to block on a store inside a running runtime. It is also the cheaper
/// shape: the alternative is one round trip per manifest entry, and a manifest
/// names up to 64 files. Four aggregate queries and one `git log` answer for
/// the whole repository.
///
/// **Every query here is one the provenance verbs already run** — the dated
/// checkpoint row and its sentinel classification from [`checkpoint_facts`],
/// the session/agent join from the same function, and the two sums from
/// [`session_cost`], grouped by session instead of filtered to one. A second
/// spelling of the same join is how two surfaces come to disagree about what a
/// turn cost.
///
/// Every field is independently absent, and absence is **unknown**: a
/// projection that will not open, a query that fails, a `git log` that cannot
/// run — each leaves its own facts out and none of them fails the serve.
pub(crate) struct CaptureProvenance {
    /// Checkpoint id → what the capture projection records about it.
    by_checkpoint: HashMap<String, ServedFacts>,
    /// Commit oid → its subject line, for the commits this checkout can see.
    subjects: HashMap<String, String>,
}

/// One session's totals, shared by every checkpoint that session appears in.
#[derive(Clone, Debug, Default)]
struct SessionTotals {
    /// The agent family that ran the session, as the `session` row records it.
    agent: Option<String>,
    /// The session's four recorded token classes.
    tokens: Option<TokenClasses>,
    /// Micro-USD over its current cost facts.
    cost_micros: Option<i64>,
}

/// How far back the commit-subject map reads.
///
/// A bound rather than the whole history: the subject is a nicety beside the
/// oid, and a repository with a hundred thousand commits must not pay for one.
/// A delta older than this carries its commit oid and no subject, which is the
/// same honest shape a delta with no mirrored commit has.
const SUBJECT_WALK: usize = 2000;

impl CaptureProvenance {
    /// Read the capture projection and this checkout's recent history.
    ///
    /// Never an error. Everything it cannot reach is simply not known, and a
    /// pull verb answers with the recall-side facts alone.
    pub(crate) async fn read_capture_side(db: &std::path::Path, root: &std::path::Path) -> Self {
        let mut out = Self {
            by_checkpoint: HashMap::new(),
            subjects: HashMap::new(),
        };
        if let Ok(store) = SqliteStore::open_read_only(db) {
            let _ = out.read_projection(&store).await;
        }
        out.read_subjects(root);
        out
    }

    /// The four aggregate reads, in one pass. Any one of them failing leaves
    /// its own facts absent and keeps the others.
    async fn read_projection(&mut self, store: &SqliteStore) -> Result<()> {
        // 1. WHEN. The unknown-time sentinel is classified here exactly as
        //    `checkpoint_facts` classifies it: `-1` is "no recorded time", not
        //    an instant in 1969.
        for r in store
            .query("SELECT checkpoint_id, created_at FROM checkpoint", &[])
            .await?
        {
            let (id, at) = (r.get_text(0)?, r.get_i64(1)?);
            self.by_checkpoint.entry(id).or_default().recorded_at_ms =
                (at != rift_shadow::CREATED_AT_UNKNOWN).then_some(at);
        }
        // 2. WHO. The first session by ordinal, the same one `rift why` reports.
        let mut totals: HashMap<String, SessionTotals> = HashMap::new();
        let mut first_session: BTreeMap<String, (i64, String)> = BTreeMap::new();
        for r in store
            .query(
                "SELECT sic.checkpoint_id, sic.session_id, sic.ordinal, s.agent \
                 FROM session_in_checkpoint sic \
                 LEFT JOIN session s ON s.session_id = sic.session_id",
                &[],
            )
            .await?
        {
            let (ck, sid, ord) = (r.get_text(0)?, r.get_text(1)?, r.get_i64(2)?);
            totals.entry(sid.clone()).or_default().agent = r.get_opt_text(3)?;
            match first_session.get(&ck) {
                Some((seen, _)) if *seen <= ord => {}
                _ => {
                    first_session.insert(ck, (ord, sid));
                }
            }
        }
        // 3. WHAT IT COST, in tokens: `session_cost`'s join, grouped.
        for r in store
            .query(
                "SELECT t.session_id, COALESCE(SUM(tu.uncached_input),0), \
                 COALESCE(SUM(tu.cached_input),0), COALESCE(SUM(tu.cache_creation),0), \
                 COALESCE(SUM(tu.output),0) \
                 FROM token_usage tu \
                 JOIN model_call mc ON mc.call_id = tu.call_id \
                 JOIN turn t ON t.turn_id = mc.turn_id \
                 GROUP BY t.session_id",
                &[],
            )
            .await?
        {
            totals.entry(r.get_text(0)?).or_default().tokens = Some(TokenClasses {
                uncached_input: r.get_i64(1)?,
                cached_input: r.get_i64(2)?,
                cache_creation: r.get_i64(3)?,
                output: r.get_i64(4)?,
            });
        }
        // 4. …and in money: the same non-superseded, open-valid predicate
        //    `session_cost` applies, grouped the same way. Tokens without a
        //    priced bill stay tokens, so a session with no fact rows carries no
        //    `cost_micros` rather than a zero.
        for r in store
            .query(
                "SELECT t.session_id, COALESCE(SUM(cf.amount_micros),0) \
                 FROM cost_fact cf \
                 JOIN model_call mc ON mc.call_id = cf.call_id \
                 JOIN turn t ON t.turn_id = mc.turn_id \
                 WHERE cf.valid_to IS NULL \
                 AND NOT EXISTS (SELECT 1 FROM cost_fact s WHERE s.supersedes_id = cf.cost_fact_id) \
                 GROUP BY t.session_id",
                &[],
            )
            .await?
        {
            totals.entry(r.get_text(0)?).or_default().cost_micros = Some(r.get_i64(1)?);
        }
        for (ck, (_, sid)) in first_session {
            let t = totals.get(&sid).cloned().unwrap_or_default();
            let entry = self.by_checkpoint.entry(ck).or_default();
            entry.agent = t.agent;
            entry.tokens = t.tokens;
            entry.cost_micros = t.cost_micros;
            entry.session = Some(sid);
        }
        Ok(())
    }

    /// The subject of each of the last [`SUBJECT_WALK`] commits.
    ///
    /// One `git log`, not one per delta: the alternative is a process spawn per
    /// manifest entry, which on a 64-entry manifest is the most expensive thing
    /// a pull verb would do. A NUL separates the oid from the subject and the
    /// newline separates records, so a subject containing anything at all still
    /// parses — and a subject containing a newline cannot forge a second record,
    /// because the oid half of a forged line would not be hex.
    fn read_subjects(&mut self, root: &std::path::Path) {
        let Ok(repo) = rift_git::discover(root) else {
            return;
        };
        let args = vec![
            "log".to_owned(),
            format!("-n{SUBJECT_WALK}"),
            "--format=%H%x00%s".to_owned(),
        ];
        let Ok((true, out)) = rift_git::run_git_status(&repo, &args, &[], &[]) else {
            return;
        };
        for line in String::from_utf8_lossy(&out).lines() {
            let Some((oid, subject)) = line.split_once('\0') else {
                continue;
            };
            if !subject.is_empty() && !oid.is_empty() && oid.chars().all(|c| c.is_ascii_hexdigit())
            {
                self.subjects.insert(oid.to_owned(), subject.to_owned());
            }
        }
    }
}

impl ProvenanceOracle for CaptureProvenance {
    fn checkpoint(&self, checkpoint_id: &str) -> Option<ServedFacts> {
        self.by_checkpoint
            .get(checkpoint_id)
            .filter(|f| !f.is_empty())
            .cloned()
    }

    fn commit_subject(&self, oid: &str) -> Option<String> {
        self.subjects.get(oid).cloned()
    }
}
#[cfg(test)]
mod tests {
    use rift_shadow::attribution::SelectionBasis;

    /// [`render`] with no `--line` range — what every pre-range test meant.
    fn render_whole(
        file: &str,
        resolved: &[Resolved],
        claims: &HashMap<String, ClaimSet>,
        facts: &HashMap<String, CheckpointFacts>,
        reads: Reads,
    ) -> Outcome {
        render(file, None, resolved, claims, facts, reads)
    }

    use super::*;

    /// **The unknown-time sentinel classifies as "no recorded time" at the
    /// facts read**: a `checkpoint.created_at = -1` row — every
    /// native checkpoint archived before the writer fix — must not ride into
    /// selection as `Some(-1)`, where it ranked as *resolved metadata* older
    /// than everything and rendered in the `--json` documents as if `-1`
    /// were an instant. The classification must NOT cost the row its file
    /// evidence: an undated checkpoint that touched the file still beats a
    /// dated one that did not, because rank criterion 1 outranks criterion 3.
    #[test]
    #[allow(clippy::too_many_lines)] // one linear seed -> classify -> assert scenario
    fn a_sentinel_dated_row_classifies_as_undated_but_keeps_its_file_evidence() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        rt.block_on(async {
            use rift_store::CanonicalStore as _;
            let s = rift_store::SqliteStore::open_in_memory().unwrap();
            rift_store::apply_migrations(&s, &rift_store::default_migrations()).unwrap();
            let t = |v: &str| Value::text(v.to_owned());
            let cols = [
                "checkpoint_id",
                "strategy",
                "created_at",
                "merkle_root",
                "schema_version",
            ];
            // A sentinel-dated checkpoint that RECORDED touching f.rs…
            s.insert(
                "checkpoint",
                &cols,
                &[
                    t("csent"),
                    t("manual-commit"),
                    Value::Int(rift_shadow::CREATED_AT_UNKNOWN),
                    t("00"),
                    Value::Int(1),
                ],
            )
            .await
            .unwrap();
            rift_store::record_file_revision(
                &s,
                &rift_store::FileRevisionRow {
                    file_revision_id: "fr1".into(),
                    path: "f.rs".into(),
                    blob_oid: "b1".into(),
                    content_sha256: "c1".into(),
                    schema_version: 1,
                },
            )
            .await
            .unwrap();
            rift_store::record_attribution(
                &s,
                &rift_store::AttributionRow {
                    checkpoint_id: "csent".into(),
                    file_revision_id: "fr1".into(),
                    agent_added: 2,
                    human_added: 0,
                    human_modified_agent: 0,
                    agent_removed_by_human: 0,
                    confidence_milli: 900,
                    method_version: "m1".into(),
                    supersedes_pk: None,
                    schema_version: 1,
                },
            )
            .await
            .unwrap();
            // …and a real-dated checkpoint that did not.
            s.insert(
                "checkpoint",
                &cols,
                &[
                    t("creal"),
                    t("manual-commit"),
                    Value::Int(1_000),
                    t("00"),
                    Value::Int(1),
                ],
            )
            .await
            .unwrap();

            let sent = checkpoint_facts(&s, "csent").await.unwrap();
            assert_eq!(sent.created_at_ms, None, "the sentinel is not a time");
            assert_eq!(
                sent.files_touched,
                vec!["f.rs".to_owned()],
                "classification must not cost the row its evidence"
            );
            let real = checkpoint_facts(&s, "creal").await.unwrap();
            assert_eq!(real.created_at_ms, Some(1_000));
            let absent = checkpoint_facts(&s, "nowhere").await.unwrap();
            assert_eq!(absent.created_at_ms, None);
            assert!(absent.files_touched.is_empty());

            // Composed into selection exactly as `resolve` composes it: the
            // undated toucher beats the dated non-toucher on file evidence —
            // and SAYS it is unresolved, instead of Some(-1)'s old false
            // "resolved" claim.
            let candidates = [
                CheckpointCandidate {
                    checkpoint_id: "csent".into(),
                    ordinal: 1,
                    created_at_ms: sent.created_at_ms,
                    files_touched: sent.files_touched.clone(),
                },
                CheckpointCandidate {
                    checkpoint_id: "creal".into(),
                    ordinal: 0,
                    created_at_ms: real.created_at_ms,
                    files_touched: real.files_touched.clone(),
                },
            ];
            let sel = select_for_file(&candidates, "f.rs").unwrap();
            assert_eq!(
                sel.checkpoint_id, "csent",
                "file evidence outranks the clock"
            );
            assert!(!sel.resolved, "a sentinel-dated winner is unresolved");
        });
    }

    /// `--line` speaks the legacy backend's grammar exactly — `N` or `N-M`, digits only,
    /// 1-based, inclusive — and everything outside it is a refusal, not a
    /// guess. `+7`, `7 `, `0`, and a reversed range are the shapes a lenient
    /// parser would have quietly accepted.
    #[test]
    fn the_line_grammar_admits_n_and_n_dash_m_and_nothing_else() {
        assert_eq!(
            parse_line_range("12").unwrap(),
            LineRange { start: 12, end: 12 }
        );
        assert_eq!(
            parse_line_range("12-20").unwrap(),
            LineRange { start: 12, end: 20 }
        );
        assert_eq!(parse_line_range("7-7").unwrap().end, 7);
        for bad in [
            "", "0", "0-3", "5-4", "+7", "7 ", " 7", "a", "3-", "-3", "3-b", "1-2-3",
        ] {
            assert!(parse_line_range(bad).is_err(), "{bad:?} must be refused");
        }
    }

    /// `why`'s spec grammar: the LAST colon carries the line only when its
    /// tail is line-LIKE; a range is refused with the single-line sentence in
    /// both positions; the line may be given once.
    #[test]
    fn the_why_spec_takes_one_line_from_the_last_colon_or_the_flag() {
        let ok = |s: &str, f: Option<&str>| parse_why_spec(s, f).unwrap();
        assert_eq!(ok("src/a.rs:42", None), ("src/a.rs".into(), Some(42)));
        assert_eq!(ok("src/a.rs", None), ("src/a.rs".into(), None));
        assert_eq!(ok("src/a.rs", Some("7")), ("src/a.rs".into(), Some(7)));
        assert_eq!(ok("a:b:9", None), ("a:b".into(), Some(9)));
        // Colon tails that are not line-like stay part of the path.
        assert_eq!(ok("C:\\repo\\a.rs", None), ("C:\\repo\\a.rs".into(), None));
        assert_eq!(ok("a.rs:-", None), ("a.rs:-".into(), None));
        // Ranges: refused with the single-line sentence, both spellings.
        for (s, f) in [("a.rs:2-5", None), ("a.rs", Some("2-5"))] {
            let e = parse_why_spec(s, f).unwrap_err().to_string();
            assert!(e.contains("single line"), "{e}");
        }
        // Line-like but malformed is an error, not a path that cannot exist.
        assert!(parse_why_spec("a.rs:0", None).is_err());
        assert!(parse_why_spec("a.rs:3-", None).is_err());
        assert!(parse_why_spec("a.rs:1-2-3", None).is_err());
        // Given twice is refused.
        assert!(parse_why_spec("a.rs:2", Some("3")).is_err());
    }

    /// The prompt block's neutralizer: controls become text, the bound holds,
    /// and the newline survives (the caller indents each line itself).
    #[test]
    fn the_prompt_excerpt_escapes_controls_and_keeps_newlines() {
        let s = "fix\u{1b}[31m the build\nplease \u{202e}now";
        let out = safe_excerpt(s, 600);
        assert!(!out.chars().any(|c| c.is_control() && c != '\n'), "{out:?}");
        assert!(out.contains('\n'));
        assert!(out.contains("\\u{1b}"), "{out:?}");
        let long = safe_excerpt(&"x".repeat(700), 600);
        assert!(long.chars().count() <= 601 && long.ends_with('…'));
    }

    /// Prompt text extraction over the shapes captured dialects actually
    /// write; a shape with no string field falls back to the raw record.
    #[test]
    fn prompt_text_is_extracted_across_dialect_shapes() {
        let s = |j: serde_json::Value| extract_prompt_text(&j);
        assert_eq!(
            s(serde_json::json!({"type":"user","message":"hi"})).as_deref(),
            Some("hi")
        );
        assert_eq!(
            s(serde_json::json!({"message":{"content":"hey"}})).as_deref(),
            Some("hey")
        );
        assert_eq!(
            s(serde_json::json!({"message":{"content":[
                {"type":"text","text":"a"},{"type":"text","text":"b"}]}}))
            .as_deref(),
            Some("a\nb")
        );
        assert_eq!(
            s(serde_json::json!({"payload":{"text":"p"}})).as_deref(),
            Some("p")
        );
        assert_eq!(s(serde_json::json!({"type":"user","message":[1]})), None);
    }

    /// Rw2's misspeak, pinned at the renderer: a shard that DOES carry a
    /// transcript but holds no prompt record must not be described as
    /// "pre-transcript era" — that sentence is reserved for the shard that
    /// archives no transcript at all. Both absences serialize as `null`, so
    /// the locked `--json` contracts do not move.
    #[test]
    fn the_two_prompt_absences_render_distinct_lines() {
        let row = Resolved {
            span: Span {
                start: 1,
                end: 1,
                commit: "c".repeat(40),
                path: "a.rs".into(),
            },
            selection: Some(Selection {
                checkpoint_id: "ab".repeat(32),
                basis: SelectionBasis::FileTouched,
                considered: 1,
                resolved: true,
            }),
            counts: None,
            sessions: vec![],
        };
        let view = |prompt: ShardPrompt| WhyView {
            file: "a.rs",
            line: Some(1),
            row: &row,
            checkpoint: None,
            prompt: Some(prompt),
            cost: None,
            file_handle: None,
            reads: Reads::default(),
        };
        let with_transcript = render_why(&view(ShardPrompt::NoPromptRecord));
        assert!(
            with_transcript
                .human
                .contains("the archived transcript holds no prompt record"),
            "{}",
            with_transcript.human
        );
        assert!(
            !with_transcript.human.contains("pre-transcript era"),
            "a transcript-carrying shard must not be called pre-transcript: {}",
            with_transcript.human
        );
        assert_eq!(with_transcript.json["prompt"], serde_json::Value::Null);

        let without = render_why(&view(ShardPrompt::NoTranscript));
        assert!(
            without
                .human
                .contains("pre-transcript era, or shard unavailable"),
            "{}",
            without.human
        );
        assert_eq!(without.json["prompt"], serde_json::Value::Null);
    }

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

    fn porcelain(entries: &[(&str, &str, &str)]) -> Vec<u8> {
        let mut s = String::new();
        for (i, (oid, path, content)) in entries.iter().enumerate() {
            let n = i + 1;
            s.push_str(&format!("{oid} {n} {n} 1\n"));
            s.push_str("author Someone <hostile@example.invalid>\n");
            s.push_str("summary feat: \u{1b}[31mnot rendered\u{1b}[0m\n");
            s.push_str(&format!("filename {path}\n"));
            s.push_str(&format!("\t{content}\n"));
        }
        s.into_bytes()
    }

    fn oid(c: char) -> String {
        std::iter::repeat_n(c, 40).collect()
    }

    #[test]
    fn porcelain_yields_one_entry_per_line_and_drops_the_untrusted_headers() {
        let out = porcelain(&[
            (&oid('a'), "src/lib.rs", "fn a() {}"),
            (&oid('a'), "src/lib.rs", "fn b() {}"),
            (&oid('b'), "src/old.rs", "fn c() {}"),
        ]);
        let lines = parse_line_porcelain(&out);
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].commit, oid('a'));
        assert_eq!(lines[2].path, "src/old.rs");
        // Author and summary were read past, not captured: nothing downstream
        // can render them by accident.
        assert!(!format!("{lines:?}").contains("hostile"));
        assert!(!format!("{lines:?}").contains("not rendered"));
    }

    #[test]
    fn a_line_whose_content_looks_like_a_header_does_not_become_one() {
        // The content of a blamed line is tab-prefixed, so a file containing
        // what looks like porcelain must not add entries.
        let mut out = porcelain(&[(&oid('a'), "src/lib.rs", "x")]);
        out.extend_from_slice(
            format!(
                "{} 2 2 1\nfilename src/lib.rs\n\t{} 9 9 1\n",
                oid('b'),
                oid('c')
            )
            .as_bytes(),
        );
        let lines = parse_line_porcelain(&out);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[1].commit, oid('b'));
    }

    /// A provenance tool that silently answers "nothing" is worse than one that
    /// errors. Accepting only 40-character oids did exactly that on a real
    /// sha256 repository: `rift blame f.rs` printed "0 lines across 0 commits"
    /// for a three-line, one-commit file, because every porcelain header was
    /// 64 characters wide and none of them matched. `rift_git::validate_oid`
    /// has accepted `40 | 64` all along, so the parser was the only disagreeing
    /// half. Asserted against a real `--object-format=sha256` repository in
    /// `tests/cli_e2e.rs`; this is the parser's half.
    #[test]
    fn a_sha256_repository_is_answered_rather_than_reported_empty() {
        let long = |c: char| -> String { std::iter::repeat_n(c, 64).collect() };
        let lines = parse_line_porcelain(&porcelain(&[
            (&long('a'), "src/lib.rs", "1"),
            (&long('a'), "src/lib.rs", "2"),
            (&long('b'), "src/lib.rs", "3"),
        ]));
        assert_eq!(lines.len(), 3, "sha256 oids are oids");
        assert_eq!(lines[2].commit, long('b'));
        assert_eq!(spans(&lines).len(), 2);
        // The null oid is as wide as the repository's hash, so "not committed"
        // cannot be a comparison against one fixed constant.
        for width in [40, 64] {
            let zeros: String = std::iter::repeat_n('0', width).collect();
            assert!(is_uncommitted(&zeros), "{width}-wide null oid");
            let rows = [Resolved {
                span: Span {
                    start: 1,
                    end: 2,
                    commit: zeros,
                    path: "src/lib.rs".to_owned(),
                },
                selection: None,
                counts: None,
                sessions: Vec::new(),
            }];
            let out = render_whole(
                "src/lib.rs",
                &rows,
                &HashMap::new(),
                &HashMap::new(),
                Reads::default(),
            );
            assert!(out.human.contains("not committed"), "{}", out.human);
            // …and an uncommitted span is not counted as a commit.
            assert_eq!(out.json["summary"]["commits"], 0);
        }
        assert!(!is_uncommitted(&long('a')));
        assert!(
            !is_uncommitted(""),
            "an absent oid is not an uncommitted one"
        );
    }

    /// The parse boundary's fail-safe, and the class it deliberately leaves
    /// alone — which is the half a `char::is_control` check got wrong.
    ///
    /// An **ASCII** control character in a `filename` value means git's
    /// unconditional quoting did not happen, so the value is not vouched for as
    /// a whole path: it keeps its line and loses its name, and an empty path
    /// matches no recorded file set, so it can never read as `file-touched`.
    ///
    /// A **C1** control (`U+0080..=U+009F`) is the opposite: it is ≥ 0x80, so
    /// arriving raw is precisely what `core.quotePath=false` asked git for
    /// (measured on git 2.50.1, asserted end to end in `tests/cli_e2e.rs`). The
    /// earlier check folded the two together, so `g\u{85}.rs` — a path git
    /// spelled in full and correctly — lost its file evidence and degraded to a
    /// guess, silently, for a reason that was never about it.
    #[test]
    fn the_fail_safe_fires_on_the_class_git_promised_to_quote_and_not_on_c1() {
        let c1 = "src/g\u{85}.rs";
        let mut out = String::new();
        out.push_str(&format!("{} 1 1 1\n", oid('a')));
        out.push_str("filename src/ok.rs\n\tone\n");
        out.push_str(&format!("{} 2 2 1\n", oid('a')));
        out.push_str("filename src/e\u{1b}[31mvil.rs\n\ttwo\n");
        out.push_str(&format!("{} 3 3 1\n", oid('a')));
        out.push_str(&format!("filename {c1}\n\tthree\n"));
        let lines = parse_line_porcelain(out.as_bytes());
        assert_eq!(lines.len(), 3, "line numbering is preserved");
        assert_eq!(lines[0].path, "src/ok.rs");
        assert_eq!(lines[1].path, "", "an unvouched-for path is no path");
        assert_eq!(lines[2].path, c1, "C1 is delivered raw on purpose");

        // No path is no evidence: with a file set recorded, the answer is a
        // labelled guess rather than the `file-touched` it could have forged.
        let excluded = [CheckpointCandidate {
            checkpoint_id: "abc1234def567".to_owned(),
            ordinal: 0,
            created_at_ms: Some(1),
            files_touched: vec!["src/e\u{1b}[31mvil.rs".to_owned()],
        }];
        let sel = select_for_file(&excluded, &lines[1].path).expect("a candidate");
        assert_eq!(sel.basis, SelectionBasis::PathExcluded);

        // …and a path git *did* vouch for gets the evidence it is entitled to.
        let recorded = [CheckpointCandidate {
            checkpoint_id: "abc1234def567".to_owned(),
            ordinal: 0,
            created_at_ms: Some(1),
            files_touched: vec![c1.to_owned()],
        }];
        let sel = select_for_file(&recorded, &lines[2].path).expect("a candidate");
        assert_eq!(sel.basis, SelectionBasis::FileTouched);

        // The escaping the renderer would apply if any of this were ever
        // printed: `safe_path` covers both classes and the two Unicode line
        // separators that are not `Cc` at all, so nothing here depends on the
        // parser having filtered first.
        for c in [
            '\u{7f}', '\u{85}', '\u{9b}', '\u{2028}', '\u{2029}', '\u{1b}',
        ] {
            let rendered = safe_path(&format!("a{c}.rs"));
            assert!(!rendered.contains(c), "U+{:04X}: {rendered}", c as u32);
        }
    }

    /// The code points `paths_contain` cannot tell from nothing, written down
    /// once so the test below can iterate them — and pinned by the sweep in
    /// [`the_collision_class_is_whatever_the_sweep_finds`], which is what stops
    /// this list from being a list someone believed.
    const COLLIDING: [char; 25] = [
        '\u{9}', '\u{a}', '\u{b}', '\u{c}', '\u{d}', '\u{20}', '\u{85}', '\u{a0}', '\u{1680}',
        '\u{2000}', '\u{2001}', '\u{2002}', '\u{2003}', '\u{2004}', '\u{2005}', '\u{2006}',
        '\u{2007}', '\u{2008}', '\u{2009}', '\u{200a}', '\u{2028}', '\u{2029}', '\u{202f}',
        '\u{205f}', '\u{3000}',
    ];

    /// The class is closed by measurement, not by enumeration.
    ///
    /// `paths_contain` compares `normalize_path`ed strings and that
    /// normalization trims, folds `\` to `/` and strips a leading `/` or `./`,
    /// so a name differing from another only in those respects is
    /// indistinguishable from a *different* file. Every scalar value to
    /// `0x10FFFF` is tried at both ends of a name: the trailing collisions come
    /// to exactly 25 — exactly `char::is_whitespace` and exactly [`COLLIDING`] —
    /// and the leading ones to 27, `/` and `\` joining through the prefix-strip
    /// and the fold. A code point that starts colliding tomorrow turns this red
    /// without anyone having edited a list.
    #[test]
    fn the_collision_class_is_whatever_the_sweep_finds() {
        let base = ["src/a.rs".to_owned()];
        let collides =
            |name: &str| rift_shadow::attribution::provenance::paths_contain(&base, name);
        let (mut trailing, mut leading) = (Vec::new(), Vec::new());
        let mut name = String::new();
        for cp in 0..=0x0010_FFFF_u32 {
            let Some(c) = char::from_u32(cp) else {
                continue;
            };
            name.clear();
            name.push_str("src/a.rs");
            name.push(c);
            if collides(&name) {
                trailing.push(c);
            }
            name.clear();
            name.push(c);
            name.push_str("src/a.rs");
            if collides(&name) {
                leading.push(c);
            }
        }
        assert_eq!(trailing, COLLIDING, "trailing collisions");
        assert!(
            trailing
                .iter()
                .copied()
                .eq(('\0'..=char::MAX).filter(|c| c.is_whitespace())),
            "the trailing class is exactly `char::is_whitespace`: {trailing:?}"
        );
        let mut expected_leading = trailing.clone();
        expected_leading.extend(['/', '\\']);
        expected_leading.sort_unstable();
        assert_eq!(leading.len(), 27, "leading collisions: {leading:?}");
        assert_eq!(leading, expected_leading, "leading collisions");
        // A multi-character run collides too, so the guard cannot be a
        // single-character check.
        assert!(collides("src/a.rs\u{20}\u{9}\u{3000}\u{85}"));
        // The opposite class, recorded because it is open and costs evidence
        // rather than forging it: two normal forms of the same name do *not*
        // collide, so a projection holding one and a filesystem the other
        // silently loses a real file's evidence. Not this boundary's to fix.
        assert!(!rift_shadow::attribution::provenance::paths_contain(
            &["caf\u{e9}.rs".to_owned()],
            "cafe\u{301}.rs"
        ));
    }

    /// Every member of the class, through the parser — with the collision
    /// asserted alongside, so the test cannot go green because the collision
    /// quietly stopped existing.
    ///
    /// Without the guard each of these resolves to `file-touched`, the basis
    /// reserved for recorded evidence, from a checkpoint that never saw that
    /// path. *Which* of them git can deliver, and who made them deliverable, is
    /// the subject of
    /// `the_quote_path_flag_this_verb_adds_is_what_makes_nineteen_collisions_reachable`
    /// in `tests/cli_e2e.rs`: 20 arrive raw under the `core.quotePath=false`
    /// this verb passes, exactly one under git's default.
    ///
    /// The guard's own cost is asserted here too, because "nothing" is the
    /// convenient answer and is not the true one: a checkpoint that really
    /// recorded a whitespace-suffixed path gets a labelled guess for it rather
    /// than the evidence it looks entitled to. Unavoidable — the matcher cannot
    /// tell that name from the different, real one — but not free, and not
    /// something to leave to a reader to discover.
    #[test]
    fn a_name_the_matcher_cannot_tell_from_another_grounds_no_evidence() {
        let recorded = [CheckpointCandidate {
            checkpoint_id: "abc1234def567".to_owned(),
            ordinal: 0,
            created_at_ms: Some(1),
            files_touched: vec!["src/a.rs".to_owned()],
        }];
        for suffix in COLLIDING {
            // Two of the 25 cannot reach the guard as a raw suffix at all,
            // because the line framing consumes them first: `LF` ends the
            // porcelain line, and `str::lines` strips a trailing `CR`. Both are
            // c-quoted by git unconditionally under either `core.quotePath`
            // (measured in `tests/cli_e2e.rs`), so the class is empty in
            // practice — and the `CR` residual is pinned below rather than left
            // to be discovered.
            if suffix == '\n' || suffix == '\r' {
                continue;
            }
            // The ambiguity itself, asserted where it actually lives, so this
            // test cannot pass because the collision stopped existing: the
            // matcher really does read these two names as one file.
            assert!(
                rift_shadow::attribution::provenance::paths_contain(
                    &["src/a.rs".to_owned()],
                    &format!("src/a.rs{suffix}")
                ),
                "U+{:04X} no longer collides — reconsider the guard",
                suffix as u32
            );
            let mut out = String::new();
            out.push_str(&format!("{} 1 1 1\n", oid('a')));
            out.push_str(&format!("filename src/a.rs{suffix}\n\tone\n"));
            let lines = parse_line_porcelain(out.as_bytes());
            assert_eq!(lines.len(), 1, "the line survives");
            assert_eq!(
                lines[0].path, "",
                "U+{:04X} is trimmed away by the matcher",
                suffix as u32
            );
            let sel = select_for_file(&recorded, &lines[0].path).expect("a candidate");
            assert_eq!(
                sel.basis,
                SelectionBasis::PathExcluded,
                "U+{:04X} became evidence",
                suffix as u32
            );
        }
        // THE RESIDUAL, stated as a test so that changing how this parser
        // splits lines has to face it: a `CR`-suffixed name reaches the guard
        // already stripped, so it grounds evidence for the *different* file
        // `src/a.rs`. Unreachable only because git c-quotes `CR`
        // unconditionally — the same kind of dependency as the newline one,
        // and asserted against real git for the same reason.
        let mut out = String::new();
        out.push_str(&format!("{} 1 1 1\n", oid('a')));
        out.push_str("filename src/a.rs\r\n\tone\n");
        let lines = parse_line_porcelain(out.as_bytes());
        assert_eq!(
            lines[0].path, "src/a.rs",
            "`str::lines` strips the CR before the guard can see it"
        );

        // WHAT THE GUARD COSTS, asserted rather than asserted away. This
        // checkpoint really did record the whitespace-suffixed path, and git
        // really did deliver it — no attacker anywhere — and the answer is a
        // labelled guess rather than the `file-touched` it looks entitled to.
        // Unavoidable, because `paths_contain` cannot tell that name from the
        // different, real `src/a.rs`, so the evidence would be a coin flip
        // printed as a fact. Not free, and not left for a reader to find.
        let genuinely = [CheckpointCandidate {
            checkpoint_id: "abc1234def567".to_owned(),
            ordinal: 0,
            created_at_ms: Some(1),
            files_touched: vec!["src/a.rs\u{3000}".to_owned()],
        }];
        let mut out = String::new();
        out.push_str(&format!("{} 1 1 1\n", oid('a')));
        out.push_str("filename src/a.rs\u{3000}\n\tone\n");
        let lines = parse_line_porcelain(out.as_bytes());
        assert_eq!(lines[0].path, "");
        assert_eq!(
            select_for_file(&genuinely, &lines[0].path)
                .expect("a candidate")
                .basis,
            SelectionBasis::PathExcluded,
            "a real file with a real whitespace name loses real evidence here"
        );

        // A leading one is the same ambiguity, and an ordinary name — every path
        // git can produce for an ordinary repository — is untouched, which is
        // the claim the paragraph above is narrow about.
        let mut out = String::new();
        out.push_str(&format!("{} 1 1 1\n", oid('a')));
        out.push_str("filename  src/a.rs\n\tone\n");
        out.push_str(&format!("{} 2 2 1\n", oid('a')));
        out.push_str("filename src/a.rs\n\ttwo\n");
        let lines = parse_line_porcelain(out.as_bytes());
        assert_eq!(lines[0].path, "", "a leading space is the same ambiguity");
        assert_eq!(lines[1].path, "src/a.rs");
        assert_eq!(
            select_for_file(&recorded, &lines[1].path)
                .expect("a candidate")
                .basis,
            SelectionBasis::FileTouched,
            "the real name still resolves"
        );
    }

    /// The asymmetry the C0 fail-safe left open, closed — and the one thing the
    /// closure must not cost.
    ///
    /// `"` and `\` are c-quoted whatever `core.quotePath` says, exactly like the
    /// C0 class, so either of them in a value git delivered *as itself* is the
    /// same broken framing. The backslash is the one that also forges: the
    /// matcher folds it to `/`, so `src/a\b.rs` reads as the different, real
    /// file `src/a/b.rs` — asserted below at the matcher, so this test cannot
    /// pass because the fold quietly went away.
    ///
    /// And the cost that would have made the closure wrong: a **c-quoted** value
    /// keeps its spelling. Refusing every backslash would refuse every quoted
    /// value, which is what `tests/cli_e2e.rs` needs in order to assert that git
    /// quoted the whole name onto one line at all.
    #[test]
    fn an_unquoted_value_holding_a_byte_git_always_escapes_grounds_no_evidence() {
        let recorded = [CheckpointCandidate {
            checkpoint_id: "abc1234def567".to_owned(),
            ordinal: 0,
            created_at_ms: Some(1),
            files_touched: vec!["src/a/b.rs".to_owned()],
        }];
        // The collisions themselves, at the matcher.
        for name in ["src/a\\b.rs", "./src/a/b.rs", "/src/a/b.rs"] {
            assert!(
                rift_shadow::attribution::provenance::paths_contain(
                    &["src/a/b.rs".to_owned()],
                    name
                ),
                "{name} no longer collides — reconsider the guard"
            );
        }
        // Each refused, and refused as a *path* rather than as a line.
        for name in ["src/a\\b.rs", "src/a\"b.rs", "./src/a/b.rs", "/src/a/b.rs"] {
            let mut out = String::new();
            out.push_str(&format!("{} 1 1 1\n", oid('a')));
            out.push_str(&format!("filename {name}\n\tone\n"));
            let lines = parse_line_porcelain(out.as_bytes());
            assert_eq!(lines.len(), 1, "{name}: the line survives");
            assert_eq!(lines[0].path, "", "{name} kept its path");
            assert_eq!(
                select_for_file(&recorded, &lines[0].path)
                    .expect("a candidate")
                    .basis,
                SelectionBasis::PathExcluded,
                "{name} became evidence"
            );
        }
        // A c-quoted value is not refused for the backslashes it is made of.
        let mut out = String::new();
        out.push_str(&format!("{} 1 1 1\n", oid('a')));
        out.push_str("filename \"b.rs\\nfilename hijacked.rs\"\n\tone\n");
        out.push_str(&format!("{} 2 2 1\n", oid('a')));
        out.push_str("filename src/a/b.rs\n\ttwo\n");
        let lines = parse_line_porcelain(out.as_bytes());
        assert_eq!(
            lines[0].path, "\"b.rs\\nfilename hijacked.rs\"",
            "a quoted value keeps the spelling git gave it"
        );
        // …and an ordinary name still resolves, so the guard costs nothing.
        assert_eq!(lines[1].path, "src/a/b.rs");
        assert_eq!(
            select_for_file(&recorded, &lines[1].path)
                .expect("a candidate")
                .basis,
            SelectionBasis::FileTouched
        );
    }

    /// What a git that stopped c-quoting would actually cost, so the e2e
    /// assertion on the quoted spelling is not merely one of several answers
    /// that would pass.
    ///
    /// These are the bytes such a git would emit for a file named
    /// `b.rs\nfilename hijacked.rs` — and nothing in this parser can tell the
    /// second header from a real one, because by then it *is* a real header.
    /// That is the whole reason `tests/cli_e2e.rs` asserts git's quoting
    /// positively instead of asserting that `hijacked.rs` is absent.
    #[test]
    fn a_raw_newline_in_a_filename_value_would_choose_the_attackers_path() {
        let mut out = String::new();
        out.push_str(&format!("{} 1 1 1\n", oid('a')));
        out.push_str("filename b.rs\nfilename hijacked.rs\n\tone\n");
        let lines = parse_line_porcelain(out.as_bytes());
        assert_eq!(lines.len(), 1, "one blamed line either way");
        assert_eq!(lines[0].path, "hijacked.rs", "the later header wins");
    }

    #[test]
    fn adjacent_lines_with_one_answer_collapse_into_one_span() {
        let lines = parse_line_porcelain(&porcelain(&[
            (&oid('a'), "src/lib.rs", "1"),
            (&oid('a'), "src/lib.rs", "2"),
            (&oid('b'), "src/lib.rs", "3"),
            (&oid('a'), "src/lib.rs", "4"),
        ]));
        let got = spans(&lines);
        assert_eq!(got.len(), 3);
        assert_eq!((got[0].start, got[0].end), (1, 2));
        assert_eq!((got[1].start, got[1].end), (3, 3));
        assert_eq!((got[2].start, got[2].end), (4, 4));
        // A rename splits a span even when the commit is the same.
        let renamed = spans(&parse_line_porcelain(&porcelain(&[
            (&oid('a'), "new.rs", "1"),
            (&oid('a'), "old.rs", "2"),
        ])));
        assert_eq!(renamed.len(), 2);
    }

    #[test]
    fn a_tag_is_never_stronger_than_the_counts_behind_it() {
        assert_eq!(Tag::of(None), Tag::Unknown);
        assert_eq!(Tag::of(Some(&AttrCounts::default())), Tag::Unknown);
        let ai = AttrCounts {
            agent_added: 4,
            ..AttrCounts::default()
        };
        assert_eq!(Tag::of(Some(&ai)), Tag::Ai);
        let human = AttrCounts {
            human_added: 4,
            ..AttrCounts::default()
        };
        assert_eq!(Tag::of(Some(&human)), Tag::Human);
        // Both present, or a human edit over agent lines: per-file counts
        // cannot name the author of a single line.
        for c in [
            AttrCounts {
                agent_added: 4,
                human_added: 1,
                ..AttrCounts::default()
            },
            AttrCounts {
                agent_added: 4,
                human_modified_agent: 1,
                ..AttrCounts::default()
            },
        ] {
            assert_eq!(Tag::of(Some(&c)), Tag::Mixed);
        }
    }

    #[test]
    fn a_hostile_label_cannot_reach_the_terminal_intact() {
        let evil = "claude\u{1b}[2Jcode\nrm -rf /";
        let safe = Label::of(evil).to_string();
        assert!(!safe.contains('\u{1b}'));
        assert!(!safe.contains('\n'));
        assert!(!safe.contains(' '));
        assert_eq!(Label::of("claude-code").to_string(), "claude-code");
        // Bounded: a very long model id cannot reflow a terminal.
        let long = Label::of(&"m".repeat(400)).to_string();
        assert_eq!(long.chars().count(), MAX_LABEL + 1);
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

    /// A projection with the v14 relations, and the two rows every worktree
    /// test here needs seeded through the production writers.
    fn worktree_fixture() -> (
        tempfile::TempDir,
        SqliteStore,
        tokio::runtime::Runtime,
        String,
    ) {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = rift_store::open_capture_projection(dir.path().join("projection.db"))
            .expect("open projection");
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let labelled: String = std::iter::repeat_n('a', 64).collect();
        rt.block_on(rift_store::record_checkpoint_worktree_if_new(
            &store,
            &labelled,
            "aabbccddeeff",
            "swt-d",
            true,
            Some("swt/d-surfaces"),
        ))
        .expect("checkpoint_worktree row");
        (dir, store, rt, labelled)
    }

    /// One `session_worktree` observation through the production writer.
    fn observe(
        store: &SqliteStore,
        rt: &tokio::runtime::Runtime,
        session: &str,
        name: &str,
        branch: Option<&str>,
        at: i64,
    ) {
        rt.block_on(rift_store::record_session_worktree(
            store,
            &rift_store::SessionWorktreeRow {
                observation_id: rift_shadow::ids::session_worktree_observation_id(
                    session,
                    "aabbccddeeff",
                    branch,
                    None,
                    None,
                ),
                session_id: session.to_owned(),
                worktree_token: "aabbccddeeff".to_owned(),
                worktree_name: name.to_owned(),
                is_linked: i64::from(name != "main"),
                work_tree: format!("/tmp/{name}"),
                branch: branch.map(str::to_owned),
                head_oid: None,
                subagent_id: None,
                basis: "payload_cwd".to_owned(),
                first_observed_at: at,
                schema_version: 1,
            },
        ))
        .expect("session_worktree row");
    }

    /// ★ The rendering clause at the reader: a checkpoint the
    /// projection has no row for is `unknown`, and a checkpoint it does have a
    /// row for is that member — so the absence can never be read as `main`.
    #[test]
    fn a_checkpoint_with_no_row_renders_unknown_and_never_main() {
        let (_dir, store, rt, labelled) = worktree_fixture();
        let bare: String = std::iter::repeat_n('b', 64).collect();

        let named = rt.block_on(checkpoint_worktree(&store, &labelled));
        assert_eq!(worktree_label(named.as_ref()), "swt-d");

        let absent = rt.block_on(checkpoint_worktree(&store, &bare));
        assert_eq!(absent, None, "no row is no fact, not a default");
        assert_eq!(worktree_label(absent.as_ref()), UNKNOWN_WORKTREE);
        assert_ne!(
            worktree_label(absent.as_ref()),
            "main",
            "back-filling the member is forbidden; naming main would invent it"
        );
        // …and the spelling is the dashboard's, so one word means one thing.
        assert_eq!(UNKNOWN_WORKTREE, rift_canopy::col3::UNKNOWN_WORKTREE);
    }

    /// A session id can be observed in several checkouts over its
    /// life, the ordered set is the history, and the **latest** observation is
    /// its current worktree.
    #[test]
    fn the_latest_observation_is_the_sessions_current_worktree() {
        let (_dir, store, rt, _labelled) = worktree_fixture();
        observe(&store, &rt, "s1", "main", Some("main"), 1_000);
        observe(&store, &rt, "s1", "swt-d", Some("swt/d-surfaces"), 2_000);

        let history = rt.block_on(session_worktree_history(&store, "s1"));
        let names: Vec<String> = history.iter().map(|o| o.name.to_string()).collect();
        assert_eq!(names, vec!["main", "swt-d"], "oldest first");
        assert_eq!(
            history.first().map(|o| o.basis.to_string()),
            Some("payload_cwd".to_owned()),
            "the basis rides so an auditor sees how the observation was decided"
        );
        let current = rt.block_on(session_worktree(&store, "s1"));
        assert_eq!(worktree_label(current.as_ref()), "swt-d");

        // A session nothing ever observed is unknown at both readers.
        assert!(rt
            .block_on(session_worktree_history(&store, "s2"))
            .is_empty());
        assert_eq!(
            worktree_label(rt.block_on(session_worktree(&store, "s2")).as_ref()),
            UNKNOWN_WORKTREE
        );
    }

    /// ★ A projection that predates v14 has neither relation. Every reader here
    /// is TOTAL over that: the verb answers, the label is `unknown`, and
    /// nothing propagates an error out of a provenance surface over a badge.
    #[test]
    fn a_pre_v14_projection_costs_the_label_and_not_the_verb() {
        let (_dir, store, rt, labelled) = worktree_fixture();
        observe(&store, &rt, "s1", "swt-d", None, 1);
        // Both relations gone — the shape of every projection still on v13.
        rt.block_on(async {
            let conn = rift_store::Store::conn(&store).await?;
            conn.execute("DROP TABLE checkpoint_worktree", &[]).await?;
            conn.execute("DROP TABLE session_worktree", &[]).await
        })
        .expect("drop the v14 relations");
        assert_eq!(rt.block_on(checkpoint_worktree(&store, &labelled)), None);
        assert!(rt
            .block_on(session_worktree_history(&store, "s1"))
            .is_empty());
        assert_eq!(rt.block_on(session_worktree(&store, "s1")), None);
        assert_eq!(
            rt.block_on(worktree_spread(&store, &[labelled])),
            vec![UNKNOWN_WORKTREE.to_owned()]
        );
    }

    /// The unknown rule for a WINDOW verb: the spread names every member the
    /// window's checkpoints were archived in and carries `unknown` exactly when
    /// one of them records none — so a mixed window cannot be read as if it
    /// happened in one place.
    #[test]
    fn the_spread_names_every_member_and_marks_the_absent_ones() {
        let (_dir, store, rt, labelled) = worktree_fixture();
        let bare: String = std::iter::repeat_n('b', 64).collect();
        assert_eq!(
            rt.block_on(worktree_spread(&store, &[labelled.clone(), bare.clone()])),
            vec!["swt-d".to_owned(), UNKNOWN_WORKTREE.to_owned()]
        );
        assert_eq!(
            rt.block_on(worktree_spread(&store, &[labelled.clone(), labelled])),
            vec!["swt-d".to_owned()],
            "distinct members, not one row per checkpoint"
        );
        // An empty window has nothing to name, and the human line says so
        // rather than printing an empty list.
        assert!(rt.block_on(worktree_spread(&store, &[])).is_empty());
        assert_eq!(spread_text(&[]), UNKNOWN_WORKTREE);
        assert_eq!(
            spread_text(&["main".to_owned(), "swt-d".to_owned()]),
            "main, swt-d"
        );
    }

    /// The claim this verb exists to make good on, stated so it can fail **at
    /// every level**.
    ///
    /// 300 lines alternating between 3 commits is the worst case for span
    /// collapsing — every line is its own span — so if any read were per line
    /// this would do 300 of them. It does 3 message reads, 1 checkpoint read
    /// and 1 per-file attribution read.
    ///
    /// The first cut of this test asserted `facts.len() == 1`, and that assertion
    /// **cannot fail**: it is the size of the returned map, which has one entry
    /// per distinct key whether it was filled once or three hundred times. Only
    /// the candidate cache was really pinned; defeating `fact_cache` or
    /// `count_cache` left the suite green. The counts below are incremented at
    /// the read sites themselves, so each cache has a number that a mutation to
    /// it moves — verified by mutating all three.
    #[test]
    fn every_cache_this_verb_relies_on_has_a_number_that_can_move() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = rift_store::open_capture_projection(dir.path().join("projection.db"))
            .expect("open projection");
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");

        let ckpt: String = std::iter::repeat_n('d', 64).collect();
        let lines: Vec<BlamedLine> = (0..300)
            .map(|i| BlamedLine {
                commit: oid(['a', 'b', 'c'][i % 3]),
                path: "src/lib.rs".to_owned(),
            })
            .collect();
        let sp = spans(&lines);
        assert_eq!(sp.len(), 300, "no two adjacent lines share a commit");

        let calls = std::cell::Cell::new(0usize);
        let r = rt
            .block_on(resolve(&store, sp, |_oid| {
                calls.set(calls.get() + 1);
                Ok(parse_checkpoint_claims(&format!(
                    "s\n\nRift-Checkpoint: {ckpt}\n"
                )))
            }))
            .expect("resolve");

        assert_eq!(r.rows.len(), 300);
        assert_eq!(
            r.reads,
            Reads {
                messages: 3,
                facts: 1,
                counts: 1,
            },
            "300 spans, 3 commits, 1 checkpoint, 1 path"
        );
        // The injected closure counts the same reads from outside the function,
        // so the two disagreeing would mean the counter, not the cache, moved.
        assert_eq!(calls.get(), r.reads.messages);
        // The maps are still returned for the renderer, and their SIZES are
        // exactly what the old assertion measured — kept here to show they do
        // not distinguish a working cache from a broken one.
        assert_eq!(r.claims.len(), 3);
        assert_eq!(r.facts.len(), 1);
        // Every span still gets an answer: caching is not skipping.
        assert!(r.rows.iter().all(|x| x.selection.is_some()));
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

    #[test]
    fn the_json_envelope_is_the_contract() {
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
        let out = render_whole("src/lib.rs", &rows, &claims, &facts, Reads::default());
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
        // `Selection` is composed, not re-spelled: its own field names and its
        // snake_case basis rename are the contract.
        assert_eq!(spans[0]["selection"]["basis"], "file_touched");
        assert_eq!(spans[0]["selection"]["considered"], 3);
        assert!(spans[1]["selection"].is_null());
        // `ClaimSet` appears once per commit, not once per span.
        assert_eq!(v["commits"][oid('a')]["claims"][0]["ordinal"], 0);
        assert_eq!(v["commits"][oid('a')]["rejected"], 0);
        assert_eq!(v["checkpoints"]["abc1234def567"]["created_at_ms"], 42);
        assert_eq!(v["summary"]["lines_with_file_evidence"], 8);
        assert_eq!(v["summary"]["commits"], 1);
        // The NESTED leaves too. Locking only the span-level keys left every
        // composed type free to rename a field without a test noticing, which
        // for a contract an agent parses is the same exposure one level down.
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
        assert_eq!(spans[0]["selection"]["checkpoint_id"], "abc1234def567");
        assert_eq!(spans[0]["selection"]["resolved"], true);
        assert_eq!(v["commits"][oid('a')]["claims"][0]["dialect"], "rift");
        assert_eq!(
            v["commits"][oid('a')]["claims"][0]["checkpoint_id"],
            "abc1234def567"
        );
        assert_eq!(v["commits"][oid('a')]["truncated"], false);
        assert_eq!(v["checkpoints"]["abc1234def567"]["files_touched"], 1);
        // What answering cost, in reads.
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
