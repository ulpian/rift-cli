//! The document corpus every document suite measures over, and the driver
//! that puts a real hook call against it: the writers each format is made
//! with, the members and the reason each one is in the corpus, the hermetic
//! checkout, the one-process-per-hook spawn, and the reader that looks at
//! what a call left behind.
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
//! Each document suite includes this file by `#[path]` as its own module and
//! uses a subset of the builders, which is why each declaration carries
//! `#[allow(dead_code)]`: the allow names a fact about the include, not a
//! suppression.
#![allow(
    // A corpus builder is a string builder: every document below is assembled
    // by mapping rows and paragraphs into markup and collecting them. The
    // fold-with-`write!` form the lint prefers hides what each writer emits,
    // which is the one thing a reviewer of a fixture corpus has to be able to
    // read.
    clippy::format_collect
)]

use std::path::Path;

use super::common;

// ----------------------------------------------------------- the generator ---

/// A deterministic byte stream, standing in for a document's media part.
///
/// A picture in a container is bytes with no text in them, and that is the
/// whole of what this has to reproduce. Written out rather than taken from a
/// crate so the corpus depends on nothing but this file.
pub(crate) struct Noise(u64);

impl Noise {
    pub(crate) const fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn byte(&mut self) -> u8 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 33) as u8
    }

    pub(crate) fn bytes(&mut self, n: usize) -> Vec<u8> {
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
pub(crate) fn paragraph(noise: &mut Noise, words: usize) -> String {
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
pub(crate) fn prose(noise: &mut Noise, count: usize, words: usize) -> Vec<(bool, String)> {
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
pub(crate) fn docx(body: &[(bool, String)], media: Option<Vec<u8>>) -> Vec<u8> {
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
pub(crate) fn xlsx(rows: usize, cell: impl Fn(usize, usize) -> String) -> Vec<u8> {
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
pub(crate) fn pptx(slides: &[(String, Vec<String>)], media: Vec<u8>) -> Vec<u8> {
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
pub(crate) fn odf(mimetype: &str, body: &str, media: Option<Vec<u8>>) -> Vec<u8> {
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
pub(crate) fn epub(chapters: &[(String, Vec<String>)], cover: Vec<u8>) -> Vec<u8> {
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
pub(crate) fn rtf(title: &str, body: &[(bool, String)]) -> Vec<u8> {
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
pub(crate) fn pdf(objects: &[Vec<u8>]) -> Vec<u8> {
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

pub(crate) fn pdf_stream(body: &[u8], extra: &str) -> Vec<u8> {
    let mut out = format!("<< /Length {}{extra} >>\nstream\n", body.len()).into_bytes();
    out.extend_from_slice(body);
    out.extend_from_slice(b"\nendstream");
    out
}

/// A page of typeset lines, in the content-stream form a reader recovers text
/// from.
pub(crate) fn pdf_text(lines: &[String]) -> Vec<u8> {
    let mut body = b"BT /F1 11 Tf 54 760 Td\n".to_vec();
    for line in lines {
        body.extend_from_slice(format!("({line}) Tj 0 -14 Td\n").as_bytes());
    }
    body.extend_from_slice(b"ET");
    body
}

/// An OLE compound-file header whose directory chain leads nowhere — what a
/// hand-authored legacy container really is, and what the engine refuses.
pub(crate) fn ole_header() -> Vec<u8> {
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

// -------------------------------------------------------------- the corpus ---

/// One document in the corpus, with the reason the corpus has it.
pub(crate) struct Member {
    /// Where it sits in the repository.
    pub(crate) path: &'static str,
    /// The format, as the breakdown names it.
    pub(crate) format: &'static str,
    /// Why a corpus that prices documents contains this one. Printed beside
    /// the figures, so a member that turns out to be unlike a real document
    /// is argued about rather than quietly re-sized.
    pub(crate) role: &'static str,
    /// Whether this document is one of the formats the bar was set over.
    ///
    /// The rule is the plan's and predates every number here: **one document
    /// per format the plan names** — DOCX, XLSX, PPTX, ODT, EPUB, CSV, RTF and
    /// the three PDFs. The other extensions the converter accepts are proved
    /// by the converter's own corpus, and they are read, priced and published
    /// here too — under their own figure, because a bar has to sit on a
    /// denominator somebody fixed before the measuring, and this one did.
    pub(crate) prescribed: bool,
    /// The bytes on disk.
    pub(crate) bytes: Vec<u8>,
}

impl Member {
    /// A member of the denominator the bar sits on, until [`corpus`] reads
    /// the list of the documents that are read beside it.
    fn new(path: &'static str, format: &'static str, role: &'static str, bytes: Vec<u8>) -> Self {
        Self {
            path,
            format,
            role,
            prescribed: true,
            bytes,
        }
    }
}

/// The prose every member of the corpus is written from, drawn in one pass.
///
/// Every document is a function of one seed, so the ORDER the draws happen in
/// is part of the corpus: drawing them all here keeps that order in one place
/// a reader can check, rather than spread across the groups below.
struct CorpusProse {
    design: Vec<(bool, String)>,
    handbook: Vec<(bool, String)>,
    memo: Vec<(bool, String)>,
    letter: Vec<(bool, String)>,
    slides: Vec<(String, Vec<String>)>,
    chapters: Vec<(String, Vec<String>)>,
}

impl CorpusProse {
    fn draw(noise: &mut Noise) -> Self {
        let design = prose(noise, 24, 34);
        let handbook = prose(noise, 18, 30);
        let memo = prose(noise, 5, 28);
        let letter = prose(noise, 7, 26);
        let slides: Vec<(String, Vec<String>)> = (1..=12)
            .map(|n| {
                (
                    format!("Quarter {n}"),
                    vec![paragraph(noise, 9), paragraph(noise, 7)],
                )
            })
            .collect();
        let chapters: Vec<(String, Vec<String>)> = (1..=3)
            .map(|n| {
                (
                    format!("Chapter {n}"),
                    (0..6).map(|_| paragraph(noise, 32)).collect(),
                )
            })
            .collect();
        Self {
            design,
            handbook,
            memo,
            letter,
            slides,
            chapters,
        }
    }
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
pub(crate) fn corpus() -> Vec<Member> {
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
/// The three groups below are the three claims the corpus makes, and each
/// member's size stays beside the reason it is in the group. The prose is
/// drawn first and in one place, so grouping the members changes nothing
/// about the bytes: the corpus is still a pure function of this seed.
pub(crate) fn documents() -> Vec<Member> {
    let mut noise = Noise::new(0x5EED_0F0C_0DE1_2345);
    let prose = CorpusProse::draw(&mut noise);
    let mut all = illustrated_containers(&mut noise, &prose);
    all.extend(plain_text_exports(&prose));
    all.extend(documents_rift_decides_against(&mut noise));
    all
}

/// The containers: a package of parts a native read pays for in full and
/// cannot use, which is where the substitution is worth the most. Each has
/// the media a real document of its kind carries.
fn illustrated_containers(noise: &mut Noise, prose: &CorpusProse) -> Vec<Member> {
    vec![
        Member::new(
            "design/architecture.docx",
            "docx",
            "a design document with its diagram: prose, headings, one picture",
            docx(&prose.design, Some(noise.bytes(48 * 1024))),
        ),
        Member::new(
            "design/handbook.odt",
            "odt",
            "the same document from the other office suite, illustrated",
            odf(
                "application/vnd.oasis.opendocument.text",
                &odt_body(&prose.handbook),
                Some(noise.bytes(32 * 1024)),
            ),
        ),
        Member::new(
            "notes/roadmap.pptx",
            "pptx",
            "a twelve-slide deck: little text, a picture, and a package of parts",
            pptx(&prose.slides, noise.bytes(40 * 1024)),
        ),
        Member::new(
            "budget/usage.xlsx",
            "xlsx",
            "a seven-hundred-row export: what the row cap is for",
            xlsx(700, cell),
        ),
        Member::new(
            "budget/usage.ods",
            "ods",
            "the same export from the other suite",
            odf(
                "application/vnd.oasis.opendocument.spreadsheet",
                &ods_body(),
                None,
            ),
        ),
        Member::new(
            "docs/manual.epub",
            "epub",
            "a three-chapter manual with a cover",
            epub(&prose.chapters, noise.bytes(24 * 1024)),
        ),
    ]
}

/// The plain-text formats: a native read handles them perfectly well, so only
/// the row cap saves anything — and one of them is dense enough that the
/// token ceiling binds before the character ceiling does.
fn plain_text_exports(prose: &CorpusProse) -> Vec<Member> {
    vec![
        Member::new(
            "budget/export.csv",
            "csv",
            "plain text a native read handles perfectly well: only the row cap saves anything here",
            csv_export(),
        ),
        Member::new(
            "intl/labels.csv",
            "csv",
            "the same export with CJK labels: characters worth about a token each, which is where the token ceiling binds",
            dense_script_export(),
        ),
        Member::new(
            "notes/memo.rtf",
            "rtf",
            "a memo: rich text is markup a native read also reads",
            rtf("A memo about reads", &prose.memo),
        ),
        Member::new(
            "notes/letter.doc",
            "doc",
            "rich text wearing a word extension, which the engine routes by its bytes",
            rtf("A letter", &prose.letter),
        ),
    ]
}

/// The documents Rift decides against: the PDFs it declares nothing for,
/// because what a page costs a model is a vision rate no artifact in this
/// tree pins, and the legacy container the engine refuses outright.
fn documents_rift_decides_against(noise: &mut Noise) -> Vec<Member> {
    vec![
        Member::new(
            "docs/release.pdf",
            "pdf",
            "a PDF with a text layer: converted, and declared at nothing, because what a page costs a model is a vision rate no artifact in this tree pins",
            pdf_with_a_text_layer(noise),
        ),
        Member::new(
            "scans/invoice.pdf",
            "pdf",
            "a scanned page: no text layer, so the family decides against it once and the read runs",
            pdf_of_a_scan(),
        ),
        Member::new(
            "docs/appendix.pdf",
            "pdf",
            "one readable page and one scanned one: the engine returns nothing at all rather than the half it could read, and the corpus carries the case that answers nothing",
            pdf_half_scanned(noise),
        ),
        Member::new(
            "legacy/ledger.xls",
            "xls",
            "a legacy binary container the engine refuses: nothing is converted and the read runs as it was written",
            ole_header(),
        ),
    ]
}

/// The handbook as an OpenDocument body: a heading stays a heading, so the
/// conversion has the same structure to recover as the DOCX above.
fn odt_body(handbook: &[(bool, String)]) -> String {
    format!(
        "<office:text>{}</office:text>",
        handbook
            .iter()
            .map(|(heading, text)| if *heading {
                format!("<text:h text:outline-level=\"1\">{text}</text:h>")
            } else {
                format!("<text:p>{text}</text:p>")
            })
            .collect::<String>()
    )
}

/// The seven-hundred-row export as an OpenDocument spreadsheet body.
fn ods_body() -> String {
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
    format!("<office:spreadsheet><table:table table:name=\"Reads\">{odf_rows}</table:table></office:spreadsheet>")
}

/// The same export as the comma-separated file a report is mailed as.
fn csv_export() -> Vec<u8> {
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
        .into_bytes()
}

/// The same export again, labelled in a script whose characters cost about a
/// token each: the one document whose declaration the token ceiling decides.
fn dense_script_export() -> Vec<u8> {
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
        .into_bytes()
}

/// One cell of the export every spreadsheet member is built from: the columns
/// a usage report really has, so a row is a row and not a repeated string.
fn cell(row: usize, column: usize) -> String {
    match column {
        0 => format!("mod{row}"),
        1 => format!("{}", row * 3),
        2 => format!("{}", row * 97),
        3 => format!("src/mod{row}.rs"),
        _ => "ok".to_owned(),
    }
}

/// A PDF with a text layer: thirty typeset lines a reader recovers.
fn pdf_with_a_text_layer(noise: &mut Noise) -> Vec<u8> {
    pdf(&[
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>".to_vec(),
        pdf_stream(
            &pdf_text(
                &(0..30)
                    .map(|_| paragraph(noise, 9))
                    .collect::<Vec<_>>(),
            ),
            "",
        ),
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec(),
    ])
}

/// A scanned page: one image and no text layer at all.
fn pdf_of_a_scan() -> Vec<u8> {
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
    ])
}

/// One readable page and one scanned one, which is the document the engine
/// answers nothing for rather than answering the half it could read.
fn pdf_half_scanned(noise: &mut Noise) -> Vec<u8> {
    pdf(&[
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R 7 0 R] /Count 2 >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>".to_vec(),
        pdf_stream(&pdf_text(&[paragraph(noise, 9)]), ""),
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec(),
        pdf_stream(
            &[0x20, 0x40, 0x60, 0x80],
            " /Type /XObject /Subtype /Image /Width 2 /Height 2 /ColorSpace /DeviceGray /BitsPerComponent 8",
        ),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /XObject << /Im0 6 0 R >> >> /Contents 8 0 R >>".to_vec(),
        pdf_stream(b"q 200 0 0 200 72 500 cm /Im0 Do Q", ""),
    ])
}

// ------------------------------------------------------------- the session ---

/// A hermetic repository with one commit and a live session, so every hook
/// below lands in a real family with a real journal.
pub(crate) fn seed_repo(tmp: &Path) -> std::path::PathBuf {
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
    let sessions = super::common::seed_sessions_dir(&repo.join(".git"));
    std::fs::create_dir_all(&sessions).unwrap();
    std::fs::write(
        sessions.join("doc-1.json"),
        r#"{"schema":1,"session_id":"doc-1","state":"active","agent":"claude-code","current_turn_ordinal":1,"turn_open":true,"compaction_boundary":null,"native_id_map":{},"updated_at_ms":1783000000000}"#,
    )
    .unwrap();
    repo
}

/// Give the gate's conversion a budget the figures do not depend on.
///
/// The warm pass converts under the worker's own thirty seconds, and a
/// document it did not finish on a loaded machine is converted again when the
/// session reads it — under the policy's quarter-second, which a debug build
/// re-spawned on that machine misses, since that quarter-second includes the
/// exec. The read then proceeds unconverted, nothing is amended, and a suite
/// would be measuring the load. The budget changes when a conversion is given
/// up, never what one says: it is outside the sidecar key.
pub(crate) fn state_a_patient_budget(repo: &Path) {
    let policies = repo.join(".rift").join("policies");
    std::fs::create_dir_all(&policies).unwrap();
    std::fs::write(
        policies.join("serve.toml"),
        "[doc]\nconvert_budget_ms = 30000\n",
    )
    .unwrap();
}

/// A path as a JSON string literal: a Windows path carries backslashes, which
/// a raw `Display` splice turns into invalid escapes.
pub(crate) fn json_str(path: &Path) -> String {
    serde_json::Value::String(path.display().to_string()).to_string()
}

/// Drive one real agent hook the way the family sends it, and keep its stdout.
pub(crate) fn hook(repo: &Path, verb: &str, payload: &str, cache: &Path) -> serde_json::Value {
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

/// The document rows the product booked, as `rift savings` sums them.
pub(crate) fn document_rows(repo: &Path, cache: &Path) -> Vec<Vec<String>> {
    rows(
        repo,
        cache,
        "SELECT handle, reason, tokens_avoided, tokens_returned FROM memd_audit_event \
         WHERE verb LIKE 'memd.doc%' ORDER BY handle ASC",
    )
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
    let store = rift_store::open_recall_projection(db).unwrap();
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

/// Write the corpus into the repository and commit it, because a document an
/// agent asks about is one the repository has — and because the pass that
/// converts ahead of a session walks the repository's own documents rather
/// than whatever is lying in the checkout.
pub(crate) fn plant_the_documents(repo: &Path, corpus: &[Member]) {
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
pub(crate) fn warm_the_family(repo: &Path, cache: &Path) {
    hook(
        repo,
        "SessionStart",
        r#"{"session_id":"doc-1","hook_event_name":"SessionStart","source":"startup"}"#,
        cache,
    );
}

/// The figure a person is shown, as the verb prints it.
pub(crate) fn savings_json(repo: &Path, cache: &Path) -> serde_json::Value {
    let out = common::rift_cmd(repo)
        .args(["savings", "--json"])
        .env("RIFT_CACHE_DIR", cache)
        .env("RIFT_NO_KEYCHAIN", "1")
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    serde_json::from_str(text.trim()).unwrap_or_else(|e| panic!("one object: {text} {e}"))
}
