//! The line between two processes: one JSON object, one conversion.
//!
//! The fourteen parsers never run in the process a hook is, so conversion
//! happens in a child process ([`crate::ChildProcessConverter`] on one side,
//! `rift` re-spawned on its `hooks doc` route on the other). This module is
//! the format they agree on, written once so the two sides cannot drift: the
//! child calls [`encode`], the parent calls `decode`, and a round-trip test
//! binds them.
//!
//! One object, one line, on standard output:
//!
//! ```text
//! {"status":"ok","text":"# Title\n","pages":null,"thin_pages":[],"sheets":[]}
//! {"status":"refused","code":"engine","engine_code":"encrypted"}
//! ```
//!
//! **The parent trusts the shape and nothing else.** A child process is a
//! parser of untrusted documents, and a parser of untrusted documents is the
//! thing most likely to be made to say something surprising. So `decode` reads
//! only what it cannot be hurt by: a status word from a closed set, an engine
//! code matched against [`ENGINE_CODES`] and replaced by a fixed
//! `&'static str`, and — for a refusal — nothing else at all. The `bytes`,
//! `cap` and `ext` fields a refusal carries are written for whoever reads the
//! JSON with their eyes; the parent rebuilds those from the arguments it
//! itself passed, so no string the child chose ever reaches a [`DocError`],
//! a log line or an agent's context.

use serde_json::Value;

use crate::{ConvertCaps, DocError, Markdown, SheetInfo};

/// A refusal because the input was over the byte cap.
const TOO_LARGE: &str = "too_large";
/// A refusal because the extension is not one Rift converts.
const UNSUPPORTED_EXTENSION: &str = "unsupported_extension";
/// A refusal the engine itself made.
const ENGINE: &str = "engine";

/// Every engine classification the parent will repeat back.
///
/// A code outside this set becomes the generic `engine`: the value arrives
/// from a process that has just parsed a hostile file, and a
/// [`DocError::Engine`] carries a `&'static str` precisely so that no such
/// value can be allocated from it.
pub const ENGINE_CODES: [&str; 8] = [
    "encrypted",
    "io",
    "malformed",
    "missingPart",
    "needsOcr",
    "panic",
    "resourceLimit",
    "unsupported",
];

/// The one JSON object a conversion attempt produces.
///
/// Success and refusal are both results, not exceptions: the helper prints one
/// of these either way, and its exit status is a convenience for a human, not
/// the channel the parent reads.
#[must_use]
pub fn encode(result: &Result<Markdown, DocError>) -> String {
    let value = match result {
        Ok(markdown) => serde_json::json!({
            "status": "ok",
            "text": markdown.text,
            "pages": markdown.pages,
            "thin_pages": markdown.thin_pages,
            "sheets": markdown
                .sheets
                .iter()
                .map(|sheet| serde_json::json!({
                    "name": sheet.name,
                    "rows": sheet.rows,
                    "rows_kept": sheet.rows_kept,
                }))
                .collect::<Vec<Value>>(),
        }),
        Err(DocError::TooLarge { bytes, cap }) => serde_json::json!({
            "status": "refused",
            "code": TOO_LARGE,
            "bytes": bytes,
            "cap": cap,
        }),
        Err(DocError::UnsupportedExtension(ext)) => serde_json::json!({
            "status": "refused",
            "code": UNSUPPORTED_EXTENSION,
            "ext": ext,
        }),
        Err(DocError::Engine { code }) => serde_json::json!({
            "status": "refused",
            "code": ENGINE,
            "engine_code": code,
        }),
    };
    value.to_string()
}

/// Read one object back, or decide it is not one.
///
/// `ext`, `bytes` and `caps` are the parent's own arguments to the conversion,
/// used to rebuild a refusal rather than believing the child's version of them
/// (see the module docs). `None` means the output is not a well-formed
/// envelope at all — a crash, a panic message, a truncated pipe, a different
/// program under the same name — and the caller turns that into the one
/// conservative outcome it has: leave the read alone.
pub(crate) fn decode(
    out: &[u8],
    ext: &str,
    bytes: u64,
    caps: &ConvertCaps,
) -> Option<Result<Markdown, DocError>> {
    let value: Value = serde_json::from_slice(out).ok()?;
    let object = value.as_object()?;
    match object.get("status")?.as_str()? {
        "ok" => Some(Ok(Markdown {
            text: object.get("text")?.as_str()?.to_owned(),
            pages: match object.get("pages") {
                None | Some(Value::Null) => None,
                Some(pages) => Some(as_u32(pages)?),
            },
            thin_pages: u32_array(object.get("thin_pages"))?,
            sheets: sheets(object.get("sheets"))?,
        })),
        "refused" => Some(Err(match object.get("code")?.as_str()? {
            TOO_LARGE => DocError::TooLarge {
                bytes,
                cap: caps.max_input_bytes,
            },
            UNSUPPORTED_EXTENSION => DocError::UnsupportedExtension(ext.to_owned()),
            ENGINE => DocError::Engine {
                code: engine_code(object.get("engine_code").and_then(Value::as_str)),
            },
            _ => return None,
        })),
        _ => None,
    }
}

/// The engine's own classification, or `engine` for anything unrecognized.
fn engine_code(code: Option<&str>) -> &'static str {
    code.and_then(|code| ENGINE_CODES.iter().copied().find(|known| *known == code))
        .unwrap_or(ENGINE)
}

/// A JSON number that is a page number.
fn as_u32(value: &Value) -> Option<u32> {
    u32::try_from(value.as_u64()?).ok()
}

/// A JSON array of page numbers; absent reads as empty.
fn u32_array(value: Option<&Value>) -> Option<Vec<u32>> {
    match value {
        None | Some(Value::Null) => Some(Vec::new()),
        Some(value) => value.as_array()?.iter().map(as_u32).collect(),
    }
}

/// A JSON array of table summaries; absent reads as empty.
fn sheets(value: Option<&Value>) -> Option<Vec<SheetInfo>> {
    let Some(value) = value else {
        return Some(Vec::new());
    };
    if value.is_null() {
        return Some(Vec::new());
    }
    value
        .as_array()?
        .iter()
        .map(|sheet| {
            let sheet = sheet.as_object()?;
            Some(SheetInfo {
                name: sheet.get("name")?.as_str()?.to_owned(),
                rows: as_u32(sheet.get("rows")?)?,
                rows_kept: as_u32(sheet.get("rows_kept")?)?,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caps() -> ConvertCaps {
        ConvertCaps {
            max_input_bytes: 10,
            ..ConvertCaps::default()
        }
    }

    fn round_trip(result: &Result<Markdown, DocError>) -> Option<Result<Markdown, DocError>> {
        decode(encode(result).as_bytes(), "docx", 20, &caps())
    }

    /// The two processes have to mean the same thing by the same object, and
    /// the only way to keep that true as either side changes is to check it.
    #[test]
    fn every_conversion_outcome_survives_the_round_trip() {
        let markdown = Markdown {
            text: "# Title\n\n| a | b |\n".to_owned(),
            pages: Some(7),
            thin_pages: vec![2, 3],
            sheets: vec![SheetInfo {
                name: "Costs".to_owned(),
                rows: 900,
                rows_kept: 200,
            }],
        };
        for result in [
            Ok(markdown),
            Ok(Markdown::default()),
            Err(DocError::TooLarge { bytes: 20, cap: 10 }),
            Err(DocError::UnsupportedExtension("docx".to_owned())),
            Err(DocError::Engine { code: "encrypted" }),
        ] {
            assert_eq!(round_trip(&result), Some(result.clone()), "{result:?}");
        }
    }

    /// A refusal's numbers and its extension are the parent's, never the
    /// child's: the fields are in the object for a human to read, and reading
    /// them back would let a hostile document choose what an error says.
    #[test]
    fn a_refusal_is_rebuilt_from_the_parents_own_arguments() {
        let envelope = br#"{"status":"refused","code":"too_large","bytes":1,"cap":2}"#;
        assert_eq!(
            decode(envelope, "pdf", 4096, &caps()),
            Some(Err(DocError::TooLarge {
                bytes: 4096,
                cap: 10
            }))
        );
        let envelope = br#"{"status":"refused","code":"unsupported_extension","ext":"../../etc"}"#;
        assert_eq!(
            decode(envelope, "pdf", 1, &caps()),
            Some(Err(DocError::UnsupportedExtension("pdf".to_owned())))
        );
    }

    /// An engine code the parent does not know is a code it will not repeat.
    #[test]
    fn an_unknown_engine_code_is_flattened_and_never_allocated() {
        let envelope = br#"{"status":"refused","code":"engine","engine_code":"<script>alert(1)"}"#;
        assert_eq!(
            decode(envelope, "pdf", 1, &caps()),
            Some(Err(DocError::Engine { code: "engine" }))
        );
        let envelope = br#"{"status":"refused","code":"engine"}"#;
        assert_eq!(
            decode(envelope, "pdf", 1, &caps()),
            Some(Err(DocError::Engine { code: "engine" }))
        );
        for code in ENGINE_CODES {
            let envelope =
                format!(r#"{{"status":"refused","code":"engine","engine_code":"{code}"}}"#);
            assert_eq!(
                decode(envelope.as_bytes(), "pdf", 1, &caps()),
                Some(Err(DocError::Engine { code }))
            );
        }
    }

    /// Anything that is not an envelope is not half an envelope: the caller
    /// gets one answer, "this did not work", and leaves the read alone.
    #[test]
    fn output_that_is_not_an_envelope_decodes_to_nothing() {
        for out in [
            b"".as_slice(),
            b"thread 'main' panicked at src/main.rs:1:1",
            br#"{"status":"ok"}"#,
            br#"{"status":"ok","text":42}"#,
            br#"{"status":"ok","text":"x","thin_pages":[-1]}"#,
            br#"{"status":"ok","text":"x","sheets":[{"name":"a"}]}"#,
            br#"{"status":"refused","code":"who_knows"}"#,
            br#"{"status":"shrug"}"#,
            br#"["ok"]"#,
            br#"{"status":"ok","text":"x"}extra"#,
        ] {
            assert_eq!(
                decode(out, "pdf", 1, &caps()),
                None,
                "{}",
                String::from_utf8_lossy(out)
            );
        }
    }

    /// The optional arrays are optional in both directions.
    #[test]
    fn a_minimal_success_envelope_is_an_empty_conversion() {
        assert_eq!(
            decode(br#"{"status":"ok","text":""}"#, "pdf", 1, &caps()),
            Some(Ok(Markdown::default()))
        );
    }
}
