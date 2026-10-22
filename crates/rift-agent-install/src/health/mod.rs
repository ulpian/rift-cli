//! Integration health: whether each wired agent's capture is actually
//! arriving, computed from facts the caller already holds.
//!
//! A separate module because it answers a different question from the rest of
//! the crate. Wiring says what Rift wrote; health says whether what it wrote is
//! being run and whether what runs reaches the ledger. The answers are
//! findings with a closed reason vocabulary, and every front end prints the
//! same ones, so the vocabulary lives beside the installer both front ends
//! already read rather than in either of them.
//!
//! Nothing here writes a byte. The caller gathers the facts (the family's
//! session caches, the projection it opened read-only, Codex's trust, the
//! wired hook binaries and the binary of record) and [`assess`] is a function
//! of them and a clock value, so every detector is testable without a machine.
//! A finding is computed when it is asked for and never stored.

mod behind;
mod binary;
mod codex;
mod lifecycle;
pub(crate) mod remedy;
pub(crate) mod surface;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use rift_shadow_hooks::session_cache::SessionCache;
use rift_shadow_hooks::AdapterRegistry;
use rift_store::SqliteStore;
use serde::{Deserialize, Serialize};

use crate::native::{codex_trust, hook_file, installed_hook_binary, CodexTrust};

pub use behind::{is_behind_candidate, sessions_behind_events};
pub use remedy::{hook_composition_sentence, remedies, remedy, stale_setup_plan_sentence, Remedy};
pub use surface::{client_surface, Surface, SurfaceEvidence};

/// Why an integration is unhealthy. Closed: a new reason is a new variant, and
/// the stable strings are the `--json` contract.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Reason {
    /// Codex is skipping at least one of Rift's hooks: modified or untrusted.
    CodexHooksSkipped,
    /// Rift cannot reproduce Codex's trust answer: an unproven Codex version,
    /// or no recorded Codex session to name one.
    CodexTrustUnverifiable,
    /// The recorded Codex is newer than every version the trust check is
    /// proven against: a separate warning, because a client update is not a
    /// sign that the user withdrew trust.
    CodexVersionUnsupported,
    /// An agent's newest recent session holds tool events but neither a
    /// native session-opening nor a prompt event.
    LifecycleHooksMissing,
    /// The binary an agent's hook file runs is absent or not executable.
    HookBinaryMissing,
    /// The agents' hooks run a binary other than the family's binary of
    /// record, both present.
    HookBinaryDisagrees,
    /// A session recorded ended or idle while its own events kept arriving.
    SessionBehindEvents,
}

/// How much a finding matters. A warning names a fact worth knowing while
/// nothing is being lost; an error is capture being lost, or Rift's own state
/// being wrong.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Severity {
    /// Capture is being lost, or Rift's own state is wrong.
    Error,
    /// Worth knowing; nothing is being lost.
    Warning,
}

/// One unhealthy fact about one agent's integration.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    /// Why.
    pub reason: Reason,
    /// How much it matters; always [`Reason::severity`] of `reason`.
    pub severity: Severity,
    /// The agent's registry token.
    pub agent: String,
    /// The session the finding is about, when it is about one.
    pub session: Option<String>,
    /// Identifiers, paths, counts and event or verb names only — never payload
    /// bytes, because a finding is printed and serialised.
    pub detail: String,
}

/// Everything the detectors read, gathered by the caller.
///
/// Public fields because there is no invariant between them: a test builds
/// one directly, and a front end fills it with [`Facts::gather`].
pub struct Facts<'a> {
    /// The family's session caches, as the caller read them.
    pub sessions: &'a [SessionCache],
    /// The family's projection, opened read-only by the caller; `None` when
    /// there is none yet.
    pub projection: Option<&'a SqliteStore>,
    /// Codex's trust in Rift's hooks; `None` when no Codex hook file holds a
    /// Rift handler.
    pub codex_trust: Option<CodexTrust>,
    /// Per agent token, the binary its Rift-written hook file runs.
    pub hook_binaries: BTreeMap<String, PathBuf>,
    /// The family's binary of record, when one is recorded.
    pub binary_of_record: Option<PathBuf>,
}

impl Reason {
    /// Every reason, in the order findings sort by.
    pub const ALL: [Self; 7] = [
        Self::CodexHooksSkipped,
        Self::CodexTrustUnverifiable,
        Self::CodexVersionUnsupported,
        Self::LifecycleHooksMissing,
        Self::HookBinaryMissing,
        Self::HookBinaryDisagrees,
        Self::SessionBehindEvents,
    ];

    /// The stable string, the same one serde writes.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CodexHooksSkipped => "codex-hooks-skipped",
            Self::CodexTrustUnverifiable => "codex-trust-unverifiable",
            Self::CodexVersionUnsupported => "codex-version-unsupported",
            Self::LifecycleHooksMissing => "lifecycle-hooks-missing",
            Self::HookBinaryMissing => "hook-binary-missing",
            Self::HookBinaryDisagrees => "hook-binary-disagrees",
            Self::SessionBehindEvents => "session-behind-events",
        }
    }

    /// The reason's one severity. The warnings are the cases where both
    /// binaries still run and where Codex's answer is unknown: nothing is
    /// being lost that Rift can show.
    #[must_use]
    pub const fn severity(self) -> Severity {
        match self {
            Self::CodexTrustUnverifiable
            | Self::CodexVersionUnsupported
            | Self::HookBinaryDisagrees => Severity::Warning,
            Self::CodexHooksSkipped
            | Self::LifecycleHooksMissing
            | Self::HookBinaryMissing
            | Self::SessionBehindEvents => Severity::Error,
        }
    }
}

impl Severity {
    /// The stable string, the same one serde writes.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warning => "warning",
        }
    }
}

impl Finding {
    /// A finding whose severity follows from its reason, so the two can never
    /// disagree.
    #[must_use]
    pub fn new(reason: Reason, agent: &str, session: Option<&str>, detail: String) -> Self {
        Self {
            reason,
            severity: reason.severity(),
            agent: agent.to_owned(),
            session: session.map(str::to_owned),
            detail,
        }
    }
}

impl<'a> Facts<'a> {
    /// The facts for one repository: its hook files, its family's binary of
    /// record and Codex's trust, read here; the session caches and the
    /// projection, which the caller already holds, passed through.
    ///
    /// One function so the command line and the dashboard gather the same
    /// facts the same way. Reads only: the hook files and `binary.json` are
    /// parsed, Codex's home is read when given, and nothing is opened for
    /// writing.
    #[must_use]
    pub fn gather(
        repo: &Path,
        family_dir: &Path,
        codex_home: Option<&Path>,
        sessions: &'a [SessionCache],
        projection: Option<&'a SqliteStore>,
    ) -> Self {
        let hook_binaries = AdapterRegistry::with_builtin()
            .agents()
            .iter()
            .map(rift_shadow_hooks::AgentId::as_str)
            .filter(|token| hook_file(token).is_some())
            .filter_map(|token| {
                installed_hook_binary(repo, token).map(|binary| (token.to_owned(), binary))
            })
            .collect();
        Self {
            sessions,
            projection,
            codex_trust: codex_trust(repo, codex_home),
            hook_binaries,
            binary_of_record: rift_shadow_hooks::read_binary_of_record(family_dir)
                .map(|record| record.path().to_path_buf()),
        }
    }
}

/// Every finding over `facts` as of `now_ms`, sorted by reason, then agent,
/// then session, so two runs over the same facts print the same lines in the
/// same order whatever order the caches were read in.
#[must_use]
pub fn assess(facts: &Facts<'_>, now_ms: i64) -> Vec<Finding> {
    let mut findings = codex::codex_findings(facts.codex_trust.as_ref());
    findings.extend(binary::binary_findings(
        &facts.hook_binaries,
        facts.binary_of_record.as_deref(),
    ));
    if let Some(projection) = facts.projection {
        findings.extend(lifecycle::lifecycle_findings(
            projection,
            facts.sessions,
            now_ms,
        ));
        findings.extend(sessions_behind_events(projection, facts.sessions, now_ms));
    }
    findings.sort_by(|a, b| {
        (a.reason, &a.agent, &a.session, &a.detail)
            .cmp(&(b.reason, &b.agent, &b.session, &b.detail))
    });
    findings
}

/// The earliest event time inside the alarm window as of `now_ms`: the
/// doctor's day, one constant, so the idle alarm and these detectors can never
/// disagree about how long "recent" is.
fn window_start(now_ms: i64) -> i64 {
    now_ms.saturating_sub(rift_shadow_hooks::session_cache::IDLE_TIMEOUT_MS)
}

/// A short-lived runtime for the store seam's async queries. `None` when one
/// cannot be built, which the detectors read as "nothing to report".
fn runtime() -> Option<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_current_thread().build().ok()
}
