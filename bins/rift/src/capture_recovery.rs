//! Private records, exact readiness admission and physical evidence; no execution authority.

// The CFX-4 recovery controller (eligibility -> selection -> retirement) is the
// module's first production caller; until it lands, only tests reach it.
#![cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "called by the CFX-4 recovery controller (eligibility → selection → retirement), not yet wired"
    )
)]

mod codec;
mod eligibility;
#[cfg(unix)]
mod files;
pub(crate) mod hold;
#[cfg(unix)]
mod inventory;
pub(crate) mod pins;
pub(crate) mod plan;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PinError;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct WireError;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct EvidenceError;

type PinResult<T> = Result<T, PinError>;
type WireResult<T> = Result<T, WireError>;
type EvidenceResult<T> = Result<T, EvidenceError>;

impl std::fmt::Display for PinError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("physical delivery evidence is unavailable or inconsistent")
    }
}
impl std::error::Error for PinError {}
impl std::fmt::Display for WireError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("recovery record is unsupported or malformed")
    }
}
impl std::error::Error for WireError {}
impl std::fmt::Display for EvidenceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("recovery admission evidence is unavailable or inconsistent")
    }
}
impl std::error::Error for EvidenceError {}

#[cfg(test)]
pub(crate) mod test_support;
