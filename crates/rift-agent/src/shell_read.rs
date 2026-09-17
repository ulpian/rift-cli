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
//! list does not spell is left alone.
//!
//! **The published list is the vocabulary, not the whole sentence.** Measured
//! on this machine's own transcripts, an agent almost never writes one of
//! these commands by itself: of 864 shell reads in eight sessions, 35 were a
//! bare simple command, and most of the other 829 were a `cd` into a
//! directory, a trailing `| head -N`, or several reads joined by `&&`. Every
//! one of those was a read of this repository that the ledger never saw, and
//! a read the ledger never saw can never be declined and never counted. So
//! three *composition* rules sit above the vocabulary — an optional leading
//! `cd <dir> &&`, a trailing `| head -N` or `| tail -N` on a read, and a chain
//! of reads joined by `&&` or `;`. Each one is a shape whose delivered lines
//! still follow from the line itself; a member the vocabulary does not spell
//! still takes the whole command out of the grammar, exactly as before.
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
//! Three rules hold everywhere below.
//!
//! 1. **A non-match is never touched.** Every ambiguity — an option the list
//!    does not name, a quote the grammar does not pin, a pipe into anything
//!    but the two narrowing commands, a member that is not itself a read, a
//!    character the shell would act on — ends in [`None`]. The cost of a
//!    missed recognition is a saving; the cost of a wrong one is a decision
//!    taken about content an agent never received.
//! 2. **A recognised command is not always a range.** `grep` delivers the
//!    lines that matched, which nothing here can know; a bare `head` or
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

use crate::{HiKind, ReadChannel, ReadRequest, ReadSpan, SearchKind, SearchRequest};

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
    /// An explicit run of lines: `sed -n 'A,Bp'`.
    Lines {
        /// The first line printed.
        lo: u32,
        /// The last line printed.
        hi: u32,
    },
    /// Only the lines that matched a pattern: the `grep` family, and `rg`
    /// with a pattern. **Not a derivable range**, and never a name search.
    Matching,
}

/// A trailing `| head -N` or `| tail -N` that narrows what a read delivered.
///
/// The two commands are in the grammar as *readers* already, and in this
/// position they are doing the same arithmetic on the run of lines the member
/// before them produced rather than on a file. That is why they are the only
/// two pipe targets admitted: every other one — a `grep`, a `wc`, a pager —
/// either changes what the lines are or hands the window something other than
/// lines, and a member piped into one of those is not a read this module will
/// name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Narrowing {
    /// `| head -N`: the first `N` lines of the run that was produced.
    First(u32),
    /// `| tail -N`: the last `N` lines of it.
    Last(u32),
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
        use Narrowing::{First, Last};
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
            (ShellSpan::FromBottom { limit: Some(m) }, None) => ReadSpan::LastLines { count: m },
            (ShellSpan::FromBottom { limit: Some(m) }, Some(Last(n))) => {
                ReadSpan::LastLines { count: m.min(n) }
            }
            // Decoration changes the byte count; a `head` or `tail` relying on
            // its own default states no count; the `grep` family prints what
            // matched. None of those is an interval of this file.
            (ShellSpan::FromBottom { limit: Some(_) }, Some(First(_)))
            | (
                ShellSpan::DecoratedWholeFile
                | ShellSpan::FromTop { limit: None }
                | ShellSpan::FromBottom { limit: None }
                | ShellSpan::Matching,
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
/// The distinction is enforced rather than recorded: quoting is pinned only
/// where the grammar needs it — a `sed` script, a `find` name pattern, a
/// search pattern — and a quoted word anywhere else takes the whole line out
/// of the grammar.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Word {
    text: String,
    quoted: bool,
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
/// Bytes above ASCII are allowed: a path may be spelled in any script, and no
/// character the shell acts on lives up there.
const fn bare_char(c: char) -> bool {
    c.is_ascii_alphanumeric()
        || matches!(c, '_' | '.' | '/' | '-' | '+' | '=' | ',' | ':' | '@' | '%')
        || !c.is_ascii()
}

/// Split a command line into words, or refuse it.
///
/// Refusal is the common case and is meant to be: anything with a character
/// the shell would act on, a quote that does not wrap a whole word, an
/// unterminated quote, a line break, or a double-quoted word that could still
/// expand (`$`, a backtick or a backslash inside it) is not a command line
/// this module will classify.
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
                let mut text = String::new();
                let mut closed = false;
                for inner in chars.by_ref() {
                    if inner == c {
                        closed = true;
                        break;
                    }
                    // Inside double quotes the shell still expands, so the
                    // three characters that do it end the recognition. A
                    // control character ends it in either kind of quote.
                    let expands = c == '"' && matches!(inner, '$' | '`' | '\\');
                    if expands || inner.is_control() {
                        return None;
                    }
                    text.push(inner);
                }
                if !closed {
                    return None;
                }
                out.push(Word { text, quoted: true });
                // The closing quote must end the word.
                if !matches!(chars.clone().next(), None | Some(' ' | '\t')) {
                    return None;
                }
            }
            _ if bare_char(c) => match current.as_mut() {
                Some(word) => word.text.push(c),
                None => {
                    current = Some(Word {
                        text: c.to_string(),
                        quoted: false,
                    });
                }
            },
            _ => return None,
        }
    }
    if let Some(word) = current.take() {
        out.push(word);
    }
    Some(out)
}

/// An unquoted word's text, or [`None`] when it was quoted.
fn bare(word: &Word) -> Option<&str> {
    (!word.quoted).then_some(word.text.as_str())
}

/// A path or directory argument: unquoted, non-empty, and not something the
/// command would read as an option.
fn path_arg(word: &Word) -> Option<String> {
    let text = bare(word)?;
    if text.is_empty() || text.starts_with('-') {
        return None;
    }
    Some(text.to_owned())
}

/// A pattern argument, which is the one place the grammar admits quotes —
/// and still not an option.
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
    let n: u32 = text.parse().ok()?;
    (n > 0).then_some(n)
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

/// A `sed` print script, `A,Bp` — the only script the grammar admits.
fn sed_range(script: &str) -> Option<ShellSpan> {
    let body = script.strip_suffix('p')?;
    let (lo, hi) = body.split_once(',')?;
    let lo: u32 = lo.parse().ok()?;
    let hi: u32 = hi.parse().ok()?;
    (lo >= 1 && hi >= lo).then_some(ShellSpan::Lines { lo, hi })
}

/// The `ls` options the grammar admits, and no others.
const LIST_OPTIONS: &[&str] = &["-l", "-a", "-la", "-al"];

/// Split a command line at the separators the grammar composes with, without
/// ever splitting inside a quote.
///
/// Deliberately a *lexer* and not a parser: it knows only that a quote runs to
/// its own closing mark and that everything outside one is text. A quote that
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
/// A `cd` is the one place the grammar takes a quoted path, because it is the
/// one place an agent routinely writes a directory with a space in it. The
/// quote still has to wrap the whole word — [`words`] sees to that — so what
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
    let count = match rest {
        [n] => line_count_text(bare(n)?.strip_prefix('-')?)?,
        [flag, n] if bare(flag) == Some("-n") => line_count(n)?,
        _ => return None,
    };
    match bare(head)? {
        "head" => Some(Narrowing::First(count)),
        "tail" => Some(Narrowing::Last(count)),
        _ => None,
    }
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
/// substitution — is a character no word may hold.
const LABEL_COMMANDS: &[&str] = &["echo", "printf"];

/// Recognise one member: a simple command, and at most one trailing pipe into
/// a narrowing command.
///
/// A narrowing is attached only to a member that is exactly one read. `cat a
/// b | head -n 5` prints the first five lines of the two files *joined*, which
/// is a run of neither of them, so the member is refused rather than
/// described wrongly — and refusing a member refuses the whole line. A label
/// piped onward is refused for the narrower version of the same reason: the
/// narrowing rule is arithmetic on the lines a *read* produced, and a pipe out
/// of something that read nothing is a shape nobody has measured.
fn recognise_member(member: &str) -> Option<Member> {
    let parts = split_on(member, &["|"], usize::MAX);
    let (simple, narrowing) = match parts.as_slice() {
        [simple] => (*simple, None),
        [simple, narrow] => (*simple, Some(narrowing(narrow)?)),
        _ => return None,
    };
    let member = recognise_simple(simple)?;
    match (member, narrowing) {
        (member, None) => Some(member),
        (Member::Reads(mut reads), Some(narrowing)) => match reads.as_mut_slice() {
            [read] => {
                read.narrowing = Some(narrowing);
                Some(Member::Reads(reads))
            }
            _ => None,
        },
        (Member::Listing(_) | Member::Label, Some(_)) => None,
    }
}

/// Peel the optional leading `cd <dir> &&`, and say what is left of the line.
///
/// Against `&&` alone, which is the join that makes the directory a
/// precondition of the reads rather than an unrelated statement standing
/// before them.
///
/// Labels in front of the `cd` are stepped over, because a label is a member
/// that does nothing and a rule that said otherwise here would mean an `echo`
/// at the head of a line silently cost every path after it its directory. The
/// labels are dropped with the `cd` for the same reason they are dropped
/// anywhere else: they contribute no read.
///
/// The whole line comes back untouched when no `cd` is found, labels included,
/// so an unpeeled label is still an ordinary member below.
fn cd_prefix(command: &str) -> (Option<String>, &str) {
    let mut rest = command;
    loop {
        let split = split_on(rest, &["&&"], 1);
        let [head, tail] = split.as_slice() else {
            return (None, command);
        };
        if let Some(dir) = cd_dir(head) {
            return (Some(dir), tail);
        }
        if !matches!(recognise_simple(head), Some(Member::Label)) {
            return (None, command);
        }
        // Each step consumes one `&&`, so this walks the line once at most.
        rest = tail;
    }
}

/// Recognise one command line, or leave it alone.
///
/// The only entry point. Above the published vocabulary it admits exactly
/// three compositions, each one a shape whose delivered lines still follow
/// from the line alone: a single leading `cd <dir> &&`, one trailing
/// `| head -N` or `| tail -N` on a read, and members joined by `&&` or `;` of
/// which every one is itself a read — **or a label**, which is a member that
/// read nothing and so neither contributes a read nor takes the line out of
/// the grammar.
///
/// Everything else is what it always was — no redirection, no substitution,
/// no variable, no glob outside a pattern argument, no environment
/// assignment, no `sudo`, no `xargs`, no background `&`, no second `cd`, no
/// `cd` anywhere but the front, no listing mixed in among reads — and every
/// one of those is a character, a word or a member the walk below never
/// accepts.
#[must_use]
pub fn recognise(command: &str) -> Option<ShellCall> {
    if command.len() > MAX_COMMAND_BYTES {
        return None;
    }
    let (dir, body) = cd_prefix(command);
    let dir = dir.as_deref();
    let members = split_on(body, &["&&", ";"], usize::MAX);
    let mut reads: Vec<ShellRead> = Vec::new();
    for member in &members {
        match recognise_member(member)? {
            Member::Reads(found) => reads.extend(found),
            // A label is a member with nothing to say for itself and nothing
            // to answer for: it read no file, so it adds no request, and it
            // is no reason to leave the rest of the line unaccounted for.
            Member::Label => {}
            // A listing stands alone. Beside anything else it would put a
            // question about names inside a decision about content, which is
            // the one substitution rule 3 exists to prevent — and "anything
            // else" includes a label, because a listing among labels is a
            // shape nobody has measured and refusing it costs one saving.
            Member::Listing(listing) if members.len() == 1 => {
                return Some(ShellCall::Listing(ShellListing {
                    dir: under(dir, listing.dir),
                    pattern: listing.pattern,
                }));
            }
            Member::Listing(_) => return None,
        }
        if reads.len() > MAX_CHAIN_READS {
            return None;
        }
    }
    // A line that labelled its own output and read nothing is not a read.
    if reads.is_empty() {
        return None;
    }
    Some(ShellCall::Read(
        reads
            .into_iter()
            .map(|read| ShellRead {
                path: under(dir, read.path),
                ..read
            })
            .collect(),
    ))
}

/// Recognise one *simple* command — the published vocabulary, and nothing
/// composed.
fn recognise_simple(command: &str) -> Option<Member> {
    let words = words(command)?;
    let (head, rest) = words.split_first()?;
    let read = |path: String, span: ShellSpan| {
        Some(Member::Reads(vec![ShellRead {
            path,
            span,
            narrowing: None,
        }]))
    };
    let listing =
        |dir: String, pattern: Option<String>| Some(Member::Listing(ShellListing { dir, pattern }));
    let head_word = bare(head)?;
    // The operands were checked before the command word was read: reaching
    // here at all means every one of them is a bare word or a whole-word
    // quoted literal, which is the whole condition on a label. The count is
    // not one of the conditions — `echo` with no operands is the blank line an
    // agent puts between two reads, and it is the commonest label there is.
    if LABEL_COMMANDS.contains(&head_word) {
        return Some(Member::Label);
    }
    match head_word {
        // The whole file, no options: an option would change what is printed,
        // and none of them are on the published list. Several paths are one
        // read each, which is what the command does — it is the *narrowing* of
        // a several-path print that has no reading, not the print.
        "cat" if !rest.is_empty() => Some(Member::Reads(
            rest.iter()
                .map(|path| {
                    Some(ShellRead {
                        path: path_arg(path)?,
                        span: ShellSpan::WholeFile,
                        narrowing: None,
                    })
                })
                .collect::<Option<Vec<_>>>()?,
        )),
        "nl" | "bat" | "batcat" => match rest {
            [path] => read(path_arg(path)?, ShellSpan::DecoratedWholeFile),
            _ => None,
        },
        "head" | "tail" => {
            let span = top_or_bottom(rest, head_word == "head")?;
            read(path_arg(rest.last()?)?, span)
        }
        // `-n` suppresses the default printing, which is what makes the
        // script's own `p` the whole of what is printed.
        "sed" => match rest {
            [flag, script, path] if bare(flag) == Some("-n") => {
                read(path_arg(path)?, sed_range(&pattern_arg(script)?)?)
            }
            _ => None,
        },
        // A content search over one named file. It is a read of lines nobody
        // here can name, and it is never answered from an index.
        "grep" | "egrep" | "fgrep" => match rest {
            [pattern, path] if pattern_arg(pattern).is_some() => {
                read(path_arg(path)?, ShellSpan::Matching)
            }
            _ => None,
        },
        // One name does both jobs: `--files` lists, a pattern searches.
        "rg" => match rest {
            [flag] if bare(flag) == Some("--files") => listing(".".to_owned(), None),
            [flag, dir] if bare(flag) == Some("--files") => listing(path_arg(dir)?, None),
            [pattern, path] if pattern_arg(pattern).is_some() => {
                read(path_arg(path)?, ShellSpan::Matching)
            }
            _ => None,
        },
        "ls" => {
            let (dir, options) = rest.split_last()?;
            if options.len() > 2
                || !options
                    .iter()
                    .all(|o| bare(o).is_some_and(|o| LIST_OPTIONS.contains(&o)))
            {
                return None;
            }
            listing(path_arg(dir)?, None)
        }
        // The directory comes first, the name pattern last, and the only
        // predicate in between is the one the grammar names.
        "find" => match rest {
            [dir, name, pattern] if bare(name) == Some("-name") => {
                listing(path_arg(dir)?, Some(pattern_arg(pattern)?))
            }
            [dir, kind, f, name, pattern]
                if bare(kind) == Some("-type")
                    && bare(f) == Some("f")
                    && bare(name) == Some("-name") =>
            {
                listing(path_arg(dir)?, Some(pattern_arg(pattern)?))
            }
            _ => None,
        },
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        recognise, Narrowing, ShellCall, ShellListing, ShellRead, ShellSpan, MAX_CHAIN_READS,
        MAX_COMMAND_BYTES,
    };
    use crate::ReadChannel;
    use crate::{HiKind, ReadRequest, ReadSpan, SearchKind};

    /// The read a command line supports, as `(path, span)`.
    fn read_of(command: &str) -> Option<(String, ReadSpan)> {
        let ReadRequest {
            path,
            span,
            channel,
        } = recognise(command)?.read_request()?;
        assert_eq!(channel, ReadChannel::Shell, "a shell read is a shell read");
        Some((path, span))
    }

    /// Every read a command line performs, as `(path, span)` apiece.
    fn reads_of(command: &str) -> Vec<(String, ReadSpan)> {
        recognise(command)
            .map(|call| call.read_requests())
            .unwrap_or_default()
            .into_iter()
            .map(|request| (request.path, request.span))
            .collect()
    }

    /// One member of a recognised read, spelled as compactly as the table
    /// needs it.
    fn member(path: &str, span: ShellSpan) -> ShellRead {
        ShellRead {
            path: path.to_owned(),
            span,
            narrowing: None,
        }
    }

    /// The same, with the trailing pipe that narrowed it.
    fn narrowed(path: &str, span: ShellSpan, narrowing: Narrowing) -> ShellRead {
        ShellRead {
            narrowing: Some(narrowing),
            ..member(path, span)
        }
    }

    /// A recognised read of one file.
    fn read(path: &str, span: ShellSpan) -> ShellCall {
        ShellCall::Read(vec![member(path, span)])
    }

    /// A recognised listing, likewise.
    fn listing(dir: &str, pattern: Option<&str>) -> ShellCall {
        ShellCall::Listing(ShellListing {
            dir: dir.to_owned(),
            pattern: pattern.map(str::to_owned),
        })
    }

    /// **Every form the grammar spells, and the range each one supports.**
    ///
    /// The table is the specification: a command on the published list yields
    /// the span that list's semantics allow, and the two forms whose lines
    /// depend on the file yield a recognition without a range rather than a
    /// range that would be a guess.
    #[test]
    fn every_grammar_form_yields_the_pinned_range() {
        let file = "src/lib.rs";
        let cases: &[(&str, ShellCall)] = &[
            ("cat src/lib.rs", read(file, ShellSpan::WholeFile)),
            ("nl src/lib.rs", read(file, ShellSpan::DecoratedWholeFile)),
            ("bat src/lib.rs", read(file, ShellSpan::DecoratedWholeFile)),
            (
                "batcat src/lib.rs",
                read(file, ShellSpan::DecoratedWholeFile),
            ),
            (
                "head src/lib.rs",
                read(file, ShellSpan::FromTop { limit: None }),
            ),
            (
                "head -n 40 src/lib.rs",
                read(file, ShellSpan::FromTop { limit: Some(40) }),
            ),
            (
                "tail src/lib.rs",
                read(file, ShellSpan::FromBottom { limit: None }),
            ),
            (
                "tail -n 40 src/lib.rs",
                read(file, ShellSpan::FromBottom { limit: Some(40) }),
            ),
            (
                "sed -n '12,48p' src/lib.rs",
                read(file, ShellSpan::Lines { lo: 12, hi: 48 }),
            ),
            (
                "sed -n 12,48p src/lib.rs",
                read(file, ShellSpan::Lines { lo: 12, hi: 48 }),
            ),
            ("grep needle src/lib.rs", read(file, ShellSpan::Matching)),
            ("egrep 'a|b' src/lib.rs", read(file, ShellSpan::Matching)),
            ("fgrep needle src/lib.rs", read(file, ShellSpan::Matching)),
            ("rg needle src/lib.rs", read(file, ShellSpan::Matching)),
            ("ls crates", listing("crates", None)),
            ("ls -la crates", listing("crates", None)),
            ("ls -l -a crates", listing("crates", None)),
            ("find crates -name '*.rs'", listing("crates", Some("*.rs"))),
            (
                "find crates -type f -name '*.rs'",
                listing("crates", Some("*.rs")),
            ),
            ("rg --files", listing(".", None)),
            ("rg --files crates", listing("crates", None)),
        ];
        for (command, expected) in cases {
            assert_eq!(recognise(command).as_ref(), Some(expected), "{command}");
        }
    }

    /// **Every way the grammar composes those forms, and what each one reads.**
    ///
    /// The second half of the same specification: the vocabulary above is a
    /// list of commands, and this is the list of shapes a command is written
    /// in — the three the census in
    /// `rift-agent-claude-code/fixtures/claude-code-tool-field-surface.md`
    /// (O8) found agents actually writing, and no fourth.
    #[test]
    fn every_composition_yields_the_pinned_reads() {
        let file = "src/lib.rs";
        let cases: &[(&str, ShellCall)] = &[
            // ---- the leading `cd`: every path after it is read from there ----
            (
                "cd crates && cat src/lib.rs",
                read("crates/src/lib.rs", ShellSpan::WholeFile),
            ),
            (
                "cd \"my crate\" && sed -n '12,48p' src/lib.rs",
                read("my crate/src/lib.rs", ShellSpan::Lines { lo: 12, hi: 48 }),
            ),
            (
                "cd 'my crate' && cat src/lib.rs",
                read("my crate/src/lib.rs", ShellSpan::WholeFile),
            ),
            (
                "cd /srv/repo && cat src/lib.rs",
                read("/srv/repo/src/lib.rs", ShellSpan::WholeFile),
            ),
            (
                "cd C:/repo && cat src/lib.rs",
                read("C:/repo/src/lib.rs", ShellSpan::WholeFile),
            ),
            // A rooted path is what it says it is; the `cd` told it nothing.
            (
                "cd crates && cat /etc/hosts",
                read("/etc/hosts", ShellSpan::WholeFile),
            ),
            (
                "cd crates && cat C:/repo/src/lib.rs",
                read("C:/repo/src/lib.rs", ShellSpan::WholeFile),
            ),
            ("cd crates && ls src", listing("crates/src", None)),
            ("cd crates && rg --files", listing("crates/.", None)),
            // ---- the trailing narrowing pipe ----
            (
                "cat src/lib.rs | head -n 5",
                ShellCall::Read(vec![narrowed(
                    file,
                    ShellSpan::WholeFile,
                    Narrowing::First(5),
                )]),
            ),
            (
                "cat src/lib.rs | head -5",
                ShellCall::Read(vec![narrowed(
                    file,
                    ShellSpan::WholeFile,
                    Narrowing::First(5),
                )]),
            ),
            (
                "cat src/lib.rs | tail -n 5",
                ShellCall::Read(vec![narrowed(
                    file,
                    ShellSpan::WholeFile,
                    Narrowing::Last(5),
                )]),
            ),
            (
                "cat src/lib.rs | tail -5",
                ShellCall::Read(vec![narrowed(
                    file,
                    ShellSpan::WholeFile,
                    Narrowing::Last(5),
                )]),
            ),
            (
                "sed -n '12,48p' src/lib.rs | head -n 5",
                ShellCall::Read(vec![narrowed(
                    file,
                    ShellSpan::Lines { lo: 12, hi: 48 },
                    Narrowing::First(5),
                )]),
            ),
            (
                "grep needle src/lib.rs | head -n 5",
                ShellCall::Read(vec![narrowed(
                    file,
                    ShellSpan::Matching,
                    Narrowing::First(5),
                )]),
            ),
        ];
        for (command, expected) in cases {
            assert_eq!(recognise(command).as_ref(), Some(expected), "{command}");
        }
    }

    /// **Every chain the grammar joins, and the reads each one is.**
    ///
    /// The third composition, and the one `cat` over several paths that is a
    /// chain without a separator — together with the line that carries all
    /// three compositions at once, because they are defined to compose.
    #[test]
    fn every_chain_yields_the_pinned_reads() {
        let file = "src/lib.rs";
        let cases: &[(&str, ShellCall)] = &[
            (
                "cat src/lib.rs src/main.rs",
                ShellCall::Read(vec![
                    member(file, ShellSpan::WholeFile),
                    member("src/main.rs", ShellSpan::WholeFile),
                ]),
            ),
            (
                "cat src/lib.rs && cat src/main.rs",
                ShellCall::Read(vec![
                    member(file, ShellSpan::WholeFile),
                    member("src/main.rs", ShellSpan::WholeFile),
                ]),
            ),
            (
                "cat src/lib.rs; cat src/main.rs",
                ShellCall::Read(vec![
                    member(file, ShellSpan::WholeFile),
                    member("src/main.rs", ShellSpan::WholeFile),
                ]),
            ),
            // ---- all three compositions on one line ----
            (
                "cd crates && head -n 40 src/lib.rs && sed -n '1,9p' src/main.rs | tail -n 2",
                ShellCall::Read(vec![
                    member("crates/src/lib.rs", ShellSpan::FromTop { limit: Some(40) }),
                    narrowed(
                        "crates/src/main.rs",
                        ShellSpan::Lines { lo: 1, hi: 9 },
                        Narrowing::Last(2),
                    ),
                ]),
            ),
        ];
        for (command, expected) in cases {
            assert_eq!(recognise(command).as_ref(), Some(expected), "{command}");
        }
    }

    /// **Every way a chain labels itself, and the reads it still is.**
    ///
    /// The shape the second census found: reads separated by the `echo` lines
    /// an agent writes to caption its own output. A label contributes no read
    /// and disqualifies nothing, so each of these is exactly the chain it
    /// would have been with the labels struck out — which is what the table
    /// asserts, by pairing every labelled line with that one chain.
    #[test]
    fn a_label_between_reads_changes_nothing_about_the_reads() {
        let plain = ShellCall::Read(vec![
            member("repo/a.rs", ShellSpan::Lines { lo: 1, hi: 80 }),
            member("repo/b.rs", ShellSpan::Lines { lo: 100, hi: 140 }),
        ]);
        for command in [
            // The measured line, in shape.
            "cd \"repo\" && echo \"=== a ===\" && sed -n '1,80p' a.rs && echo && \
             sed -n '100,140p' b.rs",
            // `sed > echo > sed`, with the directory already on the paths.
            "sed -n '1,80p' repo/a.rs && echo --- && sed -n '100,140p' repo/b.rs",
            // `cd > sed > echo > sed`, and a `printf` in the same position.
            "cd repo && sed -n '1,80p' a.rs && printf '%s' next && sed -n '100,140p' b.rs",
            // A label may open the line, close it, or stand twice over.
            "echo start && cd repo && sed -n '1,80p' a.rs && echo && echo && \
             sed -n '100,140p' b.rs && echo done",
            // …and `;` joins a label exactly as `&&` does. (The `cd` still
            // takes `&&` and only `&&`: that is the join that makes the
            // directory a precondition rather than a separate statement.)
            "cd repo && sed -n '1,80p' a.rs; echo; sed -n '100,140p' b.rs",
        ] {
            assert_eq!(recognise(command).as_ref(), Some(&plain), "{command}");
        }
        // A label opening the line is not a `cd` prefix in disguise: the
        // directory is peeled off a `cd` and off nothing else, so a path after
        // one stays exactly as it was written.
        //
        // Quoting is the whole operand rule: a bare word, a single-quoted
        // literal and a double-quoted one are all plain operands, and a label
        // with no operands at all is the blank line between two reads.
        for label in [
            "echo",
            "echo hi",
            "echo === a ===",
            "echo \"=== a ===\"",
            "echo '=== a ==='",
            "printf",
            "printf '%s\\n' one two",
        ] {
            assert_eq!(
                recognise(&format!("{label} && cat src/lib.rs")).as_ref(),
                Some(&read("src/lib.rs", ShellSpan::WholeFile)),
                "{label}"
            );
        }
    }

    /// **A line that only labels itself read nothing, and a label never
    /// carries a read into the grammar on its back.**
    ///
    /// The first half is what keeps the rule additive: a label contributes no
    /// request, so a line made of them contributes none and is not a read at
    /// all. The second is its boundary — an operand a shell would act on, a
    /// command word that merely resembles one of the two, a pipe out of
    /// something that read nothing — and every one of those takes the whole
    /// line out, exactly as an unrecognised member always has.
    #[test]
    fn a_line_that_only_labels_itself_is_not_a_read() {
        for command in [
            // Labels and nothing else.
            "echo hi",
            "echo",
            "printf '%s' hi",
            "cd repo && echo hi",
            "echo one && echo two",
            // An operand a shell would act on is not a plain operand.
            "echo $HOME && cat src/lib.rs",
            "echo \"$HOME\" && cat src/lib.rs",
            "echo `id` && cat src/lib.rs",
            "echo $(id) && cat src/lib.rs",
            "echo hi > out.txt && cat src/lib.rs",
            "echo hi >> out.txt && cat src/lib.rs",
            "echo *.rs && cat src/lib.rs",
            "echo ~ && cat src/lib.rs",
            "echo \"a\\nb\" && cat src/lib.rs",
            "echo a'b' && cat src/lib.rs",
            // A pipe out of a label is a shape nobody has measured.
            "echo hi | head -n 5 && cat src/lib.rs",
            "cat src/lib.rs && echo hi | head -n 5",
            // A near neighbour of the two label commands is not one of them.
            "cat src/lib.rs && print hi",
            "cat src/lib.rs && echoes hi",
            "cat src/lib.rs && /bin/echo hi",
            // A listing still stands alone, labels included.
            "ls crates && echo hi",
            "echo hi && ls crates",
        ] {
            assert!(
                recognise(command).is_none(),
                "{command:?} is not recognised"
            );
        }
    }

    /// **The arithmetic of a narrowed read**, which is the whole of what the
    /// trailing pipe adds: `head` takes a prefix of the run the member
    /// produced and `tail` a suffix of it, and the cell that cannot be written
    /// as one interval of the file — the *first* `N` of the *last* `M`, whose
    /// start only the file's length names — is unknown rather than the nearest
    /// interval that could be written.
    #[test]
    fn a_narrowing_pipe_cuts_the_range_the_member_produced() {
        let file = "src/lib.rs".to_owned();
        let cases: &[(&str, ReadSpan)] = &[
            (
                "cat src/lib.rs | head -n 5",
                ReadSpan::lines(1, HiKind::Explicit(5)),
            ),
            (
                "cat src/lib.rs | tail -n 5",
                ReadSpan::LastLines { count: 5 },
            ),
            // The tighter of the two counts wins, because both are ceilings.
            (
                "head -n 40 src/lib.rs | head -n 5",
                ReadSpan::lines(1, HiKind::Explicit(5)),
            ),
            (
                "head -n 5 src/lib.rs | head -n 40",
                ReadSpan::lines(1, HiKind::Explicit(5)),
            ),
            // The last 5 of the first 40 is a plain interval: 36 through 40.
            (
                "head -n 40 src/lib.rs | tail -n 5",
                ReadSpan::lines(36, HiKind::Explicit(40)),
            ),
            // …and never starts before the first line the member produced.
            (
                "head -n 3 src/lib.rs | tail -n 900",
                ReadSpan::lines(1, HiKind::Explicit(3)),
            ),
            (
                "sed -n '12,48p' src/lib.rs | head -n 5",
                ReadSpan::lines(12, HiKind::Explicit(16)),
            ),
            (
                "sed -n '12,48p' src/lib.rs | tail -n 5",
                ReadSpan::lines(44, HiKind::Explicit(48)),
            ),
            (
                "sed -n '12,48p' src/lib.rs | head -n 900",
                ReadSpan::lines(12, HiKind::Explicit(48)),
            ),
            (
                "tail -n 40 src/lib.rs | tail -n 5",
                ReadSpan::LastLines { count: 5 },
            ),
            (
                "tail -n 5 src/lib.rs | tail -n 40",
                ReadSpan::LastLines { count: 5 },
            ),
        ];
        for (command, expected) in cases {
            assert_eq!(
                read_of(command),
                Some((file.clone(), *expected)),
                "{command}"
            );
        }
        // The one cell with no interval: a count in from the end is a start
        // only the file's length names, and no span here says that.
        assert_eq!(read_of("tail -n 40 src/lib.rs | head -n 5"), None);
        // A member whose own window is unpinned stays unpinned however it is
        // cut: a prefix of an unknown run is still unknown.
        for command in [
            "head src/lib.rs | head -n 5",
            "tail src/lib.rs | tail -n 5",
            "nl src/lib.rs | head -n 5",
            "grep needle src/lib.rs | head -n 5",
        ] {
            assert!(recognise(command).is_some(), "{command} is recognised");
            assert_eq!(read_of(command), None, "{command} grants no coverage");
        }
    }

    /// **A command line that reads several files answers with several reads**,
    /// in the order it performs them — and the members whose lines nobody can
    /// name are in that list too, as unknown spans, because a caller deciding
    /// about the whole line has to see the part it cannot answer for.
    #[test]
    fn a_composed_line_yields_one_request_per_read() {
        assert_eq!(
            reads_of("cd crates && cat a.rs && grep needle b.rs && sed -n '1,9p' c.rs"),
            vec![
                (
                    "crates/a.rs".to_owned(),
                    ReadSpan::lines(1, HiKind::DefaultWindow(HiKind::END_OF_FILE))
                ),
                ("crates/b.rs".to_owned(), ReadSpan::Unknown),
                (
                    "crates/c.rs".to_owned(),
                    ReadSpan::lines(1, HiKind::Explicit(9))
                ),
            ]
        );
        // The singular answer is the first read that supports coverage, which
        // for a one-command line is the only one it ever was.
        assert_eq!(
            read_of("cd crates && grep needle b.rs && sed -n '1,9p' c.rs"),
            Some((
                "crates/c.rs".to_owned(),
                ReadSpan::lines(1, HiKind::Explicit(9))
            ))
        );
        // A listing performs no reads at all, chained or not.
        assert!(reads_of("ls crates").is_empty());
    }

    /// A chain longer than the cap is not read-shaped: the cap bounds both the
    /// work a hostile line can ask for and the length of the sentence a
    /// decline of it would have to render.
    #[test]
    fn a_chain_past_the_cap_is_left_alone() {
        let paths: Vec<String> = (0..MAX_CHAIN_READS).map(|n| format!("f{n}.rs")).collect();
        let fits = format!("cat {}", paths.join(" "));
        assert_eq!(reads_of(&fits).len(), MAX_CHAIN_READS);
        let over = format!("{fits} f.rs");
        assert!(recognise(&over).is_none(), "one read past the cap");
        let chained = (0..=MAX_CHAIN_READS)
            .map(|n| format!("cat f{n}.rs"))
            .collect::<Vec<_>>()
            .join(" && ");
        assert!(recognise(&chained).is_none(), "one member past the cap");
    }

    /// The ranges those spans support, which is what a coverage record is
    /// written from — one line where the end is unpinned, the counted line
    /// where `-n` or a `sed` script pins it.
    #[test]
    fn a_pinned_span_yields_the_range_a_record_is_written_from() {
        let file = "src/lib.rs".to_owned();
        assert_eq!(
            read_of("cat src/lib.rs"),
            Some((
                file.clone(),
                ReadSpan::lines(1, HiKind::DefaultWindow(HiKind::END_OF_FILE))
            ))
        );
        assert_eq!(
            read_of("head -n 40 src/lib.rs"),
            Some((file.clone(), ReadSpan::lines(1, HiKind::Explicit(40))))
        );
        // A *counted* `tail` is an exact range whose line numbers only the
        // file's length names, so the count travels and the step that holds
        // the bytes resolves it.
        assert_eq!(
            read_of("tail -n 40 src/lib.rs"),
            Some((file.clone(), ReadSpan::LastLines { count: 40 }))
        );
        assert_eq!(
            read_of("sed -n '12,48p' src/lib.rs"),
            Some((file, ReadSpan::lines(12, HiKind::Explicit(48))))
        );
        // A decorated print and a default window support no record at all, so
        // there is no request to write one from.
        for command in ["nl src/lib.rs", "head src/lib.rs", "tail src/lib.rs"] {
            assert_eq!(read_of(command), None, "{command}");
            assert_eq!(
                reads_of(command),
                vec![("src/lib.rs".to_owned(), ReadSpan::Unknown)],
                "{command} is still a read of that file"
            );
        }
    }

    /// A content search names its file and reads none of it: the grammar is
    /// what names the path, and `tail` — the other command whose lines nobody
    /// can state — is not a search and names nothing.
    #[test]
    fn a_content_search_names_the_one_file_it_searched() {
        for command in [
            "grep needle src/lib.rs",
            "egrep 'a|b' src/lib.rs",
            "fgrep needle src/lib.rs",
            "rg needle src/lib.rs",
        ] {
            assert_eq!(
                recognise(command).and_then(|call| call.searched_file().map(str::to_owned)),
                Some("src/lib.rs".to_owned()),
                "{command}"
            );
        }
        for command in [
            "tail -n 40 src/lib.rs",
            "cat src/lib.rs",
            "sed -n '12,48p' src/lib.rs",
            "ls crates",
        ] {
            assert_eq!(
                recognise(command).and_then(|call| call.searched_file().map(str::to_owned)),
                None,
                "{command}"
            );
        }
    }

    /// **A command whose lines depend on the file grants no coverage.**
    ///
    /// `grep` prints what matched and a `tail` with no count prints however
    /// much its own default is; neither position follows from the command
    /// line. Recording the line the file starts on would say the window holds
    /// line one — the line those are least likely to have printed — and a
    /// later request for it could then be declined against text nobody was
    /// ever handed.
    ///
    /// A **counted** `tail` is deliberately not on this list any more. Its
    /// range is exactly as pinned as `head -n N`'s; what it lacked was a way
    /// to say "N lines, ending at the last one" to a step that had not yet
    /// opened the file, and [`ReadSpan::LastLines`] is that way.
    #[test]
    fn a_read_whose_lines_depend_on_the_file_grants_no_coverage() {
        for command in [
            "tail src/lib.rs",
            "head src/lib.rs",
            "nl src/lib.rs",
            "grep needle src/lib.rs",
            "rg needle src/lib.rs",
        ] {
            assert!(recognise(command).is_some(), "{command} is recognised");
            assert!(read_of(command).is_none(), "{command} grants no coverage");
        }
    }

    /// **A content search is never a listing**, however it is spelled — the
    /// one substitution that would answer a question about content with an
    /// answer about names.
    #[test]
    fn a_content_search_is_never_a_listing() {
        for command in [
            "grep needle src/lib.rs",
            "egrep needle src/lib.rs",
            "fgrep needle src/lib.rs",
            "rg needle src/lib.rs",
        ] {
            assert!(
                recognise(command)
                    .and_then(|call| call.search_request())
                    .is_none(),
                "{command}"
            );
        }
        // A directory argument, or a recursive flag, is not a listing either:
        // it is simply not in the grammar.
        for command in [
            "grep -r needle crates",
            "grep needle crates",
            "rg needle crates -r",
        ] {
            let call = recognise(command);
            assert!(
                call.as_ref().and_then(ShellCall::search_request).is_none(),
                "{command}"
            );
        }
        let listing = recognise("ls crates").and_then(|c| c.search_request());
        assert_eq!(
            listing.map(|s| s.kind),
            Some(SearchKind::Listing {
                dir: "crates".to_owned(),
                pattern: None
            })
        );
    }

    /// The shapes a shell would act on, each one refused.
    ///
    /// They are listed rather than generated so that a reader can see the
    /// class each stands for; the property test beside this one covers the
    /// space between them.
    #[test]
    fn a_shell_that_would_do_more_than_read_is_left_alone() {
        for command in [
            "cat src/lib.rs; rm -rf /",
            "cat src/lib.rs > /tmp/out",
            "cat < src/lib.rs",
            "cat $FILE",
            "cat `echo src/lib.rs`",
            "cat $(echo src/lib.rs)",
            "cat ~/src/lib.rs",
            "cat src/*.rs",
            "cat src/lib.rs\nrm -rf /",
            "FOO=bar cat src/lib.rs",
            "sudo cat /etc/shadow",
            "xargs cat",
            "cat -n src/lib.rs",
            "head -5 src/lib.rs",
            "head -n0 src/lib.rs",
            "head -n 0 src/lib.rs",
            "sed -n '12,48d' src/lib.rs",
            "sed -n '48,12p' src/lib.rs",
            "sed -n '0,4p' src/lib.rs",
            "sed '12,48p' src/lib.rs",
            "sed -i s/a/b/ src/lib.rs",
            "cat 'src/lib.rs'",
            "cat \"src/$HOME\"",
            "cat src/lib.rs'",
            "cat 'src/lib.rs",
            "cat a'b'",
            "",
            "   ",
            "cat",
        ] {
            assert!(
                recognise(command).is_none(),
                "{command:?} is not recognised"
            );
        }
    }

    /// **The compositions stop exactly where they are spelled.**
    ///
    /// Each line here is one step outside one of the three rules — a pipe into
    /// something that is not a narrowing, a member that is not a read, a `cd`
    /// that is not the only one or not at the front, a separator the grammar
    /// does not join with — and every one of them takes the whole command line
    /// out of the grammar rather than costing only the part that stepped out.
    #[test]
    fn a_composition_the_grammar_does_not_spell_is_left_alone() {
        for command in [
            // A pipe target that is not one of the two narrowing commands.
            "cat src/lib.rs | grep needle",
            "cat src/lib.rs | wc -l",
            "cat src/lib.rs | less",
            "cat src/lib.rs | cat",
            // A narrowing with no count, or one that is not a count.
            "cat src/lib.rs | head",
            "cat src/lib.rs | head -n",
            "cat src/lib.rs | head -n0",
            "cat src/lib.rs | head -0",
            "cat src/lib.rs | head -n x",
            "cat src/lib.rs | tail -n -5",
            // One narrowing, not two, and never on a member that is several
            // reads or a listing.
            "cat src/lib.rs | head -n 20 | tail -n 5",
            "cat a.rs b.rs | head -n 5",
            "ls crates | head -n 5",
            "cat src/lib.rs |",
            "| cat src/lib.rs",
            "cat src/lib.rs || cat src/main.rs",
            // A member that is not a read takes the line with it.
            "cat src/lib.rs && cargo build",
            "cat src/lib.rs && touch done",
            "cd crates && cargo test",
            "cat src/lib.rs &&",
            "cat src/lib.rs ;",
            "cat src/lib.rs && && cat src/main.rs",
            // A `cd` is one, and it is first.
            "cd crates",
            "cd crates && cd rift-core && cat src/lib.rs",
            "cat src/lib.rs && cd crates && cat src/main.rs",
            "cd crates; cat src/lib.rs",
            "cd && cat src/lib.rs",
            "cd -P crates && cat src/lib.rs",
            "cd crates other && cat src/lib.rs",
            "cd $HOME && cat src/lib.rs",
            "cd \"$HOME\" && cat src/lib.rs",
            // A background job is not a chain, whatever it looks like.
            "cat src/lib.rs & cat src/main.rs",
            "cat src/lib.rs &",
            // A listing never stands among reads.
            "cat src/lib.rs && ls crates",
            "ls crates && ls bins",
        ] {
            assert!(
                recognise(command).is_none(),
                "{command:?} is not recognised"
            );
        }
    }

    /// A separator inside a quoted pattern is data, not punctuation — the one
    /// place widening the grammar could have quietly cut a recognised line in
    /// half.
    #[test]
    fn a_separator_inside_a_quote_is_part_of_the_pattern() {
        for command in [
            "egrep 'a|b' src/lib.rs",
            "egrep 'a&&b' src/lib.rs",
            "egrep 'a;b' src/lib.rs",
            "grep \"a|b\" src/lib.rs",
        ] {
            assert_eq!(
                recognise(command).as_ref(),
                Some(&read("src/lib.rs", ShellSpan::Matching)),
                "{command}"
            );
        }
        assert_eq!(
            recognise("find crates -name 'a|b.rs'").as_ref(),
            Some(&listing("crates", Some("a|b.rs")))
        );
    }

    /// A command line longer than the cap is refused on its length, before
    /// anything in it is classified.
    #[test]
    fn an_oversized_command_line_is_refused_on_its_length() {
        let long = format!("cat {}", "a".repeat(MAX_COMMAND_BYTES));
        assert!(recognise(&long).is_none());
        let fits = format!("cat {}", "a".repeat(MAX_COMMAND_BYTES - 4));
        assert!(recognise(&fits).is_some());
    }
}

#[cfg(test)]
mod properties {
    use super::{recognise, ShellCall};

    /// The command words the grammar admits, spelled once for the property to
    /// check a recognition against. `cd` is here because a line may *open*
    /// with one, and nowhere else.
    const COMMAND_WORDS: &[&str] = &[
        "cat", "nl", "bat", "batcat", "head", "tail", "sed", "grep", "egrep", "fgrep", "rg", "ls",
        "find", "cd", "echo", "printf",
    ];

    /// **A second, independent reading of "the shell would act on this".**
    ///
    /// Deliberately not the recogniser's own logic: a property checked with
    /// the code under test is a tautology. This walks quotes and looks for the
    /// characters that give a shell its power, and it also refuses a quote
    /// that never closes.
    ///
    /// `|`, `;` and `&` left this list when the grammar learned to compose,
    /// and nothing else did. What still holds them shut is narrower and is
    /// checked in the same property: a `|` may only lead into a narrowing
    /// command, a `;` and a `&&` may only join reads, and a lone `&` is a
    /// character no word may hold, so it reaches no reading at all.
    fn shell_would_act(command: &str) -> bool {
        const ACTIVE: &str = "<>$`\\(){}[]*?~!#\n\r";
        let mut open: Option<char> = None;
        for c in command.chars() {
            match open {
                Some(quote) if c == quote => open = None,
                Some(_) => {}
                None => {
                    if c == '\'' || c == '"' {
                        open = Some(c);
                    } else if ACTIVE.contains(c) {
                        return true;
                    }
                }
            }
        }
        open.is_some()
    }

    /// Every path or directory a recognised call reports.
    fn targets(call: &ShellCall) -> Vec<String> {
        match call {
            ShellCall::Read(reads) => reads.iter().map(|read| read.path.clone()).collect(),
            ShellCall::Listing(listing) => vec![listing.dir.clone()],
        }
    }

    /// **Whether a reported target is made of names the line actually wrote.**
    ///
    /// It is either a word of the line, or one word under the directory
    /// another word named — which is the only thing a leading `cd` may do to a
    /// path, and the only reason a target is ever not a word verbatim. The
    /// join is checked on the text rather than trusted: a target whose last
    /// component is not a word on the line is a component this recogniser
    /// invented, which is the failure the whole property exists to catch.
    ///
    /// `rg --files` is the one form whose directory is implied rather than
    /// written, and the implication is the tool's own.
    fn names_a_word(target: &str, words: &[&str]) -> bool {
        let under = |name: &str| {
            target
                .strip_suffix(name)
                .is_some_and(|prefix| prefix.ends_with('/'))
        };
        target == "." || under(".") || words.iter().any(|word| *word == target || under(word))
    }

    /// The pieces a shell-ish line is built from: words of the grammar,
    /// options it does and does not admit, paths, patterns, and the
    /// punctuation that ends a recognition.
    fn token() -> impl proptest::strategy::Strategy<Value = String> {
        let fixed: Vec<String> = [
            "cat",
            "head",
            "tail",
            "sed",
            "grep",
            "rg",
            "ls",
            "ls -R crates",
            "ls crates other",
            "find crates",
            "find crates -type d -name x",
            "find crates -name '*.rs' -type f",
            "rg --files-with-matches needle",
            "rg --files crates other",
            "find",
            "sudo",
            "xargs",
            "wc",
            "-n",
            "-l",
            "-la",
            "-r",
            "-type",
            "f",
            "-name",
            "--files",
            "5",
            "0",
            "12,48p",
            "'12,48p'",
            "'*.rs'",
            "src/lib.rs",
            "crates",
            ".",
            "..",
            "/etc/passwd",
            "~/notes.md",
            "$FILE",
            "*.rs",
            "|",
            ";",
            "&&",
            "&",
            "||",
            ">",
            "<",
            "`id`",
            "$(id)",
            "FOO=bar",
            "\"quoted\"",
            "'a|b'",
            // The composition alphabet: a `cd` and its directory, the two
            // narrowing spellings, and the near misses beside each.
            "cd",
            "cd crates",
            "cd \"my crate\"",
            "cd crates other",
            "| head -n 5",
            "| head -5",
            "| tail -n 5",
            "| tail -5",
            "| head",
            "| wc -l",
            "-5",
            "-0",
            "&& cat src/lib.rs",
            "&& rm -rf /",
            "; cat src/main.rs",
            "cat a.rs b.rs",
            // The label alphabet, and the operands that are not plain ones.
            "echo",
            "printf",
            "echo hi",
            "echo \"=== a ===\"",
            "echo '=== a ==='",
            "printf '%s\\n' one",
            "&& echo",
            "&& echo \"=== b ===\"",
            "; echo",
            "echo $HOME",
            "echo hi > out.txt",
            "echoes",
            "print",
        ]
        .iter()
        .map(|t| (*t).to_owned())
        .collect();
        proptest::prop_oneof![
            8 => proptest::sample::select(fixed),
            1 => proptest::string::string_regex("(?s).{0,12}").expect("a valid generator"),
        ]
    }

    proptest::proptest! {
        /// ★ Anything outside the grammar is not a read and not a listing.
        ///
        /// The recogniser's whole safety argument is that a non-match is left
        /// alone, so the property is stated in the direction that matters: a
        /// line a shell would do anything with — a pipeline, a sequence, a
        /// redirection, a substitution, a variable, a glob outside a quoted
        /// pattern, an unclosed quote — is never recognised. What it does
        /// recognise is checked in the other direction too: the first word is
        /// one of the published commands, and the path it reports is a word
        /// that was actually on the line.
        #[test]
        fn anything_outside_the_grammar_is_not_a_read_or_listing(
            tokens in proptest::collection::vec(token(), 1..6),
        ) {
            let command = tokens.join(" ");
            let call = recognise(&command);
            if shell_would_act(&command) {
                proptest::prop_assert!(
                    call.is_none(),
                    "a shell would act on {command:?}, so it is not a read"
                );
                return Ok(());
            }
            let Some(call) = call else { return Ok(()) };
            let words: Vec<&str> = command.split_whitespace().collect();
            proptest::prop_assert!(
                words.first().is_some_and(|w| COMMAND_WORDS.contains(w)),
                "{command:?} was recognised but starts with no published command"
            );
            for target in targets(&call) {
                proptest::prop_assert!(
                    names_a_word(&target, &words),
                    "{command:?} was read as naming {target:?}, which no word on it names"
                );
            }
            proptest::prop_assert!(
                targets(&call).len() <= words.len(),
                "{command:?} was read as more reads than it has words"
            );
        }

        /// ★ A recognised command line with anything appended to it is either
        /// still one of the grammar's own forms or nothing at all — never a
        /// read of the file it used to name.
        ///
        /// This is the mutation half: the grammar's forms are short, so the
        /// dangerous mistake is a recogniser that ignores a trailing word and
        /// keeps the reading it had.
        #[test]
        fn a_recognised_line_with_a_word_appended_is_re_read_from_scratch(
            base in proptest::sample::select(vec![
                "cat src/lib.rs",
                "head -n 40 src/lib.rs",
                "sed -n '12,48p' src/lib.rs",
                "grep needle src/lib.rs",
                "ls crates",
                "find crates -name '*.rs'",
                "rg --files",
                "cd crates && echo hi && cat src/lib.rs",
                "cat src/lib.rs && echo",
            ]),
            extra in token(),
        ) {
            let command = format!("{base} {extra}");
            if let Some(call) = recognise(&command) {
                // The grammar's own splitter: a word ends at a space or a tab,
                // so a non-ASCII separator inside a word stays part of it.
                let words: Vec<&str> = command
                    .split([' ', '\t'])
                    .filter(|w| !w.is_empty())
                    .collect();
                for target in targets(&call) {
                    proptest::prop_assert!(
                        names_a_word(&target, &words),
                        "{command:?} kept a target no word on it names"
                    );
                }
            }
        }
    }
}
