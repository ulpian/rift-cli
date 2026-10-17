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
#[cfg(unix)]
mod files;
pub(crate) mod pins;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PinError;

type PinResult<T> = Result<T, PinError>;

impl std::fmt::Display for PinError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("physical delivery evidence is unavailable or inconsistent")
    }
}
impl std::error::Error for PinError {}
