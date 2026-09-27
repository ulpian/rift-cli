//! **The window plane**: one report window over the capture projection, and
//! every fact that window yields.
//!
//! # Why the query core lives in the storage crate
//!
//! These queries are SQL over *this crate's own* capture schema, and they have
//! two readers — `bins/rift`'s workflow verbs and `rift-canopy-data`'s
//! dashboard cards. `bins/rift` is a **binary**, so nothing can link it; the
//! dashboard therefore could not reuse a line of it, and the alternative on the
//! table was a second implementation of the same window arithmetic. Homing it
//! here adds **no crate-graph edge** — `rift-store` is already an edge of both
//! — which is the whole reason this is the answer and a new `rift-workflows`
//! crate was not (that one would have created a `rift-canopy-data` cycle and
//! dragged `rift-shadow` + `rift-recall-substrate` into the dashboard's data
//! plane).
//!
//! # What is deliberately NOT here
//!
//! **Policy.** [`WindowFacts::sensitive_paths`] is the only field this module
//! never fills. Deciding whether a path is sensitive needs
//! `rift_config::EffectiveConfig`, and a `rift-store -> rift-config` edge would
//! invert the layering — storage reading project policy — on the workspace's
//! most-depended-on crate, and hand `rift-canopy-data` a policy dependency it
//! does not want. So the filter stayed in `bins/rift` and the field is left
//! `None` here, which is a fact a reader can act on rather than a zero it
//! cannot distinguish from an answer.
//!
//! **Rendering, JSON and the CLI's `Outcome`** stayed too. Nothing in this
//! module formats anything for a human or an agent; it answers with numbers,
//! identifiers, timestamps and repository paths, and what a surface may do with
//! a path is the surface's own rule, enforced where the surface
//! is.
//!
//! # The window is explicit or it is everything
//!
//! [`Window::resolve`] accepts one of two spellings and nothing else: a UTC
//! calendar day `YYYY-MM-DD`, or a canonical base-10 epoch-millisecond integer
//! (the same canonicity rule the stream cursor and the transfer container use —
//! [`crate::parse_canonical_u64`], called rather than re-derived). An omitted
//! bound is unbounded, never "now": a report whose meaning depends on when it
//! ran cannot be compared with itself. **Both bounds are inclusive, and a
//! calendar day is a period**: a day typed as `--since` opens at its midnight
//! and a day typed as `--until` closes at its last millisecond, so
//! `--since D --until D` is the whole of day `D`. Every query below carries a total
//! `ORDER BY`, so two gathers of the same window over the same projection
//! produce the same facts in the same order.
//!
//! The error messages still open with `dispatch:` because they are the CLI's
//! contract with its user and this module is a relocation, not a rewrite: a
//! move that changes what the product prints is not a move.
//!
//! # A checkpoint whose time is unknown
//!
//! `checkpoint.created_at` carries [`CREATED_AT_UNKNOWN`] (`-1`) whenever the
//! shard records no time of its own — which is **every native shard**, because
//! native `metadata.json` has no time field. A window filter that compares on
//! that column therefore answers zero on a repository full of checkpoints, and
//! the first version of this code did exactly that: 103 checkpoints and 178
//! attribution rows reported as `checkpoints: 0`, `files: []`.
//!
//! The fix is one derivation, in one place ([`CHECKPOINT_TIME_CTE`]), used by
//! every checkpoint-anchored query here:
//!
//! * a checkpoint that RECORDS a time is at that time;
//! * a checkpoint that does not is at `MAX(session.started_at)` over the
//!   sessions it archives — the tightest **lower bound** derivable from stored
//!   facts, because a shard covering a session cannot predate that session's
//!   start. It is reported as `derived`, never as recorded;
//! * a checkpoint with neither is **unresolved**: it is outside every *bounded*
//!   window and inside the unbounded one, and it is **counted**
//!   ([`WindowFacts::unresolved_checkpoints`]).
//!
//! The exclusion is deliberate and is the half worth arguing. Including
//! unknown-time checkpoints in every bounded window would make two different
//! windows report the same set, which is the one thing a window exists to
//! prevent; excluding them *silently* is what made the verb answer zero. So:
//! excluded, and counted where the reader cannot miss it.

use std::collections::BTreeMap;
use std::path::Path;

use rift_core::{Result, RiftError};

use crate::{CanonicalStore, SqliteStore, Value};

/// `checkpoint.created_at` for a shard that records no time of its own.
///
/// **Explicitly `-1`, not `0`.** Zero is a real instant — the epoch — so a
/// projection full of zeros is indistinguishable from one full of checkpoints
/// genuinely dated 1970, and nothing reading the column can tell "unknown" from
/// "very old". This value cannot be a capture time, so a reader that sorts,
/// filters or compares on it either handles the sentinel or is visibly wrong.
///
/// Nothing derives the chain head from this column — that is
/// [`ChainLink`](rift_hashchain::ChainLink) linkage's job, and reading a head out of a
/// timestamp is exactly the shape of bug this constant exists beside.
pub const CREATED_AT_UNKNOWN: i64 = -1;

/// A resolved report window: two inclusive epoch-millisecond bounds.
///
/// Both bounds are materialized (an absent one becomes the representable
/// extreme) so every query binds exactly two parameters and no SQL is built by
/// string concatenation from user input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Window {
    since_ms: i64,
    until_ms: i64,
    /// What the caller actually typed, for the report's own echo of its scope.
    since_text: Option<String>,
    until_text: Option<String>,
}

impl Window {
    /// Resolve `--since` / `--until`.
    ///
    /// # Errors
    /// [`RiftError::Validity`] naming the reason — and **never the input** —
    /// when a bound is not one of the two accepted spellings, or when the
    /// window is empty (`until` before `since`), which is a typo rather than a
    /// question and is refused instead of silently answered with zeros.
    pub fn resolve(since: Option<&str>, until: Option<&str>) -> Result<Self> {
        let since_ms = match since {
            Some(s) => parse_instant(s, "since", Edge::Opening)?,
            None => i64::MIN,
        };
        let until_ms = match until {
            Some(s) => parse_instant(s, "until", Edge::Closing)?,
            None => i64::MAX,
        };
        if until_ms < since_ms {
            return Err(RiftError::validity(
                "dispatch: --until is before --since (the window is empty)",
            ));
        }
        Ok(Self {
            since_ms,
            until_ms,
            since_text: since.map(str::to_owned),
            until_text: until.map(str::to_owned),
        })
    }

    /// The two bind parameters every *time-carrying* windowed query takes.
    ///
    /// Public because the pair is the window's contract with SQL and it has
    /// readers outside this crate: `rift recap`'s tool-mix and serve-count
    /// queries are anchored to their own start columns and stayed in
    /// `bins/rift`, and re-spelling `?1`/`?2` there would be the second
    /// spelling this module's `const` SQL exists to prevent.
    #[must_use]
    pub fn bounds(&self) -> [Value; 2] {
        [Value::Int(self.since_ms), Value::Int(self.until_ms)]
    }

    /// The four a **checkpoint-anchored** query takes. The sentinel binds
    /// FIRST (`?1`) so that a query using [`CHECKPOINT_TIME_CTE`] without the
    /// window predicate — the repository-wide unresolved count — binds exactly
    /// one parameter and needs no second numbering.
    fn checkpoint_binds(&self) -> [Value; 4] {
        [
            sentinel_bind(),
            Value::Int(self.since_ms),
            Value::Int(self.until_ms),
            Value::Int(i64::from(self.is_unbounded())),
        ]
    }

    /// A window with neither bound named — "everything", which is the only
    /// window a checkpoint of unresolved time belongs to.
    fn is_unbounded(&self) -> bool {
        self.since_ms == i64::MIN && self.until_ms == i64::MAX
    }

    /// The window as one canonical string, for a pack key.
    ///
    /// Built from the RESOLVED bounds rather than the typed text, so
    /// `--since 1970-01-01` and `--since 0` name the same window and key the
    /// same pack — the opposite rule from a frame, where two spellings must
    /// never name one artifact, and for the opposite reason: a key identifies a
    /// question, and these two are the same question.
    #[must_use]
    pub fn key_text(&self) -> String {
        format!("{}:{}", self.since_ms, self.until_ms)
    }

    /// The resolved lower bound.
    #[must_use]
    pub const fn since_ms(&self) -> i64 {
        self.since_ms
    }

    /// The resolved upper bound.
    #[must_use]
    pub const fn until_ms(&self) -> i64 {
        self.until_ms
    }

    /// What the caller typed for `--since`, or `None` for an omitted bound.
    ///
    /// **The typed text and the resolved bound are different facts and both are
    /// exposed**, because a report echoes its scope back and an echo that says
    /// `-9223372036854775808` where the user said nothing is not an echo. The
    /// rendering of that echo is the surface's job, not this crate's.
    #[must_use]
    pub fn since_text(&self) -> Option<&str> {
        self.since_text.as_deref()
    }

    /// What the caller typed for `--until`, or `None` for an omitted bound.
    #[must_use]
    pub fn until_text(&self) -> Option<&str> {
        self.until_text.as_deref()
    }
}

/// Which end of the window an instant is being parsed for.
///
/// It matters for exactly one spelling: a **calendar day** is a period, not an
/// instant, and which of its edges the caller means depends on which bound
/// they typed it as. Epoch milliseconds are already an instant and are taken
/// verbatim at either edge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Edge {
    /// `--since`: the first millisecond of the day.
    Opening,
    /// `--until`: the last millisecond of the day.
    Closing,
}

/// One instant, in exactly one of two spellings.
///
/// The refusal names the field and the accepted spellings and quotes **nothing**
/// the caller typed: an error message is printed to a terminal, and a value
/// that reached this function is a value nobody validated.
///
/// **A day named as `--until` closes that day.** Both bounds are inclusive —
/// the help text has always said so and every query binds `<= until_ms` — so
/// resolving `--until 2026-09-14` to that day's *midnight* admitted exactly
/// the one millisecond at `00:00:00.000` and silently dropped the rest of the
/// day the caller asked for. A day is a period; the edge it resolves to is the
/// edge the bound it was typed as means.
fn parse_instant(s: &str, field: &str, edge: Edge) -> Result<i64> {
    // Epoch milliseconds, canonical — the one integer rule this tree has.
    if let Some(ms) = crate::parse_canonical_u64(s) {
        return i64::try_from(ms).map_err(|_| {
            RiftError::validity(format!(
                "dispatch: --{field} is beyond the representable range"
            ))
        });
    }
    // …or a UTC calendar day.
    if let Some(ms) = day_start_ms(s) {
        return Ok(match edge {
            Edge::Opening => ms,
            Edge::Closing => ms.saturating_add(DAY_MS - 1),
        });
    }
    Err(RiftError::validity(format!(
        "dispatch: --{field} must be a UTC day (YYYY-MM-DD) or canonical epoch milliseconds"
    )))
}

/// One UTC day in milliseconds — the width a calendar day closes over.
const DAY_MS: i64 = 86_400_000;

/// `YYYY-MM-DD` → the UTC midnight that opens that day, in epoch milliseconds.
///
/// Written out rather than pulled from a date crate: the workspace pins no date
/// library, adding a dependency is an owner decision, and civil-date
/// arithmetic for a proleptic Gregorian calendar is a closed-form days-from-era
/// computation with no ambiguity to get wrong. `None` for anything that is not
/// exactly this shape, leap years included.
fn day_start_ms(s: &str) -> Option<i64> {
    let bytes = s.as_bytes();
    if bytes.len() != 10 || bytes.get(4) != Some(&b'-') || bytes.get(7) != Some(&b'-') {
        return None;
    }
    let field = |from: usize, to: usize| -> Option<i64> {
        let part = s.get(from..to)?;
        if !part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        part.parse::<i64>().ok()
    };
    let (y, m, d) = (field(0, 4)?, field(5, 7)?, field(8, 10)?);
    if !(1..=12).contains(&m) || d < 1 || d > days_in_month(y, m) {
        return None;
    }
    // Howard Hinnant's days_from_civil, which is exact for every proleptic
    // Gregorian date and needs no table.
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    days.checked_mul(86_400_000)
}

/// Days in `m` of year `y`, Gregorian.
const fn days_in_month(y: i64, m: i64) -> i64 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 => 29,
        2 => 28,
        _ => 0,
    }
}
/// **The one derivation of a checkpoint's effective time** (see this
/// module's docs), as a CTE every checkpoint-anchored query here prefixes.
///
/// It is a `const` and not three hand-written subqueries because a sentinel
/// rule copied into three places is a sentinel rule that will be right in two
/// of them.
///
/// # The bind contract, stated because this is `pub` — and CORRECTED
///
/// This doc said *"`?3` is the unknown-time sentinel"* while the SQL below
/// has always read `?1` and `Window::checkpoint_binds` has always bound it
/// first. The sentence was wrong in `bins/rift` too, where nothing outside
/// the module could act on it; it is fixed here because a bind contract that
/// crosses a crate boundary is one another crate will follow.
///
/// Prefix it to your own `SELECT` over `ct`, which exposes `checkpoint_id`,
/// `created_at` (raw, sentinel included) and **`effective_at`** — the resolved
/// time, or `NULL` for a checkpoint no stored fact can place. Bind **`?1` =
/// [`CREATED_AT_UNKNOWN`]** and number any parameters of your own from `?2` up.
///
/// It is public because the second reader is in another crate:
/// `rift-canopy-data`'s activity strip counted `checkpoint` rows on raw
/// `created_at` for want of a derivation it could link to — the same defect
/// that once made `rift dispatch` answer `checkpoints: 0` against 103 real
/// checkpoints. A sentinel rule nothing can link to gets re-spelled.
pub const CHECKPOINT_TIME_CTE: &str = "WITH ct AS (\
     SELECT c.checkpoint_id AS checkpoint_id, c.created_at AS created_at, \
     CASE WHEN c.created_at <> ?1 THEN c.created_at ELSE \
     (SELECT MAX(s.started_at) FROM session_in_checkpoint sic \
      JOIN session s ON s.session_id = sic.session_id \
      WHERE sic.checkpoint_id = c.checkpoint_id) END AS effective_at \
     FROM checkpoint c) ";

/// The unknown-time sentinel as a bind — read from the crate that writes it,
/// never re-spelled as a literal in SQL. A second spelling of `-1` is exactly
/// how the first sentinel bug happened.
fn sentinel_bind() -> Value {
    Value::Int(CREATED_AT_UNKNOWN)
}

/// The window predicate over [`CHECKPOINT_TIME_CTE`]'s `effective_at`. Binds:
/// `?2` since, `?3` until, `?4` "this window is unbounded".
const IN_WINDOW: &str = "((ct.effective_at IS NOT NULL \
      AND ct.effective_at >= ?2 AND ct.effective_at <= ?3) \
     OR (ct.effective_at IS NULL AND ?4 = 1))";

/// One checkpoint in the window, and how its time was known.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckpointFact {
    /// The checkpoint's identifier.
    pub checkpoint_id: String,
    /// The effective time — `None` only in the unbounded window, where an
    /// unresolved checkpoint is still a member.
    pub at: Option<i64>,
    /// `true` when `at` came from the checkpoint's sessions rather than from
    /// the shard's own record. A derived time is a lower bound and is never
    /// presented as the shard's claim about itself.
    pub derived: bool,
}

/// The four token classes, summed over a window.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct TokenClasses {
    /// Input tokens billed at the uncached rate.
    pub uncached_input: i64,
    /// Input tokens served from the provider's cache.
    pub cached_input: i64,
    /// Tokens spent writing that cache.
    pub cache_creation: i64,
    /// Output tokens.
    pub output: i64,
}

impl TokenClasses {
    /// Every class summed, saturating rather than wrapping.
    #[must_use]
    pub fn total(self) -> i64 {
        self.uncached_input
            .saturating_add(self.cached_input)
            .saturating_add(self.cache_creation)
            .saturating_add(self.output)
    }
}

/// The attribution rollup over a window: the AI / MX / HU lines.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct AttributionRollup {
    /// Lines the agent added (AI).
    pub agent_lines: i64,
    /// Agent lines a human then modified (MX).
    pub mixed_lines: i64,
    /// Lines the human added (HU).
    pub human_lines: i64,
    /// Agent lines a human removed.
    pub agent_removed_by_human: i64,
    /// Mean per-file confidence in milli-units, or `None` when no file in the
    /// window carries an attribution row. **Never defaulted to a number** — an
    /// absent measurement and a measured zero are different facts.
    pub confidence_milli: Option<i64>,
}

/// Recall's ledger arithmetic over a window.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Savings {
    /// Tokens the estimator says a serve avoided. **An estimated upper bound**
    /// and never a claim: it is by construction
    /// counterfactual-free, so no "Rift saved X" statement rests on it.
    pub avoided_upper_bound: i64,
    /// Tokens spent injecting manifests — real spend, subtracted rather than
    /// credited (`memd.manifest_injected` is never credit).
    pub injected: i64,
    /// Tokens actually returned by serves.
    pub returned: i64,
}

/// Everything one window yields.
#[derive(Debug, Default)]
pub struct WindowFacts {
    /// The checkpoints in the window, in effective-time then id order.
    pub checkpoints: Vec<CheckpointFact>,
    /// Every `(checkpoint, file)` attribution row in the window, as read. The
    /// rollups below are derived from these rather than from two `GROUP BY`s,
    /// which is what lets `--branch` restrict the scope by re-deriving from a
    /// retained subset instead of growing a second aggregation.
    pub attributed: Vec<AttributedFile>,
    /// Checkpoints in this repository whose time cannot be resolved at all —
    /// outside every bounded window, and reported so that "excluded" never
    /// reads as "absent". Repository-wide, deliberately: a count scoped to a
    /// window it is by definition outside of would always be zero.
    pub unresolved_checkpoints: i64,
    /// Sessions started inside the window.
    pub sessions: Vec<SessionFact>,
    /// Ids of windowed sessions with no `end` event ever recorded.
    pub open_sessions: Vec<String>,
    /// `(path, distinct checkpoints that touched it)`, ranked by count then
    /// path — a total order, and the order the evidence ordinals follow.
    pub files: Vec<(String, i64)>,
    /// The AI / MX / HU rollup derived from [`WindowFacts::attributed`].
    pub attribution: AttributionRollup,
    /// The four token classes, summed over the window.
    pub tokens: TokenClasses,
    /// Cost in micro-units, bitemporally filtered to facts still believed.
    pub cost_micros: i64,
    /// `None` when this repository has no `recall.db` — reported as absent
    /// rather than as zero, and deliberately not created by a read-only report.
    pub savings: Option<Savings>,
    /// The touched paths this repository's policy calls sensitive — **`None`
    /// when nobody has asked policy yet, which is what this crate always
    /// leaves it as.**
    ///
    /// `rift-store` cannot fill this and must not learn how: the answer needs
    /// `rift_config::EffectiveConfig`, and a `rift-store -> rift-config` edge
    /// would invert the layering (storage reading project policy) on the
    /// workspace's most-depended-on crate. The derivation therefore lives in
    /// `bins/rift`, which applies it after [`gather`] and again after
    /// [`WindowFacts::restrict_to`].
    ///
    /// It is `Option` and not `Vec` for the reason the zero-vs-absent defect
    /// has already cost this program twice: `None` is *not computed*,
    /// `Some(vec![])` is *computed, and empty*, and a reader that cannot tell
    /// those apart will read "no sensitive path was touched" off a report that
    /// never asked. `gather_leaves_the_policy_answer_unasked` pins it.
    pub sensitive_paths: Option<Vec<String>>,
}
/// One `(checkpoint, file)` attribution row, as stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttributedFile {
    /// The checkpoint this row belongs to.
    pub checkpoint_id: String,
    /// The repository path of the revision it attributes.
    pub path: String,
    /// Lines the agent added.
    pub agent_added: i64,
    /// Lines the human added.
    pub human_added: i64,
    /// Agent lines the human modified.
    pub human_modified_agent: i64,
    /// Agent lines the human removed.
    pub agent_removed_by_human: i64,
    /// This row's attribution confidence, in milli-units.
    pub confidence_milli: i64,
}

impl WindowFacts {
    /// Derive `files` and `attribution` from [`WindowFacts::attributed`].
    ///
    /// **It stops one line short of where it used to.** In `bins/rift` this
    /// function ended by filling `sensitive_paths`; that line is policy, it
    /// stayed behind, and the field is left [`None`] here on purpose — see
    /// [`WindowFacts::sensitive_paths`].
    ///
    /// **One aggregation, two callers.** It runs once after the window is
    /// gathered and again after `--branch` restricts the checkpoint set, so the
    /// branch-scoped report is the same arithmetic over fewer rows rather than
    /// a second implementation of the same rollup.
    ///
    /// `files` counts DISTINCT CHECKPOINTS per path, not rows. `attribution` is
    /// keyed `(checkpoint_id, file_revision_id)` and `file_revision` is keyed
    /// `(path, blob_oid)`, so one path attributed at several revisions inside
    /// one checkpoint contributes several rows: counting rows made the number
    /// disagree with the key that names it. Measured on the reference
    /// repository — whose 178 attribution rows all sit in a single checkpoint —
    /// the row count reported up to 5 "checkpoints" for a path exactly one
    /// checkpoint ever touched.
    fn aggregate(&mut self) {
        let mut per_path: BTreeMap<&str, std::collections::BTreeSet<&str>> = BTreeMap::new();
        let mut roll = AttributionRollup::default();
        let mut conf_total: i64 = 0;
        for a in &self.attributed {
            per_path
                .entry(a.path.as_str())
                .or_default()
                .insert(a.checkpoint_id.as_str());
            roll.agent_lines = roll.agent_lines.saturating_add(a.agent_added);
            roll.human_lines = roll.human_lines.saturating_add(a.human_added);
            roll.mixed_lines = roll.mixed_lines.saturating_add(a.human_modified_agent);
            roll.agent_removed_by_human = roll
                .agent_removed_by_human
                .saturating_add(a.agent_removed_by_human);
            conf_total = conf_total.saturating_add(a.confidence_milli);
        }
        let rows = i64::try_from(self.attributed.len()).unwrap_or(i64::MAX);
        roll.confidence_milli = (rows > 0).then(|| conf_total / rows);
        self.attribution = roll;

        let mut files: Vec<(String, i64)> = per_path
            .into_iter()
            .map(|(p, cps)| (p.to_owned(), i64::try_from(cps.len()).unwrap_or(i64::MAX)))
            .collect();
        // Rank by touch count, then by path — a total order, so the same window
        // serializes identically.
        files.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        self.files = files;
    }

    /// Restrict every checkpoint-anchored fact to `ids`, then re-derive.
    ///
    /// The session, token and cost facts are **not** touched: they are anchored
    /// to `session.started_at` / `model_call.started_at`, not to a checkpoint,
    /// so a branch cannot scope them without inventing a relation the store
    /// does not hold. The report says which half was scoped rather than
    /// implying both were.
    ///
    /// **It no longer takes a worktree root, and losing that parameter is the
    /// visible half of the carve-out.** The root was here for one reason — to
    /// re-run the sensitive-path filter over the narrowed `files` — so a caller
    /// that narrows a window must re-apply policy itself afterwards, exactly as
    /// it must after [`gather`].
    ///
    /// So this **clears [`WindowFacts::sensitive_paths`] back to `None`**
    /// rather than leaving the pre-narrowing answer in place. A retained answer
    /// would describe a file set that no longer exists and would read as
    /// current; `None` says *nobody has asked policy about THIS set*, which is
    /// the same zero-vs-absent rule the field exists for, applied one
    /// derivation later.
    /// `restrict_to_marks_the_policy_answer_uncomputed_rather_than_stale` pins
    /// it.
    pub fn restrict_to(&mut self, ids: &std::collections::BTreeSet<String>) {
        self.checkpoints.retain(|c| ids.contains(&c.checkpoint_id));
        self.attributed.retain(|a| ids.contains(&a.checkpoint_id));
        self.aggregate();
        self.sensitive_paths = None;
    }

    /// Session counts per agent, or per model — **one derivation, two
    /// surfaces**.
    ///
    /// It is here for the same reason `aggregate` is: `rift
    /// recap` renders it as a document and Canopy's recap panel renders it as a
    /// card, and the one thing neither may do is disagree with the other about
    /// the same window. A card that re-grouped `sessions` itself would be a
    /// second implementation of a rollup — the exact defect the shared query
    /// core closed for `checkpoints`/`files`/`attribution`, arriving one
    /// derivation later.
    ///
    /// Sorted by **key**, never by count: a recap is read as a table, and a
    /// stable key order is what lets two windows be diffed. An empty window
    /// yields an empty list, which is a measured fact — the surfaces are the
    /// ones that owe the reader the difference between "no sessions" and "not
    /// asked".
    #[must_use]
    pub fn session_split(&self, by_model: bool) -> Vec<(String, usize)> {
        let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
        for s in &self.sessions {
            let key = if by_model {
                s.model.as_str()
            } else {
                s.agent.as_str()
            };
            *counts.entry(key).or_default() += 1;
        }
        counts.into_iter().map(|(k, n)| (k.to_owned(), n)).collect()
    }
}

/// One session in the window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionFact {
    /// The session's identifier.
    pub session_id: String,
    /// The agent that ran it.
    pub agent: String,
    /// Its primary model.
    pub model: String,
    /// When it started, epoch milliseconds.
    pub started_at: i64,
}
/// Gather every fact one window of the capture projection yields.
///
/// `recall_db` is the repository's `recall.db` when it has one; `None` is an
/// absent fact, never a zero, and this function **never creates it**.
///
/// **It returns `sensitive_paths: None`** — the storage layer does not read
/// project policy, and the caller applies it. See
/// [`WindowFacts::sensitive_paths`].
///
/// # Errors
/// Propagates projection read errors. A missing optional source (recall's
/// ledger) is an absent fact, not an error.
pub async fn gather(
    capture: &SqliteStore,
    recall_db: Option<&Path>,
    window: &Window,
) -> Result<WindowFacts> {
    let bounds = window.bounds();
    let checkpoint_binds = window.checkpoint_binds();
    // Field order is query order: the facts are gathered one after another
    // from one window, exactly as the report lists them.
    let mut facts = WindowFacts {
        checkpoints: window_checkpoints(capture, &checkpoint_binds).await?,
        unresolved_checkpoints: unresolved_checkpoint_count(capture).await?,
        sessions: window_sessions(capture, &bounds).await?,
        open_sessions: open_session_ids(capture, &bounds).await?,
        attributed: window_attribution(capture, &checkpoint_binds).await?,
        tokens: window_tokens(capture, &bounds).await?,
        cost_micros: window_cost_micros(capture, &bounds).await?,
        savings: read_savings(recall_db, window).await?,
        ..WindowFacts::default()
    };
    facts.aggregate();
    Ok(facts)
}

/// The window's checkpoints in time order, each marked `derived` when its time
/// came from its session rather than from the shard's own record.
async fn window_checkpoints(store: &SqliteStore, cb: &[Value; 4]) -> Result<Vec<CheckpointFact>> {
    let mut out = Vec::new();
    for r in store
        .query(
            &format!(
                "{CHECKPOINT_TIME_CTE}\
                 SELECT ct.checkpoint_id, ct.effective_at, ct.created_at FROM ct \
                 WHERE {IN_WINDOW} \
                 ORDER BY COALESCE(ct.effective_at, ?1), ct.checkpoint_id"
            ),
            cb,
        )
        .await?
    {
        let at = r.get_opt_i64(1)?;
        out.push(CheckpointFact {
            checkpoint_id: r.get_text(0)?,
            at,
            derived: at.is_some() && r.get_i64(2)? == CREATED_AT_UNKNOWN,
        });
    }
    Ok(out)
}

/// Repository-wide, not windowed: a checkpoint of unresolved time belongs to no
/// bounded window, so the count is the one place it still shows up.
async fn unresolved_checkpoint_count(store: &SqliteStore) -> Result<i64> {
    Ok(store
        .query_opt_i64(
            &format!("{CHECKPOINT_TIME_CTE}SELECT COUNT(*) FROM ct WHERE ct.effective_at IS NULL"),
            &[sentinel_bind()],
        )
        .await?
        .unwrap_or(0))
}

/// The sessions that started inside the window, in start order.
async fn window_sessions(store: &SqliteStore, b: &[Value; 2]) -> Result<Vec<SessionFact>> {
    let mut out = Vec::new();
    for r in store
        .query(
            "SELECT session_id, agent, model_primary, started_at FROM session \
             WHERE started_at >= ?1 AND started_at <= ?2 \
             ORDER BY started_at, session_id",
            b,
        )
        .await?
    {
        out.push(SessionFact {
            session_id: r.get_text(0)?,
            agent: r.get_text(1)?,
            model: r.get_text(2)?,
            started_at: r.get_i64(3)?,
        });
    }
    Ok(out)
}

/// Open = started in the window with no 'end' event ever recorded. A session
/// that ended outside the window is still ended; "open" is a property of the
/// session, not of the window, and conflating them would report every old
/// session as live.
async fn open_session_ids(store: &SqliteStore, b: &[Value; 2]) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for r in store
        .query(
            "SELECT s.session_id FROM session s \
             WHERE s.started_at >= ?1 AND s.started_at <= ?2 \
             AND NOT EXISTS (SELECT 1 FROM session_event e \
                             WHERE e.session_id = s.session_id AND e.kind = 'end') \
             ORDER BY s.session_id",
            b,
        )
        .await?
    {
        out.push(r.get_text(0)?);
    }
    Ok(out)
}

/// ONE query for every per-file fact, aggregated in Rust — see
/// [`WindowFacts::aggregate`] for why the rollup is not two `GROUP BY`s.
async fn window_attribution(store: &SqliteStore, cb: &[Value; 4]) -> Result<Vec<AttributedFile>> {
    let mut out = Vec::new();
    for r in store
        .query(
            &format!(
                "{CHECKPOINT_TIME_CTE}\
                 SELECT a.checkpoint_id, fr.path, a.agent_added, a.human_added, \
                 a.human_modified_agent, a.agent_removed_by_human, a.confidence_milli \
                 FROM attribution a \
                 JOIN file_revision fr ON fr.file_revision_id = a.file_revision_id \
                 JOIN ct ON ct.checkpoint_id = a.checkpoint_id \
                 WHERE {IN_WINDOW} \
                 ORDER BY a.checkpoint_id, fr.path, a.file_revision_id"
            ),
            cb,
        )
        .await?
    {
        out.push(AttributedFile {
            checkpoint_id: r.get_text(0)?,
            path: r.get_text(1)?,
            agent_added: r.get_i64(2)?,
            human_added: r.get_i64(3)?,
            human_modified_agent: r.get_i64(4)?,
            agent_removed_by_human: r.get_i64(5)?,
            confidence_milli: r.get_i64(6)?,
        });
    }
    Ok(out)
}

/// The four token classes summed over the model calls that started in the
/// window; an aggregate always yields one row, so zeros are measured zeros.
async fn window_tokens(store: &SqliteStore, b: &[Value; 2]) -> Result<TokenClasses> {
    let rows = store
        .query(
            "SELECT COALESCE(SUM(tu.uncached_input), 0), COALESCE(SUM(tu.cached_input), 0), \
             COALESCE(SUM(tu.cache_creation), 0), COALESCE(SUM(tu.output), 0) \
             FROM token_usage tu JOIN model_call mc ON mc.call_id = tu.call_id \
             WHERE mc.started_at >= ?1 AND mc.started_at <= ?2",
            b,
        )
        .await?;
    let Some(r) = rows.first() else {
        return Ok(TokenClasses::default());
    };
    Ok(TokenClasses {
        uncached_input: r.get_i64(0)?,
        cached_input: r.get_i64(1)?,
        cache_creation: r.get_i64(2)?,
        output: r.get_i64(3)?,
    })
}

/// Bitemporal: only facts still believed, and only the ones nothing
/// supersedes. Dropping either half double-counts a re-priced call.
async fn window_cost_micros(store: &SqliteStore, b: &[Value; 2]) -> Result<i64> {
    Ok(store
        .query_opt_i64(
            "SELECT COALESCE(SUM(cf.amount_micros), 0) FROM cost_fact cf \
             JOIN model_call mc ON mc.call_id = cf.call_id \
             WHERE mc.started_at >= ?1 AND mc.started_at <= ?2 \
             AND cf.valid_to IS NULL \
             AND NOT EXISTS (SELECT 1 FROM cost_fact s \
                             WHERE s.supersedes_id = cf.cost_fact_id)",
            b,
        )
        .await?
        .unwrap_or(0))
}

/// Recall's ledger arithmetic, or `None` when this repository has no `recall.db`.
///
/// **Deliberately does not create the database.** `open_migrated` would, and a
/// read-only report that bootstraps a projection changes what the dashboard
/// shows (the Disabled/Quiet distinction) as a side effect of being run.
async fn read_savings(db: Option<&Path>, window: &Window) -> Result<Option<Savings>> {
    let Some(db) = db else { return Ok(None) };
    let store = SqliteStore::open_read_only(db)?;
    let mut out = Savings::default();
    for r in store
        .query(
            "SELECT verb, COALESCE(SUM(tokens_avoided), 0), COALESCE(SUM(tokens_returned), 0) \
             FROM memd_audit_event WHERE recorded_at >= ?1 AND recorded_at <= ?2 \
             GROUP BY verb ORDER BY verb",
            &window.bounds(),
        )
        .await?
    {
        let (verb, avoided, returned) = (r.get_text(0)?, r.get_i64(1)?, r.get_i64(2)?);
        if verb == "memd.manifest_injected" {
            out.injected = out.injected.saturating_add(returned);
        } else {
            out.avoided_upper_bound = out.avoided_upper_bound.saturating_add(avoided);
            out.returned = out.returned.saturating_add(returned);
        }
    }
    Ok(Some(out))
}

#[cfg(test)]
mod tests {
    use super::{
        gather, AttributedFile, CheckpointFact, SessionFact, TokenClasses, Window, WindowFacts,
        CREATED_AT_UNKNOWN,
    };
    use crate::{apply_migrations, default_migrations, CanonicalStore, SqliteStore, Value};

    /// Schema version stamped on every seeded row (the column is `NOT NULL`).
    const V: i64 = 1;

    fn t(s: &str) -> Value {
        Value::Text(s.to_owned())
    }

    fn i(n: i64) -> Value {
        Value::Int(n)
    }

    /// A projection holding two checkpoints — one that records its own time,
    /// one that does not and must derive it from its session — and two touched
    /// files, **one of which any policy on earth calls sensitive**.
    ///
    /// The `.env` is the whole point of the fixture: it is the row that makes
    /// [`gather`]'s `None` an observation rather than a tautology about an
    /// empty store.
    async fn seeded() -> SqliteStore {
        let s = SqliteStore::open_in_memory().expect("store");
        apply_migrations(&s, &default_migrations()).expect("migrations");
        fixture_sessions(&s).await;
        fixture_checkpoints(&s).await;
        fixture_session_links(&s).await;
        fixture_file_revisions(&s).await;
        fixture_attribution(&s).await;
        s
    }

    /// Two sessions, `s1` at 1_000 and `s2` at 5_000.
    async fn fixture_sessions(s: &SqliteStore) {
        for (id, started) in [("s1", 1_000), ("s2", 5_000)] {
            s.insert(
                "session",
                &[
                    "session_id",
                    "agent",
                    "model_primary",
                    "repo_id",
                    "started_at",
                    "schema_version",
                ],
                &[
                    t(id),
                    t("claude-code"),
                    t("opus"),
                    t("repo"),
                    i(started),
                    i(V),
                ],
            )
            .await
            .expect("session");
        }
    }

    /// `c1` records 2_000; `c2` records nothing and derives 5_000 from `s2`.
    async fn fixture_checkpoints(s: &SqliteStore) {
        for (id, created) in [("c1", 2_000), ("c2", CREATED_AT_UNKNOWN)] {
            s.insert(
                "checkpoint",
                &[
                    "checkpoint_id",
                    "strategy",
                    "created_at",
                    "merkle_root",
                    "schema_version",
                ],
                &[t(id), t("manual-commit"), i(created), t("00"), i(V)],
            )
            .await
            .expect("checkpoint");
        }
    }

    /// Each checkpoint's session, which is where `c2` derives its time.
    async fn fixture_session_links(s: &SqliteStore) {
        for (cp, sess) in [("c1", "s1"), ("c2", "s2")] {
            s.insert(
                "session_in_checkpoint",
                &["checkpoint_id", "session_id", "ordinal", "schema_version"],
                &[t(cp), t(sess), i(0), i(V)],
            )
            .await
            .expect("session_in_checkpoint");
        }
    }

    /// The touched files: `.env` (the sensitive one) and `src/main.rs`.
    async fn fixture_file_revisions(s: &SqliteStore) {
        for (id, path) in [("f1", ".env"), ("f2", "src/main.rs")] {
            s.insert(
                "file_revision",
                &[
                    "file_revision_id",
                    "path",
                    "blob_oid",
                    "content_sha256",
                    "schema_version",
                ],
                &[t(id), t(path), t(id), t("aa"), i(V)],
            )
            .await
            .expect("file_revision");
        }
    }

    /// `.env` is touched by both checkpoints, `src/main.rs` by one — so the
    /// aggregate's sort order is deterministic and asserted below.
    async fn fixture_attribution(s: &SqliteStore) {
        for (cp, fr) in [("c1", "f1"), ("c2", "f1"), ("c1", "f2")] {
            s.insert(
                "attribution",
                &[
                    "checkpoint_id",
                    "file_revision_id",
                    "agent_added",
                    "human_added",
                    "human_modified_agent",
                    "agent_removed_by_human",
                    "confidence_milli",
                    "method_version",
                    "schema_version",
                ],
                &[t(cp), t(fr), i(10), i(2), i(1), i(0), i(900), t("v1"), i(V)],
            )
            .await
            .expect("attribution");
        }
    }

    /// **THE PIN.** `rift-store` gathers a window that contains `.env` — a path
    /// every shipped policy calls sensitive — and still answers `None`.
    ///
    /// The field is not "empty because nothing matched": nothing was *asked*.
    /// If a later change ever taught this crate to fill it, `rift-store` would
    /// have grown a `rift-config` edge and this assertion is where that shows
    /// up, before the layering inversion reaches a reviewer's diff.
    #[tokio::test]
    async fn gather_leaves_the_policy_answer_unasked() {
        let s = seeded().await;
        let w = Window::resolve(None, None).expect("window");
        let f = gather(&s, None, &w).await.expect("gather");

        assert!(
            f.files.iter().any(|(p, _)| p == ".env"),
            "the fixture must actually put a sensitive-looking path in the window"
        );
        assert_eq!(
            f.sensitive_paths, None,
            "storage must never appear to have computed policy it never read"
        );
    }

    /// The narrowing half of the same rule: a scope change invalidates the
    /// policy answer, and the answer says so instead of going stale.
    #[test]
    fn restrict_to_marks_the_policy_answer_uncomputed_rather_than_stale() {
        let mut f = WindowFacts {
            checkpoints: vec![
                CheckpointFact {
                    checkpoint_id: "c1".to_owned(),
                    at: Some(1),
                    derived: false,
                },
                CheckpointFact {
                    checkpoint_id: "c2".to_owned(),
                    at: Some(2),
                    derived: false,
                },
            ],
            attributed: vec![row("c1", ".env"), row("c2", "src/main.rs")],
            // As a caller that applied policy would have left it.
            sensitive_paths: Some(vec![".env".to_owned()]),
            ..WindowFacts::default()
        };
        let keep: std::collections::BTreeSet<String> = ["c2".to_owned()].into_iter().collect();
        f.restrict_to(&keep);

        assert_eq!(f.checkpoints.len(), 1);
        assert_eq!(f.files, vec![("src/main.rs".to_owned(), 1)]);
        assert_eq!(
            f.sensitive_paths, None,
            "a policy answer about the wider set must not be presented as one about this set"
        );
    }

    fn row(cp: &str, path: &str) -> AttributedFile {
        AttributedFile {
            checkpoint_id: cp.to_owned(),
            path: path.to_owned(),
            agent_added: 10,
            human_added: 2,
            human_modified_agent: 1,
            agent_removed_by_human: 0,
            confidence_milli: 900,
        }
    }

    /// `files` counts DISTINCT CHECKPOINTS, not attribution rows — the defect
    /// the moved doc describes, asserted rather than described.
    #[test]
    fn aggregate_counts_distinct_checkpoints_not_rows() {
        let mut f = WindowFacts {
            attributed: vec![
                row("c1", "a.rs"),
                // Same path, same checkpoint, a second revision: one checkpoint.
                row("c1", "a.rs"),
                row("c2", "a.rs"),
                row("c1", "b.rs"),
            ],
            ..WindowFacts::default()
        };
        f.restrict_to(&["c1".to_owned(), "c2".to_owned()].into_iter().collect());

        assert_eq!(
            f.files,
            vec![("a.rs".to_owned(), 2), ("b.rs".to_owned(), 1)],
            "rank by touch count, then by path — a total order"
        );
        assert_eq!(f.attribution.agent_lines, 40);
        assert_eq!(f.attribution.confidence_milli, Some(900));
    }

    /// The split is by the axis it was asked for, keyed in a total order, and
    /// counts sessions rather than distinct keys.
    #[test]
    fn the_session_split_counts_by_the_axis_it_was_asked_for() {
        let sess = |id: &str, agent: &str, model: &str| SessionFact {
            session_id: id.to_owned(),
            agent: agent.to_owned(),
            model: model.to_owned(),
            started_at: 0,
        };
        let f = WindowFacts {
            sessions: vec![
                sess("s1", "claude-code", "opus"),
                sess("s2", "codex", "sonnet"),
                sess("s3", "claude-code", "sonnet"),
            ],
            ..WindowFacts::default()
        };
        assert_eq!(
            f.session_split(false),
            vec![("claude-code".to_owned(), 2), ("codex".to_owned(), 1)],
        );
        assert_eq!(
            f.session_split(true),
            vec![("opus".to_owned(), 1), ("sonnet".to_owned(), 2)],
            "by key, not by count — two windows have to be diffable"
        );
        assert!(WindowFacts::default().session_split(false).is_empty());
    }

    /// No attribution row in the window means no confidence measurement — and
    /// an absent measurement is not a measured zero.
    #[test]
    fn an_empty_window_has_no_confidence_rather_than_a_confidence_of_zero() {
        let mut f = WindowFacts::default();
        f.restrict_to(&std::collections::BTreeSet::new());
        assert_eq!(f.attribution.confidence_milli, None);
    }

    #[test]
    fn a_utc_day_resolves_to_its_midnight() {
        // Known-good anchors, computed independently of this code: the epoch
        // itself, a leap day, and a post-2000 date.
        assert_eq!(super::day_start_ms("1970-01-01"), Some(0));
        assert_eq!(super::day_start_ms("2000-02-29"), Some(951_782_400_000));
        assert_eq!(super::day_start_ms("2026-08-17"), Some(1_786_924_800_000));
    }

    #[test]
    fn a_day_that_is_not_one_is_refused_rather_than_rounded() {
        for bad in [
            "2026-02-30", // February has no 30th
            "2025-02-29", // 2025 is not a leap year
            "2026-13-01", // no 13th month
            "2026-00-10", // no 0th month
            "2026-8-17",  // not zero-padded: one date, one spelling
            "20260817",
            "2026-08-17T00:00:00Z",
            "",
        ] {
            assert_eq!(super::day_start_ms(bad), None, "{bad} must not parse");
        }
        // …and a leap day that IS one still parses, so the rule refuses
        // spellings rather than refusing everything.
        assert!(super::day_start_ms("2024-02-29").is_some());
    }

    #[test]
    fn a_window_bound_takes_canonical_epoch_millis_and_nothing_looser() {
        assert_eq!(
            super::parse_instant("0", "since", super::Edge::Opening).unwrap(),
            0
        );
        assert_eq!(
            super::parse_instant("1787011200000", "since", super::Edge::Opening).unwrap(),
            1_787_011_200_000
        );
        // The container frame's defect, refused here for the same reason: one
        // spelling per value.
        for bad in ["01", "+1", "1_000", " 1", "0x10"] {
            assert!(
                super::parse_instant(bad, "since", super::Edge::Opening).is_err(),
                "{bad} must not be a second spelling"
            );
        }
    }

    #[test]
    fn a_refusal_never_quotes_what_the_caller_typed() {
        // The bound a hostile caller supplies is printed into a terminal.
        let hostile = "\u{1b}[2J\u{1b}]0;pwned\u{7}";
        let err = super::parse_instant(hostile, "since", super::Edge::Opening)
            .unwrap_err()
            .to_string();
        for needle in ["\u{1b}", "pwned"] {
            assert!(!err.contains(needle), "the refusal echoed input: {err:?}");
        }
        assert!(
            err.contains("--since"),
            "it must still name the field: {err}"
        );
    }

    #[test]
    fn an_empty_window_is_refused_not_answered_with_zeros() {
        let err = Window::resolve(Some("2026-08-17"), Some("2026-08-01")).unwrap_err();
        assert!(err.to_string().contains("empty"), "got: {err}");
        // The same pair the right way round is a window.
        assert!(Window::resolve(Some("2026-08-01"), Some("2026-08-17")).is_ok());
    }

    #[test]
    fn an_omitted_bound_is_unbounded_and_never_now() {
        let w = Window::resolve(None, None).unwrap();
        assert_eq!(w.since_ms(), i64::MIN);
        assert_eq!(w.until_ms(), i64::MAX);
        // Resolving twice gives the same window — the determinism the report's
        // byte-identical `--json` rests on.
        assert_eq!(w, Window::resolve(None, None).unwrap());
    }

    /// Two spellings of one bound name the same window, so they key one pack.
    #[test]
    fn a_key_is_built_from_the_resolved_bounds_not_the_typed_text() {
        let day = Window::resolve(Some("1970-01-01"), None).expect("day");
        let ms = Window::resolve(Some("0"), None).expect("ms");
        assert_eq!(day.key_text(), ms.key_text());
        // …and the echo still reports what each caller actually typed.
        assert_eq!(day.since_text(), Some("1970-01-01"));
        assert_eq!(ms.since_text(), Some("0"));
        assert_eq!(day.until_text(), None);
        assert_eq!(day.until_ms(), i64::MAX);
    }

    /// A day typed as `--until` closes that day.
    ///
    /// The bounds are documented inclusive and every query binds `<= until`,
    /// so a day that resolved to its own midnight admitted one millisecond of
    /// the twenty-four hours the caller asked for. A day typed as `--since`
    /// still opens at midnight, and epoch milliseconds are an instant at
    /// either edge.
    #[test]
    fn a_day_named_as_until_covers_the_whole_day() {
        let day = Window::resolve(Some("2026-09-14"), Some("2026-09-14")).expect("one day");
        assert_eq!(day.since_ms(), 1_789_344_000_000, "midnight opens the day");
        assert_eq!(
            day.until_ms(),
            1_789_344_000_000 + 86_400_000 - 1,
            "the last millisecond closes it"
        );
        // One day named twice is a window, not an empty one.
        assert!(day.until_ms() > day.since_ms());
        // Epoch milliseconds are an instant wherever they are typed.
        let ms = Window::resolve(Some("0"), Some("1789344000000")).expect("ms");
        assert_eq!(ms.until_ms(), 1_789_344_000_000);
    }

    #[test]
    fn the_token_total_saturates_rather_than_wrapping() {
        let c = TokenClasses {
            uncached_input: i64::MAX,
            cached_input: 1,
            cache_creation: 1,
            output: 1,
        };
        assert_eq!(c.total(), i64::MAX);
    }

    /// The sentinel rule, from the only side that can observe it: an
    /// unresolvable checkpoint is outside every bounded window, inside the
    /// unbounded one, and counted either way.
    #[tokio::test]
    async fn a_derived_time_is_reported_as_derived_and_never_as_recorded() {
        let s = seeded().await;

        let all = gather(&s, None, &Window::resolve(None, None).expect("w"))
            .await
            .expect("gather");
        assert_eq!(all.checkpoints.len(), 2);
        assert_eq!(all.checkpoints[0].checkpoint_id, "c1");
        assert!(!all.checkpoints[0].derived);
        assert_eq!(all.checkpoints[1].at, Some(5_000));
        assert!(
            all.checkpoints[1].derived,
            "a time derived from a session is never presented as the shard's own claim"
        );
        assert_eq!(all.unresolved_checkpoints, 0);

        // A bounded window that excludes the derived checkpoint's time.
        let narrow = gather(
            &s,
            None,
            &Window::resolve(Some("0"), Some("3000")).expect("w"),
        )
        .await
        .expect("gather");
        assert_eq!(narrow.checkpoints.len(), 1);
        assert_eq!(narrow.attributed.len(), 2);
        assert_eq!(
            narrow.sessions.len(),
            1,
            "sessions are anchored to their own start"
        );
        assert_eq!(narrow.open_sessions.len(), 1);
    }

    /// A repository with no recall ledger reports savings as **absent** — not
    /// as three zeros, which would read as "recall ran and saved nothing".
    #[tokio::test]
    async fn a_window_without_a_ledger_reports_savings_absent_rather_than_zero() {
        let s = seeded().await;
        let f = gather(&s, None, &Window::resolve(None, None).expect("w"))
            .await
            .expect("gather");
        assert_eq!(f.savings, None);
    }

    /// The ledger arithmetic, including the one line that is not arithmetic:
    /// injected manifest tokens are real spend and are **never** credited.
    #[tokio::test]
    async fn injected_manifest_tokens_are_subtracted_spend_and_never_credit() {
        let dir = tempfile::tempdir().expect("tmp");
        let db = dir.path().join("recall.db");
        {
            let r = crate::open_migrated(&db, &crate::recall_migrations()).expect("recall db");
            for (id, verb, returned, avoided, at) in [
                ("a1", "memd.manifest_served", 120, 900, 1_600),
                ("a2", "memd.manifest_served", 30, 100, 6_200),
                ("a3", "memd.manifest_injected", 40, 0, 1_700),
            ] {
                r.insert(
                    "memd_audit_event",
                    &[
                        "audit_id",
                        "verb",
                        "tokens_returned",
                        "tokens_avoided",
                        "recorded_at",
                        "schema_version",
                    ],
                    &[t(id), t(verb), i(returned), i(avoided), i(at), i(V)],
                )
                .await
                .expect("memd_audit_event");
            }
        }
        let s = seeded().await;

        let all = gather(
            &s,
            Some(db.as_path()),
            &Window::resolve(None, None).expect("w"),
        )
        .await
        .expect("gather")
        .savings
        .expect("a ledger exists, so the fact is present");
        assert_eq!(all.avoided_upper_bound, 1_000);
        assert_eq!(all.returned, 150);
        assert_eq!(
            all.injected, 40,
            "injected spend is its own line, never credit"
        );

        // The ledger is windowed on its own `recorded_at`, so a narrower window
        // drops the later serve — and the injected row with it.
        let narrow = gather(
            &s,
            Some(db.as_path()),
            &Window::resolve(Some("0"), Some("1650")).expect("w"),
        )
        .await
        .expect("gather")
        .savings
        .expect("present");
        assert_eq!(narrow.avoided_upper_bound, 900);
        assert_eq!(narrow.injected, 0);
    }
}
