//! The writers of the document corpus, put against the shipped converter:
//! every format this fixture authors is a document the engine really reads,
//! and the corpus is the same bytes on every build.
//!
//! A corpus is only worth what the parser makes of it, so the claim that
//! belongs here is not a number — it is that the archives assembled in
//! `common/document_fixture.rs` are documents somebody could open. What a
//! session over them costs is `document_savings`.
//!
//! Every claim here is about the document seam, and the binary carries that
//! seam behind the `doc` feature: a build without it converts nothing, so
//! there is no substitution to price. The suite is the feature's, and says so
//! rather than failing a build that never promised one.
#![cfg(feature = "doc")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use std::path::Path;

mod common;

#[allow(dead_code)] // The document suites share these builders; each file uses a subset.
#[path = "common/document_fixture.rs"]
mod document_fixture;
use document_fixture::{
    documents, docx, epub, odf, ole_header, paragraph, pdf, pdf_stream, pdf_text, pptx, prose, rtf,
    xlsx, Noise,
};

/// One document from every writer the fixture has, and the prose the
/// prose-bearing ones were written from.
struct SampleCorpus {
    /// The paragraphs `spec.docx` was written from, so a round trip can be
    /// checked to the word.
    body: Vec<(bool, String)>,
    /// The documents themselves, named as they are written to disk.
    written: Vec<(&'static str, Vec<u8>)>,
}

/// One small document per writer: the same writers the corpus is made with,
/// at a size a test can convert in a moment.
fn one_document_from_every_writer() -> SampleCorpus {
    let mut noise = Noise::new(0x0D0C_0F1C_7107_1234);
    let body = prose(&mut noise, 7, 12);
    let slides = vec![("A slide".to_owned(), vec![paragraph(&mut noise, 6)])];
    let chapters = vec![("A chapter".to_owned(), vec![paragraph(&mut noise, 6)])];
    let cells = |row: usize, column: usize| format!("r{row}c{column}");
    let odf_row = "<table:table-row><table:table-cell><text:p>cell</text:p>\
                   </table:table-cell></table:table-row>";
    let written: Vec<(&'static str, Vec<u8>)> = vec![
        ("spec.docx", docx(&body, Some(noise.bytes(2048)))),
        (
            "notes.odt",
            odf(
                "application/vnd.oasis.opendocument.text",
                "<office:text><text:p>a paragraph</text:p></office:text>",
                Some(noise.bytes(1024)),
            ),
        ),
        (
            "book.ods",
            odf(
                "application/vnd.oasis.opendocument.spreadsheet",
                &format!(
                    "<office:spreadsheet><table:table table:name=\"S\">{odf_row}</table:table>\
                     </office:spreadsheet>"
                ),
                None,
            ),
        ),
        ("sheet.xlsx", xlsx(3, cells)),
        ("deck.pptx", pptx(&slides, noise.bytes(1024))),
        ("manual.epub", epub(&chapters, noise.bytes(1024))),
        ("memo.rtf", rtf("A memo", &body)),
        (
            "page.pdf",
            pdf(&[
                b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
                b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources \
                   << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>"
                    .to_vec(),
                pdf_stream(&pdf_text(&["A typeset line.".to_owned()]), ""),
                b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec(),
            ]),
        ),
    ];
    SampleCorpus { body, written }
}

/// The shipped binary's answer for one document, through the route a hook
/// re-spawns it on: its Markdown, or `None` where the engine refused the
/// container.
fn convert(cwd: &Path, document: &Path) -> Option<String> {
    let out = common::rift_cmd(cwd)
        .args(["hooks", "doc", "convert"])
        .arg(document)
        .output()
        .unwrap();
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// ★ Every writer the fixture has makes a document the shipped converter
/// answers.
///
/// The corpus is only worth what the parser makes of it: an archive assembled
/// by hand there and refused by the engine would price a document nobody can
/// open. So each writer is driven through the shipped binary's own engine, on
/// the route a hook re-spawns it on.
#[test]
fn every_writer_here_makes_a_document_the_shipped_converter_answers() {
    let tmp = tempfile::tempdir().unwrap();
    for (name, bytes) in &one_document_from_every_writer().written {
        let path = tmp.path().join(name);
        std::fs::write(&path, bytes).unwrap();
        let markdown = convert(tmp.path(), &path)
            .unwrap_or_else(|| panic!("the engine reads what these writers wrote: {name}"));
        assert!(
            !markdown.trim().is_empty(),
            "{name} converted to nothing at all"
        );
    }
}

/// ★ The prose document is checked to the word.
///
/// A writer that lost a paragraph would still convert, and the corpus would
/// price a document whose Markdown is smaller than the document.
#[test]
fn every_paragraph_of_a_written_document_survives_the_round_trip() {
    let tmp = tempfile::tempdir().unwrap();
    let sample = one_document_from_every_writer();
    let (name, bytes) = sample
        .written
        .iter()
        .find(|(name, _)| *name == "spec.docx")
        .expect("the sample carries the document its prose was written into");
    let path = tmp.path().join(name);
    std::fs::write(&path, bytes).unwrap();
    let markdown = convert(tmp.path(), &path).unwrap();
    for (heading, text) in &sample.body {
        assert!(
            markdown.contains(text.as_str()),
            "every paragraph survives the round trip; this one did not: {text}"
        );
        assert!(
            !*heading || markdown.contains(&format!("# {text}")),
            "and a heading arrives as a heading: {text}"
        );
    }
}

/// ★ The one container the engine is known to refuse is refused.
///
/// A fixture that quietly wrote an unreadable file for that case would prove
/// nothing about the refusal it claims to carry.
#[test]
fn an_ole_container_with_no_usable_directory_is_refused_rather_than_converted() {
    let tmp = tempfile::tempdir().unwrap();
    let legacy = tmp.path().join("ledger.xls");
    std::fs::write(&legacy, ole_header()).unwrap();
    assert!(
        convert(tmp.path(), &legacy).is_none(),
        "an OLE container with no usable directory is refused, not converted"
    );
}

/// ★ The corpus is the same bytes on every build.
///
/// A fixture whose documents drifted between runs would publish a different
/// pool every time it ran, and no figure it produced could be compared with
/// the one before it. Everything here is written from one seed, with archive
/// timestamps fixed and no compressor in the path, so the corpus is a pure
/// function of this file.
#[test]
fn the_corpus_is_the_same_bytes_on_every_build() {
    let first = documents();
    let second = documents();
    assert_eq!(first.len(), second.len());
    for (a, b) in first.iter().zip(second.iter()) {
        assert_eq!(a.path, b.path);
        assert_eq!(
            rift_shadow::sha256_hex(&[&a.bytes]),
            rift_shadow::sha256_hex(&[&b.bytes]),
            "{} differs between two builds of the same corpus",
            a.path
        );
        assert!(
            rift_recall_doc::is_supported_extension(
                Path::new(a.path)
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or_default()
            ),
            "{} is not a document this product converts",
            a.path
        );
    }
}
