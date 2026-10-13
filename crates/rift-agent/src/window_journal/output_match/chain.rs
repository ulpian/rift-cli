//! A command line cut into the members whose output a delivery may be
//! aligned to, and the words each one names.
//!
//! Not a parse of shell, and nothing here decides what a member read: the
//! words are only the names a member could own output under. A delivered
//! line counts only once its bytes are found in a file a member names, so a
//! word read wrong costs a saving and never a claim. Pipe stages stay inside
//! their member, because only what reaches the agent counts, and a stage's
//! own output never does.

/// The longest command line cut at all.
pub const MAX_CHAIN_COMMAND_BYTES: usize = 16 * 1024;

/// The most members one command line is cut into; a longer chain is not cut.
pub const MAX_CHAIN_MEMBERS: usize = 32;

/// One member of a command line.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChainMember {
    /// The words of every stage, quotes removed, in order.
    pub words: Vec<String>,
    /// Whether the member changes directory. It owns no output.
    pub cd: bool,
    /// The directory the member runs in, relative to where the line started
    /// (or absolute), as the line's earlier `cd` members spelled it; `None`
    /// where the line started.
    pub dir: Option<String>,
    /// Whether an earlier `cd` left the directory unknown — no operand, or a
    /// computed one. Such a member names nothing.
    pub dir_unknown: bool,
}

/// The members of `command`, or `None` when the line is too long, has too
/// many members, or leaves a quote open.
///
/// Members are split at `&&`, `||`, `;` and line breaks outside quotes; a
/// single `|` or `&` stays inside its member.
#[must_use]
pub fn chain_members(command: &str) -> Option<Vec<ChainMember>> {
    if command.len() > MAX_CHAIN_COMMAND_BYTES {
        return None;
    }
    let mut members = Vec::new();
    let mut place = Place::default();
    for words in split_members(command)? {
        if words.is_empty() {
            continue;
        }
        if members.len() >= MAX_CHAIN_MEMBERS {
            return None;
        }
        members.push(place.member(words));
    }
    Some(members)
}

/// Where the line stands as its members run.
#[derive(Default)]
struct Place {
    dir: Option<String>,
    unknown: bool,
}

impl Place {
    /// The member these words make, and the directory it leaves behind.
    fn member(&mut self, words: Vec<String>) -> ChainMember {
        let cd = matches!(words.first().map(String::as_str), Some("cd" | "pushd"));
        let member = ChainMember {
            cd,
            dir: self.dir.clone(),
            dir_unknown: self.unknown,
            words,
        };
        if cd {
            self.change_dir(member.words.get(1).map(String::as_str));
        }
        member
    }

    /// Follow one `cd`: an absolute target replaces the directory, a
    /// relative one is joined to it, and anything else makes it unknown.
    fn change_dir(&mut self, target: Option<&str>) {
        let Some(target) = target.filter(|t| !t.is_empty() && *t != "-" && !t.contains('$')) else {
            self.unknown = true;
            return;
        };
        self.dir = Some(match (&self.dir, target.starts_with('/')) {
            (Some(dir), false) => format!("{}/{target}", dir.trim_end_matches('/')),
            _ => target.to_owned(),
        });
    }
}

/// The words of each member, or `None` when a quote is left open.
fn split_members(command: &str) -> Option<Vec<Vec<String>>> {
    let mut members: Vec<Vec<String>> = Vec::new();
    let mut words: Vec<String> = Vec::new();
    let mut word = String::new();
    let mut quote: Option<char> = None;
    let mut chars = command.chars().peekable();
    while let Some(ch) = chars.next() {
        match (quote, ch) {
            (Some(open), c) if c == open => quote = None,
            (Some('"') | None, '\\') => word.extend(chars.next()),
            (None, '\'' | '"') => quote = Some(ch),
            (None, '\n' | ';') => end_member(&mut members, &mut words, &mut word),
            (None, '&' | '|') if chars.peek() == Some(&ch) => {
                chars.next();
                end_member(&mut members, &mut words, &mut word);
            }
            (None, ' ' | '\t' | '&' | '|') => end_word(&mut words, &mut word),
            (_, c) => word.push(c),
        }
    }
    if quote.is_some() {
        return None;
    }
    end_member(&mut members, &mut words, &mut word);
    Some(members)
}

/// Close the current word, if it has any characters.
fn end_word(words: &mut Vec<String>, word: &mut String) {
    if !word.is_empty() {
        words.push(std::mem::take(word));
    }
}

/// Close the current word and member.
fn end_member(members: &mut Vec<Vec<String>>, words: &mut Vec<String>, word: &mut String) {
    end_word(words, word);
    members.push(std::mem::take(words));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(member: &ChainMember) -> Vec<&str> {
        member.words.iter().map(String::as_str).collect()
    }

    /// ★ A chain is cut at its joins and a pipeline stays one member, with
    /// each member's directory as the line's `cd` members left it.
    #[test]
    fn a_chain_is_cut_at_its_joins_and_a_pipeline_is_one_member() {
        let members = chain_members(
            "cd crates/a && grep -n 'x y' src/lib.rs | head -5; cat \"b c.rs\" || true",
        )
        .unwrap();
        assert_eq!(members.len(), 4);
        assert!(members[0].cd);
        assert_eq!(
            words(&members[1]),
            ["grep", "-n", "x y", "src/lib.rs", "head", "-5"]
        );
        assert_eq!(members[1].dir.as_deref(), Some("crates/a"));
        assert_eq!(words(&members[2]), ["cat", "b c.rs"]);
        assert_eq!(members[3].dir.as_deref(), Some("crates/a"));
    }

    /// A `cd` with no operand, `-`, or a computed one leaves every later
    /// member's directory unknown; a second relative `cd` joins the first.
    #[test]
    fn a_cd_nobody_can_follow_leaves_the_directory_unknown() {
        let members = chain_members("cd a; cd b; cat x").unwrap();
        assert_eq!(members[2].dir.as_deref(), Some("a/b"));
        assert!(!members[2].dir_unknown);
        let members = chain_members("cd $HOME && cat x").unwrap();
        assert!(members[1].dir_unknown);
        let members = chain_members("cd /abs && cat x").unwrap();
        assert_eq!(members[1].dir.as_deref(), Some("/abs"));
    }

    /// An open quote, an overlong line or too many members is not cut.
    #[test]
    fn an_unclosed_quote_or_an_overlong_chain_is_not_cut() {
        assert_eq!(chain_members("cat 'a"), None);
        assert_eq!(
            chain_members(&"x".repeat(MAX_CHAIN_COMMAND_BYTES + 1)),
            None
        );
        assert_eq!(chain_members(&"true;".repeat(MAX_CHAIN_MEMBERS + 1)), None);
        assert_eq!(chain_members("").map(|m| m.len()), Some(0));
    }
}
