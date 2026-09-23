//! Filling the store: one document in, one sidecar out.
//!
//! The half no decision reaches. Converting costs a process and a parse, so it
//! belongs to the detached worker, and keeping it in its own module is what
//! makes that visible: a hook asks the store whether an answer exists and
//! never comes in here.

#[cfg(feature = "doc")]
use std::path::{Path, PathBuf};

#[cfg(feature = "doc")]
use rift_recall_policy::DocPolicy;

#[cfg(feature = "doc")]
use super::key::sidecar_key;
#[cfg(feature = "doc")]
use super::store::DocStore;

/// What became of one document.
#[cfg(feature = "doc")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Conversion {
    /// The family already held the answer; this member now has a view of it.
    Linked(PathBuf),
    /// Converted here, and linked into this member.
    Converted(PathBuf),
    /// Decided against, once and for the whole family: the reason is a
    /// property of the document, so nothing will convert it later either.
    Refused(&'static str),
    /// Not now: no helper, a deadline, a fault that says nothing about the
    /// document. The next pass tries again.
    Deferred(&'static str),
}

/// The engine classifications that are facts about the **document** rather
/// than about the run, and therefore worth remembering.
///
/// The rest — a helper that is not installed, a deadline, an unwinding parser,
/// a machine out of memory — say only that this attempt failed, and a marker
/// written for one of those would refuse a perfectly convertible document for
/// as long as the family kept it.
#[cfg(feature = "doc")]
const DOCUMENT_FAULTS: [&str; 5] = [
    "encrypted",
    "malformed",
    "missingPart",
    "needsOcr",
    "unsupported",
];

/// The caps `doc` puts on a conversion.
#[cfg(feature = "doc")]
pub(crate) fn caps(doc: &DocPolicy) -> rift_recall_doc::ConvertCaps {
    rift_recall_doc::ConvertCaps {
        table_rows: usize::try_from(doc.table_rows).unwrap_or(rift_recall_doc::DEFAULT_TABLE_ROWS),
        max_input_bytes: doc.max_input_bytes,
        pdf_min_chars_per_page: doc.pdf_min_chars_per_page,
    }
}

/// Convert the document at `path` into the family store, or say why not.
///
/// `expected_sha256` is the identity a caller recorded earlier (a journaled
/// job names the bytes it meant); a file that has moved on since is left
/// alone rather than converted under a stale name.
///
/// Never converts what the family already holds, never converts what the
/// family has already decided against, and never reads a file larger than the
/// policy's cap. Every failure is a value: this runs in the detached worker,
/// where the honest outcomes are "there is an answer now" and "there is not".
#[cfg(feature = "doc")]
pub(crate) fn convert_into_store(
    store: &DocStore,
    converter: &dyn rift_recall_doc::DocConverter,
    path: &Path,
    doc: &DocPolicy,
    expected_sha256: Option<&str>,
) -> Conversion {
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
        return Conversion::Refused("no-extension");
    };
    if !rift_recall_doc::is_supported_extension(ext) {
        return Conversion::Refused("unsupported-extension");
    }
    // The size is read before the bytes are: a document over the cap is never
    // converted, and refusing it must not cost reading it.
    let Ok(meta) = std::fs::metadata(path) else {
        return Conversion::Deferred("unreadable");
    };
    if meta.len() > doc.max_input_bytes {
        return Conversion::Refused("too-large");
    }
    let Ok(bytes) = std::fs::read(path) else {
        return Conversion::Deferred("unreadable");
    };
    let sha = rift_shadow::sha256_hex(&[&bytes]);
    if expected_sha256.is_some_and(|expected| expected != sha) {
        return Conversion::Deferred("moved-on");
    }
    let Some(key) = sidecar_key(&sha, doc) else {
        return Conversion::Deferred("unreadable");
    };
    if store.holds(&key) {
        return match store.link(&key) {
            Ok(member) => Conversion::Linked(member),
            Err(_) => Conversion::Deferred("link"),
        };
    }
    if store.is_skipped(&key) {
        return Conversion::Refused("already-decided");
    }
    let caps = caps(doc);
    match converter.convert(&bytes, ext, &caps) {
        Ok(markdown) if markdown.is_thin(&caps) => {
            // A page image is not text, and Markdown that says so is worse
            // than the page the agent asked for. Recorded so the family stops
            // paying a parse to learn it again.
            store.mark_skipped(&key, "thin");
            Conversion::Refused("thin")
        }
        Ok(markdown) => {
            let basename = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("(unnamed)");
            let text = rift_recall_doc::sidecar_text(&markdown, basename, &sha);
            match store.put(&key, &text) {
                Ok(member) => Conversion::Converted(member),
                Err(_) => Conversion::Deferred("write"),
            }
        }
        Err(rift_recall_doc::DocError::Engine { code }) if DOCUMENT_FAULTS.contains(&code) => {
            store.mark_skipped(&key, code);
            Conversion::Refused(code)
        }
        Err(rift_recall_doc::DocError::Engine { code }) => Conversion::Deferred(code),
        Err(rift_recall_doc::DocError::TooLarge { .. }) => Conversion::Refused("too-large"),
        Err(rift_recall_doc::DocError::UnsupportedExtension(_)) => {
            Conversion::Refused("unsupported-extension")
        }
    }
}

// ------------------------------------------------------- the proactive pass ---

/// Documents examined in one proactive pass.
///
/// A pass runs in the detached worker, where the session that asked for it has
/// already been answered, so the bound is not a latency budget: it is the
/// promise that a pass is a bounded amount of work on a machine somebody is
/// using, whatever the repository turns out to hold.
#[cfg(feature = "doc")]
pub(crate) const SWEEP_LIMIT: usize = 200;

/// What one pass did, for the log line that is the only thing watching it.
#[cfg(feature = "doc")]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct PassReport {
    /// Documents whose bytes were read and keyed.
    pub(crate) examined: usize,
    /// Documents converted here.
    pub(crate) converted: usize,
    /// Documents the family already held, linked into this member.
    pub(crate) linked: usize,
    /// Documents decided against, now or earlier.
    pub(crate) refused: usize,
    /// Documents whose conversion is still owed.
    pub(crate) deferred: usize,
}

/// The documents this work tree tracks, in git's order.
///
/// **Tracked**, not "found by walking": the repository's index is already the
/// statement of what belongs to this project, so a build directory full of
/// generated PDFs, an ignored vendor tree and a stray download are outside the
/// pass by construction rather than by a list of exclusions somebody has to
/// maintain. It also costs one `git` child process instead of a directory
/// walk, and it is the member's own index, so each member sweeps what it has
/// checked out.
///
/// The pathspecs are the converter's extension table, so a format Rift does
/// not convert is never even listed.
#[cfg(feature = "doc")]
fn tracked_documents(repo: &rift_git::RepoPaths) -> Vec<String> {
    let mut args = vec!["ls-files".to_owned(), "-z".to_owned(), "--".to_owned()];
    args.extend(
        rift_recall_doc::SUPPORTED_EXTENSIONS
            .iter()
            .map(|ext| format!("*.{ext}")),
    );
    let Ok((true, out)) = rift_git::run_git_status(repo, &args, &[], &[]) else {
        return Vec::new();
    };
    String::from_utf8_lossy(&out)
        .split('\0')
        .filter(|path| !path.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

/// Convert this work tree's tracked documents into the family store.
#[cfg(feature = "doc")]
pub(crate) fn convert_tracked_documents(
    repo: &rift_git::RepoPaths,
    store: &DocStore,
    converter: &dyn rift_recall_doc::DocConverter,
    config: &rift_config::EffectiveConfig,
    policy: &rift_recall_policy::ServePolicy,
    limit: usize,
) -> PassReport {
    convert_documents(
        &tracked_documents(repo),
        &repo.work_tree,
        store,
        converter,
        config,
        policy,
        limit,
    )
}

/// The pass itself, over a list of repo-relative paths.
///
/// Stops after `limit` documents have been examined, in the order it was
/// given. A repository with more documents than that converts a prefix per
/// pass and reaches the rest through the on-demand job — a pass that walked
/// everything would be an unbounded read of the whole repository every time a
/// session started.
///
/// The two refusals are the repository's own and are made before a byte is
/// read: a path it declared sensitive, and a path its serving policy never
/// serves. Neither counts against the limit — refusing a name is not work.
#[cfg(feature = "doc")]
fn convert_documents(
    paths: &[String],
    work_tree: &Path,
    store: &DocStore,
    converter: &dyn rift_recall_doc::DocConverter,
    config: &rift_config::EffectiveConfig,
    policy: &rift_recall_policy::ServePolicy,
    limit: usize,
) -> PassReport {
    let mut report = PassReport::default();
    for rel in paths {
        if report.examined >= limit {
            break;
        }
        if !is_convertible_path(config, policy, rel) {
            continue;
        }
        let Some(path) = document_path(work_tree, rel) else {
            continue;
        };
        report.examined += 1;
        match convert_into_store(store, converter, &path, &policy.doc, None) {
            Conversion::Converted(_) => report.converted += 1,
            Conversion::Linked(_) => report.linked += 1,
            Conversion::Refused(_) => report.refused += 1,
            Conversion::Deferred(_) => report.deferred += 1,
        }
    }
    report
}

/// The absolute path of a repo-relative document, or `None` when the name does
/// not stay inside the checkout.
///
/// A path can arrive from an agent's tool call by way of a journaled job, so
/// it is normalized through the same lexical rule the serving policy uses —
/// `..` resolved against earlier segments, an escape refused — before it is
/// joined to anything.
#[cfg(feature = "doc")]
pub(crate) fn document_path(work_tree: &Path, rel_path: &str) -> Option<PathBuf> {
    let rel = rift_recall_policy::normalize_rel(rel_path)?;
    (!rel.is_empty()).then(|| work_tree.join(rel))
}

/// Whether a document may be converted at all.
///
/// Two refusals, both the repository's own: a path it declared sensitive, and
/// a path its serving policy never serves. A sidecar is durable text derived
/// from the file, so a file that must not be served must not be re-encoded
/// into a second copy beside the checkout either.
#[cfg(feature = "doc")]
pub(crate) fn is_convertible_path(
    config: &rift_config::EffectiveConfig,
    policy: &rift_recall_policy::ServePolicy,
    rel_path: &str,
) -> bool {
    !config.is_sensitive_path(rel_path)
        && !policy
            .denied_paths
            .iter()
            .any(|glob| rift_config::glob_match(glob, rel_path))
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "doc")]
    use super::*;
    #[cfg(feature = "doc")]
    use crate::doc_store::tests::{family, store_for, SHA_A};

    #[cfg(feature = "doc")]
    mod conversion {
        use super::*;
        use rift_recall_doc::{ConvertCaps, DocConverter, DocError, Markdown};

        /// A converter that counts, so "converted once for the family" is an
        /// observation rather than a hope.
        struct Counting {
            calls: std::cell::Cell<usize>,
            answer: std::result::Result<Markdown, DocError>,
        }

        impl Counting {
            fn ok(text: &str) -> Self {
                Self {
                    calls: std::cell::Cell::new(0),
                    answer: Ok(Markdown {
                        text: text.to_owned(),
                        ..Markdown::default()
                    }),
                }
            }
            fn err(answer: DocError) -> Self {
                Self {
                    calls: std::cell::Cell::new(0),
                    answer: Err(answer),
                }
            }
        }

        impl DocConverter for Counting {
            fn convert(
                &self,
                _bytes: &[u8],
                _ext: &str,
                _caps: &ConvertCaps,
            ) -> std::result::Result<Markdown, DocError> {
                self.calls.set(self.calls.get() + 1);
                self.answer.clone()
            }
        }

        fn document(dir: &Path, name: &str, bytes: &str) -> PathBuf {
            let path = dir.join(name);
            std::fs::write(&path, bytes).unwrap();
            path
        }

        /// ★ The same claim as the store test, end to end through the
        /// conversion path: the second member's pass links what the first
        /// member's pass converted, and the converter is not called again.
        #[test]
        fn a_document_is_converted_once_for_the_family_and_linked_everywhere_else() {
            let f = family();
            let doc = DocPolicy::default();
            let converter = Counting::ok("# Costs\n");
            let in_main = document(&f.main, "costs.xlsx", "book");
            let in_linked = document(&f.linked, "costs.xlsx", "book");

            let first =
                convert_into_store(&store_for(&f, &f.main), &converter, &in_main, &doc, None);
            let Conversion::Converted(main_side) = first else {
                panic!("the first pass converts: {first:?}");
            };
            assert_eq!(converter.calls.get(), 1);
            assert!(std::fs::read_to_string(&main_side)
                .unwrap()
                .contains("# Costs"));

            let second = convert_into_store(
                &store_for(&f, &f.linked),
                &converter,
                &in_linked,
                &doc,
                None,
            );
            let Conversion::Linked(linked_side) = second else {
                panic!("the second member links what the family already has: {second:?}");
            };
            assert_eq!(
                converter.calls.get(),
                1,
                "the same bytes are never converted twice for one family"
            );
            assert!(linked_side.starts_with(&f.linked));
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt as _;
                assert_eq!(
                    std::fs::metadata(&main_side).unwrap().ino(),
                    std::fs::metadata(&linked_side).unwrap().ino()
                );
            }
        }

        /// The cheap refusals are made before anything is parsed, and a
        /// journaled job's bytes are checked against the identity it named.
        #[test]
        fn what_is_never_converted_is_refused_before_the_parser() {
            let f = family();
            let doc = DocPolicy {
                max_input_bytes: 4,
                ..DocPolicy::default()
            };
            let converter = Counting::ok("# nope\n");
            let store = store_for(&f, &f.main);

            let source = document(&f.main, "notes.md", "plain markdown");
            assert_eq!(
                convert_into_store(&store, &converter, &source, &doc, None),
                Conversion::Refused("unsupported-extension")
            );
            let big = document(&f.main, "big.docx", "far more than four bytes");
            assert_eq!(
                convert_into_store(&store, &converter, &big, &doc, None),
                Conversion::Refused("too-large")
            );
            let small = document(&f.main, "s.docx", "ab");
            assert_eq!(
                convert_into_store(&store, &converter, &small, &doc, Some(SHA_A)),
                Conversion::Deferred("moved-on"),
                "a job names the bytes it meant; a file that moved on is left alone"
            );
            assert_eq!(converter.calls.get(), 0, "nothing was parsed");
        }

        /// A fault of the document is remembered for the family; a fault of
        /// the run is not, so the next pass tries again.
        #[test]
        fn only_a_fault_of_the_document_is_remembered() {
            let f = family();
            let doc = DocPolicy::default();
            let store = store_for(&f, &f.main);
            let sealed = document(&f.main, "sealed.docx", "locked");
            let encrypted = Counting::err(DocError::Engine { code: "encrypted" });
            assert_eq!(
                convert_into_store(&store, &encrypted, &sealed, &doc, None),
                Conversion::Refused("encrypted")
            );
            // A second pass costs nothing at all: the family has decided.
            assert_eq!(
                convert_into_store(&store, &encrypted, &sealed, &doc, None),
                Conversion::Refused("already-decided")
            );
            assert_eq!(encrypted.calls.get(), 1);

            let missing = document(&f.main, "other.docx", "fine");
            let no_helper = Counting::err(DocError::Engine {
                code: "helperMissing",
            });
            assert_eq!(
                convert_into_store(&store, &no_helper, &missing, &doc, None),
                Conversion::Deferred("helperMissing")
            );
            assert_eq!(
                convert_into_store(&store, &no_helper, &missing, &doc, None),
                Conversion::Deferred("helperMissing"),
                "a run that failed says nothing about the document"
            );
            assert_eq!(no_helper.calls.get(), 2);
        }

        /// A scanned page is not text: the conversion is thin, nothing is
        /// stored to substitute for the page the agent asked for, and the
        /// family remembers so it does not pay the parse again.
        #[test]
        fn a_scanned_document_is_decided_against_rather_than_stored() {
            let f = family();
            let doc = DocPolicy::default();
            let store = store_for(&f, &f.main);
            let scanned = document(&f.main, "scan.pdf", "%PDF");
            let converter = Counting {
                calls: std::cell::Cell::new(0),
                answer: Ok(Markdown {
                    text: "  \n".to_owned(),
                    pages: Some(4),
                    thin_pages: vec![1, 2, 3, 4],
                    ..Markdown::default()
                }),
            };
            assert_eq!(
                convert_into_store(&store, &converter, &scanned, &doc, None),
                Conversion::Refused("thin")
            );
            let key = sidecar_key(&rift_shadow::sha256_hex(&[b"%PDF"]), &doc).unwrap();
            assert!(!store.holds(&key), "nothing thin is ever stored");
            assert!(store.is_skipped(&key));
        }

        /// The proactive pass: bounded, progressive, and refusing what the
        /// repository said it would refuse — before it reads a byte of it.
        #[test]
        fn a_pass_is_bounded_and_never_reads_what_the_repository_withholds() {
            let f = family();
            std::fs::create_dir_all(f.main.join(".rift")).unwrap();
            std::fs::write(
                f.main.join(".rift/config.toml"),
                "schema_version = 1\n\n[project]\nname = \"docs\"\n\n[paths]\n\
                 sensitive_paths = [\"board/**\"]\n",
            )
            .unwrap();
            let config = rift_config::EffectiveConfig::load(&f.main.join(".rift")).unwrap();
            let policy = rift_recall_policy::ServePolicy {
                denied_paths: vec!["private/**".to_owned()],
                ..rift_recall_policy::ServePolicy::default()
            };
            let store = store_for(&f, &f.main);
            let converter = Counting::ok("# converted\n");
            for rel in ["a.docx", "b.docx"] {
                document(&f.main, rel, rel);
            }
            for dir in ["board", "private"] {
                std::fs::create_dir_all(f.main.join(dir)).unwrap();
                document(&f.main, &format!("{dir}/secret.docx"), "confidential");
            }
            let paths: Vec<String> = [
                "a.docx",
                "b.docx",
                "board/secret.docx",
                "private/secret.docx",
            ]
            .iter()
            .map(|p| (*p).to_owned())
            .collect();

            let first = convert_documents(&paths, &f.main, &store, &converter, &config, &policy, 1);
            assert_eq!(
                first,
                PassReport {
                    examined: 1,
                    converted: 1,
                    ..PassReport::default()
                },
                "the bound is a promise about how much work one pass is"
            );

            let second =
                convert_documents(&paths, &f.main, &store, &converter, &config, &policy, 9);
            assert_eq!(
                second,
                PassReport {
                    examined: 2,
                    converted: 1,
                    linked: 1,
                    ..PassReport::default()
                },
                "what the family already holds is linked, not converted again"
            );
            assert_eq!(
                converter.calls.get(),
                2,
                "a sensitive or never-served document is refused before it is read"
            );
            let withheld =
                sidecar_key(&rift_shadow::sha256_hex(&[b"confidential"]), &policy.doc).unwrap();
            assert!(!store.holds(&withheld));
        }
    }
}
