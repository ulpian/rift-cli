//! The column-3 feeds (the data-plane split's Tokens/Nic/Recall trio): scoped, read-only
//! computations from a project's own projections into the six section
//! view-models.
//!
//! Every feed emits **full section state** (never a delta): a dropped or
//! duplicated delivery therefore converges on the fresh-read truth by
//! construction — the delivery-robustness property is a property of the
//! payload shape, not of channel heroics. Feeds follow the **active**
//! project (the reducer's `SetActive` is the subscribe/unsubscribe request:
//! selecting a project subscribes it to all three kinds; switching away
//! unsubscribes — the settled reading of the data plane's
//! subscribe/unsubscribe language), and the worker recomputes them on every
//! observed change of that project — a `Rebuilt` epoch bump re-bootstraps by
//! recomputation.
//!
//! Scope: every query runs against the project's **own** databases — its
//! repository family's, resolved through `project_family` (family identity), so
//! two checkouts of one repository read one store and two repositories never
//! read each other's. Cross-project spend cannot leak into a panel; the scope
//! property is tested over multi-repo fixtures.
//!
//! One module per card, because a card is the unit that has to stay coherent:
//! its own relations, its own honest-absence states, and its own tests beside
//! them. This module keeps only what every card needs — the family identity
//! each read is scoped by — and re-exports the feed entry points, so a caller
//! still names `feeds::compute_tokens` and cannot tell where the read lives.

mod activity;
mod calls;
mod savings;
mod system;
mod tokens;
mod window;

#[cfg(test)]
mod fixtures;

use std::path::Path;

use rift_git::{Family, FamilyState, MintPolicy};

pub use self::activity::compute_activity;
pub use self::savings::compute_recall;
pub use self::system::{compute_system, read_active_sessions, ActiveRead};
pub use self::tokens::compute_tokens;

/// The repository **family** a registered project belongs to (family identity).
///
/// The dashboard reads a project's stores through its family, so a card opened
/// on a linked worktree shows the repository's checkpoints, sessions and
/// savings instead of the empty pair the member's own path used to key — the
/// measured symptom family identity exists for (0 checkpoints from
/// `~/rift-worktrees/canopy` while the family's refs held 134).
///
/// Minting is allowed here, and the reason is that it changes nothing it does
/// not fix: the mint **adopts** the main work tree's historical ids, so a main
/// path resolves to byte-identically the file the dashboard already read, and a
/// vantage that cannot prove its main writes nothing at all. Refusing to mint
/// instead would blank every card until some other verb happened to run.
///
/// `None` on every failure — not a repository, no provable main, an unreadable
/// manifest. A card is a read, and an unresolvable identity is an absent fact,
/// never a broken panel.
pub(crate) fn project_family(path: &Path) -> Option<Family> {
    match rift_git::family_of(path, MintPolicy::Allow) {
        Ok((_, FamilyState::Minted(family))) => Some(family),
        _ => None,
    }
}
