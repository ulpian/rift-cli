#![no_main]
//! Fuzz target for document conversion.
//!
//! Contract: for *any* bytes, under any extension the crate claims, `convert`
//! returns — an answer or a refusal — without panicking, without hanging and
//! without holding memory in proportion to what the input claims rather than
//! what it carries. This is the largest parser surface in the tree by far:
//! fourteen container formats reading files an agent found in a repository,
//! which is to say files anybody could have written.
//!
//! The input is split into a format selector, a shape selector and the
//! document. The shape selector decides whether the bytes are handed over as
//! they are or wrapped in a zip container whose header *claims* four gigabytes
//! for an entry holding a few dozen: the archive readers charge a decompression
//! budget against the declared size, and a reader that believed a declaration
//! would allocate on an attacker's word. The entry is named after a part one of
//! the frontends actually opens, so the fuzzer spends its budget inside a
//! parser rather than on containers that are rejected at the door.
//!
//! What is asserted beyond "no panic":
//!
//! * **Determinism.** The same bytes and caps convert to the same answer twice.
//!   Sidecars are content-addressed, so an answer that varied between runs
//!   would serve one member of a worktree family something a sibling never saw.
//! * **The caps hold.** No table keeps more rows than the cap allows, no table
//!   reports keeping more rows than it had, and the Markdown ends in a newline
//!   with no CRLF left in it — a sidecar that spelled its line endings two ways
//!   would be two files under one hash.
//! * **A refusal names a class, not a document.** Failure reasons reach logs
//!   and, through the callers, an agent's context; a parser complaining about
//!   byte 4,096 by quoting it is how document bytes escape.
//!
//! A crash here is not always a defect in this crate: the seam catches an
//! engine that unwinds and turns it into a refusal, but the fuzzer's panic hook
//! aborts first, so an engine defect the seam already contains still stops the
//! run. Triage accordingly — and fix it upstream rather than widening the net.

use libfuzzer_sys::fuzz_target;
use rift_recall_doc::{
    AnydocConverter, ConvertCaps, DocConverter, DocError, SUPPORTED_EXTENSIONS,
};

/// The most document one exec parses. Larger inputs are a slower fuzzer, not a
/// wider one: every limit this crate has is reached long before this.
const MAX_DOCUMENT: usize = 64 * 1024;

/// Package parts the frontends open by name. An archive whose only entry is
/// called something else is refused before a parser sees a byte of it.
const PARTS: [&str; 5] = [
    "content.xml",
    "word/document.xml",
    "xl/workbook.xml",
    "ppt/presentation.xml",
    "META-INF/container.xml",
];

fuzz_target!(|data: &[u8]| {
    let Some((&format, rest)) = data.split_first() else {
        return;
    };
    let Some((&shape, body)) = rest.split_first() else {
        return;
    };
    let ext = SUPPORTED_EXTENSIONS[format as usize % SUPPORTED_EXTENSIONS.len()];
    let body = &body[..body.len().min(MAX_DOCUMENT)];
    let document = if shape % 2 == 0 {
        body.to_vec()
    } else {
        overclaiming_archive(PARTS[format as usize % PARTS.len()], body)
    };

    let caps = ConvertCaps {
        table_rows: usize::from(shape / 2),
        max_input_bytes: (MAX_DOCUMENT as u64) * 4,
        pdf_min_chars_per_page: u32::from(format),
    };
    let first = AnydocConverter.convert(&document, ext, &caps);
    let again = AnydocConverter.convert(&document, ext, &caps);
    assert!(first == again, "`{ext}` converted differently the second time");

    match first {
        Ok(markdown) => {
            assert!(
                markdown.text.is_empty() || markdown.text.ends_with('\n'),
                "converted Markdown does not end in a newline"
            );
            assert!(
                !markdown.text.contains("\r\n"),
                "converted Markdown kept a carriage return"
            );
            for sheet in &markdown.sheets {
                assert!(
                    sheet.rows_kept as usize <= caps.table_rows,
                    "a table kept {} rows under a cap of {}",
                    sheet.rows_kept,
                    caps.table_rows
                );
                assert!(sheet.rows_kept <= sheet.rows, "a table kept rows it never had");
            }
            let _ = markdown.is_thin(&caps);
        }
        Err(DocError::Engine { code }) => assert!(
            code.len() <= 32 && code.chars().all(|c| c.is_ascii_alphabetic()),
            "a refusal carried something other than a classification: {code:?}"
        ),
        // Neither is reachable from here — the input is under the cap and the
        // extension comes from the crate's own table — but a refusal is a
        // refusal and no branch of it may carry the document.
        Err(other) => assert!(
            !other.to_string().contains(char::is_control),
            "a refusal carried document bytes"
        ),
    }
});

/// A one-entry archive whose header claims four gigabytes for `body`.
///
/// Written by hand rather than with a zip writer, because a writer would refuse
/// to write the lie that is the point of it: the declared size is what a reader
/// budgets against, and a reader that trusts the declaration allocates on an
/// attacker's word. Stored, not deflated, so the bytes reaching the parser are
/// the fuzzer's own.
fn overclaiming_archive(name: &str, body: &[u8]) -> Vec<u8> {
    let claim = u32::MAX.to_le_bytes();
    let carried = u32::try_from(body.len()).unwrap_or(u32::MAX).to_le_bytes();
    let name_len = u16::try_from(name.len()).unwrap_or(u16::MAX).to_le_bytes();
    let mut out = Vec::with_capacity(body.len() + 128);

    out.extend_from_slice(b"PK\x03\x04"); // local file header
    out.extend_from_slice(&[20, 0, 0, 0, 0, 0, 0, 0, 0, 0]); // version, flags, stored, time
    out.extend_from_slice(&[0; 4]); // crc-32, unchecked until the bytes are read
    out.extend_from_slice(&carried);
    out.extend_from_slice(&claim);
    out.extend_from_slice(&name_len);
    out.extend_from_slice(&[0, 0]); // extra field length
    out.extend_from_slice(name.as_bytes());
    out.extend_from_slice(body);

    let directory = u32::try_from(out.len()).unwrap_or(u32::MAX).to_le_bytes();
    out.extend_from_slice(b"PK\x01\x02"); // central directory header
    out.extend_from_slice(&[20, 0, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0]); // versions, flags, stored
    out.extend_from_slice(&[0; 4]); // crc-32, unchecked until the bytes are read
    out.extend_from_slice(&carried);
    out.extend_from_slice(&claim);
    out.extend_from_slice(&name_len);
    out.extend_from_slice(&[0; 12]); // extra, comment, disk, attributes
    out.extend_from_slice(&[0; 4]); // the local header, which is at the start
    out.extend_from_slice(name.as_bytes());

    let size = u32::try_from(out.len()).unwrap_or(u32::MAX) - u32::from_le_bytes(directory);
    out.extend_from_slice(b"PK\x05\x06"); // end of central directory
    out.extend_from_slice(&[0; 4]); // this disk, the disk the directory starts on
    out.extend_from_slice(&[1, 0, 1, 0]); // one entry, here and in total
    out.extend_from_slice(&size.to_le_bytes());
    out.extend_from_slice(&directory);
    out.extend_from_slice(&[0, 0]); // no archive comment
    out
}
