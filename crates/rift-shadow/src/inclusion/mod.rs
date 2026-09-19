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
    ///   (`a_path_whose_lstat_fails_is_keyed_by_its_refusal`.)
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
    /// (`a_keyed_record_always_names_its_path_literally` and the tests beside
    /// it prove the three premises —
    /// `every_control_character_is_refused_before_resolve_runs` exhaustively
    /// over the `char` space for the second;
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
    /// (`an_unreadable_directory_named_twice_is_one_record` is the
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
    /// — which is how this was missed twice.
    /// (`a_path_whose_lstat_fails_is_keyed_by_its_refusal`.)
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
mod tests;
