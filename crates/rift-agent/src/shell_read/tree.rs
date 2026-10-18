//! A command line parsed into the tree a shell groups it by, and the reads it
//! performs composed over that tree.
//!
//! The grammar used to be a list of shapes, each one a rule measured before
//! it was admitted, and the census of what it refused showed why that stops
//! paying: about fifty refused shapes, the largest one an eighth of the
//! tokens, and no single new rule closing the gap. So the line is now parsed
//! — with the grammar's own quote-aware lexer, not a shell parser, and into
//! only the two levels this subset has — and its effect is composed node by
//! node. A **sequence** is pipelines joined by `&&` or `;`, and its reads are
//! the union of its members' reads. A **pipeline** is stages joined by `|`:
//! its first stage is the published vocabulary, and each later stage acts on
//! the lines the one before it produced.
//!
//! A pipeline stage after the first is one of two things, and anything else
//! takes the whole line out of the grammar (rule 1). A **narrowing** (`head
//! -N`, `tail -N`, `sed -n 'A,Bp'`) on exactly one read keeps the span exact:
//! it is arithmetic on a run of lines the command line names. A **filter**
//! (`filter.rs`) keeps the lines a pattern or a sort chose, which no
//! interval of the file names. Every read of a pipeline that reaches a
//! filter, or more than one narrowing, or a narrowing over several reads
//! joined together, is [`ShellSpan::Filtered`]: still counted as a read,
//! because the shell opened that file and its tokens were spent, and never
//! coverage, because nothing here can say which lines arrived (rule 2). A
//! listing or a label piped anywhere is refused, so a question about names
//! never flows into a decision about content (rule 3).
//!
//! A sequence also says which members are sure to run. A `;` runs what
//! follows it whatever came before. An `&&` runs it only when the member
//! before succeeded, and a search — the `grep` family, `rg`, anywhere in a
//! pipeline — fails quietly when nothing matched, so every member after one
//! in the same `&&` run may never have run. A `||` binds as tightly as `&&`,
//! so `a || b && c` is `(a || b) && c`, and nothing after the first `||` of
//! a run is sure either. Each of those reads is [`ShellSpan::MayNotRun`]:
//! counted, so a caller weighing the line sees it, and never coverage.
//!
//! Only output that reaches the agent costs anything, so a stage inside a
//! pipeline is never a unit of its own: the pipeline as a whole is.

use super::filter::is_filter;
use super::{
    bare, cd_dir, command_name, narrowing, recognise_simple, split_on, under, words, Member,
    Narrowing, ShellCall, ShellListing, ShellRead, ShellSpan, MAX_CHAIN_READS, MAX_COMMAND_BYTES,
};

/// The commands that exit non-zero, with nothing gone wrong, when they found
/// nothing — which stops an `&&` run as surely as an error does.
const QUIET_FAILURES: &[&str] = &["grep", "egrep", "fgrep", "rg"];

/// A command line's body as the shell groups it: pipelines joined by `&&`,
/// `||` or `;`, in the order they run.
struct Sequence<'a> {
    steps: Vec<Step<'a>>,
    /// Whether any member was joined by `||`.
    fallback: bool,
}

/// One member of a sequence, and whether the line is sure to reach it.
struct Step<'a> {
    pipeline: Pipeline<'a>,
    sure: bool,
}

/// One pipeline: the text of each simple command a `|` joins.
struct Pipeline<'a> {
    stages: Vec<&'a str>,
}

/// What a stage after the first does to the lines reaching it.
#[derive(Clone, Copy)]
enum Downstream {
    /// Keeps a run of them the command line names.
    Narrow(Narrowing),
    /// Keeps or reshapes them in a way the command line does not pin.
    Filter,
}

impl<'a> Sequence<'a> {
    /// Parse a body into its pipelines. Never fails: a member the walk below
    /// cannot read is refused there, where its reason is known.
    fn parse(body: &'a str) -> Self {
        let mut steps = Vec::new();
        let mut fallback = false;
        for run in split_on(body, &[";"], usize::MAX) {
            let mut sure = true;
            for (at, alternative) in split_on(run, &["||"], usize::MAX).into_iter().enumerate() {
                // Past a run's first `||`, a member runs or not by whether
                // something before it failed, which no command line says.
                fallback |= at > 0;
                sure &= at == 0;
                for member in split_on(alternative, &["&&"], usize::MAX) {
                    let pipeline = Pipeline::parse(member);
                    let quiet = pipeline.may_fail_quietly();
                    steps.push(Step { pipeline, sure });
                    sure &= !quiet;
                }
            }
        }
        Self { steps, fallback }
    }

    /// The call this sequence is, its relative paths under `dir`.
    fn compose(&self, dir: Option<&str>) -> Option<ShellCall> {
        // A `cd` that failed leaves the shell where it was, and a `||` after
        // it would then read a fallback the paths below misplace.
        if dir.is_some() && self.fallback {
            return None;
        }
        let mut reads: Vec<ShellRead> = Vec::new();
        for step in &self.steps {
            match step.pipeline.effect()? {
                Member::Reads(found) if step.sure => reads.extend(found),
                Member::Reads(found) => reads.extend(found.into_iter().map(|read| ShellRead {
                    span: ShellSpan::MayNotRun,
                    narrowing: None,
                    ..read
                })),
                // A label read no file, so it adds no request, and it is no
                // reason to leave the rest of the line unaccounted for.
                Member::Label => {}
                // A listing stands alone. Beside anything else — a label
                // included, a shape nobody has measured — it would put a
                // question about names inside a decision about content, which
                // rule 3 exists to prevent.
                Member::Listing(listing) if self.steps.len() == 1 => {
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
        let reads = reads
            .into_iter()
            .map(|read| ShellRead {
                path: under(dir, read.path),
                ..read
            })
            .collect();
        Some(ShellCall::Read(reads))
    }
}

impl<'a> Pipeline<'a> {
    /// Split a member at its pipes. An empty stage — `a |`, `| a`, the middle
    /// of `||` — stays in, and is refused where every stage is read.
    fn parse(member: &'a str) -> Self {
        Self {
            stages: split_on(member, &["|"], usize::MAX),
        }
    }

    /// Whether this pipeline can exit non-zero with nothing gone wrong. Any
    /// stage counts, so the answer holds with or without `pipefail`.
    fn may_fail_quietly(&self) -> bool {
        self.stages.iter().any(|stage| {
            words(stage).is_some_and(|words| {
                words
                    .first()
                    .and_then(bare)
                    .is_some_and(|head| QUIET_FAILURES.contains(&command_name(head)))
            })
        })
    }

    /// What this pipeline did, or [`None`] when a stage of it is outside the
    /// grammar.
    fn effect(&self) -> Option<Member> {
        let (source, rest) = self.stages.split_first()?;
        let member = recognise_simple(source)?;
        let downstream = rest
            .iter()
            .map(|stage| Downstream::of(stage))
            .collect::<Option<Vec<_>>>()?;
        match (member, downstream.as_slice()) {
            (member, []) => Some(member),
            (Member::Reads(reads), downstream) => Some(Member::Reads(through(reads, downstream))),
            // Rule 3 for a listing, and for a label the narrower reason that a
            // pipe out of something that read nothing is a shape nobody has
            // measured.
            (Member::Listing(_) | Member::Label, _) => None,
        }
    }
}

impl Downstream {
    /// The stage `text` is, or [`None`] when it is neither kind.
    fn of(text: &str) -> Option<Self> {
        if let Some(narrow) = narrowing(text) {
            return Some(Self::Narrow(narrow));
        }
        is_filter(&words(text)?).then_some(Self::Filter)
    }
}

/// The reads of a pipeline's first stage, as the stages after it left them.
///
/// One narrowing on one read is the arithmetic [`ShellRead::request`]
/// already does. Every other combination is filtered: a narrowing over two
/// files cuts their lines *joined*, which is a run of neither; two narrowings
/// compose into a range this slice does not yet name; and a filter keeps
/// lines no interval names at all.
fn through(mut reads: Vec<ShellRead>, downstream: &[Downstream]) -> Vec<ShellRead> {
    if let ([read], [Downstream::Narrow(narrowing)]) = (reads.as_mut_slice(), downstream) {
        read.narrowing = Some(*narrowing);
        return reads;
    }
    for read in &mut reads {
        read.span = ShellSpan::Filtered;
        read.narrowing = None;
    }
    reads
}

/// Peel the optional leading `cd <dir> &&` or `cd <dir>;`, and say what is
/// left of the line.
///
/// `&&` makes the directory a precondition of the reads. A `;` does not — the
/// reads run even when the `cd` fails — and it is admitted all the same,
/// because a `cd` fails only where its directory cannot be entered, and then
/// every relative path put under that directory names a file nobody can open
/// either: the caller that resolves it finds nothing and records nothing.
/// Where the `cd` succeeds the two joins deliver the same lines. So the
/// difference costs at most a saving and never makes a claim, and a rooted
/// path is read the same either way. `||`, a background `&` and a line break
/// are not peeled.
///
/// Labels in front of the `cd` are stepped over, because a label is a member
/// that does nothing and a rule that said otherwise here would mean an `echo`
/// at the head of a line silently cost every path after it its directory.
/// The whole line comes back untouched when no `cd` is found, labels
/// included, so an unpeeled label is still an ordinary member.
fn cd_prefix(command: &str) -> (Option<String>, &str) {
    let mut rest = command;
    loop {
        let split = split_on(rest, &["&&", ";"], 1);
        let [head, tail] = split.as_slice() else {
            return (None, command);
        };
        if let Some(dir) = cd_dir(head) {
            return (Some(dir), tail);
        }
        if !matches!(recognise_simple(head), Some(Member::Label)) {
            return (None, command);
        }
        // Each step consumes one join, so this walks the line once at most.
        rest = tail;
    }
}

/// Recognise one command line, or leave it alone.
///
/// The only entry point. The line is parsed into a sequence of pipelines and
/// its reads composed over them (see the module documentation): a leading
/// `cd <dir> &&` or `cd <dir>;` puts every relative path under its
/// directory, members joined by `&&` or `;` contribute their reads in order
/// and labels contribute none, a member the line may never reach is read as
/// [`ShellSpan::MayNotRun`], and a pipeline's later stages narrow or filter
/// what its first stage read.
///
/// Everything else is what it always was — no redirection, no substitution,
/// no variable, no glob outside a pattern argument, no environment
/// assignment, no `sudo`, no `xargs`, no background `&`, no `||` after a
/// `cd`, no second `cd`, no `cd` anywhere but the front, no listing mixed in among reads or
/// piped onward — and every one of those is a character, a word or a member
/// the walk never accepts.
#[must_use]
pub fn recognise(command: &str) -> Option<ShellCall> {
    if command.len() > MAX_COMMAND_BYTES {
        return None;
    }
    let (dir, body) = cd_prefix(command);
    Sequence::parse(body).compose(dir.as_deref())
}

/// The directory a recognised line's leading `cd` moved to, exactly as the
/// line spelled it.
///
/// [`recognise`] folds that directory into every path it returns, which is
/// what a read needs; the checkout an agent read *in* needs the directory
/// itself, because a read path may be absolute and say nothing about where the
/// shell stood. Only a line the grammar recognises answers, so a `cd` in front
/// of a command nothing here can read never moves an observation.
#[must_use]
pub fn leading_cd(command: &str) -> Option<String> {
    recognise(command)?;
    cd_prefix(command).0
}
