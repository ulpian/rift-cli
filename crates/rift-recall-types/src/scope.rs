//! Query scope — containment by default (Stage 3 Phase 2 Step 6, weakness W14).
//!
//! Every recall query (metrics, ranking, serving) is bounded to the current
//! repo / branch ancestry / developer / last-N checkpoints. Cross-scope access
//! is **not** a free enum variant — it requires an explicit [`CrossScopeGrant`]
//! that carries a `reason` (audited), so widening scope is a deliberate, loud
//! act and never an accident.

/// Which branch(es) are in scope.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BranchScope {
    /// The current branch and its ancestry (the safe default).
    CurrentAndAncestry,
    /// Exactly one named branch.
    Exact(String),
}

/// Which developer's sessions are in scope.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeveloperScope {
    /// Only the current developer (the safe default).
    CurrentOnly,
    /// A specific developer id.
    Specific(String),
}

/// How many checkpoints are in scope.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckpointWindow {
    /// The most recent `n` checkpoints (by walk ordinal).
    LastN(u32),
    /// Every checkpoint.
    All,
}

/// A cross-scope capability. Constructible **only** via [`CrossScopeGrant::grant`]
/// (which requires a non-empty `reason`); [`CrossScopeGrant::none`] is the
/// default and grants nothing. Fields are private so the only way to set a flag
/// is the reason-bearing constructor.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CrossScopeGrant {
    branches: bool,
    developers: bool,
    repos: bool,
    reason: Option<String>,
}

impl CrossScopeGrant {
    /// No cross-scope access (the default).
    #[must_use]
    pub fn none() -> Self {
        Self::default()
    }

    /// Grant cross-scope access with an audited `reason`. A blank reason grants
    /// nothing (cross-scope is reason-required by construction).
    #[must_use]
    pub fn grant(reason: impl Into<String>, branches: bool, developers: bool, repos: bool) -> Self {
        let reason = reason.into();
        if reason.trim().is_empty() {
            return Self::none();
        }
        Self {
            branches,
            developers,
            repos,
            reason: Some(reason),
        }
    }

    /// Whether cross-branch access was granted.
    #[must_use]
    pub fn branches(&self) -> bool {
        self.branches
    }
    /// Whether cross-developer access was granted.
    #[must_use]
    pub fn developers(&self) -> bool {
        self.developers
    }
    /// Whether cross-repo access was granted.
    #[must_use]
    pub fn repos(&self) -> bool {
        self.repos
    }
    /// The audited reason, if any cross-scope access was granted.
    #[must_use]
    pub fn reason(&self) -> Option<&str> {
        self.reason.as_deref()
    }
    /// Whether any cross-scope axis is granted.
    #[must_use]
    pub fn is_any(&self) -> bool {
        self.branches || self.developers || self.repos
    }
}

/// The default query containment. A query never returns rows outside this scope
/// unless [`CrossScopeGrant`] widens the relevant axis.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Scope {
    /// The repo this scope is pinned to (always).
    pub repo_id: String,
    /// Branch axis.
    pub branch: BranchScope,
    /// Developer axis.
    pub developer: DeveloperScope,
    /// Checkpoint window.
    pub checkpoints: CheckpointWindow,
    /// Cross-scope grant (none by default).
    pub cross: CrossScopeGrant,
}

impl Scope {
    /// The **safe** default: current repo, current branch + ancestry, current
    /// developer only, the last `last_n` checkpoints, no cross-scope.
    #[must_use]
    pub fn default_for(repo_id: impl Into<String>, last_n: u32) -> Self {
        Self {
            repo_id: repo_id.into(),
            branch: BranchScope::CurrentAndAncestry,
            developer: DeveloperScope::CurrentOnly,
            checkpoints: CheckpointWindow::LastN(last_n),
            cross: CrossScopeGrant::none(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_scope_has_no_cross_grant() {
        let s = Scope::default_for("repo1", 50);
        assert_eq!(s.developer, DeveloperScope::CurrentOnly);
        assert!(!s.cross.is_any());
        assert!(s.cross.reason().is_none());
    }

    #[test]
    fn cross_scope_requires_a_reason() {
        // a blank reason grants nothing (reason-required by construction)
        let blank = CrossScopeGrant::grant("  ", false, true, false);
        assert!(!blank.is_any());
        // a real reason widens and is recorded for audit
        let granted = CrossScopeGrant::grant("reviewing teammate branch", false, true, false);
        assert!(granted.developers());
        assert_eq!(granted.reason(), Some("reviewing teammate branch"));
    }
}
