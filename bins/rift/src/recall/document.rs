//! The document verb's half of a request, resolved out of the caller's own
//! file and the family's sidecar store.
//!
//! A separate module because nothing on this path converts anything: every
//! refusal it spells is decided from the file the caller named, before the
//! serving surface is reached at all.

use std::path::Path;

use serde_json::json;

use crate::cli::{exit, Outcome};

use super::verb::repo_relative;

/// Resolve the document half of a `doc-read` call: the bytes'
/// identity, the declared cost of the read this verb replaces, and the sidecar
/// the family holds for it — or the refusal that stops the call.
///
/// **Nothing is converted here.** Conversion costs a process and a parse, and
/// the asking path is the one place that must not pay for it: a document the
/// family has not converted yet is answered `pending`, the work is journaled
/// for the detached worker, and the next call is answered from the store. That
/// is the same fallback the hook takes when a conversion misses its deadline,
/// and it is why this verb is safe to call on anything.
///
/// Every refusal below is a **usage** refusal, decided from the caller's own
/// file: it is not a document Rift converts, it is not there, it is over the
/// size the family converts, or documents are switched off for this
/// repository. None of them is a decision of the serving surface, so none of
/// them books a serve.
pub(super) fn resolve_document(
    tool: rift_recall_serve::ToolName,
    req: rift_recall_serve::ToolRequest,
    repo: &Path,
    cfg: &rift_config::EffectiveConfig,
) -> std::result::Result<rift_recall_serve::ToolRequest, Outcome> {
    use rift_recall_serve::{Document, ToolName};

    if tool != ToolName::DocRead {
        return Ok(req);
    }
    let named = req.target.clone().unwrap_or_default();
    let usage = |tag: &str, human: String| {
        Outcome::coded(
            exit::USAGE,
            format!("doc-read: refused ({tag}) — {human}"),
            json!({ "verb": "doc-read", "error": tag, "detail": { "path": named } }),
        )
    };
    // Through the same lexical rule the serving policy normalizes with, so a
    // path dressed up with `..` is refused rather than joined to anything.
    let Some(rel) = rift_recall_policy::normalize_rel(&repo_relative(repo, &named)) else {
        return Err(usage(
            "path_outside_repository",
            "name a file inside this repository".to_owned(),
        ));
    };
    let ext = Path::new(&rel)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_owned();
    if !crate::doc_store::is_document_extension(&ext) {
        return Err(usage(
            "not_a_document",
            format!(
                "Rift converts {}; read anything else directly",
                crate::doc_store::DOCUMENT_EXTENSIONS.join(", ")
            ),
        ));
    }
    let policy = rift_recall_policy::ServePolicy::load(&repo.join(".rift"));
    if policy.doc.mode == rift_recall_policy::Mode::Off {
        return Err(Outcome::coded(
            exit::PRECONDITION,
            "doc-read: documents are switched off for this repository ([doc] mode)".to_owned(),
            json!({ "verb": "doc-read", "error": "doc_off" }),
        ));
    }
    let bytes = match document_bytes_within_the_conversion_cap(&repo.join(&rel), &policy.doc) {
        Ok(bytes) => bytes,
        Err((tag, human)) => return Err(usage(tag, human)),
    };
    let source_sha256 = rift_shadow::sha256_hex(&[&bytes]);
    let declared_baseline = rift_recall_serve::declared_document_baseline(&ext, &bytes);
    let sidecar = sidecar_held_for_this_document(repo, &rel, &source_sha256, &policy, cfg);
    Ok(rift_recall_serve::ToolRequest {
        target: Some(rel),
        document: Some(Document {
            source_sha256,
            declared_baseline,
            sidecar,
        }),
        ..req
    })
}

/// The document's bytes, or the neutral tag and the sentence that refuse them.
///
/// The size is read before the bytes are: a document over the cap is never
/// converted, so a caller asking for one is told now rather than left waiting
/// for a conversion that will not happen. The tag and its sentence are handed
/// back rather than rendered, so every refusal of this verb is spelled in one
/// place.
fn document_bytes_within_the_conversion_cap(
    path: &Path,
    policy: &rift_recall_policy::DocPolicy,
) -> std::result::Result<Vec<u8>, (&'static str, String)> {
    let Ok(meta) = std::fs::metadata(path) else {
        return Err(("no_such_document", "no file at that path".to_owned()));
    };
    if meta.len() > policy.max_input_bytes {
        return Err((
            "document_over_the_conversion_cap",
            format!(
                "{} bytes; the cap is {}",
                meta.len(),
                policy.max_input_bytes
            ),
        ));
    }
    let Ok(bytes) = std::fs::read(path) else {
        return Err(("no_such_document", "the file cannot be read".to_owned()));
    };
    Ok(bytes)
}

/// What the family holds for this document, as this member may see it.
///
/// THE GATE THAT MATTERS IS THE DISPATCHER'S, on the path, before it looks at
/// any of this. The same question is asked one step earlier for one reason: a
/// document this repository denies or calls sensitive should not have its
/// Markdown read into this process at all. What that leaves in the request is
/// the default `Owed`, which the surface never reaches — it refuses on the
/// path first — so it is not a claim about the family's store.
fn sidecar_held_for_this_document(
    repo: &Path,
    rel: &str,
    source_sha256: &str,
    policy: &rift_recall_policy::ServePolicy,
    cfg: &rift_config::EffectiveConfig,
) -> rift_recall_serve::SidecarState {
    use rift_recall_serve::SidecarState;

    let withheld = cfg.is_sensitive_path(rel)
        || policy
            .denied_paths
            .iter()
            .any(|glob| rift_config::glob_match(glob, rel));
    match (withheld, crate::family::try_at(repo)) {
        (false, Some(family)) => {
            let store = crate::doc_store::DocStore::for_member(&family.common_dir, repo);
            crate::doc_store::sidecar_key(source_sha256, &policy.doc)
                .map_or(SidecarState::Owed, |key| held_sidecar(&store, &key, repo))
        }
        _ => SidecarState::Owed,
    }
}

/// What the family's store says about one key, and — when it holds an answer —
/// this member's view of it.
///
/// The member view is a hardlink into the checkout the caller is standing in,
/// so the path the answer names is one the caller can open. Linking is
/// best-effort: the Markdown is served either way, and a member that could not
/// be given a view is told so by the absence of a path rather than by a
/// refusal.
fn held_sidecar(
    store: &crate::doc_store::DocStore,
    key: &str,
    repo: &Path,
) -> rift_recall_serve::SidecarState {
    use rift_recall_serve::{Sidecar, SidecarState};

    if store.is_skipped(key) {
        return SidecarState::Declined;
    }
    if !store.holds(key) {
        return SidecarState::Owed;
    }
    let Ok(markdown) = std::fs::read_to_string(store.family_path(key)) else {
        return SidecarState::Owed;
    };
    let member = store.link(key).ok();
    let path = member
        .as_ref()
        .and_then(|m| rift_recall_substrate::worktree::repo_relative_slash(repo, m))
        .unwrap_or_default();
    SidecarState::Held(Sidecar { path, markdown })
}

/// Journal the conversion a document verb found owing.
///
/// A request, not the work: the file is a couple of hundred bytes and one
/// rename, and the worker decides whether there is anything to do. Keyed by the
/// document's bytes, so two callers meeting the same document owe one
/// conversion. Best-effort — a serve already made is never retracted because a
/// job file could not be written.
pub(super) fn owe_document_conversion(repo: &Path, req: &rift_recall_serve::ToolRequest) -> bool {
    let (Some(rel_path), Some(doc)) = (req.target.as_deref(), req.document.as_ref()) else {
        return false;
    };
    let Ok(paths) = rift_git::discover(repo) else {
        return false;
    };
    crate::agent_queue::enqueue(
        &paths.git_dir,
        &crate::agent_queue::AgentJob {
            schema_version: 1,
            work_tree: paths.work_tree.clone(),
            queued_at_ms: crate::hooks::now_ms(),
            attempts: 0,
            kind: crate::agent_queue::JobKind::DocConvert {
                rel_path: rel_path.to_owned(),
                content_sha256: doc.source_sha256.clone(),
            },
        },
    )
    .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rift_recall_serve::ToolName;

    /// A repository with no `.rift` policy and no family: enough to drive every
    /// decision this verb makes from the caller's own file.
    fn doc_repo() -> tempfile::TempDir {
        tempfile::tempdir().expect("a temp repo")
    }

    fn doc_request(path: &str) -> rift_recall_serve::ToolRequest {
        let (_tool, req) = rift_recall_serve::decode_verb("doc-read", &json!({ "target": path }))
            .expect("the verb decodes");
        req
    }

    fn resolve_in(
        repo: &Path,
        path: &str,
    ) -> std::result::Result<rift_recall_serve::ToolRequest, Outcome> {
        let cfg = rift_config::EffectiveConfig::secure_default().expect("a default config");
        resolve_document(ToolName::DocRead, doc_request(path), repo, &cfg)
    }

    /// Everything decidable from the caller's own file is decided before a
    /// store is opened, and each refusal says which of the four it is. They are
    /// USAGE refusals on purpose: none of them is the serving surface saying
    /// no, so none of them books a serve.
    #[test]
    fn what_is_not_a_document_is_refused_before_anything_is_opened() {
        let dir = doc_repo();
        let repo = dir.path();
        std::fs::write(repo.join("notes.md"), "# notes\n").unwrap();
        std::fs::write(repo.join("plan.docx"), b"PK\x03\x04binary").unwrap();

        for (path, tag) in [
            ("notes.md", "not_a_document"),
            ("src/main.rs", "not_a_document"),
            ("missing.docx", "no_such_document"),
            ("../outside.docx", "path_outside_repository"),
        ] {
            let refusal = resolve_in(repo, path).expect_err(path);
            assert_eq!(refusal.code, exit::USAGE, "{path}");
            assert_eq!(refusal.json["error"], tag, "{path}");
        }

        // A document over the size the family converts is told now, rather than
        // left waiting for a conversion that will never be attempted.
        let policies = repo.join(".rift/policies");
        std::fs::create_dir_all(&policies).unwrap();
        std::fs::write(policies.join("serve.toml"), "[doc]\nmax_input_bytes = 4\n").unwrap();
        let refusal = resolve_in(repo, "plan.docx").expect_err("over the cap");
        assert_eq!(refusal.code, exit::USAGE);
        assert_eq!(refusal.json["error"], "document_over_the_conversion_cap");

        // And a repository that switched documents off answers that, rather
        // than converting or pretending there is nothing there.
        std::fs::write(policies.join("serve.toml"), "[doc]\nmode = \"off\"\n").unwrap();
        let refusal = resolve_in(repo, "plan.docx").expect_err("documents are off");
        assert_eq!(refusal.code, exit::PRECONDITION);
        assert_eq!(refusal.json["error"], "doc_off");
    }

    /// A document Rift converts is resolved to its identity and the declared
    /// cost of the read this verb replaces — and, with no family store behind
    /// it, to the honest `Owed`. Nothing is converted on this path.
    #[test]
    fn a_document_resolves_to_its_identity_and_its_declared_cost() {
        let dir = doc_repo();
        let repo = dir.path();
        let bytes = b"PK\x03\x04 a container full of bytes nobody can read as text";
        std::fs::write(repo.join("plan.docx"), bytes).unwrap();

        let req = resolve_in(repo, "plan.docx").expect("a document resolves");
        assert_eq!(req.target.as_deref(), Some("plan.docx"));
        let doc = req.document.expect("the document half is resolved");
        assert_eq!(doc.source_sha256, rift_shadow::sha256_hex(&[bytes]));
        assert_eq!(
            doc.declared_baseline,
            rift_recall_serve::declared_document_baseline("docx", bytes),
            "the declaration is the one function that prices a native read"
        );
        assert_eq!(doc.sidecar, rift_recall_serve::SidecarState::Owed);

        // The verb takes the path as typed — absolute or repo-relative — and
        // answers about the same file either way.
        let absolute = repo.join("plan.docx");
        let same = resolve_in(repo, &absolute.to_string_lossy()).expect("an absolute path");
        assert_eq!(same.target.as_deref(), Some("plan.docx"));

        // A PDF declares nothing: its pages are images, and no pinned artifact
        // gives a vision-token rate.
        std::fs::write(repo.join("plan.pdf"), bytes).unwrap();
        let pdf = resolve_in(repo, "plan.pdf").expect("a pdf resolves");
        assert_eq!(pdf.document.expect("resolved").declared_baseline, 0);
    }

    /// What the family's store says, in this member's words: the Markdown and a
    /// path inside this checkout when it holds one, the decision when it has
    /// made one, and `Owed` when it has neither.
    #[cfg(feature = "doc")]
    #[test]
    fn the_family_store_answers_in_this_members_own_paths() {
        use rift_recall_serve::SidecarState;

        let family = doc_repo();
        let member = doc_repo();
        let store = crate::doc_store::DocStore::for_member(family.path(), member.path());
        let key = "a".repeat(64);

        assert_eq!(
            held_sidecar(&store, &key, member.path()),
            SidecarState::Owed
        );

        store
            .put(&key, "# converted\n")
            .expect("the family converts once");
        match held_sidecar(&store, &key, member.path()) {
            SidecarState::Held(sidecar) => {
                assert_eq!(sidecar.markdown, "# converted\n");
                assert_eq!(sidecar.path, format!(".rift/state/doc/{key}.md"));
                assert!(
                    member.path().join(&sidecar.path).is_file(),
                    "the answer names a path inside the checkout the caller is standing in"
                );
            }
            other => panic!("the family holds it: {other:?}"),
        }

        // A decision against a document outranks the answer: it is a fact about
        // the document, and nothing will convert it later either.
        let declined = "b".repeat(64);
        store.mark_skipped(&declined, "thin");
        assert_eq!(
            held_sidecar(&store, &declined, member.path()),
            SidecarState::Declined
        );
    }
}
