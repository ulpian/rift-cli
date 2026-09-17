//! `rift review` — the **ReviewPack** for the commits in scope.
//!
//! # What this verb is, and what it is not
//!
//! It compiles evidence. It does not judge, and it cannot: every line it emits
//! comes from a sealed [`SectionEntry`] constructor over a closed vocabulary in
//! which a *judgment is unrepresentable*. The verdict is the user's agent's,
//! written by the user's agent, on the user's budget — Rift executes no model
//! That is the whole difference from the tool this replaces, which
//! ran several agent CLIs itself.
//!
//! # Scope
//!
//! The same scope machinery as `rift dispatch`: a window (`--since`/`--until`)
//! and optionally a branch, resolved through
//! [`crate::workflows::branch_scope`] against the trailers and shard anchors
//! that already exist. A review of "everything" is legal and is what an omitted
//! window means.
//!
//! # Profiles
//!
//! A profile is a **closed enum**, not a config string, and it selects sections
//! and a budget. A free-form profile name would be a second vocabulary — and a
//! vocabulary a caller can extend is a vocabulary the sealed constructors do
//! not cover.
//!
//! # What is NOT built, declared rather than implied
//!
//! The design names six section families. Four compile here: the scope,
//! the attribution split and confidence, the risk signals (sensitive paths,
//! low confidence), and the cost/savings signals. **Two do not, and they are
//! named rather than summarised:**
//!
//! * **redacted prompts** — the bytes exist as stored `RedactedBytes` in each
//!   shard's `content.redacted`, but no sealed constructor admits a redacted
//!   transcript into a pack section today. Admitting one is a pack-vocabulary
//!   change (a separate surface), not a caller-side decision, so this
//!   verb does not invent one;
//! * **failing-test history** — the outcome labels live in the recall ledger's
//!   bitemporal outcome rows, a plane this verb does not read.
//!
//! Neither is faked with a placeholder line. A pack that says less than it
//! could is honest; a pack whose section exists and is empty teaches its reader
//! that the fact is absent.

use rift_core::{Result, RiftError};
use rift_recall_serve::pack::{
    compile, CriticalFact, EvidenceRef, PackKeyInputs, PackKind, PackRequest, ReviewScopeFact,
    ReviewSignal, SectionEntry, DEFAULT_REVIEW_PACK_TOKENS, MAX_PACK_TOKENS,
};
use serde_json::json;

use crate::cli::Outcome;
use crate::workflows::{window_json, FactsPolicy, Planes, Window, WindowFacts};

/// The `rift review` JSON contract version.
pub(crate) const REVIEW_SCHEMA_VERSION: u32 = 1;

/// Which sections a review compiles, and how many tokens it may spend.
///
/// Closed on purpose (see the module docs). Each variant is a *question a
/// reviewer is asking*, not a feature flag: `Risk` is "what should stop me
/// merging", `Signals` is "what did this cost and who wrote it", `Full` is
/// both plus the scope narrative.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReviewProfile {
    /// Everything this verb can compile, at the review kind's default budget.
    Full,
    /// Signals and scope, no criticals — the "how was this built" read.
    Signals,
    /// Criticals and scope only, at a quarter of the budget: the smallest
    /// artifact that can still stop a merge.
    Risk,
}

impl ReviewProfile {
    /// Parse a profile name. `None` for anything else — and the refusal that
    /// follows never echoes what was typed.
    fn parse(name: &str) -> Option<Self> {
        match name {
            "full" => Some(Self::Full),
            "signals" => Some(Self::Signals),
            "risk" => Some(Self::Risk),
            _ => None,
        }
    }

    /// The stable tag this profile reports itself as.
    const fn tag(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Signals => "signals",
            Self::Risk => "risk",
        }
    }

    /// This profile's token budget.
    const fn budget(self) -> u32 {
        match self {
            Self::Full | Self::Signals => DEFAULT_REVIEW_PACK_TOKENS,
            Self::Risk => DEFAULT_REVIEW_PACK_TOKENS / 4,
        }
    }

    /// Whether criticals ride.
    const fn wants_criticals(self) -> bool {
        matches!(self, Self::Full | Self::Risk)
    }

    /// Whether derived signals ride.
    const fn wants_signals(self) -> bool {
        matches!(self, Self::Full | Self::Signals)
    }
}

/// Every profile name, for the refusal message — the accepted set is Rift's own
/// text and may be named; the rejected input is not and may not.
const PROFILE_NAMES: &str = "full, signals, risk";

/// `rift review` — compile the ReviewPack for the commits in scope.
///
/// # Errors
/// [`RiftError`] on a projection read failure or an unusable window; a bad
/// profile or an unusable branch is a refusal [`Outcome`], not an `Err`.
pub(crate) async fn review(
    planes: &Planes<'_>,
    since: Option<&str>,
    until: Option<&str>,
    branch: Option<&str>,
    profile: Option<&str>,
) -> Result<Outcome> {
    let profile = match profile {
        Some(name) => match ReviewProfile::parse(name) {
            Some(p) => p,
            None => {
                return Ok(Outcome::coded(
                    crate::cli::exit::USAGE,
                    format!("review: --profile must be one of: {PROFILE_NAMES}"),
                    json!({
                        "schema_version": REVIEW_SCHEMA_VERSION,
                        "error": "unknown_profile",
                        "accepted": ["full", "signals", "risk"],
                    }),
                ));
            }
        },
        None => ReviewProfile::Full,
    };

    let window = Window::resolve(since, until)?;
    let mut facts = crate::workflows::gather(planes, &window).await?;
    let mut scope_json = serde_json::Value::Null;
    if let Some(b) = branch {
        match crate::workflows::branch_scope(planes.root, b, &facts.checkpoints) {
            Ok(scope) => {
                facts.restrict_to_branch(&scope.ids, planes.root);
                scope_json = scope.json();
            }
            Err(refusal) => return Ok(refusal),
        }
    }

    // Worktree provenance for the review window, computed after any branch
    // narrowing: the members the reviewed checkpoints were archived in. It sits
    // beside the pack rather than inside it — the pack's section vocabulary is
    // a closed typed set in the recall plane and a provenance label is not one
    // of its facts — and the human line is appended for the same reason.
    let worktrees = crate::provenance::worktree_spread(
        planes.capture,
        &facts
            .checkpoints
            .iter()
            .map(|c| c.checkpoint_id.clone())
            .collect::<Vec<_>>(),
    )
    .await;
    let (rendered, pack) = compile_review(&window, &facts, profile)?;
    let doc = json!({
        "schema_version": REVIEW_SCHEMA_VERSION,
        "profile": profile.tag(),
        "window": window_json(&window),
        "branch": scope_json,
        "worktrees": worktrees,
        "pack": pack,
    });
    let rendered = format!(
        "{}\nworktrees: {}",
        rendered.trim_end(),
        crate::provenance::spread_text(&worktrees)
    );
    Ok(Outcome::ok(rendered, doc))
}

/// Compile `facts` into a Review-kind pack under `profile`.
///
/// # Errors
/// [`RiftError::Validity`] if a fact is offered to a section that does not
/// admit it — propagated rather than swallowed, because a placement invariant
/// is exactly the thing that must never fail quietly.
pub(crate) fn compile_review(
    window: &Window,
    facts: &WindowFacts,
    profile: ReviewProfile,
) -> Result<(String, serde_json::Value)> {
    let mut req = PackRequest::for_kind(PackKind::Review, profile.budget());

    if profile.wants_criticals() {
        // By ordinal, never by path — and selected by walking the
        // ordinal pass, so the citation is the position rather than a lookup
        // that could miss. `--profile risk` compiles a quarter of the budget of
        // `--profile full`, which is exactly why the number cannot come from
        // the finished pack: degradation drops entries, and the same file must
        // be `evidence:3` under both.
        for f in facts.evidence().iter().filter(|f| f.sensitive) {
            req.push_critical(SectionEntry::critical(
                &CriticalFact::SensitivePathTouched {
                    evidence: EvidenceRef::at(f.ordinal),
                },
            ))?;
        }
        if let Some(milli) = facts.attribution.confidence_milli {
            if milli < 700 {
                req.push_critical(SectionEntry::critical(
                    &CriticalFact::LowAttributionConfidence {
                        percent: u8::try_from(milli / 10).unwrap_or(100),
                    },
                ))?;
            }
        }
    }

    // The scope rides in EVERY profile: a pack whose reader cannot tell what it
    // covers is worse than no pack.
    req.push_review_scope(SectionEntry::review_scope(&ReviewScopeFact::Volume {
        commits: clamp(facts.checkpoints.len()),
        sessions: clamp(facts.sessions.len()),
        files: clamp(facts.files.len()),
    }))?;

    if profile.wants_signals() {
        let a = &facts.attribution;
        req.push_review_signal(SectionEntry::review_signal(
            ReviewSignal::AttributionSplit {
                ai_lines: clamp_i64(a.agent_lines),
                mixed_lines: clamp_i64(a.mixed_lines),
                human_lines: clamp_i64(a.human_lines),
            },
        ))?;
        if let Some(milli) = a.confidence_milli {
            req.push_review_signal(SectionEntry::review_signal(
                ReviewSignal::AttributionConfidence {
                    percent: u8::try_from(milli / 10).unwrap_or(100),
                },
            ))?;
        }
        // NOT clamped. `clamp_i64` saturates at `u32::MAX`, and on the
        // reference repository that understated `cache_read` by 1,082,729,615
        // tokens (20.1%) while presenting it as exact — a wrong number with no
        // ceiling marker is worse than a missing one. The signal is `u64`.
        req.push_review_signal(SectionEntry::review_signal(ReviewSignal::TokenSpend {
            input: widen(facts.tokens.uncached_input),
            output: widen(facts.tokens.output),
            cache_read: widen(facts.tokens.cached_input),
            cache_write: widen(facts.tokens.cache_creation),
        }))?;
        if let Some(s) = facts.savings {
            req.push_review_signal(SectionEntry::review_signal(ReviewSignal::RecallSavings {
                tokens_avoided: clamp_i64(s.avoided_upper_bound),
                tokens_injected: clamp_i64(s.injected),
            }))?;
        }
    }

    let pack = compile(&req, MAX_PACK_TOKENS).map_err(RiftError::from)?;
    let inputs = PackKeyInputs {
        // Domain-separated by verb AND profile: two profiles over one window
        // are two different questions and must not share a key.
        query: format!("review:{}:{}", profile.tag(), window.key_text()),
        ranked: facts
            .checkpoints
            .iter()
            .map(|c| (c.checkpoint_id.clone(), c.checkpoint_id.clone()))
            .collect(),
        policy_version: 1,
        token_budget: profile.budget(),
    };
    let evidence = pack.to_evidence(&inputs);
    let json = serde_json::to_value(&evidence).map_err(|e| RiftError::Serde(e.to_string()))?;
    Ok((pack.render(), json))
}

/// Counts cross the seam as `u32`; saturating so "very many" never wraps to a
/// small number, the only failure mode here that would be silent.
fn clamp(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// A stored `i64` count as the `u64` the pack's token signal takes. Negative
/// is impossible for a token total and becomes zero rather than a huge number.
fn widen(n: i64) -> u64 {
    u64::try_from(n).unwrap_or(0)
}

/// The same, for the sums that arrive as `i64` out of SQL.
fn clamp_i64(n: i64) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflows::{AttributionRollup, CheckpointFact, Savings, TokenClasses};

    fn facts() -> WindowFacts {
        WindowFacts {
            checkpoints: vec![
                CheckpointFact {
                    checkpoint_id: "c1".to_owned(),
                    at: Some(10),
                    derived: false,
                },
                CheckpointFact {
                    checkpoint_id: "c2".to_owned(),
                    at: Some(20),
                    derived: true,
                },
            ],
            // `sensitive_paths` is a filtered subset of `files` in production
            // (`sensitive_touched`), and the fixture keeps that shape: it is
            // what gives `.env` the ordinal a critical fact cites it by.
            files: vec![("src/a.rs".to_owned(), 2), (".env".to_owned(), 1)],
            sensitive_paths: Some(vec![".env".to_owned()]),
            attribution: AttributionRollup {
                agent_lines: 40,
                mixed_lines: 5,
                human_lines: 10,
                agent_removed_by_human: 1,
                confidence_milli: Some(550),
            },
            tokens: TokenClasses {
                uncached_input: 100,
                cached_input: 200,
                cache_creation: 30,
                output: 50,
            },
            savings: Some(Savings {
                avoided_upper_bound: 900,
                injected: 100,
                returned: 20,
            }),
            ..WindowFacts::default()
        }
    }

    #[test]
    fn a_profile_selects_sections_and_a_budget_rather_than_renaming_the_same_pack() {
        let w = Window::resolve(None, None).unwrap();
        let f = facts();
        let full = compile_review(&w, &f, ReviewProfile::Full).unwrap();
        let signals = compile_review(&w, &f, ReviewProfile::Signals).unwrap();
        let risk = compile_review(&w, &f, ReviewProfile::Risk).unwrap();

        // Risk keeps the criticals and drops the signals; Signals is the
        // opposite. Asserted on the SECTION VOCABULARY rather than on words in
        // the render: the low-confidence critical says "attribution" too, and a
        // substring test would have called that a signal.
        // A section may be PRESENT and empty (the layout table is per kind, not
        // per profile), so the discriminator is how many LINES it carries.
        let lines = |p: &(String, serde_json::Value), tag: &str| -> usize {
            p.1["sections"].as_array().map_or(0, |a| {
                a.iter()
                    .filter(|s| s["section"].as_str() == Some(tag))
                    .map(|s| s["lines"].as_array().map_or(0, Vec::len))
                    .sum()
            })
        };

        assert!(lines(&risk, "critical_top") > 0, "risk lost its criticals");
        assert_eq!(lines(&risk, "review_signals"), 0, "risk carried signals");
        // The critical rides as an ordinal: the pack SAYS a
        // sensitive path was touched and does not say which, on either profile.
        assert!(risk.0.contains("evidence:"), "got: {}", risk.0);
        assert!(!risk.0.contains(".env"), "got: {}", risk.0);

        assert!(
            lines(&signals, "review_signals") > 0,
            "signals lost its signals"
        );
        assert_eq!(
            lines(&signals, "critical_top"),
            0,
            "signals carried criticals"
        );
        assert!(!signals.0.contains(".env"), "got: {}", signals.0);

        assert!(lines(&full, "critical_top") > 0 && lines(&full, "review_signals") > 0);

        // Every profile carries the scope.
        for (name, p) in [("full", &full), ("signals", &signals), ("risk", &risk)] {
            assert!(
                p.0.contains("scope: 2 commit(s)"),
                "{name} lost its scope: {}",
                p.0
            );
        }

        // Three questions, three keys.
        let key = |p: &(String, serde_json::Value)| p.1["pack_key"].clone();
        assert_ne!(key(&full), key(&signals));
        assert_ne!(key(&full), key(&risk));
        assert_ne!(key(&signals), key(&risk));
    }

    #[test]
    fn every_profile_stays_inside_its_own_budget_and_the_shared_ceiling() {
        let w = Window::resolve(None, None).unwrap();
        let f = facts();
        for p in [
            ReviewProfile::Full,
            ReviewProfile::Signals,
            ReviewProfile::Risk,
        ] {
            let (_, json) = compile_review(&w, &f, p).unwrap();
            let total = json["total_tokens"].as_u64().unwrap_or(u64::MAX);
            assert!(
                total <= u64::from(p.budget()),
                "{} exceeded its own budget: {total} > {}",
                p.tag(),
                p.budget()
            );
            assert!(total <= u64::from(MAX_PACK_TOKENS));
        }
    }

    /// **A token total above `u32::MAX` is reported, not saturated.**
    ///
    /// The shipped signal clamped to `u32`, so on the reference repository the
    /// pack said `cache_read=4294967295` while the same run's `--json` said
    /// `cached_input: 5377696910` — understated by 1,082,729,615 tokens
    /// (20.1%), presented as exact, with no ceiling marker. No fixture anywhere
    /// went above `u32::MAX`, so nothing could have caught it.
    #[test]
    fn a_token_total_above_u32_max_is_reported_and_not_saturated() {
        let w = Window::resolve(None, None).unwrap();
        let mut f = facts();
        f.tokens = TokenClasses {
            uncached_input: 5_000_000_000,
            cached_input: 5_377_696_910,
            cache_creation: 4_294_967_296,
            output: 10,
        };
        let (rendered, json) = compile_review(&w, &f, ReviewProfile::Signals).unwrap();
        let wire = serde_json::to_string(&json).unwrap_or_default();
        for n in ["5000000000", "5377696910", "4294967296"] {
            assert!(rendered.contains(n), "{n} was not reported: {rendered}");
            assert!(wire.contains(n), "{n} was not on the wire");
        }
        assert!(
            !rendered.contains("4294967295"),
            "a saturated u32 reached the pack: {rendered}"
        );
    }

    #[test]
    fn a_hostile_path_in_a_review_is_neutralized_by_the_sealed_constructor() {
        let w = Window::resolve(None, None).unwrap();
        let mut f = facts();
        let hostile = "\u{1b}[2J\u{1b}]0;pwned\u{7}\u{202e}.env";
        f.files.push((hostile.to_owned(), 1));
        f.sensitive_paths = Some(vec![hostile.to_owned()]);
        let (rendered, json) = compile_review(&w, &f, ReviewProfile::Full).unwrap();
        let wire = serde_json::to_string(&json).unwrap_or_default();
        for needle in ["\u{1b}", "\u{7}", "\u{202e}"] {
            assert!(
                !rendered.contains(needle),
                "a control byte reached the pack"
            );
            assert!(!wire.contains(needle), "a control byte reached the wire");
        }
    }

    #[test]
    fn an_unknown_profile_is_refused_without_echoing_what_was_typed() {
        assert!(ReviewProfile::parse("nope").is_none());
        assert_eq!(ReviewProfile::parse("risk"), Some(ReviewProfile::Risk));
    }
}
