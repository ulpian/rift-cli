//! Strongly-typed identifiers.
//!
//! Ids of different kinds are distinct types so they cannot be confused at
//! compile time (a [`SessionId`] is never usable as a [`TurnId`]; a
//! [`Blake3Id`] is never comparable to a [`Sha256Id`]). Two families:
//!
//! * **Content-addressed** ([`Sha256Id`], [`Blake3Id`] and the semantic wrappers
//!   [`CheckpointId`], [`FileRevisionId`], [`SummaryId`]) — derived from a content
//!   hash, so re-ingesting the same fact is idempotent (a CRDT-like set union;
//!   see `04-database-design` §4.4). SHA-256 is used where substrate
//!   byte-compatibility matters; blake3 for internal handles.
//! * **Time-ordered surrogate** ([`Ulid`] and the wrappers [`SessionId`],
//!   [`TurnId`], [`CallId`], [`ToolCallId`]) — sortable, coordinator-free. ULID
//!   generation is injectable via [`UlidSource`] so tests never read the clock
//!   (see `06-testing` §6.3 determinism).
//!
//! Every id validates on construction *and* on deserialization (`serde`
//! `try_from`), so an invalid id can never enter the system — not from code, not
//! from a config file, not from a checkpoint artifact.

use core::fmt::Write as _;
use core::sync::atomic::{AtomicU64, Ordering};

use crate::RiftError;
use sha2::{Digest as _, Sha256};

// --- hex helpers (no external `hex` dep; keeps rift-core dependency-light) ----

fn encode_hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        // Writing to a String is infallible.
        let _ = write!(s, "{b:02x}");
    }
    s
}

fn hex_val(b: u8) -> crate::Result<u8> {
    match b {
        b'0'..=b'9' => Ok(b - b'0'),
        b'a'..=b'f' => Ok(b - b'a' + 10),
        b'A'..=b'F' => Ok(b - b'A' + 10),
        other => Err(RiftError::validity(format!(
            "non-hex character {:?} in identifier",
            char::from(other)
        ))),
    }
}

fn decode_hex_32(kind: &str, s: &str) -> crate::Result<[u8; 32]> {
    if s.len() != 64 {
        return Err(RiftError::validity(format!(
            "{kind} must be 64 hex chars, got {}",
            s.len()
        )));
    }
    let mut out = [0u8; 32];
    for (slot, pair) in out.iter_mut().zip(s.as_bytes().chunks_exact(2)) {
        if let [hi, lo] = pair {
            *slot = (hex_val(*hi)? << 4) | hex_val(*lo)?;
        }
    }
    Ok(out)
}

/// Derive the canonical capture-session row id from an adapter name and its
/// native session id.
///
/// This is the single identity authority shared by capture projection and
/// Canopy. Its encoding is byte-identical to the original `rift-shadow`
/// derivation so existing projection rows remain addressable.
#[must_use]
pub fn capture_session_row_id(agent: &str, native_session_id: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(b"session\0");
    hash.update(agent.as_bytes());
    hash.update(b"\0");
    hash.update(native_session_id.as_bytes());
    encode_hex(&hash.finalize())
}

// --- content-addressed byte ids ---------------------------------------------

macro_rules! byte_id {
    ($(#[$m:meta])* $name:ident, $algo:literal) => {
        $(#[$m])*
        #[derive(Clone, Copy, PartialEq, Eq, Hash, ::serde::Serialize, ::serde::Deserialize)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name([u8; 32]);

        impl $name {
            /// Construct from raw 32 bytes (a hash digest).
            #[must_use]
            pub const fn from_bytes(bytes: [u8; 32]) -> Self {
                Self(bytes)
            }

            /// The raw 32-byte digest.
            #[must_use]
            pub const fn as_bytes(&self) -> &[u8; 32] {
                &self.0
            }

            /// Lowercase hex encoding (64 chars).
            #[must_use]
            pub fn to_hex(&self) -> String {
                encode_hex(&self.0)
            }

            /// Parse from a 64-char hex string (case-insensitive).
            ///
            /// # Errors
            /// [`RiftError::Validity`] if not exactly 64 hex characters.
            pub fn parse_hex(s: &str) -> $crate::Result<Self> {
                Ok(Self(decode_hex_32(stringify!($name), s)?))
            }

            /// The hash algorithm this id is bound to (informational).
            #[must_use]
            pub const fn algorithm() -> &'static str {
                $algo
            }
        }

        impl ::core::fmt::Display for $name {
            fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                f.write_str(&self.to_hex())
            }
        }

        impl ::core::fmt::Debug for $name {
            fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                write!(f, "{}({})", stringify!($name), self.to_hex())
            }
        }

        impl ::core::str::FromStr for $name {
            type Err = $crate::RiftError;
            fn from_str(s: &str) -> $crate::Result<Self> {
                Self::parse_hex(s)
            }
        }

        impl ::core::convert::TryFrom<String> for $name {
            type Error = $crate::RiftError;
            fn try_from(s: String) -> $crate::Result<Self> {
                Self::parse_hex(&s)
            }
        }

        impl ::core::convert::From<$name> for String {
            fn from(v: $name) -> Self {
                v.to_hex()
            }
        }

        impl $crate::immutable::Immutable for $name {}
    };
}

byte_id!(
    /// A SHA-256 content address. SHA-256 is used where **substrate
    /// byte-compatibility** is required (the checkpoint `content_hash.txt` and
    /// the tamper chain are SHA-256). Not interchangeable with [`Blake3Id`].
    ///
    /// The SHA-256 (chain/substrate) vs blake3 (internal handle) role split that
    /// `05-security` §5.4 / the hash-chain step mandate is enforced by these being
    /// **distinct types with no conversion** — a blake3 handle can never
    /// masquerade as a chain hash:
    ///
    /// ```compile_fail
    /// let b = rift_core::Blake3Id::from_bytes([0u8; 32]);
    /// let _s: rift_core::Sha256Id = b;
    /// ```
    Sha256Id,
    "sha256"
);
byte_id!(
    /// A blake3 content address — for fast **internal** handles and worktree
    /// hashing. Not interchangeable with [`Sha256Id`].
    Blake3Id,
    "blake3"
);

// --- ULID surrogate + injectable source -------------------------------------

/// A 128-bit time-ordered identifier: 48 high bits of millisecond timestamp,
/// 80 low bits of randomness/counter. Lexicographically sortable in its
/// fixed-width hex form, so ordering by id is ordering by creation time.
#[derive(
    Clone, Copy, PartialEq, Eq, Hash, Ord, PartialOrd, ::serde::Serialize, ::serde::Deserialize,
)]
#[serde(try_from = "String", into = "String")]
pub struct Ulid(u128);

impl Ulid {
    /// Construct from a raw `u128`.
    #[must_use]
    pub const fn from_u128(v: u128) -> Self {
        Self(v)
    }

    /// The raw `u128`.
    #[must_use]
    pub const fn to_u128(self) -> u128 {
        self.0
    }

    /// Compose from a millisecond timestamp (low 48 bits used) and 80 bits of
    /// randomness/counter (low 80 bits used).
    #[must_use]
    pub const fn from_parts(timestamp_ms: u64, randomness: u128) -> Self {
        let t = ((timestamp_ms as u128) & ((1u128 << 48) - 1)) << 80;
        let r = randomness & ((1u128 << 80) - 1);
        Self(t | r)
    }

    /// The embedded millisecond timestamp (high 48 bits).
    #[must_use]
    pub const fn timestamp_ms(self) -> u64 {
        (self.0 >> 80) as u64
    }

    /// Fixed-width (32 char) lowercase hex form — sortable and round-trippable.
    #[must_use]
    pub fn to_hex(self) -> String {
        format!("{:032x}", self.0)
    }

    /// Parse from the 32-char hex form.
    ///
    /// # Errors
    /// [`RiftError::Validity`] if not exactly 32 hex characters.
    pub fn parse_hex(s: &str) -> crate::Result<Self> {
        if s.len() != 32 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(RiftError::validity(format!(
                "Ulid must be 32 hex chars, got {:?}",
                s.len()
            )));
        }
        u128::from_str_radix(s, 16)
            .map(Self)
            .map_err(|e| RiftError::validity(format!("invalid Ulid: {e}")))
    }
}

impl core::fmt::Display for Ulid {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.to_hex())
    }
}
impl core::fmt::Debug for Ulid {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "Ulid({})", self.to_hex())
    }
}
impl core::str::FromStr for Ulid {
    type Err = RiftError;
    fn from_str(s: &str) -> crate::Result<Self> {
        Self::parse_hex(s)
    }
}
impl core::convert::TryFrom<String> for Ulid {
    type Error = RiftError;
    fn try_from(s: String) -> crate::Result<Self> {
        Self::parse_hex(&s)
    }
}
impl core::convert::From<Ulid> for String {
    fn from(v: Ulid) -> Self {
        v.to_hex()
    }
}
impl crate::immutable::Immutable for Ulid {}

/// A source of fresh [`Ulid`]s. Injected so production uses wall-clock + a
/// monotonic counter while tests use a deterministic counter (no clock).
pub trait UlidSource {
    /// Produce the next, strictly-increasing ULID.
    fn next_ulid(&self) -> Ulid;
}

/// Production ULID source: millisecond clock in the high bits, a per-process
/// counter in the low bits, with a monotonic floor so a clock that steps
/// backwards still yields strictly-increasing ids.
#[derive(Debug, Default)]
pub struct MonotonicUlidSource {
    counter: AtomicU64,
    last: std::sync::Mutex<u128>,
}

impl MonotonicUlidSource {
    /// A fresh source.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl UlidSource for MonotonicUlidSource {
    fn next_ulid(&self) -> Ulid {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
        let c = u128::from(self.counter.fetch_add(1, Ordering::Relaxed));
        let candidate = Ulid::from_parts(now_ms, c).to_u128();
        // Recover a poisoned lock rather than panicking in a capture path.
        let mut guard = self
            .last
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let next = candidate.max(guard.wrapping_add(1));
        *guard = next;
        Ulid::from_u128(next)
    }
}

/// Deterministic ULID source for tests: a strictly-increasing counter, no clock.
#[derive(Debug)]
pub struct CounterUlidSource {
    next: AtomicU64,
}

impl CounterUlidSource {
    /// Start emitting ids from `start` (then `start+1`, …).
    #[must_use]
    pub fn starting_at(start: u64) -> Self {
        Self {
            next: AtomicU64::new(start),
        }
    }
}

impl Default for CounterUlidSource {
    fn default() -> Self {
        Self::starting_at(1)
    }
}

impl UlidSource for CounterUlidSource {
    fn next_ulid(&self) -> Ulid {
        Ulid::from_u128(u128::from(self.next.fetch_add(1, Ordering::Relaxed)))
    }
}

// --- semantic newtypes ------------------------------------------------------

macro_rules! ulid_newtype {
    ($(#[$m:meta])* $name:ident) => {
        $(#[$m])*
        #[derive(Clone, Copy, PartialEq, Eq, Hash, Ord, PartialOrd, Debug, ::serde::Serialize, ::serde::Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub Ulid);

        impl $name {
            /// Wrap a raw [`Ulid`].
            #[must_use]
            pub const fn new(u: Ulid) -> Self { Self(u) }
            /// The underlying [`Ulid`].
            #[must_use]
            pub const fn ulid(self) -> Ulid { self.0 }
        }
        impl ::core::fmt::Display for $name {
            fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                ::core::fmt::Display::fmt(&self.0, f)
            }
        }
        impl ::core::str::FromStr for $name {
            type Err = $crate::RiftError;
            fn from_str(s: &str) -> $crate::Result<Self> { Ulid::parse_hex(s).map(Self) }
        }
        impl $crate::immutable::Immutable for $name {}
    };
}

macro_rules! sha_newtype {
    ($(#[$m:meta])* $name:ident) => {
        $(#[$m])*
        #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, ::serde::Serialize, ::serde::Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub Sha256Id);

        impl $name {
            /// Wrap a content-addressed [`Sha256Id`].
            #[must_use]
            pub const fn new(id: Sha256Id) -> Self { Self(id) }
            /// The underlying [`Sha256Id`].
            #[must_use]
            pub const fn sha256(&self) -> &Sha256Id { &self.0 }
        }
        impl ::core::fmt::Display for $name {
            fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                ::core::fmt::Display::fmt(&self.0, f)
            }
        }
        impl ::core::str::FromStr for $name {
            type Err = $crate::RiftError;
            fn from_str(s: &str) -> $crate::Result<Self> { Sha256Id::parse_hex(s).map(Self) }
        }
        impl $crate::immutable::Immutable for $name {}
    };
}

ulid_newtype!(
    /// Identifies a capture session (ULID).
    SessionId
);
ulid_newtype!(
    /// Identifies a turn within a session (ULID).
    TurnId
);
ulid_newtype!(
    /// Identifies a model call within a turn (ULID).
    CallId
);
ulid_newtype!(
    /// Identifies a tool call within a turn (ULID).
    ToolCallId
);
ulid_newtype!(
    /// Identifies a project the Canopy dashboard watches (ULID). Assigned once at
    /// registration; the append-only `canopy_project_event` history keys every
    /// later correction (remove/relabel/reorder) by this id.
    ProjectId
);

sha_newtype!(
    /// Identifies a checkpoint on the `rift/checkpoints/v1` branch
    /// (content-addressed; SHA-256 for substrate compatibility).
    CheckpointId
);
sha_newtype!(
    /// Identifies a file revision (`path@content_sha256`): one immutable fact.
    FileRevisionId
);
sha_newtype!(
    /// Identifies a hash-bound summary (content-addressed).
    SummaryId
);

// --- repo id (string slug) --------------------------------------------------

/// A repository identifier — a stable slug derived from the repo's identity
/// (e.g. a hash of its canonical path or origin). String-shaped because it keys
/// the per-repo projection directory `~/.cache/rift/<repo_id>/`.
#[derive(Clone, PartialEq, Eq, Hash, Debug, ::serde::Serialize, ::serde::Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct RepoId(String);

impl RepoId {
    /// Construct, validating the slug is non-empty, `<= 128` bytes, and limited
    /// to `[A-Za-z0-9_.:-]` (filesystem- and key-name-safe).
    ///
    /// # Errors
    /// [`RiftError::Validity`] on empty/overlong/invalid input.
    pub fn new(value: impl Into<String>) -> crate::Result<Self> {
        let value = value.into();
        if value.is_empty() {
            return Err(RiftError::validity("RepoId must not be empty"));
        }
        if value.len() > 128 {
            return Err(RiftError::validity("RepoId too long (> 128)"));
        }
        if let Some(bad) = value
            .chars()
            .find(|c| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | ':' | '.')))
        {
            return Err(RiftError::validity(format!(
                "RepoId contains invalid character {bad:?}"
            )));
        }
        Ok(Self(value))
    }

    /// The slug as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl core::fmt::Display for RepoId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.0)
    }
}
impl core::convert::TryFrom<String> for RepoId {
    type Error = RiftError;
    fn try_from(s: String) -> crate::Result<Self> {
        Self::new(s)
    }
}
impl core::convert::From<RepoId> for String {
    fn from(v: RepoId) -> Self {
        v.0
    }
}
impl crate::immutable::Immutable for RepoId {}

#[cfg(test)]
mod tests {
    use super::*;
    use core::str::FromStr;

    #[test]
    fn byte_id_hex_roundtrips() {
        let id = Sha256Id::from_bytes([0xAB; 32]);
        let hex = id.to_hex();
        assert_eq!(hex.len(), 64);
        assert_eq!(Sha256Id::from_str(&hex).unwrap(), id);
        // case-insensitive parse
        assert_eq!(Sha256Id::parse_hex(&hex.to_uppercase()).unwrap(), id);
        assert_eq!(Sha256Id::algorithm(), "sha256");
        assert_eq!(Blake3Id::algorithm(), "blake3");
    }

    #[test]
    fn byte_id_rejects_bad_hex() {
        assert!(Sha256Id::parse_hex("").is_err());
        assert!(Sha256Id::parse_hex("zz").is_err());
        assert!(Sha256Id::parse_hex(&"a".repeat(63)).is_err());
        assert!(Sha256Id::parse_hex(&"g".repeat(64)).is_err());
    }

    #[test]
    fn byte_id_json_roundtrips_and_validates() {
        let id = Blake3Id::from_bytes([1; 32]);
        let json = serde_json::to_string(&id).unwrap();
        assert!(json.starts_with('"') && json.contains("0101"));
        let back: Blake3Id = serde_json::from_str(&json).unwrap();
        assert_eq!(id, back);
        // an invalid id must fail to deserialize, never silently enter
        assert!(serde_json::from_str::<Blake3Id>("\"nothex\"").is_err());
    }

    #[test]
    fn ulid_packs_timestamp_in_high_bits() {
        let u = Ulid::from_parts(0x0001_0203_0405, 0x42);
        assert_eq!(u.timestamp_ms(), 0x0001_0203_0405);
        let hex = u.to_hex();
        assert_eq!(hex.len(), 32);
        assert_eq!(Ulid::from_str(&hex).unwrap(), u);
    }

    /// The deterministic source yields strictly increasing, sortable ids with
    /// no dependence on the wall clock (06 §6.3 determinism).
    #[test]
    fn ulid_is_monotonic_with_injected_source() {
        let src = CounterUlidSource::starting_at(10);
        let a = src.next_ulid();
        let b = src.next_ulid();
        let c = src.next_ulid();
        assert!(a < b && b < c);
        assert_eq!(a.to_u128(), 10);
    }

    #[test]
    fn monotonic_source_is_strictly_increasing() {
        let src = MonotonicUlidSource::new();
        let mut prev = src.next_ulid();
        for _ in 0..1000 {
            let next = src.next_ulid();
            assert!(next > prev, "ulids must strictly increase");
            prev = next;
        }
    }

    #[test]
    fn semantic_ids_are_distinct_types() {
        let u = Ulid::from_u128(5);
        let s = SessionId::new(u);
        let t = TurnId::new(u);
        // Same underlying value, different types — equality across types does
        // not even compile, which is the point. Their string forms match.
        assert_eq!(s.to_string(), t.to_string());
        assert_eq!(SessionId::from_str(&s.to_string()).unwrap(), s);
    }

    #[test]
    fn checkpoint_id_wraps_sha256_and_roundtrips_json() {
        let c = CheckpointId::new(Sha256Id::from_bytes([7; 32]));
        let json = serde_json::to_string(&c).unwrap();
        let back: CheckpointId = serde_json::from_str(&json).unwrap();
        assert_eq!(c, back);
    }

    #[test]
    fn repo_id_validates() {
        assert_eq!(
            RepoId::new("acme.repo-1:abc").unwrap().as_str(),
            "acme.repo-1:abc"
        );
        assert!(RepoId::new("").is_err());
        assert!(RepoId::new("has space").is_err());
        assert!(RepoId::new("x".repeat(129)).is_err());
    }

    #[test]
    fn capture_session_identity_derivation_is_pinned() {
        assert_eq!(
            capture_session_row_id("claude-code", "native-session"),
            "537d79c24262f69f543305cd0ec7142f912d29e3c9e7e6315e4b1a47d5805c2a"
        );
        assert_ne!(
            capture_session_row_id("claude-code", "native-session"),
            capture_session_row_id("codex", "native-session")
        );
    }

    proptest::proptest! {
        /// Content-addressed ids are a pure function of their bytes: hex
        /// encode→decode is the identity (idempotency precondition, 04 §4.4).
        #[test]
        fn sha256id_hex_roundtrip_any_bytes(bytes in proptest::array::uniform32(proptest::num::u8::ANY)) {
            let id = Sha256Id::from_bytes(bytes);
            let back = Sha256Id::parse_hex(&id.to_hex()).unwrap();
            proptest::prop_assert_eq!(id, back);
        }
    }
}
