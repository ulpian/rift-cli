//! The identity of an answer.
//!
//! Its own module because a key is computed in builds that can neither convert
//! a document nor open the store: it is `sha256(source bytes) ‖ engine version
//! ‖ policy version` and nothing else, so the three inputs and the shape they
//! are allowed to take are stated here on their own.

use rift_recall_policy::{DocPolicy, SERVE_POLICY_VERSION};

#[cfg(feature = "doc")]
use super::store::SIDECAR_EXT;

/// The document converter this store's keys are the answers of.
///
/// It is stated here rather than read from the converter because the crate
/// that owns this number is optional in this binary (`--no-default-features`
/// drops the seam and the engine together) while a sidecar key must be
/// computable in every build. A test binds this to the workspace's pin, so
/// the two cannot drift apart silently: an engine upgrade that left this
/// behind would serve every existing sidecar as though the new engine had
/// produced it; where the seam is linked the test holds the two equal too.
pub(crate) const ENGINE_VERSION: &str = "0.2.4";

/// The document extensions this binary treats as documents.
///
/// Spelled here for the same reason [`ENGINE_VERSION`] is: the crate that owns
/// the list is optional in this binary (`--no-default-features` drops the
/// seam), while the verb that asks "is this a document?" must answer in every
/// build. A test holds the two lists equal wherever the seam is linked, so
/// they cannot drift in silence.
pub(crate) const DOCUMENT_EXTENSIONS: [&str; 14] = [
    "csv", "doc", "docm", "docx", "epub", "odp", "ods", "odt", "pdf", "ppt", "pptx", "rtf", "xls",
    "xlsx",
];

/// Whether `ext` names a document Rift converts. A leading `.` is accepted and
/// the comparison is case-insensitive: callers get their extension from a path,
/// and paths carry both spellings.
pub(crate) fn is_document_extension(ext: &str) -> bool {
    let ext = ext.strip_prefix('.').unwrap_or(ext);
    DOCUMENT_EXTENSIONS
        .iter()
        .any(|known| ext.eq_ignore_ascii_case(known))
}

/// The sidecar key for bytes whose SHA-256 is `content_sha256`, under `doc`.
///
/// `None` for anything that is not a 64-character lowercase hex hash: the key
/// becomes a file name, and a name is never built out of a string this program
/// has not checked.
///
/// The three inputs are the three things that decide what the Markdown says —
/// the document, the converter, and the caps the conversion ran under —
/// separated by a byte that occurs in none of them, so no two of them can
/// spell a third.
pub(crate) fn sidecar_key(content_sha256: &str, doc: &DocPolicy) -> Option<String> {
    if !is_sha256(content_sha256) {
        return None;
    }
    Some(rift_shadow::sha256_hex(&[
        content_sha256.as_bytes(),
        b"\0",
        ENGINE_VERSION.as_bytes(),
        b"\0",
        policy_version(doc).as_bytes(),
    ]))
}

/// The sidecar key a path names, when the path is this member's view of one.
///
/// The one question a read's *result* has to answer: did the tool run with the
/// path Rift substituted, or with the document the agent wrote? A view is a
/// key and an extension directly under this member's document state, so a name
/// with a separator left in it — a window's own markers live one level down —
/// is not one, and neither is anything whose stem is not a key this module
/// would have written.
///
/// Asked by the gate that made the substitution, and by nothing else.
#[cfg(feature = "doc")]
pub(crate) fn member_sidecar_key(rel_path: &str) -> Option<&str> {
    // The path is spelled here the way every other body that builds this
    // directory spells it, rather than shared through a constant: the
    // repository's own guard on family-keyed caches finds these sites by
    // reading the literal, and a constant would hide them from it.
    let name = rel_path
        .strip_prefix(".rift/state/doc")
        .and_then(|rest| rest.strip_prefix('/'))?;
    let (key, ext) = name.rsplit_once('.')?;
    (ext == SIDECAR_EXT && !key.contains('/') && is_sha256(key)).then_some(key)
}

/// The policy half of a key: the semantics version, plus every cap that
/// changes the bytes of a conversion.
///
/// Deliberately not the whole `[doc]` table. The mode, the channel, the
/// deadline and the TTL decide whether and when a document is converted and
/// how long its answer is kept — none of them changes what the answer says —
/// so including them would throw away every sidecar in the family the first
/// time somebody moved the repository from `observe` to `enforce`.
fn policy_version(doc: &DocPolicy) -> String {
    format!(
        "{SERVE_POLICY_VERSION}:{}:{}:{}:{}",
        doc.formats.as_str(),
        doc.table_rows,
        doc.max_input_bytes,
        doc.pdf_min_chars_per_page,
    )
}

/// Whether `s` is 64 lowercase hex characters — the one shape that becomes a
/// path component here.
pub(super) fn is_sha256(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::doc_store::tests::{SHA_A, SHA_B};

    /// The key is the identity of the ANSWER: the document, the engine that
    /// read it, and the caps it read it under. With two of the three fixed, a
    /// change in the third is a different sidecar — which is what makes a
    /// stale one unreachable rather than wrong.
    #[test]
    fn a_key_changes_with_the_document_the_engine_or_a_cap_that_changes_the_bytes() {
        let doc = DocPolicy::default();
        let base = sidecar_key(SHA_A, &doc).unwrap();
        assert!(is_sha256(&base), "the key is a path-safe name: {base}");
        assert_ne!(base, sidecar_key(SHA_B, &doc).unwrap(), "another document");

        let capped = DocPolicy {
            table_rows: 20,
            ..doc
        };
        assert_ne!(base, sidecar_key(SHA_A, &capped).unwrap(), "another cap");

        // The levers that decide WHETHER to convert, not WHAT a conversion
        // says, leave every sidecar in the family reachable.
        let observing = DocPolicy {
            mode: rift_recall_policy::Mode::Observe,
            channel: rift_recall_policy::DocChannel::Redirect,
            convert_budget_ms: 1,
            ttl_days: 1,
            ..doc
        };
        assert_eq!(base, sidecar_key(SHA_A, &observing).unwrap());
    }

    /// A name is never built out of a string this program has not checked.
    #[test]
    fn a_hash_that_is_not_one_is_not_a_key() {
        let doc = DocPolicy::default();
        for candidate in ["", "../evil", &SHA_A.to_uppercase(), &SHA_A[..63]] {
            assert!(
                sidecar_key(candidate, &doc).is_none(),
                "`{candidate}` must not become a file name"
            );
        }
    }

    /// What counts as a document is asked in every build, and answered from a
    /// list this binary keeps itself — the crate that owns it is optional here.
    /// Where the seam IS linked, the two lists are held equal.
    #[test]
    fn the_document_extensions_are_the_ones_the_converter_knows() {
        #[cfg(feature = "doc")]
        assert_eq!(
            DOCUMENT_EXTENSIONS.to_vec(),
            rift_recall_doc::SUPPORTED_EXTENSIONS.to_vec(),
            "the binary's own spelling of the document list drifted from the \
             crate that converts them"
        );
        assert!(is_document_extension(".DOCX") && is_document_extension("pdf"));
        assert!(!is_document_extension("rs") && !is_document_extension("md"));
    }

    /// The engine that produced a sidecar is half of its key, and a build
    /// without the seam cannot ask the engine. So the constant is bound to
    /// the workspace's pin: an upgrade that left this behind would hand every
    /// agent the old engine's answers under the new engine's name.
    #[test]
    fn the_engine_version_matches_the_workspace_pin() {
        #[cfg(feature = "doc")]
        assert_eq!(
            ENGINE_VERSION,
            rift_recall_doc::ENGINE_VERSION,
            "this binary's spelling of the engine version must equal the crate's"
        );
        let manifest =
            std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.toml"))
                .unwrap();
        let pin = format!("anydoc = \"={ENGINE_VERSION}\"");
        assert!(
            manifest.contains(&pin),
            "the workspace pins a different anydoc than ENGINE_VERSION names ({pin})"
        );
    }
}
