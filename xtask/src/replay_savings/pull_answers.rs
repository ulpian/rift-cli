//! Pull answers by verb: the times an agent really ran one of Rift's
//! lookup verbs from a shell, what each answer cost, and whether the window
//! went on to read the file the answer was about.
//!
//! A module of its own because no bucket sees these calls — they are shell
//! calls that read no file — and every figure it adds is on rows of its own,
//! so every row before it reads the same with or without it.
//!
//! **An invocation, not a mention.** The command line is cut the way the
//! shell cuts it (chains, pipes, command substitutions and groupings, quotes
//! respected, here-document bodies dropped), and a verb counts when the
//! command word of one of the cuts is the `rift` binary (or `rift-old`, or a
//! shell variable standing in front of `recall` or `memory`) followed by
//! `recall <verb>` or `memory <verb>`. Leading `NAME=value` words and the
//! wrappers `env`, `time`, `timeout`, `nice` and `perl -e '… exec @ARGV'`
//! are stepped over. The same words inside `echo`, `grep`, a commit message
//! or a list never head a cut, so they never count. A call that asks for
//! `--help` is counted on its own row and on no verb's.
//!
//! **The unit** is the shell call: its calls and the tokens its result
//! returned. A call running two verbs is on both verbs' rows and once on the
//! total.
//!
//! **A probe** is an invocation run after the shell had changed into a
//! directory under the system's temporary roots — a scratch repository made
//! to try the verb, never the family. The directory is the call's own
//! working directory until a `cd` in the same command line moves it, with
//! `NAME=value` assignments earlier in the line substituted; a `cd` whose
//! target cannot be resolved from the line leaves the call a non-probe, so
//! the probe count is a lower bound.
//!
//! **Followed** counts the read calls, in the same window and before the next
//! compaction, that read a path the answer's text names — by any read, and by
//! a read of the whole file (line 1 through the last line). Probes are left
//! out, since their answers name files of a repository that is not the
//! family's.

use serde_json::{json, Map, Value};

use super::file_order::ReadNote;
use super::render::row;
use super::report::{Report, Tally};
use super::shell_lex::{lex, Stage};

/// The verbs that answer a pull, in the order the table prints them. The
/// last entry stands for every pull verb not named above it.
const VERBS: [(&str, &str); 7] = [
    ("known-state", "known-state"),
    ("diff-since", "diff-since"),
    ("changed-hunks", "changed-hunks"),
    ("context-pack", "context-pack"),
    ("retrieve-exact", "retrieve-exact"),
    ("read-summary", "read-summary"),
    ("other", "doc-read, file-if-changed, search"),
];

/// The pull verbs that are not given a row of their own.
const OTHER_PULL_VERBS: [&str; 3] = ["doc-read", "file-if-changed", "search"];

/// How many of `VERBS` are the lookup verbs the total beneath them names.
const LOOKUP_VERBS: usize = 4;

/// The directories a probe's scratch repository lives under.
const TEMPORARY_ROOTS: [&str; 4] = [
    "/tmp",
    "/private/tmp",
    "/var/folders",
    "/private/var/folders",
];

/// The words that may stand before a command without being it.
const LEADING_WORDS: [&str; 8] = [
    "env", "time", "exec", "command", "do", "then", "else", "nohup",
];

/// The wrappers that take another command as an argument.
const WRAPPERS: [&str; 3] = ["perl", "timeout", "nice"];

/// What a replay saw of pull answers.
#[derive(Clone, Debug, Default)]
pub struct PullPool {
    /// One tally per entry of `VERBS`.
    verbs: [Tally; 7],
    /// Every pull, a call once.
    all: Tally,
    /// The calls running one of the lookup verbs, a call once.
    lookups: Tally,
    /// The pulls run in a scratch repository, a call once.
    probes: Tally,
    /// Calls asking for help, which are on no verb's row.
    help: Tally,
    /// Reads of a named path, by a window that had been answered.
    followed: Tally,
    /// Of those, the reads of the whole file.
    followed_whole: Tally,
}

/// One pull invocation found in a command line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Invocation {
    /// The verb, spelled as the command line spelled it.
    verb: String,
    /// Whether it asked for help.
    help: bool,
    /// Whether it ran in a scratch repository.
    probe: bool,
}

/// An answer a window may go on to read the files of.
#[derive(Clone, Debug)]
pub(super) struct PendingAnswer {
    /// The text the call returned.
    text: String,
}

impl PendingAnswer {
    /// An answer to remember.
    pub(super) fn new(text: &str) -> Self {
        Self {
            text: text.to_owned(),
        }
    }
}

impl PullPool {
    /// Count one shell call by the pull invocations found in it. Returns the
    /// call's answer to remember, when it has a pull outside a scratch
    /// repository to follow.
    pub(super) fn note_call(
        &mut self,
        invocations: &[Invocation],
        text: &str,
        tokens: u64,
    ) -> Option<PendingAnswer> {
        let (helps, pulls): (Vec<&Invocation>, Vec<&Invocation>) =
            invocations.iter().partition(|i| i.help);
        if !helps.is_empty() && pulls.is_empty() {
            self.help.add(tokens);
        }
        if pulls.is_empty() {
            return None;
        }
        self.all.add(tokens);
        let mut seen = [false; 7];
        for invocation in &pulls {
            let at = verb_row(&invocation.verb);
            if let Some(flag) = seen.get_mut(at) {
                *flag = true;
            }
        }
        for (at, tally) in self.verbs.iter_mut().enumerate() {
            if seen.get(at).copied().unwrap_or(false) {
                tally.add(tokens);
            }
        }
        if seen.iter().take(LOOKUP_VERBS).any(|flag| *flag) {
            self.lookups.add(tokens);
        }
        if pulls.iter().all(|i| i.probe) {
            self.probes.add(tokens);
            return None;
        }
        Some(PendingAnswer::new(text))
    }

    /// A read call of a window that holds `answers`: counted if it read a
    /// path any of them names, and once more if it read one whole.
    pub(super) fn note_follow_up(&mut self, answers: &[PendingAnswer], read: &ReadNote) {
        let named = |path: &String| answers.iter().any(|a| a.text.contains(path.as_str()));
        if read.paths.iter().any(named) {
            self.followed.add(read.tokens);
        }
        if read.whole.iter().any(named) {
            self.followed_whole.add(read.tokens);
        }
    }
}

/// The row of `VERBS` a verb is counted on.
fn verb_row(verb: &str) -> usize {
    VERBS
        .iter()
        .take(VERBS.len() - 1)
        .position(|(key, _)| *key == verb)
        .unwrap_or(VERBS.len() - 1)
}

/// Whether a verb answers a pull.
fn is_pull_verb(verb: &str) -> bool {
    VERBS
        .iter()
        .take(VERBS.len() - 1)
        .any(|(key, _)| *key == verb)
        || OTHER_PULL_VERBS.contains(&verb)
}

/// Every pull invocation in a command line, run from `cwd`.
pub(super) fn pull_invocations(command: &str, cwd: Option<&str>) -> Vec<Invocation> {
    let lexed = lex(&uncomposed(command));
    let mut env: Vec<(String, String)> = Vec::new();
    let mut here: Option<String> = cwd.map(str::to_owned);
    let mut out = Vec::new();
    for member in &lexed.members {
        for stage in &member.stages {
            let words: Vec<&str> = stage.words.iter().map(|w| w.text.as_str()).collect();
            note_assignments(&words, &mut env);
            if let Some(target) = cd_target(&words) {
                here = resolve_dir(target, here.as_deref(), &env);
                continue;
            }
            if let Some(invocation) = invocation_of(stage, here.as_deref()) {
                out.push(invocation);
            }
        }
    }
    out
}

/// The pull invocation a stage is, if it is one.
fn invocation_of(stage: &Stage, here: Option<&str>) -> Option<Invocation> {
    let words: Vec<&str> = stage.words.iter().map(|w| w.text.as_str()).collect();
    let rest = after_the_binary(&words)?;
    let [family, verb, args @ ..] = rest else {
        return None;
    };
    if *family != "recall" && *family != "memory" {
        return None;
    }
    let verb = *verb;
    let asks_for_help = verb.starts_with('-') || args.iter().any(|a| matches!(*a, "--help" | "-h"));
    if !asks_for_help && !is_pull_verb(verb) {
        return None;
    }
    Some(Invocation {
        verb: verb.to_owned(),
        help: asks_for_help,
        probe: here.is_some_and(is_temporary),
    })
}

/// The words after the `rift` binary a stage runs, or `None` when it runs
/// something else.
fn after_the_binary<'a, 'w>(words: &'w [&'a str]) -> Option<&'w [&'a str]> {
    let mut at = 0;
    while let Some(word) = words.get(at) {
        let base = word.rsplit('/').next().unwrap_or(word);
        if is_assignment(word) || LEADING_WORDS.contains(&base) || *word == "-p" {
            at += 1;
        } else if WRAPPERS.contains(&base) {
            at = words
                .iter()
                .skip(at)
                .position(|w| w.rsplit('/').next().unwrap_or(w).starts_with("rift"))
                .map(|offset| at + offset)?;
        } else {
            break;
        }
    }
    let head = words.get(at)?;
    let rest = words.get(at + 1..)?;
    let base = head.rsplit('/').next().unwrap_or(head);
    let named = base == "rift" || base == "rift-old";
    let standing_in = is_variable(head) && matches!(rest.first(), Some(&"recall" | &"memory"));
    (named || standing_in).then_some(rest)
}

/// Whether a word is `NAME=value`.
fn is_assignment(word: &str) -> bool {
    word.split_once('=').is_some_and(|(name, _)| {
        !name.is_empty()
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            && !name.starts_with(|c: char| c.is_ascii_digit())
    })
}

/// Whether a word is a shell variable, perhaps with the binary's name after
/// it: `$R`, `${R}`, `$R/rift`.
fn is_variable(word: &str) -> bool {
    let Some(rest) = word.strip_prefix('$') else {
        return false;
    };
    let rest = rest.strip_prefix('{').unwrap_or(rest);
    let name: String = rest
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    if name.is_empty() {
        return false;
    }
    let after = rest.get(name.len()..).unwrap_or_default();
    let after = after.strip_prefix('}').unwrap_or(after);
    after.is_empty() || after.starts_with("/rift")
}

/// The assignments a stage makes: its own words, or the ones after `export`.
fn note_assignments(words: &[&str], env: &mut Vec<(String, String)>) {
    let skip = usize::from(matches!(
        words.first(),
        Some(&"export" | &"declare" | &"local" | &"readonly")
    ));
    for word in words.iter().skip(skip) {
        if !is_assignment(word) {
            if skip == 0 {
                break;
            }
            continue;
        }
        if let Some((name, value)) = word.split_once('=') {
            let value = substitute(value, env);
            env.push((name.to_owned(), value));
        }
    }
}

/// The target of a stage that is `cd <target>`.
fn cd_target<'a>(words: &[&'a str]) -> Option<&'a str> {
    match words {
        [cd, target, ..] if *cd == "cd" => Some(*target),
        _ => None,
    }
}

/// A word with the variables assigned earlier in the line put in; a variable
/// that was not assigned stays as written.
fn substitute(word: &str, env: &[(String, String)]) -> String {
    let mut out = word.to_owned();
    for (name, value) in env.iter().rev() {
        out = out
            .replace(&format!("${{{name}}}"), value)
            .replace(&format!("${name}"), value);
    }
    out
}

/// Where `cd target` leaves the shell, or `None` when the line does not say.
fn resolve_dir(target: &str, here: Option<&str>, env: &[(String, String)]) -> Option<String> {
    let target = substitute(target, env);
    if target.starts_with('$') || target.starts_with('~') || target == "-" {
        return None;
    }
    if target.starts_with('/') {
        return Some(normal(&target));
    }
    here.map(|dir| normal(&format!("{dir}/{target}")))
}

/// A path with `.` and `..` components folded away.
fn normal(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => (),
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    format!("/{}", parts.join("/"))
}

/// Whether a directory is under one of the temporary roots.
fn is_temporary(dir: &str) -> bool {
    TEMPORARY_ROOTS
        .iter()
        .any(|root| dir == *root || dir.starts_with(&format!("{root}/")))
}

/// The command line with its here-document bodies dropped and its command
/// substitutions, groupings and backticks turned into chain separators, all
/// outside quotes, so the lexer sees each command the shell would run.
fn uncomposed(command: &str) -> String {
    let mut walk = Uncomposer {
        chars: command.chars().collect(),
        at: 0,
        out: String::new(),
        tags: Vec::new(),
        quote: None,
    };
    while walk.at < walk.chars.len() {
        walk.step();
    }
    walk.out
}

/// The walk over one command line, with the quote it is inside and the
/// here-document tags still waiting for their bodies.
struct Uncomposer {
    chars: Vec<char>,
    at: usize,
    out: String,
    tags: Vec<String>,
    quote: Option<char>,
}

impl Uncomposer {
    /// The character `ahead` places on from the cursor.
    fn peek(&self, ahead: usize) -> Option<char> {
        self.chars.get(self.at + ahead).copied()
    }

    /// Copy the character under the cursor.
    fn keep(&mut self) {
        if let Some(c) = self.peek(0) {
            self.out.push(c);
        }
        self.at += 1;
    }

    /// One step: the character under the cursor, or the construct it opens.
    fn step(&mut self) {
        let (Some(c), next) = (self.peek(0), self.peek(1)) else {
            return;
        };
        match (self.quote, c) {
            (None | Some('"'), '\\') => {
                self.keep();
                self.keep();
            }
            (None, '\'' | '"') => {
                self.quote = Some(c);
                self.keep();
            }
            (Some(open), _) if c == open => {
                self.quote = None;
                self.keep();
            }
            (None, '$') if next == Some('(') => {
                self.out.push(';');
                self.at += 2;
            }
            (None, '(' | ')' | '`') => {
                self.out.push(';');
                self.at += 1;
            }
            (None, '<') if next == Some('<') => self.heredoc(),
            (None, '\n') if !self.tags.is_empty() => {
                self.keep();
                self.skip_bodies();
            }
            _ => self.keep(),
        }
    }

    /// A `<<` outside quotes: cut it and its tag out, remembering the tag so
    /// its body is dropped at the next line break. `<<<` is a here-string,
    /// whose word is an ordinary argument.
    fn heredoc(&mut self) {
        if self.peek(2) == Some('<') {
            self.out.push_str("<<<");
            self.at += 3;
            return;
        }
        self.at += 2;
        if self.peek(0) == Some('-') {
            self.at += 1;
        }
        while matches!(self.peek(0), Some(' ' | '\t' | '\'' | '"')) {
            self.at += 1;
        }
        let mut tag = String::new();
        while let Some(c) = self.peek(0).filter(|c| c.is_alphanumeric() || *c == '_') {
            tag.push(c);
            self.at += 1;
        }
        while matches!(self.peek(0), Some('\'' | '"')) {
            self.at += 1;
        }
        if !tag.is_empty() {
            self.tags.push(tag);
        }
    }

    /// Drop the lines up to and including the closing line of every tag
    /// waiting for its body.
    fn skip_bodies(&mut self) {
        for tag in std::mem::take(&mut self.tags) {
            while self.at < self.chars.len() {
                let line: String = self
                    .chars
                    .iter()
                    .skip(self.at)
                    .take_while(|c| **c != '\n')
                    .collect();
                self.at += line.chars().count() + 1;
                if line.trim() == tag {
                    break;
                }
            }
        }
    }
}

/// The table's rows for the pull answers.
pub(super) fn pull_rows(report: &Report, denominator: u64) -> String {
    let pool = &report.pulls;
    let mut out =
        String::from("\n  pull answers: the verb run from a shell (not a mention of it)\n");
    for ((_, label), tally) in VERBS.iter().zip(&pool.verbs) {
        out.push_str(&row(label, *tally, denominator));
    }
    out.push_str(&row(
        "all pulls (a call running two verbs once)",
        pool.all,
        denominator,
    ));
    out.push_str(&row(
        "  of them, the four lookup verbs",
        pool.lookups,
        denominator,
    ));
    out.push_str(&row(
        "  of them, run in a scratch repository (probes)",
        pool.probes,
        denominator,
    ));
    out.push_str(&row(
        "asking for --help (on no row above)",
        pool.help,
        denominator,
    ));
    out.push_str(&row(
        "reads of a path an answer named (not probes)",
        pool.followed,
        denominator,
    ));
    out.push_str(&row(
        "  of them, the whole file",
        pool.followed_whole,
        denominator,
    ));
    out
}

/// The same rows for the machine-readable document.
pub(super) fn pull_json(report: &Report) -> Value {
    let pool = &report.pulls;
    let tally = |t: Tally| json!({ "calls": t.calls, "tokens": t.tokens });
    let mut verbs = Map::new();
    for ((key, _), t) in VERBS.iter().zip(&pool.verbs) {
        verbs.insert((*key).to_owned(), tally(*t));
    }
    json!({
        "by_verb": verbs,
        "all": tally(pool.all),
        "lookup_verbs": tally(pool.lookups),
        "probes": tally(pool.probes),
        "asking_for_help": tally(pool.help),
        "followed_by_a_read_of_a_named_path": tally(pool.followed),
        "followed_by_a_whole_file_read_of_a_named_path": tally(pool.followed_whole),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn verbs(command: &str, cwd: Option<&str>) -> Vec<(String, bool, bool)> {
        pull_invocations(command, cwd)
            .into_iter()
            .map(|i| (i.verb, i.help, i.probe))
            .collect()
    }

    fn one(verb: &str, help: bool, probe: bool) -> Vec<(String, bool, bool)> {
        vec![(verb.to_owned(), help, probe)]
    }

    /// ★ A verb counts when a command word of the cut line runs it, through
    /// a path, a wrapper, a variable, a pipe or a substitution — and never
    /// when the same words are an argument of `echo`, `grep` or a quoted
    /// string.
    #[test]
    fn a_verb_counts_only_where_the_shell_would_run_it() {
        let repo = Some("/work/family");
        let known = one("known-state", false, false);
        assert_eq!(
            verbs("./target/debug/rift recall known-state --json", repo),
            known
        );
        assert_eq!(
            verbs("cd sub && RIFT_X=1 rift recall known-state | head", repo),
            known
        );
        assert_eq!(
            verbs(
                "perl -e 'alarm 9; exec @ARGV' target/release/rift recall known-state",
                repo
            ),
            known
        );
        assert_eq!(
            verbs("R=target/debug/rift; $R recall known-state", repo),
            known
        );
        assert_eq!(
            verbs("out=$(rift recall known-state --task x)", repo),
            known
        );
        assert_eq!(verbs("(rift memory known-state)", repo), known);
        assert_eq!(
            verbs("rift recall known-state && rift memory diff-since", repo)
                .iter()
                .map(|(verb, _, _)| verb.as_str())
                .collect::<Vec<_>>(),
            ["known-state", "diff-since"]
        );
        for mention in [
            "echo 'rift recall known-state'",
            "grep -n \"rift recall known-state\" docs/guide.md",
            "git commit -m \"run rift recall known-state\"",
            "for v in known-state diff-since; do echo $v; done",
            "cat <<'EOF'\nrift recall known-state\nEOF",
            "rift recall status",
            "rift status",
        ] {
            assert!(verbs(mention, repo).is_empty(), "{mention}");
        }
        assert_eq!(
            verbs("cat <<EOF\nx\nEOF\nrift recall changed-hunks", repo),
            one("changed-hunks", false, false),
            "a command after a here-document still counts"
        );
    }

    #[test]
    fn help_is_counted_apart_and_a_probe_is_a_run_in_a_temporary_directory() {
        let repo = Some("/work/family");
        assert_eq!(
            verbs("rift recall known-state --help", repo),
            one("known-state", true, false)
        );
        assert_eq!(
            verbs("rift recall --help", repo),
            one("--help", true, false)
        );
        let probe = one("changed-hunks", false, true);
        assert_eq!(
            verbs("cd /private/tmp/x/repo && rift recall changed-hunks", repo),
            probe
        );
        assert_eq!(
            verbs(
                "D=/tmp/fixture; cd $D/repo; rift recall changed-hunks",
                repo
            ),
            probe
        );
        assert_eq!(
            verbs("rift recall changed-hunks", Some("/private/var/folders/a")),
            probe
        );
        assert_eq!(
            verbs(
                "cd /tmp/x && cd /work/other && rift recall changed-hunks",
                repo
            ),
            one("changed-hunks", false, false),
            "a later cd moves it back"
        );
        assert_eq!(
            verbs("cd \"$T\" && rift recall changed-hunks", repo),
            one("changed-hunks", false, false),
            "a target the line does not resolve is not shown to be a probe"
        );
        assert_eq!(
            verbs("cd /tmp/../work/f && rift recall changed-hunks", repo),
            one("changed-hunks", false, false)
        );
    }

    fn run(verb: &str, probe: bool) -> Invocation {
        Invocation {
            verb: verb.to_owned(),
            help: false,
            probe,
        }
    }

    fn tally(calls: u64, tokens: u64) -> Tally {
        Tally { calls, tokens }
    }

    /// ★ The pool counts calls and tokens per verb, a call running two verbs
    /// once in the totals, and probes and help apart.
    #[test]
    fn the_pool_counts_each_call_once_with_probes_and_help_apart() {
        let mut pool = PullPool::default();
        let answer = pool.note_call(
            &[run("known-state", false), run("diff-since", false)],
            "see src/a.rs",
            40,
        );
        assert!(answer.is_some(), "an answer outside a scratch repository");
        assert!(pool
            .note_call(&[run("changed-hunks", true)], "src/p.rs", 10)
            .is_none());
        pool.note_call(&[run("doc-read", false)], "", 5);
        let help = Invocation {
            help: true,
            ..run("known-state", false)
        };
        pool.note_call(&[help], "", 3);
        assert_eq!(pool.all, tally(3, 55));
        assert_eq!(pool.lookups, tally(2, 50));
        assert_eq!(pool.verbs[0], tally(1, 40));
        assert_eq!(pool.verbs[1], tally(1, 40));
        assert_eq!(pool.verbs[6], tally(1, 5));
        assert_eq!(pool.probes, tally(1, 10));
        assert_eq!(pool.help, tally(1, 3));
    }

    /// ★ A read of a path an answer names is followed; one of the whole file
    /// is followed whole; a read of another path is neither.
    #[test]
    fn a_read_of_a_named_path_follows_the_answer() {
        let mut pool = PullPool::default();
        let answers = [pool
            .note_call(&[run("known-state", false)], "see src/a.rs", 40)
            .unwrap()];
        let read = |paths: &[&str], whole: &[&str], tokens: u64| ReadNote {
            paths: paths.iter().map(|p| (*p).to_owned()).collect(),
            whole: whole.iter().map(|p| (*p).to_owned()).collect(),
            tokens,
            ..ReadNote::default()
        };
        pool.note_follow_up(&answers, &read(&["src/a.rs"], &[], 100));
        pool.note_follow_up(&answers, &read(&["src/b.rs"], &["src/b.rs"], 100));
        pool.note_follow_up(&answers, &read(&["src/b.rs", "src/a.rs"], &["src/a.rs"], 7));
        assert_eq!(pool.followed, tally(2, 107));
        assert_eq!(pool.followed_whole, tally(1, 7));
    }

    #[test]
    fn the_rows_and_document_carry_counts_and_no_command_text() {
        let mut pool = PullPool::default();
        let invocation = pull_invocations("rift recall known-state --task 'secret goal'", None);
        pool.note_call(&invocation, "secret answer", 9);
        let report = Report {
            pulls: pool,
            ..Report::default()
        };
        let table = pull_rows(&report, 100);
        assert!(table.contains("known-state"), "{table}");
        assert!(!table.contains("secret"), "no text reaches the table");
        let document = pull_json(&report);
        assert_eq!(document["by_verb"]["known-state"]["calls"], 1);
        assert_eq!(document["all"]["tokens"], 9);
    }
}
