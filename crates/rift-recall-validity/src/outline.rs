//! The code outline the file-shape digest prints: one line per top-level
//! definition with its kind, its name and the lines its block spans, under
//! the file's line and byte counts.
//!
//! A module of its own because three readers need the same grammar: the
//! extractor that writes the outline, the writer that counts the tokens of
//! each definition's lines, and the renderers that must say what an outline
//! holds without echoing a byte of source text. Keeping the grammar here means
//! none of them parses another's output by a second set of rules.
//!
//! No parser: a definition is a column-zero line whose leading words are a
//! fixed keyword table, and its range is block expansion over the layout —
//! the only range rule. Nothing inside a comment or a string is read as a
//! name unless it sits at column zero in the shape of a definition, and even
//! then only identifier characters survive into the outline.

use core::fmt::Write as _;

/// The extensions whose files get an outline. Code only: the keyword rules
/// below are a code grammar, and a heading or a table in a document is a
/// different shape this outline does not claim to know.
const CODE_EXTENSIONS: &[&str] = &[
    "rs", "py", "pyi", "go", "js", "jsx", "mjs", "cjs", "ts", "tsx", "java", "kt", "kts", "swift",
    "rb", "scala",
];

/// The words that open a definition, and the kind each one names.
const KEYWORDS: &[(&str, DefinitionKind)] = &[
    ("fn", DefinitionKind::Function),
    ("def", DefinitionKind::Function),
    ("func", DefinitionKind::Function),
    ("function", DefinitionKind::Function),
    ("fun", DefinitionKind::Function),
    ("struct", DefinitionKind::Struct),
    ("enum", DefinitionKind::Enum),
    ("union", DefinitionKind::Union),
    ("trait", DefinitionKind::Trait),
    ("protocol", DefinitionKind::Trait),
    ("impl", DefinitionKind::Impl),
    ("mod", DefinitionKind::Module),
    ("module", DefinitionKind::Module),
    ("type", DefinitionKind::Type),
    ("interface", DefinitionKind::Interface),
    ("class", DefinitionKind::Class),
    ("object", DefinitionKind::Object),
    ("const", DefinitionKind::Const),
    ("static", DefinitionKind::Static),
    ("macro_rules!", DefinitionKind::Macro),
];

/// The words a definition line may carry before its keyword.
const MODIFIERS: &[&str] = &[
    "pub",
    "export",
    "default",
    "async",
    "unsafe",
    "extern",
    "public",
    "private",
    "protected",
    "internal",
    "abstract",
    "final",
    "sealed",
    "open",
    "data",
    "override",
    "inline",
    "declare",
];

/// The longest name an outline line carries; a longer one is cut at this
/// many characters, all of them identifier characters.
const MAX_NAME_CHARS: usize = 96;

/// What a top-level definition is, from a closed set, so the first word of an
/// outline line is always Rift's own and never the source's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DefinitionKind {
    /// A function, method or procedure.
    Function,
    /// A struct.
    Struct,
    /// An enum.
    Enum,
    /// A union.
    Union,
    /// A trait or protocol.
    Trait,
    /// An impl block.
    Impl,
    /// A module.
    Module,
    /// A type alias or a type declaration.
    Type,
    /// An interface.
    Interface,
    /// A class.
    Class,
    /// A singleton object.
    Object,
    /// A constant.
    Const,
    /// A static item.
    Static,
    /// A macro.
    Macro,
}

/// One top-level definition: its kind, its name and the lines its block spans.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Definition {
    kind: DefinitionKind,
    name: String,
    first_line: u32,
    last_line: u32,
}

/// A code file's outline: its size and its top-level definitions in file
/// order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileOutline {
    lines: u32,
    bytes: u64,
    definitions: Vec<Definition>,
}

impl DefinitionKind {
    /// The word an outline line opens with.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Function => "fn",
            Self::Struct => "struct",
            Self::Enum => "enum",
            Self::Union => "union",
            Self::Trait => "trait",
            Self::Impl => "impl",
            Self::Module => "mod",
            Self::Type => "type",
            Self::Interface => "interface",
            Self::Class => "class",
            Self::Object => "object",
            Self::Const => "const",
            Self::Static => "static",
            Self::Macro => "macro",
        }
    }

    fn from_word(word: &str) -> Option<Self> {
        KEYWORDS
            .iter()
            .find(|(keyword, _)| *keyword == word)
            .map(|(_, kind)| *kind)
    }
}

impl Definition {
    /// The definition's kind.
    #[must_use]
    pub fn kind(&self) -> DefinitionKind {
        self.kind
    }

    /// The definition's name: identifier characters only.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The first line of the definition's block, 1-based.
    #[must_use]
    pub fn first_line(&self) -> u32 {
        self.first_line
    }

    /// The last line of the definition's block, 1-based and inclusive.
    #[must_use]
    pub fn last_line(&self) -> u32 {
        self.last_line
    }
}

impl FileOutline {
    /// The outline of a code file, or `None` for a path that is not code or
    /// bytes that are not UTF-8: such a file gets no summary at all.
    #[must_use]
    pub fn of(path: &str, source: &[u8]) -> Option<Self> {
        if !is_code_path(path) {
            return None;
        }
        let text = core::str::from_utf8(source).ok()?;
        Some(Self::of_text(text))
    }

    /// The outline of `text`, whatever file it came from.
    pub(crate) fn of_text(text: &str) -> Self {
        let lines: Vec<&str> = text.lines().collect();
        let mut definitions = Vec::new();
        for (at, line) in lines.iter().enumerate() {
            let Some((kind, name)) = definition_at(line) else {
                continue;
            };
            let last = block_end(&lines, at);
            definitions.push(Definition {
                kind,
                name,
                first_line: line_number(at),
                last_line: line_number(last),
            });
        }
        Self {
            lines: u32::try_from(lines.len()).unwrap_or(u32::MAX),
            bytes: u64::try_from(text.len()).unwrap_or(u64::MAX),
            definitions,
        }
    }

    /// The file's line count.
    #[must_use]
    pub fn lines(&self) -> u32 {
        self.lines
    }

    /// The file's byte count.
    #[must_use]
    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    /// The top-level definitions, in file order.
    #[must_use]
    pub fn definitions(&self) -> &[Definition] {
        &self.definitions
    }

    /// The digest text: the shape line, then one line per definition.
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "{}", self.shape_line());
        for definition in &self.definitions {
            let _ = writeln!(
                out,
                "{} {} L{}-L{}",
                definition.kind.as_str(),
                definition.name,
                definition.first_line,
                definition.last_line
            );
        }
        out
    }

    fn shape_line(&self) -> String {
        format!(
            "{} lines, {} bytes, {} top-level defs",
            self.lines,
            self.bytes,
            self.definitions.len()
        )
    }
}

/// Whether a path's extension is one the outline is written for.
#[must_use]
pub fn is_code_path(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    let Some((stem, extension)) = name.rsplit_once('.') else {
        return false;
    };
    !stem.is_empty() && CODE_EXTENSIONS.contains(&extension)
}

/// The text of lines `first..=last` (1-based) of `text`, newlines included —
/// the span a definition's token count is taken over.
#[must_use]
pub fn line_span(text: &str, first: u32, last: u32) -> &str {
    let mut start = None;
    let mut end = 0usize;
    let mut offset = 0usize;
    for (at, line) in text.split_inclusive('\n').enumerate() {
        let number = line_number(at);
        if number == first {
            start = Some(offset);
        }
        offset = offset.saturating_add(line.len());
        if number == last {
            end = offset;
            break;
        }
    }
    match start {
        Some(start) if start <= end => text.get(start..end).unwrap_or(""),
        _ => "",
    }
}

/// The kind and name a column-zero line defines, or `None` when it defines
/// nothing.
fn definition_at(line: &str) -> Option<(DefinitionKind, String)> {
    if line.starts_with(char::is_whitespace) {
        return None;
    }
    let mut rest = line;
    loop {
        rest = rest.trim_start();
        let word_len = rest
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '!'))
            .unwrap_or(rest.len());
        let (word, after) = rest.split_at_checked(word_len)?;
        if word.is_empty() {
            return None;
        }
        let kind = DefinitionKind::from_word(word);
        if let Some(found) = kind.and_then(|kind| definition_name(kind, after)) {
            return Some(found);
        }
        if kind.is_none() && !MODIFIERS.contains(&word) {
            return None;
        }
        rest = skip_modifier_argument(word, after);
    }
}

/// The kind and name after a keyword, or `None` when what follows is not a
/// name.
fn definition_name(kind: DefinitionKind, after: &str) -> Option<(DefinitionKind, String)> {
    let after = skip_balanced(after, '<', '>');
    let name = match kind {
        DefinitionKind::Impl => impl_name(after),
        DefinitionKind::Function => identifier(skip_balanced(after.trim_start(), '(', ')')),
        _ => identifier(after),
    }?;
    Some((kind, name))
}

/// The leading identifier of `text`, unless it is itself a keyword or a
/// modifier.
fn identifier(text: &str) -> Option<String> {
    let text = text.trim_start();
    let starts_well = text
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_');
    if !starts_well {
        return None;
    }
    let name: String = text
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .take(MAX_NAME_CHARS)
        .collect();
    if DefinitionKind::from_word(&name).is_some() || MODIFIERS.contains(&name.as_str()) {
        return None;
    }
    Some(name)
}

/// An impl block's name: its trait and type paths, generics left out, as far
/// as the header reads as paths and the word `for`.
fn impl_name(after: &str) -> Option<String> {
    let mut header = String::new();
    let mut depth = 0usize;
    for c in after.chars() {
        match c {
            '<' => depth = depth.saturating_add(1),
            '>' => depth = depth.saturating_sub(1),
            _ if depth > 0 => (),
            c if c.is_ascii_alphanumeric() || matches!(c, '_' | ':' | ' ') => header.push(c),
            _ => break,
        }
    }
    let tokens: Vec<&str> = header
        .split_whitespace()
        .take_while(|token| *token != "where")
        .collect();
    let first = tokens.first()?;
    if *first == "for" || !first.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_') {
        return None;
    }
    let name: String = tokens.join(" ").chars().take(MAX_NAME_CHARS).collect();
    Some(name)
}

/// `text` past a leading balanced `open … close` run, or `text` unchanged
/// when it does not open with one.
fn skip_balanced(text: &str, open: char, close: char) -> &str {
    if !text.starts_with(open) {
        return text;
    }
    let mut depth = 0usize;
    for (at, c) in text.char_indices() {
        if c == open {
            depth = depth.saturating_add(1);
        } else if c == close {
            depth = depth.saturating_sub(1);
            if depth == 0 {
                return text.get(at.saturating_add(c.len_utf8())..).unwrap_or("");
            }
        }
    }
    ""
}

/// What follows a modifier once its own argument is passed over:
/// `pub(crate)`'s scope and `extern "C"`'s ABI string.
fn skip_modifier_argument<'t>(word: &str, after: &'t str) -> &'t str {
    if word == "pub" {
        return skip_balanced(after, '(', ')');
    }
    if word == "extern" {
        let trimmed = after.trim_start();
        if let Some(quoted) = trimmed.strip_prefix('"') {
            return quoted.split_once('"').map_or("", |(_, rest)| rest);
        }
    }
    after
}

/// The 0-based index of the last line of the block that opens at `start`.
///
/// Block expansion over the layout: an indented or blank line continues the
/// block; a column-zero line that closes a bracket or a `where` clause
/// continues it too; a column-zero `}` or `end` closes it on that line; any
/// other column-zero line ends it on the last non-blank line before.
fn block_end(lines: &[&str], start: usize) -> usize {
    let mut last = start;
    for (at, line) in lines.iter().enumerate().skip(start.saturating_add(1)) {
        if line.trim().is_empty() {
            continue;
        }
        if line.starts_with(char::is_whitespace) || continues_block(line) {
            last = at;
            continue;
        }
        if closes_block(line) {
            return at;
        }
        return last;
    }
    last
}

fn continues_block(line: &str) -> bool {
    line.starts_with([')', ']', '{'])
        || line == "where"
        || line.starts_with("where ")
        || line.starts_with("->")
}

fn closes_block(line: &str) -> bool {
    line.starts_with('}') || line.trim_end() == "end"
}

fn line_number(index: usize) -> u32 {
    u32::try_from(index.saturating_add(1)).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests;
