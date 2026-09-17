//! A minimal timestamp type.
//!
//! Transaction-time and valid-time facts across the data model (see
//! the database design's bitemporal rule) are stamped with [`Timestamp`], a Unix
//! epoch millisecond count. Production reads the wall clock via
//! [`Timestamp::now`]; tests construct fixed timestamps so they never depend on
//! the clock (the determinism-in-tests rule).

/// A point in time, as milliseconds since the Unix epoch (UTC).
#[derive(
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Debug,
    ::serde::Serialize,
    ::serde::Deserialize,
)]
#[serde(transparent)]
pub struct Timestamp(i64);

impl Timestamp {
    /// Construct from explicit epoch milliseconds (deterministic; for tests and
    /// for values read back from storage).
    #[must_use]
    pub const fn from_millis(ms: i64) -> Self {
        Self(ms)
    }

    /// The epoch-millisecond value.
    #[must_use]
    pub const fn as_millis(self) -> i64 {
        self.0
    }

    /// The current wall-clock time. Saturates to `0` if the clock is before the
    /// epoch (never panics — a capture path must not abort on a bad clock).
    #[must_use]
    pub fn now() -> Self {
        let ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
        Self(ms)
    }
}

impl core::fmt::Display for Timestamp {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrips_and_orders() {
        let a = Timestamp::from_millis(100);
        let b = Timestamp::from_millis(200);
        assert!(a < b);
        assert_eq!(a.as_millis(), 100);
        assert_eq!(a.to_string(), "100");
    }

    #[test]
    fn now_is_after_epoch() {
        assert!(Timestamp::now().as_millis() > 0);
    }
}
