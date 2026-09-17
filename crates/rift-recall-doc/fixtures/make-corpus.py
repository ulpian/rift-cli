#!/usr/bin/env python3
"""Build the document fixture corpus, byte-for-byte the same on every run.

    python3 crates/rift-recall-doc/fixtures/make-corpus.py crates/rift-recall-doc/fixtures

The fixtures are committed, not generated at test time; this script is how they
came to exist and how a reviewer checks that they are what they claim to be
(run it into a scratch directory and diff). Every byte is authored here, so the
corpus carries no file whose provenance is "somebody had one lying around" --
which matters more than usual for a parser corpus, since a document from the
wild can carry anything.

Determinism, and why each piece of it is needed:

* Archives are written with ZIP_STORED and a fixed 1980 timestamp, and their
  entries carry a fixed creator and mode. A deflated entry would depend on the
  zlib build that ran the script, and a real timestamp would make every run a
  different file.
* No compression anywhere means every fixture is readable with `unzip -p` and
  reviewable as text, which a binary corpus otherwise is not.
* Documents are minimal on purpose: a package part that is not needed to prove
  something is a part nobody reads. `docx` carries a styles part because the
  heading level comes from it; the ODF documents carry `content.xml` and a
  mimetype and nothing else, because that is all the reader consults.

Three of the fourteen extensions cannot be honestly authored here:

* `.xls` and `.ppt` are OLE compound files whose readable form is a BIFF or
  PowerPoint record stream, and neither this script nor the Python standard
  library can write one. They are carried as what a hand-authored one actually
  is -- an OLE header with no usable directory -- and their goldens pin the
  refusal rather than a conversion. A real legacy document dropped in beside
  them, with its golden, would replace them without a code change.
* `.doc` is authored as Rich Text wearing a Word extension, which is the case
  the engine explicitly routes by content ("RTF files wearing a .doc extension
  are common in the wild"); the binary Word 97 container is in the same
  position as `.xls` and `.ppt`.

The goldens beside each fixture are not written here: they are what the
converter makes of these bytes, and regenerating them from anything but the
converter would make them agree with themselves instead of with it.

    cargo run -p rift -- hooks doc convert crates/rift-recall-doc/fixtures/corpus/spec.docx
"""

import os
import struct
import sys
import zipfile

OUT = sys.argv[1] if len(sys.argv) > 1 else "."
CORPUS = os.path.join(OUT, "corpus")

# --------------------------------------------------------------- the writers

# 1980-01-01: the earliest a zip timestamp can express, and the same on every
# machine that runs this.
EPOCH = (1980, 1, 1, 0, 0, 0)


def zipped(name, entries):
    """One stored-entry archive: OOXML, ODF and EPUB are all this shape."""
    with open(os.path.join(CORPUS, name), "wb") as handle:
        with zipfile.ZipFile(handle, "w", zipfile.ZIP_STORED) as archive:
            for entry, body in entries:
                info = zipfile.ZipInfo(entry, EPOCH)
                info.create_system = 3
                info.external_attr = 0o644 << 16
                archive.writestr(info, body)


def written(name, body, directory=None):
    with open(os.path.join(directory or CORPUS, name), "wb") as handle:
        handle.write(body if isinstance(body, bytes) else body.encode())


# ----------------------------------------------------------------------- xml

XML = '<?xml version="1.0" encoding="UTF-8"?>\n'
W = 'xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"'
R = 'xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"'
P = 'xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"'
A = 'xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"'
S = 'xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"'
ODF = (
    'xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" '
    'xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0" '
    'xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0" '
    'xmlns:draw="urn:oasis:names:tc:opendocument:xmlns:drawing:1.0"'
)

RELS_NS = "http://schemas.openxmlformats.org/package/2006/relationships"
OFFICE_DOC = f"{RELS_NS.replace('/package/', '/officeDocument/')}/officeDocument"
STYLES_REL = f"{RELS_NS.replace('/package/', '/officeDocument/')}/styles"
SHEET_REL = f"{RELS_NS.replace('/package/', '/officeDocument/')}/worksheet"
SLIDE_REL = f"{RELS_NS.replace('/package/', '/officeDocument/')}/slide"


def rels(*triples):
    body = "".join(
        f'<Relationship Id="{rid}" Type="{kind}" Target="{target}"/>'
        for rid, kind, target in triples
    )
    return f'{XML}<Relationships xmlns="{RELS_NS}">{body}</Relationships>'


# ------------------------------------------------------- WordprocessingML ---


def wp(text, style=None):
    props = f'<w:pPr><w:pStyle w:val="{style}"/></w:pPr>' if style else ""
    return f"<w:p>{props}<w:r><w:t>{text}</w:t></w:r></w:p>"


def wcell(text):
    return f"<w:tc><w:tcPr/>{wp(text)}</w:tc>"


def wrow(*cells):
    return "<w:tr>" + "".join(wcell(cell) for cell in cells) + "</w:tr>"


# The heading level is resolved through the styles part, so the fixture carries
# one: a `docx` whose heading came out as a paragraph would be a fixture that
# proved less than it looks like it proves.
STYLES = (
    f'{XML}<w:styles {W}><w:style w:type="paragraph" w:styleId="Heading1">'
    '<w:name w:val="heading 1"/></w:style></w:styles>'
)

DOCX_BODY = (
    f"{XML}<w:document {W}><w:body>"
    + wp("Reading a document", "Heading1")
    + wp("An agent that opens this file with a plain read pays for zip bytes.")
    + "<w:tbl>"
    + wrow("format", "bytes read")
    + wrow("docx", "12000")
    + wrow("markdown", "900")
    + "</w:tbl></w:body></w:document>"
)

DOCM_BODY = (
    f"{XML}<w:document {W}><w:body>"
    + wp("A macro-enabled report", "Heading1")
    + wp("The container is the one docx uses; the extension is the only difference.")
    + "</w:body></w:document>"
)


def wordml(body):
    return [
        ("_rels/.rels", rels(("rId1", OFFICE_DOC, "word/document.xml"))),
        ("word/_rels/document.xml.rels", rels(("rId1", STYLES_REL, "styles.xml"))),
        ("word/document.xml", body),
        ("word/styles.xml", STYLES),
    ]


# ---------------------------------------------------------- SpreadsheetML ---

SHEET_ROWS = [
    ("component", "reads", "tokens"),
    ("reader", "18", "9400"),
    ("writer", "3", "1200"),
]


def xlsx_sheet():
    rows = []
    for index, row in enumerate(SHEET_ROWS, start=1):
        cells = "".join(
            f'<c r="{chr(65 + column)}{index}" t="inlineStr"><is><t>{value}</t></is></c>'
            for column, value in enumerate(row)
        )
        rows.append(f'<row r="{index}">{cells}</row>')
    return f'{XML}<worksheet {S}><sheetData>{"".join(rows)}</sheetData></worksheet>'


XLSX = [
    ("_rels/.rels", rels(("rId1", OFFICE_DOC, "xl/workbook.xml"))),
    (
        "xl/workbook.xml",
        f"{XML}<workbook {S} {R}>"
        '<sheets><sheet name="Reads" sheetId="1" r:id="rId1"/></sheets></workbook>',
    ),
    ("xl/_rels/workbook.xml.rels", rels(("rId1", SHEET_REL, "worksheets/sheet1.xml"))),
    ("xl/worksheets/sheet1.xml", xlsx_sheet()),
]

# --------------------------------------------------------- PresentationML ---


def slide(title, *paragraphs):
    body = "".join(f"<a:p><a:r><a:t>{text}</a:t></a:r></a:p>" for text in paragraphs)
    return (
        f"{XML}<p:sld {P} {A}><p:cSld><p:spTree>"
        '<p:sp><p:nvSpPr><p:nvPr><p:ph type="title"/></p:nvPr></p:nvSpPr>'
        f"<p:txBody><a:bodyPr/><a:p><a:r><a:t>{title}</a:t></a:r></a:p></p:txBody></p:sp>"
        f"<p:sp><p:nvSpPr><p:nvPr/></p:nvSpPr><p:txBody><a:bodyPr/>{body}</p:txBody></p:sp>"
        "</p:spTree></p:cSld></p:sld>"
    )


PPTX = [
    ("_rels/.rels", rels(("rId1", OFFICE_DOC, "ppt/presentation.xml"))),
    (
        "ppt/presentation.xml",
        f"{XML}<p:presentation {P} {R}>"
        '<p:sldIdLst><p:sldId id="256" r:id="rId1"/></p:sldIdLst></p:presentation>',
    ),
    ("ppt/_rels/presentation.xml.rels", rels(("rId1", SLIDE_REL, "slides/slide1.xml"))),
    (
        "ppt/slides/slide1.xml",
        slide(
            "Deltas, not re-reads",
            "A read of an unchanged range is a read nobody needed.",
            "Markdown is the answer.",
        ),
    ),
]

# ---------------------------------------------------------------------- ODF


def odf(mimetype, body):
    return [
        ("mimetype", mimetype),
        (
            "content.xml",
            f"{XML}<office:document-content {ODF}>"
            f"<office:body>{body}</office:body></office:document-content>",
        ),
    ]


ODT_BODY = (
    "<office:text>"
    '<text:h text:outline-level="1">Notes on conversion</text:h>'
    "<text:p>A converted document is not the file the agent named.</text:p>"
    "<text:list><text:list-item><text:p>One seam.</text:p></text:list-item>"
    "<text:list-item><text:p>One cache.</text:p></text:list-item></text:list>"
    "</office:text>"
)

ODS_BODY = (
    "<office:spreadsheet>"
    '<table:table table:name="Reads">'
    "<table:table-row>"
    "<table:table-cell><text:p>component</text:p></table:table-cell>"
    "<table:table-cell><text:p>reads</text:p></table:table-cell>"
    "</table:table-row>"
    "<table:table-row>"
    "<table:table-cell><text:p>reader</text:p></table:table-cell>"
    "<table:table-cell><text:p>18</text:p></table:table-cell>"
    "</table:table-row>"
    "</table:table></office:spreadsheet>"
)

ODP_BODY = (
    "<office:presentation>"
    '<draw:page draw:name="page1">'
    "<draw:frame><draw:text-box><text:p>Deltas, not re-reads</text:p>"
    "</draw:text-box></draw:frame>"
    "<draw:frame><draw:text-box><text:p>A sidecar is content-addressed.</text:p>"
    "</draw:text-box></draw:frame>"
    "</draw:page></office:presentation>"
)

# One row, thirty thousand columns, from four hundred bytes of XML: the input
# cap says nothing about the output, and a row cap says nothing about a row
# that is a mile wide. This one is not part of the golden corpus -- its answer
# is 630 kB, which is the point of it -- and it lives beside the corpus rather
# than in it, for the seam's ceiling to be tested against a real document.
EXPANSION_COLUMNS = 30_000
EXPANSION_BODY = (
    "<office:spreadsheet>"
    '<table:table table:name="Wide"><table:table-row>'
    f'<table:table-cell table:number-columns-repeated="{EXPANSION_COLUMNS}">'
    "<text:p>expansion</text:p></table:table-cell>"
    "</table:table-row></table:table></office:spreadsheet>"
)

# --------------------------------------------------------------------- EPUB

EPUB = [
    ("mimetype", "application/epub+zip"),
    (
        "META-INF/container.xml",
        f"{XML}<container version=\"1.0\" "
        'xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles>'
        '<rootfile full-path="OEBPS/book.opf" '
        'media-type="application/oebps-package+xml"/></rootfiles></container>',
    ),
    (
        "OEBPS/book.opf",
        f'{XML}<package xmlns="http://www.idpf.org/2007/opf" version="3.0" '
        'unique-identifier="id"><metadata '
        'xmlns:dc="http://purl.org/dc/elements/1.1/">'
        "<dc:title>Reading deltas</dc:title></metadata>"
        '<manifest><item id="c1" href="chapter1.xhtml" '
        'media-type="application/xhtml+xml"/></manifest>'
        '<spine><itemref idref="c1"/></spine></package>',
    ),
    (
        "OEBPS/chapter1.xhtml",
        f'{XML}<html xmlns="http://www.w3.org/1999/xhtml">'
        "<head><title>Chapter</title></head><body><h1>The delta</h1>"
        "<p>An agent holds a state; only what moved is news.</p></body></html>",
    ),
]

# ---------------------------------------------------------------------- RTF


def rtf(title, line):
    return (
        "{\\rtf1\\ansi\\deff0{\\fonttbl{\\f0 Helvetica;}}\n"
        f"\\f0\\fs24 {{\\b {title}}}\\par\n{line}\\par\n}}\n"
    )


# ---------------------------------------------------------------------- CSV

# CRLF and a quoted comma, because both are what a spreadsheet exports and the
# conversion has to normalize the first and keep the second.
CSV = 'component,reads,tokens\r\nreader,18,9400\r\nwriter,3,"1,200"\r\n'

# ---------------------------------------------------------------------- PDF


def pdf(objects):
    """Numbered objects, a cross-reference table with real offsets, a trailer."""
    out = bytearray(b"%PDF-1.4\n")
    offsets = []
    for number, body in enumerate(objects, start=1):
        offsets.append(len(out))
        out += f"{number} 0 obj\n".encode() + body + b"\nendobj\n"
    start = len(out)
    out += f"xref\n0 {len(objects) + 1}\n".encode() + b"0000000000 65535 f \n"
    for offset in offsets:
        out += f"{offset:010d} 00000 n \n".encode()
    out += (
        f"trailer\n<< /Size {len(objects) + 1} /Root 1 0 R >>\n"
        f"startxref\n{start}\n%%EOF\n"
    ).encode()
    return bytes(out)


def stream(body, extra=""):
    return f"<< /Length {len(body)}{extra} >>\nstream\n".encode() + body + b"\nendstream"


def page(contents, font=True, image=False):
    resources = []
    if font:
        resources.append("/Font << /F1 5 0 R >>")
    if image:
        resources.append("/XObject << /Im0 6 0 R >>")
    return (
        f"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] "
        f"/Resources << {' '.join(resources)} >> /Contents {contents} 0 R >>"
    ).encode()


TEXT = b"".join(
    f"({line}) Tj 0 -24 Td\n".encode()
    for line in (
        "Rift converts documents to Markdown.",
        "A page of text is a page an agent can read.",
    )
)
TEXT_CONTENT = b"BT /F1 14 Tf 72 720 Td\n" + TEXT + b"ET"
IMAGE_CONTENT = b"q 200 0 0 200 72 500 cm /Im0 Do Q"
CATALOG = b"<< /Type /Catalog /Pages 2 0 R >>"
FONT = b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>"
# Four pixels of grey: an image is what makes a page one that only OCR could
# read, and the smallest honest one is still an image.
IMAGE = stream(
    bytes([0x20, 0x40, 0x60, 0x80]),
    " /Type /XObject /Subtype /Image /Width 2 /Height 2"
    " /ColorSpace /DeviceGray /BitsPerComponent 8",
)

TEXT_PDF = pdf(
    [
        CATALOG,
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        page(4),
        stream(TEXT_CONTENT),
        FONT,
    ]
)

SCANNED_PDF = pdf(
    [
        CATALOG,
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        page(4, font=False, image=True),
        stream(IMAGE_CONTENT),
        FONT,
        IMAGE,
    ]
)

# One readable page and one scanned one. The engine returns nothing at all for
# this document rather than the page it could read, on the ground that output
# missing a page reads as complete -- the golden says so.
MIXED_PDF = pdf(
    [
        CATALOG,
        b"<< /Type /Pages /Kids [3 0 R 7 0 R] /Count 2 >>",
        page(4),
        stream(TEXT_CONTENT),
        FONT,
        IMAGE,
        page(8, font=False, image=True),
        stream(IMAGE_CONTENT),
    ]
)

# ------------------------------------------------------------ OLE compound

OLE_MAGIC = bytes([0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1])


def ole_header():
    """An OLE container header whose directory chain leads nowhere."""
    out = bytearray(OLE_MAGIC)
    out += bytes(16)  # CLSID
    out += struct.pack("<HHHH", 0x003E, 3, 0xFFFE, 9)  # version, byte order, shift
    out += struct.pack("<HHIII", 6, 0, 0, 0, 0)  # mini shift, reserved, counts
    out += struct.pack("<IIIII", 0, 1, 0xFFFFFFFE, 0, 0x00001000)
    out += struct.pack("<II", 0xFFFFFFFE, 0)
    out += b"\xff\xff\xff\xff" * 109  # a DIFAT of free sectors
    return bytes(out)


# ----------------------------------------------------------------------- run

os.makedirs(CORPUS, exist_ok=True)

written("reads.csv", CSV)
written("memo.rtf", rtf("A memo about reads.", "Every document an agent opens is paid for once."))
written(
    "letter.doc",
    rtf(
        "Rich text wearing a word extension.",
        "Files like this one are common enough that the engine routes them by their bytes.",
    ),
)
written("legacy.xls", ole_header())
written("legacy.ppt", ole_header())
written("text-layer.pdf", TEXT_PDF)
written("scanned.pdf", SCANNED_PDF)
written("mixed.pdf", MIXED_PDF)
zipped("spec.docx", wordml(DOCX_BODY))
zipped("report.docm", wordml(DOCM_BODY))
zipped("reads.xlsx", XLSX)
zipped("deck.pptx", PPTX)
zipped("notes.odt", odf("application/vnd.oasis.opendocument.text", ODT_BODY))
zipped("reads.ods", odf("application/vnd.oasis.opendocument.spreadsheet", ODS_BODY))
zipped("deck.odp", odf("application/vnd.oasis.opendocument.presentation", ODP_BODY))
zipped("book.epub", EPUB)

with open(os.path.join(OUT, "expansion.ods"), "wb") as handle:
    with zipfile.ZipFile(handle, "w", zipfile.ZIP_STORED) as archive:
        for entry, body in odf(
            "application/vnd.oasis.opendocument.spreadsheet", EXPANSION_BODY
        ):
            info = zipfile.ZipInfo(entry, EPOCH)
            info.create_system = 3
            info.external_attr = 0o644 << 16
            archive.writestr(info, body)

for name in sorted(os.listdir(CORPUS)):
    if not name.endswith((".md", ".refused")):
        print(f"{os.path.getsize(os.path.join(CORPUS, name)):8d}  corpus/{name}")
print(f"{os.path.getsize(os.path.join(OUT, 'expansion.ods')):8d}  expansion.ods")
