//! The fourteen extensions, over documents small enough to read by hand.
//!
//! Every claim this crate makes about a format is a claim about bytes, and the
//! only way to hold it is to keep the bytes. `fixtures/corpus/` carries one
//! authored document per supported extension — plus three PDFs, because a PDF
//! with a text layer, a scanned one and a mixed one are three different
//! documents to a converter — and one golden per document beside it:
//! `<name>.md` for a conversion, `<name>.refused` for a refusal. The fixtures
//! are built by `fixtures/make-corpus.py`, which explains what each one is and
//! why it is the shape it is; the goldens are what the converter made of them.
//!
//! Two of the goldens say something the plan did not expect, and they say it
//! rather than hide it:
//!
//! * `mixed.pdf` has one readable page and one scanned one, and the engine
//!   returns **no text at all** for it — not the page it could read. The
//!   reasoning is the engine's own: output missing a page reads as complete.
//!   So the golden is a thin document naming page 2, and any later measurement
//!   that assumes a mixed PDF yields partial text is measuring a wish.
//! * `legacy.xls` and `legacy.ppt` are OLE compound files, which cannot be
//!   authored by hand or from the Python standard library. They are carried as
//!   what a hand-authored one really is — a container header leading nowhere —
//!   and their goldens pin the *refusal*, so the corpus covers all fourteen
//!   extensions while claiming a conversion for only twelve of them.
#![cfg(feature = "doc")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use rift_recall_doc::{
    AnydocConverter, ConvertCaps, DocConverter, DocError, Markdown, SUPPORTED_EXTENSIONS,
};

fn corpus() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/corpus")
}

/// The documents in the corpus, name and bytes, in a fixed order.
///
/// Sorted rather than taken in directory order: a test whose iteration order
/// came from the filesystem would report a different first failure on a
/// different machine.
fn documents() -> Vec<(String, Vec<u8>)> {
    let mut found: Vec<(String, Vec<u8>)> = std::fs::read_dir(corpus())
        .expect("the fixture corpus")
        .map(|entry| entry.expect("a corpus entry").file_name())
        .filter_map(|name| name.to_str().map(str::to_owned))
        .filter(|name| !matches!(extension(name), "md" | "refused"))
        .map(|name| {
            let bytes = std::fs::read(corpus().join(&name)).expect("a corpus document");
            (name, bytes)
        })
        .collect();
    found.sort();
    assert!(
        found.len() >= SUPPORTED_EXTENSIONS.len(),
        "the corpus has fewer documents than there are supported extensions"
    );
    found
}

/// The extension a corpus document is named by.
fn extension(name: &str) -> &str {
    name.rsplit_once('.').expect("a corpus name has one").1
}

/// What the committed golden says this document converts to.
fn golden(name: &str) -> Result<String, String> {
    let converted = corpus().join(format!("{name}.md"));
    let refused = corpus().join(format!("{name}.refused"));
    match (converted.is_file(), refused.is_file()) {
        (true, false) => Ok(std::fs::read_to_string(converted).expect("a golden")),
        (false, true) => Err(std::fs::read_to_string(refused).expect("a golden")),
        (true, true) => panic!("`{name}` has both a conversion and a refusal golden"),
        (false, false) => {
            panic!("`{name}` has no golden; run the converter over it and commit one")
        }
    }
}

/// Tables the Markdown actually carries, counted the way a reader would: a
/// delimiter row under a header. Used to check the reported sheets against the
/// document rather than against the code that reported them.
fn delimiter_rows(text: &str) -> usize {
    text.lines()
        .filter(|line| line.starts_with("| ---"))
        .count()
}

/// The two documents the engine reports a page count for. Every other
/// conversion carries `pages: None` — including `text-layer.pdf`, because a
/// *successful* PDF conversion returns a bare string and the page count
/// reaches a caller only through the scanned-pages error. That is a limit of
/// the seam and it is asserted here so nobody builds on the opposite.
const PAGED: [(&str, u32, &[u32]); 2] = [("mixed.pdf", 2, &[2]), ("scanned.pdf", 1, &[1])];

fn paged(name: &str) -> Option<(u32, &'static [u32])> {
    PAGED
        .iter()
        .find(|(fixture, _, _)| *fixture == name)
        .map(|(_, pages, thin)| (*pages, *thin))
}

/// ★ Every supported extension is in the corpus, every document in the corpus
/// converts to exactly what its golden says, and converting it again produces
/// the same bytes.
///
/// Determinism is checked two ways, because they fail differently: twice in
/// this process catches an engine that carries state between calls, and the
/// golden catches an engine whose answer moved between builds — the goldens
/// were written by a different process on a different day, and a sidecar is
/// content-addressed, so an answer that drifted would serve one member of a
/// worktree family something its sibling never saw.
#[test]
fn every_supported_format_converts_deterministically_on_the_fixture_set() {
    let caps = ConvertCaps::default();
    let mut covered = BTreeSet::new();

    for (name, bytes) in documents() {
        covered.insert(extension(&name).to_ascii_lowercase());
        let first = AnydocConverter.convert(&bytes, extension(&name), &caps);
        let again = AnydocConverter.convert(&bytes, extension(&name), &caps);
        assert_eq!(
            first, again,
            "`{name}` converted differently the second time"
        );

        match (first, golden(&name)) {
            (Ok(markdown), Ok(expected)) => {
                assert_eq!(
                    markdown.text, expected,
                    "`{name}` no longer matches its golden"
                );
                assert_eq!(
                    markdown.sheets.len(),
                    delimiter_rows(&markdown.text),
                    "`{name}` reports sheets the Markdown does not show"
                );
                let reported = markdown
                    .pages
                    .map(|pages| (pages, markdown.thin_pages.as_slice()));
                assert_eq!(
                    reported,
                    paged(&name),
                    "`{name}` reports a different page count than the corpus records"
                );
            }
            (Err(error), Err(expected)) => {
                assert_eq!(
                    format!("{error}\n"),
                    expected,
                    "`{name}` refuses differently"
                );
                assert!(
                    matches!(error, DocError::Engine { .. }),
                    "`{name}` refused for a reason the corpus does not describe: {error:?}"
                );
            }
            (Ok(markdown), Err(expected)) => {
                panic!(
                    "`{name}` converted but its golden is the refusal {expected:?}: {markdown:?}"
                )
            }
            (Err(error), Ok(_)) => panic!("`{name}` no longer converts: {error}"),
        }
    }

    let supported: BTreeSet<String> = SUPPORTED_EXTENSIONS
        .iter()
        .map(|ext| (*ext).to_owned())
        .collect();
    assert_eq!(
        covered, supported,
        "the corpus and the extension table disagree about what this crate converts"
    );
}

/// ★ A scanned PDF is a document with nothing to convert, not a conversion
/// that failed — and the difference decides what the caller does.
///
/// The body names the pages instead of being empty, because a caller that got
/// an empty document could not tell it from a document with no content, and a
/// sidecar that said nothing would be substituted for a read that would at
/// least have shown the agent the page images.
#[test]
fn a_scanned_pdf_is_reported_thin_not_empty() {
    let caps = ConvertCaps::default();
    let bytes = std::fs::read(corpus().join("scanned.pdf")).unwrap();
    let markdown: Markdown = AnydocConverter
        .convert(&bytes, "pdf", &caps)
        .expect("a scanned PDF is a conversion with a thin answer, not an error");

    assert_eq!(markdown.pages, Some(1));
    assert_eq!(markdown.thin_pages, vec![1]);
    assert!(markdown.is_thin(&caps), "{markdown:?}");
    assert!(
        !markdown.text.trim().is_empty(),
        "a thin answer is not an empty one"
    );
    assert!(markdown.text.contains("page(s) 1 of 1"), "{markdown:?}");
    assert!(
        markdown.text.contains("scanned or image-only"),
        "{markdown:?}"
    );
    assert_eq!(markdown.text, golden("scanned.pdf").unwrap());
}

/// A PDF with one readable page and one scanned page yields **no text at
/// all** — not the readable page. That is the engine's decision, on the ground
/// that a document missing a page reads as complete, and it is asserted here
/// so that a later measurement of what document conversion recovers cannot
/// quietly assume otherwise.
#[test]
fn a_mixed_pdf_recovers_no_text_from_the_page_it_could_read() {
    let caps = ConvertCaps::default();
    let mixed = std::fs::read(corpus().join("mixed.pdf")).unwrap();
    let markdown = AnydocConverter.convert(&mixed, "pdf", &caps).unwrap();

    assert_eq!(markdown.pages, Some(2));
    assert_eq!(
        markdown.thin_pages,
        vec![2],
        "only the image page needs OCR"
    );
    assert!(markdown.is_thin(&caps));
    // The text that page 1 carries, and that the same bytes converted alone do
    // produce, is absent from the mixed document's answer.
    let alone = std::fs::read(corpus().join("text-layer.pdf")).unwrap();
    let recovered = AnydocConverter.convert(&alone, "pdf", &caps).unwrap();
    assert!(recovered
        .text
        .contains("Rift converts documents to Markdown."));
    assert!(
        !markdown.text.contains("Rift converts documents"),
        "the mixed document recovered text the engine does not return: {markdown:?}"
    );
    assert_eq!(
        recovered.pages, None,
        "a successful PDF conversion carries no page count"
    );
}

/// A golden with no document beside it is a golden nobody checks: the corpus
/// would keep passing after the file it describes was deleted.
#[test]
fn no_golden_describes_a_document_that_is_gone() {
    let documents: BTreeSet<String> = documents().into_iter().map(|(name, _)| name).collect();
    for entry in std::fs::read_dir(corpus()).unwrap() {
        let name = entry.unwrap().file_name();
        let name = name.to_str().unwrap().to_owned();
        let Some(document) = name
            .strip_suffix(".md")
            .or_else(|| name.strip_suffix(".refused"))
        else {
            continue;
        };
        assert!(
            documents.contains(document),
            "`{name}` is a golden for `{document}`, which is not in the corpus"
        );
    }
}
