//! Shadow & session readers — the local-only half.
//!
//! Shadow branches (`<dialect>/<commit7>-<wt6>`) hold **raw, pre-redaction**
//! session data, so this is the most sensitive read in the reader. The
//! enumerator iterates **local branches only** (never `refs/remotes/*`), and
//! [`classify_shadow`] is a second belt that rejects any remote name. There is no
//! fetch path and no network in the substrate.

use std::path::Path;

use rift_core::{Result, RiftError};

use crate::session::{self, Phase};
use crate::{Dialect, ShardWalker};

/// A discovered local shadow branch (raw, working-tree-scoped capture ref).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShadowRef {
    /// Full ref name, e.g. `refs/heads/rift/1a2b3c4-5d6e7f`.
    pub full_name: String,
    /// Base commit short id (7 hex).
    pub commit7: String,
    /// Worktree discriminator (6 hex).
    pub wt6: String,
    /// Which dialect.
    pub dialect: Dialect,
    /// Tip oid (hex); empty until filled by the enumerator.
    pub tip: String,
}

fn is_short_hex(s: &str, n: usize) -> bool {
    s.len() == n && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Classify a ref name as a local shadow branch for `dialect`, or `None`.
///
/// Rejects (returns `None` for): any `refs/remotes/*` name, the orphan
/// `checkpoints/` branch, and anything not matching `<commit7>-<wt6>`. This is a
/// pure function so the local-only invariant is unit-testable without a repo.
#[must_use]
pub fn classify_shadow(ref_name: &str, dialect: Dialect) -> Option<ShadowRef> {
    if ref_name.starts_with("refs/remotes/") {
        return None; // belt: raw shadow data is never read from a remote
    }
    let rest = ref_name.strip_prefix(dialect.shadow_prefix())?;
    if rest.contains("checkpoints/") {
        return None; // that's the durable orphan branch, not a shadow ref
    }
    let (commit7, wt6) = rest.split_once('-')?;
    if !is_short_hex(commit7, 7) || !is_short_hex(wt6, 6) {
        return None;
    }
    Some(ShadowRef {
        full_name: ref_name.to_string(),
        commit7: commit7.to_string(),
        wt6: wt6.to_string(),
        dialect,
        tip: String::new(),
    })
}

/// A unified session entry: its tag pin, lifecycle phase hint, and agent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionEntry {
    /// The session id.
    pub session_id: String,
    /// Dialect of the session tag, if pinned.
    pub dialect: Option<Dialect>,
    /// Tag oid (hex), if pinned.
    pub tag_oid: Option<String>,
    /// Lifecycle phase from the out-of-tree hint, if present.
    pub phase: Option<Phase>,
    /// Producing agent, if recorded in the phase file.
    pub agent: Option<String>,
}

impl ShardWalker {
    /// Enumerate local shadow refs for a dialect (deterministic order). Local
    /// branches only — remote-tracking refs are structurally never iterated.
    ///
    /// # Errors
    /// [`RiftError::Substrate`] on a read failure.
    pub fn shadow_refs(&self, dialect: Dialect) -> Result<Vec<ShadowRef>> {
        let platform = self
            .repo()
            .references()
            .map_err(|e| RiftError::substrate(format!("open refs: {e}")))?;
        let iter = platform
            .local_branches()
            .map_err(|e| RiftError::substrate(format!("iter local branches: {e}")))?;
        let mut out = Vec::new();
        for r in iter {
            let r = r.map_err(|e| RiftError::substrate(format!("read ref: {e}")))?;
            let name = r.name().as_bstr().to_string();
            if let Some(mut s) = classify_shadow(&name, dialect) {
                let id = r
                    .into_fully_peeled_id()
                    .map_err(|e| RiftError::substrate(format!("peel shadow ref: {e}")))?;
                s.tip = id.detach().to_string();
                out.push(s);
            }
        }
        out.sort_by(|a, b| a.full_name.cmp(&b.full_name));
        Ok(out)
    }

    /// Enumerate per-session tags (`refs/tags/<dialect>/sessions/<id>`) → `(id, oid)`.
    ///
    /// # Errors
    /// [`RiftError::Substrate`] on a read failure.
    pub fn session_tags(&self, dialect: Dialect) -> Result<Vec<(String, String)>> {
        let ns = dialect.session_tag_ns();
        let platform = self
            .repo()
            .references()
            .map_err(|e| RiftError::substrate(format!("open refs: {e}")))?;
        let iter = platform
            .all()
            .map_err(|e| RiftError::substrate(format!("iter refs: {e}")))?;
        let mut out = Vec::new();
        for r in iter {
            let r = r.map_err(|e| RiftError::substrate(format!("read ref: {e}")))?;
            let name = r.name().as_bstr().to_string();
            if let Some(sid) = name.strip_prefix(ns) {
                if sid.is_empty() {
                    continue;
                }
                let sid = sid.to_string();
                let id = r
                    .into_fully_peeled_id()
                    .map_err(|e| RiftError::substrate(format!("peel session tag: {e}")))?;
                out.push((sid, id.detach().to_string()));
            }
        }
        out.sort();
        Ok(out)
    }

    /// Join session tags (both dialects) with the out-of-tree phase hints into a
    /// deterministic `SessionEntry` list keyed by `session_id`. A session present
    /// only as a phase hint (or only as a tag) is still included.
    ///
    /// The hints are read from `sessions`, the member's sessions directory,
    /// which the caller resolves: it lives in the family's run state, which
    /// this crate cannot locate.
    ///
    /// # Errors
    /// [`RiftError::Substrate`] on a read failure.
    pub fn session_index(&self, sessions: &Path) -> Result<Vec<SessionEntry>> {
        use std::collections::BTreeMap;
        let mut entries: BTreeMap<String, SessionEntry> = BTreeMap::new();
        for dialect in [Dialect::Rift, Dialect::Legacy] {
            for (sid, oid) in self.session_tags(dialect)? {
                entries.entry(sid.clone()).or_insert_with(|| SessionEntry {
                    session_id: sid.clone(),
                    dialect: None,
                    tag_oid: None,
                    phase: None,
                    agent: None,
                });
                if let Some(e) = entries.get_mut(&sid) {
                    e.dialect = Some(dialect);
                    e.tag_oid = Some(oid);
                }
            }
        }
        let (phases, _issues) = session::read_session_phases(sessions);
        for p in phases {
            let e = entries
                .entry(p.session_id.clone())
                .or_insert_with(|| SessionEntry {
                    session_id: p.session_id.clone(),
                    dialect: None,
                    tag_oid: None,
                    phase: None,
                    agent: None,
                });
            e.phase = Some(p.phase);
            e.agent = p.agent;
        }
        Ok(entries.into_values().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_valid_rift_shadow_ref() {
        let s = classify_shadow("refs/heads/rift/1a2b3c4-5d6e7f", Dialect::Rift).unwrap();
        assert_eq!(s.commit7, "1a2b3c4");
        assert_eq!(s.wt6, "5d6e7f");
    }

    #[test]
    fn rejects_remote_and_checkpoint_and_malformed() {
        assert!(
            classify_shadow("refs/remotes/origin/rift/1a2b3c4-5d6e7f", Dialect::Rift).is_none()
        );
        assert!(classify_shadow("refs/heads/rift/checkpoints/v1", Dialect::Rift).is_none());
        assert!(classify_shadow("refs/heads/rift/not-a-shadow", Dialect::Rift).is_none());
        assert!(classify_shadow("refs/heads/rift/zzzzzzz-5d6e7f", Dialect::Rift).is_none());
    }

    proptest::proptest! {
        /// Any `refs/remotes/*` name is never classified as a shadow ref.
        #[test]
        fn remote_names_never_classify(suffix in "[a-z0-9/_-]{0,40}") {
            let name = format!("refs/remotes/{suffix}");
            proptest::prop_assert!(classify_shadow(&name, Dialect::Rift).is_none());
        }
    }
}
