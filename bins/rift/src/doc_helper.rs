//! `rift hooks doc` — one document, one Markdown answer, one child process.
//!
//! The fourteen document parsers are linked into this binary, and conversion
//! still never runs in the process a hook or a worker is. It runs in `rift`
//! **re-spawned** on this hidden route, the way the capture worker is already
//! reached: a one-shot child that reads bytes, prints one JSON object and
//! exits. Being a process is the point — the deadline is enforced by killing
//! it, and a zip container that decompresses to more memory than the machine
//! has kills the child and nothing else. What changed is only that there is no
//! second binary to find: the program the seam spawns is the one that is
//! already running, by its own real path, so nothing is looked for beside the
//! executable and nothing on `PATH` is ever believed.
//!
//! The route is deliberately the least interesting thing in the tree. It opens
//! no store, reads no policy, touches no repository, talks to no network,
//! keeps no cache and outlives no call. Every decision that could be made
//! somewhere else is made somewhere else, because this is the process that
//! parses hostile input.
//!
//! ```text
//! rift hooks doc convert report.docx --json
//! rift hooks doc convert - --ext pdf --table-rows 200 --json < report.pdf
//! rift hooks doc convert notes.odt          # the Markdown itself, for a human
//! ```
//!
//! **Exit codes.** `0` the document converted; `1` it did not, for a reason
//! ([`DocError`]) that is on standard output as JSON under `--json`; `2` the
//! conversion was never attempted — bad arguments, or a path that could not be
//! read. The parent reads the JSON object and not the status, so that a child
//! which dies mid-answer and one which answers badly are the same event to it;
//! the codes are here for a human and a shell script.

#[cfg(feature = "doc")]
use std::io::{Read, Write};
#[cfg(feature = "doc")]
use std::path::{Path, PathBuf};
#[cfg(feature = "doc")]
use std::time::Duration;

#[cfg(feature = "doc")]
use clap::{Parser, Subcommand};
#[cfg(feature = "doc")]
use rift_recall_doc::{
    output_ceiling, wire, AnydocConverter, ChildProcessConverter, ConvertCaps, DocConverter,
    DocError, Markdown, DEFAULT_MAX_INPUT_BYTES, DEFAULT_PDF_MIN_CHARS_PER_PAGE,
    DEFAULT_TABLE_ROWS,
};

/// The hidden route, as the arguments `rift` is re-spawned with.
#[cfg(feature = "doc")]
pub(crate) const ROUTE: [&str; 2] = ["hooks", "doc"];

/// The document standing in for standard input.
#[cfg(feature = "doc")]
const STDIN: &str = "-";

/// The converter this binary reaches conversion through: itself, re-spawned
/// on [`ROUTE`], under `budget`.
///
/// The one place that decides what `rift` spawns. `None` only when the
/// running executable cannot name itself, which the callers treat the way
/// they treat every other refusal: the read proceeds unconverted.
#[cfg(feature = "doc")]
pub(crate) fn self_exec_converter(budget: Duration) -> Option<ChildProcessConverter> {
    ChildProcessConverter::self_exec(ROUTE).map(|converter| converter.with_budget(budget))
}

#[cfg(feature = "doc")]
#[derive(Parser)]
#[command(
    name = "rift hooks doc",
    about = "Convert a document to Markdown",
    no_binary_name = true,
    disable_help_subcommand = true
)]
struct DocHelperCli {
    #[command(subcommand)]
    command: DocHelperCmd,
}

#[cfg(feature = "doc")]
#[derive(Subcommand)]
enum DocHelperCmd {
    /// Convert one document and print the result.
    Convert {
        /// The document to convert, or `-` to read it from standard input.
        path: PathBuf,
        /// The extension that decides the parser. Taken from `path` when it is
        /// a path; required with `-`, because standard input has no name.
        #[arg(long)]
        ext: Option<String>,
        /// Body rows kept per table before the rest become a count.
        #[arg(long, default_value_t = DEFAULT_TABLE_ROWS)]
        table_rows: usize,
        /// Inputs larger than this are refused without being parsed.
        #[arg(long, default_value_t = DEFAULT_MAX_INPUT_BYTES)]
        max_input_bytes: u64,
        /// Characters per page below which a PDF counts as having no text.
        #[arg(long, default_value_t = DEFAULT_PDF_MIN_CHARS_PER_PAGE)]
        pdf_min_chars_per_page: u32,
        /// Print the machine-readable object instead of the Markdown.
        #[arg(long)]
        json: bool,
    },
}

/// `rift hooks doc …`: convert one document and exit. Diverges.
///
/// Everything that fails before a conversion is attempted has no answer to
/// print and so no JSON to print it in: it is a line on standard error and
/// exit status `2`.
#[cfg(feature = "doc")]
pub(crate) fn run_doc_helper(args: &[String]) -> ! {
    let code = match doc_helper_main(args) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("rift hooks doc: {error}");
            2
        }
    };
    std::process::exit(i32::from(code))
}

/// The route in a build without the document seam: honestly not here.
#[cfg(not(feature = "doc"))]
pub(crate) fn run_doc_helper(_args: &[String]) -> ! {
    eprintln!("rift hooks doc: document conversion is not compiled into this build");
    std::process::exit(2)
}

#[cfg(feature = "doc")]
fn doc_helper_main(args: &[String]) -> Result<u8, String> {
    let DocHelperCmd::Convert {
        path,
        ext,
        table_rows,
        max_input_bytes,
        pdf_min_chars_per_page,
        json,
    } = DocHelperCli::try_parse_from(args)
        .map_err(|error| error.to_string())?
        .command;
    let caps = ConvertCaps {
        table_rows,
        max_input_bytes,
        pdf_min_chars_per_page,
    };
    let ext = doc_helper_extension(&path, ext.as_deref())?;
    let bytes = read_document_capped(&path, caps.max_input_bytes)?;
    let result = AnydocConverter.convert(&bytes, &ext, &caps);
    Ok(doc_helper_report(&result, &caps, json))
}

/// The extension deciding the parser: the flag if it was given, else the
/// path's own. Standard input has no name, so there it is the flag or nothing
/// — guessing the format of bytes is exactly what this route does not do.
#[cfg(feature = "doc")]
fn doc_helper_extension(path: &Path, ext: Option<&str>) -> Result<String, String> {
    if let Some(ext) = ext {
        return Ok(ext.to_owned());
    }
    if path == Path::new(STDIN) {
        return Err("reading a document from standard input needs `--ext <extension>`".to_owned());
    }
    path.extension()
        .and_then(std::ffi::OsStr::to_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("`{}` has no extension to convert it by", path.display()))
}

/// The document's bytes: never more than the cap, plus the one byte that
/// proves the cap was passed.
///
/// Both sources are read through the same limit, so a 4 GB file costs one
/// page of reading rather than 4 GB of memory, and the refusal is still made
/// by the converter — one place, one `too_large`, whichever source it came
/// from. Enforcing the cap on the way in is the only version of a cap that
/// protects anything.
#[cfg(feature = "doc")]
fn read_document_capped(path: &Path, cap: u64) -> Result<Vec<u8>, String> {
    let ceiling = cap.saturating_add(1);
    let mut bytes = Vec::new();
    if path == Path::new(STDIN) {
        std::io::stdin()
            .lock()
            .take(ceiling)
            .read_to_end(&mut bytes)
            .map_err(|error| format!("reading the document from standard input: {error}"))?;
    } else {
        std::fs::File::open(path)
            .and_then(|file| file.take(ceiling).read_to_end(&mut bytes))
            .map_err(|error| format!("reading `{}`: {error}", path.display()))?;
    }
    Ok(bytes)
}

/// The bytes this route will actually send, and whether they are a refusal.
///
/// The input cap does not bound the output: `docx`, `xlsx`, `pptx` and `epub`
/// are zip containers, and a few kilobytes of one decompress to as much as
/// their author chose. The parent refuses to read past the same ceiling, so
/// this is not the only guard; it is the polite half of it, which turns a
/// truncated pipe the parent cannot explain into a refusal naming the limit
/// that was hit.
#[cfg(feature = "doc")]
fn doc_helper_answer(result: &Result<Markdown, DocError>, caps: &ConvertCaps) -> (String, bool) {
    let encoded = wire::encode(result);
    if encoded.len() as u64 > output_ceiling(caps) {
        return (
            wire::encode(&Err(DocError::Engine {
                code: "resourceLimit",
            })),
            true,
        );
    }
    (encoded, result.is_err())
}

/// Print the outcome and say, in the exit status, which one it was.
#[cfg(feature = "doc")]
fn doc_helper_report(result: &Result<Markdown, DocError>, caps: &ConvertCaps, json: bool) -> u8 {
    if json {
        let (encoded, failed) = doc_helper_answer(result, caps);
        let mut out = std::io::stdout().lock();
        let _ = out.write_all(encoded.as_bytes());
        let _ = out.write_all(b"\n");
        let _ = out.flush();
        return u8::from(failed);
    }
    // A human at a terminal asked for the Markdown and asked for all of it.
    match result {
        Ok(markdown) => print!("{}", markdown.text),
        Err(error) => eprintln!("rift hooks doc: {error}"),
    }
    u8::from(result.is_err())
}

#[cfg(all(test, feature = "doc"))]
mod tests {
    use super::*;

    fn scratch_dir() -> PathBuf {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/rift-doc-helper-tests");
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_extension_comes_from_the_flag_then_the_path() {
        assert_eq!(
            doc_helper_extension(Path::new("a/b/spec.DOCX"), None).unwrap(),
            "DOCX"
        );
        assert_eq!(
            doc_helper_extension(Path::new("a/b/spec.docx"), Some("pdf")).unwrap(),
            "pdf"
        );
        assert_eq!(
            doc_helper_extension(Path::new("-"), Some("csv")).unwrap(),
            "csv"
        );
        assert!(doc_helper_extension(Path::new("-"), None).is_err());
        assert!(doc_helper_extension(Path::new("Makefile"), None).is_err());
    }

    /// An oversized file is never read: the converter is handed exactly enough
    /// to refuse, which keeps the refusal in one place instead of two.
    #[test]
    fn a_file_over_the_cap_is_refused_without_being_read() {
        let path = scratch_dir().join("big.csv");
        std::fs::write(&path, vec![b'a'; 512]).unwrap();
        let bytes = read_document_capped(&path, 8).unwrap();
        assert_eq!(bytes.len(), 9);
        assert!(matches!(
            AnydocConverter.convert(
                &bytes,
                "csv",
                &ConvertCaps {
                    max_input_bytes: 8,
                    ..ConvertCaps::default()
                }
            ),
            Err(DocError::TooLarge { bytes: 9, cap: 8 })
        ));
        assert_eq!(read_document_capped(&path, 4096).unwrap().len(), 512);
        assert!(read_document_capped(&scratch_dir().join("absent.csv"), 4096).is_err());
    }

    /// The arguments after the route are the ones the seam spawns, unchanged
    /// from the helper binary they used to reach.
    #[test]
    fn the_command_line_is_the_one_the_seam_spawns() {
        let cli = DocHelperCli::try_parse_from([
            "convert",
            "-",
            "--ext",
            "csv",
            "--table-rows",
            "7",
            "--max-input-bytes",
            "4096",
            "--pdf-min-chars-per-page",
            "11",
            "--json",
        ])
        .unwrap();
        let DocHelperCmd::Convert {
            path,
            ext,
            table_rows,
            max_input_bytes,
            pdf_min_chars_per_page,
            json,
        } = cli.command;
        assert_eq!(path, PathBuf::from("-"));
        assert_eq!(ext.as_deref(), Some("csv"));
        assert_eq!(table_rows, 7);
        assert_eq!(max_input_bytes, 4096);
        assert_eq!(pdf_min_chars_per_page, 11);
        assert!(json);
    }

    #[test]
    fn the_caps_default_to_the_seams_own() {
        let cli = DocHelperCli::try_parse_from(["convert", "a.csv"]).unwrap();
        let DocHelperCmd::Convert {
            table_rows,
            max_input_bytes,
            pdf_min_chars_per_page,
            json,
            ..
        } = cli.command;
        assert_eq!(table_rows, DEFAULT_TABLE_ROWS);
        assert_eq!(max_input_bytes, DEFAULT_MAX_INPUT_BYTES);
        assert_eq!(pdf_min_chars_per_page, DEFAULT_PDF_MIN_CHARS_PER_PAGE);
        assert!(!json);
    }

    /// A conversion larger than the parent will read is not sent: a truncated
    /// pipe the parent cannot explain becomes a refusal that names its limit.
    #[test]
    fn an_answer_past_the_output_ceiling_becomes_a_refusal() {
        let caps = ConvertCaps {
            max_input_bytes: 32,
            ..ConvertCaps::default()
        };
        let big: Result<Markdown, DocError> = Ok(Markdown {
            text: "a".repeat(usize::try_from(output_ceiling(&caps)).unwrap()),
            ..Markdown::default()
        });
        let (encoded, failed) = doc_helper_answer(&big, &caps);
        assert!(failed);
        assert_eq!(
            encoded,
            wire::encode(&Err(DocError::Engine {
                code: "resourceLimit"
            }))
        );
        // A ceiling that fits the answer changes nothing about the answer.
        let roomy = ConvertCaps {
            max_input_bytes: 1024 * 1024,
            ..ConvertCaps::default()
        };
        let (encoded, failed) = doc_helper_answer(&big, &roomy);
        assert!(!failed);
        assert_eq!(encoded, wire::encode(&big));
    }

    #[test]
    fn a_refusal_exits_nonzero_and_a_conversion_does_not() {
        let caps = ConvertCaps::default();
        let refused: Result<Markdown, DocError> = Err(DocError::Engine { code: "encrypted" });
        assert_eq!(doc_helper_report(&refused, &caps, true), 1);
        assert_eq!(doc_helper_report(&refused, &caps, false), 1);
        assert_eq!(doc_helper_report(&Ok(Markdown::default()), &caps, true), 0);
        assert_eq!(doc_helper_report(&Ok(Markdown::default()), &caps, false), 0);
    }

    /// The child never spawns a child: the route's half of this file — from
    /// the entry point to the tests — constructs no converter that spawns and
    /// starts no process. A self-spawn that re-spawned itself would be a fork
    /// bomb with a deadline, and structure is the only guard worth having.
    #[test]
    fn the_route_never_spawns_anything() {
        let source = include_str!("doc_helper.rs");
        let start = source
            .find("pub(crate) fn run_doc_helper")
            .expect("the entry point");
        let end = source.find("#[cfg(all(test").expect("the tests");
        let route = &source[start..end];
        for forbidden in [
            "ChildProcessConverter",
            "self_exec",
            "Command::new",
            "spawn(",
        ] {
            assert!(
                !route.contains(forbidden),
                "the route spells `{forbidden}`, which is how a child would spawn a child"
            );
        }
    }
}
