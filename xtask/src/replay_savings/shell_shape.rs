//! The census of the shell reads the grammar refused: which shape each one
//! took, by calls and tokens, so the grammar is widened where the tokens are.
//!
//! A shape is decided from the command line alone, never from the bytes a
//! result carried, so two runs over one corpus agree and a shape cannot move
//! because a file grew. The vocabulary is closed: every name a call can be
//! counted under is spelled in [`Shape::key`], and a line whose reason cannot
//! be told without a real shell parse is `other` rather than a guess.
//!
//! The grammar itself is only ever asked, through its one public entry point,
//! [`rift_agent::shell_read::recognise`]; nothing here widens or restates what
//! it admits. A part of a refused line the grammar accepts on its own is how
//! the census finds the part it did not.

use std::collections::BTreeMap;

use rift_agent::shell_read::{recognise, ShellCall};

use super::report::Tally;
use super::shell_lex::{lex, Lexed, Member, Stage, Word};

/// Why the grammar refused a shell call that looks like a read.
///
/// Declared in the order a tie between two shapes is broken in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Shape {
    /// `cat -n`.
    CatNumbered,
    /// `cat` with any other option.
    CatOption,
    /// `nl` or `bat` with an option.
    DecoratedOption,
    /// A numbered print (`cat -n`, `nl …`) piped into a range or a count.
    NumberedLinesPiped,
    /// `head` or `tail` with a flag the grammar does not pin (`-20`, `-c`,
    /// `-n +N`, `-f`).
    HeadTailOption,
    /// `sed -n` with a script other than one `A,Bp`.
    SedScript,
    /// `sed` without `-n`, or with an option.
    SedOption,
    /// Several paths to one `sed -n`, `head`, `tail` or `nl`.
    SeveralPaths,
    /// `grep` or `rg` with a flag.
    SearchFlags,
    /// `grep` or `rg` over several paths.
    SearchSeveralPaths,
    /// `git show <rev>:<path>`.
    GitShowRevPath,
    /// `git show` of a commit.
    GitShow,
    /// `git diff`, with or without a path.
    GitDiff,
    /// `git log -p` or `git blame`.
    GitLogOrBlame,
    /// A recognised read piped into a content filter.
    PipedIntoFilter(Filter),
    /// A recognised read piped into `head` or `tail` in a form not pinned.
    PipedIntoHeadTail,
    /// A chain with a member that is neither a read nor a label.
    ChainMember(Family),
    /// A command's output that is not a read, piped into a reader.
    PipedFrom(Family),
    /// A chain every member of which the grammar reads alone, refused for how
    /// they are joined (`||`, a background `&`, a `cd` after the front, a
    /// listing among reads, more members than the cap).
    ChainComposition,
    /// A redirection or `2>&1` on a line that is otherwise recognised.
    Redirect,
    /// An expansion, a glob, a `~`, a backslash or a quote the grammar does
    /// not pin.
    QuotingOrShellCharacter,
    /// A here-document fed to anything but an interpreter.
    Heredoc,
    /// An environment assignment or a wrapper (`sudo`, `xargs`, `env`,
    /// `timeout`) in front of the command.
    Wrapper,
    /// `awk`, `perl`, or an interpreter's inline script reading a file.
    ScriptInterpreter,
    /// `wc`, `stat`, `file`, `du`: sizes, not content.
    SizeOnly,
    /// A pager or a byte dump: `less`, `more`, `xxd`, `od`, `hexdump`.
    PagerOrDump,
    /// Anything the reasons above do not name.
    Other,
}

/// The content filter a recognised read was piped into.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Filter {
    /// `grep`, `egrep`, `fgrep`, `rg`.
    Grep,
    /// `sed`.
    Sed,
    /// `awk`.
    Awk,
    /// `wc`.
    Wc,
    /// `sort`, `uniq`.
    Sort,
    /// `cut`, `tr`, `column`.
    Cut,
    /// Any other command.
    Other,
}

/// The family of a command that is not a read.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Family {
    /// `git`, other than the reading subcommands.
    Git,
    /// `cargo`.
    Cargo,
    /// A build or run tool: `npm`, `make`, `go`, `python` running a script.
    Build,
    /// `ls`, `find`, `tree`, `fd`.
    Listing,
    /// `echo` or `printf` the grammar could not take as a label.
    Label,
    /// A loop, a conditional or a test.
    ControlFlow,
    /// Rift's own binaries.
    Rift,
    /// `mkdir`, `rm`, `cp`, `mv`, `touch`.
    FileOps,
    /// A shell builtin, a variable assignment or a small utility: `cd`,
    /// `export`, `pwd`, `sleep`, `ps`.
    Shell,
    /// A content search the vocabulary does not spell: `ggrep`, `ag`, `ack`.
    Search,
    /// A command spelled through an expansion, so it cannot be named.
    Expansion,
    /// Any other command.
    Other,
}

/// One shape's calls: every one counted on the loose bound, and the part of
/// them naming a file this checkout has.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ShapeRow {
    /// Every call on the loose bound with this shape.
    pub all: Tally,
    /// Of those, the calls naming a work-tree file.
    pub named: Tally,
}

/// The commands the grammar's vocabulary is spelled with.
const VOCABULARY: [&str; 11] = [
    "cat", "nl", "bat", "batcat", "head", "tail", "sed", "grep", "egrep", "fgrep", "rg",
];

/// The commands that put another command behind them.
const WRAPPERS: [&str; 8] = [
    "sudo", "xargs", "env", "timeout", "time", "nice", "nohup", "command",
];

/// The interpreters that read a file only through an inline script.
const INLINE_INTERPRETERS: [&str; 5] = ["python", "python3", "ruby", "node", "perl"];

impl Shape {
    /// The name this shape is published under, and the only name of it that
    /// reaches an output line.
    #[must_use]
    pub fn key(self) -> String {
        let fixed = match self {
            Self::PipedIntoFilter(filter) => return format!("piped_into_filter:{}", filter.key()),
            Self::ChainMember(family) => return format!("chain_member:{}", family.key()),
            Self::PipedFrom(family) => return format!("piped_from:{}", family.key()),
            Self::CatNumbered => "cat_numbered",
            Self::CatOption => "cat_option",
            Self::DecoratedOption => "nl_or_bat_option",
            Self::NumberedLinesPiped => "numbered_print_piped",
            Self::HeadTailOption => "head_tail_option",
            Self::SedScript => "sed_script",
            Self::SedOption => "sed_option",
            Self::SeveralPaths => "several_paths",
            Self::SearchFlags => "search_flags",
            Self::SearchSeveralPaths => "search_several_paths",
            Self::GitShowRevPath => "git_show_rev_path",
            Self::GitShow => "git_show",
            Self::GitDiff => "git_diff",
            Self::GitLogOrBlame => "git_log_patch_or_blame",
            Self::PipedIntoHeadTail => "piped_into_head_tail",
            Self::ChainComposition => "chain_composition",
            Self::Redirect => "redirect",
            Self::QuotingOrShellCharacter => "quoting_or_shell_character",
            Self::Heredoc => "heredoc",
            Self::Wrapper => "wrapper",
            Self::ScriptInterpreter => "script_interpreter",
            Self::SizeOnly => "size_only",
            Self::PagerOrDump => "pager_or_dump",
            Self::Other => "other",
        };
        fixed.to_owned()
    }
}

impl Filter {
    /// The filter's published name.
    const fn key(self) -> &'static str {
        match self {
            Self::Grep => "grep",
            Self::Sed => "sed",
            Self::Awk => "awk",
            Self::Wc => "wc",
            Self::Sort => "sort",
            Self::Cut => "cut",
            Self::Other => "other",
        }
    }

    /// The filter a command word is.
    fn of(head: &str) -> Self {
        match head {
            "grep" | "egrep" | "fgrep" | "rg" => Self::Grep,
            "sed" => Self::Sed,
            "awk" | "gawk" => Self::Awk,
            "wc" => Self::Wc,
            "sort" | "uniq" => Self::Sort,
            "cut" | "tr" | "column" => Self::Cut,
            _ => Self::Other,
        }
    }
}

impl Family {
    /// The family's published name.
    const fn key(self) -> &'static str {
        match self {
            Self::Git => "git",
            Self::Cargo => "cargo",
            Self::Build => "build",
            Self::Listing => "listing",
            Self::Label => "label",
            Self::ControlFlow => "control_flow",
            Self::Rift => "rift",
            Self::FileOps => "file_ops",
            Self::Shell => "shell",
            Self::Search => "search",
            Self::Expansion => "expansion",
            Self::Other => "other",
        }
    }

    /// The family of a stage's command: a group or a subshell is control
    /// flow, an assignment is the shell's, and a command word spelled through
    /// an expansion has no name to give.
    fn of_stage(stage: &Stage) -> Self {
        let Some(first) = stage.words.first() else {
            return Self::Other;
        };
        if first.text.starts_with(['(', '{', '}', ')']) {
            return Self::ControlFlow;
        }
        if is_assignment(first) {
            return Self::Shell;
        }
        if first.unpinned {
            return Self::Expansion;
        }
        Self::of(stage.head())
    }

    /// The family a command word belongs to.
    fn of(head: &str) -> Self {
        match head {
            "git" => Self::Git,
            "cargo" | "rustc" | "rustup" => Self::Cargo,
            "npm" | "npx" | "pnpm" | "yarn" | "node" | "make" | "go" | "python" | "python3"
            | "pytest" | "uv" | "pip" | "pip3" | "ruby" | "bash" | "sh" | "zsh" => Self::Build,
            "ls" | "find" | "tree" | "fd" => Self::Listing,
            "echo" | "printf" => Self::Label,
            "for" | "while" | "until" | "if" | "then" | "else" | "elif" | "fi" | "do" | "done"
            | "case" | "esac" | "test" | "[" | "[[" => Self::ControlFlow,
            "rift" | "rift-memd" => Self::Rift,
            "mkdir" | "rm" | "cp" | "mv" | "touch" | "chmod" | "ln" => Self::FileOps,
            "cd" | "export" | "source" | "." | ":" | "set" | "pwd" | "sleep" | "true" | "false"
            | "which" | "type" | "exit" | "wait" | "date" | "basename" | "dirname" | "realpath"
            | "ps" | "pgrep" | "pkill" | "kill" | "lsof" => Self::Shell,
            "ggrep" | "ag" | "ack" => Self::Search,
            _ => Self::Other,
        }
    }
}

/// Count one refused call under its shape: on the loose bound always, and on
/// the tight one when it names a work-tree file.
pub(super) fn count_shape(
    shapes: &mut BTreeMap<Shape, ShapeRow>,
    command: &str,
    named: bool,
    tokens: u64,
) {
    let row = shapes.entry(shape_of(command)).or_default();
    row.all.add(tokens);
    if named {
        row.named.add(tokens);
    }
}

/// The shapes in the order they are published: most tokens naming a file
/// first, then most calls, then the loose bound, then declaration order.
pub(super) fn ranked(shapes: &BTreeMap<Shape, ShapeRow>) -> Vec<(Shape, ShapeRow)> {
    let mut rows: Vec<(Shape, ShapeRow)> = shapes.iter().map(|(s, r)| (*s, *r)).collect();
    rows.sort_by(|(a_shape, a), (b_shape, b)| {
        (b.named.tokens, b.named.calls, b.all.tokens, b.all.calls)
            .cmp(&(a.named.tokens, a.named.calls, a.all.tokens, a.all.calls))
            .then(a_shape.cmp(b_shape))
    });
    rows
}

/// The shape of one refused command line.
pub(super) fn shape_of(command: &str) -> Shape {
    let lexed = lex(command);
    if lexed.heredoc {
        return heredoc_shape(&lexed);
    }
    if lexed.redirected {
        if recognise(&lexed.without_redirects).is_some() {
            return Shape::Redirect;
        }
        return chain_shape(&lex(&lexed.without_redirects));
    }
    chain_shape(&lexed)
}

/// A here-document is an inline script when an interpreter is fed it.
fn heredoc_shape(lexed: &Lexed) -> Shape {
    let interpreted = lexed
        .members
        .iter()
        .flat_map(|member| &member.stages)
        .any(|stage| INLINE_INTERPRETERS.contains(&stage.head()));
    if interpreted {
        Shape::ScriptInterpreter
    } else {
        Shape::Heredoc
    }
}

/// The shape of a whole line: its members with the `cd`s and labels set
/// aside, and the first one the grammar does not read.
fn chain_shape(lexed: &Lexed) -> Shape {
    let members: Vec<&Member> = lexed
        .members
        .iter()
        .filter(|member| !is_cd_or_label(member))
        .collect();
    match members.as_slice() {
        [only] if !is_read(only.text(lexed)) => member_shape(lexed, only),
        [_, _, ..] => first_unread_member(lexed, &members),
        // Nothing but `cd`s and labels: the read word sat inside an operand.
        [] => Shape::Other,
        [_] => fallback(lexed),
    }
}

/// In a chain, the shape of the first member the grammar does not read.
fn first_unread_member(lexed: &Lexed, members: &[&Member]) -> Shape {
    for member in members {
        if is_read(member.text(lexed)) {
            continue;
        }
        let Some(first) = member.stages.first() else {
            continue;
        };
        if is_reader(first) {
            return member_shape(lexed, member);
        }
        return Shape::ChainMember(Family::of_stage(first));
    }
    fallback(lexed)
}

/// A line whose parts the grammar reads alone: refused for a character, or
/// for how the parts are joined.
fn fallback(lexed: &Lexed) -> Shape {
    if lexed.members.iter().any(Member::unpinned) {
        Shape::QuotingOrShellCharacter
    } else {
        Shape::ChainComposition
    }
}

/// The shape of one member: a simple command, or a pipeline.
fn member_shape(lexed: &Lexed, member: &Member) -> Shape {
    let Some((first, rest)) = member.stages.split_first() else {
        return Shape::Other;
    };
    if rest.is_empty() {
        return simple_shape(&first.words);
    }
    if is_read(first.text(lexed)) {
        return piped_after_a_read(rest);
    }
    if is_numbered_print(first) && rest.len() == 1 {
        return Shape::NumberedLinesPiped;
    }
    if is_reader(first) {
        return simple_shape(&first.words);
    }
    Shape::PipedFrom(Family::of_stage(first))
}

/// What a recognised read was piped into.
fn piped_after_a_read(rest: &[Stage]) -> Shape {
    let filter = rest
        .iter()
        .map(Stage::head)
        .find(|head| !matches!(*head, "head" | "tail"));
    match filter {
        Some(head) => Shape::PipedIntoFilter(Filter::of(head)),
        None => Shape::PipedIntoHeadTail,
    }
}

/// The shape of one simple command.
fn simple_shape(words: &[Word]) -> Shape {
    let Some((head, rest)) = words.split_first() else {
        return Shape::Other;
    };
    let name = head.text.rsplit('/').next().unwrap_or(&head.text);
    if is_assignment(head) || WRAPPERS.contains(&name) {
        return Shape::Wrapper;
    }
    match name {
        "awk" | "gawk" | "perl" => return Shape::ScriptInterpreter,
        "python" | "python3" | "ruby" | "node" if has_inline_script(rest) => {
            return Shape::ScriptInterpreter;
        }
        "wc" | "stat" | "file" | "du" => return Shape::SizeOnly,
        "less" | "more" | "xxd" | "od" | "hexdump" => return Shape::PagerOrDump,
        "git" => return git_shape(rest),
        _ => (),
    }
    if !VOCABULARY.contains(&name) {
        return Shape::Other;
    }
    if words.iter().any(|word| word.unpinned) {
        return Shape::QuotingOrShellCharacter;
    }
    vocabulary_shape(name, rest)
}

/// The shape of a vocabulary command the grammar refused.
fn vocabulary_shape(name: &str, rest: &[Word]) -> Shape {
    let options = rest.iter().filter(|word| is_option(word)).count();
    let operands = rest.len() - options;
    match name {
        "cat" if options == 0 => Shape::QuotingOrShellCharacter,
        "cat" if rest.iter().filter(|w| is_option(w)).all(|w| w.text == "-n") => Shape::CatNumbered,
        "cat" => Shape::CatOption,
        "nl" | "bat" | "batcat" if options > 0 => Shape::DecoratedOption,
        "head" | "tail" => head_tail_shape(rest),
        "sed" => sed_shape(rest),
        "grep" | "egrep" | "fgrep" | "rg" if options > 0 => Shape::SearchFlags,
        "grep" | "egrep" | "fgrep" | "rg" if operands > 2 => Shape::SearchSeveralPaths,
        _ if operands > 1 && !matches!(name, "grep" | "egrep" | "fgrep" | "rg") => {
            Shape::SeveralPaths
        }
        _ => Shape::QuotingOrShellCharacter,
    }
}

/// `head` and `tail`: the pinned `-n N`, or a flag the grammar does not pin.
fn head_tail_shape(rest: &[Word]) -> Shape {
    let paths = match rest {
        [flag, count, paths @ ..] if flag.text == "-n" && is_count(count) => paths,
        _ if rest.iter().any(is_option) => return Shape::HeadTailOption,
        paths => paths,
    };
    if paths.len() > 1 {
        Shape::SeveralPaths
    } else {
        Shape::QuotingOrShellCharacter
    }
}

/// `sed`: the one pinned script over one path, or what it was instead.
fn sed_shape(rest: &[Word]) -> Shape {
    match rest {
        [flag, script, paths @ ..] if flag.text == "-n" && !flag.quoted => {
            if !is_line_range(&script.text) || paths.iter().any(is_option) {
                Shape::SedScript
            } else if paths.len() > 1 {
                Shape::SeveralPaths
            } else {
                Shape::QuotingOrShellCharacter
            }
        }
        _ => Shape::SedOption,
    }
}

/// `git`: the reading subcommands, by shape.
fn git_shape(rest: &[Word]) -> Shape {
    let Some((sub, args)) = rest.split_first() else {
        return Shape::Other;
    };
    let has = |flags: &[&str]| args.iter().any(|word| flags.contains(&word.text.as_str()));
    match sub.text.as_str() {
        "show" if args.iter().any(|w| !is_option(w) && w.text.contains(':')) => {
            Shape::GitShowRevPath
        }
        "show" => Shape::GitShow,
        "diff" => Shape::GitDiff,
        "blame" => Shape::GitLogOrBlame,
        "log" if has(&["-p", "--patch", "-u"]) => Shape::GitLogOrBlame,
        _ => Shape::Other,
    }
}

/// Whether the grammar recognises this text as a read.
fn is_read(text: &str) -> bool {
    matches!(recognise(text), Some(ShellCall::Read(_)))
}

/// Whether a member is a `cd`, or an `echo`/`printf` over pinned words —
/// the two members a chain sets aside.
fn is_cd_or_label(member: &Member) -> bool {
    let [stage] = member.stages.as_slice() else {
        return false;
    };
    match stage.head() {
        "cd" => true,
        "echo" | "printf" => !member.unpinned(),
        _ => false,
    }
}

/// Whether a stage's command reads a file's content or its size — or puts a
/// command behind a wrapper — so the reason lies in the stage itself rather
/// than in what it was joined to.
fn is_reader(stage: &Stage) -> bool {
    let head = stage.head();
    let args = stage.words.get(1..).unwrap_or_default();
    let assigns_then_runs = stage.words.first().is_some_and(is_assignment) && !args.is_empty();
    if assigns_then_runs || WRAPPERS.contains(&head) {
        return true;
    }
    match head {
        "git" => {
            matches!(
                args.first().map(|w| w.text.as_str()),
                Some("show" | "diff" | "blame")
            ) || git_shape(args) == Shape::GitLogOrBlame
        }
        "python" | "python3" | "ruby" | "node" => has_inline_script(args),
        _ => {
            VOCABULARY.contains(&head)
                || matches!(
                    head,
                    "awk"
                        | "gawk"
                        | "perl"
                        | "wc"
                        | "stat"
                        | "file"
                        | "du"
                        | "less"
                        | "more"
                        | "xxd"
                        | "od"
                        | "hexdump"
                )
        }
    }
}

/// `cat -n` or `nl` with an option: a print whose lines carry numbers.
fn is_numbered_print(stage: &Stage) -> bool {
    let options = || stage.words.iter().skip(1).filter(|w| is_option(w));
    match stage.head() {
        "cat" => options().any(|w| w.text == "-n"),
        "nl" => options().next().is_some(),
        _ => false,
    }
}

/// Whether an interpreter was handed an inline script.
fn has_inline_script(args: &[Word]) -> bool {
    args.iter().any(|w| matches!(w.text.as_str(), "-c" | "-e"))
}

/// `NAME=value`: a variable assignment, standing alone or in front of a
/// command.
fn is_assignment(word: &Word) -> bool {
    let Some((name, _)) = word.text.split_once('=') else {
        return false;
    };
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// An unquoted word the command would read as an option.
fn is_option(word: &Word) -> bool {
    !word.quoted && word.text.len() > 1 && word.text.starts_with('-')
}

/// A positive line count in digits, and digits only.
fn is_count(word: &Word) -> bool {
    word.text.bytes().all(|b| b.is_ascii_digit()) && word.text.parse::<u32>().is_ok_and(|n| n > 0)
}

/// `A,Bp`, the one `sed` script the grammar pins — both ends in digits only,
/// since `1,+3p` is a relative range the grammar refuses.
fn is_line_range(script: &str) -> bool {
    let Some((lo, hi)) = script.strip_suffix('p').and_then(|b| b.split_once(',')) else {
        return false;
    };
    let number = |text: &str| {
        let digits = !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit());
        digits.then(|| text.parse::<u32>().ok()).flatten()
    };
    matches!((number(lo), number(hi)), (Some(lo), Some(hi)) if lo >= 1 && hi >= lo)
}

#[cfg(test)]
mod tests {
    use super::{shape_of, Family, Filter, Shape};

    #[test]
    fn each_vocabulary_command_names_the_option_the_grammar_does_not_pin() {
        assert_eq!(shape_of("cat -n src/a.rs"), Shape::CatNumbered);
        assert_eq!(shape_of("cat -A src/a.rs"), Shape::CatOption);
        assert_eq!(shape_of("nl -ba src/a.rs"), Shape::DecoratedOption);
        assert_eq!(shape_of("head -20 src/a.rs"), Shape::HeadTailOption);
        assert_eq!(shape_of("tail -c 100 src/a.rs"), Shape::HeadTailOption);
        assert_eq!(shape_of("tail -n +5 src/a.rs"), Shape::HeadTailOption);
        assert_eq!(
            shape_of("cat src/a.rs | tail -n +5"),
            Shape::PipedIntoHeadTail
        );
        assert_eq!(shape_of("sed -n '1,+3p' a"), Shape::SedScript);
        assert_eq!(shape_of("sed -n '1,5p;9,12p' a"), Shape::SedScript);
        assert_eq!(shape_of("sed 's/a/b/' a"), Shape::SedOption);
        assert_eq!(shape_of("sed -n '1,5p' a b"), Shape::SeveralPaths);
        assert_eq!(shape_of("head -n 5 a b"), Shape::SeveralPaths);
        assert_eq!(shape_of("grep -c x a"), Shape::SearchFlags);
        assert_eq!(shape_of("rg x a b"), Shape::SearchSeveralPaths);
    }

    #[test]
    fn git_reads_are_named_by_subcommand() {
        assert_eq!(shape_of("git show HEAD:a | head -5"), Shape::GitShowRevPath);
        assert_eq!(shape_of("git show abc | grep x"), Shape::GitShow);
        assert_eq!(shape_of("git diff a -- b | head"), Shape::GitDiff);
        assert_eq!(shape_of("git log -p a | head"), Shape::GitLogOrBlame);
        assert_eq!(shape_of("git blame a | grep x"), Shape::GitLogOrBlame);
        assert_eq!(
            shape_of("git status | head -5"),
            Shape::PipedFrom(Family::Git)
        );
    }

    #[test]
    fn a_pipe_and_a_chain_are_named_by_what_follows_the_read() {
        assert_eq!(
            shape_of("cat a | grep x"),
            Shape::PipedIntoFilter(Filter::Grep)
        );
        assert_eq!(
            shape_of("sed -n '1,9p' a | wc -l"),
            Shape::PipedIntoFilter(Filter::Wc)
        );
        assert_eq!(shape_of("cat a | head"), Shape::PipedIntoHeadTail);
        assert_eq!(
            shape_of("nl -ba a | sed -n '1,9p'"),
            Shape::NumberedLinesPiped
        );
        assert_eq!(
            shape_of("cd x && cat a && cargo test"),
            Shape::ChainMember(Family::Cargo)
        );
        assert_eq!(shape_of("cat a || cat b"), Shape::ChainComposition);
        assert_eq!(
            shape_of("cargo test | tail -5"),
            Shape::PipedFrom(Family::Cargo)
        );
    }

    #[test]
    fn the_shell_itself_is_named_where_it_is_the_whole_reason() {
        assert_eq!(shape_of("cat a 2>&1"), Shape::Redirect);
        assert_eq!(
            shape_of("cat a 2>/dev/null | grep x"),
            Shape::PipedIntoFilter(Filter::Grep)
        );
        assert_eq!(shape_of("cat $F"), Shape::QuotingOrShellCharacter);
        assert_eq!(shape_of("cat ~/a"), Shape::QuotingOrShellCharacter);
        assert_eq!(
            shape_of("head -n 5 src/*.rs"),
            Shape::QuotingOrShellCharacter
        );
        assert_eq!(shape_of("cat <<EOF\nx\nEOF"), Shape::Heredoc);
        assert_eq!(
            shape_of("python3 - <<'EOF'\nopen('a')\nEOF"),
            Shape::ScriptInterpreter
        );
        assert_eq!(shape_of("awk 'NR<5' a"), Shape::ScriptInterpreter);
        assert_eq!(shape_of("wc -l a && cat a"), Shape::SizeOnly);
        assert_eq!(shape_of("xxd a | head"), Shape::PagerOrDump);
        assert_eq!(shape_of("FOO=1 cat a"), Shape::Wrapper);
        assert_eq!(
            shape_of("S=/tmp/x; cat a; cat b"),
            Shape::ChainMember(Family::Shell),
            "an assignment standing alone is the shell's own member"
        );
        assert_eq!(
            shape_of("cat a && $RUN x"),
            Shape::ChainMember(Family::Expansion)
        );
        assert_eq!(
            shape_of("cat a && { cargo test; }"),
            Shape::ChainMember(Family::ControlFlow)
        );
        assert_eq!(shape_of("cat a; sudo cat b"), Shape::Wrapper);
        assert_eq!(shape_of("echo 'see; cat a'"), Shape::Other);
        assert_eq!(
            shape_of("for f in a; do cat $f; done"),
            Shape::ChainMember(Family::ControlFlow)
        );
    }

    #[test]
    fn every_shape_has_a_distinct_published_name() {
        let shapes = [
            Shape::PipedIntoFilter(Filter::Other),
            Shape::ChainMember(Family::Other),
            Shape::PipedFrom(Family::Rift),
            Shape::Other,
        ];
        let keys: Vec<String> = shapes.iter().map(|shape| shape.key()).collect();
        assert_eq!(
            keys,
            [
                "piped_into_filter:other",
                "chain_member:other",
                "piped_from:rift",
                "other"
            ]
        );
    }
}
