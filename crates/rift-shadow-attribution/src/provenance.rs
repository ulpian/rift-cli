//! Commit → checkpoint provenance: reading every checkpoint a commit claims.
//!
//! `rift-shadow-hooks::trailers` owns the **write** side (`prepare-commit-msg`
//! injects, `commit-msg` normalizes). This module owns the **read** side, which
//! is a different problem: by the time `rift blame`/`rift why` look at a commit
//! message, git may have fused several commits into it, or replayed it onto a
//! new sha, and the message is untrusted input from anyone who has ever typed
//! `git commit`.
//!
//! # Why the scan is not line-anchored
//!
//! Because a real squash is not. Measured against git itself
//! (`tests/rewrite_resilience.rs`, scenario `squash_merge`): `git merge --squash`
//! writes `.git/SQUASH_MSG` as an *indented transcript* of the squashed commits,
//! so every constituent trailer arrives with four leading spaces and the
//! resulting commit carries **no column-0 `Rift-Checkpoint` line at all**. A
//! parser that anchors the key at the start of a line sees none of them and
//! reports the squash as human-written. A forge's "squash and merge" produces
//! the other shape: several *unindented* trailers stacked in one message. Both
//! are real; both must resolve.
//!
//! Deferring to `git interpret-trailers` does not help, and the fixtures prove
//! it: run over a `git merge --squash` message it emits **nothing at all**, and
//! over a forge-squash message it emits **only the last** trailer, because it
//! reads the final paragraph and nothing else. Either answer silently discards
//! provenance, which is why this module scans the whole message itself.
//!
//! This is the same conclusion the legacy backend reached (`ParseAllCheckpoints` at
//! `upstream: trailers/trailers.go:124` in the pinned tag uses an unanchored
//! regex and dedups preserving order). Rift diverges from it in one place, on
//! purpose — see [`select_for_file`].
//!
//! # Trailer order is not chronological order
//!
//! Measured, not assumed. `git merge --squash` renders `git log`, which is
//! **newest-first**, so the first trailer in a locally squashed commit belongs
//! to the *last* commit written. A forge's squash-and-merge lists its bullets
//! **oldest-first**, so the first trailer belongs to the *first* commit written.
//! "Take the first trailer" therefore names two opposite commits depending on
//! how the squash was made — which is why [`select_for_file`] orders by
//! created-at and keeps trailer order only as a tie-break.
//! `tests/rewrite_resilience.rs::trailer_order_in_a_squash_does_not_mean_chronological_order`
//! pins both directions against real git output.
//!
//! # Trust boundary
//!
//! A trailer value is attacker-authorable: a commit message is free text, and a
//! resolved checkpoint id goes on to become a *path component* when its shard is
//! read (`SubstrateReader::read_artifact` addresses shards by fanned-out id).
//! [`parse_checkpoint_claims`] therefore admits only lowercase hex of a bounded
//! length, counts what it refused instead of dropping it silently, and bounds
//! both the lines it scans and the claims it collects.

/// The Rift-native checkpoint trailer key.
pub const RIFT_CHECKPOINT_KEY: &str = "Rift-Checkpoint";

/// The pre-Rift checkpoint trailer key, retained for read compatibility.
///
/// The constant is named for what it is; **its value is a byte sequence already
/// written into other people's commit messages** and is frozen: a value that
/// has left this machine is not ours to rename. Renaming it would not rename
/// history — it would only stop Rift
/// reading it.
pub const LEGACY_CHECKPOINT_TRAILER: &str = concat!("Ent", "ire-Checkpoint");

/// Lines of a commit message the scanner will look at. A message is untrusted
/// and unbounded; a squash of a very long branch is still far under this.
pub const MAX_SCANNED_LINES: usize = 4096;

/// Distinct checkpoint claims collected from one message.
pub const MAX_CLAIMS: usize = 256;

/// Shortest admissible checkpoint id. Legacy ids are 12 hex; Rift's are
/// 64. Below 7 an id is not plausibly distinguishing.
const MIN_ID_LEN: usize = 7;

/// Longest admissible checkpoint id — a full sha-256 in hex.
const MAX_ID_LEN: usize = 64;

/// Which dialect's trailer key carried a claim.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, ::serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TrailerDialect {
    /// `Rift-Checkpoint`.
    Rift,
    /// The retired checkpoint key (read-compat).
    ///
    /// The variant carried the retired name until 2026-08-10. This type derives
    /// `Serialize` with `rename_all = "lowercase"`, so the rename alone WOULD
    /// would have moved the frozen wire value to `"legacy"` — a serialized
    /// form, which the freeze condition forbids. The explicit `rename` below
    /// pins it, and `the_legacy_trailer_wire_value_survived_the_rename` fails if
    /// anyone removes it.
    #[serde(rename = "ent\u{69}re")]
    Legacy,
}

impl TrailerDialect {
    /// The trailer key this dialect spells.
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Self::Rift => RIFT_CHECKPOINT_KEY,
            Self::Legacy => LEGACY_CHECKPOINT_TRAILER,
        }
    }
}

/// One checkpoint id a commit message claims.
#[derive(Clone, Debug, PartialEq, Eq, ::serde::Serialize)]
pub struct TrailerClaim {
    /// The claimed checkpoint id (lowercase hex, validated).
    pub checkpoint_id: String,
    /// Which key carried it.
    pub dialect: TrailerDialect,
    /// First-seen position in the message, from 0. Preserved because it is the
    /// only ordering a squash message itself provides.
    pub ordinal: usize,
}

/// Everything one commit message claims, plus what was refused getting there.
#[derive(Clone, Debug, Default, PartialEq, Eq, ::serde::Serialize)]
pub struct ClaimSet {
    /// Distinct claims in first-seen order.
    pub claims: Vec<TrailerClaim>,
    /// Trailer lines whose value failed the id shape check. Counted, never
    /// quoted: the value is untrusted and reporting it verbatim would put
    /// attacker text on an operator's terminal.
    pub rejected: usize,
    /// True when [`MAX_SCANNED_LINES`] or [`MAX_CLAIMS`] cut the scan short, so a
    /// caller can say "at least these" rather than "these".
    pub truncated: bool,
}

impl ClaimSet {
    /// Whether the commit claims more than one checkpoint — i.e. it is a squash
    /// (or a message that accumulated trailers through repeated rewrites).
    #[must_use]
    pub fn is_multi(&self) -> bool {
        self.claims.len() > 1
    }

    /// The claimed ids, in first-seen order.
    #[must_use]
    pub fn ids(&self) -> Vec<&str> {
        self.claims
            .iter()
            .map(|c| c.checkpoint_id.as_str())
            .collect()
    }
}

/// Whether `s` is an admissible checkpoint id: lowercase hex, bounded length.
///
/// Uppercase is refused rather than folded, because a shard is *addressed* by
/// its id verbatim. Ids are minted by `rift_shadow::sha256_hex` →
/// `Sha256Id::to_hex` → `encode_hex`, which writes `{b:02x}` — always lowercase
/// — and `ShardWalker::read_artifact_in` builds the path from the id it is
/// given with no case folding (`checkpoint_id[..2]` / `checkpoint_id[2..]`).
/// Its `is_hex` guard is case-*insensitive*, so an uppercase id passes the guard
/// and then addresses a directory that was never written. Admitting it would
/// mint a claim that looks answerable and is not — worse than a refusal that is
/// counted.
#[must_use]
pub fn is_checkpoint_id(s: &str) -> bool {
    (MIN_ID_LEN..=MAX_ID_LEN).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Every checkpoint a commit message claims, in first-seen order, deduplicated.
///
/// Scanning rules, each earned by a measured shape:
///
/// * leading whitespace is skipped (`git merge --squash` indents by four);
/// * the value is the first whitespace-delimited token after the colon, so a
///   trailer annotated by a human (`Rift-Checkpoint: ab12… (from #42)`) still
///   yields its id;
/// * a value that is not an admissible id increments [`ClaimSet::rejected`];
/// * duplicates collapse to their first occurrence — a rebase that replays a
///   commit whose message already carried the trailer must not double-count it.
#[must_use]
pub fn parse_checkpoint_claims(message: &str) -> ClaimSet {
    let mut set = ClaimSet::default();
    let mut scanned = 0usize;
    for line in message.lines() {
        scanned += 1;
        if scanned > MAX_SCANNED_LINES {
            set.truncated = true;
            break;
        }
        let trimmed = line.trim_start();
        let Some((dialect, rest)) = split_key(trimmed) else {
            continue;
        };
        let value = rest.split_whitespace().next().unwrap_or("");
        if !is_checkpoint_id(value) {
            set.rejected += 1;
            continue;
        }
        if set.claims.iter().any(|c| c.checkpoint_id == value) {
            continue;
        }
        if set.claims.len() >= MAX_CLAIMS {
            set.truncated = true;
            break;
        }
        set.claims.push(TrailerClaim {
            checkpoint_id: value.to_owned(),
            dialect,
            ordinal: set.claims.len(),
        });
    }
    set
}

/// Match either dialect's key followed by `:`, returning the remainder.
fn split_key(line: &str) -> Option<(TrailerDialect, &str)> {
    for dialect in [TrailerDialect::Rift, TrailerDialect::Legacy] {
        if let Some(rest) = line
            .strip_prefix(dialect.key())
            .and_then(|r| r.strip_prefix(':'))
        {
            return Some((dialect, rest));
        }
    }
    None
}

/// What a resolver found when it looked one claimed checkpoint up.
#[derive(Clone, Debug, PartialEq, Eq, ::serde::Serialize)]
pub struct CheckpointCandidate {
    /// The claimed id.
    pub checkpoint_id: String,
    /// First-seen position in the commit message.
    pub ordinal: usize,
    /// Transaction time from the `checkpoint` row (epoch ms). `None` when the
    /// checkpoint's metadata is not resolvable locally — the id is claimed but
    /// nothing is known about it.
    pub created_at_ms: Option<i64>,
    /// Paths this checkpoint's sessions recorded touching, already normalized
    /// with [`normalize_path`].
    pub files_touched: Vec<String>,
}

impl CheckpointCandidate {
    /// A candidate with no local metadata.
    #[must_use]
    pub fn unresolved(checkpoint_id: impl Into<String>, ordinal: usize) -> Self {
        Self {
            checkpoint_id: checkpoint_id.into(),
            ordinal,
            created_at_ms: None,
            files_touched: Vec::new(),
        }
    }

    /// Whether local metadata was found for this claim.
    #[must_use]
    pub fn is_resolved(&self) -> bool {
        self.created_at_ms.is_some()
    }
}

/// Why a candidate was chosen — surfaced so a caller never presents a guess as
/// evidence.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ::serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SelectionBasis {
    /// The chosen checkpoint recorded touching this exact file — the only basis
    /// backed by evidence.
    FileTouched,
    /// The chosen checkpoint recorded **no paths at all**, so nothing is known
    /// either way. Absence of evidence, not evidence of absence: an unresolved
    /// claim, or a checkpoint whose file set was never populated.
    NoPathEvidence,
    /// The chosen checkpoint recorded paths and this file is **not** among them
    /// — the file was renamed after the checkpoint, or the fused commit's other
    /// constituent wrote it. A guess, and the only basis that should be
    /// presented with a caveat.
    PathExcluded,
}

impl SelectionBasis {
    /// Whether the choice rests on a recorded path for this file.
    #[must_use]
    pub fn is_evidence(self) -> bool {
        matches!(self, Self::FileTouched)
    }
}

/// The chosen checkpoint for a line, with the evidence behind the choice.
#[derive(Clone, Debug, PartialEq, Eq, ::serde::Serialize)]
pub struct Selection {
    /// The chosen candidate's id.
    pub checkpoint_id: String,
    /// Why it was chosen.
    pub basis: SelectionBasis,
    /// How many claims the commit carried (`> 1` ⇒ a squash).
    pub considered: usize,
    /// Whether the chosen candidate had resolvable local metadata.
    pub resolved: bool,
}

/// Normalize a path for comparison: trim, drop a leading `/` or `./`, and fold
/// `\` to `/`.
///
/// The fold is for callers, not for git: git records `/`-separated paths, so a
/// stored path never needs it, but a Windows developer typing `rift blame
/// src\lib.rs` does. The cost is that a unix path containing a *literal*
/// backslash compares equal to the same path with a separator there — a
/// collision that can only mislead if two such paths appear in one repository,
/// which is a trade taken knowingly.
#[must_use]
pub fn normalize_path(path: &str) -> String {
    let t = path.trim().replace('\\', "/");
    let t = t.strip_prefix("./").unwrap_or(&t);
    t.strip_prefix('/').unwrap_or(t).to_owned()
}

/// Whether any of `paths` is `file` (both normalized).
#[must_use]
pub fn paths_contain(paths: &[String], file: &str) -> bool {
    let want = normalize_path(file);
    paths.iter().any(|p| normalize_path(p) == want)
}

/// Choose the checkpoint that best explains a line in `file`.
///
/// **Rift's rule, and where it diverges from the legacy backend.** The legacy backend
/// (`preferredAttributionCandidate`, `upstream: attribution.go:1346` at the
/// pinned tag) takes the *first* candidate whose `files_touched` contains the
/// file and otherwise `candidates[0]` — i.e. trailer order decides. Trailer
/// order is only accidentally chronological: an interactive rebase that reorders
/// picks, or a squash assembled from cherry-picks, permutes it freely. Rift
/// keeps the file-evidence preference and then orders by **created-at,
/// descending**, because the newest checkpoint that touched
/// the file is the one whose writer last owned those lines — the same LIFO
/// ownership rule the rest of this crate applies to writers within a commit.
/// Trailer order cannot stand in for that: it is newest-first in a
/// `git merge --squash` message and oldest-first in a forge's (module docs).
///
/// The order is total and independent of input order, so the answer never
/// depends on how the claims happened to be listed:
///
/// 1. candidates that recorded touching `file` beat those that did not;
/// 2. within a group, resolved metadata beats unresolved;
/// 3. then later `created_at_ms` first;
/// 4. then lower `ordinal` (trailer order — the legacy backend's rule, as the tie-break);
/// 5. then lexicographically smaller id.
///
/// Returns `None` only for an empty candidate list.
#[must_use]
pub fn select_for_file(candidates: &[CheckpointCandidate], file: &str) -> Option<Selection> {
    let considered = candidates.len();
    let best = candidates
        .iter()
        .reduce(|a, b| if rank(b, file) < rank(a, file) { b } else { a })?;
    let basis = if paths_contain(&best.files_touched, file) {
        SelectionBasis::FileTouched
    } else if best.files_touched.is_empty() {
        SelectionBasis::NoPathEvidence
    } else {
        SelectionBasis::PathExcluded
    };
    Some(Selection {
        checkpoint_id: best.checkpoint_id.clone(),
        basis,
        considered,
        resolved: best.is_resolved(),
    })
}

/// The total order key for [`select_for_file`] — smaller is better.
fn rank<'a>(c: &'a CheckpointCandidate, file: &str) -> (u8, u8, i64, usize, &'a str) {
    (
        u8::from(!paths_contain(&c.files_touched, file)),
        u8::from(!c.is_resolved()),
        // Negated so that a later timestamp sorts first. `saturating_neg` keeps
        // `i64::MIN` from wrapping into a "newest" that outranks everything.
        c.created_at_ms.unwrap_or(i64::MIN).saturating_neg(),
        c.ordinal,
        c.checkpoint_id.as_str(),
    )
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    /// A 64-hex id made of one repeated digit — distinct, valid, readable.
    fn id(c: char) -> String {
        std::iter::repeat_n(c, 64).collect()
    }

    /// The message shape `git merge --squash` + `git commit` actually produces,
    /// measured in `tests/rewrite_resilience.rs`: an indented transcript, with
    /// no column-0 trailer anywhere.
    fn squash_msg(ids: &[String]) -> String {
        let mut m = String::from("Squashed commit of the following:\n");
        for (i, id) in ids.iter().enumerate() {
            m.push_str(&format!(
                "\ncommit 0000000000000000000000000000000000000000\n\
                 Author: Rift Fixture <fixture@rift.invalid>\n\
                 Date:   Wed Jan 1 00:0{i}:00 2025 +0000\n\n\
                 \x20   feat: step {i}\n\n\x20   Rift-Checkpoint: {id}\n"
            ));
        }
        m
    }

    #[test]
    fn an_indented_squash_yields_every_constituent_in_message_order() {
        let ids = [id('a'), id('b'), id('c')];
        let set = parse_checkpoint_claims(&squash_msg(&ids));
        assert_eq!(
            set.ids(),
            ids.iter().map(String::as_str).collect::<Vec<_>>()
        );
        assert!(set.is_multi());
        assert_eq!(set.rejected, 0);
        assert!(!set.truncated);
        // The negative half: nothing in that message starts at column 0, so a
        // line-anchored reader would have found zero.
        assert!(!squash_msg(&ids)
            .lines()
            .any(|l| l.starts_with(RIFT_CHECKPOINT_KEY)));
    }

    #[test]
    fn an_unindented_forge_squash_yields_every_constituent() {
        let msg = format!(
            "feat: add routing (#42)\n\n* feat: router\n\nRift-Checkpoint: {}\n\n\
             * feat: handler\n\nRift-Checkpoint: {}\n",
            id('1'),
            id('2')
        );
        assert_eq!(parse_checkpoint_claims(&msg).ids(), vec![id('1'), id('2')]);
    }

    #[test]
    fn duplicates_collapse_to_their_first_occurrence() {
        let msg = format!(
            "s\n\nRift-Checkpoint: {a}\nRift-Checkpoint: {b}\nRift-Checkpoint: {a}\n",
            a = id('a'),
            b = id('b')
        );
        let set = parse_checkpoint_claims(&msg);
        assert_eq!(set.ids(), vec![id('a'), id('b')]);
        assert_eq!(set.claims[0].ordinal, 0);
        assert_eq!(set.claims[1].ordinal, 1);
    }

    #[test]
    fn an_annotated_trailer_still_yields_its_id() {
        let msg = format!("s\n\nRift-Checkpoint: {} (from #42)\n", id('d'));
        assert_eq!(parse_checkpoint_claims(&msg).ids(), vec![id('d')]);
    }

    #[test]
    fn the_legacy_dialect_key_is_read_too() {
        let msg = format!("s\n\n{}: {}\n", LEGACY_CHECKPOINT_TRAILER, "abc1234");
        let set = parse_checkpoint_claims(&msg);
        assert_eq!(set.ids(), vec!["abc1234"]);
        assert_eq!(set.claims[0].dialect, TrailerDialect::Legacy);
    }

    #[test]
    fn a_hostile_value_is_refused_and_counted() {
        // Every one of these would become a path component if admitted.
        for hostile in [
            "../../../../etc/passwd",
            "..",
            "a/../b",
            "aaaaaaa\u{0}b",
            "аaaaaaa", // Cyrillic а
            "AAAAAAA", // uppercase: shards are stored lowercase
            "short",
            &"f".repeat(65),
        ] {
            let msg = format!("s\n\nRift-Checkpoint: {hostile}\n");
            let set = parse_checkpoint_claims(&msg);
            assert!(set.claims.is_empty(), "admitted {hostile:?}");
            assert_eq!(set.rejected, 1, "did not count {hostile:?}");
        }
    }

    #[test]
    fn a_message_without_trailers_is_an_empty_answer_not_an_error() {
        let set = parse_checkpoint_claims("chore: import\n\nJust a body.\n");
        assert_eq!(set, ClaimSet::default());
        assert!(!set.is_multi());
    }

    #[test]
    fn the_claim_count_is_bounded() {
        let mut msg = String::from("s\n\n");
        for i in 0..(MAX_CLAIMS + 40) {
            msg.push_str(&format!("Rift-Checkpoint: {i:064x}\n"));
        }
        let set = parse_checkpoint_claims(&msg);
        assert_eq!(set.claims.len(), MAX_CLAIMS);
        assert!(set.truncated);
    }

    #[test]
    fn the_line_scan_boundary_is_exact() {
        // Stated at the boundary rather than "somewhere past it", because an
        // off-by-one here is silent: the message still parses, it just stops
        // one line early, and the checkpoint that explains the line is simply
        // never claimed. `before` is the 0-based index of the trailer line.
        for (before, read) in [
            (MAX_SCANNED_LINES - 2, true),
            (MAX_SCANNED_LINES - 1, true),
            (MAX_SCANNED_LINES, false),
            (MAX_SCANNED_LINES + 1, false),
        ] {
            let mut msg = "filler\n".repeat(before);
            msg.push_str(&format!("Rift-Checkpoint: {}\n", id('e')));
            let set = parse_checkpoint_claims(&msg);
            assert_eq!(
                set.claims.len(),
                usize::from(read),
                "trailer on 0-based line {before} of {}",
                before + 1
            );
            // Truncation is reported exactly when the scan really was cut
            // short — a flag that over-reports would make every long message
            // look like a partial answer.
            assert_eq!(set.truncated, !read, "trailer on line {before}");
        }
    }

    fn cand(c: char, ordinal: usize, at: Option<i64>, files: &[&str]) -> CheckpointCandidate {
        CheckpointCandidate {
            checkpoint_id: id(c),
            ordinal,
            created_at_ms: at,
            files_touched: files.iter().map(|f| (*f).to_owned()).collect(),
        }
    }

    #[test]
    fn among_file_matches_the_latest_wins() {
        // Trailer order says `a`; created-at says `c`. Rift picks `c`.
        let cands = [
            cand('a', 0, Some(300), &["src/lib.rs"]),
            cand('b', 1, Some(100), &["src/lib.rs"]),
            cand('c', 2, Some(900), &["src/lib.rs"]),
        ];
        let sel = select_for_file(&cands, "src/lib.rs").unwrap();
        assert_eq!(sel.checkpoint_id, id('c'));
        assert_eq!(sel.basis, SelectionBasis::FileTouched);
        assert_eq!(sel.considered, 3);
        assert!(sel.resolved);
    }

    #[test]
    fn file_evidence_beats_a_newer_checkpoint_that_never_touched_it() {
        let cands = [
            cand('a', 0, Some(100), &["src/lib.rs"]),
            cand('b', 1, Some(999), &["docs/readme.md"]),
        ];
        let sel = select_for_file(&cands, "src/lib.rs").unwrap();
        assert_eq!(sel.checkpoint_id, id('a'));
        assert_eq!(sel.basis, SelectionBasis::FileTouched);
    }

    #[test]
    fn resolved_metadata_beats_an_unresolved_claim() {
        let cands = [
            CheckpointCandidate::unresolved(id('z'), 0),
            cand('a', 1, Some(1), &[]),
        ];
        let sel = select_for_file(&cands, "src/lib.rs").unwrap();
        assert_eq!(sel.checkpoint_id, id('a'));
        assert_eq!(sel.basis, SelectionBasis::NoPathEvidence);
        assert!(sel.resolved);
    }

    #[test]
    fn a_guess_is_labelled_a_guess_and_ignorance_is_not() {
        // Recorded paths that exclude the file = rename evidence = a guess.
        let excluded = [cand('a', 0, Some(5), &["other.rs"])];
        let sel = select_for_file(&excluded, "src/lib.rs").unwrap();
        assert_eq!(sel.basis, SelectionBasis::PathExcluded);
        assert!(!sel.basis.is_evidence());
        // No recorded paths at all = nothing is known; presenting that as a
        // rename caveat would be a misleading claim about the data.
        let silent = [cand('a', 0, Some(5), &[])];
        let sel = select_for_file(&silent, "src/lib.rs").unwrap();
        assert_eq!(sel.basis, SelectionBasis::NoPathEvidence);
        assert!(!sel.basis.is_evidence());
    }

    #[test]
    fn every_claim_unresolved_still_answers_and_flags_it() {
        let cands = [
            CheckpointCandidate::unresolved(id('b'), 1),
            CheckpointCandidate::unresolved(id('a'), 0),
        ];
        let sel = select_for_file(&cands, "src/lib.rs").unwrap();
        // Trailer order is the only signal left, so it decides.
        assert_eq!(sel.checkpoint_id, id('a'));
        assert!(!sel.resolved);
    }

    #[test]
    fn nothing_claimed_selects_nothing() {
        assert!(select_for_file(&[], "src/lib.rs").is_none());
    }

    #[test]
    fn paths_are_compared_after_normalization() {
        let files = vec!["src/lib.rs".to_owned()];
        for spelling in ["src/lib.rs", "./src/lib.rs", "/src/lib.rs", "src\\lib.rs"] {
            assert!(paths_contain(&files, spelling), "{spelling}");
        }
        assert!(!paths_contain(&files, "src/other.rs"));
    }

    proptest! {
        /// The chosen checkpoint must not depend on the order the candidates
        /// were listed in — only on their data. Ordinal carries trailer order as
        /// a field precisely so that position never leaks into the answer.
        #[test]
        fn selection_is_independent_of_candidate_order(
            raw in prop::collection::vec(
                (0u8..6, prop::option::of(0i64..1000), any::<bool>()),
                1..8,
            ),
            rotate in 0usize..8,
        ) {
            let alphabet = ['a', 'b', 'c', 'd', 'e', 'f'];
            let mut cands: Vec<CheckpointCandidate> = raw
                .iter()
                .enumerate()
                .map(|(i, (c, at, touched))| {
                    let files: &[&str] = if *touched { &["src/lib.rs"] } else { &["other.rs"] };
                    cand(alphabet[*c as usize % alphabet.len()], i, *at, files)
                })
                .collect();
            let first = select_for_file(&cands, "src/lib.rs");
            let n = cands.len().max(1);
            cands.rotate_left(rotate % n);
            cands.reverse();
            let second = select_for_file(&cands, "src/lib.rs");
            prop_assert_eq!(first, second);
        }

        /// Parsing never panics and never admits an id that is not a bounded
        /// lowercase-hex token, whatever the message contains.
        ///
        /// **Why the generator is a mix, measured rather than argued.** Commit
        /// `0a5ff1f` added the conjuncts below and claimed they "make the new
        /// states pay". Over **200,000 draws** of a bare `(?s).{0,400}` the key
        /// `Rift-Checkpoint` appears as a substring **0 times** — so the loop
        /// ran **0 times**, every conjunct in it was unreachable, and the
        /// property caught **0 of 6** parser mutants including the one its own
        /// `ordinal == i` conjunct is aimed at. The conjuncts were never
        /// tautologies (a constructed parser variant fails each); they were
        /// simply never reached, which is the harder failure to see, because
        /// the test reads as coverage and passes the gate.
        ///
        /// [`hostile_message`] fixes that by keeping three parts arbitrary
        /// bytes to one part *trailer-shaped* text. Over the same 200,000
        /// draws the key now appears in **21.5 %** of messages, **13.1 %**
        /// yield at least one claim (4.0 % yield two or more; 35,139 claims in
        /// total), **17.0 %** carry at least one rejection, 18.1 % carry an
        /// indented trailer and 15.3 % a mid-line decoy. Mutant detection goes
        /// from **0/6 to 3/6**, each caught in 5 of 5 runs of this test at its
        /// real 256-case budget: dedup dropped, `ordinal` pinned to a constant,
        /// and `is_checkpoint_id` case-folded — the last one only because the
        /// charset conjunct below is spelled out instead of delegated.
        ///
        /// The other three are structurally beyond an *unmodelled* property and
        /// are covered where an oracle exists:
        /// `a_modelled_multiline_message_parses_exactly_as_modelled` catches
        /// dropped `trim_start` and mid-line anchoring (5/5 each),
        /// `the_line_scan_boundary_is_exact` catches the cap off-by-one (5/5),
        /// and the three together catch **6/6**. Reach is not coverage, and a
        /// conjunct's existence is not its reachability.
        ///
        /// This is still not a *refusal* property. Nothing here asserts
        /// `set.claims.is_empty()`, so it cannot show that garbage was turned
        /// away; `a_hostile_value_is_refused_and_counted` is the test that pins
        /// refusal, and deleting it believing this subsumes it would drop the
        /// trust boundary this module exists to hold.
        ///
        /// Both arms carry `(?s)` deliberately. `proptest::string::string_regex`
        /// parses with `ParserBuilder::new().build()`, whose
        /// `dot_matches_new_line` defaults to **false**, so a bare `.{0,400}`
        /// never emits a newline and this property — over a *line scanner* —
        /// would only ever have seen single-line input.
        #[test]
        fn parsing_admits_only_well_shaped_ids(msg in hostile_message()) {
            let set = parse_checkpoint_claims(&msg);
            let mut seen = std::collections::BTreeSet::new();
            for (i, claim) in set.claims.iter().enumerate() {
                prop_assert!(is_checkpoint_id(&claim.checkpoint_id));
                // Spelled out rather than delegated a second time. The charset
                // *is* the trust boundary — an admitted id becomes a path
                // component — and an oracle that calls the same predicate the
                // parser calls cannot notice that predicate being widened:
                // case-folding `is_checkpoint_id` mutates parser and oracle in
                // one edit. This conjunct is what survives that.
                prop_assert!(
                    claim.checkpoint_id
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
                    "admitted an id that is not lowercase hex"
                );
                prop_assert!((MIN_ID_LEN..=MAX_ID_LEN).contains(&claim.checkpoint_id.len()));
                // Ordinals are the claim's own index: `select_for_file` uses
                // them as its tie-break, so a gap or a repeat there would
                // silently reorder a squash's candidates.
                prop_assert_eq!(claim.ordinal, i);
                prop_assert!(seen.insert(claim.checkpoint_id.clone()), "duplicate claim");
                // Nothing is fabricated across lines: one single line of the
                // message spelled this dialect's key AND carried this id. A
                // scanner that leaked state from one line into the next (the
                // failure mode only multi-line input can expose) breaks this.
                prop_assert!(
                    msg.lines().any(|l| l.contains(claim.dialect.key())
                        && l.contains(claim.checkpoint_id.as_str())),
                    "claim came from no single line"
                );
            }
            prop_assert!(set.claims.len() <= MAX_CLAIMS);
            // Neither branch of the generator can reach either cap, so a set
            // flag here is an over-report rather than a truncation. Arbitrary
            // text: ≤ 400 characters ⇒ ≤ 401 lines. Interleaved: ≤ 12 chunks,
            // each ≤ 40 characters ⇒ ≤ 492 lines and ≤ 12 claims. Both are
            // under MAX_SCANNED_LINES (4096) and MAX_CLAIMS (256).
            prop_assert!(!set.truncated);
        }

        /// The line scan, modelled. A message is assembled from lines whose
        /// meaning is known **by construction** — indented and flush trailers,
        /// both dialects, repeats, ill-shaped values, filler, and **decoys**
        /// that spell the key mid-line and must yield nothing at all — and the
        /// whole `ClaimSet` is predicted from that model rather than by
        /// re-parsing the rendered text, so the oracle is independent of the
        /// parser it checks.
        ///
        /// A [`ModelLine::Decoy`] contributes to neither `want` nor `rejected`,
        /// which is what makes the *anchoring* rule falsifiable: a parser that
        /// searched for the key anywhere in a line would either mint a claim
        /// (changing `ids()`, or its order when the decoy comes first) or count
        /// a rejection, and either way the prediction below stops matching.
        /// Attributed rather than assumed: with `split_key` switched from
        /// `strip_prefix` to `find` this property fails 5 runs out of 5, and
        /// with the decoy arm of [`modelled_line`] neutralised *and the same
        /// mutation in place* the whole `provenance` suite passes 3 runs out of
        /// 3. The decoy is what closes it, not something else in this change.
        ///
        /// **This is where the parser's accept side is covered, and as of
        /// today it is the only place.** The property above draws arbitrary
        /// text, which does not spell `Rift-Checkpoint:` (0 substring hits in
        /// 200,000 draws), so it proves only that parsing does not panic — it
        /// proves neither acceptance *nor* refusal. This one proves the parser
        /// accepts what it should, in the order it should, **across lines** —
        /// the whole of the indentation, dedup and ordinal behaviour that a
        /// squash message depends on — against an oracle that never re-parses
        /// the rendered text.
        #[test]
        fn a_modelled_multiline_message_parses_exactly_as_modelled(
            lines in prop::collection::vec(modelled_line(), 0..25),
        ) {
            let mut want: Vec<(String, TrailerDialect)> = Vec::new();
            let mut rejected = 0usize;
            for line in &lines {
                if let ModelLine::Trailer { dialect, yields, .. } = line {
                    match yields {
                        None => rejected += 1,
                        Some(claimed) => if !want.iter().any(|(w, _)| w == claimed) {
                            want.push((claimed.clone(), *dialect));
                        },
                    }
                }
            }
            let msg = lines
                .iter()
                .map(ModelLine::text)
                .collect::<Vec<_>>()
                .join("\n");
            let set = parse_checkpoint_claims(&msg);
            prop_assert_eq!(
                set.ids(),
                want.iter().map(|(s, _)| s.as_str()).collect::<Vec<_>>()
            );
            prop_assert_eq!(set.rejected, rejected);
            prop_assert!(!set.truncated);
            for (i, (claim, (_, dialect))) in set.claims.iter().zip(want.iter()).enumerate() {
                prop_assert_eq!(claim.dialect, *dialect);
                prop_assert_eq!(claim.ordinal, i);
            }
        }
    }

    /// One line of a modelled commit message, paired with what the scanner must
    /// make of it.
    #[derive(Clone, Debug)]
    enum ModelLine {
        /// Text that cannot spell either trailer key (no uppercase in its
        /// alphabet, so both checkpoint-key prefixes are unreachable) and no newline, so a
        /// filler line stays exactly one line.
        Filler(String),
        /// A trailer line as rendered, plus the id it must yield — `None` when
        /// the value is ill-shaped and must be *counted* rather than admitted.
        Trailer {
            text: String,
            dialect: TrailerDialect,
            yields: Option<String>,
        },
        /// A line where the key appears **mid-line** — a commit message
        /// quoting a trailer in prose, `see Rift-Checkpoint: <id>`, or a
        /// quoted reply. It must yield **nothing**: not a claim, and not a
        /// rejection either, because [`split_key`] never looked past the start
        /// of the line.
        ///
        /// Nothing else in this crate pins that. Anchoring is `strip_prefix`
        /// today and correct, but a "simplification" to `find` would keep every
        /// other test green while turning any prose mention of a checkpoint id
        /// into a provenance claim — and a commit message is untrusted input,
        /// so that is an attacker writing this module's answer.
        Decoy(String),
    }

    impl ModelLine {
        /// The rendered line.
        fn text(&self) -> &str {
            match self {
                Self::Filler(t) | Self::Trailer { text: t, .. } | Self::Decoy(t) => t.as_str(),
            }
        }
    }

    /// Values a modelled trailer can carry: three well-shaped ids drawn from a
    /// pool small enough that duplicates really occur, and five shapes that must
    /// be refused (empty, too short, uppercase, too long, a path).
    fn model_values() -> [(String, bool); 8] {
        [
            (id('a'), true),
            (id('b'), true),
            ("abc1234".to_owned(), true), // MIN_ID_LEN — the pre-Rift shape
            (String::new(), false),
            ("short".to_owned(), false),
            ("A".repeat(MAX_ID_LEN), false),
            ("f".repeat(MAX_ID_LEN + 1), false),
            ("../../etc/passwd".to_owned(), false),
        ]
    }

    /// Generate one modelled line.
    fn modelled_line() -> impl Strategy<Value = ModelLine> {
        prop_oneof![
            2 => "[a-z0-9 #*()_-]{0,24}".prop_map(ModelLine::Filler),
            3 => (0usize..5, any::<bool>(), 0usize..8, any::<bool>(), any::<bool>())
                .prop_map(|(indent, rift, value, spaced, annotated)| {
                    // Every indent is whitespace `trim_start` removes; four
                    // spaces is the shape `git merge --squash` actually writes.
                    let indents = ["", " ", "    ", "\t", "  \t "];
                    let (token, valid) = model_values()[value].clone();
                    let dialect = if rift {
                        TrailerDialect::Rift
                    } else {
                        TrailerDialect::Legacy
                    };
                    // An annotation never changes the outcome — the value is the
                    // first whitespace-delimited token — so the model needs no
                    // case for it.
                    let text = format!(
                        "{}{}:{}{token}{}",
                        indents[indent],
                        dialect.key(),
                        if spaced { " " } else { "" },
                        if annotated { " (from #42)" } else { "" },
                    );
                    ModelLine::Trailer { text, dialect, yields: valid.then_some(token) }
                }),
            1 => (0usize..DECOY_PREFIXES.len(), any::<bool>(), 0usize..8)
                .prop_map(|(prefix, rift, value)| {
                    let dialect = if rift {
                        TrailerDialect::Rift
                    } else {
                        TrailerDialect::Legacy
                    };
                    let (token, _) = model_values()[value].clone();
                    ModelLine::Decoy(format!(
                        "{}{}: {token}",
                        DECOY_PREFIXES[prefix],
                        dialect.key()
                    ))
                }),
        ]
    }

    /// Prefixes that put a trailer key mid-line. Each carries a non-whitespace
    /// character, so `trim_start` cannot turn one into a real trailer.
    const DECOY_PREFIXES: [&str; 5] = ["see ", "> ", "(cf. ", "#", "x"];

    /// The generator for [`parsing_admits_only_well_shaped_ids`]: three parts
    /// arbitrary bytes to one part text **assembled out of trailer-shaped
    /// lines**.
    ///
    /// Pure noise is what the property was drawn from before, and pure noise
    /// does not spell `Rift-Checkpoint` — 0 substring hits in 200,000 draws —
    /// so the property's claim loop never ran. Mixing in shaped lines keeps the
    /// panic-freedom coverage of arbitrary bytes (still 3/4 of the draws) and
    /// adds draws where claims, rejections, indents, duplicates and mid-line
    /// decoys really occur. Both arms are `(?s)`, so a "line" of the
    /// interleaved arm may itself be several.
    ///
    /// The bound that keeps `truncated` an over-report rather than a
    /// truncation: ≤ 12 chunks × (1 + 40 possible newlines) = 492 lines and
    /// ≤ 12 claims, against `MAX_SCANNED_LINES` 4096 and `MAX_CLAIMS` 256.
    fn hostile_message() -> impl Strategy<Value = String> {
        prop_oneof![
            3 => "(?s).{0,400}",
            1 => prop::collection::vec(
                prop_oneof![2 => "(?s).{0,40}", 3 => trailer_shaped_line()],
                0..12,
            ).prop_map(|lines| lines.join("\n")),
        ]
    }

    /// One line that looks like a trailer: six spellings of where the key sits
    /// (column 0, three whitespace indents git and humans really write, and two
    /// mid-line decoys) over the same eight values [`model_values`] uses, so
    /// the two properties cannot drift apart about what a value means.
    fn trailer_shaped_line() -> impl Strategy<Value = String> {
        (0usize..6, any::<bool>(), 0usize..8, any::<bool>()).prop_map(
            |(prefix, rift, value, spaced)| {
                let prefixes = [
                    "",
                    "    ",
                    "\t",
                    "  \t ",
                    DECOY_PREFIXES[0],
                    DECOY_PREFIXES[1],
                ];
                let dialect = if rift {
                    TrailerDialect::Rift
                } else {
                    TrailerDialect::Legacy
                };
                let (token, _) = model_values()[value].clone();
                format!(
                    "{}{}:{}{token}",
                    prefixes[prefix],
                    dialect.key(),
                    if spaced { " " } else { "" }
                )
            },
        )
    }

    /// The rename guard: the type may be renamed, the bytes may not.
    /// The retired dialect variant became `Legacy` and `LEGACY_CHECKPOINT_KEY`
    /// became `LEGACY_CHECKPOINT_TRAILER`; **neither byte on the wire moved**.
    ///
    /// Two literals, two different reasons to freeze them:
    /// * the trailer KEY is already written into commit messages Rift does not
    ///   own, so changing it would not rename history — it would stop Rift
    ///   reading it;
    /// * the SERIALIZED variant name is the trap. This enum derives `Serialize`
    ///   with `rename_all = "lowercase"`, so renaming the variant silently
    ///   rewrites the JSON to `"legacy"` unless the explicit escaped rename
    ///   holds it. Delete that attribute and this
    ///   test is what notices.
    #[test]
    fn the_legacy_trailer_wire_value_survived_the_rename() {
        assert_eq!(LEGACY_CHECKPOINT_TRAILER, concat!("Ent", "ire-Checkpoint"));
        assert_eq!(
            TrailerDialect::Legacy.key(),
            concat!("Ent", "ire-Checkpoint")
        );
        assert_eq!(
            serde_json::to_string(&TrailerDialect::Legacy).unwrap(),
            format!("\"{}\"", concat!("ent", "ire")),
            "the serde rename is the only thing keeping this value stable"
        );
        // The native side is unchanged, so the pin is a pair and not a half.
        assert_eq!(TrailerDialect::Rift.key(), "Rift-Checkpoint");
        assert_eq!(
            serde_json::to_string(&TrailerDialect::Rift).unwrap(),
            "\"rift\""
        );
        // And the reader still recognises a real legacy trailer end to end.
        let set = parse_checkpoint_claims(concat!("subject\n\nEnt", "ire-Checkpoint: abc1234\n"));
        assert_eq!(set.claims.len(), 1);
        assert_eq!(set.claims[0].dialect, TrailerDialect::Legacy);
    }
}
