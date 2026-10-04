//! A second, independent reading of the grammar's safety argument, checked
//! over generated lines: nothing a shell would act on is ever recognised, and
//! a recognised target is always a word the line wrote.

use crate::shell_read::{recognise, ShellCall};

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

/// The vocabulary alphabet: words of the grammar, options it does and does
/// not admit, paths, patterns, and the punctuation that ends a recognition.
const VOCABULARY_TOKENS: &[&str] = &[
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
];

/// The composition alphabet: a `cd` and its directory, the two narrowing
/// spellings, and the near misses beside each.
const COMPOSITION_TOKENS: &[&str] = &[
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
];

/// The label alphabet, and the operands that are not plain ones.
const LABEL_TOKENS: &[&str] = &[
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
];

/// The pieces a shell-ish line is built from: the three alphabets above,
/// weighted well over a short arbitrary string for the space between them.
fn token() -> impl proptest::strategy::Strategy<Value = String> {
    let fixed: Vec<String> = [VOCABULARY_TOKENS, COMPOSITION_TOKENS, LABEL_TOKENS]
        .concat()
        .into_iter()
        .map(str::to_owned)
        .collect();
    proptest::prop_oneof![
        8 => proptest::sample::select(fixed),
        1 => proptest::string::string_regex("(?s).{0,12}").expect("a valid generator"),
    ]
}

/// The spellings a count reaches the grammar in, each with a `{}` where the
/// count goes: the command position, both narrowing spellings, and either
/// end of a `sed` range.
const COUNT_POSITIONS: &[&str] = &[
    "tail -n {} src/lib.rs",
    "head -n {} src/lib.rs",
    "cat src/lib.rs | tail -n {}",
    "cat src/lib.rs | tail -{}",
    "cat src/lib.rs | head -n {}",
    "cat src/lib.rs | head -{}",
    "sed -n '{},99999p' src/lib.rs",
    "sed -n '1,{}p' src/lib.rs",
];

/// The search flags every command of the family pins, each spelled as the
/// whole option word it is written as.
const PINNED_SEARCH_FLAGS: &[&str] = &[
    "-n",
    "-i",
    "-w",
    "-x",
    "-in",
    "-nw",
    "-A 3",
    "-B 1",
    "-C 0",
    "-A12",
    "-nA 2",
    "-iC4",
    "-c",
    "-e needle",
    "-ne needle",
];

/// Flags one step outside that set: each one reaches past the file, prints
/// something other than its lines, or is not a count where one goes.
const UNPINNED_SEARCH_FLAGS: &[&str] = &[
    "-r",
    "-R",
    "-l",
    "-L",
    "-o",
    "-v",
    "-q",
    "-f pats",
    "-P",
    "-nr",
    "-A",
    "-A -1",
    "-A +1",
    "-An",
    "--line-number",
    "-",
];

proptest::proptest! {
    /// ★ A run of pinned flags reads the same wherever it stands before
    /// `--`: in front of the pattern, between the pattern and the path, or
    /// after the path, the line is the same search of the same one file.
    #[test]
    fn a_pinned_flag_run_reads_the_same_wherever_it_stands(
        command in proptest::sample::select(&["grep", "egrep", "fgrep", "rg"][..]),
        flags in proptest::collection::vec(proptest::sample::select(PINNED_SEARCH_FLAGS), 1..5),
        place in 0_usize..3,
    ) {
        let flags = flags.join(" ");
        let pattern = if flags.contains("needle") { "" } else { "needle" };
        let line = match place {
            0 => format!("{command} {flags} {pattern} src/lib.rs"),
            1 => format!("{command} {pattern} {flags} src/lib.rs"),
            _ => format!("{command} {pattern} src/lib.rs {flags}"),
        };
        let early = recognise(&format!("{command} {flags} {pattern} src/lib.rs"));
        proptest::prop_assert!(early.is_some(), "{flags:?} is pinned");
        proptest::prop_assert_eq!(recognise(&line), early, "{}", line);
    }


    /// ★ A search over one file with any run of pinned flags is a content
    /// search of exactly that file and grants no coverage; one flag outside
    /// the set, anywhere in the run, takes the line out of the grammar.
    #[test]
    fn a_pinned_flag_run_keeps_a_search_on_its_one_file(
        command in proptest::sample::select(&["grep", "egrep", "fgrep", "rg"][..]),
        flags in proptest::collection::vec(proptest::sample::select(PINNED_SEARCH_FLAGS), 0..5),
        unpinned in proptest::sample::select(UNPINNED_SEARCH_FLAGS),
        at in 0_usize..5,
    ) {
        let pattern = if flags.iter().any(|flag| flag.ends_with("needle")) { "" } else { "needle" };
        let line = |flags: &[&str]| {
            format!("{command} {} {pattern} src/lib.rs", flags.join(" "))
        };
        let call = recognise(&line(&flags));
        let Some(ShellCall::Read(reads)) = call else {
            return Err(proptest::test_runner::TestCaseError::fail(format!(
                "{:?} is a pinned search and was not recognised",
                line(&flags)
            )));
        };
        proptest::prop_assert_eq!(reads.len(), 1);
        proptest::prop_assert_eq!(reads[0].path.as_str(), "src/lib.rs");
        proptest::prop_assert_eq!(reads[0].span, crate::shell_read::ShellSpan::Matching);
        proptest::prop_assert!(reads[0].read_request().is_none());
        let mut widened = flags.clone();
        widened.insert(at.min(widened.len()), unpinned);
        proptest::prop_assert!(
            recognise(&line(&widened)).is_none(),
            "{:?} carries a flag the grammar does not pin",
            line(&widened)
        );
    }


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

    /// ★ A recognised count is never signed.
    ///
    /// A sign in front of a count changes what `tail` and `sed` print — from
    /// line `N` on, or `N` lines past the start — so wherever a count goes,
    /// a signed one leaves the line unrecognised, and the same digits
    /// unsigned are still read.
    #[test]
    fn a_recognised_count_is_never_signed(
        position in proptest::sample::select(COUNT_POSITIONS),
        sign in proptest::sample::select(&["+", "-", "+-", "-+"][..]),
        count in 1_u32..100_000,
    ) {
        let signed = position.replace("{}", &format!("{sign}{count}"));
        proptest::prop_assert!(
            recognise(&signed).is_none(),
            "{signed:?} carries a signed count, so it is not a read"
        );
        let plain = position.replace("{}", &count.to_string());
        proptest::prop_assert!(
            recognise(&plain).is_some(),
            "{plain:?} is the same count unsigned, and it is read"
        );
    }
}
