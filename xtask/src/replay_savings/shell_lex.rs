//! A lexer for the shell calls the read grammar refused, so the census can
//! say what shape each one took.
//!
//! It is a measuring instrument and decides nothing. The grammar in
//! `rift_agent::shell_read` is the only thing that says what a read is, and
//! its helpers are private; this module never answers that question, it only
//! cuts a refused line into members, pipeline stages and words so the census
//! can name the first thing about the line the grammar has no reading for.
//! Where a shape cannot be told without a real shell parse it is left to the
//! census's `other`, never guessed at.

/// One word of a command line, quotes removed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Word {
    /// The word's text with its quotes taken off.
    pub(super) text: String,
    /// Whether a quote produced any of it.
    pub(super) quoted: bool,
    /// Whether it holds something the grammar never pins: an expansion, a
    /// glob, a backslash, a quote that does not wrap the whole word.
    pub(super) unpinned: bool,
}

/// A run of words between two pipes.
#[derive(Clone, Debug)]
pub(super) struct Stage {
    /// The stage's words, in order.
    pub(super) words: Vec<Word>,
    /// Where the stage starts and ends in the lexed text.
    span: (usize, usize),
}

/// One member of a chain: its pipeline stages, in order.
#[derive(Clone, Debug)]
pub(super) struct Member {
    /// The stages, never empty.
    pub(super) stages: Vec<Stage>,
}

/// A command line cut into members.
#[derive(Clone, Debug)]
pub(super) struct Lexed {
    /// The text the spans index into.
    pub(super) source: String,
    /// The chain's members, empty ones dropped.
    pub(super) members: Vec<Member>,
    /// Whether the line redirects anything, a here-document included.
    pub(super) redirected: bool,
    /// Whether the line opens a here-document.
    pub(super) heredoc: bool,
    /// The line with every redirection and its target cut out.
    pub(super) without_redirects: String,
}

/// The separators the lexer cuts at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Sep {
    /// `|` or `|&`: the next stage of one member.
    Pipe,
    /// `&&`, `||`, `;`, a line break or a background `&`: the next member.
    Member,
}

/// One token: a word, a separator, or a cut-out redirection.
#[derive(Clone, Debug)]
enum Token {
    Word(Word, (usize, usize)),
    Sep(Sep),
    Redirect(usize, usize),
}

/// The word being built and where it began.
struct Building {
    word: Word,
    start: usize,
}

/// The walk over one command line.
struct Lexer<'a> {
    source: &'a str,
    at: usize,
    tokens: Vec<Token>,
    current: Option<Building>,
    heredoc: bool,
}

impl Stage {
    /// The stage's own text, as the grammar would be handed it.
    pub(super) fn text<'a>(&self, lexed: &'a Lexed) -> &'a str {
        lexed
            .source
            .get(self.span.0..self.span.1)
            .unwrap_or_default()
    }

    /// The command word, without a directory in front of it.
    pub(super) fn head(&self) -> &str {
        self.words.first().map_or("", |word| {
            word.text.rsplit('/').next().unwrap_or(&word.text)
        })
    }
}

impl Member {
    /// The member's own text, its pipes included.
    pub(super) fn text<'a>(&self, lexed: &'a Lexed) -> &'a str {
        let (Some(first), Some(last)) = (self.stages.first(), self.stages.last()) else {
            return "";
        };
        lexed
            .source
            .get(first.span.0..last.span.1)
            .unwrap_or_default()
    }

    /// Whether any word of the member holds something the grammar never pins.
    pub(super) fn unpinned(&self) -> bool {
        self.stages
            .iter()
            .flat_map(|stage| &stage.words)
            .any(|word| word.unpinned)
    }
}

impl Lexer<'_> {
    /// The character at the cursor, and the one after it.
    fn peek(&self) -> (Option<char>, Option<char>) {
        let mut chars = self.source.get(self.at..).unwrap_or_default().chars();
        (chars.next(), chars.next())
    }

    /// Close the word being built, if there is one.
    fn finish_word(&mut self) {
        if let Some(building) = self.current.take() {
            self.tokens
                .push(Token::Word(building.word, (building.start, self.at)));
        }
    }

    /// Add one character to the word being built.
    fn push_char(&mut self, c: char, unpinned: bool) {
        let at = self.at;
        let building = self.current.get_or_insert_with(|| Building {
            word: Word {
                text: String::new(),
                quoted: false,
                unpinned: false,
            },
            start: at,
        });
        building.word.text.push(c);
        building.word.unpinned |= unpinned;
        self.at += c.len_utf8();
    }

    /// Close the word and record a separator `width` bytes long.
    fn separator(&mut self, sep: Sep, width: usize) {
        self.finish_word();
        self.tokens.push(Token::Sep(sep));
        self.at += width;
    }

    /// One step of the walk.
    fn step(&mut self, c: char, next: Option<char>) {
        match (c, next) {
            (' ' | '\t', _) => {
                self.finish_word();
                self.at += 1;
            }
            ('\\', Some('\n')) => {
                self.finish_word();
                self.at += 2;
            }
            ('\\', Some(escaped)) => {
                self.at += 1;
                self.push_char(escaped, true);
            }
            ('&', Some('&')) | ('|', Some('|')) => self.separator(Sep::Member, 2),
            ('&', Some('>')) | ('>' | '<', _) => self.redirect(),
            ('\n' | ';' | '&', _) => self.separator(Sep::Member, 1),
            ('|', Some('&')) => self.separator(Sep::Pipe, 2),
            ('|', _) => self.separator(Sep::Pipe, 1),
            ('\'' | '"', _) => self.quote(c),
            _ => {
                let unpinned = !bare_char(c);
                self.push_char(c, unpinned);
            }
        }
    }

    /// A quoted run, which the grammar pins only when it is the whole word.
    fn quote(&mut self, mark: char) {
        let partial = self.current.is_some();
        let start = self.at;
        self.at += 1;
        let mut text = String::new();
        let mut closed = false;
        let mut expands = false;
        while let (Some(inner), next) = self.peek() {
            self.at += inner.len_utf8();
            if inner == mark {
                closed = true;
                break;
            }
            // Inside double quotes a backslash escapes only the four
            // characters the shell still treats specially there, and a
            // newline; before anything else it is kept as written, which is
            // a value the line pins rather than one the shell computes.
            if mark == '"' && inner == '\\' && next.is_some_and(escapes_in_double_quotes) {
                expands = true;
                text.push(inner);
                if let Some(escaped) = next {
                    self.at += escaped.len_utf8();
                    text.push(escaped);
                }
                continue;
            }
            expands |= mark == '"' && matches!(inner, '$' | '`');
            text.push(inner);
        }
        // The closing quote must end the word, as it must for the grammar.
        let ends_here = matches!(
            self.peek().0,
            None | Some(' ' | '\t' | '\n' | ';' | '|' | '&' | '>' | '<')
        );
        let building = self.current.get_or_insert_with(|| Building {
            word: Word {
                text: String::new(),
                quoted: false,
                unpinned: false,
            },
            start,
        });
        building.word.text.push_str(&text);
        building.word.quoted = true;
        building.word.unpinned |= partial || expands || !closed || !ends_here;
    }

    /// A redirection: its operator and its target are cut out of the line.
    fn redirect(&mut self) {
        let fd = self.current.as_ref().is_some_and(|building| {
            !building.word.quoted && building.word.text.bytes().all(|b| b.is_ascii_digit())
        });
        let start = match self.current.take() {
            Some(building) if fd => building.start,
            other => {
                self.current = other;
                self.finish_word();
                self.at
            }
        };
        if self
            .source
            .get(self.at..)
            .is_some_and(|rest| rest.starts_with("<<"))
        {
            self.heredoc = true;
            self.at = self.source.len();
            self.tokens.push(Token::Redirect(start, self.at));
            return;
        }
        while let (Some('>' | '<' | '&' | '|'), _) = self.peek() {
            self.at += 1;
        }
        let rest = self.source.get(self.at..).unwrap_or_default();
        let skipped = rest.len() - rest.trim_start_matches([' ', '\t']).len();
        self.at += skipped;
        let target = self.source.get(self.at..).unwrap_or_default();
        let length = target
            .find([' ', '\t', '\n', ';', '|', '&'])
            .unwrap_or(target.len());
        self.at += length;
        self.tokens.push(Token::Redirect(start, self.at));
    }
}

/// Whether a character may stand unquoted in a word the grammar pins: the
/// same small positive set the grammar's own word walk admits, restated
/// because the grammar keeps its helper private.
const fn bare_char(c: char) -> bool {
    c.is_ascii_alphanumeric()
        || matches!(c, '_' | '.' | '/' | '-' | '+' | '=' | ',' | ':' | '@' | '%')
        || (!c.is_ascii() && !matches!(c, '\u{80}'..='\u{9f}'))
}

/// Whether a backslash before this character, inside double quotes, is an
/// escape the shell resolves rather than a character it keeps.
const fn escapes_in_double_quotes(c: char) -> bool {
    matches!(c, '$' | '`' | '"' | '\\' | '\n')
}

/// The line with every double-quoted word whose backslashes the shell keeps as
/// written respelled in single quotes, or [`None`] when there is no such word.
///
/// The two spellings pass the shell the same word, and the grammar pins the
/// second, so whether the respelled line is recognised says whether those
/// backslashes are the whole of the reason the line was refused. A word with
/// an expansion, a `!`, an escape or a `'` in it is left as it was.
pub(super) fn literal_backslashes_single_quoted(command: &str) -> Option<String> {
    let mut out = String::with_capacity(command.len());
    let mut changed = false;
    let mut rest = command;
    while let Some(at) = rest.find(['\'', '"', '\\']) {
        let (before, from) = rest.split_at(at);
        out.push_str(before);
        let mut chars = from.chars();
        let mark = chars.next()?;
        let body = chars.as_str();
        let (taken, close) = match mark {
            '\\' => (body.chars().next().map_or(0, char::len_utf8), 0),
            _ => (body.find(mark)?, 1),
        };
        let inner = body.get(..taken)?;
        match respelled(mark, inner) {
            Some(single) => {
                out.push_str(&single);
                changed = true;
            }
            None => out.push_str(from.get(..1 + taken + close)?),
        }
        rest = body.get(taken + close..)?;
    }
    out.push_str(rest);
    changed.then_some(out)
}

/// One double-quoted word's text in single quotes, when the two pass the same
/// word and the double-quoted one holds a backslash it keeps.
fn respelled(mark: char, inner: &str) -> Option<String> {
    let plain = !inner.contains(['$', '`', '!', '\'']);
    let kept = inner.match_indices('\\').all(|(at, _)| {
        !inner
            .get(at + 1..)
            .and_then(|r| r.chars().next())
            .is_some_and(escapes_in_double_quotes)
    });
    (mark == '"' && plain && kept && inner.contains('\\')).then(|| format!("'{inner}'"))
}

/// Cut a command line into members, stages and words.
pub(super) fn lex(command: &str) -> Lexed {
    let (tokens, heredoc) = tokens_of(command);
    let without_redirects = cut_redirects(command, &tokens);
    let redirected = tokens.iter().any(|t| matches!(t, Token::Redirect(..)));
    Lexed {
        source: command.to_owned(),
        members: members_of(tokens),
        redirected,
        heredoc,
        without_redirects,
    }
}

/// The tokens of a line, and whether it opened a here-document.
fn tokens_of(command: &str) -> (Vec<Token>, bool) {
    let mut lexer = Lexer {
        source: command,
        at: 0,
        tokens: Vec::new(),
        current: None,
        heredoc: false,
    };
    while let (Some(c), next) = lexer.peek() {
        lexer.step(c, next);
    }
    lexer.finish_word();
    (lexer.tokens, lexer.heredoc)
}

/// The line with every redirection's span cut out.
fn cut_redirects(command: &str, tokens: &[Token]) -> String {
    let mut out = String::new();
    let mut from = 0;
    for token in tokens {
        if let Token::Redirect(start, end) = token {
            out.push_str(command.get(from..*start).unwrap_or_default());
            from = *end;
        }
    }
    out.push_str(command.get(from..).unwrap_or_default());
    out.trim_end().to_owned()
}

/// Fold the tokens into members of stages, dropping the empty ones.
fn members_of(tokens: Vec<Token>) -> Vec<Member> {
    let mut members: Vec<Member> = Vec::new();
    let mut stages: Vec<Stage> = Vec::new();
    let mut stage = Stage {
        words: Vec::new(),
        span: (0, 0),
    };
    for token in tokens {
        match token {
            Token::Word(word, (start, end)) => {
                if stage.words.is_empty() {
                    stage.span.0 = start;
                }
                stage.span.1 = end;
                stage.words.push(word);
            }
            Token::Redirect(..) => (),
            Token::Sep(sep) => {
                close_stage(&mut stages, &mut stage);
                if sep == Sep::Member && !stages.is_empty() {
                    members.push(Member {
                        stages: std::mem::take(&mut stages),
                    });
                }
            }
        }
    }
    close_stage(&mut stages, &mut stage);
    if !stages.is_empty() {
        members.push(Member { stages });
    }
    members
}

/// Move a finished stage onto its member, if it has any words.
fn close_stage(stages: &mut Vec<Stage>, stage: &mut Stage) {
    let done = std::mem::replace(
        stage,
        Stage {
            words: Vec::new(),
            span: (0, 0),
        },
    );
    if !done.words.is_empty() {
        stages.push(done);
    }
}

#[cfg(test)]
mod tests {
    use super::lex;

    fn heads(command: &str) -> Vec<Vec<String>> {
        lex(command)
            .members
            .iter()
            .map(|member| member.stages.iter().map(|s| s.head().to_owned()).collect())
            .collect()
    }

    #[test]
    fn a_line_is_cut_at_chains_and_pipes_but_never_inside_a_quote() {
        assert_eq!(
            heads("cd a && sed -n '1,5p' f | grep 'x|y'; echo; tail f"),
            [vec!["cd"], vec!["sed", "grep"], vec!["echo"], vec!["tail"]]
        );
        assert_eq!(heads("a || b & c\nd"), [["a"], ["b"], ["c"], ["d"]]);
    }

    #[test]
    fn a_redirection_and_its_target_are_cut_out() {
        let lexed = lex("cat f 2>&1 | head -n 5 > out.txt");
        assert!(lexed.redirected);
        assert_eq!(lexed.without_redirects, "cat f  | head -n 5");
        assert_eq!(lex("cat f &>/dev/null").without_redirects, "cat f");
        let heredoc = lex("python3 - <<'EOF'\nprint(1)\nEOF");
        assert!(heredoc.heredoc);
        assert_eq!(heads("python3 - <<'EOF'\nprint(1)\nEOF"), [["python3"]]);
    }

    #[test]
    fn a_word_the_grammar_never_pins_is_marked() {
        let words = |command: &str| -> Vec<bool> {
            lex(command).members[0].stages[0]
                .words
                .iter()
                .map(|word| word.unpinned)
                .collect()
        };
        assert_eq!(
            words("cat $F ~/a *.rs 'ok'"),
            [false, true, true, true, false]
        );
        assert_eq!(words("grep \"a$b\" x'y' a\\ b"), [false, true, true, true]);
        assert_eq!(
            words(r#"grep "a\|b" 'c\d' "e\$f" "g\"h""#),
            [false, false, false, true, true]
        );
        let member = &lex("sed -n '1,5p' f").members[0];
        assert!(!member.unpinned());
        assert_eq!(member.text(&lex("sed -n '1,5p' f")), "sed -n '1,5p' f");
    }
}
