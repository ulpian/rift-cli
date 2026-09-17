//! Capture inclusion controls: the `.worktreeinclude` opt-in.
//!
//! Git-ignored files are invisible to every plane Rift builds on git, so work an
//! agent does in one is invisible too. The legacy backend's answer is a repo-root
//! `.worktreeinclude` naming ignored paths to fold back into the shadow
//! snapshot; at the pinned tag its own file holds exactly two literal paths
//! (`.claude/settings.local.json`, `mise.local.toml` — fetched from
//! `.worktreeinclude` at the tag root). Rift takes the surface and inverts the
//! default: the legacy backend snapshots the working tree and re-includes exceptions, while
//! Rift captures **nothing** from the working tree unless this file names it. A
//! repository without the file produces a byte-identical empty capture, which is
//! what every checkpoint written before this module also produced.
//!
//! These properties hold whatever the file says, and they are the point of the
//! module. Each names the tests that would fail if it were false, because this
//! module has now shipped three paragraphs that were true of its rule set and
//! false of the operating system, and the difference is what a probe would have
//! shown.
//!
//! * **The gate does not move.** Everything here is raw, local
//!   [`SecretBytes`]; the document this builds is the *input* to
//!   `rift-shadow-redact`, never an output. The opt-in widens what is captured,
//!   never what skips redaction — there is no path from this module to a durable
//!   write that does not pass `redact`, because the only thing it can return is
//!   `SecretBytes` and the only durable sink takes `RedactedBytes`.
//!   (`tests/inclusion_redaction.rs`, which asserts both halves: the local
//!   capture really took the secret, and the durable bytes do not hold it.)
//! * **One place decides what a path means.** Every path this module opens is
//!   built by `resolve`, component by component, out of the parse the
//!   operating system itself performs — and each check runs on the very
//!   component the returned path is then built from, so a guard and the
//!   operation it guards cannot be about different things. All three escapes
//!   this module has had were that one shape: a *name* checked while a *path*
//!   was opened; *spellings* checked while the kernel resolved *components*;
//!   components split on `\` while the kernel, on Unix, read `\` as an ordinary
//!   filename byte. (`only_one_place_in_this_module_turns_an_entry_into_a_path`
//!   pins the derivations this module would plausibly write and says in its own
//!   docstring what it cannot see — a `format!` + `.into()` re-derivation passes
//!   it — so read it as a tripwire, not a proof. This sentence used to say the
//!   test "counts the derivations and fails at two", which is the overstatement
//!   that test's docstring was written to retract.
//!   `resolve_only_ever_lands_inside_the_work_tree` searches the decision.)
//! * **Nothing this document records is a transcription.** The document is
//!   UTF-8 text, a filesystem deals in bytes, and where the two disagree the
//!   record is refused rather than approximated: file *content* that is not
//!   valid UTF-8 earns `binary`, a symlink *target* that is not earns
//!   `unsafe-link-target`, and a discovered *name* that is not is refused under
//!   a summary of its parent. The rule is [`Refusal::Binary`]'s and it was
//!   older than its own reach — content was refused on it while a target was
//!   transcribed into a link the filesystem does not hold and a name into a
//!   path the kernel does not have, both silently and neither bound by a test.
//!   **Rift does not store a lossy transcription of a thing while claiming to
//!   hold it**, and a record is that claim.
//!
//!   One lossy conversion survives, in `resolve`, and it cannot lose
//!   anything: its result is only ever *compared* — to `.git`, and to empty —
//!   while the path is pushed from the original component, and `.git` is four
//!   ASCII bytes, so nothing that fails UTF-8 can compare equal to it.
//!   (`nothing_this_module_records_is_a_lossy_transcription` pins all three
//!   refusals and that survivor by line, over three spellings — `_lossy`,
//!   `to_str`, and `Path::display`, the third added after it was found passing
//!   unseen. It makes a fourth *site in this module, spelled one of those three
//!   ways* a test edit; it is not a proof, it sees nothing outside this module,
//!   and its docstring names a live `.display()` durable write elsewhere in
//!   this crate rather than leaving the reader to assume there is none.)
//! * **`.git` is never capturable.** Not by a literal entry, not by a directory
//!   walk, not nested (`vendor/x/.git`), not through a case-variant spelling nor
//!   a Windows trailing-dot or trailing-space one, and not through a symlinked
//!   ancestor in **any** spelling: `g -> .git` with an entry `g/config`, and the
//!   same link named `g\h`, which is one component to the kernel on Unix and two
//!   on Windows. Git ships such a symlink through clone. Capturing a git dir
//!   would copy loose objects, refs and the index — the substrate's own state —
//!   into a checkpoint stored in that same substrate.
//!   (`a_git_path_is_refused_however_it_is_spelled`,
//!   `a_directory_root_never_walks_into_a_nested_git_dir`,
//!   `any_path_with_a_git_segment_is_refused`,
//!   `an_entry_under_an_ancestor_symlinked_at_the_git_dir_is_refused`,
//!   `a_backslash_named_ancestor_is_the_symlink_the_kernel_sees`, and the
//!   containment property, whose tree holds a `.git` with a credential in it.)
//!
//!   Two limits, stated because the `.git` half of this is a rule over *names*
//!   while a filesystem can offer other ways to address the same directory.
//!   First: a **hardlink** to `.git/config` under an innocuous name is a regular
//!   file at an accepted path with no symlink in its ancestry, and **is**
//!   captured under that name. Git cannot ship one — clone materialises regular
//!   files — so it takes a local `ln`, by which point the same hand could copy
//!   the bytes in directly. (`a_hardlink_is_captured_under_the_name_it_was_given`
//!   — measured, not inferred, which is how a limit stays true when rules move.)
//!   Second, and **open**: Windows can name a directory by an 8.3 alias
//!   (`GIT~1`), which is neither a `.git` segment nor a symlink, so nothing here
//!   would refuse it. It cannot be observed from a Unix host, so it is recorded
//!   as an unmeasured hypothesis needing a Windows probe rather than asserted in
//!   either direction — and the `.git` sentence above should be read as "by
//!   name, in every spelling this module can see". The other Windows aliasing
//!   route, an alternate data stream (`.git:$I30`), IS closed, by the `:` rule
//!   in the next property.
//! * **Symlinks are never followed — as a target and as an ancestor, with
//!   ancestors checked component-wise as the operating system parses them —
//!   and a link that is recorded is recorded exactly.** A link met by the walk
//!   — named by the list or discovered inside an
//!   included directory — is never followed: its target's bytes are never read
//!   and a symlinked directory is not descended. Following would let a one-line
//!   include file exfiltrate anything the developer can read (`ln -s
//!   ~/.ssh/id_rsa notes.txt`) and would silently escape the work tree.
//!
//!   **"Never followed" is unconditional; "recorded as an `l` record" is not,
//!   and the two are stated apart because they were once stated together.** A
//!   link earns an `l` record when the walk records it at all, and the screens
//!   below apply to a discovered name exactly as they do to a typed one: a
//!   symlink *named* `a\b` or `a:b` is recorded `x <path> unsafe-path`, one
//!   spelled with a `.git` segment `x <path> git-path`, and one whose target
//!   the record cannot hold **exactly** — not valid UTF-8, or a spelling that
//!   would move the record's own separator — `x <path> unsafe-link-target`.
//!   Every one
//!   of those is a refusal, not a link record — and in every one of them the
//!   link is still not followed, which is the property that matters.
//!   (`an_entry_that_two_platforms_would_read_differently_is_refused` for the
//!   discovered-name case;
//!   `a_discovered_link_the_record_cannot_hold_is_refused` for the
//!   discovered-target case, which also asserts the walk did not descend it;
//!   `a_link_target_that_is_not_utf8_is_refused_rather_than_transcribed` and
//!   `a_repository_can_ship_a_non_utf8_link_target_through_clone` for the
//!   transcription case, which was recorded as an `l` record naming a target
//!   the filesystem did not hold until a probe measured it.)
//!
//!   The same escape exists one component to the left, and it is subtler:
//!   `symlink_metadata` does not follow the FINAL component — that is what makes
//!   a link a link record — but the kernel follows every INTERMEDIATE one. So an
//!   entry spelled `cfg/id_rsa`, where `cfg` is a symlink to `~/.ssh`, would
//!   read a file that is not in the tree at all, under a name that says it is.
//!   Every ancestor component of an entry must therefore `lstat` as something
//!   other than a symlink, or the entry is refused `symlinked-parent`. Git
//!   stores a symlink as mode `120000` whether it points at a file or a
//!   directory, so a repository can ship this whole vector with an ordinary
//!   regular control file — no local `ln` required, unlike the hardlink above.
//!
//!   **Where the components are is the operating system's answer, not this
//!   module's.** `bs\dir/id_rsa` is two components on Unix and three on Windows;
//!   a guard that splits it the other way `lstat`s paths the kernel never
//!   resolves, answers "no symlinked ancestor", and watches the walk follow the
//!   link it just cleared. That was the backslash-component defect, and it is
//!   why the ancestor rule lives in `resolve` rather than beside it.
//!   (`an_entry_under_a_symlinked_ancestor_is_refused_not_followed`,
//!   `a_backslash_named_ancestor_is_the_symlink_the_kernel_sees`,
//!   `no_spelling_of_an_ancestor_symlink_reaches_bytes_outside_the_tree`, and
//!   both properties — each mutation-verified in both directions.)
//!
//!   Every check here is a `stat` before a `read`, so a tree being rewritten
//!   underneath a running hook can still change what the read finds. That is a
//!   statement about a window rather than about a behaviour, so no test asserts
//!   it; it is recorded as a known limit and not as a guarantee.
//! * **An entry that would name two different files on two platforms is
//!   refused.** `\` and `:` are path syntax on Windows (separator; drive and
//!   alternate-data-stream separator) and ordinary filename bytes on Unix, and
//!   `.worktreeinclude` is a repo-*portable* file, so an entry carrying either
//!   has no single meaning to capture. Git answers this the same way — `\` is
//!   an *escape* in both of its path dialects, so a rule or pathspec
//!   `src\main.rs` names `srcmain.rs` and never `src/main.rs` (measured, git
//!   2.50.1) — so the refusal narrows this file to the dialect it imitates.
//!
//!   **The two halves are not the same kind of rule.** The `\` half is
//!   belt-and-braces over the ancestor rule above: remove it and nothing
//!   escapes, measured by removing it. The `:` half is **load-bearing on
//!   Windows** and the only thing refusing an alternate-data-stream name
//!   there — `.git:$I30` and `a.txt:stream` carry no drive prefix and no
//!   separator, so they are one `Component::Normal` that `resolve` accepts
//!   and opens. A previous version of this bullet said the screen was
//!   belt-and-braces *in both bytes*; that was measured on `\`, on Unix, and
//!   generalized, which is how a guard gets deleted.
//!   The screen runs on walk-**discovered** names as well as typed ones, so a
//!   symlink already on disk named `a\b` is recorded `unsafe-path` rather than
//!   as a link. Nothing is captured either way and the record still names the
//!   path; it says "ambiguous spelling" where it might have said "symlink",
//!   which is the honest answer for a name a Windows checkout could not hold.
//!   (`an_entry_that_two_platforms_would_read_differently_is_refused`, whose
//!   second half asserts the decision underneath still holds for `\` and does
//!   *not* for `:`, and whose third asserts the discovered case; and
//!   `only_the_colon_screen_refuses_an_alternate_data_stream_name`, which runs
//!   on every leg and whose **Windows** run is what adjudicates the `:` half —
//!   the drive-prefix claim can only be wrong there, and only there does it
//!   fail.)
//! * **The control file is read only when it is a regular file in the tree.**
//!   `.worktreeinclude` is `symlink_metadata`-checked before it is opened, so a
//!   *symlinked* control file is not read at all. This is the same door as the
//!   property above, one level up, and it is the one that costs the most: git
//!   stores a symlink as mode `120000` and clone recreates it, so a repository
//!   can **ship** `.worktreeinclude` pointing at `~/.aws/credentials`. A plain
//!   `read_to_string` follows it; every line of the victim's file becomes an
//!   include entry; and an entry that names nothing in the tree is echoed
//!   verbatim into the durable document as `x <line> missing` — the walk's care
//!   undone by the control file it obeys. The same check excludes directories,
//!   fifos, sockets and device nodes, and an oversize control file (over
//!   [`MAX_FILE_BYTES`]) is declined the same way rather than read into a git
//!   hook's memory.
//!
//!   **That case is silent: the empty list, no record** (a maintainer decision,
//!   2026-08-07). It reads against this module's own "refusals are recorded"
//!   habit, so here is the why. An `x .worktreeinclude not-a-file` record would
//!   put bytes into the capture of a repository that opted into *nothing*, and
//!   the capture is an input to the checkpoint id — so the no-op parity below,
//!   which keeps a non-participating repository's ids exactly where they were,
//!   would break for the one repository shape most likely to hit this by
//!   accident. Recording is a rule about paths the list *names*; the list itself
//!   is not one of them. What survives instead is the emptiness: the same bytes,
//!   and therefore the same checkpoint id, as no control file at all.
//!
//!   The guard is over the directory entry, not the inode behind it: a
//!   **hardlink** is a regular file and passes. Git cannot ship one — clone
//!   materialises regular files — so that costs local write access to the tree,
//!   and a local writer can just write the paths into an ordinary file.
//!
//! The document is a record stream, deliberately line-oriented so that
//! redaction — which substitutes fixed placeholders and can never *introduce* a
//! newline — cannot break its framing. Content lines carry a one-space prefix,
//! so no line of a captured file can be mistaken for a record header:
//!
//! ```text
//! rift-capture/1
//! f notes.txt
//!  first line
//!  second line
//! l link.txt -> ../outside/secret
//! x .git/config git-path
//! ```
//!
//! A file's content lines are `split('\n')` of its text, one line each; a reader
//! rejoins them with `\n` to recover the exact bytes (the inverse is exact,
//! including a trailing newline, which appears as a final empty content line).
//!
//! Each record header splits at the field the *writer* constrains, which is
//! what makes both rules exact rather than best-effort: an `x` record's reason
//! is the LAST whitespace-delimited field (the token comes from a closed set
//! and holds no whitespace, while a path may hold spaces), and an `l` record's
//! separator is the LAST ` -> ` — exact because the writer **reads its own
//! record back by that rule before writing it** and refuses
//! `unsafe-link-target` when the pair that comes out is not the pair that went
//! in. A symlink whose *name* holds ` -> ` therefore round-trips, and a target
//! that would move the last separator — one holding ` -> `, and equally one
//! that merely *begins* `-> `, which manufactures a second separator against
//! the one the writer emits — is refused rather than written ambiguously.
//!
//! Reading an `l` record back to the right *pair* is not the same as reading it
//! back to the right *target*, and the second needs its own rule: the document
//! is text, a symlink target is arbitrary bytes, so a target that is **not
//! valid UTF-8** is refused too rather than transcribed into the nearest
//! spelling — a transcription rsplits perfectly and names a target the
//! filesystem does not hold. See `Walk::link`, which is where the reasoning
//! lives, and which says what the substring version of this guard missed.
//!
//! **Compare a target's bytes, never its rendering.** An empty target earns
//! `unsafe-link-target`, but a target of one byte-order mark is valid UTF-8,
//! non-empty, not a control character and round-trips, so it is written — and
//! `l t_bom -> ` on a terminal is the empty target it is not. Every zero-width
//! character behaves this way. The guarantee is over bytes; a reader that
//! infers emptiness from a glyph is asking the document a question it does not
//! answer. (`a_target_that_renders_as_nothing_is_not_an_empty_target`.)

use std::path::{Component, Path, PathBuf};

use rift_core::SecretBytes;

/// The capture-side opt-in file, read from the working-tree root.
pub const INCLUDE_FILE: &str = ".worktreeinclude";

/// The capture document's first line — the framing version. Bump it when the
/// record grammar changes; a reader that does not recognise the header must
/// treat the document as opaque bytes rather than guess.
pub const CAPTURE_DOC_HEADER: &str = "rift-capture/1";

/// Most **records** one document may carry — captures and refusals alike. A
/// hook budget, not a security barrier.
///
/// It counted only captures until a measurement showed why that is not enough:
/// a per-path refusal spent nothing, so an opted-in directory of 600 binary
/// files produced 600 `binary` records and no budget ever stopped it. The
/// refusal is a line in the document exactly as a capture is, and a directory
/// whose every file is declined is otherwise unbounded narration — the failure
/// `Walk::exhausted` describes, reached by a road it did not cover.
pub const MAX_ENTRIES: usize = 256;

/// Largest single file this module reads. A captured file over it is refused
/// whole — never truncated, so a partial capture can never be mistaken for a
/// full one — and the control file itself is bounded by the same number, so a
/// repository cannot make a git hook read an arbitrary number of bytes.
///
/// # Why half the document budget (settled 2026-08-19)
///
/// **The property is fairness across files:** no single file may consume the
/// whole document allowance, so one hostile or accidentally-large file cannot
/// crowd out everything else the include list selected.
///
/// This was a flat 256 KiB, and when [`MAX_TOTAL_BYTES`] dropped to 8 KiB that
/// left the per-file cap **32× larger than the whole document budget**. The
/// refusal did not become unreachable — `Walk::file` tests this cap *before*
/// the budget, so a file over 256 KiB still answers `too-large`, and
/// `an_oversize_file_is_refused_whole_never_truncated` kept passing throughout.
/// What died was the PROPERTY: every file in the range where fairness actually
/// decides anything — from 8 KiB up to 256 KiB — was refused by the budget
/// instead, so the per-file cap contributed nothing to the reason it exists.
///
/// **The value is a fraction of the budget, not a number**, so it can never
/// again drift out of range when the budget moves. Half is the LARGEST fraction
/// that still guarantees the documented expected include set survives the worst
/// case: this module records that the legacy backend's own control file at the pinned tag
/// is **two literal paths**, and at half the budget two files sized exactly at
/// the cap both fit. A smaller fraction would guarantee more files while
/// refusing more real ones whole, and refusing whole is the expensive failure —
/// the operator asked for that file and gets nothing.
///
/// A file at exactly this limit costs **1.54 ms** of the 7.3 ms the whole
/// document has (4,096 B × 0.375 µs/B at the 2× documented on
/// [`MAX_TOTAL_BYTES`]) — **21% of the hook's legacy slack for one file**, and
/// the budget still caps the document as a whole at 42%.
pub const MAX_FILE_BYTES: u64 = MAX_TOTAL_BYTES as u64 / 2;

/// Largest total file content one document may **read**.
///
/// Charged on the read, not on the record: a file the walk read and then
/// declined (`binary`) still cost the hook the read, and 600 × 64 KiB of
/// declined files was 39 MB read inside a commit hook with no budget in the
/// way. Identical to "content carried" for every file that is captured, since
/// UTF-8 validation does not change a length.
///
/// **Tested** against the size `stat` already reported, so the file that would
/// overrun is refused rather than opened, and **charged** on the bytes actually
/// read — the two can differ only for a file rewritten between the two
/// syscalls. See `Walk::file`.
///
/// **What the redactor is handed is up to TWICE this, and that 2× is MEASURED,
/// not argued.** `Walk::file` prefixes every content line with one space, so a
/// file of N bytes contributes up to 2N + 2 to the document — worst case a file
/// that is all newlines, where every line is empty and the prefix is the whole
/// line. A full budget therefore measures a document **just over 16 KiB**, not
/// 8. The latency sweep measured the doubling end to end with a paired design
/// in which
/// each worst-case arm reads exactly its twin's bytes (asserted by a test):
/// **+79.3 ms at the ceiling from identical bytes read**, `redact` going
/// **65.28 → 138.50 ms (74% of hook-body time)**, and time spent *outside* the
/// measured bodies **unchanged between the twins (78.83 vs 77.94 ms)** — which
/// is what makes the added time attributable to the work rather than to
/// overhead. (`the_document_handed_on_is_up_to_twice_the_bytes_read`.)
///
/// # Why 8 KiB (settled 2026-08-19)
///
/// This was 1 MiB, chosen before anything had been measured. The sweep then
/// measured
/// the `prepare-commit-msg` budget at real opt-in sizes for the first time in
/// this repository's history, across thirteen sweeps with the box confirmed
/// clear at the start and end of each:
///
/// * **Nothing opted in is 92.7 ms median over 11 runs** against the 100 ms hard
///   budget — so **7.3 ms of headroom is the whole budget available to opt-in**,
///   not 1 MiB.
/// * Cost near the low end is **0.375 µs per byte read**, at the 2× above.
/// * A cap whose cost equals the headroom puts the median exactly on the line
///   and fails half the time. That is not a prediction: **51,200 B did it** —
///   median 100.6 ms, 2 of 4 over on a quiet box, then 11 of 13 over across ten
///   further runs.
/// * So the cap spends **at most half** the headroom: 3.65 ms ÷ 0.375 µs/B
///   ≈ 9,700 B, rounded down to **8 KiB ≈ 3.1 ms, 42% of headroom** — a 128×
///   reduction. **16 KiB is the defensible upper bound** (~84% of headroom,
///   median-level protection only); anything above it is unsupportable.
///
/// # What this constant CANNOT do
///
/// **No value of it makes the hook budget safe at p95**, and a reader should not
/// infer otherwise from the existence of a cap. The sweep's uninvited finding: the
/// zero-opt-in path is already over budget in **3 of 11 runs** before any
/// opted-in corpus exists, and roughly **78 ms of that is a process floor** — a
/// cost no change to this number moves. A cap bounds what opting in **adds**; it
/// cannot rescue a base path that already consumes the budget. The owner
/// ratified 8 KiB knowing this, and ruled that the result is recorded as
/// **p50 met, p95 not** with the floor named.
pub const MAX_TOTAL_BYTES: usize = 8 * 1024;

/// Deepest directory recursion under a listed directory root.
pub const MAX_DEPTH: usize = 16;

/// Why a path was not captured. Recorded in the document (an `x` record) rather
/// than dropped, so a checkpoint says what it declined and why.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// A `.git` segment — the substrate's own state, never capturable.
    GitPath,
    /// Empty, absolute, `..`-bearing, drive-qualified, or control-char bearing
    /// — or, for a name the walk discovered rather than the list typed, not
    /// valid UTF-8, which the document cannot spell at all. Every one of those
    /// is a refusal the path earns from its spelling, which is what this token
    /// means.
    ///
    /// **Neither of the module's two summary records belongs to this token.** A
    /// control-bearing path is summarised whole as `<N unprintable bytes>`
    /// (`Walk::refuse`); a discovered name that is not UTF-8 is summarised
    /// under its parent as `<parent>/<N unnameable bytes>` (`discovered`).
    /// The path field and the reason field are decided independently:
    /// `Walk::refuse` summarises the path whatever reason it was handed, and
    /// every caller able to hand it a summary charges the record cap **first**,
    /// so a summary is written under *whatever token the record earned*.
    /// Measured, one summary under three tokens — `unsafe-path` for a typed
    /// control-bearing entry, `git-path` for `.git/a\u{7}b` (this function's
    /// screen tests `.git` before path safety), and `too-many` for a
    /// control-bearing entry that arrives with the cap already spent
    /// (`Walk::visit` charges `Walk::cap_spent` before [`hard_refusal`]) —
    /// while `Walk::walk_dir` charges that same cap on the unnameable
    /// summary, so that one is the path field of a `too-many` record too. The
    /// token therefore tells a reader neither WHICH summary is in front of them
    /// nor WHETHER either one is.
    ///
    /// An earlier version said this token "carries BOTH of the module's summary
    /// records" — a true conclusion resting on a false premise, and the test it
    /// cited asserted only the `unsafe-path` instance, so it passed unchanged
    /// while the claim was false.
    /// (`a_summary_is_written_under_whatever_token_the_record_earned`.)
    ///
    /// **What IS true of the tokens is the other direction**, and it is stated
    /// in full at `Walk::recorded`: this token, `git-path` and `too-many` are
    /// the only three of the eleven that can ever appear on a summary record.
    /// The seven keyed tokens cannot, because every keyed exit is downstream of
    /// the control-character screen that makes a summary happen, and neither
    /// can `symlinked-parent`.
    /// (`a_keyed_record_always_names_its_path_literally`.)
    UnsafePath,
    /// An ancestor component of the entry is a symlink, so resolving the entry
    /// would leave the working tree through a door the spelling does not show.
    SymlinkedParent,
    /// Named but absent (or unreadable) at capture time.
    Missing,
    /// Not a regular file, directory or symlink (socket, fifo, device).
    NotAFile,
    /// A symlink whose target cannot be recorded on one line, or cannot be
    /// recorded *unambiguously*, or cannot be recorded *exactly*: not valid
    /// UTF-8 (the document is text, and a lossy transcription would name a
    /// target the filesystem does not hold — the rule [`Refusal::Binary`]
    /// states for content, one field over), empty, control-character bearing,
    /// or one that would make the assembled record read back as a different
    /// path/target pair than the one that went in — which covers a target
    /// holding the record's own ` -> ` and a target that merely begins `-> `.
    /// See `Walk::link`.
    UnsafeLinkTarget,
    /// Over [`MAX_FILE_BYTES`].
    TooLarge,
    /// Not valid UTF-8. Redaction reads text, so capturing binary bytes would
    /// store a lossy transcription while claiming to hold the file.
    ///
    /// The token is spelled `binary` and stays that way (it is in the format),
    /// but the test is UTF-8 validity and nothing else: a UTF-8 file is captured
    /// whatever it holds, and a latin-1 text file is refused. Prose that says
    /// "binary files are refused" is describing the token, not the rule.
    Binary,
    /// Over [`MAX_ENTRIES`].
    TooMany,
    /// Over [`MAX_TOTAL_BYTES`].
    BudgetSpent,
    /// Deeper than [`MAX_DEPTH`].
    TooDeep,
}

impl Refusal {
    /// The token written to the document (stable; part of the format).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::GitPath => "git-path",
            Self::UnsafePath => "unsafe-path",
            Self::SymlinkedParent => "symlinked-parent",
            Self::Missing => "missing",
            Self::NotAFile => "not-a-file",
            Self::UnsafeLinkTarget => "unsafe-link-target",
            Self::TooLarge => "too-large",
            Self::Binary => "binary",
            Self::TooMany => "too-many",
            Self::BudgetSpent => "budget-spent",
            Self::TooDeep => "too-deep",
        }
    }
}

/// One path segment as the platform that opens it will see it.
///
/// macOS and Windows both resolve `.GIT` to `.git`, and Win32 strips trailing
/// dots and spaces during path resolution — so `.git.` and `.git ` open the git
/// directory there, and `.. ` is the parent directory.
///
/// This exists because both of the module's segment questions — "is this the git
/// dir?" and "does this segment name a child at all?" — have to be asked about
/// the **same** string. They were not: the `.git` comparison trimmed and the
/// traversal check read the raw segment, so `..` was refused and `.. ` was not.
/// After the trim, "names no child" is one predicate rather than three, because
/// a segment built only from dots and spaces trims to nothing: `.`, `..`, `...`
/// and `.. ` are all empty here, and so is an empty segment.
fn segment_as_resolved(segment: &str) -> &str {
    segment.trim_end_matches(['.', ' '])
}

/// The refusal a path earns from its *spelling alone*, before the filesystem is
/// consulted at all. A **screen**, not the decision: `resolve` runs on
/// everything this accepts and is what says where a path actually lands, so
/// adding a rule here can only subtract from what is captured and can never
/// admit anything. That asymmetry is why a rule may live here on portability
/// grounds it could not justify as a security boundary.
///
/// `.git` is checked before path safety so a `.git` path is named as one; both
/// answers refuse, so the ordering is diagnostics, not policy. Segments are
/// split on `/` **and** `\` deliberately: on Unix that is a superset of the
/// operating system's own parse (`\` is an ordinary filename byte there), and a
/// superset of a refusal is still a refusal.
///
/// A `\` or a `:` **anywhere** in the path is refused outright, and that is the
/// one rule here that costs a developer something, so it is argued rather than
/// asserted. `.worktreeinclude` is a repo-*portable* file, and both bytes are
/// path syntax on Windows and ordinary filename bytes on Unix, so a line
/// carrying one means two different files depending on who ran the hook:
/// `src\main.rs` is `src/main.rs` to Windows and one file named `src\main.rs` to
/// Unix, and `a.txt:s` addresses an NTFS alternate data stream there and a file
/// called `a.txt:s` here. `resolve` will faithfully deliver each, which is
/// exactly the problem — a capture whose contents depend on the operator has no
/// honest semantics. Git already answers this question, and answers it this way
/// — measured on git 2.50.1 in a scratch repository, not read off the rule set,
/// because the sentence this replaces was read off the rule set and was wrong.
/// `\` is an **escape** in both of git's path dialects, so it never separates
/// and never names the file a Windows author meant. A `.gitignore` rule
/// `src\main.rs` ignores `srcmain.rs` (`check-ignore` exits 0) and does not
/// ignore `src/main.rs` (exit 1). The pathspec `src\main.rs` likewise reports
/// `srcmain.rs` — plus a file literally named `src\main.rs` where one exists,
/// since pathspecs match literally as well as by wildmatch — and never
/// `src/main.rs`. What it does **not** do is match nothing, which is what this
/// paragraph used to say. The index and `ls-files` are `/`-separated on every
/// platform, and a name containing a `\` is stored quoted, as the oddity it is.
/// (That neither byte can be checked out on Windows at all is the usual further
/// reason and is **not** measured here — no Windows host — so the refusal rests
/// on the two dialect measurements above, which are.) So the refusal narrows
/// this file to the dialect the file it imitates already has.
/// **The two bytes are not the same kind of rule, and the difference is the one
/// thing here worth reading twice.** This paragraph used to say the screen "is
/// belt-and-braces over `resolve`, never a substitute — every escape it
/// screens is independently refused there". That is true of `\`, on Unix, which
/// is all that was ever measured, and **false of `:` on Windows**:
///
/// * The `\` half **is** belt-and-braces. Delete it and nothing escapes —
///   measured by deleting it, not argued
///   (`an_entry_that_two_platforms_would_read_differently_is_refused` asserts
///   the `resolve` decision with the screen out of the way).
/// * The `:` half is **load-bearing on Windows**, and is the only thing
///   refusing an alternate-data-stream name there. `.git:$I30` and
///   `a.txt:stream` carry no drive prefix — Rust's own prefix parser wants an
///   ASCII letter at byte 0 (`sys/path/windows_prefix.rs::parse_drive`) — and
///   `:` is not a Windows path separator either, so [`Path::components`] hands
///   back **one** `Component::Normal`, `resolve` returns `Ok`, and the entry
///   is opened. Delete this rule and that route is open on the platform CI
///   tests. (`only_the_colon_screen_refuses_an_alternate_data_stream_name`
///   states it on every leg; the drive-prefix half can only be false on
///   Windows, so it is the Windows run that adjudicates this bullet.)
///
/// Whether opening `<work_tree>/.git:$I30` on NTFS really reaches the git dir's
/// index-allocation stream is a separate question, **unmeasured** — there is no
/// Windows host here — and recorded as a hypothesis rather than asserted, like
/// `GIT~1` in the module doc. The refusal does not depend on it.
///
/// The sentence this replaces mattered more than a doc nit: written as it was,
/// it invited a reader to delete a live guard on the grounds that something
/// else already covered it — argued from the rule set rather than probed, which
/// is the exact move that produced the backslash-component defect.
/// (`:` also subsumes the drive-letter check it replaced.)
#[must_use]
pub fn hard_refusal(rel: &str) -> Option<Refusal> {
    if rel
        .split(['/', '\\'])
        .any(|seg| segment_as_resolved(seg).eq_ignore_ascii_case(".git"))
    {
        return Some(Refusal::GitPath);
    }
    if rel.contains(['\\', ':'])
        || rel.chars().any(char::is_control)
        || rel
            .split(['/', '\\'])
            .any(|seg| segment_as_resolved(seg).is_empty())
    {
        return Some(Refusal::UnsafePath);
    }
    None
}

/// Where an entry lands in this tree — **the** path decision of this module.
///
/// Every path this module opens comes from here, and each check is run on the
/// very component the returned path is then built from, so the guard and the
/// operation cannot be about different things. That coupling is the whole
/// design. This module has had three escapes and all three were one shape: a
/// check written in a different vocabulary than the operation it guarded — a
/// *name* checked while a *path* was opened; *spellings* checked while the
/// kernel resolved *components*; components split on `\` while the kernel, on
/// Unix, read `\` as an ordinary filename byte and `bs\dir/id_rsa` as two
/// components, not three. A guard that re-derives what a path means will keep
/// drifting from what the operating system does, so this one does not derive:
/// [`Path::components`] is the same parse `open` will use, on whichever platform
/// is running (`a\b/c` is three components on Windows and two on Unix, and
/// `Components` says so on each).
///
/// The path is built by asking `join` for the whole thing and asking the OS to
/// take the working tree back off. An entry that is absolute, or drive-qualified
/// on Windows, replaces the root outright and fails to strip — that is the
/// refusal, not a spelling rule about leading slashes.
///
/// Refused, per component, in this order:
///
/// * a component that resolves to `.git` in any case — [`Refusal::GitPath`];
/// * a component that names no child (`..`, `.`, `...`, `.. `, empty), or a
///   root, prefix or parent component — [`Refusal::UnsafePath`];
/// * a non-final component that `lstat`s as a **symlink** —
///   [`Refusal::SymlinkedParent`]. `symlink_metadata` answers about the final
///   component only, which is what makes a symlink a link record instead of its
///   target's bytes, but the kernel follows every intermediate one. Git stores a
///   symlink as mode `120000` whether it points at a file or a **directory**,
///   and clone recreates both, so a repository can ship this whole vector with
///   an ordinary regular control file. The final component is deliberately left
///   alone: canonicalizing it would follow it, destroying preserve-not-follow.
///
/// The cost is one `lstat` per non-final component of every path visited. It is
/// paid on paths the walk discovered too, whose ancestors it has already
/// descended: "verified by construction" is exactly the kind of implicit
/// invariant that drifts, and the price of not relying on it is bounded by
/// [`MAX_DEPTH`] per path.
fn resolve(work_tree: &Path, rel: &str) -> Result<PathBuf, Refusal> {
    let Ok(added) = work_tree
        .join(rel)
        .strip_prefix(work_tree)
        .map(Path::to_owned)
    else {
        return Err(Refusal::UnsafePath);
    };
    let mut full = work_tree.to_path_buf();
    let mut components = added.components().peekable();
    let mut named_a_child = false;
    while let Some(component) = components.next() {
        let Component::Normal(name) = component else {
            return Err(Refusal::UnsafePath);
        };
        let text = name.to_string_lossy();
        let resolved = segment_as_resolved(&text);
        if resolved.eq_ignore_ascii_case(".git") {
            return Err(Refusal::GitPath);
        }
        if resolved.is_empty() {
            return Err(Refusal::UnsafePath);
        }
        named_a_child = true;
        full.push(name);
        if components.peek().is_some()
            && std::fs::symlink_metadata(&full).is_ok_and(|m| m.is_symlink())
        {
            return Err(Refusal::SymlinkedParent);
        }
    }
    if named_a_child {
        Ok(full)
    } else {
        // The entry resolved to the working tree itself: `.`, or empty, or
        // anything else that adds no component. "The whole tree" is what this
        // module's opt-in-nothing default exists to decline.
        Err(Refusal::UnsafePath)
    }
}

/// One include line, reduced to the path it means.
///
/// Every rewrite here is one the operating system would make anyway, on *both*
/// platforms, so that normalizing cannot change which file an entry names — the
/// last place this module still decided what a path meant. A trailing `/` is
/// "this directory", a run of `/` is one `/`, and a leading `./` is "here";
/// `Path::components` collapses all three identically on Unix and on Windows.
///
/// `\` is **not** among them, and used to be: the trailing trim and the `./`
/// strip both took it, so `src\` quietly became `src` — a rewrite that is right
/// on Windows and invents a different file on Unix, where `src\` is an ordinary
/// name. It now survives to earn the refusal [`hard_refusal`] gives it. That
/// also retires this function's part in that defect: collapsing separator RUNS across
/// both characters turned `bs\/dir` into `bs\dir`, manufacturing the exact
/// spelling the guard was blind to — a hardening change that widened the reach
/// because it was argued from the rule set instead of measured against the OS.
///
/// The rewrites that remain are here because a refusal record ECHOES the entry:
/// `./a.txt` used to be recorded as `x ./a.txt unsafe-path` (a refusal that
/// taught nothing, for a line naming a perfectly ordinary file) and `.//a.txt`
/// left `x /a.txt unsafe-path` — a path the developer never typed, reading like
/// an absolute one they had tried to capture. A bare `.` is left alone to earn
/// its refusal: that one really does mean "the whole tree", which this module's
/// opt-in-nothing default exists to decline.
fn normalize_entry(entry: &str) -> String {
    let trimmed = entry.trim().trim_end_matches('/');
    let mut collapsed = String::with_capacity(trimmed.len());
    let mut after_separator = false;
    for c in trimmed.chars() {
        let is_separator = c == '/';
        if !(is_separator && after_separator) {
            collapsed.push(c);
        }
        after_separator = is_separator;
    }
    let mut rest = collapsed.as_str();
    while let Some(shorter) = rest.strip_prefix("./") {
        rest = shorter;
    }
    rest.to_owned()
}

/// What a directory entry the walk **discovered** contributes: `Ok` is the
/// entry path to visit, `Err` is the refusal record it earns instead, because
/// the document cannot spell its name.
///
/// The document is UTF-8 text keyed by path, so a name that is not valid UTF-8
/// has no key. Transcribing it lossily invents a path the tree does not hold
/// (and, if a sibling is spelled with the replacement characters, silently
/// erases the entry fully — see [`Walk::walk_dir`]); dropping it silently
/// contradicts this module's rule that a refusal is recorded. So the record
/// names the parent, which the document *can* spell — a `rel` that reached
/// [`Walk::walk_dir`] passed [`hard_refusal`] and therefore holds no control
/// character — and counts the bytes it cannot print.
///
/// The `Err` summary is a diagnostic, not an identity, exactly like
/// [`Walk::refuse`]'s own: two unnameable siblings of the same length render
/// the same line, and a repository may legally ship a file named
/// `<6 unnameable bytes>`. A reader has the reason token and nothing else —
/// and the token does not settle it either, because a summary is written under
/// whatever token its record earned ([`Refusal::UnsafePath`]). So the document
/// does not tell those apart at all, which is the price of refusing to invent
/// a spelling; what it does hold is that a summary was written.
///
/// **This is a function so that the decision can be asserted where the
/// filesystem cannot be persuaded to produce the input.** APFS refuses a
/// non-UTF-8 filename outright (`EILSEQ`) and the macOS FAT driver
/// percent-encodes one back into ASCII, both measured, so on this host no
/// `read_dir` hands such a name to the loop; here it is handed one directly.
/// (`a_discovered_name_is_a_path_to_visit_or_a_refusal_to_record`, on every
/// leg; `a_directory_entry_that_cannot_be_named_is_refused_not_renamed`
/// end-to-end, adjudicated on the Linux leg.)
fn discovered(rel: &str, name: &std::ffi::OsStr) -> Result<String, String> {
    match name.to_str() {
        Some(child) => Ok(format!("{rel}/{child}")),
        None => Err(format!(
            "{rel}/<{} unnameable bytes>",
            name.as_encoded_bytes().len()
        )),
    }
}

/// The parsed `.worktreeinclude`: the roots capture is allowed to reach.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IncludeList {
    roots: Vec<String>,
}

impl IncludeList {
    /// Load `<work_tree>/.worktreeinclude`. An absent, unreadable, non-regular
    /// or oversize control file is an empty list — capture never fails on the
    /// control file that governs it, and never *follows* it. See the module
    /// doc's fourth property for why that silence is the whole answer.
    #[must_use]
    pub fn load(work_tree: &Path) -> Self {
        // Through [`resolve`] like every other path here, so the control file is
        // not the one path in the module with its own idea of what a name means.
        let Ok(path) = resolve(work_tree, INCLUDE_FILE) else {
            return Self::default();
        };
        // `symlink_metadata`, not `metadata`, and *before* the open: the check
        // is on the entry that lives in the tree, never on what it points at. A
        // symlink, a directory, a fifo, a socket and a device node all fail
        // `is_file`, and none of them is opened.
        //
        // The residual race — the entry swapped for a symlink between this stat
        // and the read below — needs a local writer in the tree, who can simply
        // put the paths in an ordinary control file instead. The threat this
        // closes is the one a *repository* can ship.
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            return Self::default();
        };
        if !meta.is_file() || meta.len() > MAX_FILE_BYTES {
            return Self::default();
        }
        std::fs::read_to_string(&path)
            .map(|text| Self::parse(&text))
            .unwrap_or_default()
    }

    /// Parse the file's text: one repo-relative path per line, `#` comments and
    /// blank lines ignored, a trailing `/` meaning "this directory", a leading
    /// `./` meaning nothing at all (see `normalize_entry`). Entries are
    /// deduplicated and sorted so the capture document is a function of the
    /// *set* named, never of the order it was typed in.
    ///
    /// Globs are deliberately not a syntax here. The legacy backend's own file at the pinned
    /// tag is two literal paths, literal paths plus directories cover it, and a
    /// matcher would have to come from `rift-config` — another crate and a new
    /// graph edge — to be the repo's one glob dialect rather than a second.
    #[must_use]
    pub fn parse(text: &str) -> Self {
        let mut roots: Vec<String> = text
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .map(normalize_entry)
            .filter(|l| !l.is_empty())
            .collect();
        roots.sort_unstable();
        roots.dedup();
        roots.truncate(MAX_ENTRIES);
        Self { roots }
    }

    /// The listed roots (sorted, deduplicated).
    #[must_use]
    pub fn roots(&self) -> &[String] {
        &self.roots
    }

    /// Whether nothing was opted in.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.roots.is_empty()
    }
}

/// Build the working tree's capture document: the opted-in files' content, the
/// links preserved as links, and every refusal named.
///
/// Returns **empty** bytes when nothing is opted in, which is byte-identical to
/// the capture every checkpoint carried before this module existed — a
/// repository that never writes `.worktreeinclude` sees no change at all, not
/// even in its checkpoint ids. Exactly two other roads end in those same empty
/// bytes, and both end there on purpose: a control file that is not a regular
/// file (module doc, fourth property), and a list that names only empty
/// directories (`Walk::walk_dir`). Every other outcome writes a record — a
/// refusal is content, and a document that holds one is not empty.
///
/// The result is [`SecretBytes`]: raw, local, un-`Debug`-able, and the input to
/// the one-way redaction gate. Nothing here writes anything durable.
#[must_use]
pub fn capture_document(work_tree: &Path) -> SecretBytes {
    let list = IncludeList::load(work_tree);
    if list.is_empty() {
        return SecretBytes::new(Vec::new());
    }
    let mut w = Walk {
        doc: Vec::new(),
        records: 0,
        bytes: 0,
        recorded: std::collections::BTreeSet::new(),
        stopped: false,
    };
    for root in list.roots() {
        w.visit(work_tree, root, 0);
    }
    if w.doc.is_empty() {
        return SecretBytes::new(Vec::new());
    }
    let mut out = Vec::with_capacity(w.doc.len() + CAPTURE_DOC_HEADER.len() + 1);
    out.extend_from_slice(CAPTURE_DOC_HEADER.as_bytes());
    out.push(b'\n');
    out.extend_from_slice(&w.doc);
    SecretBytes::new(out)
}

/// The in-progress document plus the budgets it spends. Deliberately no
/// `Debug`: `doc` holds raw file bytes until the gate runs.
struct Walk {
    doc: Vec<u8>,
    /// Lines written: captures **and** refusals. See [`MAX_ENTRIES`].
    records: usize,
    /// File bytes read, captured or not. See [`MAX_TOTAL_BYTES`].
    bytes: usize,
    /// Resolved paths the walk has already **decided about** — captures and
    /// per-path refusals alike. A second spelling of a path that earned
    /// `too-large`, `binary`, `missing`, `not-a-file`, `unsafe-link-target` or
    /// `too-deep` is dropped exactly as a captured one is: one record, not two,
    /// whatever that record says. `budget-spent` is keyed as well, and is
    /// counted with them below, but its drop is unobservable rather than
    /// behaviour — that record ends the walk ([`Walk::exhausted`]), so no second
    /// spelling is ever reached. Stated apart because "is keyed" and "can be
    /// seen to deduplicate" are different claims, and this docstring's whole
    /// history is claims that merged.
    ///
    /// `too-deep` needs the same care one step down, for anyone tempted to read
    /// the six as individually demonstrable. Its drop is real and its shape is
    /// **cross-token**: what a second naming of a `too-deep` directory earns is
    /// no record at all, never a second `too-deep` line. A directory is
    /// `too-deep` only when the walk *descended* to it, and depth is counted
    /// from the root that named it, so two namings of one directory arrive at
    /// two different depths — which is an argument, not a measurement, and is
    /// why the sentence above says "whatever that record says" rather than
    /// promising six tokens each visible twice.
    ///
    /// **The insert does NOT run "for every non-directory path, before the
    /// decision is made".** That was this docstring's stated reason, it is
    /// false, and it is the part that let one defect reopen at a third exit: the
    /// reason was read off the rule set rather than off the code, so it covered
    /// exits the code does not. What the code does is key at **two** sites.
    ///
    /// * [`Walk::visit`]'s insert, which runs *after* `symlink_metadata` has
    ///   answered and only for a non-directory. It therefore keys the capture
    ///   and every refusal **downstream of itself**: `not-a-file`,
    ///   [`Walk::link`]'s three and [`Walk::file`]'s four.
    /// * [`Walk::refuse_once`], at the three exits that write a record
    ///   **without reaching that insert**: [`Walk::walk_dir`]'s `too-deep` and
    ///   `missing`, and [`Walk::visit`]'s own `symlink_metadata` failure, which
    ///   returns three lines *before* the insert. That third one is this
    ///   cycle's fix. Unrouted it duplicated exactly as the two directory exits
    ///   did: a directory at mode `r--` is readable but not searchable, so
    ///   `read_dir` lists the child and `lstat` on the child fails, and `d/`
    ///   plus `d/x` wrote `x d/x missing` **twice** — two records, one path,
    ///   two charges against [`MAX_ENTRIES`].
    ///   (`a_path_named_twice_is_captured_once`, fifth shape.)
    ///
    /// **The enumeration, because reasoning from the rules has now been wrong
    /// here three times.** Sixteen **refusal** exits in this walk write a
    /// record. Strictly, eighteen lines write one: the two captures —
    /// [`Walk::link`]'s `l` and [`Walk::file`]'s `f` — are records too, and both
    /// are keyed, being downstream of [`Walk::visit`]'s insert. "Sixteen exits"
    /// therefore understated the keying rather than overstating it and licensed
    /// nothing unsafe, but this enumeration is about refusals, which is what its
    /// test name has said all along. Eleven of the sixteen are
    /// keyed: `missing` at all four of the sites that write it ([`Walk::visit`]'s
    /// `symlink_metadata`, [`Walk::link`]'s `read_link`, [`Walk::walk_dir`]'s
    /// `read_dir`, [`Walk::file`]'s `read`), plus `not-a-file`,
    /// `unsafe-link-target` at both of its sites, `too-deep`, `too-large`,
    /// `binary` and `budget-spent`. Five are not, and they are the five below.
    /// So the deduplicated half of this format can be stated **by token**: every
    /// token in that list is keyed wherever it is written, and every token in
    /// the unkeyed list is keyed nowhere. No token is keyed at one site and not
    /// at another — which is the property the old sentence was reaching for and
    /// asserted before it was true. The two lists are disjoint and their union
    /// is all eleven [`Refusal`] variants: a partition of the token space, not
    /// merely a non-overlap.
    ///
    /// **And a second property falls out of the same partition: a keyed record
    /// always names its path literally.** [`Walk::refuse`] summarises exactly
    /// when `rel` holds a control character; [`hard_refusal`] refuses every
    /// control character and answers *before* [`resolve`] runs; and all eleven
    /// keyed exits are downstream of it. So the summary branch cannot fire at a
    /// keyed exit, and a keyed record's path field is the path the walk decided
    /// about rather than a stand-in for it.
    ///
    /// The converse is **not** symmetric, and "the five unkeyed exits are the
    /// ones that carry summaries" would be false at one of them: [`resolve`]'s
    /// `Err` arm is unkeyed and also downstream of that screen, and it is the
    /// only exit that writes `symlinked-parent`. So by token, three of the
    /// eleven can ever appear on a summary record — `unsafe-path`, `git-path`
    /// and `too-many` — and the other eight cannot.
    /// (`a_keyed_record_always_names_its_path_literally` proves the three
    /// premises, exhaustively over the `char` space for the second;
    /// `a_captured_files_parent_always_resolves_inside_the_work_tree` searches
    /// for a counterexample per `x` record.)
    ///
    /// `parse` deduplicates entry *strings*, which does not catch a directory
    /// root and a file inside it (`d/` plus `d/a.txt` captured `f d/a.txt`
    /// twice, content and all, both counted against the caps).
    ///
    /// A directory is tracked **only where it earns a record of its own** —
    /// `missing` when it cannot be read, `too-deep` when it may not be
    /// descended — which is [`Walk::refuse_once`], now shared with the
    /// non-directory exit above. That is not a refinement: a previous version of
    /// this docstring said directories were deliberately untracked because
    /// walking one twice "records nothing of its own", and that sentence was
    /// **false at both of those exits**. `p` plus `p/d`, with `p/d` an
    /// unreadable directory, produced two identical `x p/d missing` records. A
    /// directory the walk *can* read still keys nothing, so the set grows with
    /// [`MAX_ENTRIES`] rather than with the shape of the tree: every insert, at
    /// either site, is followed by exactly one record.
    ///
    /// **What this set cannot key, and no reader may assume it does.** The
    /// identity here is the resolved path, so the five exits that write a record
    /// without one write it once per *naming* rather than once per path. Two are
    /// decided **before any path exists** — [`hard_refusal`] screens a spelling,
    /// and [`resolve`] can fail — and between them they carry `unsafe-path`,
    /// `git-path` and `symlinked-parent`. One is not a path **by construction**:
    /// [`Walk::walk_dir`]'s `<parent>/<N unnameable bytes>` summary, under
    /// `unsafe-path` again. Those three are pinned by
    /// `a_refusal_with_no_path_to_key_on_is_recorded_once_per_naming`.
    ///
    /// The remaining two are `too-many`, at both callers of [`Walk::cap_spent`]:
    /// [`Walk::visit`] charges the cap before [`resolve`] has run, and
    /// [`Walk::walk_dir`] charges it on the summary. Unkeyed like the rest, and
    /// unable to duplicate anyway, because [`Walk::exhausted`] sets `stopped`
    /// and a document holds at most one. They are enumerated rather than left
    /// out on that ground: "cannot duplicate" and "is keyed" are different
    /// claims, and collapsing them is the move this docstring is correcting.
    ///
    /// A path is also a *spelling* and never an inode — two hardlinked names are
    /// two records, and on a case-insensitive filesystem two case spellings of
    /// one file are two paths
    /// (`a_second_case_spelling_is_a_second_path_to_this_document`).
    recorded: std::collections::BTreeSet<PathBuf>,
    /// Set when a global budget ran out. The walk ends there rather than
    /// narrating the remainder — see [`Walk::exhausted`].
    stopped: bool,
}

impl Walk {
    /// Record one path, recursing into a directory. Every exit writes a record,
    /// so the document accounts for each path the list reached — up to the point
    /// a global budget ends the walk, and excepting a path a *previous* record
    /// already named (see [`Walk::recorded`]), which is accounted for by that
    /// record rather than twice.
    ///
    /// Two checks precede the filesystem answer, and they are different
    /// questions. [`hard_refusal`] screens the path's *spelling*, refusing some
    /// spellings that mean nothing good on any platform even where this one
    /// would resolve them harmlessly. Then [`resolve`] decides what the path
    /// **is** in this tree, and hands back the path to open — `rel` is never
    /// turned into a path again, by this function or any other, because the two
    /// derivations disagreeing is the defect the shape exists to prevent.
    fn visit(&mut self, work_tree: &Path, rel: &str, depth: usize) {
        if self.stopped {
            return;
        }
        // The record cap is charged here rather than beside each write, so a
        // refusal spends it exactly as a capture does.
        if self.cap_spent(rel) {
            return;
        }
        if let Some(why) = hard_refusal(rel) {
            self.refuse(rel, why);
            return;
        }
        let full = match resolve(work_tree, rel) {
            Ok(full) => full,
            Err(why) => {
                self.refuse(rel, why);
                return;
            }
        };
        // `symlink_metadata`, never `metadata`: the latter follows, which is the
        // exact behaviour this module exists to refuse.
        //
        // `refuse_once`, not `refuse`: this exit returns three lines before the
        // insert below, so the path it refuses would otherwise be decided about
        // without ever being keyed. See [`Walk::recorded`].
        let Ok(meta) = std::fs::symlink_metadata(&full) else {
            self.refuse_once(&full, rel, Refusal::Missing);
            return;
        };
        if meta.is_dir() {
            self.walk_dir(work_tree, &full, rel, depth);
        } else if !self.recorded.insert(full.clone()) {
            // Already captured under some other spelling of the same path.
        } else if meta.is_symlink() {
            self.link(&full, rel);
        } else if meta.is_file() {
            self.file(&full, rel, meta.len());
        } else {
            self.refuse(rel, Refusal::NotAFile);
        }
    }

    /// A symlink: preserved as its target string, never followed.
    ///
    /// **The record is `l <path> -> <target>`, and the reader's rule is the
    /// LAST ` -> ` on the line.** That rule is exact rather than best-effort,
    /// and this function is what makes it so. Both sides can contain the
    /// separator and both ship through git — a repository can carry a symlink
    /// *named* `a -> b` in its index, and clone materialises it, and a
    /// symlink's *target* is arbitrary bytes in a blob — so neither anchor is
    /// right in general: last-` -> ` misreads a target that holds one, first-
    /// ` -> ` misreads a name that holds one. The writer therefore constrains
    /// one side, and a link *named* `a -> b` round-trips exactly.
    ///
    /// **The constraint is the reader's own rule, applied to the assembled
    /// record, and it is written that way because the substring version of it
    /// was measurably wrong.** This function used to refuse a target that
    /// `contains(" -> ")` — but it also *emits* ` -> ` immediately before the
    /// target, so a target that merely **begins** `-> ` manufactures a second
    /// separator by concatenation, which a test of the target alone cannot
    /// see: `ln -s '-> y' link` is an ordinary blob that clone materialises,
    /// and the record `l arrowtarget -> -> y` rsplits to `("arrowtarget ->",
    /// "y")` when the truth is the path `arrowtarget` and the target `-> y`.
    /// **Both sides misread it, and the guard passed it.** So the record is
    /// assembled once and read back here by the same rule the reader will use;
    /// anything that does not come back as the pair that went in earns
    /// [`Refusal::UnsafeLinkTarget`]. A guard that has to anticipate spellings
    /// is the shape that has now failed this module four times — the check must
    /// be written in the vocabulary of the operation it guards, which here is
    /// the reader's parse, exactly as the path decision is written in the
    /// vocabulary of the OS's own parse. It is strictly narrower than the
    /// substring test (any target holding the separator still fails the
    /// round-trip) and it refuses nothing that reads back correctly: a target
    /// of `->`, or one ending ` -`, still round-trips and is still written.
    ///
    /// **The round-trip is a statement about the record's FRAMING and says
    /// nothing about whether the target is the one on disk, so UTF-8 validity
    /// is a separate rule rather than a consequence of it.** A target is
    /// arbitrary bytes — it is a blob — and this function used to record
    /// `to_string_lossy` of them, so `a\xFF\xFEb` became `a<U+FFFD><U+FFFD>b`:
    /// non-empty, control-free, and rsplitting back to exactly the pair that
    /// went in. **Well framed and false.** That is the trade
    /// [`Refusal::Binary`] refuses for file content, for the reason it gives
    /// there — this module does not store a lossy transcription of a thing
    /// while claiming to hold it — and a link target is now held to the same
    /// rule: a target that is not valid UTF-8 earns
    /// [`Refusal::UnsafeLinkTarget`] like any other target the record cannot
    /// hold. Strictly narrowing: everything written before this rule that is
    /// still written is written byte-for-byte as it was.
    /// (`a_link_target_that_is_not_utf8_is_refused_rather_than_transcribed`
    /// asserts the framing success first, so the two rules are visibly
    /// independent; `a_repository_can_ship_a_non_utf8_link_target_through_clone`
    /// measures the class through a real mode-`120000` index entry and a real
    /// clone, which is the "clone materialises it" this module asserts in prose
    /// in four places and had measured in none.)
    ///
    /// The target is the constrained side rather than the name because the name
    /// is the load-bearing datum (it says what is in the tree, and a refusal
    /// record still carries it either way), and because a file named `a -> b`
    /// is merely odd where a symlink target holding ` -> ` is only ever
    /// hostile. It is the same shape as the `x` record's reader rule — anchor
    /// on the side the writer constrains.
    /// (`a_link_record_is_read_back_by_its_last_arrow`, which drives all three
    /// directions that can break the rule.)
    fn link(&mut self, full: &Path, rel: &str) {
        let Ok(target) = std::fs::read_link(full) else {
            self.refuse(rel, Refusal::Missing);
            return;
        };
        // Never `to_string_lossy`: a target that is not UTF-8 has no spelling
        // in this document, and the lossy one is a DIFFERENT target that reads
        // back perfectly. See the paragraph above.
        let Some(target) = target.to_str().map(str::to_owned) else {
            self.refuse(rel, Refusal::UnsafeLinkTarget);
            return;
        };
        // The record a reader will see, assembled once, then read back by the
        // reader's rule. The bytes written below are these bytes, so the test
        // and the record cannot be about different strings.
        let record = format!("{rel} -> {target}");
        if target.is_empty()
            || target.chars().any(char::is_control)
            || record.rsplit_once(" -> ") != Some((rel, target.as_str()))
        {
            self.refuse(rel, Refusal::UnsafeLinkTarget);
            return;
        }
        self.records += 1;
        self.doc.extend_from_slice(b"l ");
        self.doc.extend_from_slice(record.as_bytes());
        self.doc.push(b'\n');
    }

    /// A directory root: walked in sorted order so the document is deterministic
    /// across filesystems that hand back entries in creation order.
    ///
    /// A directory contributes its children and nothing of its own, so an
    /// included directory that HAS no children contributes nothing at all — no
    /// record says the walk looked. The silence is chosen, not overlooked: an
    /// include file naming only empty directories still mints the checkpoint id
    /// the repository had before it opted in (the same parity the absent control
    /// file keeps), and the alternative would be a refusal token for a path that
    /// was not refused — the walk reached it and it held nothing. What was
    /// *asked* for is not a fact the document has to carry: `.worktreeinclude`
    /// sits in the repository beside the shard, and it is the question, not the
    /// answer.
    ///
    /// **A discovered name that is not valid UTF-8 is refused, not renamed.**
    /// This is [`Walk::link`]'s rule one component to the left and it was wrong
    /// the same way: the name was converted with `to_string_lossy`, so
    /// `na\xFF\xFEme` became the entry `d/na<U+FFFD><U+FFFD>me`, which [`resolve`]
    /// then rebuilt into a path the kernel does not have — recorded
    /// `x d/na<U+FFFD><U+FFFD>me missing`, a **false reason** for a file that
    /// exists, under a path nobody typed and the tree does not hold. It could be
    /// worse than mislabelled: a sibling genuinely named `na<U+FFFD><U+FFFD>me`
    /// converts to the *same* string, so the two entries dedup against each other
    /// in [`Walk::visit`] and the non-UTF-8 file leaves **no record at all** —
    /// a silent omission from a document whose whole job is to say what it
    /// declined. Both spellings ship in one index.
    ///
    /// The document is UTF-8 text keyed by path, so a name it cannot spell
    /// cannot be a key, and the honest record is "there is a child here I
    /// cannot name": `x <parent>/<N unnameable bytes> unsafe-path`, scoped
    /// under the parent, charged against both caps like any other record. The
    /// reason token is an existing one — this is a path refused on its spelling
    /// alone, which is what [`Refusal::UnsafePath`] means — so no reader's token
    /// set changes. That is the token this **exit** earns, not one the summary
    /// owns: the loop below charges the cap before it chooses the refusal,
    /// exactly as [`Walk::visit`] does, so a summary discovered with the cap
    /// already spent is the path field of a `too-many` record instead
    /// (`a_summary_is_written_under_whatever_token_the_record_earned`).
    ///
    /// Unreachable on macOS — APFS refuses such a name outright with `EILSEQ`
    /// and the FAT driver percent-encodes it back into ASCII, both measured —
    /// and reachable on Linux, so the Linux leg is what adjudicates the
    /// end-to-end assertion, the same posture as the `:` screen whose Windows
    /// run adjudicates it. The **decision** does not wait for that leg: it is
    /// [`discovered`], a function, handed the name directly.
    /// (`a_discovered_name_is_a_path_to_visit_or_a_refusal_to_record` on every
    /// leg; `a_directory_entry_that_cannot_be_named_is_refused_not_renamed`
    /// end-to-end.)
    fn walk_dir(&mut self, work_tree: &Path, full: &Path, rel: &str, depth: usize) {
        if depth >= MAX_DEPTH {
            self.refuse_once(full, rel, Refusal::TooDeep);
            return;
        }
        let Ok(read) = std::fs::read_dir(full) else {
            self.refuse_once(full, rel, Refusal::Missing);
            return;
        };
        // The raw names, never a transcription of them. Sorted as the OS
        // strings they are, which for every name this document can spell is the
        // byte order `String` sorts by — so no ordinary tree's document moves.
        let mut names: Vec<std::ffi::OsString> =
            read.filter_map(|e| e.ok().map(|e| e.file_name())).collect();
        names.sort_unstable();
        for name in names {
            if self.stopped {
                break;
            }
            match discovered(rel, &name) {
                Ok(child) => self.visit(work_tree, &child, depth + 1),
                Err(summary) => {
                    if self.cap_spent(&summary) {
                        break;
                    }
                    self.refuse(&summary, Refusal::UnsafePath);
                }
            }
        }
    }

    /// A regular file: captured whole, or refused whole.
    fn file(&mut self, full: &Path, rel: &str, len: u64) {
        if len > MAX_FILE_BYTES {
            self.refuse(rel, Refusal::TooLarge);
            return;
        }
        // Both caps are decided on the size `stat` already reported, BEFORE the
        // open: a file that cannot fit the remaining budget is not read in
        // order to discover that. It is the difference between 16 reads and 17
        // for the 600-file shape in [`Walk::exhausted`], and it makes "every
        // read is charged" exact — which is what lets the document's own record
        // counts stand for the reads, since nothing else can observe a read.
        // `len` is a `stat`, so a file rewritten between the two syscalls can
        // still be read at its new size (the window this module records as a
        // known limit, and already open at [`MAX_FILE_BYTES`]); that is why the
        // charge below is the bytes actually read rather than `len`.
        let size = usize::try_from(len).unwrap_or(usize::MAX);
        if self.bytes.saturating_add(size) > MAX_TOTAL_BYTES {
            self.exhausted(rel, Refusal::BudgetSpent);
            return;
        }
        let Ok(bytes) = std::fs::read(full) else {
            self.refuse(rel, Refusal::Missing);
            return;
        };
        // Charged before the UTF-8 test, so a file that is read and then
        // declined still spends what reading it cost. See [`MAX_TOTAL_BYTES`].
        self.bytes = self.bytes.saturating_add(bytes.len());
        let Ok(text) = String::from_utf8(bytes) else {
            self.refuse(rel, Refusal::Binary);
            return;
        };
        self.records += 1;
        self.doc.extend_from_slice(b"f ");
        self.doc.extend_from_slice(rel.as_bytes());
        self.doc.push(b'\n');
        for line in text.split('\n') {
            self.doc.push(b' ');
            self.doc.extend_from_slice(line.as_bytes());
            self.doc.push(b'\n');
        }
    }

    /// A global budget ran out at `rel`: name it once, then end the walk.
    ///
    /// Per-path refusals (`too-large`, `binary`, a `.git` path) are local — the
    /// walk carries on and the next path may well be captured. A spent *global*
    /// budget is not: every remaining path would earn the identical refusal, and
    /// narrating them is the failure mode this guards against. Opting in a
    /// directory like `node_modules/` would otherwise emit a refusal line per
    /// file — a multi-megabyte document, every byte of it re-scanned by the
    /// redactor on every commit hook, to say one thing 100 000 times. So the
    /// document says it once, at the path where the budget ran out, and stops.
    ///
    /// **This docstring described a road it did not cover, and the measurement
    /// is why both budgets moved.** A per-path refusal used to spend neither
    /// budget, so an opted-in directory of 600 binary files produced exactly the
    /// narration above — 600 refusal lines — while reading 39 MB to write them.
    /// Both caps now charge what actually happened: [`MAX_ENTRIES`] counts
    /// records rather than captures, and [`MAX_TOTAL_BYTES`] is charged on the
    /// read rather than on the record. **Those numbers were 16 reads — 1 MiB,
    /// the budget exactly — and 17 records, and they were computed at the
    /// 1 MiB / 256 KiB pair those two constants used to hold.** Both have since
    /// moved, and at the current pair a 64 KiB file
    /// exceeds [`MAX_FILE_BYTES`] outright, so the same 600 files are refused
    /// whole without a single read. The live numbers are not restated here —
    /// restating them is how this docstring came to be off by one in the first
    /// place — they are pinned, budget-relative, by
    /// `declined_files_spend_the_budgets_they_cost`. That count is 16 and not 17 because [`Walk::file`] decides the
    /// byte budget on the size `stat` already reported rather than on the bytes
    /// it read: the file that overruns is refused without being opened. This
    /// docstring said 17 records and **16 reads** while the code read 17 —
    /// off by one, and no test bound either number, which is the whole reason
    /// the read now happens after the decision instead of before it.
    /// (`declined_files_spend_the_budgets_they_cost` pins both numbers at the
    /// size that produces them, and pins the seventeenth file's refusal against
    /// a file the walk could not have read.)
    fn exhausted(&mut self, rel: &str, why: Refusal) {
        self.refuse(rel, why);
        self.stopped = true;
    }

    /// Is the record cap spent? `true` means the walk is over — it was spent,
    /// and the document says so at `rel`.
    ///
    /// One rule, two callers, deliberately: [`Walk::visit`] charges every path
    /// that has a spelling, and [`Walk::walk_dir`] charges a discovered name
    /// that has none. A second copy of `records >= MAX_ENTRIES` would be a
    /// second place deciding a cap, which is the shape that let a per-path
    /// refusal spend nothing at all (see [`Walk::exhausted`]).
    fn cap_spent(&mut self, rel: &str) -> bool {
        if self.records >= MAX_ENTRIES {
            self.exhausted(rel, Refusal::TooMany);
            return true;
        }
        false
    }

    /// Name a path that was reached but not captured, **unless some earlier
    /// record already decided about that path**.
    ///
    /// A directory contributes no record of its own when it can be read — its
    /// children speak for it — so [`Walk::visit`] never keys one, and for a
    /// long time that was harmless. It stopped being harmless the moment a
    /// directory could earn a record: a directory the walk cannot read
    /// (`missing`) or may not descend (`too-deep`) is a line in the document
    /// exactly as a file's refusal is, and `p` plus `p/d` named the same
    /// unreadable `p/d` twice — measured, two identical `x p/d missing`
    /// records, which falsified both this module's set claim and
    /// [`Walk::recorded`]'s own docstring.
    ///
    /// So the two exits that give a directory a record of its own charge the
    /// same key every other record charges: the resolved path. A directory the
    /// walk *can* read still keys nothing, so the set stays bounded by
    /// [`MAX_ENTRIES`] rather than by the shape of the tree — every insert here
    /// is followed by exactly one record, as every insert in [`Walk::visit`] is.
    /// (`a_path_named_twice_is_captured_once`, whose fourth shape is the
    /// counterexample.)
    ///
    /// **The third caller is not a directory**, and it is here because the
    /// remediation above fixed two exits and then described a property its own
    /// third exit falsified. [`Walk::visit`] refuses at its `symlink_metadata`
    /// failure three lines *before* the insert that keys everything downstream,
    /// so that exit had no key either, for the same structural reason and not
    /// for a directory-shaped one: this helper is "key it where the ordinary
    /// insert cannot", and whether the path is a directory was never what
    /// decided that. [`Walk::recorded`] carries the full sixteen-refusal-exit
    /// enumeration so the next reader checks the list instead of re-deriving it
    /// — which is how this was missed twice. (Fifth shape of the same test.)
    fn refuse_once(&mut self, full: &Path, rel: &str, why: Refusal) {
        if self.recorded.insert(full.to_path_buf()) {
            self.refuse(rel, why);
        }
    }

    /// Name a path that was reached but not captured. Spends a record, because
    /// a refusal is a line in the document exactly as a capture is.
    ///
    /// **A refusal record does not always name its path, and no reader may
    /// assume it does.** Two summaries can stand in for one: this one, for a
    /// path holding control characters, and [`discovered`]'s, for a discovered
    /// name that is not UTF-8. Both are diagnostics and neither is an identity
    /// — two distinct control-bearing paths of the same length render the
    /// identical line, and a repository may legally hold a file named exactly
    /// like either summary.
    ///
    /// **And neither summary belongs to a reason token.** The summary here is
    /// decided from the path alone and written under whatever `why` the caller
    /// passed, so the same line shape appears under `unsafe-path`, `git-path`
    /// and `too-many` — see [`Refusal::UnsafePath`], which is where that
    /// mistake was made and is now argued.
    /// (`a_summary_is_written_under_whatever_token_the_record_earned`.)
    ///
    /// The alternative is worse in both directions:
    /// echoing raw control bytes into a line-framed document, or inventing a
    /// spelling for a name the document cannot hold.
    fn refuse(&mut self, rel: &str, why: Refusal) {
        self.records += 1;
        // A refusal record carries the path, so a path too hostile to print on
        // one line is summarised rather than echoed.
        let safe: String = if rel.chars().any(char::is_control) {
            format!("<{} unprintable bytes>", rel.len())
        } else {
            rel.to_owned()
        };
        self.doc.extend_from_slice(b"x ");
        self.doc.extend_from_slice(safe.as_bytes());
        self.doc.push(b' ');
        self.doc.extend_from_slice(why.as_str().as_bytes());
        self.doc.push(b'\n');
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// The document as text (tests read raw local bytes; nothing durable here).
    fn doc(work_tree: &Path) -> String {
        String::from_utf8(capture_document(work_tree).as_slice().to_vec()).unwrap()
    }

    fn tree(files: &[(&str, &str)], include: Option<&str>) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (path, body) in files {
            let full = dir.path().join(path);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(full, body).unwrap();
        }
        if let Some(list) = include {
            std::fs::write(dir.path().join(INCLUDE_FILE), list).unwrap();
        }
        dir
    }

    /// The keyed half of [`Walk::recorded`]'s partition, as a value the
    /// assertions can search with. Every exit that writes one of these keys the
    /// path it names; the four in [`UNKEYED_TOKENS`] are keyed nowhere.
    ///
    /// **The split is asserted text and the union is not.** Which side a token
    /// belongs on is pinned by
    /// `every_refusal_exit_in_the_walk_is_enumerated_and_classified` (source
    /// order, labelled) and behaviourally by
    /// `a_path_named_twice_is_captured_once` — a reader who moved a token
    /// between these two lists would fail those, not this. But that the two
    /// lists are **disjoint and exhaustive over [`Refusal`]** is checked here,
    /// in `a_keyed_record_always_names_its_path_literally`: a twelfth variant,
    /// or a token in both halves, or a token in neither, reddens. The
    /// docstrings call that a partition of the token space, so a partition is
    /// what a test has to fail on.
    const KEYED_TOKENS: [&str; 7] = [
        "missing",
        "not-a-file",
        "unsafe-link-target",
        "too-deep",
        "too-large",
        "binary",
        "budget-spent",
    ];

    /// The other half: four tokens across five exits, and **the gap is not one
    /// token written twice** — the sentence here used to read "five exits
    /// (`too-many` is written at two of them)", which invites the arithmetic
    /// 5 − 1 = 4 and is wrong in both directions. Two of the five exits write
    /// whatever token their decider returned, so the map is many-to-many:
    /// `too-many` at two exits ([`Walk::visit`]'s record cap and
    /// [`Walk::walk_dir`]'s), `unsafe-path` at three ([`hard_refusal`]'s exit,
    /// [`resolve`]'s `Err` arm, and `walk_dir`'s unnameable summary),
    /// `git-path` at two (the first two of those, since either decider can
    /// return it), and `symlinked-parent` at exactly one — [`resolve`]'s `Err`
    /// arm, its only production site. See [`KEYED_TOKENS`] for what is asserted
    /// about the pair and what is not.
    const UNKEYED_TOKENS: [&str; 4] = ["too-many", "git-path", "unsafe-path", "symlinked-parent"];

    /// Does this `x` record's path field stand in for a path instead of naming
    /// one? Both of the module's summaries end the same way — [`Walk::refuse`]'s
    /// `<N unprintable bytes>` and [`discovered`]'s `<parent>/<N unnameable
    /// bytes>`.
    ///
    /// This is a **shape**, not an identity, exactly as the summaries
    /// themselves are: a repository may legally hold a file named
    /// `<7 unprintable bytes>`, and it would be captured as an `f` record that
    /// no assertion here reads.
    ///
    /// **Where that stops being harmless is an `x` record**, and it is worth
    /// knowing before the next vocabulary edit rather than after it. Both
    /// callers ask the same question — the search in
    /// `a_captured_files_parent_always_resolves_inside_the_work_tree` and the
    /// sweep in `the_summary_search_sees_the_records_it_asserts_about` — and it
    /// is that no record under a keyed token is a summary. So a real file
    /// legitimately *named* like one and refused under a keyed token (say
    /// `<7 unnameable bytes>` earning `missing`) would trip that assertion as a
    /// **false positive**. Nothing in that vocabulary produces one today — the
    /// sweep walks it exhaustively and counts zero — and adding one is a
    /// two-line experiment: `<7 unnameable bytes>` as a leaf reddens the sweep
    /// deterministically and the property randomly. If either ever reports a
    /// record that is both, the sweep's message names this paragraph as one of
    /// the two things it can be.
    fn is_summary(path: &str) -> bool {
        path.ends_with(" unprintable bytes>") || path.ends_with(" unnameable bytes>")
    }

    #[test]
    fn no_include_file_captures_nothing_at_all() {
        let dir = tree(&[("secret.env", "TOKEN=x")], None);
        assert!(
            capture_document(dir.path()).as_slice().is_empty(),
            "a repo that never opted in produces the byte-identical empty \
             capture it produced before this module existed"
        );
    }

    #[test]
    fn an_empty_or_comment_only_list_captures_nothing() {
        let dir = tree(&[("a.txt", "x")], Some("# nothing here\n\n   \n"));
        assert!(capture_document(dir.path()).as_slice().is_empty());
    }

    #[test]
    fn an_opted_in_file_is_captured_line_by_line() {
        let dir = tree(&[(".env", "A=1\nB=2\n")], Some(".env\n"));
        assert_eq!(doc(dir.path()), "rift-capture/1\nf .env\n A=1\n B=2\n \n");
    }

    #[test]
    fn content_lines_rejoin_to_the_exact_bytes() {
        for body in ["", "a", "a\n", "a\nb", "a\nb\n", "\n\n"] {
            let dir = tree(&[("f.txt", body)], Some("f.txt\n"));
            let text = doc(dir.path());
            let rejoined: Vec<&str> = text
                .lines()
                .skip(2)
                .map(|l| l.strip_prefix(' ').unwrap())
                .collect();
            assert_eq!(rejoined.join("\n"), body, "round-trip for {body:?}");
        }
    }

    #[test]
    fn a_git_path_is_refused_however_it_is_spelled() {
        let dir = tree(
            &[
                (".git/config", "[core]"),
                ("vendor/dep/.git/HEAD", "ref: x"),
            ],
            Some(".git/config\nvendor/dep/.git/HEAD\n.GIT/config\n.git\n"),
        );
        let text = doc(dir.path());
        assert!(
            !text.contains("[core]") && !text.contains("ref: x"),
            "no git-dir content is ever captured: {text}"
        );
        assert_eq!(
            text.lines().filter(|l| l.ends_with("git-path")).count(),
            4,
            "every spelling is refused by name: {text}"
        );
    }

    #[test]
    fn a_directory_root_never_walks_into_a_nested_git_dir() {
        let dir = tree(
            &[
                ("pkg/keep.txt", "kept"),
                ("pkg/.git/config", "SECRET-GIT-STATE"),
            ],
            Some("pkg/\n"),
        );
        let text = doc(dir.path());
        assert!(text.contains(" kept"), "the ordinary file is captured");
        assert!(
            !text.contains("SECRET-GIT-STATE"),
            "the walk refuses the nested git dir: {text}"
        );
        assert!(text.contains("x pkg/.git git-path"), "and says so: {text}");
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_is_recorded_and_never_followed() {
        let dir = tempfile::tempdir().unwrap();
        let outside = dir.path().join("outside-the-tree");
        std::fs::write(&outside, "PRIVATE-KEY-BYTES").unwrap();
        let work = dir.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        std::os::unix::fs::symlink(&outside, work.join("notes.txt")).unwrap();
        std::fs::write(work.join(INCLUDE_FILE), "notes.txt\n").unwrap();

        let text = doc(&work);
        assert!(
            !text.contains("PRIVATE-KEY-BYTES"),
            "following the link would exfiltrate anything the developer can \
             read: {text}"
        );
        assert!(
            text.contains("l notes.txt -> ") && text.contains("outside-the-tree"),
            "the link is preserved as a link: {text}"
        );
    }

    /// The `l` record's reader rule, from all **three** directions that can
    /// break it. This docstring said "both sides" and the test drove two; the
    /// third was the one that was live.
    ///
    /// All three ship through git: a repository can carry a symlink *named*
    /// `a -> b` in its index and clone materialises it, and a symlink's
    /// *target* is arbitrary bytes in a blob, so `ln -s '-> y' link` travels as
    /// an ordinary blob too. So this is a rule the format needs, not a corner.
    ///
    /// 1. the NAME holds the separator — written, and read back exactly;
    /// 2. the TARGET holds the separator — refused;
    /// 3. the TARGET merely **begins** `-> ` — refused now, and **not** by the
    ///    guard this replaces. `target.contains(" -> ")` cannot see it, because
    ///    the writer emits ` -> ` itself and the concatenation supplies the
    ///    second separator: `l arrowtarget -> -> y` rsplits to
    ///    `("arrowtarget ->", "y")` when the truth is `("arrowtarget", "-> y")`.
    ///    Both sides misread it and no test failed — which is why the writer
    ///    now reads its own record back instead of screening a substring.
    ///
    /// The fourth case is the control that keeps the rule from silently
    /// becoming "no arrows in targets": a target of `->` moves no separator,
    /// round-trips, and is still written.
    #[cfg(unix)]
    #[test]
    fn a_link_record_is_read_back_by_its_last_arrow() {
        fn one_link(name: &str, target: &str) -> String {
            let held = tempfile::tempdir().unwrap();
            let work = held.path().join("work");
            std::fs::create_dir_all(&work).unwrap();
            std::os::unix::fs::symlink(target, work.join(name)).unwrap();
            std::fs::write(work.join(INCLUDE_FILE), format!("{name}\n")).unwrap();
            doc(&work)
        }

        let text = one_link("a -> b", "elsewhere");
        let rest = text.lines().find_map(|l| l.strip_prefix("l ")).unwrap();
        assert_eq!(
            rest.rsplit_once(" -> "),
            Some(("a -> b", "elsewhere")),
            "the last ` -> ` is the separator the writer put there, so a link \
             whose NAME holds one round-trips exactly: {text}"
        );

        // …exact only because a target that would move that separator is
        // refused rather than written ambiguously. Remove the round-trip check
        // and this document reads back as the link `link -> x` pointing at `y`.
        assert_eq!(
            one_link("link", "x -> y"),
            format!("{CAPTURE_DOC_HEADER}\nx link unsafe-link-target\n"),
            "a target that holds the separator cannot be recorded unambiguously"
        );

        // The direction a substring guard cannot reach: the writer's own ` -> `
        // plus a target beginning `-> ` is two separators, and the last one is
        // not the writer's.
        assert_eq!(
            one_link("arrowtarget", "-> y"),
            format!("{CAPTURE_DOC_HEADER}\nx arrowtarget unsafe-link-target\n"),
            "`l arrowtarget -> -> y` reads back as (\"arrowtarget ->\", \"y\"), \
             which is neither the path nor the target"
        );

        // The control: the rule is the round-trip, not a ban on arrows.
        let text = one_link("plain", "->");
        let rest = text.lines().find_map(|l| l.strip_prefix("l ")).unwrap();
        assert_eq!(
            rest.rsplit_once(" -> "),
            Some(("plain", "->")),
            "a target that reads back correctly is still written: {text}"
        );
    }

    /// **An empty target is refused; a target that RENDERS as empty is
    /// written**, and to a human the second is the first. A byte-order mark is
    /// valid UTF-8, non-empty, not a control character (it is `Cf`, not `Cc`),
    /// and round-trips, so `l t_bom -> \u{FEFF}` is written — and printed to a
    /// terminal it reads `l t_bom -> `, indistinguishable from the empty target
    /// that earns `unsafe-link-target`.
    ///
    /// Nothing here is wrong: the record holds the exact bytes `readlink`
    /// returned, which is the guarantee. What would be wrong is a reader
    /// inferring emptiness from a *rendering*, so the format text says it in
    /// one clause and this test is why it may. The same holds for any
    /// zero-width or bidi-control-free formatting character; the BOM is just
    /// the one a repository acquires by accident.
    #[cfg(unix)]
    #[test]
    fn a_target_that_renders_as_nothing_is_not_an_empty_target() {
        let held = tempfile::tempdir().unwrap();
        let work = held.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        std::os::unix::fs::symlink("\u{FEFF}", work.join("t_bom")).unwrap();
        // Measured on APFS: `ls` renders these two as `t_bom -> ` and
        // `t_empty ->`, which is the whole point of the clause.
        let empty_exists = std::os::unix::fs::symlink("", work.join("t_empty")).is_ok();
        std::fs::write(work.join(INCLUDE_FILE), "t_bom\nt_empty\n").unwrap();

        let text = doc(&work);
        let rest = text
            .lines()
            .find_map(|l| l.strip_prefix("l "))
            .unwrap_or_default();
        assert_eq!(
            rest.rsplit_once(" -> "),
            Some(("t_bom", "\u{FEFF}")),
            "a BOM-only target is WRITTEN, and reads back as the exact bytes \
             `readlink` returned: {text}"
        );
        assert!(
            empty_exists,
            "the control has to exist for the contrast to be measured"
        );
        assert!(
            text.contains("x t_empty unsafe-link-target"),
            "while an EMPTY target is REFUSED — the two are one invisible \
             character apart on a terminal and a whole rule apart here: {text}"
        );
    }

    /// A symlink target that is **not valid UTF-8**: refused, never
    /// transcribed — and it needs its own rule because the round-trip guard
    /// above structurally cannot see it.
    ///
    /// A target is arbitrary bytes (it is a blob), so `a\xFF\xFEb` is a target
    /// a repository can ship. Recorded through `to_string_lossy` it became
    /// `a<U+FFFD><U+FFFD>b`, which is non-empty, holds no control character,
    /// and rsplits back to exactly the pair that went in — so the record was
    /// **well framed and false**: the pair it yields is not the pair the
    /// filesystem holds. The first assertion below is that framing *success*,
    /// so the two rules are visibly independent rather than one being read as
    /// a consequence of the other.
    ///
    /// The rule this settles is [`Refusal::Binary`]'s, one field over: this
    /// module does not store a lossy transcription of a thing while claiming to
    /// hold it. Content was already refused on that ground; a link target was
    /// not. (`a_repository_can_ship_a_non_utf8_link_target_through_clone` is
    /// the same class measured through a real index and a real clone, so
    /// "shippable" is not argued here either.)
    #[cfg(unix)]
    #[test]
    fn a_link_target_that_is_not_utf8_is_refused_rather_than_transcribed() {
        use std::os::unix::ffi::OsStrExt;

        let raw: &[u8] = b"a\xff\xfeb";
        let lossy = String::from_utf8_lossy(raw).into_owned();

        // The premise, asserted rather than assumed: the transcription passes
        // every check the `l` record already had. A guard that refuses this
        // would have to refuse `->`-free, control-free, non-empty targets too.
        let framed = format!("link -> {lossy}");
        assert_eq!(
            framed.rsplit_once(" -> "),
            Some(("link", lossy.as_str())),
            "the round-trip guard accepts the lossy record — it is exact about \
             the record's FRAMING and says nothing about whether the target is \
             the one on disk"
        );
        assert_ne!(
            lossy.as_bytes(),
            raw,
            "…and the pair it yields is not the pair the filesystem holds"
        );

        let held = tempfile::tempdir().unwrap();
        let work = held.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        std::os::unix::fs::symlink(std::ffi::OsStr::from_bytes(raw), work.join("link")).unwrap();
        std::os::unix::fs::symlink("elsewhere", work.join("plain")).unwrap();
        std::fs::write(work.join(INCLUDE_FILE), "link\nplain\n").unwrap();
        assert_eq!(
            std::fs::read_link(work.join("link"))
                .unwrap()
                .as_os_str()
                .as_bytes(),
            raw,
            "the filesystem really holds those bytes, so the refusal below is \
             about a link that exists"
        );

        let text = doc(&work);
        assert!(
            text.contains("x link unsafe-link-target"),
            "a target the document cannot spell exactly is a refusal: {text}"
        );
        assert!(
            !text.contains(char::REPLACEMENT_CHARACTER),
            "and never a transcription of it: {text}"
        );
        assert!(
            text.contains("l plain -> elsewhere"),
            "the control — the rule is UTF-8 validity, not a ban on links: {text}"
        );
    }

    /// The same class, reached the way a victim would reach it: from a hostile
    /// repository, through a real index entry at mode `120000` and a real
    /// clone. The module says "clone materialises it" in four places and has
    /// never measured it; every escape this module has had was a sentence that
    /// was true of its rule set and false of something outside it.
    #[cfg(unix)]
    #[test]
    fn a_repository_can_ship_a_non_utf8_link_target_through_clone() {
        use std::os::unix::ffi::OsStrExt;

        /// Hermetic git: neither the operator's config nor an ambient `GIT_*`
        /// (a test suite can be run from inside a hook) can change the answer.
        fn git(dir: &Path, args: &[&str]) -> String {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(dir)
                .env_remove("GIT_DIR")
                .env_remove("GIT_WORK_TREE")
                .env_remove("GIT_INDEX_FILE")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_SYSTEM", "/dev/null")
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@example.invalid")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@example.invalid")
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8_lossy(&out.stdout).trim().to_owned()
        }

        let raw: &[u8] = b"a\xff\xfeb";
        let held = tempfile::tempdir().unwrap();
        let origin = held.path().join("origin");
        std::fs::create_dir_all(&origin).unwrap();
        git(&origin, &["init", "-q", "-b", "main"]);

        // The target's bytes are a blob's contents, so nothing validates them.
        let carrier = held.path().join("target.bin");
        std::fs::write(&carrier, raw).unwrap();
        let oid = git(&origin, &["hash-object", "-w", carrier.to_str().unwrap()]);
        // Both halves of the rule in one repository: a link the include list
        // TYPES, and a link the walk DISCOVERS inside an included directory.
        for at in ["link", "d/link"] {
            git(
                &origin,
                &[
                    "update-index",
                    "--add",
                    "--cacheinfo",
                    &format!("120000,{oid},{at}"),
                ],
            );
        }
        std::fs::create_dir_all(origin.join("d")).unwrap();
        std::fs::write(origin.join("d/ok.txt"), "fine").unwrap();
        std::fs::write(origin.join(INCLUDE_FILE), "link\nd\n").unwrap();
        git(&origin, &["add", INCLUDE_FILE, "d/ok.txt"]);
        git(&origin, &["commit", "-q", "-m", "ship it"]);

        let clone = held.path().join("clone");
        git(
            held.path(),
            &[
                "clone",
                "-q",
                origin.to_str().unwrap(),
                clone.to_str().unwrap(),
            ],
        );

        for at in ["link", "d/link"] {
            assert_eq!(
                std::fs::read_link(clone.join(at))
                    .unwrap()
                    .as_os_str()
                    .as_bytes(),
                raw,
                "clone materialises the target byte-exact at {at} — the claim \
                 this module makes in prose everywhere and had measured nowhere"
            );
        }
        let text = doc(&clone);
        assert!(
            text.contains(" fine"),
            "the walk really entered the included directory, so the discovered \
             refusal below is one it reached: {text}"
        );
        assert!(
            text.contains("x link unsafe-link-target")
                && text.contains("x d/link unsafe-link-target"),
            "so the refusal is what a cloned repository gets, typed or \
             discovered: {text}"
        );
        assert!(
            !text.contains(char::REPLACEMENT_CHARACTER),
            "and not a transcription: {text}"
        );
    }

    /// What a discovered name decides — asserted on **every** leg, including
    /// the ones whose filesystem cannot be persuaded to produce the input
    /// (APFS: `EILSEQ`; macOS FAT: percent-encoded back into ASCII; both
    /// measured). The end-to-end test below is adjudicated on Linux; this one
    /// is the decision itself, handed the name directly.
    #[cfg(unix)]
    #[test]
    fn a_discovered_name_is_a_path_to_visit_or_a_refusal_to_record() {
        use std::os::unix::ffi::OsStrExt;

        assert_eq!(
            discovered("d", std::ffi::OsStr::new("ok.txt")),
            Ok("d/ok.txt".to_owned()),
            "an ordinary name is just the entry path"
        );
        assert_eq!(
            discovered("d", std::ffi::OsStr::from_bytes(b"na\xff\xfeme")),
            Err("d/<6 unnameable bytes>".to_owned()),
            "a name the document cannot spell names its PARENT — a path the \
             document can spell — and counts the child rather than inventing it"
        );
        // Whatever the summary says, it must be printable as-is: `refuse`
        // echoes a control-free path verbatim, so a summary that smuggled one
        // in would be re-summarised into `<N unprintable bytes>` and lose the
        // parent that is the whole point of it.
        let summary = discovered("a/b", std::ffi::OsStr::from_bytes(b"\xff")).unwrap_err();
        assert!(
            !summary.chars().any(char::is_control),
            "the summary is printable: {summary}"
        );
        assert_eq!(hard_refusal(&summary), None, "…and echoes as itself");
    }

    /// The rule of [`Walk::link`] one component to the left: a **name** the
    /// document cannot spell is refused, not renamed.
    ///
    /// Lossily converted, `na\xFF\xFEme` became the entry `d/na<U+FFFD><U+FFFD>me`,
    /// which `resolve` rebuilt into a path the kernel does not have — so the
    /// document said `missing`, a **false reason** for a file that exists,
    /// under a path nobody typed.
    ///
    /// **Adjudicated on the Linux leg.** APFS refuses a non-UTF-8 filename
    /// outright (`EILSEQ`, measured on this machine), so the class is
    /// unreachable on macOS and the body below degrades to the control
    /// assertion there — the same posture as
    /// `only_the_colon_screen_refuses_an_alternate_data_stream_name`, whose
    /// Windows run is what adjudicates it.
    /// `a_discovered_name_is_a_path_to_visit_or_a_refusal_to_record` covers the
    /// record's text on every leg — it hands [`discovered`] the bytes directly
    /// — and what only Linux can exercise is the branch that chooses it. (This
    /// docstring cited `an_unnameable_child_is_summarised_under_its_parent`,
    /// which does not exist and never did; in a module whose discipline is that
    /// each property names the tests that would fail if it were false, a
    /// citation to a test that does not exist is a claim without a test.)
    #[cfg(unix)]
    #[test]
    fn a_directory_entry_that_cannot_be_named_is_refused_not_renamed() {
        use std::os::unix::ffi::OsStrExt;

        let held = tempfile::tempdir().unwrap();
        let work = held.path().join("work");
        std::fs::create_dir_all(work.join("d")).unwrap();
        std::fs::write(work.join("d/ok.txt"), "fine").unwrap();
        std::fs::write(work.join(INCLUDE_FILE), "d\n").unwrap();

        let raw: &[u8] = b"na\xff\xfeme";
        let hostile = work.join("d").join(std::ffi::OsStr::from_bytes(raw));
        let filesystem_holds_it = std::fs::write(&hostile, "HOSTILE-BYTES").is_ok();

        let text = doc(&work);
        assert!(
            text.contains("f d/ok.txt"),
            "the nameable sibling is captured either way: {text}"
        );
        if !filesystem_holds_it {
            return; // macOS: EILSEQ. The Linux CI leg asserts the rest.
        }
        assert!(
            !text.contains(char::REPLACEMENT_CHARACTER),
            "no invented path: {text}"
        );
        assert!(
            text.contains("x d/<6 unnameable bytes> unsafe-path"),
            "the child is refused under a summary of its parent: {text}"
        );
        assert!(
            !text.contains("missing"),
            "and not with a false reason for a file that exists: {text}"
        );
        assert!(
            !text.contains("HOSTILE-BYTES"),
            "and refusing it did not read it: {text}"
        );
    }

    /// The discovered half of the same rule, and the half a format reader is
    /// likeliest to get wrong: a link the walk **meets** inside an included
    /// directory is screened exactly as a typed one is, so it earns an `l`
    /// record only when the record can hold it. Not followed either way — that
    /// is the property that matters — but `unsafe-link-target` is what the
    /// document says instead, and a text that promises an `l` record for every
    /// discovered symlink is wrong about a class git can ship.
    #[cfg(unix)]
    #[test]
    fn a_discovered_link_the_record_cannot_hold_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        // A directory outside the tree whose NAME holds the separator, so the
        // link's target does too — nothing about the link's own name is odd.
        let outside = dir.path().join("private -> x");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("id_rsa"), "PRIVATE-KEY-BYTES").unwrap();
        let work = dir.path().join("work");
        std::fs::create_dir_all(work.join("cfg")).unwrap();
        std::fs::write(work.join("cfg/ok.txt"), "fine").unwrap();
        std::os::unix::fs::symlink(&outside, work.join("cfg/linked")).unwrap();
        std::fs::write(work.join(INCLUDE_FILE), "cfg/\n").unwrap();

        let text = doc(&work);
        assert!(
            text.contains(" fine"),
            "the ordinary sibling is captured: {text}"
        );
        assert!(
            text.contains("x cfg/linked unsafe-link-target"),
            "a discovered link the record cannot hold is a REFUSAL, not an `l` \
             record: {text}"
        );
        assert!(
            !text.contains("l cfg/linked"),
            "and it is not written ambiguously either: {text}"
        );
        assert!(
            !text.contains("PRIVATE-KEY-BYTES") && !text.contains("id_rsa"),
            "and refusing it did not descend it: {text}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_walk_records_a_symlinked_directory_without_descending_it() {
        let dir = tempfile::tempdir().unwrap();
        let outside = dir.path().join("private");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("id_rsa"), "PRIVATE-KEY-BYTES").unwrap();
        let work = dir.path().join("work");
        std::fs::create_dir_all(work.join("cfg")).unwrap();
        std::fs::write(work.join("cfg/ok.txt"), "fine").unwrap();
        std::os::unix::fs::symlink(&outside, work.join("cfg/linked")).unwrap();
        std::fs::write(work.join(INCLUDE_FILE), "cfg/\n").unwrap();

        let text = doc(&work);
        assert!(text.contains(" fine"));
        assert!(
            !text.contains("PRIVATE-KEY-BYTES") && !text.contains("id_rsa"),
            "a symlinked directory is not a door into the walk: {text}"
        );
        assert!(text.contains("l cfg/linked -> "), "{text}");
    }

    /// The door one component to the left of the one the link-record guard
    /// closed. `lstat` does not
    /// follow the FINAL component — that is what makes a symlink a link record —
    /// but the kernel follows every INTERMEDIATE one, so an entry whose ancestor
    /// is a symlink reads a file outside the tree under an in-tree name. The
    /// spelling check cannot see it: `cfg/id_rsa` is an ordinary path.
    #[cfg(unix)]
    #[test]
    fn an_entry_under_a_symlinked_ancestor_is_refused_not_followed() {
        let dir = tempfile::tempdir().unwrap();
        let outside = dir.path().join("private");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("id_rsa"), "PRIVATE-KEY-BYTES-XYZ").unwrap();
        let work = dir.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        std::os::unix::fs::symlink(&outside, work.join("cfg")).unwrap();
        std::fs::write(work.join(INCLUDE_FILE), "cfg/id_rsa\n").unwrap();

        let text = doc(&work);
        assert!(
            !text.contains("PRIVATE-KEY-BYTES-XYZ"),
            "an ancestor symlink is a door out of the tree: {text}"
        );
        assert_eq!(
            text,
            format!("{CAPTURE_DOC_HEADER}\nx cfg/id_rsa symlinked-parent\n"),
            "and the refusal is recorded, naming what was wrong with the path"
        );
    }

    /// The same door pointed at the substrate — and the reason the `.git` rule
    /// needed more than a spelling check. `g -> .git` is a symlinked directory,
    /// which git stores as mode `120000` and clone recreates, so a repository
    /// can ship the whole vector: no local `ln`, no non-regular control file.
    #[cfg(unix)]
    #[test]
    fn an_entry_under_an_ancestor_symlinked_at_the_git_dir_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        rift_git::test_support::fake_repo(dir.path()).unwrap();
        std::fs::write(
            dir.path().join(".git/config"),
            "[remote \"origin\"]\n\turl = https://x:GIT-TOKEN-abc123@h/r\n",
        )
        .unwrap();
        std::os::unix::fs::symlink(".git", dir.path().join("g")).unwrap();
        std::fs::write(dir.path().join(INCLUDE_FILE), "g/config\n").unwrap();

        let text = doc(dir.path());
        assert!(
            !text.contains("GIT-TOKEN-abc123"),
            "`.git` reached through a symlinked ancestor is still `.git`: {text}"
        );
        assert_eq!(
            text,
            format!("{CAPTURE_DOC_HEADER}\nx g/config symlinked-parent\n"),
        );
    }

    /// A tree with every door this module has ever had left open in it: a
    /// directory outside the work tree holding the things an escape would fetch,
    /// a `.git` inside it holding a credential, and symlinks into both — spelled
    /// with `/` **and** with a `\` in the link's own name, because `\` is an
    /// ordinary filename byte on Unix and a separator on Windows, and a guard
    /// that splits it the wrong way sees ancestors the kernel does not.
    ///
    /// Built on every platform (the symlinks are Unix-only, which is where they
    /// can be created without privilege) so the properties over it run on every
    /// platform too.
    fn doors(dir: &Path) -> PathBuf {
        let outside = dir.join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("id_rsa"), "PRIVATE-KEY-BYTES-XYZ").unwrap();
        std::fs::write(outside.join("secret.txt"), "PRIVATE-KEY-BYTES-XYZ").unwrap();
        std::fs::write(outside.join("notes.md"), "MY-PRIVATE-JOURNAL-ENTRY").unwrap();

        let work = dir.join("work");
        std::fs::create_dir_all(work.join("real/sub")).unwrap();
        std::fs::write(work.join("real/a.txt"), "ordinary").unwrap();
        std::fs::write(work.join("real/sub/a.txt"), "deeper").unwrap();
        rift_git::test_support::fake_repo(&work).unwrap();
        std::fs::write(
            work.join(".git/config"),
            "[remote \"origin\"]\n\turl = https://x:GIT-TOKEN-abc123@h/r\n",
        )
        .unwrap();

        #[cfg(unix)]
        {
            // Out of the tree, spelled three ways: portable, with a `\` in the
            // link's own name, and one of each below a real directory.
            for name in ["cfg", "out", "bs\\dir", "deep/x\\y"] {
                let link = work.join(name);
                std::fs::create_dir_all(link.parent().unwrap()).unwrap();
                std::os::unix::fs::symlink(&outside, link).unwrap();
            }
            // At the substrate, and back INTO the tree — an ancestor symlink is
            // refused either way, because the rule is about the shape of the
            // path, not about where it happens to land.
            for name in ["g", "g\\h"] {
                std::os::unix::fs::symlink(".git", work.join(name)).unwrap();
            }
            std::os::unix::fs::symlink("real", work.join("in")).unwrap();
            std::os::unix::fs::symlink("real/sub", work.join("deeper")).unwrap();
        }
        work
    }

    /// The backslash-component defect, and the reason the guard is now
    /// component-wise. `bs\dir` is ONE
    /// component on Unix and the kernel follows it; a check that splits `rel` on
    /// `\` looks for `work/bs` and `work/bs/dir`, finds neither, and reports "no
    /// symlinked ancestor" while the walk reads a private key through the link.
    /// Asserted against [`resolve`] itself, not through the document, so it
    /// stays a test of the guard rather than of whatever screens precede it.
    #[cfg(unix)]
    #[test]
    fn a_backslash_named_ancestor_is_the_symlink_the_kernel_sees() {
        let dir = tempfile::tempdir().unwrap();
        let work = doors(dir.path());

        for entry in ["cfg/id_rsa", "bs\\dir/id_rsa", "deep/x\\y/id_rsa"] {
            assert_eq!(
                resolve(&work, entry),
                Err(Refusal::SymlinkedParent),
                "{entry}: every spelling of an ancestor symlink is one ancestor \
                 symlink — the operating system's parse is the only one that \
                 decides where the components are"
            );
        }
        for entry in ["g/config", "g\\h/config"] {
            assert_eq!(
                resolve(&work, entry),
                Err(Refusal::SymlinkedParent),
                "{entry}: `.git` reached through a symlinked ancestor is `.git`"
            );
        }
        assert_eq!(
            resolve(&work, "real/a.txt"),
            Ok(work.join("real/a.txt")),
            "and an ordinary path still resolves, to the path that is opened"
        );
    }

    /// The same doors from the outside: whatever the refusal is spelled, no byte
    /// from beyond the tree reaches the document, and every entry is accounted
    /// for by a record.
    #[cfg(unix)]
    #[test]
    fn no_spelling_of_an_ancestor_symlink_reaches_bytes_outside_the_tree() {
        let dir = tempfile::tempdir().unwrap();
        let work = doors(dir.path());
        let entries = [
            "cfg/id_rsa",
            "bs\\dir/id_rsa",
            "bs\\dir/notes.md",
            "deep/x\\y/id_rsa",
            "g/config",
            "g\\h/config",
        ];
        std::fs::write(work.join(INCLUDE_FILE), entries.join("\n")).unwrap();

        let text = doc(&work);
        for forbidden in [
            "PRIVATE-KEY-BYTES-XYZ",
            "MY-PRIVATE-JOURNAL-ENTRY",
            "GIT-TOKEN-abc123",
        ] {
            assert!(
                !text.contains(forbidden),
                "capture left the work tree: {text}"
            );
        }
        assert_eq!(
            text.lines().filter(|l| l.starts_with("x ")).count(),
            entries.len(),
            "and each entry is refused by name, none silently dropped: {text}"
        );
    }

    /// The limit the module doc states, measured rather than inferred: the
    /// checks here are over names and over ancestors, so a **hardlink** — a
    /// second name for an inode, a regular file at an accepted path with no
    /// symlink in its ancestry — is captured under the name it was given. Git
    /// cannot ship one (clone materialises regular files), so reaching this
    /// needs local write access to the tree, at which point the same hand could
    /// write the bytes into an ordinary file. Pinned so that the doc's limit is
    /// a fact about this code and not a guess about it.
    #[cfg(unix)]
    #[test]
    fn a_hardlink_is_captured_under_the_name_it_was_given() {
        let dir = tempfile::tempdir().unwrap();
        rift_git::test_support::fake_repo(dir.path()).unwrap();
        std::fs::write(dir.path().join(".git/config"), "GIT-CONFIG-BYTES").unwrap();
        std::fs::hard_link(
            dir.path().join(".git/config"),
            dir.path().join("innocuous.txt"),
        )
        .unwrap();
        std::fs::write(dir.path().join(INCLUDE_FILE), "innocuous.txt\n").unwrap();

        assert_eq!(
            doc(dir.path()),
            "rift-capture/1\nf innocuous.txt\n GIT-CONFIG-BYTES\n",
            "the `.git` rule is over paths, and a hardlink is a path that is \
             not one — the doc says so, and this is why it may"
        );
    }

    /// The third item of that finding: the `.git` comparison trimmed trailing dots and
    /// spaces (Win32 strips them while resolving) and the traversal check read
    /// the raw segment, so `..` refused and `.. ` did not — under this module's
    /// own stated premise, parent traversal with the refusal record echoing it.
    /// One notion of a segment now answers both.
    #[test]
    fn the_two_segment_questions_are_asked_about_the_same_segment() {
        for spelling in ["..", ".. ", ". ", ".", "...", "a/.. /b", "a/. /b", "   "] {
            assert_eq!(
                hard_refusal(spelling),
                Some(Refusal::UnsafePath),
                "{spelling:?}: a segment that resolves to nothing names no child"
            );
        }
        for spelling in [".git", ".GIT", ".git.", ".git ", "a/.git./b"] {
            assert_eq!(
                hard_refusal(spelling),
                Some(Refusal::GitPath),
                "{spelling:?}: and the git dir is still named as the git dir"
            );
        }
        assert_eq!(hard_refusal("a.txt"), None);
        assert_eq!(
            hard_refusal("dot."),
            None,
            "a real name that merely ends in \
             a dot resolves to `dot`, which is a child like any other"
        );

        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            resolve(dir.path(), "a/.. /b"),
            Err(Refusal::UnsafePath),
            "and the decision agrees with the screen"
        );
    }

    /// The **frame** every source screen below shares — and the assertion that
    /// makes it a frame rather than an assumption.
    ///
    /// Five tests in this module ask their question of the *product half*:
    /// everything above `mod tests`. Each of them used to derive that half for
    /// itself — `source.split("\n#[cfg(test)]").next()` — five derivations of
    /// one boundary whose precondition, that the anchor occurs **once**, was
    /// written down nowhere.
    ///
    /// **A second `#[cfg(test)]` at column zero moves all five horizons at
    /// once**, and everything between it and `mod tests` is unscreened for
    /// good. Measured rather than reasoned: `#[cfg(test)] mod probe {}`
    /// planted above the real one, with a fresh `impl Walk` under it whose
    /// body is the plainest call in this file —
    /// `self.refuse(rel, Refusal::NotAFile);` — ran **54 passed, 0 failed**.
    /// Every screen green, the `mod` tripwire in
    /// `only_three_places_in_this_module_write_a_record` included, because it
    /// reads the same truncated half. No clever spelling was involved; the
    /// frame did all the work. Re-planted against the assertion below, the
    /// module runs **49 passed, 5 failed** — one failure per screen, each
    /// naming this function.
    ///
    /// So the boundary is derived **here and nowhere else**, and its
    /// precondition rides with the derivation. That is the move the product
    /// half already makes with [`resolve`], for the reason the screen below
    /// gives: a second derivation is a second parse, and two parses drifting
    /// apart is what every defect in this module has been. Ask this function
    /// for the product half; do not split the source again.
    ///
    /// The count is deliberately **stricter than the horizon needs** — an
    /// anchor *below* `mod tests` cannot move `.next()` — because "exactly
    /// one" is checkable at a glance where "the first one is the intended one"
    /// is not, and the cost of the strictness is a red test with a message,
    /// not a silent hole.
    ///
    /// **The anchor is joined, not bare** (an earlier review found this used to
    /// count `#[cfg(test)]` and never identify it). Counting the bare token
    /// says there is one marker; it does not say the one marker is the one
    /// above `mod tests`. A column-zero `#[cfg(test)]` on a helper `fn` plus
    /// a respelled module attribute (`#[cfg(all(test))]`) keeps the count at
    /// exactly one and moves the horizon up to the helper — every screen
    /// green, everything between the decoy and the real module unscreened.
    /// The joined anchor `\n#[cfg(test)]\nmod tests {` matches only the
    /// attribute whose next line opens THE test module, so the decoy is not
    /// an anchor and the respelling is not an anchor: count zero, refused,
    /// named. Same shape `ci_meta::production_half` uses for `bins/`, and
    /// assembled from pieces so this source cannot match its own spelling.
    /// [`product_half_of`] is the pure form the calibration below drives
    /// with synthetic sources; [`product_half`] is that applied to this file.
    fn product_half() -> &'static str {
        product_half_of(include_str!("inclusion.rs"))
    }

    /// Everything above the ONE joined anchor in `source` — see
    /// [`product_half`] for why the anchor is joined and the count is one.
    fn product_half_of(source: &str) -> &str {
        // Assembled so this scanner never matches itself.
        let horizon = ["\n#[cfg(", "test)]\nmod tests {"].concat();
        assert_eq!(
            source.matches(horizon.as_str()).count(),
            1,
            "this file must hold exactly one column-zero `#[cfg(test)]` whose \
             very next line opens `mod tests {{` — because every source screen \
             below takes it as the end of the product half, and `.next()` \
             stops at the FIRST one. A second one (the fully ordinary \
             `#[cfg(test)] mod proptests;`) moves every horizon up at once and \
             leaves everything under it unscreened, which is how the plainest \
             `self.refuse(…)` in the file can sit in an `impl Walk` that no \
             screen reads; a respelled attribute (`#[cfg(all(test))]`) or a \
             decoy on a helper fn matches nothing and is refused here rather \
             than cut at. If you want a second test module, nest it inside \
             `mod tests`, where it needs no second attribute; if you are \
             moving this boundary on purpose, say in the commit message which \
             screens you re-aimed and what now stands above them."
        );
        source.split(horizon.as_str()).next().unwrap_or_default()
    }

    /// The calibration for [`product_half_of`]: the decoy-plus-respelling
    /// shape that kept the old bare-token count at exactly one is refused,
    /// not cut at; the real shape returns the product half. The synthetic
    /// sources are assembled from pieces so this test's own text carries no
    /// column-zero anchor.
    #[test]
    fn a_column_zero_decoy_and_a_respelled_anchor_are_refused_not_cut_at() {
        let attr = ["#[cfg(", "test)]"].concat();
        let respelled = ["#[cfg(all(", "test))]"].concat();
        let decoyed = format!(
            "fn product() {{}}\n{attr}\nfn helper() {{}}\nfn unscreened() {{}}\n{respelled}\nmod tests {{}}\n"
        );
        // The bare-token count is exactly one here — the old horizon would
        // have cut at the helper and left `unscreened` under it.
        assert_eq!(decoyed.matches(attr.as_str()).count(), 1);
        let refused = std::panic::catch_unwind(|| product_half_of(&decoyed).len());
        assert!(
            refused.is_err(),
            "a decoy attribute plus a respelled module anchor must be refused, not cut at"
        );

        let real =
            format!("fn product() {{}}\n{attr}\nfn helper() {{}}\n\n{attr}\nmod tests {{}}\n");
        assert_eq!(
            product_half_of(&real),
            format!("fn product() {{}}\n{attr}\nfn helper() {{}}\n"),
            "the real shape cuts at the joined anchor and keeps the helper screened"
        );
    }

    /// The structural rule the three escapes cost, enforced rather than
    /// promised: exactly one place in this module turns an entry into a path,
    /// and every filesystem call takes a path that came from there. A second
    /// derivation is a second parse, and two parses drifting apart is what
    /// every defect here has been.
    ///
    /// **This is a textual screen, and the doc used to overstate it.** It
    /// counted `.join(` and claimed to "count the derivations and fail at two";
    /// a second derivation written with `PathBuf::push` — that defect's shape exactly,
    /// guard path ≠ opened path — sailed past it, and all 33 tests stayed
    /// green. What it does now is hold an **inventory**: every product-half
    /// line that builds a path or touches the filesystem is listed here
    /// verbatim, so a new derivation shows up as a line this list does not
    /// have. Measured against four second-derivations planted in `visit`,
    /// each replacing the path `resolve` returned with a re-derived one:
    /// `PathBuf::push` (that shape), `PathBuf::from(format!(..))` and
    /// `Path::new(&s).to_path_buf()` are all **caught**. The list is also the
    /// readable answer to "where does this module touch the disk?", which is
    /// worth having on its own.
    ///
    /// **What it still cannot see, measured rather than guessed:** a derivation
    /// with no constructor token at all. `let full: PathBuf =
    /// format!("{}/{}", work_tree.display(), rel).into();` compiles, re-derives
    /// the opened path from `rel`, and this test stays **green**. So it screens
    /// the spellings this module would plausibly use and does not prove there
    /// are no others — an `Into`, an extension trait, a helper in another
    /// module would each pass. Said plainly, with the counterexample, because
    /// the previous wording ("counts the derivations and fails at two") is
    /// exactly why a `push` derivation went unnoticed.
    #[test]
    fn only_one_place_in_this_module_turns_an_entry_into_a_path() {
        let product = product_half();
        let inventory: Vec<&str> = product
            .lines()
            .map(str::trim)
            .filter(|l| !l.starts_with("//"))
            .filter(|l| {
                l.contains(".join(")
                    || l.contains("PathBuf::from")
                    || l.contains("PathBuf::new")
                    || l.contains("Path::new")
                    || l.contains("to_path_buf")
                    || l.contains("std::fs::")
                    // A byte pushed onto the document is not a path derivation;
                    // anything else pushed might be.
                    || (l.contains(".push(") && !l.contains(".push(b'"))
            })
            .collect();
        assert_eq!(
            inventory,
            [
                // `resolve`, and only `resolve`, turns an entry into a path.
                ".join(rel)",
                "let mut full = work_tree.to_path_buf();",
                "full.push(name);",
                "&& std::fs::symlink_metadata(&full).is_ok_and(|m| m.is_symlink())",
                // `normalize_entry` builds a String, never a path.
                "collapsed.push(c);",
                // Every other filesystem call takes a path `resolve` returned.
                "let Ok(meta) = std::fs::symlink_metadata(&path) else {",
                "std::fs::read_to_string(&path)",
                "let Ok(meta) = std::fs::symlink_metadata(&full) else {",
                "let Ok(target) = std::fs::read_link(full) else {",
                "let Ok(read) = std::fs::read_dir(full) else {",
                "let Ok(bytes) = std::fs::read(full) else {",
                // Not a derivation: it copies the path `resolve` already
                // returned, to use as a set key. Nothing is parsed and nothing
                // is opened — `full` arrives from the caller.
                "if self.recorded.insert(full.to_path_buf()) {",
            ],
            "the product half of this module must build paths in exactly one \
             place — `resolve`, which checks the components it builds from — \
             and every filesystem call must take a path it returned. If you \
             need a path here, ask `resolve` for it; if you need one it cannot \
             give you, change `resolve` so the check moves with it. If you \
             genuinely added a line that belongs, add it to this list in the \
             same commit and say in the message why it is not a second parse."
        );
    }

    /// The **warrant** for the enumeration next door, and it exists because the
    /// enumeration shipped without it and the assumption had a hole.
    ///
    /// Three places in this module write a record — [`Walk::link`]'s `l`,
    /// [`Walk::file`]'s `f`, [`Walk::refuse`]'s `x` — and that is what makes
    /// enumerating [`Walk::refuse`]'s callers *exhaustive* rather than merely
    /// long: a line can only be a refusal exit by going through `refuse`.
    ///
    /// **The hole, measured rather than argued.** The exit inventory filters
    /// four call names and the window screen filters bare `refuse`, so a fourth
    /// helper writing to `doc`/`records` **directly** — the idiom `link`
    /// and `file` already use — matches neither. Planted in this module, it
    /// produced `x d/x.a15vprobe not-a-file` **twice** for one path: two
    /// records, two charges against [`MAX_ENTRIES`], the double-record defect
    /// exactly, with
    /// both of the other screens green. Re-measured on this tree: 53 passed,
    /// 1 failed, and the one that failed was this one. **The total moves with
    /// the module and the 1 does not** — it read 51 when this test was split
    /// out of the enumeration, and it says nothing then or now except how many
    /// tests the module happened to hold; the half of the measurement worth
    /// keeping is that exactly one test noticed, and it was this one.
    ///
    /// The filter takes reads as well as writes, and on **any** receiver rather
    /// than on `self`, because separating them would be a guess about which
    /// spellings mutate (`&w.doc` reads, `&mut w.doc` writes; a helper taking
    /// `&mut Walk` writes `w.records`; a helper handing out `&mut self.doc`
    /// writes neither) — and a guess about which spellings matter is precisely
    /// what let that defect back in twice.
    ///
    /// **What this cannot see**, in the shape its two siblings
    /// (`only_one_place_in_this_module_turns_an_entry_into_a_path`,
    /// `nothing_this_module_records_is_a_lossy_transcription`) already state:
    /// it reads one module's product half by substring, so a spelling that
    /// reaches these fields without naming them passes. **Demonstrated, not
    /// supposed** — a helper destructuring `Walk { doc, records, .. }` and
    /// writing through the bindings names neither field, and put
    /// `x d/a.a18destr not-a-file` in the document twice for one path with
    /// every screen in this module green. It also says nothing about any other
    /// **file**, which is the honest bound: [`Walk`] is private to this
    /// module, and private-to-module reaches the module's whole *subtree* —
    /// `mod tests` below already constructs a [`Walk`] and writes its fields —
    /// so a child module living in a separate file could write a record with
    /// all three screens blind to it. That one is now **tripwired rather than
    /// closed**, by the first assertion in the body below: the child is still
    /// invisible once it exists, but the `mod` line admitting it cannot be
    /// added quietly.
    #[test]
    fn only_three_places_in_this_module_write_a_record() {
        let product = product_half();
        // The BOUND on the enumeration below, tripwired rather than argued.
        // All three screens read `inclusion.rs` and nothing else, while
        // `Walk` being private admits every file in this module's *subtree* —
        // so a child module in a second file writes records none of them can
        // see. Planted end to end rather than reasoned: `#[path = …] mod
        // a18child;` whose body called the ordinary `w.refuse(rel, …)` put
        // `x d/a.a18child not-a-file` in the document TWICE for one path —
        // the double-record defect exactly, at 54 passed and 0 failed. This assertion does not
        // close that hole — the child is still invisible once it exists. It
        // makes the one line that opens it impossible to add quietly.
        //
        // THE PREDICATE IS A WHITESPACE TOKEN, NOT A RUST LEXICAL ONE, and it
        // is written that way deliberately. Measured on this tree, it fires on
        // three benign shapes at once — `let _ = "a mod b";`, `/* mod a; */`,
        // and a trailing `// mod boundary note` — and each is FAIL-CLOSED and
        // self-describing: the failure prints the offending line, so a false
        // positive costs one glance and a reword, where a false negative costs
        // a module nobody screens. The file's own style keeps it far from that
        // edge; the product half carries no trailing `//`, no block comment
        // and no standalone `mod` word. What it does NOT see, also measured:
        // a whole-line `//` comment, filtered one line above the predicate,
        // and `mod` welded to an attribute — `#[allow(dead_code)]mod x {}` is
        // legal Rust, is a single whitespace token, and passed. That gap is
        // closed by the fmt leg, which splits the two onto separate lines;
        // what the screens in this module do and do not owe to `cargo fmt` is
        // set out in `every_refusal_exit_in_the_walk_is_enumerated_and_classified`.
        let declared: Vec<&str> = product
            .lines()
            .map(str::trim)
            .filter(|l| !l.starts_with("//"))
            .filter(|l| l.split_whitespace().any(|t| t == "mod"))
            .collect();
        assert_eq!(
            declared,
            [] as [&str; 0],
            "the product half of this module declares no child module, and \
             the three screens holding `Walk`'s record writers are exhaustive \
             only while that is true. `mod tests` is the only child this \
             module has and it sits below the `#[cfg(test)]` split — \
             `product_half` pins that there is exactly one such split, which \
             is what makes `below it` mean anything at all. If you are adding \
             a module here, you \
             are adding a file these screens cannot read: give them that file \
             in the same commit, or say in the message why the record writers \
             stay in this one."
        );
        let touches: Vec<&str> = product
            .lines()
            .map(str::trim)
            .filter(|l| !l.starts_with("//"))
            .filter(|l| l.contains(".doc") || l.contains(".records"))
            .collect();
        assert_eq!(
            touches,
            [
                // ---- `capture_document`: reads the finished document. Writes
                // nothing, and the header it prepends goes onto `out`.
                "if w.doc.is_empty() {",
                "let mut out = Vec::with_capacity(w.doc.len() + CAPTURE_DOC_HEADER.len() + 1);",
                "out.extend_from_slice(&w.doc);",
                // ---- WRITER 1 of 3: `link`'s `l` capture.
                "self.records += 1;",
                "self.doc.extend_from_slice(b\"l \");",
                "self.doc.extend_from_slice(record.as_bytes());",
                "self.doc.push(b'\\n');",
                // ---- WRITER 2 of 3: `file`'s `f` capture, header then content
                // lines.
                "self.records += 1;",
                "self.doc.extend_from_slice(b\"f \");",
                "self.doc.extend_from_slice(rel.as_bytes());",
                "self.doc.push(b'\\n');",
                "self.doc.push(b' ');",
                "self.doc.extend_from_slice(line.as_bytes());",
                "self.doc.push(b'\\n');",
                // ---- `cap_spent`: reads the count to decide, writes via
                // `exhausted` -> `refuse`.
                "if self.records >= MAX_ENTRIES {",
                // ---- WRITER 3 of 3: `refuse`'s `x` record — the one every
                // refusal exit in the enumeration next door goes through.
                "self.records += 1;",
                "self.doc.extend_from_slice(b\"x \");",
                "self.doc.extend_from_slice(safe.as_bytes());",
                "self.doc.push(b' ');",
                "self.doc.extend_from_slice(why.as_str().as_bytes());",
                "self.doc.push(b'\\n');",
            ],
            "three places in this module write a record — `link`'s `l`, \
             `file`'s `f`, `refuse`'s `x` — and \
             `every_refusal_exit_in_the_walk_is_enumerated_and_classified` is \
             exhaustive only because of it: a refusal is a refusal by going \
             through `refuse`. A fourth writer is a fourth exit that the \
             enumeration's call-name filter cannot see, which is \
             how one path earned two records and two charges against \
             `MAX_ENTRIES` three times running. If you are adding one, do not: \
             route it through `refuse` (or `refuse_once`, if it decides after \
             `resolve` produced a path). If you genuinely need a fourth, it \
             goes in this list, in that enumeration, and in `Walk::recorded`'s \
             own, in the same commit."
        );
    }

    /// The exit inventory, and it exists because a **counted** claim now sits
    /// on [`Walk::recorded`]: sixteen refusal exits in this walk write a
    /// record, eleven keyed and five not. This is what fails if that stops
    /// being true.
    ///
    /// It is here because the claim it replaced was general — "the insert runs
    /// for every non-directory path, before the decision is made" — and general
    /// was the whole defect. It was written from the rule set, it was false at
    /// one exit, and it was corrected **three times** by adding a shape to
    /// `a_path_named_twice_is_captured_once` while leaving the reason general
    /// enough to cover the next exit nobody had checked. A behavioural shape
    /// catches the exit its author thought of. An inventory catches the one they
    /// did not: a seventeenth exit, or a moved one, reddens here until it is
    /// listed **and classified**, in the same commit.
    ///
    /// **Three screens hold this property and they live in three tests.**
    /// `only_three_places_in_this_module_write_a_record` is their WARRANT: it
    /// pins the fact that makes the enumeration below *exhaustive* rather than
    /// merely long — a line can only be an exit by going through
    /// [`Walk::refuse`], so enumerating that function's callers enumerates the
    /// refusals. That was an unstated assumption for a round, and it had a hole
    /// — read its docstring for what the hole was and what it produced.
    /// `the_window_from_resolving_a_path_to_keying_it_takes_refuse_once_only`
    /// holds the second, which is structural rather than textual; it was split
    /// out of this test when the two together crossed clippy's line ceiling,
    /// which is this module's idiom for that lint rather than an `#[allow]`.
    ///
    /// This one is the **inventory**: every line that writes a record or spends
    /// the record cap, in source order, each labelled with whether the path it
    /// names is keyed — the three helper bodies included, so that rerouting an
    /// exit through a different helper is visible too.
    /// **What this cannot see**, said plainly for the reason its siblings say
    /// it: the keyed/unkeyed labels are asserted *text*, not derived facts.
    /// This list is textual and the window screen next door is structural, so a
    /// reader who mislabels a line here fails no test — a verifier planted
    /// exactly that (an exit listed, but labelled KEYED when it is not) and
    /// only the window named it.
    ///
    /// **What nobody can do is *move* or *delete* an exit silently** — though
    /// not both by the same screen, and the attribution is worth getting right
    /// because it says which test to read when one goes red. *Delete* is the
    /// inventory's: the list is exact, so a line that goes away shortens it.
    /// *Move* is the inventory's **only when the move reorders the list**, and
    /// a move exists that does not: hoisting the `NotAFile` exit above
    /// `self.recorded.insert(` leaves the nineteen filtered lines
    /// **byte-identical**, because the insert is not one of them. Measured
    /// rather than reasoned — the inventory stayed green and the window alone
    /// went red, 53 passed and 1 failed. That is the division of labour working
    /// as built: the inventory holds the list, the window holds the interval,
    /// and a move *into* the interval is the window's to catch.
    ///
    /// ***add* is a tripwire, not a proof**, and it is written that way because
    /// the stronger sentence kept measuring false. It stood here as "a new exit
    /// either calls one of the four names the inventory filters or writes
    /// `doc`/`records` itself, and this module holds no third way to put a line
    /// in the document" — **contradicted by its own sibling one docstring away**
    /// (`only_three_places_in_this_module_write_a_record` states plainly that "a
    /// spelling that reaches them without naming them would pass"), and then
    /// defeated in measurement repeatedly — most recently by the two
    /// path-qualified spellings below — each time producing two records
    /// for one path with every screen green.
    ///
    /// What the two screens between them catch: a call to one of the four
    /// names — **on any receiver and under any path qualification** — and a
    /// direct write to `doc`/`records`. The qualification half was a defeat and
    /// then its repair was a second one. `Walk::refuse(self, …)` is neither
    /// `.refuse(` nor a field name, and passed both screens while putting
    /// `x d/a.a18probe not-a-file` in the document twice for one path at 54
    /// passed and 0 failed. Adding `Walk::refuse(` reddens that plant on both
    /// screens at 52 passed and 2 failed — and is then defeated in turn by
    /// `Self::refuse(self, …)`, the same call one spelling over, back to 54
    /// passed and 0 failed on the widened filters. So both filters anchor on
    /// `::refuse(` rather than on a type name, which is what makes `Self::`,
    /// `<Walk>::` and a fully-qualified path one case instead of three. The
    /// widening is pure: no line in the product half carries any of those
    /// spellings, so the nineteen-line list is unchanged and green on a clean
    /// tree.
    ///
    /// **This screen and the window next door lean on the fmt leg, and that is
    /// said here rather than left implied.** Every anchor above is a substring
    /// of a *single line*, so whitespace beats them at source level with no
    /// exotic spelling at all: `Walk::refuse (self, …)` with one space carries
    /// no `::refuse(`, and the same call broken after the `::` carries it on
    /// neither line. Both were planted in the product half and both left this
    /// test and the window **green** — 54 passed, 0 failed. What stops them is
    /// `cargo fmt --all -- --check`, the first leg of `./skills.sh check`:
    /// measured on this tree, rustfmt deletes the space and rejoins the split
    /// path, so neither shape survives on a tree the gate accepts.
    ///
    /// **Why lean rather than widen**, when the house fixed the matcher in the
    /// analogous case — `xtask/src/ci_meta.rs`,
    /// `catches_whitespace_before_the_paren`, "`#![allow (clippy::all)]` is
    /// legal Rust and must not evade the gate". The precedent is *not* that
    /// fmt cannot see the shape: rustfmt normalises `#[allow (dead_code)]`
    /// too, measured in the same run as the two above. It is that a gate of
    /// last resort must not depend on another gate — and `ci_meta` is that
    /// gate, screening the whole workspace's escape hatch from the lint system
    /// across files no single author owns. These five screens are one module's
    /// own tests, run by the same command as fmt, and they sit on a ladder
    /// with **no top**: a macro expanding to `w.$m(` reaches [`Walk::refuse`]
    /// with no such spelling anywhere in the source, and the frame defeat that
    /// [`product_half`] now pins beat all five at once *without any spelling
    /// at all*. One more rung would buy one shape and cost a second
    /// normalising parse of the source, in a module whose every defect has
    /// been two parses drifting apart.
    ///
    /// **The gap that reliance leaves, named and measured rather than
    /// hoped away:** `#[rustfmt::skip]` on the offending item opts out of the
    /// fmt leg locally — planted with the spaced call under it, `cargo fmt
    /// --all -- --check` exits **0** and this module runs **54 passed, 0
    /// failed** — and a `cargo test` run with no fmt leg at all sees none of
    /// this. Both are one-line, diff-visible additions, which is the same
    /// standing every other tripwire in this module has.
    ///
    /// **Among the cases none of these covers** — and the enumeration is open,
    /// because these are the routes that were *planted*, not the routes that
    /// exist. **(i) A spelling that reaches the four functions or the two
    /// fields without naming either.** Two were planted: a helper
    /// destructuring `Walk { doc, records, .. }` and writing through the
    /// bindings, and an alias `let f: fn(&mut Walk, &str, Refusal) =
    /// Walk::refuse;` called as `f(w, …)`. Each earned two records for one
    /// path, and with both live the module ran 54 passed, 0 failed.
    /// **(ii) A child module in a separate file.** [`Walk`] being private
    /// bounds the writers to this module's own *subtree*, not to this file,
    /// and all three screens read `inclusion.rs` alone: `#[path = …] mod
    /// a18child;` whose body called the ordinary `w.refuse(rel, …)` was
    /// planted and produced the same two records with all three green. Its
    /// *declaration* is now tripwired in
    /// `only_three_places_in_this_module_write_a_record` — re-planted against
    /// that assertion the module runs 53 passed, 1 failed, and the one failure
    /// is the tripwire. A tripwire and not a closure: the child's writes stay
    /// invisible, only the line admitting them does not.
    ///
    /// The labels themselves are pinned behaviourally by
    /// `a_path_named_twice_is_captured_once` (five shapes, spanning both keying
    /// sites) and by
    /// `a_refusal_with_no_path_to_key_on_is_recorded_once_per_naming`.
    #[test]
    fn every_refusal_exit_in_the_walk_is_enumerated_and_classified() {
        let product = product_half();
        let exits: Vec<&str> = product
            .lines()
            .map(str::trim)
            .filter(|l| !l.starts_with("//"))
            .filter(|l| {
                // On ANY receiver, not on `self`: a helper taking `&mut Walk`
                // and calling `w.refuse(rel, …)` is an exit like any other, and
                // it is invisible to a `self.`-anchored filter. That gap was
                // measured — the probe put `x d/x.a16vprobe not-a-file` in the
                // document twice, with all three screens green.
                //
                // And under ANY path qualification, which is the `::` half:
                // `Walk::refuse(self, …)` is that same call spelled without a
                // receiver, and it defeated the line above. Anchoring on
                // `::refuse(` rather than on `Walk::refuse(` is deliberate and
                // measured too — the type-named form was defeated in turn by
                // `Self::refuse(self, …)`, one spelling over, while `::` takes
                // `Self::`, `<Walk>::` and a fully-qualified path alike.
                l.contains(".refuse(")
                    || l.contains(".refuse_once(")
                    || l.contains(".exhausted(")
                    || l.contains(".cap_spent(")
                    || l.contains("::refuse(")
                    || l.contains("::refuse_once(")
                    || l.contains("::exhausted(")
                    || l.contains("::cap_spent(")
            })
            .collect();
        assert_eq!(
            exits,
            [
                // ---- `visit`: five exits, and the boundary runs through it.
                // UNKEYED — the record cap is charged before `resolve` runs.
                "if self.cap_spent(rel) {",
                // UNKEYED — `hard_refusal` decides on the spelling alone.
                "self.refuse(rel, why);",
                // UNKEYED — `resolve` returned no path to key on.
                "self.refuse(rel, why);",
                // KEYED, by the helper — this exit returns three lines before
                // the insert, which is the whole of the double-record defect.
                "self.refuse_once(&full, rel, Refusal::Missing);",
                // KEYED, by the insert immediately above it.
                "self.refuse(rel, Refusal::NotAFile);",
                // ---- `link`: three exits, every one past that insert.
                // KEYED.
                "self.refuse(rel, Refusal::Missing);",
                // KEYED.
                "self.refuse(rel, Refusal::UnsafeLinkTarget);",
                // KEYED.
                "self.refuse(rel, Refusal::UnsafeLinkTarget);",
                // ---- `walk_dir`: four exits. A directory never reaches the
                // insert, so the two records it can earn key here instead.
                // KEYED, by the helper.
                "self.refuse_once(full, rel, Refusal::TooDeep);",
                // KEYED, by the helper.
                "self.refuse_once(full, rel, Refusal::Missing);",
                // UNKEYED — the cap is charged on a summary, which is no path.
                "if self.cap_spent(&summary) {",
                // UNKEYED — the summary is not a path by construction.
                "self.refuse(&summary, Refusal::UnsafePath);",
                // ---- `file`: four exits, every one past that insert.
                // KEYED.
                "self.refuse(rel, Refusal::TooLarge);",
                // KEYED.
                "self.exhausted(rel, Refusal::BudgetSpent);",
                // KEYED.
                "self.refuse(rel, Refusal::Missing);",
                // KEYED.
                "self.refuse(rel, Refusal::Binary);",
                // ---- Not exits: the three helper bodies the sixteen above go
                // through — `exhausted`, `cap_spent` and `refuse_once`.
                "self.refuse(rel, why);",
                "self.exhausted(rel, Refusal::TooMany);",
                "self.refuse(rel, why);",
            ],
            "sixteen refusal exits write a record and three helpers carry them \
             If you added an exit, put it in this list, label it keyed or \
             unkeyed, and make `Walk::recorded`'s enumeration say the same \
             thing — in this commit. If it decides after `resolve` has produced \
             a path, it is `refuse_once` and not `refuse`; that mistake is \
             the double-record defect and has now been made three times."
        );
    }

    /// The **window** screen is the defect's own shape rather than a list:
    /// between the line where [`resolve`] hands back a path and the line
    /// where [`Walk::visit`] keys it, a path *exists* and is *not yet keyed*,
    /// so any bare `refuse` in that interval duplicates by construction. The
    /// double-record defect was exactly one such line. The interval must therefore hold no bare
    /// `refuse` at all, **on any receiver** — only [`Walk::refuse_once`],
    /// which keys what it names. This filter and the inventory's next door both
    /// take `.refuse(` rather than `self.refuse(` for that reason: a helper
    /// taking `&mut Walk` and calling `w.refuse(rel, …)` is an exit, it was
    /// planted, and it put two records in the document with every screen
    /// green. Both also take `::refuse(`, for the same reason one spelling
    /// further out: `Walk::refuse(self, …)` is that call with no receiver at
    /// all, it was planted in this very interval, and it went green on both
    /// screens while writing `x d/a.a18probe not-a-file` twice for one path.
    /// The anchor is `::refuse(` and not `Walk::refuse(` because the type-named
    /// form was itself defeated by `Self::refuse(self, …)`. Both anchors are
    /// single-line substrings and are therefore beaten by whitespace at source
    /// level; that this screen leans on `cargo fmt` for those shapes, what the
    /// house precedent actually says about it, and what the reliance leaves
    /// open are set out with their measurements in
    /// `every_refusal_exit_in_the_walk_is_enumerated_and_classified`.
    /// Note what the window deliberately starts after: [`resolve`]'s
    /// own `Err` arm is a bare `refuse` and belongs there, because that exit
    /// has no path to key. **That excuse is positional, so it excused more
    /// than it meant to**: skipping to the `match`'s closing `};` skips the
    /// `Ok` arm too, and the `Ok` arm is already inside the interval — the
    /// path is resolved there and not yet keyed. A bare `refuse` planted in
    /// it, then listed and labelled in the inventory next door exactly as that
    /// assertion's own failure message instructs, left the whole suite green
    /// with two records for one path. So the skipped `match` is now pinned
    /// line for line before the window runs: the `Ok` arm is
    /// `Ok(full) => full,` and holds no code, which is what makes skipping
    /// it safe. **A window bounded by source literals is empty
    /// whenever an anchor stops matching, and an empty window screens
    /// nothing while still passing** — so the window is now required to
    /// contain the `lstat`, the one syscall the interval exists around,
    /// before it is required to hold no bare `refuse`. That anchor is
    /// deliberately not an exit: anchoring on that defect's own `refuse_once` would
    /// confound "the window drifted" with "the exit changed", which is
    /// the inventory's job. The hole was measured, not imagined: hoisting
    /// the call out of its `match`, restoring the bare `refuse` in it and
    /// relaxing the list to match left this whole test green, and only
    /// `a_path_named_twice_is_captured_once`'s fifth shape went red.
    #[test]
    fn the_window_from_resolving_a_path_to_keying_it_takes_refuse_once_only() {
        let product = product_half();
        let visit = product
            .split("fn visit(&mut self, work_tree: &Path, rel: &str, depth: usize) {")
            .nth(1)
            .unwrap();
        // The window below opens PAST the `match` that decides the path,
        // because `resolve`'s `Err` arm is the one bare `refuse` that belongs
        // inside the interval. That excuse is positional, not semantic: the
        // skip steps over the `Ok` arm as well, and the `Ok` arm is already in
        // the true interval — the path is resolved there and not yet keyed.
        // Measured, not supposed: a bare `refuse` planted in the `Ok` arm, then
        // listed and labelled in the inventory above exactly as its own failure
        // message instructs, left the whole suite GREEN while one path earned
        // two records and two charges against `MAX_ENTRIES`.
        //
        // So the skipped region is pinned line for line. The `Ok` arm hands the
        // resolved path straight out and holds no code at all, which is what
        // makes skipping it safe; anything else in there — including a `refuse`
        // — reddens here, and this assertion is not vacuous under a moved
        // anchor either (a start that stops matching empties the list, an end
        // that stops matching runs it to the end of `visit`, and both fail).
        let decision: Vec<&str> = visit
            .lines()
            .map(str::trim)
            .skip_while(|l| !l.contains("resolve(work_tree, rel)"))
            .take_while(|l| *l != "};")
            .filter(|l| !l.starts_with("//"))
            .collect();
        assert_eq!(
            decision,
            [
                "let full = match resolve(work_tree, rel) {",
                "Ok(full) => full,",
                "Err(why) => {",
                "self.refuse(rel, why);",
                "return;",
                "}",
            ],
            "the window screen below skips this whole `match` in order to \
             excuse the one bare `refuse` that belongs in it — `resolve`'s \
             `Err` arm, which has no path to key on. That skip is only \
             honest while the `Ok` arm is `Ok(full) => full,` and nothing \
             else: a decision made there is made after the path exists and \
             before the walk keys it, which is the double-record defect \
             exactly. If you need code \
             in that arm, the window must start before the `match` and the \
             `Err` arm must be excused by name instead."
        );

        let window: Vec<&str> = visit
            .lines()
            .map(str::trim)
            .skip_while(|l| !l.contains("resolve(work_tree, rel)"))
            // Past the match's own closing brace: its `Err` arm refuses a path
            // that does not exist, which is the one bare `refuse` that belongs.
            .skip_while(|l| *l != "};")
            .take_while(|l| !l.contains("self.recorded.insert("))
            .filter(|l| !l.starts_with("//"))
            .collect();
        // The window is bounded by source literals, so it is EMPTY whenever an
        // anchor stops matching — and an empty window satisfies the assertion
        // below without screening anything. Measured, not supposed: hoisting the
        // call out of the `match` (`let decided = resolve(…); let full = match
        // decided {`), restoring that defect's bare `refuse` in the interval and
        // relaxing the inventory to match left this whole test GREEN, and only
        // the behavioural fifth shape of `a_path_named_twice_is_captured_once`
        // went red. A refactor that is not about exits at all removed the
        // structural floor with nothing to say so.
        //
        // So the window must contain the thing the interval exists FOR. That is
        // the `lstat` — the one syscall between the path being resolved and the
        // path being keyed, and the reason a decision can be made in there at
        // all. It is deliberately not an exit: anchoring on that defect's own
        // `refuse_once` would make this assertion fire whenever that exit
        // moved, which is the inventory's job, and would confound "the window
        // drifted" with "the exit changed".
        assert!(
            window.contains(&"let Ok(meta) = std::fs::symlink_metadata(&full) else {"),
            "the window screen is not looking at the interval it screens. It \
             runs from the line calling `resolve(work_tree, rel)`, past the \
             `}};` that closes that decision, to the `self.recorded.insert(` \
             that keys the path — and if any of those three anchors has moved \
             or been respelled the window is empty and the assertion below \
             passes vacuously. What it saw instead: {window:?}"
        );
        let unkeyed_window: Vec<&str> = window
            .iter()
            .copied()
            .filter(|l| l.contains(".refuse(") || l.contains("::refuse("))
            .collect();
        assert_eq!(
            unkeyed_window,
            [] as [&str; 0],
            "between the path being resolved and the path being keyed, a bare \
             `refuse` writes a record for a path the walk never keys — so a \
             second spelling writes it again and charges `MAX_ENTRIES` twice. \
             That interval takes `refuse_once` only."
        );
    }

    /// The sibling inventory: **no byte string becomes a record by being
    /// approximated.** Two of the three sites below were `to_string_lossy`
    /// calls that shipped in this delta's own first commit and were found by a
    /// verifier rather than by a test — a symlink target recorded as a link the
    /// filesystem does not hold, and a directory entry recorded as a path the
    /// kernel does not have. Neither was visible in a diff review, because a
    /// lossy conversion is the *shortest* way to write each line.
    ///
    /// So the module's own rule ([`Refusal::Binary`]: capturing binary bytes
    /// would "store a lossy transcription while claiming to hold the file") is
    /// pinned structurally, in the shape this module already uses for its path
    /// derivations. Adding a fourth site is a test edit, and a test edit is a
    /// sentence in a commit message.
    ///
    /// The survivor in [`resolve`] is listed rather than removed because it
    /// cannot lose anything: it feeds two *comparisons* and never the path,
    /// which is pushed from the original component.
    ///
    /// **What this screen missed, and what it still cannot see.** It watched
    /// `_lossy` and `.to_str()`, which is two of the spellings; `Path::display`
    /// is a third and passed unseen, and the counterexample is live in this
    /// crate rather than hypothetical — `provision.rs:60` writes
    /// `report.install_root.display().to_string()` into the durable
    /// `substrate_install` row, so a non-UTF-8 install root would be stored as
    /// a path the filesystem does not hold. That is Rift's own path and low
    /// severity, it is outside this item, and it is named here with its file
    /// and line so the next reader does not have to find it twice. The screen
    /// below now takes `.display()` too, so a fourth site cannot appear
    /// *in this module* by that spelling.
    ///
    /// **The site outside this module is not the only one, and it is the only
    /// durable one.** `parity.rs:399` —
    /// a case-insensitive search for the retired token (assembled from pieces) —
    /// is the same spelling in the benign class: it feeds a boolean predicate
    /// and never a record, exactly like the [`resolve`] survivor above, so an
    /// approximation there can change an answer but cannot become a stored
    /// path. Named because the paragraph above, alone, reads as "the one thing
    /// this screen cannot see is a durable write", and the honest shape of the
    /// blind spot is that it cannot see either class.
    ///
    /// The blind spot is stated rather than closed, in the shape the sibling
    /// test `only_one_place_in_this_module_turns_an_entry_into_a_path` already
    /// uses: this reads one module's product half by substring, so a conversion
    /// spelled some other way (`String::from_utf8_lossy` on a slice held
    /// elsewhere, `OsStr::to_owned` then a `Display` impl, a helper in another
    /// module) passes it, and it says nothing at all about the rest of the
    /// crate. It is a tripwire on the shortest ways to write the mistake, not a
    /// proof that the mistake is unwritable.
    #[test]
    fn nothing_this_module_records_is_a_lossy_transcription() {
        let product = product_half();
        let inventory: Vec<&str> = product
            .lines()
            .map(str::trim)
            .filter(|l| !l.starts_with("//"))
            .filter(|l| l.contains("_lossy") || l.contains(".to_str()") || l.contains(".display()"))
            .collect();
        assert_eq!(
            inventory,
            [
                // Compared, never pushed: `resolve` builds the path from the
                // original component, and `.git` is four ASCII bytes.
                "let text = name.to_string_lossy();",
                // A discovered name the document cannot spell: refused.
                "match name.to_str() {",
                // A symlink target the document cannot spell: refused.
                "let Some(target) = target.to_str().map(str::to_owned) else {",
            ],
            "every byte string this module turns into text must either be \
             refused when the conversion would lose something, or provably \
             feed nothing but a comparison. A new `to_string_lossy` or \
             `.display()` here writes a record that reads back perfectly and \
             names something the filesystem does not hold — which is what \
             `Refusal::Binary` already refuses for file content, and is the \
             rule for every field, not just that one. This screen is a \
             tripwire on three spellings in one module, not a proof: see its \
             docstring for what it cannot see, including a live `.display()` \
             durable write elsewhere in this crate."
        );
    }

    /// The control file is a door too, and it was the one left open. Git stores
    /// a symlink as mode `120000` and clone recreates it, so a repository can
    /// **ship** `.worktreeinclude` pointed at a file only the developer can
    /// read. Reading through it turns every line of `~/.aws/credentials` into an
    /// include entry — and an entry that names nothing in the tree is echoed
    /// verbatim into the durable document as `x <line> missing`.
    #[cfg(unix)]
    #[test]
    fn a_control_file_symlinked_out_of_the_tree_is_never_read() {
        let dir = tempfile::tempdir().unwrap();
        let credentials = dir.path().join("credentials");
        std::fs::write(
            &credentials,
            "aws_access_key_id = AKIA-EXFILTRATED-BY-THE-CONTROL-FILE\n",
        )
        .unwrap();
        let work = dir.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        std::fs::write(work.join("ordinary.txt"), "in the tree").unwrap();
        std::os::unix::fs::symlink(&credentials, work.join(INCLUDE_FILE)).unwrap();

        let text = doc(&work);
        assert!(
            !text.contains("AKIA-EXFILTRATED-BY-THE-CONTROL-FILE"),
            "a symlinked control file must not be read through: {text}"
        );
        assert!(
            capture_document(&work).as_slice().is_empty(),
            "and the capture is the empty one, byte for byte: {text}"
        );
    }

    /// The same door, pointed at the substrate instead of the home directory.
    /// `hard_refusal` guards *walked* paths; it never saw the control file, so
    /// `.git`'s own bytes could be read as a list of paths and echoed back out.
    #[cfg(unix)]
    #[test]
    fn a_control_file_symlinked_into_the_git_dir_is_never_read() {
        let dir = tempfile::tempdir().unwrap();
        rift_git::test_support::fake_repo(dir.path()).unwrap();
        std::fs::write(
            dir.path().join(".git/config"),
            "[remote \"origin\"]\n\turl = https://x:GIT-CONFIG-TOKEN-c0ffee@host/r.git\n",
        )
        .unwrap();
        std::os::unix::fs::symlink(
            dir.path().join(".git/config"),
            dir.path().join(INCLUDE_FILE),
        )
        .unwrap();

        let text = doc(dir.path());
        assert!(
            !text.contains("GIT-CONFIG-TOKEN-c0ffee"),
            "the control file must not launder .git's bytes into the document: {text}"
        );
        assert!(capture_document(dir.path()).as_slice().is_empty(), "{text}");
    }

    /// The check is not only about symlinks. A directory and an absurd file are
    /// both "not a list", and both answer the way the ruling says: the empty
    /// capture, silently, with nothing added to the document.
    #[test]
    fn a_non_regular_or_oversize_control_file_is_the_empty_list() {
        let dir = tree(&[("a.txt", "kept")], None);
        std::fs::create_dir_all(dir.path().join(INCLUDE_FILE)).unwrap();
        assert!(
            capture_document(dir.path()).as_slice().is_empty(),
            "a directory is not a list"
        );

        let big = tree(&[("a.txt", "kept")], None);
        let padding = "# padding\n".repeat(usize::try_from(MAX_FILE_BYTES).unwrap() / 10 + 1);
        std::fs::write(big.path().join(INCLUDE_FILE), format!("a.txt\n{padding}")).unwrap();
        assert!(
            capture_document(big.path()).as_slice().is_empty(),
            "an oversize control file is declined whole, never parsed to its cap \
             — a git hook does not read an arbitrary number of bytes because a \
             repository asked it to"
        );
    }

    #[test]
    fn a_binary_file_is_refused_rather_than_transcribed() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("blob.bin"), [0xff_u8, 0xfe, 0x00, 0x01]).unwrap();
        std::fs::write(dir.path().join(INCLUDE_FILE), "blob.bin\n").unwrap();
        assert_eq!(doc(dir.path()), "rift-capture/1\nx blob.bin binary\n");
    }

    /// **The half-budget property, exercised in the band where it decides
    /// anything: over the per-file cap, UNDER the document budget.**
    ///
    /// That band is the whole point. While the cap was a flat 256 KiB inside an
    /// 8 KiB budget the refusal still fired — but only for files 32× larger than
    /// the whole document, and everything between 8 KiB and 256 KiB was refused
    /// by the BUDGET instead, so the per-file cap decided nothing. A fixture
    /// that only tripped it at `MAX_FILE_BYTES + 1` could not tell the two
    /// apart; this one asserts its own premise so it cannot quietly stop being
    /// about fairness if either constant moves again.
    ///
    /// Fairness is the second half and is what a refusal alone does not prove:
    /// the oversize file is declined and **its neighbours are still captured**.
    /// One large file must not crowd out the rest of the include list.
    #[test]
    fn one_oversize_file_is_refused_without_crowding_out_its_neighbours() {
        let dir = tempfile::tempdir().unwrap();
        let cap = usize::try_from(MAX_FILE_BYTES).unwrap();
        let hog = "x".repeat(cap + 1);
        assert!(
            hog.len() < MAX_TOTAL_BYTES,
            "premise: the hog must fit the DOCUMENT budget and be refused by the \
             PER-FILE cap — otherwise this tests the budget, not fairness \
             ({} bytes vs budget {MAX_TOTAL_BYTES})",
            hog.len()
        );
        std::fs::write(dir.path().join("hog.txt"), &hog).unwrap();
        std::fs::write(dir.path().join("a.txt"), "alpha\n").unwrap();
        std::fs::write(dir.path().join("b.txt"), "beta\n").unwrap();
        std::fs::write(dir.path().join(INCLUDE_FILE), "a.txt\nb.txt\nhog.txt\n").unwrap();

        let text = doc(dir.path());
        assert!(
            text.contains("x hog.txt too-large"),
            "the per-file cap refused it, and said so as `too-large` rather than \
             `budget-spent`: {text}"
        );
        assert!(
            text.contains("alpha") && text.contains("beta"),
            "and the files it would have crowded out are still captured: {text}"
        );
    }

    #[test]
    fn an_oversize_file_is_refused_whole_never_truncated() {
        let dir = tempfile::tempdir().unwrap();
        let big = "x".repeat(usize::try_from(MAX_FILE_BYTES).unwrap() + 1);
        std::fs::write(dir.path().join("big.txt"), &big).unwrap();
        std::fs::write(dir.path().join(INCLUDE_FILE), "big.txt\n").unwrap();
        assert_eq!(doc(dir.path()), "rift-capture/1\nx big.txt too-large\n");
    }

    #[test]
    fn a_missing_or_unsafe_entry_is_named_not_dropped() {
        let dir = tree(&[], Some("gone.txt\n../escape\n/abs\n"));
        let text = doc(dir.path());
        assert!(text.contains("x gone.txt missing"), "{text}");
        assert!(text.contains("x ../escape unsafe-path"), "{text}");
        assert!(text.contains("x /abs unsafe-path"), "{text}");
    }

    /// **A summary is written under whatever token the record earned, so the
    /// token says neither which summary nor whether either.** The previous
    /// version of this test asserted only the `unsafe-path` instance, and the
    /// docstring above it read "this token carries BOTH of the module's summary
    /// records" — which is a claim about a token that this test could not
    /// falsify, in a module whose discipline is that every claim names the test
    /// that would fail if it were false. It is false: the path field and the
    /// reason field are decided independently, and both callers that can hand
    /// [`Walk::refuse`] a summary charge the record cap before the refusal is
    /// chosen.
    ///
    /// So the same summary is measured here under three tokens, from three
    /// ordinary trees and no permissions trick — `unsafe-path`, `git-path`
    /// ([`hard_refusal`] tests `.git` before path safety) and `too-many`
    /// ([`Walk::visit`] charges [`Walk::cap_spent`] before [`hard_refusal`]) —
    /// plus the unnameable summary under `too-many`, which [`Walk::walk_dir`]
    /// reaches by the same charge-first ordering one function over. That last
    /// one is asserted at the decision rather than end-to-end because no
    /// filesystem on this host will hold the name (APFS: `EILSEQ`), the same
    /// posture `a_discovered_name_is_a_path_to_visit_or_a_refusal_to_record`
    /// already uses.
    ///
    /// Neither summary is an identity. Two distinct paths of the same length
    /// render the identical line, and a repository may legally hold a file
    /// named exactly like either summary — which is why the shape is asserted
    /// here rather than described, and why the format text withdraws the
    /// promise instead of narrowing it to one token.
    #[test]
    fn a_summary_is_written_under_whatever_token_the_record_earned() {
        let entry = "a\u{7}b.txt";
        let dir = tree(&[], Some(&format!("{entry}\n")));
        assert_eq!(
            doc(dir.path()),
            format!(
                "{CAPTURE_DOC_HEADER}\nx <{} unprintable bytes> unsafe-path\n",
                entry.len()
            ),
            "a control-bearing path is summarised"
        );

        // `.git` is decided before path safety, so the summary is a `git-path`
        // record — an `x` line whose reason is not `unsafe-path` and whose path
        // field is still a byte count.
        let git = ".git/a\u{7}b";
        let dir = tree(&[], Some(&format!("{git}\n")));
        assert_eq!(
            doc(dir.path()),
            format!(
                "{CAPTURE_DOC_HEADER}\nx <{} unprintable bytes> git-path\n",
                git.len()
            ),
            "…under `git-path` when the `.git` screen answered first"
        );

        // And the record cap is charged before the spelling is judged at all,
        // so a control-bearing entry that arrives late is summarised under
        // `too-many`.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("q")).unwrap();
        for i in 0..MAX_ENTRIES {
            std::fs::write(dir.path().join(format!("q/f{i:04}.txt")), "x").unwrap();
        }
        let late = "z\u{7}b.txt";
        std::fs::write(dir.path().join(INCLUDE_FILE), format!("q/\n{late}\n")).unwrap();
        let text = doc(dir.path());
        assert!(
            text.ends_with(&format!("x <{} unprintable bytes> too-many\n", late.len())),
            "…and under `too-many` when the cap was spent first: {}",
            text.lines().next_back().unwrap_or_default()
        );

        // The unnameable summary takes the same charge one function over
        // ([`Walk::walk_dir`] runs `cap_spent(&summary)` before refusing it), so
        // it is not an `unsafe-path`-only shape either.
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;

            let summary =
                discovered("d", std::ffi::OsStr::from_bytes(b"na\xff\xfeme")).unwrap_err();
            let mut w = Walk {
                doc: Vec::new(),
                records: MAX_ENTRIES,
                bytes: 0,
                recorded: std::collections::BTreeSet::new(),
                stopped: false,
            };
            assert!(w.cap_spent(&summary), "the cap is spent");
            assert_eq!(
                String::from_utf8(w.doc).unwrap(),
                "x d/<6 unnameable bytes> too-many\n",
                "the unnameable summary is the path field of a `too-many` record"
            );
        }
    }

    /// **A keyed record always names its path literally**, and only unkeyed
    /// exits can ever carry a summary. This is the test that fails if that
    /// stops being true.
    ///
    /// It is the *true* version of a sentence this delta withdrew twice. The
    /// withdrawn one gave the summary to a token, which is false — the path
    /// field and the reason field are decided independently
    /// (`a_summary_is_written_under_whatever_token_the_record_earned`). What is
    /// true lines up exactly with [`Walk::recorded`]'s keyed/unkeyed partition,
    /// and it is **provable rather than measured**, from three facts each of
    /// which already has a test that fails if it is false:
    ///
    /// 1. [`Walk::refuse`] summarises exactly when `rel` holds a control
    ///    character, and otherwise writes `rel` byte for byte (its body, pinned
    ///    by the writers screen in
    ///    `every_refusal_exit_in_the_walk_is_enumerated_and_classified`, and
    ///    measured in both directions by the tests either side of this one).
    /// 2. [`hard_refusal`] refuses **every** control character, and
    ///    [`Walk::visit`] runs it before [`resolve`]. Asserted below over the
    ///    whole `char` space rather than a sample.
    /// 3. All eleven keyed exits are downstream of that screen — which is what
    ///    the exit inventory pins, in source order.
    ///
    /// So `refuse`'s summary branch **cannot fire at a keyed exit**.
    ///
    /// 4. **The module's other summary is not `refuse`'s, and the three
    ///    premises above say nothing about it.** [`discovered`]'s
    ///    `<parent>/<N unnameable bytes>` arrives at [`Walk::refuse`] *as* the
    ///    caller's `rel`; it holds no control character, so it is written out
    ///    literally, and no screen is involved in that at all. What keeps it
    ///    off a keyed record is where it can travel: [`discovered`] has one
    ///    call site, its `Err` summary is consumed in that call's own `Err`
    ///    arm, and that arm holds [`Walk::walk_dir`]'s two exits — **both
    ///    unkeyed**, and pinned as unkeyed by the exit inventory. It reaches no
    ///    keyed exit because it reaches no other exit. Argue this half from the
    ///    control-character premise instead and the argument is simply invalid;
    ///    it happens to reach a true conclusion.
    ///
    /// So neither summary can appear on a keyed record, for two different
    /// reasons: one is screened out of those exits, the other never travels to
    /// them. A keyed record's path field is the path the walk decided about,
    /// and a reader may treat it as one.
    ///
    /// **The converse is not symmetric, and "the five unkeyed exits are the
    /// ones that carry summaries" would be false at one of them.** [`resolve`]'s
    /// `Err` arm is unkeyed and *also* downstream of [`hard_refusal`], so it
    /// names its path literally too — and since it is the only exit that can
    /// write [`Refusal::SymlinkedParent`], that token never appears on a
    /// summary either. Four of the five unkeyed exits can: [`Walk::visit`]'s
    /// `too-many` and [`hard_refusal`]'s own refusal write `refuse`'s
    /// `<N unprintable bytes>`, and [`Walk::walk_dir`]'s two are handed
    /// [`discovered`]'s `<parent>/<N unnameable bytes>` as their `rel` already.
    /// **By token: `unsafe-path`, `git-path` and `too-many` are the only three
    /// of the eleven that can ever appear on a summary record. The other eight
    /// — the seven keyed ones and `symlinked-parent` — never can.**
    ///
    /// The document-wide half of this runs as a search rather than as three
    /// trees: `a_captured_files_parent_always_resolves_inside_the_work_tree`
    /// asserts it per `x` record over a vocabulary that draws a bare control
    /// character on purpose.
    ///
    /// The partition the property is stated over is checked here too, first:
    /// every [`Refusal`] variant lands on exactly one of [`KEYED_TOKENS`] and
    /// [`UNKEYED_TOKENS`], so "a partition of the token space" is a thing a
    /// test fails on rather than a sentence in a docstring.
    #[test]
    fn a_keyed_record_always_names_its_path_literally() {
        // The partition itself, because the docstrings call it one. Every
        // `Refusal` variant is on exactly one side, so a twelfth variant, or a
        // token moved without being removed, or a rename of a token string,
        // reddens here rather than leaving a sentence that no longer describes
        // the enum. (WHICH side a token is on is asserted text; the exit
        // inventory and `a_path_named_twice_is_captured_once` are what fail if
        // that is wrong.)
        let all = [
            Refusal::GitPath,
            Refusal::UnsafePath,
            Refusal::SymlinkedParent,
            Refusal::Missing,
            Refusal::NotAFile,
            Refusal::UnsafeLinkTarget,
            Refusal::TooLarge,
            Refusal::Binary,
            Refusal::TooMany,
            Refusal::BudgetSpent,
            Refusal::TooDeep,
        ];
        for why in all {
            let token = why.as_str();
            assert_ne!(
                KEYED_TOKENS.contains(&token),
                UNKEYED_TOKENS.contains(&token),
                "`{token}` is on both sides of the partition, or on neither — \
                 add it to exactly one, and to `Walk::recorded`'s enumeration, \
                 in the same commit"
            );
        }
        let mut tokens: Vec<&str> = all.iter().map(|w| w.as_str()).collect();
        tokens.sort_unstable();
        let mut listed: Vec<&str> = KEYED_TOKENS
            .iter()
            .chain(&UNKEYED_TOKENS)
            .copied()
            .collect();
        listed.sort_unstable();
        assert_eq!(
            tokens, listed,
            "the two lists must cover `Refusal` exactly: 7 keyed + 4 unkeyed = \
             11 variants, disjoint, no gaps"
        );

        // Premise 2, over the whole `char` space rather than a sample: every
        // character that would make `refuse` summarise is refused before
        // `resolve` runs. The count is asserted so that narrowing either
        // `is_control` call cannot quietly empty this loop.
        let controls: Vec<char> = (0u32..=0x0010_FFFF)
            .filter_map(char::from_u32)
            .filter(|c| c.is_control())
            .collect();
        assert_eq!(
            controls.len(),
            65,
            "the Cc block is 0x00–0x1F, 0x7F–0x9F; if this moved, the two \
             `is_control` calls this property rests on are no longer about the \
             same set"
        );
        for c in controls {
            assert_eq!(
                hard_refusal(&format!("a{c}b.txt")),
                Some(Refusal::UnsafePath),
                "U+{:04X} makes `Walk::refuse` summarise, so it must never \
                 reach an exit that keys",
                c as u32
            );
        }

        // The composition at a keyed exit's own shape, end to end. The file is
        // real, readable and regular: without the screen this entry resolves,
        // `lstat`s clean and reaches `file`'s KEYED capture. With it, the walk
        // never gets there, and the record that names the summary is unkeyed.
        #[cfg(unix)]
        {
            let dir = tempfile::tempdir().unwrap();
            let entry = "a\u{7}b.txt";
            std::fs::write(dir.path().join(entry), "SECRET-CONTENT\n").unwrap();
            std::fs::write(dir.path().join(INCLUDE_FILE), format!("{entry}\n")).unwrap();
            let text = doc(dir.path());
            assert_eq!(
                text,
                format!(
                    "{CAPTURE_DOC_HEADER}\nx <{} unprintable bytes> unsafe-path\n",
                    entry.len()
                ),
                "the file exists and could be read — the screen, not the \
                 filesystem, is what keeps a summary off a keyed exit"
            );
            assert!(!text.contains("SECRET-CONTENT"), "{text}");

            // And the one unkeyed exit that can never summarise either:
            // `resolve`'s `Err` arm is downstream of the same screen. `in` is a
            // symlinked ancestor, so an ordinary entry under it earns
            // `symlinked-parent` — but a control-bearing one is answered by
            // `hard_refusal` first and never reaches `resolve` at all. That is
            // why `symlinked-parent` is not on the summary-capable list.
            let dir = tempfile::tempdir().unwrap();
            let work = doors(dir.path());
            assert_eq!(
                resolve(&work, "in/a.txt"),
                Err(Refusal::SymlinkedParent),
                "`in` is the symlinked ancestor this exit exists for"
            );
            let entry = "in/a\u{7}b.txt";
            std::fs::write(work.join(INCLUDE_FILE), format!("{entry}\n")).unwrap();
            assert_eq!(
                doc(&work),
                format!(
                    "{CAPTURE_DOC_HEADER}\nx <{} unprintable bytes> unsafe-path\n",
                    entry.len()
                ),
                "the screen answers before `resolve` does, so no summary is \
                 ever written under `symlinked-parent`"
            );
        }
    }

    #[test]
    fn a_spent_budget_stops_the_walk_instead_of_narrating_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        let big = dir.path().join("many");
        std::fs::create_dir_all(&big).unwrap();
        for i in 0..(MAX_ENTRIES + 32) {
            std::fs::write(big.join(format!("f{i:04}.txt")), "x").unwrap();
        }
        std::fs::write(dir.path().join(INCLUDE_FILE), "many/\n").unwrap();

        let text = doc(dir.path());
        assert_eq!(
            text.lines().filter(|l| l.starts_with("f ")).count(),
            MAX_ENTRIES,
            "captured up to the cap"
        );
        // …and exactly ONE refusal, not one per remaining file: a per-file
        // narration of a spent budget is how `node_modules/` turns a hook into a
        // multi-megabyte document the redactor has to re-scan every commit.
        assert_eq!(
            text.lines().filter(|l| l.starts_with("x ")).count(),
            1,
            "the walk stops at the cap: {}",
            text.lines().filter(|l| l.starts_with("x ")).count()
        );
        assert!(text.trim_end().ends_with("too-many"), "and says why");
    }

    /// A refusal is a line in the document and a read is work the hook paid
    /// for, so both budgets charge them. Measured before the change: an
    /// opted-in directory of 600 × 64 KiB binary files produced **600** refusal
    /// lines and read **39 MB**, with no budget in the way — the very
    /// multi-megabyte-in-a-commit-hook shape [`Walk::exhausted`] describes,
    /// reached by the one road it did not cover, and shippable in a repository.
    #[test]
    fn declined_files_spend_the_budgets_they_cost() {
        // Many small declines: the RECORD cap ends the walk.
        let many = tempfile::tempdir().unwrap();
        let small = many.path().join("bin");
        std::fs::create_dir_all(&small).unwrap();
        for i in 0..(MAX_ENTRIES + 32) {
            std::fs::write(small.join(format!("b{i:04}.bin")), [0xff_u8]).unwrap();
        }
        std::fs::write(many.path().join(INCLUDE_FILE), "bin/\n").unwrap();
        let text = doc(many.path());
        assert_eq!(
            text.lines().filter(|l| l.starts_with("x ")).count(),
            MAX_ENTRIES + 1,
            "the declines themselves are capped, plus the one record that says \
             the walk stopped: {text}"
        );
        assert!(
            text.trim_end().ends_with("too-many"),
            "and says why: {text}"
        );

        // A few large declines: the BYTE cap ends the walk, charged on the read
        // rather than on the record, because reading is what it cost.
        let big = tempfile::tempdir().unwrap();
        let heavy = big.path().join("bin");
        std::fs::create_dir_all(&heavy).unwrap();
        // Sized against the BUDGET, not against `MAX_FILE_BYTES`. It used to be
        // one `MAX_FILE_BYTES` chunk per file, which made the expected count
        // `MAX_TOTAL_BYTES / 256 KiB` — 4 at the old 1 MiB ceiling and **0** at
        // the 8 KiB one, an assertion that passes without reading anything and
        // proves nothing about the budget. A quarter-budget chunk keeps the
        // shape (4 fit, 4 refused) at any ceiling.
        let chunk = vec![0xff_u8; MAX_TOTAL_BYTES / 4];
        for i in 0..8 {
            std::fs::write(heavy.join(format!("b{i}.bin")), &chunk).unwrap();
        }
        std::fs::write(big.path().join(INCLUDE_FILE), "bin/\n").unwrap();
        let text = doc(big.path());
        assert_eq!(
            text.lines().filter(|l| l.ends_with("binary")).count(),
            MAX_TOTAL_BYTES / chunk.len(),
            "exactly the budget's worth is read before the walk stops: {text}"
        );
        assert!(
            text.trim_end().ends_with("budget-spent"),
            "and says why: {text}"
        );

        // The shape [`Walk::exhausted`] names, at the size that produces its
        // numbers: 64 KiB declines, sixteen of which fit the byte budget. Its
        // docstring claimed "16 reads and 17 records" while the code read 17 —
        // nothing bound either number. Both are bound here now.
        let sized = tempfile::tempdir().unwrap();
        let sized_bin = sized.path().join("bin");
        std::fs::create_dir_all(&sized_bin).unwrap();
        // A sixteenth of the budget, so sixteen fit and the seventeenth
        // overruns — the shape this probe's comments describe. Hard-coding
        // 64 KiB made `fits` 16 at the old ceiling and **0** at 8 KiB, which
        // silently moved "the seventeenth file" onto `b00.bin` and left the
        // read-count proof asserting nothing.
        let block = vec![0xff_u8; MAX_TOTAL_BYTES / 16];
        let fits = MAX_TOTAL_BYTES / block.len();
        for i in 0..(fits + 4) {
            std::fs::write(sized_bin.join(format!("b{i:02}.bin")), &block).unwrap();
        }
        // The seventeenth file — the one that overruns — is unreadable. A walk
        // that read before it decided would answer `missing` here; only a walk
        // that decides on the size it already has can answer `budget-spent`.
        // That is the whole proof of the read count, since a read leaves no
        // other trace in the document.
        //
        // Each platform has one way to make a file `stat` can size but `read`
        // cannot open: mode `0o000` on Unix; on Windows a handle held open with
        // no share mode, which turns every later open into a sharing violation
        // while the directory listing and `symlink_metadata` still answer.
        // The handle lives until the walk below has finished.
        let overruns = sized_bin.join(format!("b{fits:02}.bin"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&overruns, std::fs::Permissions::from_mode(0o000)).unwrap();
        }
        #[cfg(windows)]
        let _held_open = {
            use std::os::windows::fs::OpenOptionsExt as _;
            std::fs::OpenOptions::new()
                .read(true)
                .share_mode(0)
                .open(&overruns)
                .unwrap()
        };
        assert!(
            std::fs::read(&overruns).is_err(),
            "premise of the probe below: this process cannot read the \
             seventeenth file. A run with the privilege to read it (root) \
             cannot distinguish the two walks and must not report that it did."
        );
        std::fs::write(sized.path().join(INCLUDE_FILE), "bin/\n").unwrap();
        let text = doc(sized.path());
        assert_eq!(
            (
                text.lines().skip(1).count(),
                text.lines().filter(|l| l.ends_with("binary")).count(),
            ),
            (fits + 1, fits),
            "16 reads and 17 records: sixteen files are read, charged and \
             declined, and the seventeenth ends the walk — so a 600-file \
             directory of these costs the same, the other 583 never opened: \
             {text}"
        );
        let last = format!("x bin/b{fits:02}.bin budget-spent");
        assert_eq!(
            text.lines().last(),
            Some(last.as_str()),
            "and the file that overruns is refused WITHOUT being opened — it \
             is unreadable, so a walk that opened it would say `missing`: \
             {text}"
        );
    }

    /// [`MAX_TOTAL_BYTES`] bounds bytes **read**, and this is the gap between
    /// that and the document those bytes become — measured at the ceiling, on
    /// the shape that maximises it.
    ///
    /// The constant is the input budget for a git hook, so it is the number
    /// anyone sizing the *downstream* cost reaches for, and it is the wrong one:
    /// [`Walk::file`] writes a one-space prefix per content line, so a file that
    /// is all newlines doubles. The whole budget spent on that shape hands
    /// `redact` a document just over **16 KiB**. Pinned here rather than argued
    /// because the doubling is invisible in the constant and the arithmetic is
    /// the easy half — a cost that is worse than linear in its input pays the
    /// square of a 2× error, which is the difference between a budget missed
    /// and a budget missed by 4×.
    #[test]
    fn the_document_handed_on_is_up_to_twice_the_bytes_read() {
        // Quarter-budget files, so four spend the budget exactly. Against
        // `MAX_FILE_BYTES` this computed **zero** files at the 8 KiB ceiling and
        // the test reddened outright — the honest half of the fallout, and the
        // reason the other two sites above were worth checking rather than
        // trusting.
        let per_file = MAX_TOTAL_BYTES / 4;
        let files = MAX_TOTAL_BYTES / per_file;
        let body = "\n".repeat(per_file);
        let dir = tempfile::tempdir().unwrap();
        let mut list = String::new();
        for i in 0..files {
            std::fs::write(dir.path().join(format!("f{i}")), &body).unwrap();
            list.push_str(&format!("f{i}\n"));
        }
        std::fs::write(dir.path().join(INCLUDE_FILE), &list).unwrap();

        let document = capture_document(dir.path()).as_slice().len();
        assert!(
            document > 2 * MAX_TOTAL_BYTES,
            "the whole read budget spent on empty lines is more than twice \
             itself as a document: {document} bytes from {MAX_TOTAL_BYTES} read"
        );
        assert!(
            document < 2 * MAX_TOTAL_BYTES + 1024,
            "and the overhead beyond the doubling is per-record framing, not \
             per-byte, so it stays small: {document}"
        );
    }

    /// [`IncludeList::parse`] deduplicates entry *strings*, which does not catch
    /// a directory root and a path inside it. `d/` plus `d/a.txt` recorded
    /// `f d/a.txt` **twice**, content and all, with both copies counted against
    /// the caps — a document that is not a function of the paths named.
    ///
    /// **The fourth shape is the counterexample this test used to stop one
    /// short of**, and it is the reason the first three are not enough on their
    /// own: file, link and `too-large` are all *non-directory* decisions, and
    /// [`Walk::visit`] keys only those. A directory earns a record of its own
    /// at two exits, and neither was keyed — `p` plus an unreadable `p/d` said
    /// `x p/d missing` twice. Three shapes agreeing is not the property; the
    /// property is over every record a path can earn, so the shape that has no
    /// key is the one worth the test.
    ///
    /// **And the fifth shape is the one the fourth still stopped short of** —
    /// the same sentence, the same test, a third exit. Adding the fourth fixed
    /// two *directory* exits and generalised the docstring to a property over
    /// every record, but the exit that has no key is not "a directory": it is
    /// any exit that decides **before** [`Walk::visit`]'s insert, and `visit`'s
    /// own `symlink_metadata` failure returns three lines before it. A directory
    /// at mode `r--` is the construction that reaches it — readable enough to
    /// list the child, not searchable enough to `lstat` it — and `d/` plus `d/x`
    /// wrote `x d/x missing` twice. Four shapes agreeing was not the property
    /// either, so [`Walk::recorded`] now carries the enumeration of all sixteen
    /// refusal exits and this test targets the ones that key outside `visit`'s
    /// insert: two directory exits and one file exit, which is the whole of
    /// [`Walk::refuse_once`]'s caller set.
    ///
    /// What is *not* deduplicated has its own test rather than a caveat here:
    /// `a_refusal_with_no_path_to_key_on_is_recorded_once_per_naming` and
    /// `a_second_case_spelling_is_a_second_path_to_this_document`.
    #[test]
    fn a_path_named_twice_is_captured_once() {
        let dir = tree(&[("d/a.txt", "CONTENT")], Some("d/\nd/a.txt\n"));
        assert_eq!(
            doc(dir.path()),
            format!("{CAPTURE_DOC_HEADER}\nf d/a.txt\n CONTENT\n"),
            "the walk reached it twice; the document names it once"
        );

        #[cfg(unix)]
        {
            let held = tempfile::tempdir().unwrap();
            let work = held.path().join("w");
            std::fs::create_dir_all(work.join("p")).unwrap();
            std::os::unix::fs::symlink("elsewhere", work.join("p/link")).unwrap();
            std::fs::write(work.join(INCLUDE_FILE), "p/\np/link\n").unwrap();
            let text = doc(&work);
            assert_eq!(
                text.lines().filter(|l| l.starts_with("l ")).count(),
                1,
                "and a link reached twice is one link record: {text}"
            );
        }

        // A path REFUSED twice is also one record, because the set is charged
        // for every non-directory path the walk decides about rather than only
        // for the ones it captures. That is what makes "one record per path
        // named" true of the whole document instead of only of its captures —
        // the reading `Walk::recorded`'s docstring used to understate.
        let refused = tree(&[("d/big.bin", "")], Some("d/\nd/big.bin\n"));
        std::fs::write(
            refused.path().join("d/big.bin"),
            vec![0_u8; usize::try_from(MAX_FILE_BYTES).unwrap() + 1],
        )
        .unwrap();
        assert_eq!(
            doc(refused.path()),
            format!("{CAPTURE_DOC_HEADER}\nx d/big.bin too-large\n"),
            "the walk reached the refusal twice; the document says it once"
        );

        // The shape the three assertions above stop one short of, and the one
        // that falsified the clause: a **directory** earns a record of its own
        // when it cannot be read, and `visit` inserts into `recorded` only for
        // non-directories. Named twice, `p/d` said `missing` twice.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let held = tempfile::tempdir().unwrap();
            let work = held.path().join("w");
            std::fs::create_dir_all(work.join("p/d")).unwrap();
            std::fs::write(work.join(INCLUDE_FILE), "p/\np/d\n").unwrap();
            std::fs::set_permissions(work.join("p/d"), std::fs::Permissions::from_mode(0o000))
                .unwrap();
            let unreadable = std::fs::read_dir(work.join("p/d")).is_err();
            let text = doc(&work);
            std::fs::set_permissions(work.join("p/d"), std::fs::Permissions::from_mode(0o700))
                .unwrap();
            if unreadable {
                assert_eq!(
                    text,
                    format!("{CAPTURE_DOC_HEADER}\nx p/d missing\n"),
                    "a DIRECTORY reached twice is one record too: {text}"
                );
            } else {
                // Running as root, where mode 000 does not stop `read_dir`.
                assert_eq!(text, format!("{CAPTURE_DOC_HEADER}"), "{text}");
            }
        }

        // The fifth shape, and the third exit at which this one property has
        // been reopened. The four above all reach a decision the walk keys —
        // three downstream of `visit`'s own insert, one through `refuse_once` —
        // and the exit that keyed nothing is `visit`'s OWN `symlink_metadata`
        // failure, which returns three lines BEFORE that insert. A directory at
        // mode `r--` is the construction that separates the two syscalls the
        // shape needs: `read_dir` lists the child (read permission), and `lstat`
        // on the child fails (no search permission), so the path is decided
        // about without ever being keyed. Named twice, `d/x` said `missing`
        // twice — two records, one path, two charges against `MAX_ENTRIES`.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let held = tempfile::tempdir().unwrap();
            let work = held.path().join("w");
            std::fs::create_dir_all(work.join("d")).unwrap();
            std::fs::write(work.join("d/x"), "CONTENT").unwrap();
            std::fs::write(work.join(INCLUDE_FILE), "d/\nd/x\n").unwrap();
            std::fs::set_permissions(work.join("d"), std::fs::Permissions::from_mode(0o400))
                .unwrap();
            let unsearchable = std::fs::symlink_metadata(work.join("d/x")).is_err();
            let text = doc(&work);
            std::fs::set_permissions(work.join("d"), std::fs::Permissions::from_mode(0o700))
                .unwrap();
            if unsearchable {
                assert_eq!(
                    text,
                    format!("{CAPTURE_DOC_HEADER}\nx d/x missing\n"),
                    "a path whose `lstat` fails is keyed by its refusal like any \
                     other path the walk decided about: {text}"
                );
            } else {
                // Running as root, where a missing execute bit does not stop
                // `lstat` — so the file is simply captured, once.
                assert_eq!(
                    text,
                    format!("{CAPTURE_DOC_HEADER}\nf d/x\n CONTENT\n"),
                    "{text}"
                );
            }
        }
    }

    /// **The exceptions, pinned rather than asserted away.** The identity this
    /// module deduplicates on is the resolved path, and it has exactly one — so
    /// a record that never had a path cannot be keyed at all, and is written
    /// once per naming rather than once per path.
    ///
    /// Two shapes reach it and both are recorded here so the format text can
    /// state them instead of promising a set. A refusal decided **before a
    /// resolved path exists** ([`hard_refusal`], and [`resolve`]'s own `Err`)
    /// has nothing to key on, so an entry the list types *and* the walk
    /// discovers earns two identical lines. And the
    /// `<parent>/<N unnameable bytes>` summary is never a path by construction
    /// — that is what it is *for* — so a directory named twice re-summarises
    /// the same child.
    ///
    /// [`resolve`]'s `Err` covers a **fourth** unkeyed token, `symlinked-parent`,
    /// which the filesystem decides rather than the spelling. It is enumerated
    /// here for completeness and not asserted below, because it cannot
    /// duplicate by construction: the walk descends only real directories, so a
    /// *discovered* path never has a symlinked ancestor, and two typed spellings
    /// of one entry are deduplicated as strings by `IncludeList::parse`. That is
    /// an argument, not a measurement — which is exactly why it is written here
    /// rather than asserted as a guarantee.
    ///
    /// A **fifth** unkeyed token completes the list [`Walk::recorded`]
    /// enumerates: `too-many`, at both callers of [`Walk::cap_spent`] —
    /// [`Walk::visit`] charges the cap before [`resolve`] has produced a path,
    /// and [`Walk::walk_dir`] charges it on the unnameable summary. Also not
    /// asserted below, and for a reason that *is* structural rather than an
    /// argument about shapes: [`Walk::exhausted`] sets `stopped`, and both
    /// loops test it, so one document holds at most one `too-many` record and
    /// there is no second naming to record. Listed anyway, because a token that
    /// cannot duplicate is still a token this set does not key, and treating
    /// those two as the same claim is what
    /// `a_path_named_twice_is_captured_once` was re-taught by its fifth shape.
    ///
    /// Inventing a second key (the entry string) would close both, and is the
    /// move this module has been rejected for three times: a check written in a
    /// different vocabulary than the operation it guards. The entry string is
    /// not what the walk opens, and a set keyed on it would answer a different
    /// question than the one `recorded` exists to answer.
    #[test]
    fn a_refusal_with_no_path_to_key_on_is_recorded_once_per_naming() {
        let typed_and_discovered = tree(&[("d/.git/config", "[core]")], Some("d/\nd/.git\n"));
        let text = doc(typed_and_discovered.path());
        assert_eq!(
            text.matches("x d/.git git-path").count(),
            2,
            "a spelling refusal is decided before `resolve` runs, so there is \
             no path to key it on and the walk records it once per naming: \
             {text}"
        );

        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;

            let held = tempfile::tempdir().unwrap();
            let work = held.path().join("w");
            std::fs::create_dir_all(work.join("p/d")).unwrap();
            std::fs::write(work.join(INCLUDE_FILE), "p/\np/d\n").unwrap();
            let raw: &[u8] = b"na\xff\xfeme";
            let hostile = work.join("p/d").join(std::ffi::OsStr::from_bytes(raw));
            if std::fs::write(&hostile, "HOSTILE-BYTES").is_ok() {
                let text = doc(&work);
                assert_eq!(
                    text.matches("x p/d/<6 unnameable bytes> unsafe-path")
                        .count(),
                    2,
                    "and a summary that is not a path cannot be keyed either: \
                     {text}"
                );
            }
            // macOS: APFS returns EILSEQ. The Linux leg adjudicates this half.
        }
    }

    /// **A path is a spelling, not an inode**, and the document never claims
    /// otherwise in either direction — which is the same fact
    /// `a_hardlink_is_captured_under_the_name_it_was_given` records from the
    /// other side.
    ///
    /// On a case-insensitive filesystem — the macOS default, and what this
    /// repository's own volumes are — `A.txt` and `a.txt` are one file and two
    /// paths, so the document holds two records and two copies of the content,
    /// and charges both against both caps for one inode. The caps still bound
    /// the document; they are spent, not defeated.
    ///
    /// It is recorded rather than closed. Closing it means folding case, and a
    /// fold this module computes will disagree with the filesystem's on some
    /// input — HFS+ froze Unicode 3.2, APFS did not, and a volume may be
    /// case-sensitive at all — which is precisely the shape that broke this
    /// module three times: a check written in a different vocabulary than the
    /// operation it guards. The alternative key, the inode, is worse: it would
    /// silently drop the hardlink record the module deliberately keeps.
    ///
    /// Adjudicated on whichever leg the filesystem decides; the case-sensitive
    /// arm below is the control, not a pass.
    #[test]
    fn a_second_case_spelling_is_a_second_path_to_this_document() {
        let dir = tree(&[("A.txt", "CONTENT")], Some("A.txt\na.txt\n"));
        let text = doc(dir.path());
        if std::fs::symlink_metadata(dir.path().join("a.txt")).is_ok() {
            assert_eq!(
                text,
                format!("{CAPTURE_DOC_HEADER}\nf A.txt\n CONTENT\nf a.txt\n CONTENT\n"),
                "one file, two spellings, two records and two copies: {text}"
            );
        } else {
            assert_eq!(
                text,
                format!("{CAPTURE_DOC_HEADER}\nf A.txt\n CONTENT\nx a.txt missing\n"),
                "case-sensitive control: the second spelling is another path \
                 and there is nothing at it: {text}"
            );
        }
    }

    /// An included directory speaks only through its children, so an empty one
    /// says nothing — and a repository whose include file names only empty
    /// directories keeps the exact capture, and the exact checkpoint id, it had
    /// before it opted in. Pinned in both shapes so the silence cannot be read
    /// as the walk having failed to look.
    #[test]
    fn an_include_naming_only_an_empty_directory_records_nothing() {
        let alone = tree(&[], Some("empty/\n"));
        std::fs::create_dir_all(alone.path().join("empty")).unwrap();
        assert!(
            capture_document(alone.path()).as_slice().is_empty(),
            "an empty directory is not a refusal, and not a record either"
        );

        let beside = tree(&[("a.txt", "hi")], Some("a.txt\nempty/\n"));
        std::fs::create_dir_all(beside.path().join("empty")).unwrap();
        assert_eq!(
            doc(beside.path()),
            "rift-capture/1\nf a.txt\n hi\n",
            "and it stays silent in a document that has something to say"
        );
    }

    #[test]
    fn the_document_is_deterministic_and_order_independent() {
        let a = tree(
            &[("d/b.txt", "bee"), ("d/a.txt", "ay")],
            Some("d/b.txt\nd/a.txt\n"),
        );
        let b = tree(
            &[("d/b.txt", "bee"), ("d/a.txt", "ay")],
            Some("d/a.txt\nd/b.txt\n"),
        );
        assert_eq!(doc(a.path()), doc(b.path()));
        assert_eq!(doc(a.path()), doc(a.path()), "and stable across runs");
        assert!(
            doc(a.path()).find("a.txt") < doc(a.path()).find("b.txt"),
            "sorted, not as typed"
        );
    }

    /// `./a.txt` is the same file as `a.txt` — to git, to the shell, and now
    /// here. It used to earn `x ./a.txt unsafe-path` (the `.` segment), which
    /// told a developer their line was dangerous when it was merely spelled the
    /// long way. A bare `.` still refuses: that one really does mean "the whole
    /// tree", which is the thing this module's default declines to capture.
    #[test]
    fn a_leading_dot_slash_is_stripped_but_a_bare_dot_still_refuses() {
        assert_eq!(
            IncludeList::parse("./a.txt\n.\\b.txt\n././c/\n.\n").roots(),
            [".", ".\\b.txt", "a.txt", "c"],
            "`./` is the OS's own no-op on both platforms and is stripped; \
             `.\\` is one on Windows only, so it is left to be refused rather \
             than rewritten into a file Unix would have read instead"
        );

        let dir = tree(&[("a.txt", "hi")], Some("./a.txt\n.\n"));
        let text = doc(dir.path());
        assert!(
            text.contains("f a.txt\n hi\n"),
            "captured under its real name, \
             not refused for how it was typed: {text}"
        );
        assert!(text.contains("x . unsafe-path"), "{text}");
    }

    /// The measured answer to "should an entry that means two files on two
    /// platforms be refused outright": yes, and the belt is a screen over
    /// [`resolve`], never a substitute for it — the second half of this test is
    /// what says so, and it is the half that must keep passing if the screen is
    /// ever removed.
    #[test]
    fn an_entry_that_two_platforms_would_read_differently_is_refused() {
        for spelling in [
            "src\\main.rs",
            "src\\",
            "bs\\/dir/id_rsa",
            // `:` is the other byte of the same class: an NTFS alternate data
            // stream separator (and the drive separator) on Windows, an
            // ordinary filename byte here. `.git:$I30` is how a Windows path
            // addresses the git dir's index without spelling `.git` as a
            // segment, which is why it is screened rather than left to a rule
            // that compares names.
            "a.txt:stream",
            ".git:$I30",
            "c:/x",
        ] {
            assert_eq!(
                hard_refusal(spelling),
                Some(Refusal::UnsafePath),
                "{spelling:?}: a path that means two things on two platforms \
                 has no single thing to capture"
            );
        }
        assert_eq!(hard_refusal("src/main.rs"), None, "the dialect git uses");

        // The screen runs on walk-DISCOVERED names too, not only on typed
        // ones, so a symlink already on disk whose name holds `\` is recorded
        // `unsafe-path` rather than as a link. Nothing is captured either way;
        // the record says "ambiguous spelling" instead of "symlink", which is
        // the honest answer for a name a Windows checkout could not hold.
        #[cfg(unix)]
        {
            let held = tempfile::tempdir().unwrap();
            let work = held.path().join("w");
            std::fs::create_dir_all(work.join("p")).unwrap();
            std::os::unix::fs::symlink("/etc", work.join("p/a\\b")).unwrap();
            std::fs::write(work.join(INCLUDE_FILE), "p/\n").unwrap();
            assert_eq!(
                doc(&work),
                format!("{CAPTURE_DOC_HEADER}\nx p/a\\b unsafe-path\n"),
                "a discovered name is screened like a typed one"
            );
        }

        let dir = tree(&[("a.txt", "hi")], Some("src\\main.rs\n"));
        assert_eq!(
            doc(dir.path()),
            "rift-capture/1\nx src\\main.rs unsafe-path\n",
            "recorded, so a Windows developer learns the file wants `/` — \
             where it used to capture on Windows and record `missing` on Unix, \
             one line meaning two things"
        );

        // …and with the screen out of the way, the decision still holds — for
        // `\`. This is the belt-and-braces claim, stated where it can fail, and
        // stated for the ONE byte it is true of.
        #[cfg(unix)]
        {
            let held = tempfile::tempdir().unwrap();
            let work = doors(held.path());
            assert_eq!(
                resolve(&work, "bs\\dir/id_rsa"),
                Err(Refusal::SymlinkedParent),
                "the screen refuses this spelling, and so does the decision \
                 underneath it — remove the `\\` screen and nothing escapes"
            );
            // The `:` half is the opposite, and the doc now says so. `:` is an
            // ordinary filename byte here as it is on Windows, so `resolve`
            // resolves an ADS-shaped name to an ordinary path in the tree; the
            // screen is what refuses it. Harmless on Unix, load-bearing there.
            assert_eq!(
                resolve(&work, "a.txt:stream"),
                Ok(work.join("a.txt:stream")),
                "`resolve` accepts a `:` name — the screen is not \
                 belt-and-braces over this byte, it is the only rule"
            );
        }
    }

    /// The `:` half of the screen is **not** belt-and-braces, and this is the
    /// assertion that says so on the platform where it matters.
    ///
    /// [`hard_refusal`]'s doc claims `.git:$I30` and `a.txt:stream` reach
    /// [`resolve`] as a single ordinary component on Windows — no drive prefix,
    /// because Rust's `parse_drive` wants an ASCII letter at byte 0, and no
    /// separator, because `:` is not one there either — so `resolve` returns
    /// `Ok` and the screen is the only refusal. That is a claim about Windows
    /// written from a Unix host, which is the exact shape that has been wrong
    /// three times in this module, so it is written as an assertion and **CI's
    /// Windows leg adjudicates it**, not this comment.
    ///
    /// Deliberately not `#[cfg(windows)]`: a Windows-only test cannot be
    /// compiled or reasoned about from here, and the assertion is meaningful on
    /// both platforms — on Unix it is the same fact with no security weight
    /// (`:` is an ordinary byte and the entry names a file in the tree), on
    /// Windows it is an alternate-data-stream route to the git dir. If the
    /// prefix parse were ever what the deleted sentence assumed, `parsed` would
    /// hold a `Component::Prefix` and this would fail there and pass here.
    #[test]
    fn only_the_colon_screen_refuses_an_alternate_data_stream_name() {
        let dir = tempfile::tempdir().unwrap();
        for spelling in [".git:$I30", "a.txt:stream"] {
            let parsed: Vec<_> = Path::new(spelling).components().collect();
            assert_eq!(
                parsed.len(),
                1,
                "{spelling:?}: parsed as one component, not a drive-qualified \
                 path: {parsed:?}"
            );
            assert!(
                matches!(parsed[0], Component::Normal(_)),
                "{spelling:?}: and that component is an ordinary name: {parsed:?}"
            );
            assert!(
                resolve(dir.path(), spelling).is_ok(),
                "{spelling:?}: so `resolve` accepts it — the ancestor rule \
                 cannot see an alternate data stream, which is why the `:` rule \
                 is the only one refusing this and not a second opinion"
            );
            assert_eq!(
                hard_refusal(spelling),
                Some(Refusal::UnsafePath),
                "{spelling:?}: …leaving the screen as the only thing refusing it"
            );
        }
    }

    /// A refusal record echoes the entry, so a spelling nobody typed must not be
    /// the one that lands in a durable document: `.//a.txt` used to normalize to
    /// `/a.txt` and be recorded as an absolute path the developer never wrote.
    #[test]
    fn repeated_separators_collapse_so_no_invented_path_is_echoed() {
        assert_eq!(
            IncludeList::parse(".//a.txt\nsrc//lib//x.rs\n").roots(),
            ["a.txt", "src/lib/x.rs"],
        );

        let dir = tree(&[("a.txt", "hi")], Some(".//a.txt\n"));
        let text = doc(dir.path());
        assert!(
            !text.contains("/a.txt unsafe-path"),
            "the shard must not echo a path nobody typed: {text}"
        );
        assert_eq!(text, "rift-capture/1\nf a.txt\n hi\n");
    }

    #[test]
    fn parse_normalizes_comments_blanks_dedup_and_trailing_slashes() {
        // A directory written with and without its trailing `/` is one root, and
        // a repeated file is one entry. `src\` is NOT a third spelling of the
        // same root: it is a name Unix would resolve to a different file, so it
        // survives normalization and earns its own refusal.
        let list = IncludeList::parse("# a comment\n\n  b.txt  \nsrc/\nsrc\\\nb.txt\ncfg/\n");
        assert_eq!(
            list.roots(),
            ["b.txt", "cfg", "src", "src\\"],
            "trimmed, the portable separator stripped, deduplicated, sorted"
        );
        assert_eq!(hard_refusal("src\\"), Some(Refusal::UnsafePath));
    }

    /// The draw behind `captured_records_only_ever_name_accepted_paths`, named
    /// so the property and the counter that proves it non-vacuous cannot use
    /// different ones.
    ///
    /// Two thirds arbitrary spelling (the original alphabet, which is what
    /// makes the property a statement about *whatever the include file says*)
    /// and one third paths that name something real in [`doors`] — the two
    /// captured files, the two directories above them, the symlinks that pass
    /// the screen, and `.git/config`, which must not be captured. Without the
    /// second branch every entry is `missing` and the property's assertions
    /// never run, which is exactly what happened.
    fn converse_draw() -> proptest::strategy::BoxedStrategy<Vec<String>> {
        proptest::collection::vec(
            prop_oneof![
                2 => "[a-zA-Z.\\\\/]{1,12}",
                1 => proptest::sample::select(vec![
                    "real/a.txt", "real/sub/a.txt", "real", "real/", "real/sub",
                    "cfg", "out", "g", "in", "deeper",
                    "bs\\dir", "g\\h", ".git/config", "a.txt:s",
                ]).prop_map(str::to_owned),
            ],
            0..6,
        )
        .boxed()
    }

    /// The reachability half of the property above, as a test rather than as
    /// something a verifier has to go and measure. It drives the same strategy
    /// and counts what the assertions get to read; a future edit that empties
    /// the tree, narrows the alphabet or drops the real-path branch turns the
    /// property hollow again and fails **here**, loudly, instead of passing
    /// silently for twenty thousand draws.
    #[test]
    fn the_converse_property_sees_the_records_it_asserts_about() {
        use proptest::strategy::Strategy as _;
        use proptest::test_runner::TestRunner;

        let strategy = converse_draw();
        let mut runner = TestRunner::deterministic();
        let (mut files, mut links, mut refusals) = (0usize, 0usize, 0usize);
        for _ in 0..200 {
            let entries = strategy.new_tree(&mut runner).unwrap().current();
            let dir = tempfile::tempdir().unwrap();
            let work = doors(dir.path());
            std::fs::write(work.join(INCLUDE_FILE), entries.join("\n")).unwrap();
            let text = String::from_utf8(capture_document(&work).as_slice().to_vec()).unwrap();
            for line in text.lines() {
                match line.as_bytes().first() {
                    Some(b'f') => files += 1,
                    Some(b'l') => links += 1,
                    Some(b'x') => refusals += 1,
                    _ => {}
                }
            }
        }
        assert!(
            files > 0,
            "the `f` assertion never runs: {files}/{links}/{refusals}"
        );
        assert!(
            refusals > 0,
            "…and the arbitrary half of the draw stopped reaching refusals: \
             {files}/{links}/{refusals}"
        );
        // Symlinks exist only on the Unix legs (see `doors`), so only there can
        // the `l` assertion have anything to read.
        #[cfg(unix)]
        assert!(
            links > 0,
            "the `l` assertion never runs: {files}/{links}/{refusals}"
        );
    }

    /// The vocabulary
    /// `a_captured_files_parent_always_resolves_inside_the_work_tree` draws
    /// from, lifted out of the draw so the sweep below reads the same list.
    /// Ancestors: every door in [`doors`], real directories, and the segments
    /// Win32 resolves to traversal.
    const SUMMARY_ANCESTORS: [&str; 17] = [
        "cfg", "out", "in", "deeper", "g", "deep", "bs\\dir", "g\\h", "x\\y", "real", "sub", "..",
        ".", ".. ", ". ", "...", ".git",
    ];

    /// Leaves: what an escape would fetch, what an ordinary capture holds, and
    /// what would forge a record if the framing leaked.
    const SUMMARY_LEAVES: [&str; 11] = [
        "id_rsa",
        "secret.txt",
        "notes.md",
        "config",
        "a.txt",
        "sub",
        ".git",
        "..",
        "one\nf /etc/passwd",
        "bell\u{7}rung",
        "f spaced.txt",
    ];

    /// The reachability half of the summary search, in the shape
    /// `the_converse_property_sees_the_records_it_asserts_about` already uses
    /// next door — and it is here because the numbers that stood in for it were
    /// **not constants**. "525 `x` records, 177 keyed, 52 summaries, 0 both"
    /// was one run of a randomly seeded proptest; a verifier's run of the same
    /// tree gave 548/173/42. Only the last of the four is a property, so only
    /// the last is asserted as one, and the rest become a question a test
    /// answers rather than a figure a reader has to trust: does the vocabulary
    /// still reach both kinds of record the search reads?
    ///
    /// The sweep is exhaustive rather than sampled — every ancestor × every
    /// leaf × both joiners, in chunks well under [`MAX_ENTRIES`] so no chunk
    /// ends early — so a vocabulary edit that stops producing keyed records, or
    /// stops producing summaries, reddens here deterministically instead of
    /// leaving the property next door asserting over nothing.
    ///
    /// The `both` count is the same property the search asserts per record,
    /// repeated here for its *message*: a record that is keyed and
    /// summary-shaped is either the real defect or the false positive
    /// [`is_summary`] warns about, and the reader needs to be told which two
    /// things to look at.
    #[test]
    fn the_summary_search_sees_the_records_it_asserts_about() {
        let mut entries: Vec<String> = Vec::new();
        for ancestor in SUMMARY_ANCESTORS {
            for leaf in SUMMARY_LEAVES {
                for sep in ["/", "\\"] {
                    entries.push(format!("{ancestor}{sep}{leaf}"));
                }
            }
        }
        let (mut keyed, mut summaries, mut both) = (0usize, 0usize, 0usize);
        for chunk in entries.chunks(32) {
            let dir = tempfile::tempdir().unwrap();
            let work = doors(dir.path());
            std::fs::write(work.join(INCLUDE_FILE), chunk.join("\n")).unwrap();
            let text = String::from_utf8(capture_document(&work).as_slice().to_vec()).unwrap();
            for line in text.lines() {
                let Some(rest) = line.strip_prefix("x ") else {
                    continue;
                };
                let (path, token) = rest.rsplit_once(' ').unwrap_or((rest, ""));
                let is_keyed = KEYED_TOKENS.contains(&token);
                keyed += usize::from(is_keyed);
                summaries += usize::from(is_summary(path));
                both += usize::from(is_keyed && is_summary(path));
            }
        }
        assert!(
            keyed > 0,
            "the keyed half of the summary search reads nothing: no entry in \
             this vocabulary earns a keyed refusal any more ({keyed} keyed, \
             {summaries} summaries)"
        );
        assert!(
            summaries > 0,
            "the summary half of the search reads nothing: no entry in this \
             vocabulary makes a writer summarise a path any more ({keyed} \
             keyed, {summaries} summaries)"
        );
        assert_eq!(
            both, 0,
            "a record is both keyed and summary-shaped. Either a keyed exit \
             summarised its path — which \
             `a_keyed_record_always_names_its_path_literally` proves cannot \
             happen, so start there — or this vocabulary now holds a real name \
             that ends like a summary, which is the false positive \
             `is_summary` documents"
        );
    }

    proptest! {
        /// The `.git` exclusion, as a property over the whole path space: a
        /// segment spelled `.git` in ANY case, at ANY depth, under either
        /// separator, with ANY run of trailing dots and spaces — which Win32
        /// strips during path resolution, so `.git.` and `.git ` open the same
        /// directory there. This is the rule an include file must not be able to
        /// argue with, on any platform this ships to.
        #[test]
        fn any_path_with_a_git_segment_is_refused(
            before in proptest::collection::vec("[a-z]{1,4}", 0..4),
            after in proptest::collection::vec("[a-z]{1,4}", 0..4),
            upper in proptest::collection::vec(any::<bool>(), 4..5),
            trailing in "[. ]{0,3}",
            sep in any::<bool>(),
        ) {
            let mut git: String = ".git".chars().zip(upper.iter()).map(|(c, up)| {
                if *up { c.to_ascii_uppercase() } else { c }
            }).collect();
            git.push_str(&trailing);
            let mut segs = before.clone();
            segs.push(git);
            segs.extend(after.clone());
            let path = segs.join(if sep { "/" } else { "\\" });
            prop_assert_eq!(hard_refusal(&path), Some(Refusal::GitPath));
        }

        /// The converse, so the trimming above cannot swallow ordinary names: a
        /// segment that merely STARTS with `.git` is not `.git`, whatever it
        /// trails, and stays capturable.
        #[test]
        fn a_name_that_only_starts_with_git_is_not_the_git_dir(
            tail in "[a-z]{1,6}",
            trailing in "[. ]{0,3}",
        ) {
            let path = format!(".git{tail}{trailing}");
            prop_assert_eq!(hard_refusal(&path), None);
        }

        /// The class three point patches did not close, stated over the function
        /// that now decides it and searched with the spellings that broke it.
        ///
        /// Why this exists beside the end-to-end property below, restated after
        /// measurement because the first answer was wrong. That one passed with
        /// the defect present, and the reason recorded here was "its
        /// vocabulary held no `\`". Widening the vocabulary did not fix it:
        /// with its guard restored and the `\` screen in place, the end-to-end
        /// property is green 10 runs out of 10 even with `\` in its sample,
        /// because `hard_refusal` refuses those entries before `resolve` is
        /// consulted. **A screen upstream of a defect hides it from every
        /// property downstream of the screen, whatever they draw.**
        ///
        /// This one is downstream of nothing. It targets [`resolve`] directly
        /// and consults no screen, so nothing upstream can mask it: if
        /// `resolve` hands back a path, the directory that path will be read
        /// from is inside the work tree. In that same screen-in-place tree it
        /// reddens **7 of 10 runs** at 256 cases — on a uniform draw, which is
        /// the point: aiming the property at the deciding function bought more
        /// than shaping the sample did. Delete the `hard_refusal` `\` rule and
        /// this still holds; delete the ancestor check and it does not.
        ///
        /// Its vocabulary names the spellings three rounds cost: `\` (an
        /// ordinary filename byte on Unix, a separator on Windows), `.. ` and
        /// `. ` (which Win32 resolves to `..` and `.`), every ancestor door in
        /// the tree, and both separators as the joiner.
        #[test]
        fn resolve_only_ever_lands_inside_the_work_tree(
            segments in proptest::collection::vec(
                proptest::sample::select(vec![
                    "bs\\dir", "g\\h", "x\\y", "deep", "cfg", "g", "out", "in",
                    "deeper", "real", "sub", "id_rsa", "secret.txt", "notes.md",
                    "a.txt", "config", "..", ".. ", ".", ". ", "...", "",
                    ".git", ".GIT", ".git ", ".worktreeinclude",
                    // A newline and a bare control character are ordinary
                    // filename bytes on Unix, so a path property that never
                    // contains one is a single-line property in disguise.
                    // `resolve` sees entries before the screen does, so these
                    // reach it exactly as spelled.
                    "one\nf /etc/passwd", "bell\u{7}rung",
                ]),
                1..4,
            ),
            sep in any::<bool>(),
        ) {
            let dir = tempfile::tempdir().unwrap();
            let work = doors(dir.path());
            let root = std::fs::canonicalize(&work).unwrap();

            let entry = segments.join(if sep { "/" } else { "\\" });
            let Ok(full) = resolve(&work, &entry) else { return Ok(()) };

            prop_assert!(
                full.starts_with(&work),
                "`{}` resolved to {}, which is not under the work tree even \
                 before anything is followed", entry, full.display(),
            );
            // The parent is where the bytes come from: `lstat` leaves the final
            // component alone (that is what makes a link a link record), so the
            // question an ancestor symlink answers differently is where the
            // directory holding the file actually is.
            if let Some(parent) = full.parent() {
                if let Ok(resolved) = std::fs::canonicalize(parent) {
                    prop_assert!(
                        resolved.starts_with(&root),
                        "`{}` would be read from {} — outside {}",
                        entry, resolved.display(), root.display(),
                    );
                }
            }
        }

        /// Whatever the include file says, a captured path is always one that
        /// `hard_refusal` accepts: the document's `f`/`l` records can only name
        /// paths that passed the gate above.
        ///
        /// **This property shipped hollow, and how it was hollow is the part
        /// worth keeping.** It ran over an EMPTY tempdir, so every drawn entry
        /// named nothing and earned `missing`: over 20 000 draws it produced
        /// 16 698 non-empty documents, 50 056 `x` records and **0 `f` and 0
        /// `l`** — both `prop_assert_eq!` bodies below executed **zero times**.
        /// Its alphabet could not even spell the control file it needed. A
        /// property whose vocabulary and shape are both fine can still assert
        /// nothing, because it never reached a state its assertions read.
        ///
        /// It now runs over [`doors`] — real files, real directories, real
        /// symlinks — and [`converse_draw`] mixes arbitrary spellings with
        /// paths that actually name something in that tree, so the records
        /// exist to be examined. Reachability is not left to a reader to
        /// notice: `the_converse_property_sees_the_records_it_asserts_about`
        /// drives the same strategy and fails if either record kind is absent.
        #[test]
        fn captured_records_only_ever_name_accepted_paths(
            entries in converse_draw(),
        ) {
            let dir = tempfile::tempdir().unwrap();
            let work = doors(dir.path());
            std::fs::write(work.join(INCLUDE_FILE), entries.join("\n")).unwrap();
            let text = String::from_utf8(
                capture_document(&work).as_slice().to_vec(),
            ).unwrap();
            for line in text.lines() {
                if let Some(rest) = line.strip_prefix("f ") {
                    prop_assert_eq!(hard_refusal(rest), None, "f {}", rest);
                }
                if let Some(rest) = line.strip_prefix("l ") {
                    let path = rest.rsplit_once(" -> ").map_or(rest, |(p, _)| p);
                    prop_assert_eq!(hard_refusal(path), None, "l {}", rest);
                }
            }
        }

        /// The property the spelling rules cannot express, over a tree built to
        /// break it: every captured file's parent, RESOLVED, is inside the work
        /// tree. `hard_refusal` says what a path may be spelled; this says where
        /// the bytes actually came from, which is the question an ancestor
        /// symlink answers differently from the name it was captured under.
        /// The vocabulary carries a NEWLINE and a bare control character on
        /// purpose (a maintainer note, 2026-08-07): the document is line-framed,
        /// `f <path>` is a header, and a property over paths that never contains
        /// `\n` is a single-line property wearing an arbitrary-input costume.
        /// Both are handled at different layers — the include file is read by
        /// lines, so a newline SPLITS an entry and can never sit inside one, and
        /// a surviving control character earns a refusal with the echo
        /// summarised rather than printed (`unsafe-path`, or `git-path` when
        /// the vocabulary also drew a `.git` segment — the summary belongs to no
        /// one token, see [`Refusal::UnsafePath`]) — so the second assertion below states
        /// the framing itself: every line of the document is a record or a
        /// content line, whatever the include file said.
        ///
        /// It also carries the backslash-component spellings — `\` in a link's own name, `.. `,
        /// `. `, `...` — and the tree now holds a `.git` with a credential in
        /// it, so "no bytes from outside" and "no bytes from the substrate" are
        /// searched together.
        ///
        /// The generator is SHAPED, not uniform — drawn as (ancestors, leaf,
        /// joiner) so the *shape* of the escape class is the thing searched.
        /// What that bought is a **retraction**: this comment used to claim a
        /// uniform draw found the restored escape in "0 of 256" cases. It
        /// does not. Re-measured with that guard restored byte-for-byte, the
        /// `\` screen removed, and the leak proven live first — vocabulary held
        /// constant across all three shapes so only the shape varies, 20 runs ×
        /// 256 cases and then 20 000 single draws per shape:
        ///
        /// | draw | runs red | per case |
        /// |---|---|---|
        /// | shaped, as shipped | 20/20 | 112/20 000 = 1 in 179 |
        /// | uniform over the same 25 tokens | 4/20 | 32/20 000 = 1 in 625 |
        /// | pre-reshape (flat vec, chunked) | 7/20 | 16/20 000 = 1 in 1250 |
        ///
        /// So reshaping sharpened the search ~3.5× per case against a uniform
        /// draw. Real, and worth keeping — but a 3.5× sharpening, not the
        /// difference between a blind property and a sighted one.
        ///
        /// **What does produce a zero is not the generator at all.** With the
        /// same guard restored and the `\` screen LEFT IN PLACE, detection is
        /// 0/10 runs for *every* shape, this one included: the screen refuses
        /// every `\`-bearing entry before `capture_document` ever reaches
        /// `resolve`, so an end-to-end property cannot see the defect whatever
        /// it draws. The belt masks the braces. The original "0 of 256" was
        /// almost certainly measured with the screen in, and attributed to
        /// shape an effect produced by the screen.
        ///
        /// That is precisely why the property ABOVE this one exists: it targets
        /// `resolve` directly and consults no screen, and in the same
        /// screen-in-place tree it reddens 7/10 runs on a *uniform* draw. What
        /// makes this class findable by search is the layer a property is aimed
        /// at, not the shape of its sample.
        #[test]
        fn a_captured_files_parent_always_resolves_inside_the_work_tree(
            entries in proptest::collection::vec(
                (
                    // Ancestors: every door in the tree, real directories, and
                    // the segments Win32 resolves to traversal.
                    proptest::collection::vec(
                        proptest::sample::select(SUMMARY_ANCESTORS.to_vec()),
                        0..3,
                    ),
                    // Leaves: what an escape would fetch, what an ordinary
                    // capture holds, and what would forge a record if the
                    // framing leaked.
                    proptest::sample::select(SUMMARY_LEAVES.to_vec()),
                    any::<bool>(),
                ),
                1..4,
            ),
        ) {
            let dir = tempfile::tempdir().unwrap();
            let work = doors(dir.path());

            let list: Vec<String> = entries
                .iter()
                .map(|(ancestors, leaf, slash)| {
                    let sep = if *slash { "/" } else { "\\" };
                    let mut segments = ancestors.clone();
                    segments.push(leaf);
                    segments.join(sep)
                })
                .collect();
            std::fs::write(work.join(INCLUDE_FILE), list.join("\n")).unwrap();

            let text = String::from_utf8(
                capture_document(&work).as_slice().to_vec(),
            ).unwrap();
            for forbidden in [
                "PRIVATE-KEY-BYTES-XYZ", "MY-PRIVATE-JOURNAL-ENTRY", "GIT-TOKEN-abc123",
            ] {
                prop_assert!(
                    !text.contains(forbidden),
                    "captured bytes from outside the tree (or from the \
                     substrate): {}", text,
                );
            }

            let root = std::fs::canonicalize(&work).unwrap();
            for (n, line) in text.lines().enumerate() {
                // The framing, stated over the whole document: after the header
                // every line is a record or a space-prefixed content line, so
                // nothing an include file names can forge a record of its own.
                prop_assert!(
                    n == 0
                        || line.starts_with("f ")
                        || line.starts_with("l ")
                        || line.starts_with("x ")
                        || line.starts_with(' '),
                    "line {} is neither a record nor content: {:?}", n, line,
                );

                // The summary property, as a search rather than as three
                // trees: no record under a keyed token ever stands in for its
                // path. The reader's rule for an `x` record is the last
                // whitespace field, which is what `refuse` writes.
                // (`a_keyed_record_always_names_its_path_literally` proves the
                // three premises; this is what would find a fourth.)
                if let Some(rest) = line.strip_prefix("x ") {
                    let (path, token) = rest.rsplit_once(' ').unwrap_or((rest, ""));
                    prop_assert!(
                        !(KEYED_TOKENS.contains(&token) && is_summary(path)),
                        "a keyed record summarised its path, so the token no \
                         longer tells a reader whether the path field names \
                         anything: {:?}", line,
                    );
                }

                let Some(rel) = line.strip_prefix("f ") else { continue };
                let parent = work.join(rel).parent().unwrap().to_owned();
                let resolved = std::fs::canonicalize(&parent).unwrap();
                prop_assert!(
                    resolved.starts_with(&root),
                    "`f {}` was read from {} — outside {}",
                    rel, resolved.display(), root.display(),
                );
            }
        }
    }
}
