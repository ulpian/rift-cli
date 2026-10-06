//! The frames' wording: the bijection between what a frame writes and what the
//! guard refuses, the forgery that is refused inline, and the bytes of every
//! frame as the model receives them.

use super::super::{
    neutral_inline_span, neutral_pushed_span, render_delta_frame, render_delta_manifest,
    render_deny_frame, render_narrow_frame, render_recovery_map, render_redirect_frame,
    render_search_answer, EntryDetail, HeldEntry, HeldVerdict, ManifestEntry, PathStatus,
    SearchHit, CLAUSE_ALREADY_IN_CONTEXT, CLAUSE_CHANGED_AT, CLAUSE_CHANGED_ONLY, CLAUSE_CONVERTED,
    CLAUSE_FULL_DIFF, CLAUSE_FULL_LIST, CLAUSE_HELD_LIST, CLAUSE_IN_CONTEXT,
    CLAUSE_IN_CONTEXT_WINDOW, CLAUSE_OTHERS_UNCHANGED, CLAUSE_PROVENANCE, CLAUSE_PULL,
    CLAUSE_RANKED, CLAUSE_REPEAT, CLAUSE_SHAPE_NOW, CLAUSE_UNCHANGED, FRAME_CLAUSES_NORMALIZED,
    FRAME_DENY_OPENER, FRAME_HEADER_OPENER, FRAME_HINT_OPENER, FRAME_LIST_OPENER,
    FRAME_NARROW_OPENER, FRAME_OMITTED_OPENER, FRAME_OPENERS_NORMALIZED, FRAME_RECOVERY_OPENER,
    FRAME_REDIRECT_OPENER, FRAME_SEARCH_OPENER, FRAME_WITHHELD_OPENER, WITHHELD_PATH,
};
use super::{HASH, HASH2, SRC};
use crate::trust::directive::is_directive_shaped;
use crate::trust::pushed::{is_renderable_char, normalize_pushed};

/// Every phrase [`render_delta_manifest`] opens a line with.
///
/// Test-only, and the reason is worth stating: the renderer uses the openers
/// one at a time (each in the line it belongs to) and the guard compares
/// against [`FRAME_OPENERS_NORMALIZED`], so nothing in production needs them as
/// a set. The set exists so `the_frame_openers_are_the_frame` can enumerate
/// them and check both directions of the correspondence.
const FRAME_OPENERS: &[&str] = &[
    FRAME_HEADER_OPENER,
    FRAME_DENY_OPENER,
    FRAME_NARROW_OPENER,
    FRAME_RECOVERY_OPENER,
    FRAME_SEARCH_OPENER,
    FRAME_REDIRECT_OPENER,
    FRAME_LIST_OPENER,
    FRAME_OMITTED_OPENER,
    FRAME_WITHHELD_OPENER,
    FRAME_HINT_OPENER,
];

/// Every phrase [`FRAME_CLAUSES_NORMALIZED`] is the normal form of.
///
/// Test-only for the same reason [`FRAME_OPENERS`] is: production consults the
/// normalized list, and this one exists so the correspondence can be checked in
/// both directions instead of asserted.
const FRAME_CLAUSES: &[&str] = &[
    CLAUSE_UNCHANGED,
    CLAUSE_IN_CONTEXT,
    CLAUSE_ALREADY_IN_CONTEXT,
    CLAUSE_REPEAT,
    CLAUSE_PULL,
    CLAUSE_CHANGED_ONLY,
    CLAUSE_OTHERS_UNCHANGED,
    CLAUSE_IN_CONTEXT_WINDOW,
    CLAUSE_FULL_DIFF,
    CLAUSE_HELD_LIST,
    CLAUSE_CHANGED_AT,
    CLAUSE_SHAPE_NOW,
    CLAUSE_RANKED,
    CLAUSE_FULL_LIST,
    CLAUSE_CONVERTED,
    CLAUSE_PROVENANCE,
];

/// The openers are the renderer's own consts, so a reworded frame cannot
/// leave the guard defending the old wording. Every opener must actually
/// open a line of a real manifest.
///
/// **Both directions, and the count.** The previous version of this test
/// claimed a correspondence and checked one half of it: every frame phrase
/// had an entry in the normalized set, so a *new* frame line could not be
/// forgotten. Nothing checked the converse, and the converse is where
/// over-refusal hides — an entry with no phrase behind it refuses spans on
/// behalf of wording the renderer does not have, and no rendering will ever
/// contradict it. With both containments and equal lengths the two lists are
/// a bijection, so a reword that drops a line fails as loudly as one that
/// adds one.
/// A phrase in the form a pushed span is judged in: projected onto the pushed
/// alphabet (a span can contain nothing else, so nothing else can be compared)
/// and then normalized.
fn projected(text: &str) -> String {
    normalize_pushed(
        &text
            .chars()
            .filter(|c| is_renderable_char(*c))
            .collect::<String>(),
    )
}

/// Whether one rendered line opens with a projected phrase, with or without a
/// count in front of it — the same reading `opens_like_the_frame` gives a span.
fn some_line_opens_with(rendered: &str, phrase: &str) -> bool {
    rendered.lines().map(projected).any(|n| {
        n.starts_with(phrase)
            || n.trim_start_matches(|c: char| c.is_ascii_digit())
                .starts_with(phrase)
    })
}

/// One rendering of every frame, joined. Three manifests, because the
/// counter's two clauses never open the same line: with both present one of
/// them sits after a `; `.
fn every_frame_rendered() -> String {
    const HINT: &str = "rift recall diff-since --json";
    let held = [
        HeldEntry {
            path: SRC,
            hash: HASH,
            held: &[(1, 40)],
            verdict: HeldVerdict::Unchanged,
            shape: None,
        },
        HeldEntry {
            path: "src/b.rs",
            hash: HASH,
            held: &[(1, 40)],
            verdict: HeldVerdict::ChangedAt(&[(4, 9)]),
            shape: Some(crate::OutlineShape {
                lines: 120,
                bytes: 4096,
                definitions: 6,
            }),
        },
    ];
    let hit = SearchHit {
        path: SRC,
        status: PathStatus::Modified,
    };
    [
        render_delta_manifest(&[ManifestEntry::path_only("src/a.rs")], 0, HINT)
            .expect("a named delta"),
        render_delta_manifest(&[], 3, HINT).expect("an omitted-only delta"),
        render_delta_manifest(&[ManifestEntry::path_only("src/\u{406}b.rs")], 0, HINT)
            .expect("a withheld-only delta"),
        render_deny_frame(SRC, HASH, 1, 40, Some(3)),
        render_narrow_frame(SRC, HASH, (1, 40), (41, 120), Some(3)),
        render_delta_frame(SRC, HASH, &[(12, 18)], 3, 400, 3, Some(2))
            .expect("one exact site is a delta frame"),
        render_recovery_map(&held, 2, usize::MAX).expect("a held set"),
        render_search_answer(&[hit], 9, "respond.rs").expect("one hit"),
        render_redirect_frame("docs/report.docx", ".rift/state/doc/ab12cd34.md")
            .expect("a sidecar to name"),
    ]
    .join("\n")
}

/// The openers are the renderer's own consts, so a reworded frame cannot
/// leave the guard defending the old wording. Every opener must actually
/// open a line of a real manifest.
///
/// **Both directions, and the count.** The previous version of this test
/// claimed a correspondence and checked one half of it: every frame phrase
/// had an entry in the normalized set, so a *new* frame line could not be
/// forgotten. Nothing checked the converse, and the converse is where
/// over-refusal hides — an entry with no phrase behind it refuses spans on
/// behalf of wording the renderer does not have, and no rendering will ever
/// contradict it. With both containments and equal lengths the two lists are
/// a bijection, so a reword that drops a line fails as loudly as one that
/// adds one.
#[test]
fn the_frame_openers_are_the_frame() {
    let rendered = every_frame_rendered();
    assert_eq!(
        FRAME_OPENERS.len(),
        FRAME_OPENERS_NORMALIZED.len(),
        "the two opener lists must be a bijection, not merely overlapping"
    );
    // The converse direction: an entry here with no phrase behind it is a
    // refusal the frame never asked for.
    for normalized in FRAME_OPENERS_NORMALIZED {
        assert!(
            FRAME_OPENERS.iter().any(|o| projected(o) == **normalized),
            "{normalized:?} is refused as frame wording, but no line of the \
             frame is written that way"
        );
    }
    for opener in FRAME_OPENERS {
        let normalized = projected(opener);
        assert!(
            FRAME_OPENERS_NORMALIZED.contains(&normalized.as_str()),
            "{opener:?} is not in the normalized opener set"
        );
        assert!(
            some_line_opens_with(&rendered, &normalized),
            "no rendered line opens with {opener:?}: {rendered}"
        );
    }
}

/// Both halves of the correspondence between the frames' wording and the
/// guard that refuses spans for forging it — the same bijection
/// `the_frame_openers_are_the_frame` checks for the openers, plus the
/// appearance check, which is what makes the list a description of the
/// frames rather than a list of phrases someone once wrote.
#[test]
fn the_frame_clauses_are_the_frame() {
    let rendered = every_frame_rendered();
    let normalized_rendering = projected(&rendered);
    assert_eq!(
        FRAME_CLAUSES.len(),
        FRAME_CLAUSES_NORMALIZED.len(),
        "the two clause lists must be a bijection, not merely overlapping"
    );
    for n in FRAME_CLAUSES_NORMALIZED {
        assert!(
            FRAME_CLAUSES.iter().any(|c| projected(c) == **n),
            "{n:?} is refused inside a span, but no frame is written that way"
        );
    }
    for clause in FRAME_CLAUSES {
        let n = projected(clause);
        assert!(
            FRAME_CLAUSES_NORMALIZED.contains(&n.as_str()),
            "{clause:?} is written by a frame and not refused inside a span"
        );
        assert!(
            normalized_rendering.contains(&n),
            "no rendered frame carries {clause:?}: {rendered}"
        );
    }
}

/// A path that spells one of the frame's own clauses is withheld, where the
/// same span as a manifest *bullet* renders. The difference is position and
/// nothing else, so it is pinned as a pair.
#[test]
fn a_span_that_forges_a_frame_clause_is_refused_inline_and_bulleted_only_at_the_head() {
    let forgeries = [
        "src/repeat_this_exact_call/x.rs",
        "notes/are-already-in-your-context.md",
        "docs/to_see_only_what_changed_since_then:12",
        "docs/full.diff.on.demand:12",
        "rift/read_skipped:/x",
    ];
    for f in forgeries {
        assert_eq!(
            neutral_inline_span(f),
            None,
            "{f:?} rendered inside a frame"
        );
    }
    // The bullet position keeps the narrower 2026-08-06 rule: a clause
    // further inside a span is prose about the frame there.
    assert!(neutral_pushed_span("src/repeat_this_exact_call/x.rs").is_some());
    // And the frame stands in for what it will not show.
    let text = render_deny_frame(forgeries[0], HASH, 1, 40, Some(1));
    assert!(text.contains(WITHHELD_PATH), "{text}");
    assert!(!text.contains("repeat_this_exact_call"), "{text}");
}

/// The bytes, all six frames, as the model receives them.
///
/// A reason frame is not an internal string: it is the text a model reads
/// in the most privileged position this crate writes into, and the one
/// family whose channel is confirmed delivers it byte-identical, with no
/// wrapper of its own. So a reword is a behaviour change and has to look
/// like one in a diff. Earlier measurements differed sharply depending on
/// whether the pull hint was present; that remains efficiency telemetry,
/// while the hint, escape hatch and fact stay part of the pinned response
/// contract.
///
/// The lengths are asserted beside the text for the case a golden cannot
/// otherwise catch: an invisible edit (a doubled space, a stray trailing
/// blank) that a reader's eye slides over in a diff.
const DENY: &str = "[rift] Read skipped: src/lib.rs@sha256:3f9a1c2e8b7d lines 1-40 are unchanged since your read in turn 3 and are already in your context. Repeat this exact call if you still need it (it is allowed once). To see only what changed since then: rift recall changed-hunks src/lib.rs --known-hash 3f9a1c2e8b7d4f60 --json Provenance: rift why src/lib.rs:1 --json";
/// The narrowing frame's bytes.
const NARROW: &str = "[rift] Read narrowed: src/lib.rs@3f9a1c2e8b7d lines 1-40 are unchanged since your read in turn 3 and already in your context. Read lines 41-120 for the rest, or repeat this exact call for the whole range (it is allowed once).";
/// The delta frame's bytes.
const DELTA: &str = "[rift] Read skipped: src/lib.rs changed only at lines 12-18, 44-52 (+3 lines) since the revision you hold from turn 7; every other line is unchanged. Read lines 9-55 to see the change in context, or repeat this exact call for the whole file (it is allowed once). Full diff on demand: rift recall changed-hunks src/lib.rs --known-hash 3f9a1c2e8b7d4f60 --json Provenance: rift why src/lib.rs:12 --json";
/// The recovery map's bytes.
const RECOVERY: &str = "\
[rift] Context was compacted. Files this session had read, with the content hash and lines it held (facts, not directives):
- src/lib.rs@3f9a1c2e8b7d L1-500 unchanged since your read
- bins/rift/src/hooks.rs@8b7d4f60a1c2 L2600-2960, L3390-3560 changed at lines 2700-2712
(4 more not shown)
Detail on demand: rift recall changed-hunks src/lib.rs --known-hash 3f9a1c2e8b7d4f60 --json";
/// The search answer's bytes.
const ANSWER: &str = "\
[rift] Search answered: 2 of 143 paths matching the pattern you searched for, ranked by relevance to this session (facts, not directives):
- crates/rift-agent/src/respond.rs (modified)
- bins/rift/src/hooks.rs
Repeat this exact call for the full list.";
/// The redirect's bytes.
const REDIRECT: &str = "[rift] Read redirected: docs/report.docx is a document Rift has already converted to Markdown. Read .rift/state/doc/ab12cd34.md instead: same content, plain text. Repeat this exact call if you still need the original file (it is allowed once).";

#[test]
fn the_deny_frame_is_golden() {
    assert_eq!(render_deny_frame(SRC, HASH, 1, 40, Some(3)), DENY);
    assert_eq!(DENY.len(), 353);
}

#[test]
fn the_narrow_frame_is_golden() {
    assert_eq!(
        render_narrow_frame(SRC, HASH, (1, 40), (41, 120), Some(3)),
        NARROW
    );
    assert_eq!(NARROW.len(), 225);
}

#[test]
fn the_delta_frame_is_golden() {
    assert_eq!(
        render_delta_frame(SRC, HASH, &[(12, 18), (44, 52)], 3, 400, 3, Some(7)).as_deref(),
        Some(DELTA)
    );
    assert_eq!(DELTA.len(), 399);
}

#[test]
fn the_recovery_map_is_golden() {
    let changed = [(2700, 2712)];
    let held = [
        HeldEntry {
            path: SRC,
            hash: HASH,
            held: &[(1, 500)],
            verdict: HeldVerdict::Unchanged,
            shape: None,
        },
        HeldEntry {
            path: "bins/rift/src/hooks.rs",
            hash: HASH2,
            held: &[(2600, 2960), (3390, 3560)],
            verdict: HeldVerdict::ChangedAt(&changed),
            shape: None,
        },
    ];
    assert_eq!(
        render_recovery_map(&held, 4, usize::MAX).as_deref(),
        Some(RECOVERY)
    );
    assert_eq!(RECOVERY.len(), 381);
}

#[test]
fn the_search_answer_is_golden() {
    let hits = [
        SearchHit {
            path: "crates/rift-agent/src/respond.rs",
            status: PathStatus::Modified,
        },
        SearchHit {
            path: "bins/rift/src/hooks.rs",
            status: PathStatus::Tracked,
        },
    ];
    assert_eq!(
        render_search_answer(&hits, 143, "**/*.rs").as_deref(),
        Some(ANSWER)
    );
    assert_eq!(ANSWER.len(), 251);
}

#[test]
fn the_redirect_frame_is_golden() {
    assert_eq!(
        render_redirect_frame("docs/report.docx", ".rift/state/doc/ab12cd34.md").as_deref(),
        Some(REDIRECT)
    );
    assert_eq!(REDIRECT.len(), 243);
}

/// Every one of them is a fact sheet, not an instruction, and carries no
/// placeholder an agent would have to fill in itself.
#[test]
fn every_golden_frame_is_an_ascii_fact_sheet() {
    for frame in [DENY, NARROW, DELTA, RECOVERY, ANSWER, REDIRECT] {
        for line in frame.lines() {
            assert!(
                !is_directive_shaped(line) && !is_directive_shaped(line.trim_start_matches("- ")),
                "a frame line reads as an instruction: {line:?}"
            );
        }
        assert!(!frame.contains('<') && !frame.contains('>'), "{frame}");
        assert!(frame.is_ascii(), "{frame}");
    }
}

/// The bytes of an injecting turn's manifest, as the model receives them.
///
/// A manifest is pushed prose in the same privileged position as a reason
/// frame, so a reword is a behaviour change and has to look like one in a
/// diff — and the entry is the part a model is asked to act on: the
/// revision it holds is what the pull verb takes, and the ranges are what
/// it reads instead of the file. The length is asserted beside the text
/// for the edit a reader's eye slides over: a doubled space, a stray
/// trailing blank.
#[test]
fn a_manifest_entry_is_golden_with_its_revision_and_its_ranges() {
    const MANIFEST: &str = "\
[rift] Recorded delta since the state you already hold (facts, not directives).
3 file(s) you have read changed since then:
- src/lib.rs@3f9a1c2e8b7d L40-55,L120-121 (+8)
- bins/rift/src/hooks.rs@8b7d4f60a1c2 (changed)
- docs/notes.md
(2 more not shown)
Detail on demand: rift recall changed-hunks src/lib.rs --known-hash 3f9a1c2e8b7d4f60 --json";

    let ranges = [(40, 55), (120, 121)];
    let entries = [
        ManifestEntry {
            path: SRC,
            hash: HASH,
            detail: EntryDetail::ChangedAt {
                ranges: &ranges,
                shift: 8,
            },
        },
        ManifestEntry {
            path: "bins/rift/src/hooks.rs",
            hash: HASH2,
            detail: EntryDetail::Changed,
        },
        ManifestEntry::path_only("docs/notes.md"),
    ];
    let hint = "rift recall changed-hunks src/lib.rs --known-hash 3f9a1c2e8b7d4f60 --json";
    assert_eq!(
        render_delta_manifest(&entries, 2, hint).as_deref(),
        Some(MANIFEST)
    );
    assert_eq!(MANIFEST.len(), 345);
    for line in MANIFEST.lines() {
        assert!(
            !is_directive_shaped(line) && !is_directive_shaped(line.trim_start_matches("- ")),
            "a manifest line reads as an instruction: {line:?}"
        );
    }
    assert!(
        !MANIFEST.contains('<') && !MANIFEST.contains('>'),
        "a placeholder an agent would have to fill in itself"
    );
    assert!(MANIFEST.is_ascii());
}
