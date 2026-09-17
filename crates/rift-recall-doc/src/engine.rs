//! The engine behind the seam: `anydoc`, held to the seam's promises.
//!
//! `anydoc` is pure Rust and reaches no network — the hosted OCR path its
//! project publishes exists only in the Node and Python bindings, not in the
//! crate — which is why it can be linked into a binary that runs on a hook
//! path at all. What it does not do, this module does: refuse oversized input
//! before parsing it, keep to the fourteen extensions Rift claims, cap the
//! tables, and turn a parser that unwinds into a refusal like any other.

use std::panic::catch_unwind;

use anydoc::{ConvertError, Format};

use crate::{cap_tables, is_supported_extension, ConvertCaps, DocConverter, DocError, Markdown};

/// Conversion in this process, by the linked engine.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct AnydocConverter;

impl AnydocConverter {
    /// The engine's version, as pinned in the workspace manifest.
    ///
    /// This is not decoration: a sidecar's cache key is the source hash joined
    /// with this string and the policy's, so upgrading the engine retires every
    /// conversion it made instead of serving yesterday's Markdown for tomorrow's
    /// parser. A test binds it to the pin, since a constant that drifted from
    /// the version actually linked would do the opposite of its job.
    pub const ENGINE_VERSION: &'static str = crate::ENGINE_VERSION;
}

impl DocConverter for AnydocConverter {
    fn convert(&self, bytes: &[u8], ext: &str, caps: &ConvertCaps) -> Result<Markdown, DocError> {
        let size = bytes.len() as u64;
        if size > caps.max_input_bytes {
            return Err(DocError::TooLarge {
                bytes: size,
                cap: caps.max_input_bytes,
            });
        }
        let ext = ext.strip_prefix('.').unwrap_or(ext).to_ascii_lowercase();
        if !is_supported_extension(&ext) {
            return Err(DocError::UnsupportedExtension(ext));
        }
        // Unreachable for the fourteen, and a refusal rather than an assertion
        // if that ever stops being true.
        let Some(format) = Format::from_extension(&ext) else {
            return Err(DocError::UnsupportedExtension(ext));
        };

        // A parser that unwinds is a defect, and a defect on a hook path must
        // still leave the developer's read alone. Catching it here means every
        // caller has one thing to handle instead of two.
        let converted = catch_unwind(|| anydoc::to_markdown_bytes(bytes, format))
            .map_err(|_| DocError::Engine { code: "panic" })?;

        match converted {
            Ok(text) => {
                let (text, sheets) = cap_tables(&text, caps.table_rows);
                Ok(Markdown {
                    text,
                    pages: None,
                    thin_pages: Vec::new(),
                    sheets,
                })
            }
            // A scanned PDF is not a failed conversion, it is a document with
            // no text to convert. Reporting it as thin — with the pages named —
            // is what lets a caller leave the read alone and let the agent see
            // the pages as images, which is the only way to see them at all.
            Err(ConvertError::NeedsOcr { pages, page_count }) => Ok(Markdown {
                text: no_text_layer(&pages, page_count),
                pages: Some(page_count),
                thin_pages: pages,
                sheets: Vec::new(),
            }),
            Err(other) => Err(DocError::Engine { code: other.code() }),
        }
    }
}

/// What a sidecar says instead of nothing when the pages carry no text.
fn no_text_layer(pages: &[u32], page_count: u32) -> String {
    format!(
        "*No text layer: page(s) {} of {page_count} are scanned or image-only, so this document \
         carries no text to convert.*\n",
        page_ranges(pages)
    )
}

/// `2, 5-7, 12` from ascending page numbers.
fn page_ranges(pages: &[u32]) -> String {
    let mut ranges = Vec::new();
    let mut pages = pages.iter().copied().peekable();
    while let Some(start) = pages.next() {
        let mut end = start;
        while pages.next_if_eq(&(end + 1)).is_some() {
            end += 1;
        }
        ranges.push(if end > start {
            format!("{start}-{end}")
        } else {
            start.to_string()
        });
    }
    ranges.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    const CSV: &[u8] = b"name,cost\nalpha,1\nbeta,2\ngamma,3\n";

    #[test]
    fn a_csv_becomes_a_markdown_table() {
        let markdown = AnydocConverter
            .convert(CSV, "csv", &ConvertCaps::default())
            .unwrap();
        assert!(markdown.text.contains("| name | cost |"), "{markdown:?}");
        assert!(markdown.text.contains("| gamma | 3 |"), "{markdown:?}");
        assert_eq!(markdown.pages, None);
        assert!(markdown.thin_pages.is_empty());
        assert_eq!(markdown.sheets.len(), 1);
        assert_eq!(markdown.sheets[0].rows, 3);
    }

    /// The extension is taken with or without its dot and in any case, because
    /// it arrives from a path and paths carry every spelling.
    #[test]
    fn the_extension_is_read_the_way_a_path_spells_it() {
        let caps = ConvertCaps::default();
        for ext in ["csv", ".csv", "CSV", ".CsV"] {
            assert!(AnydocConverter.convert(CSV, ext, &caps).is_ok(), "{ext}");
        }
        assert_eq!(
            AnydocConverter.convert(CSV, ".md", &caps),
            Err(DocError::UnsupportedExtension("md".to_owned()))
        );
    }

    /// The size cap is checked before the parser sees a byte: that is the whole
    /// point of a cap on untrusted input.
    #[test]
    fn an_input_over_the_cap_is_refused_unparsed() {
        let caps = ConvertCaps {
            max_input_bytes: 8,
            ..ConvertCaps::default()
        };
        assert_eq!(
            AnydocConverter.convert(CSV, "csv", &caps),
            Err(DocError::TooLarge {
                bytes: CSV.len() as u64,
                cap: 8
            })
        );
    }

    #[test]
    fn the_row_cap_reaches_the_engine_output() {
        let caps = ConvertCaps {
            table_rows: 1,
            ..ConvertCaps::default()
        };
        let markdown = AnydocConverter.convert(CSV, "csv", &caps).unwrap();
        assert!(markdown.text.contains("(2 more rows)"), "{markdown:?}");
        assert_eq!(markdown.sheets[0].rows_kept, 1);
    }

    /// Bytes that are not the format their name claims are a refusal, not a
    /// guess: the caller leaves the read alone and the agent sees the file.
    #[test]
    fn bytes_that_are_not_the_named_format_are_refused() {
        let error = AnydocConverter
            .convert(b"not a document", "docx", &ConvertCaps::default())
            .unwrap_err();
        assert!(matches!(error, DocError::Engine { .. }), "{error:?}");
    }

    /// The version in the sidecar key has to be the version that is linked.
    #[test]
    fn page_ranges_collapse_runs() {
        assert_eq!(page_ranges(&[2, 5, 6, 7, 12]), "2, 5-7, 12");
        assert_eq!(page_ranges(&[1]), "1");
        assert_eq!(page_ranges(&[]), "");
    }
}
