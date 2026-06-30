//! Local-only developer identity.
//!
//! Rift attributes captured sessions to a developer **locally and privately** —
//! no cloud account, no identity provider, no network (cloud identity is
//! Stage 7, explicitly out of scope here). Identity is derived from what is
//! already on the machine: the git author/email and a salted machine
//! fingerprint, both **hashed** so raw identifiers never reach a derived store
//! (`05-security` §5.7/§5.8). A session binds to a developer with an explicit
//! [`Confidence`]; ambiguity yields a low-confidence [`BindingBasis::Ambiguous`]
//! rather than a confident wrong guess — *fail honest, not confident*.
//!
//! All reads go through the injectable [`IdentitySource`] so tests supply
//! fixtures and never touch the real machine, clock, or git config.

use crate::id::{Blake3Id, SessionId};
use crate::time::Timestamp;

fn blake3_id(parts: &[&[u8]]) -> Blake3Id {
    let mut hasher = blake3::Hasher::new();
    for p in parts {
        hasher.update(p);
    }
    Blake3Id::from_bytes(*hasher.finalize().as_bytes())
}

/// A locally-derived developer identity: `blake3(install_salt ‖ "developer:" ‖
/// normalized_email)`. No cloud account; a *local convenience key*, not a
/// security principal (cross-scope access is still gated + audited in recall).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, ::serde::Serialize, ::serde::Deserialize)]
#[serde(transparent)]
pub struct DeveloperId(pub Blake3Id);

impl core::fmt::Display for DeveloperId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        core::fmt::Display::fmt(&self.0, f)
    }
}

/// A salted machine identity: `blake3(install_salt ‖ "machine:" ‖
/// raw_fingerprint)`. The **raw fingerprint is never persisted**; the per-install
/// salt prevents two installs on the same hardware producing a globally
/// correlatable id.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, ::serde::Serialize, ::serde::Deserialize)]
#[serde(transparent)]
pub struct MachineId(pub Blake3Id);

impl core::fmt::Display for MachineId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        core::fmt::Display::fmt(&self.0, f)
    }
}

/// A confidence score in the closed unit interval `[0.0, 1.0]`.
#[derive(Clone, Copy, PartialEq, PartialOrd, Debug, ::serde::Serialize, ::serde::Deserialize)]
pub struct Confidence(f32);

impl Confidence {
    /// Construct, clamping to `[0.0, 1.0]`.
    #[must_use]
    pub fn new(value: f32) -> Self {
        Self(value.clamp(0.0, 1.0))
    }

    /// The clamped value.
    #[must_use]
    pub fn value(self) -> f32 {
        self.0
    }
}

/// What evidence produced a [`SessionBinding`].
#[derive(Clone, Copy, PartialEq, Eq, Debug, ::serde::Serialize, ::serde::Deserialize)]
pub enum BindingBasis {
    /// Git author name + email both present and consistent (highest signal).
    GitAuthorMatch,
    /// An explicit environment override supplied the identity.
    EnvOverride,
    /// Only a machine default was available (e.g. detached/CI context).
    MachineDefault,
    /// No reliable signal / conflicting signals — bound at low confidence.
    Ambiguous,
}

impl BindingBasis {
    /// The default confidence this basis carries.
    #[must_use]
    pub fn default_confidence(self) -> Confidence {
        Confidence::new(match self {
            Self::EnvOverride => 0.99,
            Self::GitAuthorMatch => 0.90,
            Self::MachineDefault => 0.40,
            Self::Ambiguous => 0.10,
        })
    }
}

/// One observation of a developer's local identity (append-only fact).
#[derive(Clone, Debug, PartialEq, Eq, ::serde::Serialize, ::serde::Deserialize)]
pub struct IdentitySnapshot {
    /// The hashed developer identity.
    pub developer: DeveloperId,
    /// Git author name as configured (display only).
    pub git_author_name: String,
    /// Normalized (trimmed, lowercased) git email — the `DeveloperId` basis.
    pub git_email_normalized: String,
    /// The hashed machine identity (raw fingerprint never stored).
    pub machine: MachineId,
    /// Transaction-time of the observation.
    pub observed_at: Timestamp,
}

/// Binds a session to a developer + machine with an explicit confidence.
#[derive(Clone, Debug, PartialEq, ::serde::Serialize, ::serde::Deserialize)]
pub struct SessionBinding {
    /// The bound session.
    pub session_id: SessionId,
    /// The developer the session is attributed to.
    pub developer: DeveloperId,
    /// The machine the session ran on.
    pub machine: MachineId,
    /// How sure we are of the attribution.
    pub confidence: Confidence,
    /// What evidence produced this binding.
    pub basis: BindingBasis,
    /// Transaction-time the binding was recorded.
    pub recorded_at: Timestamp,
}

/// Injectable source of the local identity inputs. Production reads git config /
/// the OS; tests supply a [`FixtureIdentity`]. The trait has **no network
/// methods** — identity derivation is local-only by construction.
pub trait IdentitySource {
    /// Git `user.name`, if configured.
    fn git_author_name(&self) -> Option<String>;
    /// Git `user.email`, if configured.
    fn git_email(&self) -> Option<String>;
    /// A platform machine fingerprint (read transiently, never persisted raw).
    fn machine_fingerprint(&self) -> Option<String>;
    /// A per-install random salt (custodied in the OS keychain in production).
    fn install_salt(&self) -> [u8; 32];
    /// An explicit environment identity override, if any.
    fn env_developer_override(&self) -> Option<String> {
        None
    }
}

fn normalize_email(s: &str) -> String {
    s.trim().to_ascii_lowercase()
}

fn derive_developer(salt: &[u8; 32], normalized_email: &str) -> DeveloperId {
    DeveloperId(blake3_id(&[
        salt,
        b"developer:",
        normalized_email.as_bytes(),
    ]))
}

fn derive_machine(salt: &[u8; 32], fingerprint: &str) -> MachineId {
    MachineId(blake3_id(&[salt, b"machine:", fingerprint.as_bytes()]))
}

/// Observe the current local identity from `src` at time `at`.
pub fn snapshot(src: &impl IdentitySource, at: Timestamp) -> IdentitySnapshot {
    let salt = src.install_salt();
    let normalized = src
        .env_developer_override()
        .or_else(|| src.git_email())
        .as_deref()
        .map(normalize_email)
        .unwrap_or_default();
    // The raw fingerprint is read transiently and never stored.
    let fingerprint = src.machine_fingerprint().unwrap_or_default();
    IdentitySnapshot {
        developer: derive_developer(&salt, &normalized),
        git_author_name: src.git_author_name().unwrap_or_default(),
        git_email_normalized: normalized,
        machine: derive_machine(&salt, &fingerprint),
        observed_at: at,
    }
}

/// Bind a session to a developer, choosing an honest basis + confidence.
pub fn bind_session(
    src: &impl IdentitySource,
    session_id: SessionId,
    at: Timestamp,
) -> SessionBinding {
    let snap = snapshot(src, at);
    let basis = if src.env_developer_override().is_some() {
        BindingBasis::EnvOverride
    } else if src.git_email().is_some() && src.git_author_name().is_some() {
        BindingBasis::GitAuthorMatch
    } else if src.machine_fingerprint().is_some() {
        BindingBasis::MachineDefault
    } else {
        BindingBasis::Ambiguous
    };
    SessionBinding {
        session_id,
        developer: snap.developer,
        machine: snap.machine,
        confidence: basis.default_confidence(),
        basis,
        recorded_at: at,
    }
}

/// A deterministic [`IdentitySource`] for tests (and for downstream crates'
/// tests). Construct with explicit fields; no real machine/git/clock access.
#[derive(Clone, Debug, Default)]
pub struct FixtureIdentity {
    /// Optional git author name.
    pub name: Option<String>,
    /// Optional git email.
    pub email: Option<String>,
    /// Optional machine fingerprint.
    pub fingerprint: Option<String>,
    /// The fixed per-install salt.
    pub salt: [u8; 32],
    /// Optional environment override.
    pub env_override: Option<String>,
}

impl IdentitySource for FixtureIdentity {
    fn git_author_name(&self) -> Option<String> {
        self.name.clone()
    }
    fn git_email(&self) -> Option<String> {
        self.email.clone()
    }
    fn machine_fingerprint(&self) -> Option<String> {
        self.fingerprint.clone()
    }
    fn install_salt(&self) -> [u8; 32] {
        self.salt
    }
    fn env_developer_override(&self) -> Option<String> {
        self.env_override.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::Ulid;

    fn alice() -> FixtureIdentity {
        FixtureIdentity {
            name: Some("Alice".into()),
            email: Some("Alice@Example.com ".into()),
            fingerprint: Some("machine-guid-1".into()),
            salt: [9u8; 32],
            env_override: None,
        }
    }

    #[test]
    fn developer_id_from_normalized_email() {
        let a = snapshot(&alice(), Timestamp::from_millis(1));
        let mut alice2 = alice();
        alice2.email = Some("alice@example.com".into()); // different casing/spacing
        let b = snapshot(&alice2, Timestamp::from_millis(2));
        assert_eq!(
            a.developer, b.developer,
            "normalization must collapse casing"
        );

        let mut bob = alice();
        bob.email = Some("bob@example.com".into());
        let c = snapshot(&bob, Timestamp::from_millis(3));
        assert_ne!(a.developer, c.developer, "different email → different id");
    }

    #[test]
    fn machine_id_is_salted_hash_never_raw() {
        let snap = snapshot(&alice(), Timestamp::from_millis(1));
        let dbg = format!("{snap:?}");
        assert!(
            !dbg.contains("machine-guid-1"),
            "raw fingerprint must not appear"
        );

        let mut other_salt = alice();
        other_salt.salt = [1u8; 32];
        let snap2 = snapshot(&other_salt, Timestamp::from_millis(1));
        assert_ne!(
            snap.machine, snap2.machine,
            "salt change must change MachineId"
        );
    }

    #[test]
    fn binding_high_confidence_on_git_author_match() {
        let b = bind_session(
            &alice(),
            SessionId::new(Ulid::from_u128(1)),
            Timestamp::from_millis(1),
        );
        assert_eq!(b.basis, BindingBasis::GitAuthorMatch);
        assert!(b.confidence.value() >= 0.9);
    }

    #[test]
    fn ambiguous_identity_yields_low_confidence_not_guess() {
        let nobody = FixtureIdentity {
            salt: [0u8; 32],
            ..Default::default()
        };
        let b = bind_session(
            &nobody,
            SessionId::new(Ulid::from_u128(1)),
            Timestamp::from_millis(1),
        );
        assert_eq!(b.basis, BindingBasis::Ambiguous);
        assert!(b.confidence.value() <= 0.2);
    }

    #[test]
    fn env_override_takes_precedence() {
        let mut src = alice();
        src.env_override = Some("ci-bot@example.com".into());
        let b = bind_session(
            &src,
            SessionId::new(Ulid::from_u128(1)),
            Timestamp::from_millis(1),
        );
        assert_eq!(b.basis, BindingBasis::EnvOverride);
        assert!(b.confidence.value() > 0.95);
    }

    proptest::proptest! {
        /// Confidence is always within the unit interval for any evidence.
        #[test]
        fn confidence_in_unit_interval(v in -10.0f32..10.0) {
            let c = Confidence::new(v);
            proptest::prop_assert!((0.0..=1.0).contains(&c.value()));
        }

        /// Identity derivation is a pure function of (normalized email, salt):
        /// no hidden clock/state dependence.
        #[test]
        fn same_inputs_same_identity(email in "[a-z]{1,12}@[a-z]{1,8}\\.com", salt_byte in proptest::num::u8::ANY) {
            let src = FixtureIdentity {
                email: Some(email),
                salt: [salt_byte; 32],
                ..Default::default()
            };
            let a = snapshot(&src, Timestamp::from_millis(1));
            let b = snapshot(&src, Timestamp::from_millis(999));
            proptest::prop_assert_eq!(a.developer, b.developer);
        }
    }
}
