//! What one context window has already been told about documents.
//!
//! Its own module because it is memory rather than storage: nothing here is an
//! answer, only the record of what a conversation was handed and what it was
//! told once. The sidecars it names are the store's; the sweep that collects
//! these directories is the sweep's.

#[cfg(feature = "doc")]
use std::path::{Path, PathBuf};

#[cfg(feature = "doc")]
use super::key::is_sha256;
#[cfg(feature = "doc")]
use super::store::restrict;

/// The directory a window's document decisions are remembered in, under this
/// member's own run state.
pub(super) const WINDOW_DIR: &str = "w";

/// The marker that says this window is answered by the redirect rather than by
/// the substitution.
#[cfg(feature = "doc")]
const REDIRECT_MARKER: &str = "redirect";

/// What one **context window** has already been told about documents.
///
/// Three facts, and each is a file whose *existence* is the fact, created with
/// `create_new` so that the process which created it is the one that acted.
/// That is not a stylistic choice: two of the three are "has this happened
/// yet", a client may run its hooks in parallel, and a read-modify-write of a
/// shared record would let two hooks both conclude they were first — which for
/// the redirect means declining the same read twice, and a decline is allowed
/// exactly once per key precisely so that the repeated call is the escape
/// hatch its own sentence promises.
///
/// Per window and not per session: a subagent runs under its parent's session
/// id with its own agent id and holds different bytes, so it is told
/// separately — the same key the window journal is filed under, computed by
/// the same function.
///
/// Local run state. Losing the directory costs one extra redirect or one extra
/// substitution and can never cost an answer.
///
/// A window remembers what the document gate said, so it exists where the
/// gate does: a build without the document seam answers no document read and
/// has nothing to remember. What outlives the feature is the sweep below,
/// which removes the directories a converting build left behind.
#[cfg(feature = "doc")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DocWindow {
    dir: PathBuf,
}

/// One amendment this window made, as its result finds it again: the document
/// the agent asked for, and the identity of the bytes the amendment was made
/// about.
#[cfg(feature = "doc")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ArmedRewrite {
    /// This checkout's own spelling of the document.
    pub(crate) rel_path: String,
    /// The bytes it had when the substitution was decided — the identity the
    /// declaration is made under, and the one a worker re-reads it by.
    pub(crate) content_sha256: String,
}

#[cfg(feature = "doc")]
impl DocWindow {
    /// The window a hook belongs to, as this member sees it.
    pub(crate) fn at(work_tree: &Path, session_id: &str, agent_id: Option<&str>) -> Self {
        Self {
            dir: work_tree
                .join(".rift/state/doc")
                .join(WINDOW_DIR)
                .join(crate::window_journal::key(session_id, agent_id)),
        }
    }

    /// Whether this window has been flipped to the redirect.
    pub(crate) fn is_redirecting(&self) -> bool {
        self.dir.join(REDIRECT_MARKER).exists()
    }

    /// Flip it, and say whether **this** call is the one that did.
    ///
    /// The caller records a dropped substitution once, not once per hook that
    /// notices the same drop.
    pub(crate) fn flip_to_redirect(&self) -> bool {
        self.create_marker(REDIRECT_MARKER, b"dropped")
    }

    /// Take the redirect for `key`, and say whether this call may make it.
    ///
    /// `false` means some earlier call in this window already named that
    /// sidecar, so the read runs: **one decline per key**, and the repeat is
    /// the escape hatch the frame itself promises.
    pub(crate) fn redirect_once(&self, key: &str) -> bool {
        is_sha256(key) && self.create_marker(&format!("r-{key}"), b"")
    }

    /// Remember that this window's read of `rel_path` was answered with the
    /// sidecar `key` names, so the result of that read can be checked against
    /// it — from whichever side the result arrives on.
    ///
    /// **Two markers for one amendment**, because a result names one of two
    /// paths and each of them has to find the fact from its own name: the
    /// sidecar, which means the amendment survived to the tool and the read
    /// the agent wrote never happened, or the document, which means a sibling
    /// hook's amendment was the one the client kept. Each marker names the
    /// other, so whichever is taken takes its twin with it: one amendment is
    /// answered once, and no later result can answer it a second time.
    ///
    /// The names are digests and keys — never a path — and the bodies carry
    /// the identity of the bytes the amendment was made about, so the result's
    /// own hook hashes nothing and the accounting row it journals does not
    /// have to re-read the file to say what it is about.
    pub(crate) fn arm_rewrite(&self, rel_path: &str, key: &str, content_sha256: &str) {
        if !is_sha256(key) || !is_sha256(content_sha256) || rel_path.is_empty() {
            return;
        }
        let _ = self.create_marker(
            &Self::armed(rel_path),
            format!("{content_sha256}\n{key}").as_bytes(),
        );
        let _ = self.create_marker(
            &Self::armed_sidecar(key),
            format!("{content_sha256}\n{rel_path}").as_bytes(),
        );
    }

    /// Take the arming for `rel_path` — the **dropped** side — and hand back
    /// the identity of the bytes it was made about.
    ///
    /// Consumed, not read: a substitution that was dropped is recorded once,
    /// and the window is flipped once. Its twin goes with it, so the sidecar
    /// this window was pointed at can no longer be presented as a substitution
    /// that worked.
    pub(crate) fn take_armed(&self, rel_path: &str) -> Option<String> {
        let (sha, key) = self.take_marker(&Self::armed(rel_path))?;
        let _ = std::fs::remove_file(self.dir.join(Self::armed_sidecar(&key)));
        is_sha256(&sha).then_some(sha)
    }

    /// Take the arming the sidecar for `key` was made under — the **verified**
    /// side — and hand back the document it stood in for.
    ///
    /// `None` for a sidecar this window never armed: an agent reading Rift's
    /// own Markdown on its own account is not a substitution, and only a read
    /// Rift amended may be claimed.
    pub(crate) fn take_armed_sidecar(&self, key: &str) -> Option<ArmedRewrite> {
        if !is_sha256(key) {
            return None;
        }
        let (content_sha256, rel_path) = self.take_marker(&Self::armed_sidecar(key))?;
        let _ = std::fs::remove_file(self.dir.join(Self::armed(&rel_path)));
        (is_sha256(&content_sha256) && !rel_path.is_empty()).then_some(ArmedRewrite {
            rel_path,
            content_sha256,
        })
    }

    /// Read one marker's two fields and remove it, so the fact it carries is
    /// answered by exactly one caller.
    ///
    /// The identity comes first because it is a fixed shape; the whole of what
    /// follows the first newline is the other field, so a path that carries a
    /// newline of its own survives the round trip.
    fn take_marker(&self, name: &str) -> Option<(String, String)> {
        let path = self.dir.join(name);
        let body = std::fs::read_to_string(&path).ok()?;
        std::fs::remove_file(&path).ok()?;
        let (head, tail) = body.split_once('\n')?;
        Some((head.to_owned(), tail.to_owned()))
    }

    /// The marker name for one path.
    fn armed(rel_path: &str) -> String {
        format!(
            "p-{}",
            rift_hashchain::leaf_hash(rel_path.as_bytes()).to_hex()
        )
    }

    /// The marker name for the sidecar one amendment pointed at.
    fn armed_sidecar(key: &str) -> String {
        format!("s-{key}")
    }

    /// Create one marker, and say whether this call created it.
    fn create_marker(&self, name: &str, body: &[u8]) -> bool {
        if std::fs::create_dir_all(&self.dir).is_err() {
            return false;
        }
        let path = self.dir.join(name);
        let Ok(mut file) = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        else {
            return false;
        };
        use std::io::Write as _;
        let _ = file.write_all(body);
        restrict(&path);
        true
    }
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "doc")]
    use super::*;
    #[cfg(feature = "doc")]
    use crate::doc_store::key::{member_sidecar_key, sidecar_key};
    #[cfg(feature = "doc")]
    use crate::doc_store::tests::{family, SHA_A, SHA_B};
    #[cfg(feature = "doc")]
    use rift_recall_policy::DocPolicy;

    /// One amendment, two names, and exactly one claim.
    ///
    /// A read's result names either the sidecar the amendment pointed at or
    /// the document the agent wrote, so the arming has to be findable from
    /// both — and taking it from either side has to take it from the other,
    /// because the two names are one amendment and a saving claimed twice for
    /// one read is a saving Rift did not make.
    #[cfg(feature = "doc")]
    #[test]
    fn one_amendment_is_answered_from_either_side_and_only_once() {
        let f = family();
        let key = sidecar_key(SHA_A, &DocPolicy::default()).unwrap();
        let window = DocWindow::at(&f.main, "s-1", None);

        // A view of this member's store is recognised as one; a window's own
        // markers, a name that is not a key, and a document are not.
        assert_eq!(
            member_sidecar_key(&format!(".rift/state/doc/{key}.md")),
            Some(key.as_str())
        );
        for other in [
            format!(".rift/state/doc/{WINDOW_DIR}/abc/{key}.md"),
            ".rift/state/doc/notes.md".to_owned(),
            format!("{key}.md"),
            format!(".rift/state/doc/{key}.txt"),
        ] {
            assert_eq!(member_sidecar_key(&other), None, "{other}");
        }

        // The verified side: the result named the sidecar, so the document it
        // stood in for and the bytes it was made about come back — once.
        window.arm_rewrite("docs/plan.docx", &key, SHA_A);
        assert_eq!(
            window.take_armed_sidecar(&key),
            Some(ArmedRewrite {
                rel_path: "docs/plan.docx".to_owned(),
                content_sha256: SHA_A.to_owned(),
            })
        );
        assert_eq!(window.take_armed_sidecar(&key), None, "claimed once");
        assert_eq!(
            window.take_armed("docs/plan.docx"),
            None,
            "and the same amendment cannot then be reported as dropped"
        );

        // The dropped side, of a second amendment: the result named the
        // document, and the sidecar this window was pointed at can no longer
        // be presented as a substitution that happened.
        window.arm_rewrite("docs/plan.docx", &key, SHA_A);
        assert_eq!(window.take_armed("docs/plan.docx"), Some(SHA_A.to_owned()));
        assert_eq!(window.take_armed_sidecar(&key), None);

        // Neither side answers about an amendment nobody made.
        assert_eq!(
            window.take_armed_sidecar(&sidecar_key(SHA_B, &DocPolicy::default()).unwrap()),
            None
        );
        assert_eq!(window.take_armed("docs/other.docx"), None);
    }
}
