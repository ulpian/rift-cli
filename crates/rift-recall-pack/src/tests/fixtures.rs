//! The builders every subject reaches for: a request of each kind, the
//! envelopes and handles that fill one, the readers that parse a rendered pack
//! back, and the three red-team corpora.

use rift_recall_types::{ArtifactState, Envelope, Handle, Trust};
use rift_recall_validity::trust::{serialize_untrusted, NeutralDigest};

use crate::*;

pub(super) const ALL_KINDS: [PackKind; 3] =
    [PackKind::Context, PackKind::Review, PackKind::Investigation];

/// The degradation fixture: five body entries (three changed hunks, one
/// unchanged summary, one recommendation) plus the cost note, over one critical
/// fact. Shared by the snapshots and the budget-announcement tests, so both
/// describe the same pack.
pub(super) const DEGRADING_BODY_ENTRIES: usize = 6;

/// The trusted content the exact-state entry carries: the ONE thing in a pack
/// allowed to span lines, so the assertions below hold it out by name rather
/// than quietly excusing multi-line entries in general.
pub(super) const TRUSTED_BLOCK: &str = "fn benign() {\n    work();\n}";

/// The append-only prompt-injection corpus (shared with the chokepoint's own
/// red-team leg). Reached cross-crate: it is the ONE oracle, never duplicated.
const CORPUS: &str = include_str!("../../../rift-recall-validity/fixtures/injection_corpus.jsonl");

/// The evidence-pack corpus: hostile investigation SEEDS and hostile
/// agent FINDINGS. It is a second oracle for a second vector, not a copy of the
/// chokepoint corpus above — that one attacks `serialize_untrusted`, this one
/// attacks the evidence pack's own frame (its `seed:`/`finding:`/`scope:`/
/// `signal:`/`CRITICAL:` line shapes and its "unverified" tag).
const EVIDENCE_CORPUS: &str = include_str!("../../fixtures/investigation_corpus.jsonl");

/// The identifier corpus (A1+A2): hostile *names* rather than hostile bytes —
/// a committed filename, a revision, a handle, a summary digest. Trusted by
/// provenance, which is exactly why they were the hole.
const IDENTIFIER_CORPUS: &str = include_str!("../../fixtures/identifier_corpus.jsonl");

pub(super) fn req() -> PackRequest {
    let mut r = PackRequest::new(1200);
    r.push_critical(SectionEntry::critical(&CriticalFact::RevertedProvenance {
        evidence: EvidenceRef::at(1),
    }))
    .unwrap();
    r.push_changed(env_entry(
        &exact_env("src/retry.rs", "fn retry() { /* 3 hunks */ }"),
        Sensitivity::Normal,
    ))
    .unwrap();
    r.push_unchanged(env_entry(
        &summary_env("src/billing.rs", OUTLINE_DIGEST),
        Sensitivity::Normal,
    ))
    .unwrap();
    r.push_recommended(SectionEntry::from_handle(
        &file_handle("src/retry.rs"),
        ordinal_of("src/retry.rs"),
    ))
    .unwrap();
    r.set_cost_note(SectionEntry::cost_note(CostNote::RepeatedReadAreas))
        .unwrap();
    r
}

pub(super) fn review_req(budget: u32) -> PackRequest {
    let mut r = PackRequest::for_kind(PackKind::Review, budget);
    r.push_critical(SectionEntry::critical(
        &CriticalFact::SensitivePathTouched {
            evidence: EvidenceRef::at(2),
        },
    ))
    .unwrap();
    r.push_critical(SectionEntry::critical(
        &CriticalFact::LowAttributionConfidence { percent: 41 },
    ))
    .unwrap();
    r.push_review_scope(SectionEntry::review_scope(&ReviewScopeFact::CommitRange {
        base: "main".into(),
        head: "HEAD".into(),
    }))
    .unwrap();
    r.push_review_scope(SectionEntry::review_scope(&ReviewScopeFact::Volume {
        commits: 7,
        sessions: 3,
        files: 12,
    }))
    .unwrap();
    r.push_changed(env_entry(
        &exact_env(
            "src/retry.rs",
            "fn retry() {
    backoff();
}",
        ),
        Sensitivity::Normal,
    ))
    .unwrap();
    r.push_changed(env_entry(
        &summary_env("config/secrets.rs", "loader shape unchanged"),
        Sensitivity::Sensitive,
    ))
    .unwrap();
    for signal in [
        ReviewSignal::AttributionSplit {
            ai_lines: 120,
            mixed_lines: 30,
            human_lines: 50,
        },
        ReviewSignal::AttributionConfidence { percent: 41 },
        ReviewSignal::FailingTests {
            failures: 2,
            window_commits: 7,
        },
        ReviewSignal::TokenSpend {
            input: 18_000,
            output: 2_400,
            cache_read: 96_000,
            cache_write: 1_200,
        },
        ReviewSignal::RecallSavings {
            tokens_avoided: 41_000,
            tokens_injected: 3_100,
        },
    ] {
        r.push_review_signal(SectionEntry::review_signal(signal))
            .unwrap();
    }
    r.push_recommended(SectionEntry::from_handle(
        &file_handle("src/retry.rs"),
        ordinal_of("src/retry.rs"),
    ))
    .unwrap();
    r.set_cost_note(SectionEntry::cost_note(CostNote::RepeatedReadAreas))
        .unwrap();
    r
}

pub(super) fn finding(kind: FindingKind, text: &str, handle: &str) -> SectionEntry {
    let shape = FindingShape {
        kind,
        evidence: file_handle("src/retry.rs"),
    };
    SectionEntry::unverified_finding(
        &shape,
        &EvidenceRef::cite(1, &shape.evidence),
        &serialize_untrusted("transcript", text.as_bytes(), handle),
    )
}

pub(super) fn investigation_req(budget: u32) -> PackRequest {
    let mut r = PackRequest::for_kind(PackKind::Investigation, budget);
    r.push_critical(SectionEntry::critical(
        &CriticalFact::UnverifiedFindingsPresent,
    ))
    .unwrap();
    r.push_seed(SectionEntry::seed(&SeedDescriptor::Path))
        .unwrap();
    r.push_seed(SectionEntry::seed(&SeedDescriptor::ErrorSignature {
        sha256: "b".repeat(64),
    }))
    .unwrap();
    r.push_finding(finding(
        FindingKind::Lesson,
        "The backoff resets on every retry, so the ceiling never applies.",
        "log:toolu_a1",
    ))
    .unwrap();
    r.push_finding(finding(
        FindingKind::ErrorSignature,
        "thread panicked at 'attempt to subtract with overflow'",
        "log:toolu_a2",
    ))
    .unwrap();
    r.push_changed(env_entry(
        &exact_env("src/retry.rs", "fn retry() { backoff(); }"),
        Sensitivity::Normal,
    ))
    .unwrap();
    r.push_recommended(SectionEntry::from_handle(
        &file_handle("src/retry.rs"),
        ordinal_of("src/retry.rs"),
    ))
    .unwrap();
    r
}

pub(super) fn degrading_req(budget: u32) -> PackRequest {
    let mut r = PackRequest::new(budget);
    r.push_critical(SectionEntry::critical(&CriticalFact::RevertedProvenance {
        evidence: EvidenceRef::at(1),
    }))
    .unwrap();
    for i in 0..3 {
        r.push_changed(env_entry(
            &exact_env(
                &format!("src/changed{i}.rs"),
                "fn work() {
    // a hunk of changed lines
    do_the_thing();
}",
            ),
            Sensitivity::Normal,
        ))
        .unwrap();
    }
    r.push_unchanged(env_entry(
        &summary_env("src/stable.rs", OUTLINE_DIGEST),
        Sensitivity::Normal,
    ))
    .unwrap();
    r.push_recommended(SectionEntry::from_handle(
        &file_handle("src/retry.rs"),
        ordinal_of("src/retry.rs"),
    ))
    .unwrap();
    r.set_cost_note(SectionEntry::cost_note(CostNote::RepeatedReadAreas))
        .unwrap();
    r
}

pub(super) fn key_inputs() -> PackKeyInputs {
    PackKeyInputs {
        query: "task: fix retry".into(),
        ranked: vec![
            ("id1".into(), "hashA".into()),
            ("id2".into(), "hashB".into()),
        ],
        policy_version: 1,
        token_budget: 1200,
    }
}

/// The evidence-ordinal pass, for fixtures: derive a STABLE citation from the
/// envelope's own path, so two entries in one request stay distinguishable
/// exactly as their paths used to make them. Production assigns the ordinal
/// from the caller's ordered fact set (`WindowFacts::evidence`, the serve
/// path's ranked list); a fixture has no such list, and a counter would make
/// the number depend on push order — which is precisely what naming by ordinal
/// must never do.
pub(super) fn ordinal_of(path: &str) -> u32 {
    let mut h: u32 = 2_166_136_261;
    for b in path.bytes() {
        h ^= u32::from(b);
        h = h.wrapping_mul(16_777_619);
    }
    h % 900 + 1
}

/// [`SectionEntry::from_envelope`] with that citation.
pub(super) fn env_entry(e: &Envelope, s: Sensitivity) -> SectionEntry {
    SectionEntry::from_envelope(e, s, &EvidenceRef::at(ordinal_of(&e.path)))
}

pub(super) fn env(path: &str, state: ArtifactState) -> Envelope {
    Envelope {
        path: path.to_string(),
        trust: Trust::CodeFact,
        state,
        estimated_tokens_returned: 0,
    }
}

pub(super) fn exact_env(path: &str, content: &str) -> Envelope {
    env(
        path,
        ArtifactState::ExactIncluded {
            handle: handle_str(path),
            basis_sha256: sha(),
            content: content.to_string(),
            range: None,
        },
    )
}

/// A file-shape outline as the extractor writes one: the only digest a pack
/// renders as a summary line.
pub(super) const OUTLINE_DIGEST: &str =
    "12 lines, 300 bytes, 2 top-level defs\nfn charge L1-L8\nstruct BillingClient L10-L12\n";

pub(super) fn summary_env(path: &str, summary: &str) -> Envelope {
    env(
        path,
        ArtifactState::SummaryIncluded {
            handle: handle_str(path),
            basis_sha256: sha(),
            summary: summary.to_string(),
            summary_key: "k".into(),
        },
    )
}

pub(super) fn unchanged_env(path: &str) -> Envelope {
    env(
        path,
        ArtifactState::KnownUnchanged {
            handle: handle_str(path),
            basis_sha256: sha(),
            last_seen_turn: None,
        },
    )
}

pub(super) fn file_handle(path: &str) -> Handle {
    Handle::File {
        path: path.to_string(),
        sha256: sha(),
        range: None,
    }
}

pub(super) fn sha() -> String {
    "a".repeat(64)
}

pub(super) fn handle_str(path: &str) -> String {
    format!("file:{path}@sha256:{}", sha())
}

/// The number a `CRITICAL: … finding(s) …` line claims, if it claims one.
pub(super) fn claimed_findings(line: &str) -> Option<u32> {
    let (n, tail) = line.strip_prefix("CRITICAL: ")?.split_once(' ')?;
    tail.starts_with("finding(s)")
        .then(|| n.parse().ok())
        .flatten()
}

/// The count a pack announced, if it announced one.
pub(super) fn announced_drops(pack: &Pack) -> Option<u32> {
    pack.render()
        .lines()
        .find_map(|l| l.strip_prefix(BUDGET_TAG)?.split_once(' ')?.0.parse().ok())
}

/// Body ENTRIES the pack carries (each section line is one entry), less the
/// announcement, which is the compiler talking rather than an entry.
pub(super) fn body_entries(pack: &Pack) -> usize {
    pack.sections
        .iter()
        .filter(|s| !matches!(s.kind, SectionKind::CriticalTop | SectionKind::CriticalTail))
        .flat_map(|s| s.lines.iter())
        .filter(|l| !l.starts_with(BUDGET_TAG))
        .count()
}

/// Every constructor that takes a caller-supplied identifier, with the hostile
/// string threaded through it. All of them render exactly one line; the exact
/// state additionally carries [`TRUSTED_BLOCK`] after its header line, which is
/// the documented exception and is checked as one.
pub(super) fn entries_carrying(hostile: &str) -> Vec<(&'static str, SectionEntry)> {
    let digest = serialize_untrusted("tool_output", b"benign", hostile);
    let mut entries = envelope_entries_carrying(hostile);
    entries.extend(vocabulary_entries_carrying(hostile, &digest));
    entries
}

/// The envelope half of the census: every artifact state, entered through
/// [`SectionEntry::from_envelope`] with the hostile string as its path and,
/// where the state has one, its handle and its summary.
fn envelope_entries_carrying(hostile: &str) -> Vec<(&'static str, SectionEntry)> {
    vec![
        (
            "envelope/known_unchanged",
            env_entry(&unchanged_env(hostile), Sensitivity::Normal),
        ),
        (
            "envelope/summary",
            env_entry(&summary_env(hostile, hostile), Sensitivity::Normal),
        ),
        (
            "envelope/summary(sensitive)",
            env_entry(&summary_env(hostile, hostile), Sensitivity::Sensitive),
        ),
        (
            "envelope/exact",
            env_entry(&exact_env(hostile, TRUSTED_BLOCK), Sensitivity::Normal),
        ),
        (
            "envelope/handle",
            env_entry(
                &env(
                    hostile,
                    ArtifactState::AvailableByHandle {
                        handle: hostile.to_string(),
                        basis_sha256: None,
                        reason: rift_recall_types::OmitReason::NotRequested,
                    },
                ),
                Sensitivity::Normal,
            ),
        ),
    ]
}

/// The closed-vocabulary half of the census: every constructor that takes a
/// caller-supplied identifier by name rather than through an envelope, plus
/// the digest the untrusted chokepoint made of the hostile string as a handle.
fn vocabulary_entries_carrying(
    hostile: &str,
    digest: &NeutralDigest,
) -> Vec<(&'static str, SectionEntry)> {
    vec![
        // The two critical constructors are still in the table, and they are
        // the ONLY rows that no longer take `hostile` at all: their path field
        // became an ordinal, so the hostile string is
        // unrepresentable there rather than neutralized there. They stay so the
        // table remains a census of every constructor, and so that deleting the
        // ordinal-ization would redden the census rather than shrink it
        // silently.
        (
            "critical/reverted",
            SectionEntry::critical(&CriticalFact::RevertedProvenance {
                evidence: EvidenceRef::at(1),
            }),
        ),
        (
            "critical/sensitive",
            SectionEntry::critical(&CriticalFact::SensitivePathTouched {
                evidence: EvidenceRef::at(2),
            }),
        ),
        (
            "handle",
            SectionEntry::from_handle(
                &Handle::Log {
                    tool_use_id: hostile.to_string(),
                },
                5,
            ),
        ),
        ("neutral", SectionEntry::from_neutral(digest)),
        // `seed/path` and `finding` join `critical/*` as rows that no longer
        // take `hostile` at all: neither a path field nor a rendered handle is
        // left for it to arrive in. They stay in the census so
        // removing the ordinal-ization reddens it.
        ("seed/path", SectionEntry::seed(&SeedDescriptor::Path)),
        (
            "seed/checkpoint",
            SectionEntry::seed(&SeedDescriptor::Checkpoint {
                id: hostile.to_string(),
            }),
        ),
        (
            "scope/range",
            SectionEntry::review_scope(&ReviewScopeFact::CommitRange {
                base: hostile.to_string(),
                head: hostile.to_string(),
            }),
        ),
        (
            "finding",
            SectionEntry::unverified_finding(
                &FindingShape {
                    kind: FindingKind::Lesson,
                    evidence: Handle::Log {
                        tool_use_id: hostile.to_string(),
                    },
                },
                &EvidenceRef::at(1),
                digest,
            ),
        ),
    ]
}

/// An entry splits into its identifier-derived FRAME (the first line) and, for
/// the exact state alone, the trusted content after it. The tests below strip
/// the trusted half off by name rather than relaxing the property to "some
/// entries may span lines".
pub(super) fn split_frame(text: &str) -> (&str, Option<&str>) {
    text.split_once('\n')
        .map_or((text, None), |(frame, rest)| (frame, Some(rest)))
}

pub(super) fn frame_and_content(entry: &SectionEntry) -> (&str, Option<&str>) {
    split_frame(entry.text())
}

pub(super) fn corpus() -> Vec<(String, Vec<String>)> {
    CORPUS
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with("//"))
        .map(|l| {
            let v: serde_json::Value = serde_json::from_str(l).expect("corpus line is JSON");
            let payload = v["payload"].as_str().expect("payload string").to_string();
            let forbidden = v["forbidden"]
                .as_array()
                .expect("forbidden array")
                .iter()
                .map(|s| s.as_str().expect("forbidden span string").to_string())
                .collect();
            (payload, forbidden)
        })
        .collect()
}

pub(super) fn evidence_corpus() -> Vec<(String, String, Vec<String>)> {
    EVIDENCE_CORPUS
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with("//"))
        .map(|l| {
            let v: serde_json::Value = serde_json::from_str(l).expect("corpus line is JSON");
            let vector = v["vector"].as_str().expect("vector string").to_string();
            let payload = v["payload"].as_str().expect("payload string").to_string();
            let forbidden = v["forbidden"]
                .as_array()
                .expect("forbidden array")
                .iter()
                .map(|s| s.as_str().expect("forbidden span string").to_string())
                .collect();
            (vector, payload, forbidden)
        })
        .collect()
}

pub(super) fn identifier_corpus() -> Vec<(String, String, String)> {
    IDENTIFIER_CORPUS
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with("//"))
        .map(|l| {
            let v: serde_json::Value = serde_json::from_str(l).expect("corpus line is JSON");
            let field = v["field"].as_str().expect("field string").to_string();
            let value = v["value"].as_str().expect("value string").to_string();
            let why = v["why"].as_str().expect("why string").to_string();
            (field, value, why)
        })
        .collect()
}
