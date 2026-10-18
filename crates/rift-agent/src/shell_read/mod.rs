//! The one shell grammar Rift recognises — a vendor's own list of commands
//! that count as having read a file, and the three ways an agent is measured
//! composing them.
//!
//! An agent that runs `cat src/lib.rs` in a terminal tool has read that file
//! as surely as one that called a read tool, and the tokens are spent either
//! way. The difficulty is that a shell command line is not a tool call: it is
//! a small program, and deciding what an arbitrary one did is exactly the
//! guess this codebase refuses to make.
//!
//! What makes the narrow case safe is that it is not Rift's invention.
//! Claude Code publishes the list of commands it *itself* treats as
//! satisfying "read the file before editing it" — `cat`, `nl`, `bat`,
//! `batcat`, `head`, `tail`, `sed -n 'X,Yp'`, `grep`, `egrep`, `fgrep`, `rg`
//! on a single file, with no pipes or redirects — and that list is pinned in
//! this tree beside the adapter that reads it. A listing grammar of the same
//! shape (`ls`, `find … -name`, `rg --files`) rounds it out. Recognising an
//! exact match from a published list is not a parse of shell; anything the
//! list does not spell is left alone — with one exception, a line pick
//! written in `awk` (`awk 'NR>=A && NR<=B'`), which prints exactly what the
//! listed `sed` range does and is read as that range.
//!
//! **The published list is the vocabulary, not the whole sentence.** Measured
//! on this machine's own transcripts, an agent almost never writes one of
//! these commands by itself: of 864 shell reads in eight sessions, 35 were a
//! bare simple command, and most of the other 829 were a `cd` into a
//! directory, a trailing `| head -N`, or several reads joined by `&&`. Every
//! one of those was a read of this repository that the journal never saw, and
//! a read the journal never saw can never be declined and never counted. So
//! three *composition* rules sit above the vocabulary — an optional leading
//! `cd <dir> &&` (or `cd <dir>;`), a trailing `| head -N` or `| tail -N` on
//! a read (or the line pick `| sed -n 'A,Bp'`, the same arithmetic with both
//! ends stated), and a chain of reads joined by `&&` or `;`. Each one is a
//! shape whose delivered lines still follow from the line itself; a member
//! the vocabulary does not spell still takes the whole command out of the
//! grammar, exactly as before.
//!
//! **A chain labels itself.** Measured again once those three landed, the
//! largest shape still going unseen was a chain whose reads are separated by
//! `echo` lines the agent writes to caption its own output — `sed`, `echo`,
//! `sed`; `cd`, `echo`, `sed`; a bare `echo` for a blank line — about ninety
//! commands, enough to roughly double what the grammar sees. An `echo` or a
//! `printf` over plain operands opens no file under any argument, so such a
//! member is a **label**: it contributes no read and it disqualifies nothing,
//! and a chain of reads and labels is decided on its reads alone. A line that
//! is labels and nothing else is not a read at all.
//!
//! **Then the line is parsed and composed.** The shapes stopped paying once
//! the census showed fifty of them refused with none above an eighth of the
//! tokens, so the composition now lives in `tree.rs`: the line is parsed —
//! with this module's own lexer, no shell parser — into a sequence of
//! pipelines, and each node's reads are composed over it. A read piped into
//! a *filter* (`filter.rs`: a `grep`, `cut`, `sort` or `uniq` that opens
//! nothing of its own) is counted as a read of its file and spans
//! [`ShellSpan::Filtered`], so it is never coverage.
//!
//! Three rules hold everywhere below.
//!
//! 1. **A non-match is never touched.** Every ambiguity — an option the list
//!    does not name, a quote the grammar does not pin, a pipe into anything
//!    but a narrowing command or a filter, a member that is not itself a read, a
//!    character the shell would act on — ends in [`None`]. The cost of a
//!    missed recognition is a saving; the cost of a wrong one is a decision
//!    taken about content an agent never received.
//! 2. **A recognised command is not always a range.** `grep` delivers the
//!    lines that matched — with or without the flags that only choose and
//!    number them — which nothing here can know; a bare `head` or
//!    `tail` relies on a default window no artifact in this tree pins. All of
//!    them are recognised — so none is ever mistaken for a search that could
//!    be answered from an index — and none yields coverage, because the honest
//!    interval for them is one this type cannot express. A *counted* `tail` is
//!    the one that moved: its range is exactly `N` lines ending at the last
//!    one, so it travels as [`ReadSpan::LastLines`] and the step that already
//!    holds the bytes turns the count into line numbers.
//! 3. **A content search is never a listing.** `grep`, `egrep`, `fgrep` and a
//!    pattern-carrying `rg` read *inside* files; `ls`, `find -name` and `rg
//!    --files` read names. The first group can never become the second, which
//!    is what keeps a name index from ever answering a question about
//!    content.

mod filter;
mod tree;

use crate::{HiKind, ReadChannel, ReadRequest, ReadSpan, SearchKind, SearchRequest};

// The entry points moved with the composition they run; callers keep the
// paths they always named.
pub use tree::{leading_cd, recognise};

/// The longest command line this recogniser will even look at.
///
/// Nothing in the grammar needs more: a `cd`, a handful of members, and each
/// member a command word, at most two options and its paths. The cap is here
/// so that a payload built to be expensive is rejected by its length before a
/// single character of it is classified.
pub const MAX_COMMAND_BYTES: usize = 4096;

/// The most reads one command line may be recognised as performing.
///
/// A chain is a convenience an agent reaches for, not a batch protocol, and
/// the ones a person writes are short. The cap bounds two things at once —
/// the work this recogniser does on a hostile line, and the length of the
/// sentence a decline of such a line would have to render, since a frame that
/// lists a member per line is only readable while the list is short.
pub const MAX_CHAIN_READS: usize = 16;

/// Where in a file a recognised command's output came from.
///
/// The variants are deliberately not all convertible into a line range from
/// the command line alone. Some describe output whose position depends on the
/// file itself — one of them exactly, which the resolution step can therefore
/// settle, and the rest not at all — and saying which is which is the whole
/// reason they exist as separate cases rather than collapsing into a
/// plausible interval.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShellSpan {
    /// The whole file byte-for-byte, from line one: plain `cat`.
    WholeFile,
    /// The whole file transformed for display: `nl`, `bat`, `batcat`.
    DecoratedWholeFile,
    /// A count of lines from the top: `head`, with the count when `-n` gave
    /// one.
    FromTop {
        /// The line count `-n` named, or [`None`] when the command relied on
        /// the tool's own default — a number no artifact in this tree pins.
        limit: Option<u32>,
    },
    /// A count of lines from the bottom: `tail`. Which lines those are
    /// depends on how many the file has, so a counted one travels as
    /// [`ReadSpan::LastLines`] and is settled where the bytes are; an
    /// uncounted one rests on a default window no artifact pins and is
    /// settled nowhere.
    FromBottom {
        /// The line count `-n` named, or [`None`] for the tool's default.
        limit: Option<u32>,
    },
    /// An explicit run of lines: `sed -n 'A,Bp'`, or the one line `'Ap'`.
    Lines {
        /// The first line printed.
        lo: u32,
        /// The last line printed.
        hi: u32,
    },
    /// Only the lines that matched a pattern: the `grep` family, and `rg`
    /// with a pattern. **Not a derivable range**, and never a name search.
    Matching,
    /// One run of a `sed -n` script that prints several, `'A,Bp;Cp'`.
    ///
    /// The lines are exactly named, and still not credited: the runs reach
    /// the window as one output, which can pass the terminal threshold — and
    /// be written to a file the window only gets a pointer to — while each
    /// run's own slice stays under it, and the gate weighs each read alone.
    OneOfSeveralRuns {
        /// The first line of the run.
        lo: u32,
        /// The last line of the run.
        hi: u32,
    },
    /// Lines of the file that reached the agent only through a later
    /// pipeline stage that chose or reshaped them — a `grep`, a `cut`, a
    /// `sort`, or a narrowing over several reads joined together. The shell
    /// opened the file, so the read is counted; which lines arrived is
    /// nothing the command line names, so it is never coverage.
    Filtered,
}

/// A trailing `| head -N`, `| tail -N` or `| sed -n 'A,Bp'` that narrows
/// what a read delivered.
///
/// The three commands are in the grammar as *readers* already, and in this
/// position they are doing the same arithmetic on the run of lines the member
/// before them produced rather than on a file. That is why they are the only
/// pipe targets that keep a span exact: a filter (`grep`, `cut`, `sort`,
/// `uniq`) changes which lines arrive, so a read behind one is
/// [`ShellSpan::Filtered`], and every other target — a `wc`, a pager, a `sed`
/// that edits — hands the window something no read names, so a member piped
/// into one is not a read this module will name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Narrowing {
    /// `| head -N`: the first `N` lines of the run that was produced.
    First(u32),
    /// `| tail -N`: the last `N` lines of it.
    Last(u32),
    /// `| sed -n 'A,Bp'`, or `'Ap'` for one line: lines `A` to `B` of it,
    /// counted from its own first line.
    Pick {
        /// The first line of the run kept.
        lo: u32,
        /// The last line of the run kept.
        hi: u32,
    },
}

/// A shell command line that read one file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShellRead {
    /// The path exactly as the command line spelled it, under the directory
    /// a leading `cd` named — relative to the agent's working directory when
    /// it is still relative, which is the caller's to resolve, since this
    /// module has no filesystem authority.
    pub path: String,
    /// Which part of the file the command printed.
    pub span: ShellSpan,
    /// The trailing pipe that cut that part down, where there was one.
    pub narrowing: Option<Narrowing>,
}

impl ShellRead {
    /// The part of the file this read delivered, as the span the rest of the
    /// system speaks in — [`ReadSpan::Unknown`] where it delivered lines
    /// nobody here can name.
    ///
    /// The table is the arithmetic of the two commands in the narrowing
    /// position applied to the run the member produced, and every cell that
    /// cannot be written as one interval of the file is `Unknown` rather than
    /// the nearest interval that can. The one shape worth naming: the *first*
    /// `N` lines of the *last* `M` start at a line only the file's length
    /// names, and no span here says "a count in from the end", so that cell is
    /// unknown even though both halves of it are pinned.
    fn delivered(&self) -> ReadSpan {
        use Narrowing::{First, Last, Pick};
        // A count that reached here is at least one: the grammar refuses a
        // zero, and a command that prints nothing has read nothing.
        let back = |n: u32| n.saturating_sub(1);
        match (self.span, self.narrowing) {
            // Plain `cat` is byte-exact and reaches EOF while it stays below
            // the separately pinned shell-delivery threshold. The gate owns
            // that size check because only it has the file bytes.
            (ShellSpan::WholeFile, None) => {
                ReadSpan::lines(1, HiKind::DefaultWindow(HiKind::END_OF_FILE))
            }
            (ShellSpan::WholeFile, Some(First(n))) => ReadSpan::lines(1, HiKind::Explicit(n)),
            (ShellSpan::WholeFile, Some(Last(n))) => ReadSpan::LastLines { count: n },
            (ShellSpan::FromTop { limit: Some(m) }, None) => {
                ReadSpan::lines(1, HiKind::Explicit(m))
            }
            (ShellSpan::FromTop { limit: Some(m) }, Some(First(n))) => {
                ReadSpan::lines(1, HiKind::Explicit(m.min(n)))
            }
            // The last `n` of the first `m` is a plain interval: both ends are
            // numbers the command line states. Where the file turns out to be
            // shorter than `m` the interval simply is not one of that file,
            // and the resolution step drops it — a lost saving, never a claim.
            (ShellSpan::FromTop { limit: Some(m) }, Some(Last(n))) => {
                ReadSpan::lines(m.saturating_sub(back(n)).max(1), HiKind::Explicit(m))
            }
            (ShellSpan::Lines { lo, hi }, None) => ReadSpan::lines(lo, HiKind::Explicit(hi)),
            (ShellSpan::Lines { lo, hi }, Some(First(n))) => {
                ReadSpan::lines(lo, HiKind::Explicit(hi.min(lo.saturating_add(back(n)))))
            }
            (ShellSpan::Lines { lo, hi }, Some(Last(n))) => {
                ReadSpan::lines(lo.max(hi.saturating_sub(back(n))), HiKind::Explicit(hi))
            }
            // A pick is the same count from the run's own first line, so it
            // has a reading wherever the run's first line is a number.
            (ShellSpan::WholeFile, Some(Pick { lo, hi })) => picked((1, u32::MAX), (lo, hi)),
            (ShellSpan::FromTop { limit: Some(m) }, Some(Pick { lo, hi })) => {
                picked((1, m), (lo, hi))
            }
            (
                ShellSpan::Lines {
                    lo: first,
                    hi: last,
                },
                Some(Pick { lo, hi }),
            ) => picked((first, last), (lo, hi)),
            (ShellSpan::FromBottom { limit: Some(m) }, None) => ReadSpan::LastLines { count: m },
            (ShellSpan::FromBottom { limit: Some(m) }, Some(Last(n))) => {
                ReadSpan::LastLines { count: m.min(n) }
            }
            // Decoration changes the byte count; a `head` or `tail` relying on
            // its own default states no count; the `grep` family prints what
            // matched; a run among several is part of one output whose size
            // no member answers for. None of those is an interval of this
            // file a record may be written from.
            (ShellSpan::FromBottom { limit: Some(_) }, Some(First(_) | Pick { .. }))
            | (
                ShellSpan::DecoratedWholeFile
                | ShellSpan::FromTop { limit: None }
                | ShellSpan::FromBottom { limit: None }
                | ShellSpan::Matching
                | ShellSpan::OneOfSeveralRuns { .. }
                | ShellSpan::Filtered,
                _,
            ) => ReadSpan::Unknown,
        }
    }

    /// This read as the request the rest of the system speaks in, whether or
    /// not its lines are derivable.
    ///
    /// The unnameable cases keep their place here — as [`ReadSpan::Unknown`],
    /// which records nothing and can satisfy no decline — because a caller
    /// deciding about a *whole command line* has to be able to see the member
    /// it cannot answer for. Dropping it would leave a chain looking wholly
    /// held when one of its reads was never accounted for at all.
    #[must_use]
    pub fn request(&self) -> ReadRequest {
        ReadRequest {
            path: self.path.clone(),
            span: self.delivered(),
            // Terminal output, whatever the command asked for: past a measured
            // size this client writes it to a file and hands the model a
            // pointer, and the record has to answer for the pointer.
            channel: ReadChannel::Shell,
        }
    }

    /// The coverage record this read supports, or [`None`] when it supports
    /// none.
    ///
    /// A bare `head` or `tail`, a decorated print and the `grep` family return
    /// [`None`] on purpose. A record is a claim that a context window was
    /// handed particular lines, and the only claim available for those is
    /// "some lines, position unknown" — which, written down as the single line
    /// the file starts on, would say the agent holds line one when it may well
    /// have seen everything but. The recognition is still worth having: it is
    /// what stops the same call from being read as a name search.
    #[must_use]
    pub fn read_request(&self) -> Option<ReadRequest> {
        let request = self.request();
        (request.span != ReadSpan::Unknown).then_some(request)
    }
}

impl ShellRead {
    /// The file a recognised **content search** named, or [`None`] for
    /// anything else.
    ///
    /// A `grep` over one named file is the one recognised command that prints
    /// *matching lines* rather than a run of them: it supports no coverage
    /// (see [`ShellRead::read_request`]), and it is also not a re-read — the
    /// window is handed the matches, not the file. So it is exactly the shape
    /// an accounting of reads that did not happen needs to see, and the path
    /// it names is pinned by the grammar rather than read off an argument no
    /// artifact claims.
    #[must_use]
    pub fn searched_file(&self) -> Option<&str> {
        matches!(self.span, ShellSpan::Matching).then_some(self.path.as_str())
    }
}

/// A shell command line that listed names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShellListing {
    /// The directory, exactly as the command line spelled it, under the one a
    /// leading `cd` named.
    pub dir: String,
    /// The name pattern, where the command took one.
    pub pattern: Option<String>,
}

impl ShellListing {
    /// This listing as the search request the rest of the system speaks in.
    #[must_use]
    pub fn search_request(&self) -> SearchRequest {
        SearchRequest {
            kind: SearchKind::Listing {
                dir: self.dir.clone(),
                pattern: self.pattern.clone(),
            },
        }
    }
}

/// What a recognised command line was.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShellCall {
    /// It read files: one entry per read the line performs, in the order it
    /// performs them, and never empty. A chain of members contributes one
    /// each; a `cat` over several paths contributes one per path.
    Read(Vec<ShellRead>),
    /// It listed the names in one directory. A listing is a whole command
    /// line on its own: a chain is a chain of *reads*, so a name search is
    /// never one member of a decision about content.
    Listing(ShellListing),
}

impl ShellCall {
    /// Every read this call performs, in order — including the ones whose
    /// lines are not derivable, which carry [`ReadSpan::Unknown`].
    ///
    /// This is the answer a caller deciding about the whole command line
    /// wants, and the unnameable members are in it for the reason
    /// [`ShellRead::request`] gives: a chain is declined only when every one
    /// of its parts could be, and a part that was silently dropped would look
    /// like no part at all.
    #[must_use]
    pub fn read_requests(&self) -> Vec<ReadRequest> {
        match self {
            Self::Read(reads) => reads.iter().map(ShellRead::request).collect(),
            Self::Listing(_) => Vec::new(),
        }
    }

    /// The coverage record this call supports, or [`None`] — a listing never
    /// supports one, and neither do the read forms whose lines are not
    /// derivable.
    ///
    /// The **first** such read, which for the one-command lines that are all
    /// this grammar used to spell is the only one. It is what a caller with
    /// room for a single fact about a call gets; a caller that can hold every
    /// read asks [`ShellCall::read_requests`] instead.
    #[must_use]
    pub fn read_request(&self) -> Option<ReadRequest> {
        match self {
            Self::Read(reads) => reads.iter().find_map(ShellRead::read_request),
            Self::Listing(_) => None,
        }
    }

    /// The search request this call is, or [`None`].
    ///
    /// A read is never one. That asymmetry is the point: a `grep` recognised
    /// here answers "no" to the question "may this be served from a list of
    /// names", where an unrecognised command line answers nothing at all.
    #[must_use]
    pub fn search_request(&self) -> Option<SearchRequest> {
        match self {
            Self::Listing(listing) => Some(listing.search_request()),
            Self::Read(_) => None,
        }
    }

    /// The file a recognised content search named — see
    /// [`ShellRead::searched_file`].
    ///
    /// Only a line that is *nothing but* that search answers. The accounting
    /// this feeds books a read that did not happen, and a search standing
    /// beside other reads on one line is a line whose cost is already being
    /// counted somewhere else.
    #[must_use]
    pub fn searched_file(&self) -> Option<&str> {
        match self {
            Self::Read(reads) => match reads.as_slice() {
                [read] => read.searched_file(),
                _ => None,
            },
            Self::Listing(_) => None,
        }
    }
}

/// One word of a command line, and whether quotes produced it.
///
/// The text is the word the shell passes, quotes taken off and escapes
/// resolved; that a quote produced it is kept because the positions differ
/// in what they admit. A pattern, a `sed` script and a `cd` take any quoted
/// word the walk below resolves; a path takes one only when it spells a file
/// rather than a pattern; and every other position — a command word, an
/// option, a count — takes none.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Word {
    text: String,
    quoted: bool,
    /// Whether a backslash stood inside double quotes, kept as written or
    /// escaping the character after it. The shell's word is exact either
    /// way, but a path spelled with one is a path an agent almost never
    /// means, and a label's command may read the text as an escape of its
    /// own, so both positions refuse it.
    backslash: bool,
}

/// Whether a character may appear in an unquoted word.
///
/// The allowed set is small and positive: letters, digits, and the punctuation
/// that occurs in paths and in a `sed` line range. Everything else is
/// rejected, which is how the metacharacters get excluded without a list of
/// them that could be missing one — `|`, `;`, `&`, `>`, `<`, `$`, a backtick,
/// a backslash, `(`, `)`, `{`, `}`, `*`, `?`, `[`, `]`, `~`, `!`, `#` and the
/// quote characters themselves are all simply not in it.
///
/// The three separators the grammar now composes with are no exception: they
/// are taken off the line by [`split_on`] *before* a word is ever built, so a
/// `|` that reaches this function is a `|` in a position the grammar has no
/// meaning for, and it is refused exactly as it always was.
///
/// Characters above ASCII are allowed: a path may be spelled in any script,
/// and no character the shell acts on lives up there. The C1 control codes
/// (U+0080 to U+009F) are the exception: a terminal acts on them, nobody
/// spells a name with one, and a quoted word already refuses every control
/// character, so a bare word does too.
///
/// The `~` is excluded here and admitted only by [`inert_tilde`], which is
/// the one position where it is a letter rather than a home directory.
const fn bare_char(c: char) -> bool {
    c.is_ascii_alphanumeric()
        || matches!(c, '_' | '.' | '/' | '-' | '+' | '=' | ',' | ':' | '@' | '%')
        || (!c.is_ascii() && !matches!(c, '\u{80}'..='\u{9f}'))
}

/// Whether a `~` continuing the unquoted word `word` so far is a literal
/// character of it.
///
/// A shell expands a tilde only at the start of a word, or after the `=` or
/// a `:` of an assignment; anywhere else inside a word it is the letter a
/// Windows short name such as `RUNNER~1` is spelled with, which is the case
/// this admits. A word not yet started, and one whose last character is an
/// `=` or a `:`, are refused rather than told apart from an assignment —
/// a missed recognition costs a saving, a wrong one a decision.
fn inert_tilde(word: Option<&Word>) -> bool {
    word.and_then(|word| word.text.chars().last())
        .is_some_and(|last| !matches!(last, '=' | ':'))
}

/// Split a command line into words, or refuse it.
///
/// Refusal is the common case and is meant to be: anything with a character
/// the shell would act on, a quote that does not wrap a whole word, an
/// unterminated quote, a line break, or a double-quoted word whose text the
/// shell would compute rather than pass (see [`quoted_word`]) is not a
/// command line this module will classify.
fn words(command: &str) -> Option<Vec<Word>> {
    if command.len() > MAX_COMMAND_BYTES {
        return None;
    }
    let mut out: Vec<Word> = Vec::new();
    let mut current: Option<Word> = None;
    let mut chars = command.chars();
    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' => {
                if let Some(word) = current.take() {
                    out.push(word);
                }
            }
            '\'' | '"' => {
                // A quote opens a word and closes it: `a'b'` and `'a'b` are
                // shapes the grammar does not pin, so they are not read.
                if current.is_some() {
                    return None;
                }
                out.push(quoted_word(c, &mut chars)?);
                // The closing quote must end the word.
                if !matches!(chars.clone().next(), None | Some(' ' | '\t')) {
                    return None;
                }
            }
            _ if bare_char(c) || (c == '~' && inert_tilde(current.as_ref())) => {
                match current.as_mut() {
                    Some(word) => word.text.push(c),
                    None => {
                        current = Some(Word {
                            text: c.to_string(),
                            quoted: false,
                            backslash: false,
                        });
                    }
                }
            }
            _ => return None,
        }
    }
    if let Some(word) = current.take() {
        out.push(word);
    }
    Some(out)
}

/// The rest of a quoted word, its opening `mark` already read, as the word
/// the shell passes — or [`None`] when the shell would compute it instead.
///
/// Inside single quotes every character is itself. Inside double quotes the
/// shell still acts on `$` and a backtick, and a backslash escapes `$`, a
/// backtick, `"`, `\` and a newline; before any other character it is kept
/// as written. So a double-quoted word with no `$` and no backtick is its
/// text with each escaped `"` and `\` taken as that one character and every
/// other backslash kept — a resolution as fixed as the quote itself, which
/// is what lets an agent's `grep -n "\"decision\""` in. The other three
/// escapes stay refused: an escaped `$` or backtick is a spelling an agent's
/// reads almost never use, and an escaped newline joins two lines. A `!` is
/// refused as well: an interactive shell expands it even inside double
/// quotes, and this module cannot tell which shell it is. A control character
/// ends the recognition in either kind of quote.
fn quoted_word(mark: char, chars: &mut std::str::Chars<'_>) -> Option<Word> {
    let mut word = Word {
        text: String::new(),
        quoted: true,
        backslash: false,
    };
    while let Some(inner) = chars.next() {
        if inner == mark {
            return Some(word);
        }
        if inner.is_control() {
            return None;
        }
        if mark == '"' {
            match inner {
                '$' | '`' | '!' => return None,
                '\\' => {
                    word.backslash = true;
                    match chars.clone().next() {
                        None | Some('$' | '`' | '\n') => return None,
                        Some(escaped @ ('"' | '\\')) => {
                            chars.next();
                            word.text.push(escaped);
                            continue;
                        }
                        Some(_) => (),
                    }
                }
                _ => (),
            }
        }
        word.text.push(inner);
    }
    None
}

/// An unquoted word's text, or [`None`] when it was quoted.
fn bare(word: &Word) -> Option<&str> {
    (!word.quoted).then_some(word.text.as_str())
}

/// A path or directory argument: non-empty, and not something the command
/// would read as an option.
///
/// A quoted one is the word the shell passes, which is what lets a path with a
/// space in it be read. It is still refused when it spells what an agent
/// almost certainly meant as something else — a glob or a `~` the quotes kept
/// from expanding, or a backslash inside double quotes — because the shell
/// would open a file by that literal name, and a read credited to it is one
/// nobody intended.
fn path_arg(word: &Word) -> Option<String> {
    let text = word.text.as_str();
    let spells_a_pattern = word.backslash || text.contains(['*', '?', '[', '~']);
    if text.is_empty() || text.starts_with('-') || (word.quoted && spells_a_pattern) {
        return None;
    }
    Some(text.to_owned())
}

/// A pattern argument, which takes any quoted word the walk resolved — and
/// still not an option.
fn pattern_arg(word: &Word) -> Option<String> {
    if !word.quoted && (word.text.is_empty() || word.text.starts_with('-')) {
        return None;
    }
    Some(word.text.clone())
}

/// A positive line count, unquoted and written in digits.
///
/// Zero is refused: a command that prints nothing has read nothing, and a
/// range of no lines is not one this system records.
fn line_count(word: &Word) -> Option<u32> {
    line_count_text(bare(word)?)
}

/// The same count, read off text a caller has already unwrapped — the `-N`
/// spelling has the digits stuck to the option, so there is no word of its
/// own to hand [`line_count`].
fn line_count_text(text: &str) -> Option<u32> {
    let n = digits(text)?;
    (n > 0).then_some(n)
}

/// A number written in plain ASCII digits and nothing else, or [`None`].
///
/// Every number the grammar reads comes through here, because the integer
/// parser underneath accepts a leading `+` and the commands do not mean the
/// same thing by one: `tail -n +5` prints from line five to the end, and
/// `sed` reads `1,+3p` as line one and the three after it. A signed count is
/// refused rather than given its own reading — "from line `N` to the end" is
/// a span this module could only state with an end the hook does not hold.
/// Leading zeros pass: all three commands read the count in decimal, so
/// `05` is five to each of them.
fn digits(text: &str) -> Option<u32> {
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

/// `head` and `tail`, which share a shape: an optional `-n N` and one path.
fn top_or_bottom(rest: &[Word], from_top: bool) -> Option<ShellSpan> {
    let limit = match rest.len() {
        1 => None,
        3 if bare(rest.first()?) == Some("-n") => Some(line_count(rest.get(1)?)?),
        _ => return None,
    };
    Some(if from_top {
        ShellSpan::FromTop { limit }
    } else {
        ShellSpan::FromBottom { limit }
    })
}

/// One `sed` print element, `A,Bp` or the single line `Ap`.
fn sed_range(script: &str) -> Option<(u32, u32)> {
    let body = script.strip_suffix('p')?;
    let (lo, hi) = match body.split_once(',') {
        Some((lo, hi)) => (digits(lo)?, digits(hi)?),
        None => (digits(body)?, digits(body)?),
    };
    (lo >= 1 && hi >= lo).then_some((lo, hi))
}

/// A `sed` print script of runs joined by `;`, `A,Bp;Cp`, as its runs in the
/// order it names them — or [`None`] when any element is not one, or there
/// are more than one call may read.
///
/// Under `-n` such a script prints each run in the file's order, a line twice
/// where two overlap, and nothing else, so the runs are exactly what it
/// delivered. Nothing between the elements is admitted, not even a space.
fn sed_runs(script: &str) -> Option<Vec<(u32, u32)>> {
    let runs = script
        .split(';')
        .map(sed_range)
        .collect::<Option<Vec<_>>>()?;
    (runs.len() <= MAX_CHAIN_READS).then_some(runs)
}

/// The `ls` options the grammar admits, and no others.
const LIST_OPTIONS: &[&str] = &["-l", "-a", "-la", "-al"];

/// The single-letter flags a `grep` search may carry: a line number, a case
/// fold, a whole-word or whole-line match, a fixed string, the extended
/// dialect, and a count of the matching lines in place of the lines.
///
/// Each one changes which of the file's own lines are printed, how they are
/// labelled, or prints how many there were, and nothing else, which is the
/// whole condition: the output is still decided by the one file's bytes and
/// is still no interval of it. A flag that reaches past the file (`-r`, `-f`,
/// `--include`), prints something that is neither its lines nor their count
/// (`-l`, `-o`, `-q`), or the lines that did *not* match (`-v`), is not in
/// the set, and a long option never is.
const GREP_FLAGS: &str = "niwxcFE";

/// The same set for `egrep` and `fgrep`, which already name their dialect,
/// so a second dialect letter on one of them is left alone rather than
/// resolved.
const GREP_VARIANT_FLAGS: &str = "niwxc";

/// The same set for `rg`, without `-E`: there it names a text encoding and
/// takes a value, which is a different option behind the same letter.
const RG_FLAGS: &str = "niwxcF";

/// The letters that take a context count — lines after, before and around
/// each match — in every one of the four search commands.
const CONTEXT_FLAGS: &str = "ABC";

/// The letter that takes the pattern itself as its value.
const PATTERN_FLAG: char = 'e';

/// Split a command line at the separators the grammar composes with, without
/// ever splitting inside a quote.
///
/// Deliberately a *lexer* and not a parser: it knows only that a quote runs to
/// its own closing mark — past an escaped one inside double quotes, as the
/// shell reads it — and that everything outside one is text. A quote that
/// never closes swallows the rest of the line, which yields a piece
/// [`words`] refuses on the same grounds it always did — so the two halves
/// agree without either one asking the other.
///
/// `seps` is matched longest-first by the caller's own ordering, which is how
/// `&&` is found before a lone `&` would be (and a lone `&` is left in the
/// piece, where it is refused as a character no word may hold). `limit` caps
/// how many splits are made, so the first `&&` can be found without cutting
/// the rest of the line into pieces that would have to be joined back up —
/// and once that many have been made the walk stops, because everything left
/// is the last piece whatever it holds. That is what lets the `cd` peel below
/// step over a run of labels without re-reading the line once per label.
fn split_on<'a>(command: &'a str, seps: &[&str], limit: usize) -> Vec<&'a str> {
    let mut out: Vec<&str> = Vec::new();
    let mut open: Option<char> = None;
    let mut start = 0_usize;
    let mut at = 0_usize;
    while at < command.len() && out.len() < limit {
        let rest = &command[at..];
        let Some(c) = rest.chars().next() else { break };
        match open {
            Some(quote) => {
                if c == quote {
                    open = None;
                }
                // An escaped character inside double quotes never closes
                // one, so the split agrees with the word walk about where
                // each quote ends.
                if quote == '"' && c == '\\' {
                    at += rest.chars().nth(1).map_or(0, char::len_utf8);
                }
                at += c.len_utf8();
            }
            None => {
                if c == '\'' || c == '"' {
                    open = Some(c);
                    at += c.len_utf8();
                } else if let Some(sep) = seps.iter().find(|sep| rest.starts_with(**sep)) {
                    out.push(&command[start..at]);
                    at += sep.len();
                    start = at;
                } else {
                    at += c.len_utf8();
                }
            }
        }
    }
    out.push(&command[start..]);
    out
}

/// The directory a leading `cd` names, or [`None`] when this piece is not one.
///
/// The directory may be quoted, because a `cd` is where an agent most often
/// writes a directory with a space in it. The quote still has to wrap the
/// whole word and resolve to one fixed word — [`words`] sees to that — so what
/// is admitted is a spelling, never an expansion.
fn cd_dir(piece: &str) -> Option<String> {
    let words = words(piece)?;
    match words.as_slice() {
        [head, dir] if bare(head) == Some("cd") => {
            (!dir.text.is_empty() && !dir.text.starts_with('-')).then(|| dir.text.clone())
        }
        _ => None,
    }
}

/// Whether a path is already rooted, so that a leading `cd` says nothing
/// about it.
///
/// Two spellings, because both reach this grammar: a POSIX path, and the
/// drive-letter form a Windows agent writes with forward slashes. Nothing here
/// touches a filesystem, so this is a judgement about the text and is
/// deliberately the *wider* of the two readings — a path wrongly called rooted
/// is one the caller fails to resolve, which costs a saving.
fn is_rooted(path: &str) -> bool {
    let mut chars = path.chars();
    match (chars.next(), chars.next()) {
        (Some('/'), _) => true,
        (Some(drive), Some(':')) => drive.is_ascii_alphabetic(),
        _ => false,
    }
}

/// A path as read from a directory a leading `cd` moved to.
///
/// A lexical join and nothing more: no component is resolved, no `..` is
/// folded away, and the caller's own path resolution — which is the only
/// thing in this system with filesystem authority — does all of that against
/// the working directory the payload states.
fn under(dir: Option<&str>, path: String) -> String {
    match dir {
        Some(dir) if !is_rooted(&path) => format!("{}/{path}", dir.trim_end_matches('/')),
        _ => path,
    }
}

/// The trailing `| head -N` / `| tail -N` of a member, or [`None`] when the
/// pipe led somewhere the grammar has no reading for.
///
/// Both the `-N` and the `-n N` spellings are taken here, and only here. In
/// the *command* position `head -5 file` stays out of the grammar, because
/// there a bare `-5` sits next to a path and the published list spells the
/// option form; after a pipe there is no path to be confused with, and `-N` is
/// what an agent actually writes.
fn narrowing(piece: &str) -> Option<Narrowing> {
    let words = words(piece)?;
    let (head, rest) = words.split_first()?;
    match bare(head)? {
        "head" => Some(Narrowing::First(narrowing_count(rest)?)),
        "tail" => Some(Narrowing::Last(narrowing_count(rest)?)),
        "sed" => line_pick(rest),
        _ => None,
    }
}

/// The count of a narrowing `head` or `tail`, in either spelling.
fn narrowing_count(rest: &[Word]) -> Option<u32> {
    match rest {
        [n] => line_count_text(bare(n)?.strip_prefix('-')?),
        [flag, n] if bare(flag) == Some("-n") => line_count(n),
        _ => None,
    }
}

/// A narrowing `sed -n 'A,Bp'`: one print element of the script `sed_read`
/// takes, and nothing after it.
///
/// With no file after the script `sed` reads the pipe, and under `-n` it
/// prints lines `A` to `B` of what came down it and nothing else — `head` and
/// `tail` arithmetic with both ends stated. A file after the script would be
/// read in place of the pipe, so there is none; a script of several elements
/// prints several stretches no one narrowing names, so there is one.
fn line_pick(rest: &[Word]) -> Option<Narrowing> {
    let [flag, script] = rest else {
        return None;
    };
    if bare(flag) != Some("-n") {
        return None;
    }
    let (lo, hi) = sed_range(&pattern_arg(script)?)?;
    Some(Narrowing::Pick { lo, hi })
}

/// Lines `lo` to `hi` of a run that is lines `first` to `last` of the file,
/// as the span of the file they are.
///
/// The pick is cut at the run's own end, as `sed` stops where its input
/// does. One that starts past that end printed nothing, and a command that
/// prints nothing has read nothing, so it is unknown rather than an empty
/// interval no span can spell.
fn picked((first, last): (u32, u32), (lo, hi): (u32, u32)) -> ReadSpan {
    let Some(start) = first.checked_add(lo.saturating_sub(1)) else {
        return ReadSpan::Unknown;
    };
    let end = last.min(first.saturating_add(hi.saturating_sub(1)));
    if start > end {
        return ReadSpan::Unknown;
    }
    ReadSpan::lines(start, HiKind::Explicit(end))
}

/// What one member of a chain was, before a leading `cd` is applied to it.
enum Member {
    /// One or more reads, in the order the member performs them.
    Reads(Vec<ShellRead>),
    /// A name listing.
    Listing(ShellListing),
    /// A member that printed something the agent had already written and read
    /// nothing at all: the label between two reads.
    Label,
}

/// The two commands a chain uses to label its own output.
///
/// Neither opens a file under any argument. `echo` writes its operands and
/// `printf` writes its operands through a format, and that is the whole of
/// what either does — so a member spelled with one is a member that read
/// nothing, and the only question a chain has to ask about it is whether it
/// can be *sure* that is all it did. The word walk answers that: an operand
/// reaches this list only as a bare word or a whole-word quoted literal, and
/// every way a shell could turn one of those into something else — `$`, a
/// backtick, a backslash outside a single-quoted word, a redirect, a
/// substitution — is a character no word may hold. A backslash inside double
/// quotes is the one exception the walk admits, and a label refuses it
/// itself, because both commands may read it as an escape of their own.
const LABEL_COMMANDS: &[&str] = &["echo", "printf"];

/// The two directories a command word may be spelled from.
///
/// They hold the system's own copies of the vocabulary — root-owned on a
/// Unix system, and sealed by the operating system on macOS — so a
/// program reached through one of them is the program the word names, more
/// exactly than a bare word resolved through a `PATH` is. Every other
/// directory is one a user or a package manager writes into, and a program
/// found there is whatever was put there.
const SYSTEM_COMMAND_DIRS: &[&str] = &["/usr/bin/", "/bin/"];

/// Recognise one *simple* command — the published vocabulary, and nothing
/// composed.
///
/// A dispatch on the command word and nothing more: each family below reads
/// its own operands, so a shape one family admits is never mistaken for a
/// shape another one does, and a word off the list falls through to [`None`].
fn recognise_simple(command: &str) -> Option<Member> {
    let words = words(command)?;
    let (head, rest) = words.split_first()?;
    let head_word = bare(head)?;
    // The operands were checked before the command word was read: reaching
    // here at all means every one of them is a bare word or a whole-word
    // quoted literal, which is the whole condition on a label. The count is
    // not one of the conditions — `echo` with no operands is the blank line an
    // agent puts between two reads, and it is the commonest label there is.
    // A backslash inside double quotes leaves a word the shell passes
    // exactly, but `echo` and `printf` may each read the text as an escape of
    // their own, and a label that prints something other than its text is a
    // shape nobody has measured.
    if LABEL_COMMANDS.contains(&head_word) {
        return (!rest.iter().any(|word| word.backslash)).then_some(Member::Label);
    }
    let name = command_name(head_word);
    match name {
        "cat" => cat_reads(rest),
        "nl" | "bat" | "batcat" => decorated_read(rest),
        "head" | "tail" => {
            let span = top_or_bottom(rest, name == "head")?;
            one_read(path_arg(rest.last()?)?, span)
        }
        "sed" => sed_read(rest),
        "awk" => awk_read(rest),
        "grep" => content_search(rest, GREP_FLAGS),
        "egrep" | "fgrep" => content_search(rest, GREP_VARIANT_FLAGS),
        "rg" => rg_call(rest),
        "ls" => ls_listing(rest),
        "find" => find_listing(rest),
        _ => None,
    }
}

/// The command a bare command word names: the word itself, or the name
/// after one of the [`SYSTEM_COMMAND_DIRS`].
///
/// Only the read and listing dispatch asks. A `cd` is read off its bare word
/// alone, because the `cd` a system ships runs in a child process and moves
/// nobody's directory; a label and the narrowing commands after a pipe keep
/// their bare spelling, because that is the only one anybody has measured in
/// those positions. A name left with a `/` in it is no command of the
/// vocabulary, so the dispatch refuses it like any other word off the list.
fn command_name(word: &str) -> &str {
    SYSTEM_COMMAND_DIRS
        .iter()
        .find_map(|dir| word.strip_prefix(dir))
        .unwrap_or(word)
}

/// `cat`: the whole file, no options — an option would change what is
/// printed, and none of them are on the published list. Several paths are one
/// read each, which is what the command does; it is the *narrowing* of a
/// several-path print that has no reading, not the print.
fn cat_reads(rest: &[Word]) -> Option<Member> {
    if rest.is_empty() {
        return None;
    }
    let reads = rest
        .iter()
        .map(|path| {
            Some(ShellRead {
                path: path_arg(path)?,
                span: ShellSpan::WholeFile,
                narrowing: None,
            })
        })
        .collect::<Option<Vec<_>>>()?;
    Some(Member::Reads(reads))
}

/// `nl`, `bat`, `batcat`: the whole file, transformed for display.
fn decorated_read(rest: &[Word]) -> Option<Member> {
    match rest {
        [path] => one_read(path_arg(path)?, ShellSpan::DecoratedWholeFile),
        _ => None,
    }
}

/// `sed -n 'A,Bp'`: `-n` suppresses the default printing, which is what makes
/// the script's own `p` the whole of what is printed.
///
/// One run is one read with its range. A script of several runs is one read
/// per run, recognised and credited with nothing (see
/// [`ShellSpan::OneOfSeveralRuns`]); naming the runs is still what keeps the
/// line out of the unseen pool and in a decision that can see all of it.
fn sed_read(rest: &[Word]) -> Option<Member> {
    let [flag, script, path] = rest else {
        return None;
    };
    if bare(flag) != Some("-n") {
        return None;
    }
    let path = path_arg(path)?;
    match sed_runs(&pattern_arg(script)?)?.as_slice() {
        [(lo, hi)] => one_read(path, ShellSpan::Lines { lo: *lo, hi: *hi }),
        runs => Some(Member::Reads(
            runs.iter()
                .map(|(lo, hi)| ShellRead {
                    path: path.clone(),
                    span: ShellSpan::OneOfSeveralRuns { lo: *lo, hi: *hi },
                    narrowing: None,
                })
                .collect(),
        )),
    }
}

/// `awk 'NR>=A && NR<=B'`: the one `awk` program that is a line pick, and the
/// only `awk` the grammar reads.
///
/// A pattern with no action prints every record it matches as it was read,
/// and a record is a line, so this program prints lines `A` to `B` of the
/// one file and nothing else — exactly what `sed -n 'A,Bp'` prints, and so
/// the same span with the same coverage. It is not on the published list; it
/// is admitted because no reading of it depends on the file's bytes, which
/// is the condition every form here meets. Every other `awk` program is left
/// alone, because what a program prints is the program's to decide.
///
/// The operand must be a file: `awk` reads one spelled `name=value` as an
/// assignment, so a path holding `=` is refused, and a second file would
/// carry the record count across, so there is exactly one.
fn awk_read(rest: &[Word]) -> Option<Member> {
    let [program, path] = rest else {
        return None;
    };
    let (lo, hi) = awk_line_pick(&pattern_arg(program)?)?;
    let path = path_arg(path)?;
    if path.contains('=') {
        return None;
    }
    one_read(path, ShellSpan::Lines { lo, hi })
}

/// The two line numbers of an `NR>=A && NR<=B` program, or [`None`] for any
/// other program.
///
/// Spaces may stand between the program's tokens and nowhere inside one —
/// `N R` is two variables to `awk`, and `> =` is no operator. A tab never
/// reaches here: the quoted word that carries the program refuses every
/// control character.
fn awk_line_pick(program: &str) -> Option<(u32, u32)> {
    let rest = awk_token(program, "NR")?;
    let rest = awk_token(rest, ">=")?;
    let (lo, rest) = awk_number(rest)?;
    let rest = awk_token(rest, "&&")?;
    let rest = awk_token(rest, "NR")?;
    let rest = awk_token(rest, "<=")?;
    let (hi, rest) = awk_number(rest)?;
    let ends = rest.trim_start_matches(' ').is_empty();
    (ends && lo >= 1 && hi >= lo).then_some((lo, hi))
}

/// The program after `token`, where `token` comes next after any spaces.
fn awk_token<'a>(program: &'a str, token: &str) -> Option<&'a str> {
    program.trim_start_matches(' ').strip_prefix(token)
}

/// The number that comes next after any spaces, and the program after it.
///
/// Plain digits, as every count here is, and with no leading zero: unlike
/// `sed`, `gawk` reads a constant in the program's text that starts with `0`
/// as octal, so `010` is eight to it and ten to the `awk` macOS ships, and a
/// line pick whose lines depend on which `awk` runs is no line pick.
fn awk_number(program: &str) -> Option<(u32, &str)> {
    let program = program.trim_start_matches(' ');
    let end = program
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(program.len());
    let (number, rest) = program.split_at(end);
    if number.starts_with('0') {
        return None;
    }
    Some((digits(number)?, rest))
}

/// The `grep` family: a content search over the files the line names. It is
/// a read of lines nobody here can name, and it is never answered from an
/// index.
///
/// **Several named files are one search of each**, in the order the line
/// names them: the tool opens those files and nothing else, searches each for
/// the same pattern under the same flags, and labels every match with its
/// file, so what it printed is still decided by the named files' bytes alone.
/// Each one is recognised as a one-file search is, with no coverage, and the
/// member is refused a narrowing pipe like any other member of several reads,
/// because a cut of the joined matches is a run of none of the files. A
/// flag that reaches past the named files (`-r`) stays refused, because then
/// which files were opened is the filesystem's to say.
///
/// Options are read the way the command's own option parser reads them —
/// letters may share one word, and a letter that takes a value takes the rest
/// of its word or, when that is empty, the next word — so the word this walk
/// calls a path is the word the search opens.
///
/// **An option may stand anywhere before `--`**, and every word after `--` is
/// an operand. That is GNU argument permutation, and it is how the `grep` an
/// agent's shell actually runs reads a line: Claude Code runs `grep` as
/// `ugrep`, which permutes, and so do GNU `grep` and `rg`. A `grep` that does
/// not permute would read a late `-n` as a file name, so this reading can be
/// wrong on such a machine — and it is admitted anyway because a search
/// grants no coverage. A misread search can only move a measurement; it can
/// never decline a read. Any later form that *does* grant coverage from a
/// late option must first establish which `grep` runs, because there a
/// misreading would be a decline about content the agent never received.
///
/// Every admitted flag only chooses or labels the lines of the one file, so a
/// flagged search is the same recognition a bare one is: [`ShellSpan::Matching`],
/// with no coverage.
fn content_search(rest: &[Word], flags: &str) -> Option<Member> {
    let mut words = rest.iter();
    let mut pattern_given = false;
    let mut operands: Vec<&Word> = Vec::new();
    while let Some(word) = words.next() {
        if bare(word) == Some("--") {
            operands.extend(words.by_ref());
            break;
        }
        if !word.text.starts_with('-') {
            operands.push(word);
            continue;
        }
        // A quoted word that starts with `-` is an option to the tool all
        // the same, wherever it stands, and the grammar pins no quoted one.
        let letters = bare(word)?.strip_prefix('-')?;
        pattern_given |= search_option(letters, flags, &mut words)?;
    }
    let paths = match (pattern_given, operands.as_slice()) {
        (true, paths) => paths,
        (false, [pattern, paths @ ..]) if !pattern.text.starts_with('-') => paths,
        _ => return None,
    };
    if paths.is_empty() {
        return None;
    }
    let reads = paths
        .iter()
        .map(|path| {
            Some(ShellRead {
                path: path_arg(path)?,
                span: ShellSpan::Matching,
                narrowing: None,
            })
        })
        .collect::<Option<Vec<_>>>()?;
    Some(Member::Reads(reads))
}

/// One option word of a search, its leading `-` already taken off: whether it
/// gave the pattern, or [`None`] when any letter in it is not one the grammar
/// pins.
///
/// A pattern here is any word, since the option is what says it is one —
/// except an unquoted one that starts with `-`, which the same rule refuses
/// everywhere a pattern goes. A count is digits and nothing else, so a signed
/// one is refused as it is in every other count position.
fn search_option<'a>(
    letters: &str,
    flags: &str,
    words: &mut impl Iterator<Item = &'a Word>,
) -> Option<bool> {
    if letters.is_empty() {
        return None;
    }
    for (at, letter) in letters.char_indices() {
        if flags.contains(letter) {
            continue;
        }
        let attached = letters.get(at + letter.len_utf8()..)?;
        if CONTEXT_FLAGS.contains(letter) {
            let count = match attached {
                "" => bare(words.next()?)?,
                count => count,
            };
            return digits(count).map(|_| false);
        }
        if letter != PATTERN_FLAG {
            return None;
        }
        let pattern_is_one = match attached {
            "" => pattern_arg(words.next()?).is_some(),
            pattern => !pattern.starts_with('-'),
        };
        return pattern_is_one.then_some(true);
    }
    Some(false)
}

/// `rg`: one name does both jobs — `--files` lists, a pattern searches.
fn rg_call(rest: &[Word]) -> Option<Member> {
    match rest {
        [flag] if bare(flag) == Some("--files") => one_listing(".".to_owned(), None),
        [flag, dir] if bare(flag) == Some("--files") => one_listing(path_arg(dir)?, None),
        _ => content_search(rest, RG_FLAGS),
    }
}

/// `ls` over one directory, with at most two of the options the grammar
/// admits and none it does not.
fn ls_listing(rest: &[Word]) -> Option<Member> {
    let (dir, options) = rest.split_last()?;
    let admitted = |option: &Word| bare(option).is_some_and(|o| LIST_OPTIONS.contains(&o));
    if options.len() > 2 || !options.iter().all(admitted) {
        return None;
    }
    one_listing(path_arg(dir)?, None)
}

/// `find`: the directory comes first, the name pattern last, and the only
/// predicate in between is the one the grammar names.
fn find_listing(rest: &[Word]) -> Option<Member> {
    match rest {
        [dir, name, pattern] if bare(name) == Some("-name") => {
            one_listing(path_arg(dir)?, Some(pattern_arg(pattern)?))
        }
        [dir, kind, f, name, pattern]
            if bare(kind) == Some("-type")
                && bare(f) == Some("f")
                && bare(name) == Some("-name") =>
        {
            one_listing(path_arg(dir)?, Some(pattern_arg(pattern)?))
        }
        _ => None,
    }
}

/// A member that is exactly one read of `path`, with no narrowing yet — the
/// pipe, where there is one, is attached by the member walk above.
fn one_read(path: String, span: ShellSpan) -> Option<Member> {
    Some(Member::Reads(vec![ShellRead {
        path,
        span,
        narrowing: None,
    }]))
}

/// A member that is one listing of `dir`.
fn one_listing(dir: String, pattern: Option<String>) -> Option<Member> {
    Some(Member::Listing(ShellListing { dir, pattern }))
}

#[cfg(test)]
mod tests;
