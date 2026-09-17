//! Documents become Markdown here, and nowhere else.
//!
//! An agent that opens `spec.docx`, `costs.xlsx` or `rfc.pdf` with a plain file
//! read gets zip bytes, an OLE stream or a page image: format noise it pays for
//! and cannot use. This crate converts those bytes to GitHub-flavoured Markdown
//! behind one seam, so that the conversion is a decision made once — with its
//! caps, its refusals and its determinism — and every caller (the hook that
//! substitutes a sidecar path, the detached worker that converts ahead of time,
//! the `doc-read` verb) shares it.
//!
//! What the seam promises, and why each promise is here:
//!
//! - **Total.** [`DocConverter::convert`] returns a [`DocError`] rather than
//!   panicking or blocking, for every input including hostile ones. A caller on
//!   a hook path turns any error into "leave the read alone", which is the
//!   conservative direction.
//! - **Capped before it is read.** Inputs over
//!   [`ConvertCaps::max_input_bytes`] are refused without being parsed at all,
//!   and tables are cut to [`ConvertCaps::table_rows`] rows apiece, so a 400 MB
//!   spreadsheet cannot become a context-sized answer.
//! - **Deterministic.** The same bytes and the same caps produce the same
//!   Markdown, byte for byte. Sidecars are content-addressed, so a conversion
//!   that varied between runs would serve one member of a worktree family
//!   something a sibling never saw.
//! - **Offline.** The engine is pure Rust and reaches no network. The hosted
//!   OCR path that this project publishes lives in its Node and Python
//!   bindings, never in the crate, and the resolved-graph check in `ci-meta`
//!   is what keeps that true rather than this sentence.
//!
//! Nothing here reads a file, writes a file, opens a store or knows what a
//! repository is: the caller supplies bytes and gets a [`Markdown`] back. Where
//! the sidecar lives and how it is keyed is the caller's decision, and it is
//! keyed by the repository family rather than by a checkout.
//!
//! No timing lives inside a conversion either. The 250 ms deadline is the
//! caller's, and [`ChildProcessConverter`] is how it is actually kept: the
//! parsers run in a one-shot child process, where a deadline can be enforced
//! by killing something and a hostile container dies in another address
//! space. `rift` links this crate with its `doc` feature and is that child —
//! re-spawned on its hidden `hooks doc` route — so the engine is in the one
//! binary and still never parses inside the process a hook is.
#![forbid(unsafe_code)]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]

/// The conversion engine's version, as pinned in the workspace manifest.
///
/// Readable without the engine feature on purpose: a sidecar's cache key is
/// the source hash joined with this string and the output-affecting caps, and
/// the binary that writes sidecars does not link the parsers. A test binds it
/// to the pin, since a constant that drifted from the version actually linked
/// would serve yesterday's Markdown under tomorrow's parser's name.
pub const ENGINE_VERSION: &str = "0.2.4";

mod child;
#[cfg(feature = "doc")]
mod engine;
mod tables;
pub mod wire;

pub use child::{
    output_ceiling, ChildProcessConverter, DEFAULT_BUDGET, MIN_OUTPUT_BYTES, OUTPUT_MULTIPLE,
};
#[cfg(feature = "doc")]
pub use engine::AnydocConverter;
pub use tables::cap_tables;

use rift_core::RiftError;

/// The document extensions Rift converts — and no others.
///
/// Fourteen: the ones this program decided to convert. The engine accepts a
/// few further spellings of the same containers (`.xlsb`, `.pptm`, `.pps`) and
/// they are deliberately not here, because a format nobody chose is a format
/// nobody measured. `.md`, `.txt` and every source extension are absent for the
/// opposite reason: an agent reads those perfectly well already, and converting
/// them would replace a file it asked for with a rewrite of that file.
pub const SUPPORTED_EXTENSIONS: [&str; 14] = [
    "csv", "doc", "docm", "docx", "epub", "odp", "ods", "odt", "pdf", "ppt", "pptx", "rtf", "xls",
    "xlsx",
];

/// Rows kept per table before the rest are summarized.
pub const DEFAULT_TABLE_ROWS: usize = 200;

/// The largest input converted at all, in bytes (16 MiB).
pub const DEFAULT_MAX_INPUT_BYTES: u64 = 16 * 1024 * 1024;

/// Characters per page below which a PDF counts as having no usable text layer.
pub const DEFAULT_PDF_MIN_CHARS_PER_PAGE: u32 = 200;

/// Whether `ext` names a document this crate converts.
///
/// A leading `.` is accepted and the comparison is case-insensitive, because
/// the callers get their extension from a path and paths carry both spellings.
#[must_use]
pub fn is_supported_extension(ext: &str) -> bool {
    let ext = ext.strip_prefix('.').unwrap_or(ext);
    SUPPORTED_EXTENSIONS
        .iter()
        .any(|known| ext.eq_ignore_ascii_case(known))
}

/// The ceilings a conversion runs under.
///
/// These are policy values (`[doc]` in a repository's configuration), passed in
/// rather than read here so that the crate has no opinion about where policy
/// lives and a test can state a cap of two rows in one line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConvertCaps {
    /// Body rows kept per table; the rest become a one-line count.
    pub table_rows: usize,
    /// Inputs larger than this are refused unparsed.
    pub max_input_bytes: u64,
    /// A PDF averaging fewer characters per page than this has no text layer
    /// worth substituting for the page images the agent would otherwise see.
    pub pdf_min_chars_per_page: u32,
}

impl Default for ConvertCaps {
    fn default() -> Self {
        Self {
            table_rows: DEFAULT_TABLE_ROWS,
            max_input_bytes: DEFAULT_MAX_INPUT_BYTES,
            pdf_min_chars_per_page: DEFAULT_PDF_MIN_CHARS_PER_PAGE,
        }
    }
}

/// One table in a converted document, under the heading it appeared beneath.
///
/// Named for the spreadsheet case, which is the one the caps exist for: a
/// workbook renders one table per sheet, so a sheet's name is the heading above
/// its table. A table inside a text document is reported the same way, with
/// whatever heading it sits under (or an empty name at the top of a file).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SheetInfo {
    /// The nearest heading above the table; empty when there is none.
    pub name: String,
    /// Body rows the document actually carries (the header row is not one).
    pub rows: u32,
    /// Body rows kept in the Markdown.
    pub rows_kept: u32,
}

impl SheetInfo {
    /// Whether rows were dropped to fit the cap.
    #[must_use]
    pub fn truncated(&self) -> bool {
        self.rows_kept < self.rows
    }
}

/// A converted document.
///
/// `text` is the body only. The provenance comment is added by whichever
/// caller writes the sidecar ([`sidecar_text`]), because only the caller knows
/// the source's name and hash — and because a body that already carried its own
/// provenance could not be compared against a re-conversion for equality.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Markdown {
    /// GitHub-flavoured Markdown, capped.
    pub text: String,
    /// Pages, where the format has them and the engine reports them.
    pub pages: Option<u32>,
    /// 1-indexed pages carrying no text layer (scanned or image-only).
    pub thin_pages: Vec<u32>,
    /// Every table in the document, in order.
    pub sheets: Vec<SheetInfo>,
}

impl Markdown {
    /// Whether this conversion is too thin to stand in for the original.
    ///
    /// True when the engine named pages with no text layer, or when the text
    /// averages under [`ConvertCaps::pdf_min_chars_per_page`] over a known page
    /// count. A thin conversion is still returned rather than refused — a
    /// caller has to be able to say "this PDF is scanned" instead of "this PDF
    /// failed", and the two lead to different behaviour — but no caller may
    /// substitute it for the read the agent asked for.
    #[must_use]
    pub fn is_thin(&self, caps: &ConvertCaps) -> bool {
        if !self.thin_pages.is_empty() {
            return true;
        }
        match self.pages {
            Some(pages) if pages > 0 => {
                let chars = self.text.chars().count() as u64;
                chars / u64::from(pages) < u64::from(caps.pdf_min_chars_per_page)
            }
            _ => false,
        }
    }
}

/// Why a document was not converted.
///
/// Every variant carries an identifier or a number and never a fragment of the
/// document: these strings reach logs and, through the callers, an agent's
/// context, and a parser's complaint about byte 4,096 has been known to quote
/// it.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DocError {
    /// The input is over [`ConvertCaps::max_input_bytes`]; it was not parsed.
    #[error("document is {bytes} bytes, over the {cap}-byte conversion cap")]
    TooLarge {
        /// The input's size.
        bytes: u64,
        /// The cap it exceeded.
        cap: u64,
    },
    /// The extension is not one of [`SUPPORTED_EXTENSIONS`].
    #[error("`{0}` is not a document extension rift converts")]
    UnsupportedExtension(String),
    /// The engine could not produce a usable document.
    ///
    /// `code` is the engine's own stable classification (`encrypted`,
    /// `malformed`, `resourceLimit`, …), plus `panic` for an engine that
    /// unwound: unwinding is a defect in a parser, and a caller must treat it
    /// exactly as it treats a refusal rather than propagating it.
    #[error("the engine could not read the document ({code})")]
    Engine {
        /// The engine's classification of the failure.
        code: &'static str,
    },
}

impl From<DocError> for RiftError {
    fn from(error: DocError) -> Self {
        let message = error.to_string();
        match error {
            DocError::TooLarge { .. } => RiftError::PolicyDenied(message),
            DocError::UnsupportedExtension(_) | DocError::Engine { .. } => {
                RiftError::Validity(message)
            }
        }
    }
}

/// The one way a document becomes Markdown.
///
/// A trait rather than a function because the engine may move out of this
/// process: if the shipped binary cannot afford fourteen parsers, the same
/// conversion is reached by spawning a helper, and every caller keeps calling
/// `convert`.
pub trait DocConverter {
    /// Convert `bytes`, which `ext` names the format of, under `caps`.
    ///
    /// `ext` is the extension with or without its leading dot, in any case. It
    /// selects the parser: the bytes are not sniffed, so a file's name decides
    /// how it is read, exactly as it decided that Rift would look at it at all.
    ///
    /// # Errors
    /// [`DocError::TooLarge`] before any parsing, [`DocError::UnsupportedExtension`]
    /// for anything outside [`SUPPORTED_EXTENSIONS`], and [`DocError::Engine`]
    /// when the document is encrypted, malformed or otherwise unreadable.
    fn convert(&self, bytes: &[u8], ext: &str, caps: &ConvertCaps) -> Result<Markdown, DocError>;
}

/// Characters of the source hash carried in the provenance comment.
pub const PROVENANCE_SHA_CHARS: usize = 12;

/// The first line of every sidecar: what this file is and what it came from.
///
/// A converted document is not the file the agent named, and the line says so
/// before a single converted byte. The hash is the source's, abbreviated, so a
/// reader can tell two conversions of two versions apart without carrying
/// sixty-four characters of hexadecimal.
///
/// The name is neutralized (`<`, `>` and control characters become `_`, and it
/// is cut at 120 characters) because a file name is chosen by whoever wrote the
/// file, and this line is HTML-comment-shaped: a name containing the comment
/// terminator would end the comment early and put the rest of the name in the
/// document as text.
#[must_use]
pub fn provenance_line(basename: &str, source_sha256_hex: &str) -> String {
    format!(
        "<!-- converted by rift from {} sha256:{} -->",
        neutralized(basename),
        short_hash(source_sha256_hex)
    )
}

/// A whole sidecar: the provenance line, then the converted body.
///
/// Always ends in a newline, so appending is never ambiguous.
#[must_use]
pub fn sidecar_text(markdown: &Markdown, basename: &str, source_sha256_hex: &str) -> String {
    let mut out = provenance_line(basename, source_sha256_hex);
    out.push('\n');
    if !markdown.text.is_empty() {
        out.push('\n');
        out.push_str(&markdown.text);
        if !out.ends_with('\n') {
            out.push('\n');
        }
    }
    out
}

/// A file name that cannot end the comment it is written into.
fn neutralized(basename: &str) -> String {
    let mut out = String::new();
    for ch in basename.chars().take(120) {
        if ch.is_control() || ch == '<' || ch == '>' {
            out.push('_');
        } else {
            out.push(ch);
        }
    }
    if out.is_empty() {
        out.push_str("(unnamed)");
    }
    out
}

/// The leading hexadecimal of a hash, lowercased, with anything else dropped.
fn short_hash(hex: &str) -> String {
    let short: String = hex
        .chars()
        .filter(char::is_ascii_hexdigit)
        .map(|c| c.to_ascii_lowercase())
        .take(PROVENANCE_SHA_CHARS)
        .collect();
    if short.is_empty() {
        "unknown".to_owned()
    } else {
        short
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_engine_version_matches_the_workspace_pin() {
        let manifest = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.toml"),
        )
        .expect("workspace manifest");
        let pin = format!("anydoc = \"={}\"", crate::ENGINE_VERSION);
        assert!(
            manifest.contains(&pin),
            "the workspace pins a different anydoc than ENGINE_VERSION names ({pin})"
        );
    }

    use super::*;

    /// The extension table is the whole gate: a format not named here is never
    /// looked at, so a source file or a note can never be replaced by a
    /// rewrite of itself.
    #[test]
    fn readable_text_formats_are_not_documents() {
        for ext in ["md", "txt", "rs", "toml", "json", "yaml", "markdown", ""] {
            assert!(
                !is_supported_extension(ext),
                "`{ext}` must not be convertible"
            );
        }
        for ext in SUPPORTED_EXTENSIONS {
            assert!(is_supported_extension(ext));
            assert!(is_supported_extension(&format!(".{}", ext.to_uppercase())));
        }
    }

    /// The count is load-bearing: the plan says fourteen extensions, and a
    /// fifteenth would be a format nobody decided to support.
    #[test]
    fn the_table_holds_fourteen_sorted_distinct_extensions() {
        let mut sorted = SUPPORTED_EXTENSIONS.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), 14);
        assert_eq!(sorted.as_slice(), SUPPORTED_EXTENSIONS.as_slice());
    }

    #[test]
    fn provenance_names_the_source_and_twelve_hash_characters() {
        let line = provenance_line(
            "spec.docx",
            "9f2c4a1b7e60d3f58a12bb4477cc8899aabbccddeeff00112233445566778899",
        );
        assert_eq!(
            line,
            "<!-- converted by rift from spec.docx sha256:9f2c4a1b7e60 -->"
        );
    }

    /// A file name is written by whoever wrote the file. One shaped like the
    /// end of an HTML comment would put the rest of itself into the document.
    #[test]
    fn a_file_name_cannot_close_the_provenance_comment() {
        let line = provenance_line("evil--><script>.docx", "abc");
        assert!(!line[..line.len() - 4].contains("-->"), "{line}");
        assert_eq!(
            line,
            "<!-- converted by rift from evil--__script_.docx sha256:abc -->"
        );
    }

    #[test]
    fn a_sidecar_opens_with_its_provenance_line() {
        let markdown = Markdown {
            text: "# Title\n\nbody\n".to_owned(),
            ..Markdown::default()
        };
        let text = sidecar_text(&markdown, "a.docx", "00ff");
        let first = text.lines().next().unwrap();
        assert_eq!(first, "<!-- converted by rift from a.docx sha256:00ff -->");
        assert!(text.ends_with("body\n"));
    }

    #[test]
    fn a_named_thin_page_makes_a_conversion_thin_whatever_its_length() {
        let caps = ConvertCaps::default();
        let thin = Markdown {
            text: "x".repeat(10_000),
            pages: Some(2),
            thin_pages: vec![2],
            ..Markdown::default()
        };
        assert!(thin.is_thin(&caps));

        let sparse = Markdown {
            text: "x".repeat(100),
            pages: Some(4),
            ..Markdown::default()
        };
        assert!(sparse.is_thin(&caps));

        let full = Markdown {
            text: "x".repeat(1_000),
            pages: Some(4),
            ..Markdown::default()
        };
        assert!(!full.is_thin(&caps));

        // No page count is not evidence of thinness: the formats that have no
        // pages (a spreadsheet, a CSV) are never withheld for having few.
        let paged_unknown = Markdown {
            text: String::new(),
            ..Markdown::default()
        };
        assert!(!paged_unknown.is_thin(&caps));
    }

    #[test]
    fn a_refusal_carries_a_reason_and_no_document_bytes() {
        let too_large = DocError::TooLarge { bytes: 20, cap: 10 };
        assert_eq!(
            too_large.to_string(),
            "document is 20 bytes, over the 10-byte conversion cap"
        );
        assert!(matches!(
            RiftError::from(too_large),
            RiftError::PolicyDenied(_)
        ));
        assert!(matches!(
            RiftError::from(DocError::Engine { code: "encrypted" }),
            RiftError::Validity(_)
        ));
    }
}
