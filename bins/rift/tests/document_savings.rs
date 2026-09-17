//! The document corpus and what a session over it costs with and without the
//! substitution — the fixture behind the nine-tenths bar for documents.
//!
//! **Everything here is generated, and generated in Rust.** The corpus is a
//! pure function of this file: the same bytes on every machine and every
//! platform, with no committed binaries to review by eye and no interpreter to
//! find at test time. The XML writers are ported from the converter's own
//! corpus script (`crates/rift-recall-doc/fixtures/make-corpus.py`, which
//! authored the small parser-correctness corpus); what is new here is scale
//! and shape, because a fixture that prices documents has to be made of
//! documents somebody would actually have.
//!
//! **Why the containers are stored rather than deflated.** A real Office file
//! deflates its parts, and deflate output is not reproducible across zlib
//! builds, so a deflating generator would make the fixture's numbers depend on
//! the machine. Stored parts cost this fixture something, and it is worth
//! saying which direction: a deflated part reaches a native read as bytes with
//! no text in them (measured at about four fifths of a token per character),
//! while a stored one reaches it as the XML it is (about a fifth). So every
//! pool figure here is the SMALLER of the two, and the share the fixture
//! reports is the pessimistic one. What the corpus does keep from real
//! documents is their media: a design document has diagrams and a deck has
//! pictures, those parts are opaque in any archive, and they are most of what
//! a native read of a real document actually pays for.
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
    clippy::panic,
    // A corpus builder is a string builder: every document below is assembled
    // by mapping rows and paragraphs into markup and collecting them. The
    // fold-with-`write!` form the lint prefers hides what each writer emits,
    // which is the one thing a reviewer of a fixture corpus has to be able to
    // read.
    clippy::format_collect
)]

use std::path::Path;

mod common;

// ----------------------------------------------------------- the generator ---

/// A deterministic byte stream, standing in for a document's media part.
///
/// A picture in a container is bytes with no text in them, and that is the
/// whole of what this has to reproduce. Written out rather than taken from a
/// crate so the corpus depends on nothing but this file.
struct Noise(u64);

impl Noise {
    const fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn byte(&mut self) -> u8 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 33) as u8
    }

    fn bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n).map(|_| self.byte()).collect()
    }
}

/// CRC-32 (IEEE), computed bit by bit: a table would be faster and this is a
/// corpus builder.
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFF_u32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// One archive with every entry stored, entries in the order given.
///
/// The order is load-bearing twice over: an EPUB's `mimetype` has to come
/// first, and what a native read sees is the first thirty thousand characters
/// of this file, which is the front of this list.
fn zip_stored(entries: &[(&str, Vec<u8>)]) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();
    let mut directory: Vec<u8> = Vec::new();
    // 1980-01-01, the earliest a zip timestamp can express and the same on
    // every machine that builds this corpus.
    let (time, date) = (0_u16, 0x0021_u16);
    for (name, body) in entries {
        let offset = u32::try_from(out.len()).unwrap();
        let crc = crc32(body);
        let size = u32::try_from(body.len()).unwrap();
        let namelen = u16::try_from(name.len()).unwrap();
        out.extend_from_slice(b"PK\x03\x04");
        out.extend_from_slice(&20_u16.to_le_bytes());
        out.extend_from_slice(&0_u16.to_le_bytes());
        out.extend_from_slice(&0_u16.to_le_bytes());
        out.extend_from_slice(&time.to_le_bytes());
        out.extend_from_slice(&date.to_le_bytes());
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&size.to_le_bytes());
        out.extend_from_slice(&size.to_le_bytes());
        out.extend_from_slice(&namelen.to_le_bytes());
        out.extend_from_slice(&0_u16.to_le_bytes());
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(body);

        directory.extend_from_slice(b"PK\x01\x02");
        directory.extend_from_slice(&0x031E_u16.to_le_bytes());
        directory.extend_from_slice(&20_u16.to_le_bytes());
        directory.extend_from_slice(&0_u16.to_le_bytes());
        directory.extend_from_slice(&0_u16.to_le_bytes());
        directory.extend_from_slice(&time.to_le_bytes());
        directory.extend_from_slice(&date.to_le_bytes());
        directory.extend_from_slice(&crc.to_le_bytes());
        directory.extend_from_slice(&size.to_le_bytes());
        directory.extend_from_slice(&size.to_le_bytes());
        directory.extend_from_slice(&namelen.to_le_bytes());
        directory.extend_from_slice(&0_u16.to_le_bytes());
        directory.extend_from_slice(&0_u16.to_le_bytes());
        directory.extend_from_slice(&0_u16.to_le_bytes());
        directory.extend_from_slice(&0_u16.to_le_bytes());
        directory.extend_from_slice(&(0o644_u32 << 16).to_le_bytes());
        directory.extend_from_slice(&offset.to_le_bytes());
        directory.extend_from_slice(name.as_bytes());
    }
    let start = u32::try_from(out.len()).unwrap();
    let count = u16::try_from(entries.len()).unwrap();
    out.extend_from_slice(&directory);
    out.extend_from_slice(b"PK\x05\x06");
    out.extend_from_slice(&0_u16.to_le_bytes());
    out.extend_from_slice(&0_u16.to_le_bytes());
    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(&u32::try_from(directory.len()).unwrap().to_le_bytes());
    out.extend_from_slice(&start.to_le_bytes());
    out.extend_from_slice(&0_u16.to_le_bytes());
    out
}

/// The words the corpus's prose is made of: ordinary English of the length
/// ordinary English has, so a sidecar's token count is a document's token
/// count and not an artefact of a repeated string.
const WORDS: [&str; 24] = [
    "retrieval",
    "delta",
    "ledger",
    "family",
    "sidecar",
    "conversion",
    "budget",
    "window",
    "token",
    "agent",
    "document",
    "markdown",
    "container",
    "measure",
    "baseline",
    "worktree",
    "policy",
    "channel",
    "frame",
    "session",
    "checkout",
    "revision",
    "summary",
    "boundary",
];

/// One paragraph of `words` words, drawn deterministically.
fn paragraph(noise: &mut Noise, words: usize) -> String {
    let mut out = String::new();
    for n in 0..words {
        if n > 0 {
            out.push(' ');
        }
        out.push_str(WORDS[usize::from(noise.byte()) % WORDS.len()]);
    }
    out.push('.');
    out
}

/// `count` paragraphs, every sixth one a heading, as prose in a document is.
fn prose(noise: &mut Noise, count: usize, words: usize) -> Vec<(bool, String)> {
    (0..count)
        .map(|n| {
            if n % 6 == 0 {
                (true, format!("Section {}", n / 6 + 1))
            } else {
                (false, paragraph(noise, words))
            }
        })
        .collect()
}

const XML_HEAD: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n";
const W_NS: &str = "xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"";
const R_NS: &str =
    "xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\"";
const P_NS: &str = "xmlns:p=\"http://schemas.openxmlformats.org/presentationml/2006/main\"";
const A_NS: &str = "xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\"";
const S_NS: &str = "xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\"";
const ODF_NS: &str = "xmlns:office=\"urn:oasis:names:tc:opendocument:xmlns:office:1.0\" \
     xmlns:text=\"urn:oasis:names:tc:opendocument:xmlns:text:1.0\" \
     xmlns:table=\"urn:oasis:names:tc:opendocument:xmlns:table:1.0\" \
     xmlns:draw=\"urn:oasis:names:tc:opendocument:xmlns:drawing:1.0\"";
const RELS_NS: &str = "http://schemas.openxmlformats.org/package/2006/relationships";

fn rels(triples: &[(&str, &str, &str)]) -> Vec<u8> {
    let body: String = triples
        .iter()
        .map(|(id, kind, target)| {
            format!("<Relationship Id=\"{id}\" Type=\"{kind}\" Target=\"{target}\"/>")
        })
        .collect();
    format!("{XML_HEAD}<Relationships xmlns=\"{RELS_NS}\">{body}</Relationships>").into_bytes()
}

fn office_rel(kind: &str) -> String {
    format!("http://schemas.openxmlformats.org/officeDocument/2006/relationships/{kind}")
}

/// A WordprocessingML package, with the styles part the heading level is
/// resolved through and, where a document has one, its picture.
fn docx(body: &[(bool, String)], media: Option<Vec<u8>>) -> Vec<u8> {
    let paragraphs: String = body
        .iter()
        .map(|(heading, text)| {
            let props = if *heading {
                "<w:pPr><w:pStyle w:val=\"Heading1\"/></w:pPr>"
            } else {
                ""
            };
            format!("<w:p>{props}<w:r><w:t>{text}</w:t></w:r></w:p>")
        })
        .collect();
    let styles = format!(
        "{XML_HEAD}<w:styles {W_NS}><w:style w:type=\"paragraph\" w:styleId=\"Heading1\">\
         <w:name w:val=\"heading 1\"/></w:style></w:styles>"
    );
    let mut entries = vec![
        (
            "_rels/.rels",
            rels(&[("rId1", &office_rel("officeDocument"), "word/document.xml")]),
        ),
        (
            "word/_rels/document.xml.rels",
            rels(&[("rId1", &office_rel("styles"), "styles.xml")]),
        ),
        (
            "word/document.xml",
            format!("{XML_HEAD}<w:document {W_NS}><w:body>{paragraphs}</w:body></w:document>")
                .into_bytes(),
        ),
        ("word/styles.xml", styles.into_bytes()),
    ];
    if let Some(picture) = media {
        entries.push(("word/media/image1.png", picture));
    }
    zip_stored(&entries)
}

/// A SpreadsheetML workbook of `rows` rows: the shape a report exports.
fn xlsx(rows: usize, cell: impl Fn(usize, usize) -> String) -> Vec<u8> {
    let mut sheet = String::from(XML_HEAD);
    sheet.push_str(&format!("<worksheet {S_NS}><sheetData>"));
    for row in 1..=rows {
        sheet.push_str(&format!("<row r=\"{row}\">"));
        for column in 0..5 {
            let value = cell(row, column);
            let letter = char::from(b'A' + u8::try_from(column).unwrap());
            sheet.push_str(&format!(
                "<c r=\"{letter}{row}\" t=\"inlineStr\"><is><t>{value}</t></is></c>"
            ));
        }
        sheet.push_str("</row>");
    }
    sheet.push_str("</sheetData></worksheet>");
    zip_stored(&[
        (
            "_rels/.rels",
            rels(&[("rId1", &office_rel("officeDocument"), "xl/workbook.xml")]),
        ),
        (
            "xl/workbook.xml",
            format!(
                "{XML_HEAD}<workbook {S_NS} {R_NS}><sheets>\
                 <sheet name=\"Reads\" sheetId=\"1\" r:id=\"rId1\"/></sheets></workbook>"
            )
            .into_bytes(),
        ),
        (
            "xl/_rels/workbook.xml.rels",
            rels(&[("rId1", &office_rel("worksheet"), "worksheets/sheet1.xml")]),
        ),
        ("xl/worksheets/sheet1.xml", sheet.into_bytes()),
    ])
}

/// A PresentationML deck: a title and two lines a slide, and one picture.
fn pptx(slides: &[(String, Vec<String>)], media: Vec<u8>) -> Vec<u8> {
    let mut entries: Vec<(&'static str, Vec<u8>)> = Vec::new();
    let ids: String = (1..=slides.len())
        .map(|n| format!("<p:sldId id=\"{}\" r:id=\"rId{n}\"/>", 255 + n))
        .collect();
    let targets: Vec<(String, String, String)> = (1..=slides.len())
        .map(|n| {
            (
                format!("rId{n}"),
                office_rel("slide"),
                format!("slides/slide{n}.xml"),
            )
        })
        .collect();
    let triples: Vec<(&str, &str, &str)> = targets
        .iter()
        .map(|(a, b, c)| (a.as_str(), b.as_str(), c.as_str()))
        .collect();
    entries.push((
        "_rels/.rels",
        rels(&[(
            "rId1",
            &office_rel("officeDocument"),
            "ppt/presentation.xml",
        )]),
    ));
    entries.push((
        "ppt/presentation.xml",
        format!("{XML_HEAD}<p:presentation {P_NS} {R_NS}><p:sldIdLst>{ids}</p:sldIdLst></p:presentation>")
            .into_bytes(),
    ));
    entries.push(("ppt/_rels/presentation.xml.rels", rels(&triples)));
    // The slide parts are named by an index this loop cannot borrow a literal
    // for, so the names are leaked into the static lifetime the archive takes.
    for (n, (title, lines)) in slides.iter().enumerate() {
        let body: String = lines
            .iter()
            .map(|line| format!("<a:p><a:r><a:t>{line}</a:t></a:r></a:p>"))
            .collect();
        let slide = format!(
            "{XML_HEAD}<p:sld {P_NS} {A_NS}><p:cSld><p:spTree>\
             <p:sp><p:nvSpPr><p:nvPr><p:ph type=\"title\"/></p:nvPr></p:nvSpPr>\
             <p:txBody><a:bodyPr/><a:p><a:r><a:t>{title}</a:t></a:r></a:p></p:txBody></p:sp>\
             <p:sp><p:nvSpPr><p:nvPr/></p:nvSpPr><p:txBody><a:bodyPr/>{body}</p:txBody></p:sp>\
             </p:spTree></p:cSld></p:sld>"
        );
        let name: &'static str =
            Box::leak(format!("ppt/slides/slide{}.xml", n + 1).into_boxed_str());
        entries.push((name, slide.into_bytes()));
    }
    entries.push(("ppt/media/image1.png", media));
    zip_stored(&entries)
}

/// An OpenDocument package: the mimetype part, the content, and any picture.
fn odf(mimetype: &str, body: &str, media: Option<Vec<u8>>) -> Vec<u8> {
    let mut entries = vec![
        ("mimetype", mimetype.as_bytes().to_vec()),
        (
            "content.xml",
            format!(
                "{XML_HEAD}<office:document-content {ODF_NS}>\
                 <office:body>{body}</office:body></office:document-content>"
            )
            .into_bytes(),
        ),
    ];
    if let Some(picture) = media {
        entries.push(("Pictures/image1.png", picture));
    }
    zip_stored(&entries)
}

/// An EPUB: the container, the package document, and a chapter apiece.
fn epub(chapters: &[(String, Vec<String>)], cover: Vec<u8>) -> Vec<u8> {
    let manifest: String = (1..=chapters.len())
        .map(|n| {
            format!(
                "<item id=\"c{n}\" href=\"chapter{n}.xhtml\" \
                 media-type=\"application/xhtml+xml\"/>"
            )
        })
        .collect();
    let spine: String = (1..=chapters.len())
        .map(|n| format!("<itemref idref=\"c{n}\"/>"))
        .collect();
    let mut entries: Vec<(&'static str, Vec<u8>)> = vec![
        ("mimetype", b"application/epub+zip".to_vec()),
        (
            "META-INF/container.xml",
            format!(
                "{XML_HEAD}<container version=\"1.0\" \
                 xmlns=\"urn:oasis:names:tc:opendocument:xmlns:container\"><rootfiles>\
                 <rootfile full-path=\"OEBPS/book.opf\" \
                 media-type=\"application/oebps-package+xml\"/></rootfiles></container>"
            )
            .into_bytes(),
        ),
        (
            "OEBPS/book.opf",
            format!(
                "{XML_HEAD}<package xmlns=\"http://www.idpf.org/2007/opf\" version=\"3.0\" \
                 unique-identifier=\"id\"><metadata \
                 xmlns:dc=\"http://purl.org/dc/elements/1.1/\">\
                 <dc:title>The operator's manual</dc:title></metadata>\
                 <manifest>{manifest}</manifest><spine>{spine}</spine></package>"
            )
            .into_bytes(),
        ),
    ];
    for (n, (title, paragraphs)) in chapters.iter().enumerate() {
        let body: String = paragraphs
            .iter()
            .map(|text| format!("<p>{text}</p>"))
            .collect();
        let page = format!(
            "{XML_HEAD}<html xmlns=\"http://www.w3.org/1999/xhtml\"><head><title>{title}</title>\
             </head><body><h1>{title}</h1>{body}</body></html>"
        );
        let name: &'static str =
            Box::leak(format!("OEBPS/chapter{}.xhtml", n + 1).into_boxed_str());
        entries.push((name, page.into_bytes()));
    }
    entries.push(("OEBPS/cover.png", cover));
    zip_stored(&entries)
}

/// Rich Text: the format a memo is saved in, and the bytes a `.doc` in the
/// wild often really carries.
fn rtf(title: &str, body: &[(bool, String)]) -> Vec<u8> {
    let mut out = String::from("{\\rtf1\\ansi\\deff0{\\fonttbl{\\f0 Helvetica;}}\n");
    out.push_str(&format!("\\f0\\fs24 {{\\b {title}}}\\par\n"));
    for (heading, text) in body {
        if *heading {
            out.push_str(&format!("{{\\b {text}}}\\par\n"));
        } else {
            out.push_str(&format!("{text}\\par\n"));
        }
    }
    out.push_str("}\n");
    out.into_bytes()
}

/// A PDF: numbered objects, a cross-reference table with real offsets, a
/// trailer. Ported from the converter's corpus script, which authored the
/// same shape for the parser goldens.
fn pdf(objects: &[Vec<u8>]) -> Vec<u8> {
    let mut out = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (n, body) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n", n + 1).as_bytes());
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendobj\n");
    }
    let start = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
    out.extend_from_slice(b"0000000000 65535 f \n");
    for offset in offsets {
        out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{start}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    out
}

fn pdf_stream(body: &[u8], extra: &str) -> Vec<u8> {
    let mut out = format!("<< /Length {}{extra} >>\nstream\n", body.len()).into_bytes();
    out.extend_from_slice(body);
    out.extend_from_slice(b"\nendstream");
    out
}

/// A page of typeset lines, in the content-stream form a reader recovers text
/// from.
fn pdf_text(lines: &[String]) -> Vec<u8> {
    let mut body = b"BT /F1 11 Tf 54 760 Td\n".to_vec();
    for line in lines {
        body.extend_from_slice(format!("({line}) Tj 0 -14 Td\n").as_bytes());
    }
    body.extend_from_slice(b"ET");
    body
}

/// An OLE compound-file header whose directory chain leads nowhere — what a
/// hand-authored legacy container really is, and what the engine refuses.
fn ole_header() -> Vec<u8> {
    let mut out = vec![0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];
    out.extend_from_slice(&[0; 16]);
    for half in [0x003E_u16, 3, 0xFFFE, 9, 6, 0] {
        out.extend_from_slice(&half.to_le_bytes());
    }
    for word in [
        0_u32,
        0,
        0,
        0,
        1,
        0xFFFF_FFFE,
        0,
        0x0000_1000,
        0xFFFF_FFFE,
        0,
    ] {
        out.extend_from_slice(&word.to_le_bytes());
    }
    for _ in 0..109 {
        out.extend_from_slice(&0xFFFF_FFFF_u32.to_le_bytes());
    }
    out
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

/// ★ Every writer in this file makes a document the shipped converter answers.
///
/// The corpus is only worth what the parser makes of it: an archive assembled
/// by hand here and refused by the engine would price a document nobody can
/// open. So each writer is driven through the shipped binary's own engine, on
/// the route a hook re-spawns it on, and the one container the engine is
/// known to refuse is asserted to be refused — a fixture that quietly wrote
/// an unreadable file for that case would prove nothing about the refusal it
/// claims to carry.
#[test]
fn every_writer_here_makes_a_document_the_shipped_converter_answers() {
    let tmp = tempfile::tempdir().unwrap();
    let mut noise = Noise::new(0x0D0C_0F1C_7107_1234);
    let body = prose(&mut noise, 7, 12);
    let slides = vec![("A slide".to_owned(), vec![paragraph(&mut noise, 6)])];
    let chapters = vec![("A chapter".to_owned(), vec![paragraph(&mut noise, 6)])];
    let cells = |row: usize, column: usize| format!("r{row}c{column}");
    let odf_row = "<table:table-row><table:table-cell><text:p>cell</text:p>\
                   </table:table-cell></table:table-row>";
    let written: Vec<(&str, Vec<u8>)> = vec![
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
    for (name, bytes) in &written {
        let path = tmp.path().join(name);
        std::fs::write(&path, bytes).unwrap();
        let markdown = convert(tmp.path(), &path)
            .unwrap_or_else(|| panic!("the engine reads what these writers wrote: {name}"));
        assert!(
            !markdown.trim().is_empty(),
            "{name} converted to nothing at all"
        );
    }

    // The prose document is checked to the word: a writer that lost a
    // paragraph would still convert, and the corpus would price a document
    // whose Markdown is smaller than the document.
    let markdown = convert(tmp.path(), &tmp.path().join("spec.docx")).unwrap();
    for (heading, text) in &body {
        assert!(
            markdown.contains(text.as_str()),
            "every paragraph survives the round trip; this one did not: {text}"
        );
        assert!(
            !*heading || markdown.contains(&format!("# {text}")),
            "and a heading arrives as a heading: {text}"
        );
    }

    // And the legacy container is refused, which is the claim it carries.
    let legacy = tmp.path().join("ledger.xls");
    std::fs::write(&legacy, ole_header()).unwrap();
    assert!(
        convert(tmp.path(), &legacy).is_none(),
        "an OLE container with no usable directory is refused, not converted"
    );
}

// -------------------------------------------------------------- the corpus ---

/// One document in the corpus, with the reason the corpus has it.
struct Member {
    /// Where it sits in the repository.
    path: &'static str,
    /// The format, as the breakdown names it.
    format: &'static str,
    /// Why a corpus that prices documents contains this one. Printed beside
    /// the figures, so a member that turns out to be unlike a real document
    /// is argued about rather than quietly re-sized.
    role: &'static str,
    /// Whether this document is one of the formats the bar was set over.
    ///
    /// The rule is the plan's and predates every number here: **one document
    /// per format the plan names** — DOCX, XLSX, PPTX, ODT, EPUB, CSV, RTF and
    /// the three PDFs. The other extensions the converter accepts are proved
    /// by the converter's own corpus, and they are read, priced and published
    /// here too — under their own figure, because a bar has to sit on a
    /// denominator somebody fixed before the measuring, and this one did.
    prescribed: bool,
    /// The bytes on disk.
    bytes: Vec<u8>,
}

/// The corpus: documents of the kind a repository keeps, in the formats the
/// converter accepts, at the sizes those documents have.
///
/// Every member is here for a stated reason and none is here to make a number
/// look better. Three groups: the containers a native read pays for and cannot
/// use; the plain-text formats where a native read works perfectly well and
/// only the row cap saves anything; and the documents Rift decides against —
/// the PDFs it declares nothing for and the two legacy containers it refuses —
/// which are in the corpus precisely because a fixture that dropped them would
/// be reporting on a repository nobody has.
fn corpus() -> Vec<Member> {
    let mut all = documents();
    for member in &mut all {
        member.prescribed = !BEYOND_THE_CORPUS.contains(&member.path);
    }
    all
}

/// The documents this fixture reads that the bar does not sit on: one per
/// extension the plan did not name, plus the dense-script probe.
///
/// Fixed here, in a list, rather than decided member by member: each is a
/// second document in a format the corpus already has, or a format the
/// converter's own corpus proves. They are read, converted, priced and
/// published exactly like the rest — the figures below carry both
/// denominators, so nothing here is a member quietly left out of a fraction.
const BEYOND_THE_CORPUS: [&str; 4] = [
    // A second spreadsheet, and a second rich-text document: the same claim
    // the corpus's own XLSX and RTF members make.
    "budget/usage.ods",
    "notes/letter.doc",
    // A second CSV, in a script whose characters cost about a token each —
    // here for one claim only: that the character ceiling is not a token
    // ceiling, and a document dense enough is declared at the tighter one.
    "intl/labels.csv",
    // A legacy binary container the engine refuses. Its own claim: a baseline
    // must not assume a conversion happened.
    "legacy/ledger.xls",
];

/// Every document this fixture reads, in the order it reads them.
///
/// One long list on purpose: a corpus is a declaration, and splitting it into
/// helpers would put the sizes somewhere other than beside the reason for
/// them.
#[allow(clippy::too_many_lines)]
fn documents() -> Vec<Member> {
    let mut noise = Noise::new(0x5EED_0F0C_0DE1_2345);
    let member = |path, format, role, bytes| Member {
        path,
        format,
        role,
        prescribed: true,
        bytes,
    };
    let design = prose(&mut noise, 24, 34);
    let handbook = prose(&mut noise, 18, 30);
    let memo = prose(&mut noise, 5, 28);
    let letter = prose(&mut noise, 7, 26);
    let slides: Vec<(String, Vec<String>)> = (1..=12)
        .map(|n| {
            (
                format!("Quarter {n}"),
                vec![paragraph(&mut noise, 9), paragraph(&mut noise, 7)],
            )
        })
        .collect();
    let chapters: Vec<(String, Vec<String>)> = (1..=3)
        .map(|n| {
            (
                format!("Chapter {n}"),
                (0..6).map(|_| paragraph(&mut noise, 32)).collect(),
            )
        })
        .collect();
    let cell = |row: usize, column: usize| match column {
        0 => format!("mod{row}"),
        1 => format!("{}", row * 3),
        2 => format!("{}", row * 97),
        3 => format!("src/mod{row}.rs"),
        _ => "ok".to_owned(),
    };
    let odf_rows: String = (1..=700)
        .map(|row| {
            let cells: String = (0..5)
                .map(|column| {
                    format!(
                        "<table:table-cell><text:p>{}</text:p></table:table-cell>",
                        cell(row, column)
                    )
                })
                .collect();
            format!("<table:table-row>{cells}</table:table-row>")
        })
        .collect();
    let odt_body: String = format!(
        "<office:text>{}</office:text>",
        handbook
            .iter()
            .map(|(heading, text)| if *heading {
                format!("<text:h text:outline-level=\"1\">{text}</text:h>")
            } else {
                format!("<text:p>{text}</text:p>")
            })
            .collect::<String>()
    );

    vec![
        member(
            "design/architecture.docx",
            "docx",
            "a design document with its diagram: prose, headings, one picture",
            docx(&design, Some(noise.bytes(48 * 1024))),
        ),
        member(
            "design/handbook.odt",
            "odt",
            "the same document from the other office suite, illustrated",
            odf(
                "application/vnd.oasis.opendocument.text",
                &odt_body,
                Some(noise.bytes(32 * 1024)),
            ),
        ),
        member(
            "notes/roadmap.pptx",
            "pptx",
            "a twelve-slide deck: little text, a picture, and a package of parts",
            pptx(&slides, noise.bytes(40 * 1024)),
        ),
        member(
            "budget/usage.xlsx",
            "xlsx",
            "a seven-hundred-row export: what the row cap is for",
            xlsx(700, cell),
        ),
        member(
            "budget/usage.ods",
            "ods",
            "the same export from the other suite",
            odf(
                "application/vnd.oasis.opendocument.spreadsheet",
                &format!("<office:spreadsheet><table:table table:name=\"Reads\">{odf_rows}</table:table></office:spreadsheet>"),
                None,
            ),
        ),
        member(
            "docs/manual.epub",
            "epub",
            "a three-chapter manual with a cover",
            epub(&chapters, noise.bytes(24 * 1024)),
        ),
        member(
            "budget/export.csv",
            "csv",
            "plain text a native read handles perfectly well: only the row cap saves anything here",
            (1..=700)
                .map(|row| {
                    format!(
                        "{}\r\n",
                        (0..5)
                            .map(|column| cell(row, column))
                            .collect::<Vec<_>>()
                            .join(",")
                    )
                })
                .collect::<String>()
                .into_bytes(),
        ),
        member(
            "intl/labels.csv",
            "csv",
            "the same export with CJK labels: characters worth about a token each, which is where the token ceiling binds",
            (1..=1_400)
                .map(|row| {
                    let label: String = (0..12)
                        .map(|n| {
                            char::from_u32(0x4E00 + u32::try_from((row * 13 + n) % 20_000).unwrap())
                                .unwrap()
                        })
                        .collect();
                    format!("{label},{row}\r\n")
                })
                .collect::<String>()
                .into_bytes(),
        ),
        member(
            "notes/memo.rtf",
            "rtf",
            "a memo: rich text is markup a native read also reads",
            rtf("A memo about reads", &memo),
        ),
        member(
            "notes/letter.doc",
            "doc",
            "rich text wearing a word extension, which the engine routes by its bytes",
            rtf("A letter", &letter),
        ),
        member(
            "docs/release.pdf",
            "pdf",
            "a PDF with a text layer: converted, and declared at nothing, because what a page costs a model is a vision rate no artifact in this tree pins",
            pdf(&[
                b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
                b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>".to_vec(),
                pdf_stream(
                    &pdf_text(
                        &(0..30)
                            .map(|_| paragraph(&mut noise, 9))
                            .collect::<Vec<_>>(),
                    ),
                    "",
                ),
                b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec(),
            ]),
        ),
        member(
            "scans/invoice.pdf",
            "pdf",
            "a scanned page: no text layer, so the family decides against it once and the read runs",
            pdf(&[
                b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
                b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /XObject << /Im0 6 0 R >> >> /Contents 4 0 R >>".to_vec(),
                pdf_stream(b"q 200 0 0 200 72 500 cm /Im0 Do Q", ""),
                b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec(),
                pdf_stream(
                    &[0x20, 0x40, 0x60, 0x80],
                    " /Type /XObject /Subtype /Image /Width 2 /Height 2 /ColorSpace /DeviceGray /BitsPerComponent 8",
                ),
            ]),
        ),
        member(
            "docs/appendix.pdf",
            "pdf",
            "one readable page and one scanned one: the engine returns nothing at all rather than the half it could read, and the corpus carries the case that answers nothing",
            pdf(&[
                b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
                b"<< /Type /Pages /Kids [3 0 R 7 0 R] /Count 2 >>".to_vec(),
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>".to_vec(),
                pdf_stream(&pdf_text(&[paragraph(&mut noise, 9)]), ""),
                b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec(),
                pdf_stream(
                    &[0x20, 0x40, 0x60, 0x80],
                    " /Type /XObject /Subtype /Image /Width 2 /Height 2 /ColorSpace /DeviceGray /BitsPerComponent 8",
                ),
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /XObject << /Im0 6 0 R >> >> /Contents 8 0 R >>".to_vec(),
                pdf_stream(b"q 200 0 0 200 72 500 cm /Im0 Do Q", ""),
            ]),
        ),
        member(
            "legacy/ledger.xls",
            "xls",
            "a legacy binary container the engine refuses: nothing is converted and the read runs as it was written",
            ole_header(),
        ),
    ]
}

// ------------------------------------------------------------- the session ---

/// A hermetic repository with one commit and a live session, so every hook
/// below lands in a real family with a real journal.
fn seed_repo(tmp: &Path) -> std::path::PathBuf {
    use std::process::{Command, Stdio};
    let repo = tmp.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let git = |args: &[&str]| {
        let status = Command::new("git")
            .args(args)
            .current_dir(&repo)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    };
    git(&["init", "-q"]);
    // An identity with no address in it. Git accepts any non-empty string,
    // and the repository's own redaction census counts every address-shaped
    // literal in its tracked text against a recorded ceiling — a fixture is
    // not a reason to spend one.
    git(&["config", "user.email", "documents"]);
    git(&["config", "user.name", "documents"]);
    std::fs::write(repo.join("a.txt"), "one\n").unwrap();
    git(&["add", "a.txt"]);
    git(&["commit", "-q", "-m", "seed"]);
    let sessions = repo.join(".git").join("rift-sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    std::fs::write(
        sessions.join("doc-1.json"),
        r#"{"schema":1,"session_id":"doc-1","state":"active","agent":"claude-code","current_turn_ordinal":1,"turn_open":true,"compaction_boundary":null,"native_id_map":{},"updated_at_ms":1783000000000}"#,
    )
    .unwrap();
    repo
}

/// A path as a JSON string literal: a Windows path carries backslashes, which
/// a raw `Display` splice turns into invalid escapes.
fn json_str(path: &Path) -> String {
    serde_json::Value::String(path.display().to_string()).to_string()
}

/// Drive one real agent hook the way the family sends it, and keep its stdout.
fn hook(repo: &Path, verb: &str, payload: &str, cache: &Path) -> serde_json::Value {
    use std::io::Write as _;
    use std::process::Stdio;
    let mut child = common::rift_cmd(repo)
        .args(["hooks", "claude-code", verb])
        .env("RIFT_CACHE_DIR", cache)
        .env("RIFT_NO_KEYCHAIN", "1")
        .env("RIFT_CAPTURE_SYNC", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "a hook never fails the agent: {verb}");
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    if text.trim().is_empty() {
        return serde_json::Value::Null;
    }
    serde_json::from_str(text.trim()).unwrap_or_else(|e| panic!("one object on stdout: {text} {e}"))
}

/// The family's one recall projection, resolved the way the product resolves
/// it — through the family manifest, never through a path.
fn rows(repo: &Path, cache: &Path, sql: &str) -> Vec<Vec<String>> {
    use rift_store::CanonicalStore as _;
    let paths = rift_git::discover(repo).expect("the fixture is a repository");
    let cache_dir = cache.display().to_string();
    let lookup = |key: &str| (key == "RIFT_CACHE_DIR").then(|| cache_dir.clone());
    let rift_git::FamilyState::Minted(family) =
        rift_git::family::resolve_family_under(lookup, &paths, rift_git::MintPolicy::Never)
            .unwrap()
    else {
        panic!("the session above should have minted the family");
    };
    let db = rift_store::recall_db_under(cache, &family.recall_id);
    let store = rift_store::open_migrated(db, &rift_store::recall_migrations()).unwrap();
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    rt.block_on(async {
        store
            .query(sql, &[])
            .await
            .unwrap()
            .iter()
            .map(|r| {
                (0..4)
                    .map_while(|c| {
                        r.get_text(c)
                            .ok()
                            .or_else(|| r.get_i64(c).ok().map(|n| n.to_string()))
                    })
                    .collect()
            })
            .collect()
    })
}

/// What one document cost, both ways.
struct Priced {
    format: &'static str,
    path: &'static str,
    role: &'static str,
    /// Whether the bar's denominator counts it — see [`Member::prescribed`].
    prescribed: bool,
    /// The declared native cost of reading the container: the pool this
    /// document is worth. Zero where nothing in this tree pins a rate.
    pool: u32,
    /// What the agent received instead — the Markdown it was pointed at, or,
    /// where Rift did nothing, the container it read itself.
    received: u32,
    /// Whether Rift answered this read with a sidecar at all.
    answered: bool,
}

/// Write the corpus into the repository and commit it, because a document an
/// agent asks about is one the repository has — and because the pass that
/// converts ahead of a session walks the repository's own documents rather
/// than whatever is lying in the checkout.
fn plant_the_documents(repo: &Path, corpus: &[Member]) {
    for member in corpus {
        let path = repo.join(member.path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &member.bytes).unwrap();
    }
    for args in [
        vec!["add", "-A"],
        vec!["commit", "-q", "-m", "the documents"],
    ] {
        assert!(std::process::Command::new("git")
            .args(&args)
            .current_dir(repo)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap()
            .success());
    }
}

/// The pass that converts before anybody asks, which is where a family's
/// documents really become Markdown.
///
/// Both channels are measured after it, on purpose. A conversion started while
/// an agent waits has a quarter-second to finish and hands the work to the
/// worker when it does not, so measuring the substitution against a cold store
/// would be measuring this machine's load: on a busy one a document is read as
/// it was, which is the right BEHAVIOUR and a meaningless SAVING. What is
/// asserted afterwards is that the store really was warmed — a run where the
/// pass did nothing goes red rather than publishing a low share.
fn warm_the_family(repo: &Path, cache: &Path) {
    hook(
        repo,
        "SessionStart",
        r#"{"session_id":"doc-1","hook_event_name":"SessionStart","source":"startup"}"#,
        cache,
    );
}

/// The documents no channel answers, and why each is one: two PDFs the engine
/// decides against (a page with no text layer, and a document one of whose
/// pages has none) and a legacy container it refuses.
const ANSWERED_BY_NOBODY: [&str; 3] = [
    "scans/invoice.pdf",
    "docs/appendix.pdf",
    "legacy/ledger.xls",
];

/// Assert that everything else really was answered.
///
/// The bar could otherwise be met by a corpus that converted nothing: every
/// unanswered document is charged its container, so a session that answered
/// none of them has a share of zero — but a session that answered most of them
/// and quietly deferred one would publish a number nobody could account for.
fn assert_every_convertible_document_was_answered(priced: &[Priced], channel: &str) {
    let unanswered: Vec<&str> = priced
        .iter()
        .filter(|p| !p.answered)
        .map(|p| p.path)
        .collect();
    assert_eq!(
        unanswered, ANSWERED_BY_NOBODY,
        "{channel}: exactly these documents are answered by nobody; anything \
         else in this list is a conversion that did not happen"
    );
}

/// The whole scripted session: an agent opens each document in the corpus,
/// once, through the shipped binary.
///
/// What is measured is what the agent ended up with. A read Rift amended is
/// followed by the result the tool really ran with, exactly as a client sends
/// it, so the substitution is verified rather than assumed; a read Rift left
/// alone delivered the container, and this fixture charges the session for
/// every token of it.
fn play_the_session(repo: &Path, cache: &Path, corpus: &[Member]) -> Vec<Priced> {
    let mut priced = Vec::new();
    for member in corpus {
        let path = repo.join(member.path);
        let ext = Path::new(member.path)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or_default();
        let pool = rift_recall_serve::declared_document_baseline(ext, &member.bytes);
        let call = format!(
            r#"{{"session_id":"doc-1","tool_name":"Read","tool_input":{{"file_path":{}}}}}"#,
            json_str(&path)
        );
        let answer = hook(repo, "PreToolUse", &call, cache);
        let sidecar = answer
            .pointer("/hookSpecificOutput/updatedInput/file_path")
            .and_then(serde_json::Value::as_str)
            .map(std::path::PathBuf::from);
        let received = match &sidecar {
            Some(view) => {
                let markdown = std::fs::read_to_string(view).unwrap_or_else(|e| {
                    panic!("the sidecar an agent was sent to is readable: {e}")
                });
                // The result the tool really ran with, so the amendment is
                // checked the way a live session checks it.
                let executed = format!(
                    r#"{{"session_id":"doc-1","tool_name":"Read","tool_input":{{"file_path":{}}},"tool_response":"ok"}}"#,
                    json_str(view)
                );
                hook(repo, "PostToolUse", &executed, cache);
                rift_tokens::count(&markdown)
            }
            // Rift did nothing, so the agent read the document itself and paid
            // what the container costs. Counted against the mechanism.
            None => pool,
        };
        priced.push(Priced {
            format: member.format,
            path: member.path,
            role: member.role,
            prescribed: member.prescribed,
            pool,
            received,
            answered: sidecar.is_some(),
        });
    }
    priced
}

// --------------------------------------------------------- what it removed ---

/// The figures one run of the corpus publishes.
struct Figures {
    /// What the session would have spent reading the containers: the sum of
    /// the declared native cost of every document the tree prices.
    pool: u32,
    /// What it received instead over those same documents.
    received: u32,
    /// `pool - received`.
    removed: u32,
    /// What the documents nobody prices cost the session: the PDFs, whose
    /// native cost is pages times a vision rate no artifact in this tree pins.
    /// Published, never divided — a share whose numerator has no denominator
    /// behind it is not a share.
    unpriced_received: u32,
    /// Σ avoided over the rows the product itself booked.
    avoided: u64,
    /// Σ returned over the same rows.
    returned: u64,
}

/// The corpus's own composition, asserted by format.
///
/// A member quietly dropped from a denominator would raise a share with
/// nothing going red, and that is the one way these figures could flatter
/// themselves. Both denominators are spelled out here, so moving a document
/// from one to the other is an edit somebody has to argue for.
fn assert_the_corpus_composition(corpus: &[Member]) {
    let of = |format: &str, prescribed: bool| {
        corpus
            .iter()
            .filter(|m| m.format == format && m.prescribed == prescribed)
            .count()
    };
    assert_eq!(
        (
            corpus.len(),
            corpus.iter().filter(|m| m.prescribed).count(),
            of("docx", true),
            of("odt", true),
            of("pptx", true),
            of("xlsx", true),
            of("epub", true),
            of("csv", true),
            of("rtf", true),
            of("pdf", true),
        ),
        (14, 10, 1, 1, 1, 1, 1, 1, 1, 3),
        "the corpus the bar sits on: one document per format the plan names, \
         and the three PDFs"
    );
    assert_eq!(
        (
            of("ods", false),
            of("doc", false),
            of("csv", false),
            of("xls", false)
        ),
        (1, 1, 1, 1),
        "and the four read beside it, each published under its own figure"
    );
}

/// The figures over one set of documents.
fn figures(priced: &[&Priced], booked: &[Vec<String>]) -> Figures {
    let sum = |f: &dyn Fn(&Priced) -> u32| -> u32 { priced.iter().map(|p| f(p)).sum() };
    let paths: Vec<&str> = priced.iter().map(|p| p.path).collect();
    let mine = |row: &Vec<String>| {
        paths
            .iter()
            .any(|path| row[0].starts_with(&format!("file:{path}@sha256:")))
    };
    let pool = sum(&|p| p.pool);
    let received = sum(&|p| if p.pool == 0 { 0 } else { p.received });
    Figures {
        pool,
        received,
        removed: pool.saturating_sub(received),
        unpriced_received: sum(&|p| if p.pool == 0 { p.received } else { 0 }),
        avoided: booked
            .iter()
            .filter(|row| mine(row))
            .map(|row| row[2].parse::<u64>().unwrap())
            .sum(),
        returned: booked
            .iter()
            .filter(|row| mine(row))
            .map(|row| row[3].parse::<u64>().unwrap())
            .sum(),
    }
}

/// A share as a percentage, with a share of nothing reported as nothing.
fn share(part: u32, whole: u32) -> f64 {
    if whole == 0 {
        0.0
    } else {
        f64::from(part) * 100.0 / f64::from(whole)
    }
}

/// Print one set of figures under a label.
fn publish(label: &str, f: &Figures, members: usize) {
    println!(
        "{label}: {members} documents · pool {} · received {} · removed {} ({:.2} %) · \
         booked avoided {} · booked returned {} · unpriced documents received {}",
        f.pool,
        f.received,
        f.removed,
        share(f.removed, f.pool),
        f.avoided,
        f.returned,
        f.unpriced_received,
    );
}

/// Run every document through the substitution channel and publish what each
/// cost, per document, per format, and over both denominators.
fn measure_the_corpus(repo: &Path, cache: &Path, corpus: &[Member]) -> (Figures, Figures) {
    let priced = play_the_session(repo, cache, corpus);
    assert_every_convertible_document_was_answered(&priced, "the substitution");
    // The product's own books, over the same session.
    let booked = rows(
        repo,
        cache,
        "SELECT handle, reason, tokens_avoided, tokens_returned FROM memd_audit_event \
         WHERE verb LIKE 'memd.doc%' ORDER BY handle ASC",
    );
    cross_check_the_bookings(&priced, &booked);

    println!("\nrecall-doc-savings: per document");
    for p in &priced {
        let removed = p.pool.saturating_sub(p.received);
        println!(
            "  {:<24} {:<5} pool {:>6} · received {:>6} · removed {:>6} ({:>6.2} %) {} {} — {}",
            p.path,
            p.format,
            p.pool,
            p.received,
            removed,
            share(removed, p.pool),
            if p.answered { "answered " } else { "untouched" },
            if p.prescribed { "in    " } else { "beside" },
            p.role,
        );
    }
    let mut formats: Vec<&str> = priced.iter().map(|p| p.format).collect();
    formats.sort_unstable();
    formats.dedup();
    println!("recall-doc-savings: per format, over every document read");
    for format in formats {
        let of = |f: &dyn Fn(&Priced) -> u32| -> u32 {
            priced.iter().filter(|p| p.format == format).map(f).sum()
        };
        let (pool, received) = (of(&|p| p.pool), of(&|p| p.received));
        println!(
            "  {format:<5} pool {:>7} · received {:>7} · removed {:>7} ({:>6.2} %)",
            pool,
            received,
            pool.saturating_sub(received),
            share(pool.saturating_sub(received), pool),
        );
    }
    let of_corpus: Vec<&Priced> = priced.iter().filter(|p| p.prescribed).collect();
    let everything: Vec<&Priced> = priced.iter().collect();
    let corpus_figures = figures(&of_corpus, &booked);
    let wider = figures(&everything, &booked);
    publish(
        "recall-doc-savings (the corpus)",
        &corpus_figures,
        of_corpus.len(),
    );
    publish(
        "recall-doc-savings (and beside it)",
        &wider,
        everything.len(),
    );
    // The bar's own scope, printed with the figure it is read against: the
    // share is over the formats this tree can price, and the PDFs are not
    // among them — no artifact here pins what a rendered page costs a model,
    // so they enter neither the numerator nor the denominator and are
    // published beside it with what they cost.
    println!(
        "recall-doc-savings: the gate is >= 90 % on the priced formats — {} of \
         the {} documents above; the {} unpriced (pdf) received {} and are \
         divided by nothing",
        of_corpus.iter().filter(|p| p.pool > 0).count(),
        of_corpus.len(),
        of_corpus.iter().filter(|p| p.pool == 0).count(),
        corpus_figures.unpriced_received,
    );
    (corpus_figures, wider)
}

/// Reconcile this fixture's own price for each document against the figure the
/// product booked for the same document.
///
/// Two independent measurements of one quantity: the fixture prices the bytes
/// it wrote, and the product prices the bytes it found under the hash its own
/// decision recorded. A booked figure that differed would mean one of the two
/// is describing a document the session did not read.
fn cross_check_the_bookings(priced: &[Priced], booked: &[Vec<String>]) {
    for p in priced {
        let prefix = format!("file:{}@sha256:", p.path);
        let mut rows = booked.iter().filter(|row| row[0].starts_with(&prefix));
        let Some(row) = rows.next() else {
            assert!(
                !p.answered,
                "{} was answered with a sidecar and nothing was booked",
                p.path
            );
            continue;
        };
        if !p.answered {
            // A document this channel did not answer books its own row and no
            // figure: a conversion the family decided against or handed to the
            // worker saved nothing, and the container is charged above.
            assert_eq!(
                row[2], "0",
                "an unanswered document claims nothing: {row:?}"
            );
            continue;
        }
        assert!(
            rows.next().is_none(),
            "two bookings for one document would price {} twice",
            p.path
        );
        assert_eq!(
            row[2].parse::<u32>().unwrap(),
            p.pool,
            "the product's declared cost of {} and this fixture's are one \
             quantity: {row:?}",
            p.path
        );
        assert_eq!(
            row[1],
            rift_recall_serve::TAG_DECLARED_BASELINE,
            "a declared figure says on the row that it is one: {row:?}"
        );
    }
}

/// ★ The corpus's own numbers, and the nine-tenths bar.
///
/// **What the pool is.** The declared native cost of every document the tree
/// can price — the bytes as a client hands them to a model when it opens a
/// container it cannot parse, cut at the client's own two ceilings. Declared,
/// never measured: the read this prices never happened, so there is no second
/// reading to count, and every row that carries the figure says so.
///
/// **What removed is.** The pool minus what the agent received instead: the
/// Markdown it was pointed at, and — for every document Rift left alone — the
/// container it read itself, charged in full. It is NOT the avoided column,
/// which is published beside it along with what the decisions cost.
///
/// **What the bar sits on.** The corpus the plan named before any of this was
/// measured: one document per format it lists, and the three PDFs — and the
/// bar itself is **≥ 90 % on the priced formats**, because the PDFs declare
/// nothing (see below) and a share cannot be taken over a pool nobody has
/// measured. Four more
/// documents are read in the same session and published in the same lines —
/// a second spreadsheet, a second rich-text file, a dense-script export and a
/// legacy container the engine refuses — and the wider figure they make is
/// printed beside the corpus's own, because a bar that could be met by
/// choosing a denominator afterwards would be worth nothing.
///
/// **What is outside every fraction and why.** The PDFs' pool. A PDF is not
/// presented as text at all; its honest cost is pages times a vision-token
/// rate, no artifact in this tree pins that rate, and inventing one is the one
/// thing a declared figure must never be. They are read, converted and
/// published here with what they cost — they simply cannot be divided by a
/// denominator nobody has measured.
#[test]
fn the_doc_fixture_removes_at_least_ninety_percent_of_its_pool() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_repo(tmp.path());
    let cache = tmp.path().join("cache");
    let corpus = corpus();
    assert_the_corpus_composition(&corpus);
    plant_the_documents(&repo, &corpus);
    warm_the_family(&repo, &cache);
    let (f, wider) = measure_the_corpus(&repo, &cache, &corpus);

    assert!(f.pool > 0, "a fixture with no pool measures nothing");
    assert!(
        f.received > 0 && f.received < f.pool,
        "the Markdown the agent read instead is never free, and never dearer \
         than the documents it replaced — pool {}, received {}",
        f.pool,
        f.received
    );
    assert!(
        f.avoided > f.returned,
        "the honesty gate: what the substitutions declared must exceed what \
         the decisions cost — avoided {}, returned {}",
        f.avoided,
        f.returned
    );
    assert!(
        wider.avoided > wider.returned,
        "and over every document read, not only the corpus — avoided {}, \
         returned {}",
        wider.avoided,
        wider.returned
    );
    assert!(
        u64::from(f.removed) * 100 >= u64::from(f.pool) * 90,
        "the fixture removed {} of a {}-token pool ({:.2} per cent), under the \
         nine-tenths bar on the priced formats; the per-format lines above \
         name which format is short. Publish this number and raise it as a \
         decision, never tune the corpus",
        f.removed,
        f.pool,
        share(f.removed, f.pool)
    );
    assert!(
        f.unpriced_received > 0,
        "the documents nobody prices still cost something, and the figure is \
         published rather than divided"
    );

    // And the owner's own figure is this one. The savings verb sums the same
    // relation the fixture just read, so a document decision reaches the
    // number a person is shown — once, under the family that made it.
    let savings = savings_json(&repo, &cache);
    assert_eq!(
        savings["overall"]["tokens_avoided"].as_u64(),
        Some(wider.avoided),
        "the verb counts this session's documents once: {savings}"
    );
    let families = savings["by_family"]
        .as_array()
        .expect("the per-family rows");
    assert_eq!(
        families
            .iter()
            .filter(|row| row["family"] == "claude-code")
            .count(),
        1,
        "one family made every one of these decisions: {savings}"
    );
}

/// The figure a person is shown, as the verb prints it.
fn savings_json(repo: &Path, cache: &Path) -> serde_json::Value {
    let out = common::rift_cmd(repo)
        .args(["savings", "--json"])
        .env("RIFT_CACHE_DIR", cache)
        .env("RIFT_NO_KEYCHAIN", "1")
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    serde_json::from_str(text.trim()).unwrap_or_else(|e| panic!("one object: {text} {e}"))
}

/// One container, to watch a single amendment through each of its two
/// outcomes: the same writer the corpus uses, at a size that declares plainly
/// more than nothing.
fn one_document() -> Member {
    let mut noise = Noise::new(0x0D0C_1234_5678_9ABC);
    Member {
        path: "design/plan.docx",
        format: "docx",
        role: "one amendment, followed to what the client really ran",
        prescribed: false,
        bytes: docx(&prose(&mut noise, 24, 14), None),
    }
}

/// A session that reads `member` once through the shipped binary, and the
/// path Rift amended the call to.
fn amended_read(repo: &Path, cache: &Path, member: &Member) -> String {
    plant_the_documents(repo, std::slice::from_ref(member));
    warm_the_family(repo, cache);
    let call = format!(
        r#"{{"session_id":"doc-1","tool_name":"Read","tool_input":{{"file_path":{}}}}}"#,
        json_str(&repo.join(member.path))
    );
    hook(repo, "PreToolUse", &call, cache)
        .pointer("/hookSpecificOutput/updatedInput/file_path")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .expect("the read of a document this family holds is amended")
}

/// The result of that read, as the client sends it: the input the tool really
/// ran with.
fn result_of(repo: &Path, cache: &Path, executed: &Path) {
    let payload = format!(
        r#"{{"session_id":"doc-1","tool_name":"Read","tool_input":{{"file_path":{}}},"tool_response":"ok"}}"#,
        json_str(executed)
    );
    hook(repo, "PostToolUse", &payload, cache);
}

/// The document rows the product booked, as `rift savings` sums them.
fn document_rows(repo: &Path, cache: &Path) -> Vec<Vec<String>> {
    rows(
        repo,
        cache,
        "SELECT handle, reason, tokens_avoided, tokens_returned FROM memd_audit_event \
         WHERE verb LIKE 'memd.doc%' ORDER BY handle ASC",
    )
}

/// ★ A substitution the client dropped claims nothing.
///
/// The client applies the last amendment it is handed for one event, so a
/// sibling hook can take a call Rift amended and the agent reads the container
/// in full. The read's own result is where that is found out — and the figure
/// a person is shown must not already contain a saving that result has just
/// contradicted. The document is one this fixture can price, so a zero here is
/// a claim withheld and not a document worth nothing.
#[test]
fn a_dropped_substitution_leaves_the_savings_at_zero_for_the_document_it_did_not_replace() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_repo(tmp.path());
    let cache = tmp.path().join("cache");
    let member = one_document();
    let declared = rift_recall_serve::declared_document_baseline("docx", &member.bytes);
    assert!(declared > 0, "the document is worth claiming: {declared}");

    amended_read(&repo, &cache, &member);
    // The result names the DOCUMENT: Rift's amendment is not the one the
    // client kept, and the agent read every byte of the container.
    result_of(&repo, &cache, &repo.join(member.path));

    let booked = document_rows(&repo, &cache);
    assert!(
        booked.iter().any(|row| row[1] == "rewrite_dropped"),
        "the drop itself is on the record: {booked:?}"
    );
    assert!(
        booked.iter().all(|row| row[2] == "0"),
        "and no row claims a document the agent read: {booked:?}"
    );
    assert_eq!(
        savings_json(&repo, &cache)["overall"]["tokens_avoided"].as_u64(),
        Some(0),
        "the figure a person is shown credits nothing to a read that happened"
    );
}

/// ★ A substitution the client applied books the document's declared cost,
/// once.
///
/// The other half of the same claim: where the executed call names the sidecar
/// the read the agent wrote never happened, and that is the one arm of this
/// channel that may claim a saving. Two results for one amendment — a client
/// that repeats one, a hook that runs twice — still book it once.
#[test]
fn a_verified_substitution_books_the_declared_figure_once() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_repo(tmp.path());
    let cache = tmp.path().join("cache");
    let member = one_document();
    let declared = rift_recall_serve::declared_document_baseline("docx", &member.bytes);

    let sidecar = amended_read(&repo, &cache, &member);
    result_of(&repo, &cache, Path::new(&sidecar));
    result_of(&repo, &cache, Path::new(&sidecar));

    let booked = document_rows(&repo, &cache);
    let mine: Vec<&Vec<String>> = booked
        .iter()
        .filter(|row| row[0].starts_with(&format!("file:{}@sha256:", member.path)))
        .collect();
    assert_eq!(mine.len(), 1, "one amendment, one row: {booked:?}");
    assert_eq!(
        mine[0][1],
        rift_recall_serve::TAG_DECLARED_BASELINE,
        "a declared figure says on the row that it is one"
    );
    assert_eq!(
        mine[0][2].parse::<u32>().unwrap(),
        declared,
        "and it is the same quantity this fixture prices the document at"
    );
    assert_eq!(
        savings_json(&repo, &cache)["overall"]["tokens_avoided"].as_u64(),
        Some(u64::from(declared)),
        "which is the figure a person is shown, counted once"
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

/// One `rift recall doc-read --json` call, as an agent with no amendment
/// channel makes it.
fn doc_read(repo: &Path, cache: &Path, rel: &str) -> serde_json::Value {
    let out = common::rift_cmd(repo)
        .args(["--json", "recall", "doc-read", rel])
        .env("RIFT_CACHE_DIR", cache)
        .env("RIFT_NO_KEYCHAIN", "1")
        .env("RIFT_CAPTURE_SYNC", "1")
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    serde_json::from_str(text.trim()).unwrap_or_else(|e| panic!("one object on stdout: {text} {e}"))
}

/// The same corpus, asked for rather than amended: one `doc-read` per
/// document, and what each answer cost the caller.
fn ask_for_each_document(repo: &Path, cache: &Path, corpus: &[Member]) -> Vec<Priced> {
    let mut priced: Vec<Priced> = Vec::new();
    for member in corpus {
        let ext = Path::new(member.path)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or_default();
        let declared = rift_recall_serve::declared_document_baseline(ext, &member.bytes);
        // The verb never converts: an ask the proactive pass has not reached
        // yet is answered with what the family holds and journals the work,
        // and the ask after it is answered from the store.
        let first = doc_read(repo, cache, member.path);
        let answer = if first.pointer("/result/state") == Some(&serde_json::json!("pending")) {
            doc_read(repo, cache, member.path)
        } else {
            first
        };
        let cost = |key: &str| -> u32 {
            u32::try_from(
                answer
                    .get(key)
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0),
            )
            .unwrap_or(u32::MAX)
        };
        let served = answer
            .pointer("/result/content")
            .is_some_and(|c| !c.is_null());
        if served {
            assert_eq!(
                cost("estimated_tokens_avoided"),
                declared.saturating_sub(cost("estimated_tokens_returned")),
                "the verb books the declared cost net of what it returned: {}",
                member.path
            );
        }
        priced.push(Priced {
            format: member.format,
            path: member.path,
            role: member.role,
            prescribed: member.prescribed,
            pool: declared,
            // An answer that carried no Markdown left the caller to read the
            // document itself, so the container is what it cost.
            received: if served {
                cost("estimated_tokens_returned")
            } else {
                declared
            },
            answered: served,
        });
    }
    priced
}

/// ★ The same corpus through the verb, for the families that have no
/// amendment channel — published beside the substitution's figure, on the same
/// documents, and held to no bar.
///
/// The two channels are measured in separate sessions on purpose. Each call an
/// agent makes is a read it would otherwise have paid for, so each books its
/// own row; running both channels over one session would credit one session
/// with two savings for one document, which is a defect and not a sum.
///
/// **Why no bar here.** What the caller receives is the whole response, not
/// the Markdown alone — the verb's answer is a document with the sidecar
/// inside it, and it pays for its own envelope on every call — so this share
/// is under the substitution's by construction, and the bar was set on the
/// substitution. The figure is published for an owner to weigh, and what is
/// asserted is what has to hold on any channel: the answers save more than
/// they cost, and every one of them books what it really returned.
#[test]
fn the_pull_path_serves_the_same_corpus_and_publishes_its_own_figures() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_repo(tmp.path());
    let cache = tmp.path().join("cache");
    let corpus = corpus();
    std::fs::create_dir_all(repo.join(".rift")).unwrap();
    std::fs::write(
        repo.join(".rift").join("config.toml"),
        "schema_version = 1\n\n[project]\nname = \"corpus\"\n\n[features]\ntotal_recall = true\n",
    )
    .unwrap();
    plant_the_documents(&repo, &corpus);
    // The channel this verb belongs to has no amendment, so its sidecars are
    // made the way they are made for those families: the detached pass at the
    // start of a session, before anybody asks.
    warm_the_family(&repo, &cache);

    let priced = ask_for_each_document(&repo, &cache, &corpus);
    let booked = rows(
        &repo,
        &cache,
        "SELECT handle, reason, tokens_avoided, tokens_returned FROM memd_audit_event \
         WHERE verb LIKE 'memd.doc%' ORDER BY handle ASC",
    );
    println!("\nrecall-doc-savings (the verb): per document");
    for p in &priced {
        println!(
            "  {:<24} {:<5} pool {:>6} · received {:>6} {} {} — {}",
            p.path,
            p.format,
            p.pool,
            p.received,
            if p.answered { "answered " } else { "untouched" },
            if p.prescribed { "in    " } else { "beside" },
            p.role,
        );
    }
    let of_corpus: Vec<&Priced> = priced.iter().filter(|p| p.prescribed).collect();
    let everything: Vec<&Priced> = priced.iter().collect();
    let f = figures(&of_corpus, &booked);
    let wider = figures(&everything, &booked);
    publish(
        "recall-doc-savings (the verb, the corpus)",
        &f,
        of_corpus.len(),
    );
    publish(
        "recall-doc-savings (the verb, and beside it)",
        &wider,
        everything.len(),
    );

    assert!(f.pool > 0 && f.received > 0);
    assert!(
        f.avoided > f.returned,
        "the honesty gate on the asking channel, where every answer really \
         does cost something: avoided {}, returned {}",
        f.avoided,
        f.returned
    );
    assert!(
        f.removed > f.received,
        "the asking channel still removes more than it costs — removed {}, \
         received {}",
        f.removed,
        f.received
    );
    assert!(
        wider.removed > 0 && wider.avoided > wider.returned,
        "and so does the wider set: removed {}, avoided {}, returned {}",
        wider.removed,
        wider.avoided,
        wider.returned
    );
}
