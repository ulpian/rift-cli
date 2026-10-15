//! Stored collection observations cannot certify authorship percentages.

use rift_core::Result;
use rift_shadow_hooks::trailers::AttributionSummary;
use rift_store::CanonicalStore;

/// No preview is derived from persisted boundary collections: their event
/// tokens, kinds and collection intervals do not prove historical writers.
///
/// # Errors
/// This conservative implementation performs no I/O and returns success.
pub async fn attribution_preview(
    _store: &impl CanonicalStore,
    _agent: &str,
    _native_session: &str,
) -> Result<Option<AttributionSummary>> {
    Ok(None)
}
