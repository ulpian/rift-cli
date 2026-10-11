//! The code outline the file-shape digest prints: one line per top-level
//! definition with its kind, its name and the lines its block spans, under
//! the file's line and byte counts. A Markdown file is outlined the same way
//! with its headings as the definitions ([`markdown`]).
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

mod markdown;

use core::fmt::Write as _;

use crate::summary::ValidSummary;

/// The extensions whose files get an outline. Code only: the keyword rules
/// below are a code grammar, and a heading or a table in a document is a
/// different shape this outline does not claim to know.
const CODE_EXTENSIONS: &[&str] = &[
    "rs", "py", "pyi", "go", "js", "jsx", "mjs", "cjs", "ts", "tsx", "java", "kt", "kts", "swift",
    "rb", "scala",
];

/// The extensions whose files are outlined by heading. A heading is the one
/// structure a Markdown file states about itself in a form a line scan reads
/// exactly, so it is the document's counterpart of a top-level definition.
const MARKDOWN_EXTENSIONS: &[&str] = &["md", "markdown"];

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
    /// A Markdown heading of this level, one to six.
    Heading(u8),
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

/// The size an outline's shape line states: what a surface may say about a
/// file without naming anything inside it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OutlineShape {
    /// The file's line count.
    pub lines: u32,
    /// The file's byte count.
    pub bytes: u64,
    /// How many top-level definitions the outline lists.
    pub definitions: u32,
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
            Self::Heading(1) => "h1",
            Self::Heading(2) => "h2",
            Self::Heading(3) => "h3",
            Self::Heading(4) => "h4",
            Self::Heading(5) => "h5",
            Self::Heading(_) => "h6",
        }
    }

    /// The kind an outline line's first word names, from the closed set
    /// [`DefinitionKind::as_str`] writes, or `None` for any other word.
    #[must_use]
    pub fn from_label(word: &str) -> Option<Self> {
        let headings = (1..=6).map(Self::Heading);
        KEYWORDS
            .iter()
            .map(|(_, kind)| *kind)
            .chain(headings)
            .find(|kind| kind.as_str() == word)
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
    /// The outline of a code or Markdown file, or `None` for a path that is
    /// neither or bytes that are not UTF-8: such a file gets no summary at
    /// all.
    #[must_use]
    pub fn of(path: &str, source: &[u8]) -> Option<Self> {
        if !is_outlinable_path(path) {
            return None;
        }
        let text = core::str::from_utf8(source).ok()?;
        if is_markdown_path(path) {
            return Some(Self::of_markdown(text));
        }
        Some(Self::of_text(text))
    }

    /// The heading outline of Markdown `text`.
    pub(crate) fn of_markdown(text: &str) -> Self {
        let lines: Vec<&str> = text.lines().collect();
        Self {
            lines: u32::try_from(lines.len()).unwrap_or(u32::MAX),
            bytes: u64::try_from(text.len()).unwrap_or(u64::MAX),
            definitions: markdown::headings(&lines),
        }
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

/// Whether a path's extension is one the code outline is written for.
#[must_use]
pub fn is_code_path(path: &str) -> bool {
    extension_of(path).is_some_and(|extension| CODE_EXTENSIONS.contains(&extension))
}

/// Whether a path's extension is one the heading outline is written for.
#[must_use]
pub fn is_markdown_path(path: &str) -> bool {
    extension_of(path).is_some_and(|extension| MARKDOWN_EXTENSIONS.contains(&extension))
}

/// Whether a path gets an outline at all: code by definition, Markdown by
/// heading. The one test the binder, the serving gate and the outline cache
/// share, so a file one of them outlines is never one another refuses.
#[must_use]
pub fn is_outlinable_path(path: &str) -> bool {
    is_code_path(path) || is_markdown_path(path)
}

/// The extension of a path's last component, when it has a stem as well.
fn extension_of(path: &str) -> Option<&str> {
    let name = path.rsplit('/').next().unwrap_or(path);
    let (stem, extension) = name.rsplit_once('.')?;
    (!stem.is_empty()).then_some(extension)
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

/// An outline digest with every name left out — the shape line, then each
/// definition by its position, kind and lines — or `None` when the digest is
/// not an outline in this grammar.
///
/// For a surface that must not render source text at all: a name is an
/// identifier, but it is still a span of a file someone else may have
/// written, while a position, a closed kind word and two numbers are Rift's
/// own. Anything that does not parse is refused whole rather than passed
/// through in part.
#[must_use]
pub fn unnamed_outline(digest: &str) -> Option<String> {
    let mut lines = digest.lines();
    let mut out = shape_line_of(lines.next()?)?.to_owned();
    for (at, line) in lines.enumerate() {
        let (kind, range) = definition_line(line)?;
        let _ = write!(out, "; #{} {kind} {range}", at.saturating_add(1));
    }
    Some(out)
}

/// The shape a valid summary's outline states, or `None` when the summary is
/// not an outline.
///
/// Takes a [`ValidSummary`] rather than text, so a shape can only ever come
/// from a summary proven current against the checkout.
#[must_use]
pub fn outline_shape(summary: &ValidSummary) -> Option<OutlineShape> {
    let digest = core::str::from_utf8(summary.digest()).ok()?;
    let line = shape_line_of(digest.lines().next()?)?;
    let mut counts = line
        .split(", ")
        .map(|part| part.split(' ').next().unwrap_or_default());
    Some(OutlineShape {
        lines: counts.next()?.parse().ok()?,
        bytes: counts.next()?.parse().ok()?,
        definitions: counts.next()?.parse().ok()?,
    })
}

/// The definitions a valid summary's outline lists, in file order, or `None`
/// when the summary is not an outline in this grammar.
///
/// Takes a [`ValidSummary`] for [`outline_shape`]'s reason. The read is the
/// grammar [`FileOutline::render`] writes and nothing looser: a line that does
/// not parse refuses the whole outline, so a reader never holds half of one.
#[must_use]
pub fn outline_definitions(summary: &ValidSummary) -> Option<Vec<Definition>> {
    let digest = core::str::from_utf8(summary.digest()).ok()?;
    let mut lines = digest.lines();
    shape_line_of(lines.next()?)?;
    lines.map(parsed_definition).collect()
}

/// One definition line read back into the definition it was written from.
fn parsed_definition(line: &str) -> Option<Definition> {
    let (kind, rest) = line.split_once(' ')?;
    let (name, range) = rest.rsplit_once(' ')?;
    let (lo, hi) = range.strip_prefix('L')?.split_once("-L")?;
    let is_number = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if name.is_empty() || !is_number(lo) || !is_number(hi) {
        return None;
    }
    Some(Definition {
        kind: DefinitionKind::from_label(kind)?,
        name: name.to_owned(),
        first_line: lo.parse().ok()?,
        last_line: hi.parse().ok()?,
    })
}

/// `line` when it is an outline's shape line.
fn shape_line_of(line: &str) -> Option<&str> {
    let mut parts = line.split(", ");
    let counts = [" lines", " bytes", " top-level defs"];
    for suffix in counts {
        let number = parts.next()?.strip_suffix(suffix)?;
        if number.is_empty() || !number.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
    }
    if parts.next().is_some() {
        return None;
    }
    Some(line)
}

/// The kind word and the `Lx-Ly` range of one outline definition line.
fn definition_line(line: &str) -> Option<(&'static str, &str)> {
    let (kind, rest) = line.split_once(' ')?;
    let (_, range) = rest.rsplit_once(' ')?;
    let kind = DefinitionKind::from_label(kind)?.as_str();
    let (lo, hi) = range.strip_prefix('L')?.split_once("-L")?;
    let is_number = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if !is_number(lo) || !is_number(hi) {
        return None;
    }
    Some((kind, range))
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
