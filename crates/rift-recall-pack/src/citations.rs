//! The pack-local citation table — how an ordinal becomes an address again.
//!
//! An agent-facing pack names an artifact by a pack-local ordinal and never by
//! path (see [`crate::EvidenceRef`]). That closed the injection door and opened
//! a protocol question: a recommendation exists so the agent can *retrieve*
//! the thing, and a bare number is not something a server can resolve. The
//! answer is this table. Every compiled pack carries one, keyed by the pack's
//! own `PackKey` ([`crate::pack_key_for`]), mapping `ordinal → (path,
//! content_sha256)`.
//!
//! Three properties are the point, and each is enforced here rather than
//! documented and hoped for:
//!
//! * **It never rides the wire.** [`Citations`] is deliberately not
//!   `Serialize`, and [`crate::EvidencePack`] has no field for it. The table is
//!   the *server's* half of the exchange; if it travelled, the path would
//!   travel with it and naming by ordinal would be decorative.
//! * **Both halves of the address, or no row.** `Handle::File`'s own contract
//!   says a resolver must match path AND content address — the path alone is a
//!   name an attacker can create, the hash alone does not say which file it
//!   was — so a row that cannot state both is refused at [`Citations::cite`],
//!   not repaired later.
//! * **Ordinals are assigned once.** [`Citations::cite`] refuses to overwrite
//!   an ordinal it already holds. Degradation may DROP an entry; nothing may
//!   renumber one, and here that is a return value rather than a convention.

use std::collections::BTreeMap;

use crate::entry::{is_content_address, EVIDENCE_TAG};

/// The tag a citation target opens with: `pack:<key>/evidence:<n>`.
pub const PACK_TAG: &str = "pack:";

/// One row of a pack's citation table: **both halves** of a file revision's
/// address, as the record holds them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Citation {
    path: String,
    content_sha256: String,
}

impl Citation {
    /// The repo-relative path the record pairs with [`Self::content_sha256`].
    /// This is what the **existing** path-based policy gate is applied to — the
    /// whole reason the ordinal resolves to an address instead of the caller
    /// naming one.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// The content address that other half of the pair.
    #[must_use]
    pub fn content_sha256(&self) -> &str {
        &self.content_sha256
    }
}

/// A pack's ordinal → address table. Pack-local, server-side, never serialized.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Citations {
    rows: BTreeMap<u32, Citation>,
}

impl Citations {
    /// An empty table.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// File `ordinal → (path, content_sha256)`.
    ///
    /// Returns `false` — and records nothing — when the row cannot be trusted
    /// to mean what it says:
    ///
    /// * `path` is not a safe repo-relative path (absolute, `..`, or a control
    ///   character): such a path is never a legitimate repo artifact, and the
    ///   serve pass already drops those candidates before they are cited.
    /// * `content_sha256` is not a content address: half an address is not an
    ///   address, and a row that resolved on the path alone would be exactly
    ///   the hash-only regression this rule refuses in the other direction.
    /// * `ordinal` is already filed. **This is the no-renumbering guarantee**:
    ///   a table cannot be talked into pointing an already-published number at
    ///   a different artifact.
    pub fn cite(&mut self, ordinal: u32, path: &str, content_sha256: &str) -> bool {
        if !rift_recall_types::path_is_safe(path) || !is_content_address(content_sha256) {
            return false;
        }
        if self.rows.contains_key(&ordinal) {
            return false;
        }
        self.rows.insert(
            ordinal,
            Citation {
                path: path.to_owned(),
                content_sha256: content_sha256.to_owned(),
            },
        );
        true
    }

    /// The address filed under `ordinal`, if any. `None` is a **refusal**, not
    /// a fallback: an ordinal past the end of the fact set and an ordinal whose
    /// entry was dropped before it was ever cited are the same answer here, and
    /// deliberately so — distinguishing them would answer an attacker's real
    /// question.
    #[must_use]
    pub fn get(&self, ordinal: u32) -> Option<&Citation> {
        self.rows.get(&ordinal)
    }

    /// How many artifacts this pack can resolve.
    #[must_use]
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// Whether the table resolves nothing at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// The filed ordinals, ascending. Gaps are real: an entry the pass dropped
    /// leaves its number unused rather than shifting the ones behind it.
    pub fn ordinals(&self) -> impl Iterator<Item = u32> + '_ {
        self.rows.keys().copied()
    }
}

/// A retrieval target that names a pack-local citation:
/// `pack:<pack key>/evidence:<n>[@sha256:<content address>]`.
///
/// The pack key is what makes the ordinal meaningful — an ordinal is pack-local,
/// so a number with no pack behind it names nothing. The trailing content
/// address is optional and is **never** the authority: the table's row is. It
/// rides because it is already on the rendered line the agent read back, and
/// because the substrate layer resolves a blob by content address before the
/// dispatcher runs; a caller whose address disagrees with the table's is
/// refused rather than reconciled.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CitationTarget {
    /// The `PackKey` of the pack that published the ordinal.
    pub pack_key: String,
    /// The pack-local ordinal.
    pub ordinal: u32,
    /// The content address the caller believes the ordinal names, if it stated
    /// one.
    pub content_sha256: Option<String>,
}

impl CitationTarget {
    /// Parse a citation target. `None` for anything that is not exactly this
    /// shape — there is no lenient arm, because a target that "nearly" names a
    /// citation is a target whose meaning nobody agreed on.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        let rest = s.strip_prefix(PACK_TAG)?;
        let (pack_key, rest) = rest.split_once('/')?;
        if !is_content_address(pack_key) {
            return None;
        }
        let rest = rest.strip_prefix(EVIDENCE_TAG)?;
        let (digits, sha) = match rest.split_once("@sha256:") {
            Some((d, h)) => (d, Some(h)),
            None => (rest, None),
        };
        let ordinal: u32 = digits.parse().ok()?;
        if sha.is_some_and(|h| !is_content_address(h)) {
            return None;
        }
        Some(Self {
            pack_key: pack_key.to_owned(),
            ordinal,
            content_sha256: sha.map(str::to_owned),
        })
    }
}

impl core::fmt::Display for CitationTarget {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "{PACK_TAG}{}/{EVIDENCE_TAG}{}",
            self.pack_key, self.ordinal
        )?;
        match &self.content_sha256 {
            Some(h) => write!(f, "@sha256:{h}"),
            None => Ok(()),
        }
    }
}
