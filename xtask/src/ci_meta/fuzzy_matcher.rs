//! The screen that keeps the fuzzy matcher's workspace pin bounds-checked.
use std::path::Path;

/// The fuzzy matcher, and the feature that decides whether it reads past what
/// it was given.
///
/// `neo_frizbee`'s vectorised backends load a whole register at a time and
/// rely on the load staying inside the same page — an over-read that is
/// undefined behaviour in Rust however well it behaves in practice, and one
/// this workspace's own fuzz target reached. `safe_read` is the published
/// feature that turns those loads into bounded ones.
const FUZZY_MATCHER: (&str, &str) = ("neo_frizbee", "safe_read");

/// The one violation this check can report, over a manifest already parsed.
///
/// Split from the file read so the claim stays about a declaration: a test
/// that states the declaration directly proves the check on every spelling of
/// it rather than on a temporary file.
fn matcher_pin_carries_bounds(manifest: &toml::Value) -> Vec<String> {
    let (crate_name, feature) = FUZZY_MATCHER;
    let Some(pin) = manifest
        .get("workspace")
        .and_then(|w| w.get("dependencies"))
        .and_then(|d| d.get(crate_name))
    else {
        // Not depended on at all is not a violation: a workspace that stopped
        // using the matcher has nothing to hold to a feature of it.
        return Vec::new();
    };
    if pin
        .get("features")
        .and_then(toml::Value::as_array)
        .is_some_and(|features| features.iter().any(|f| f.as_str() == Some(feature)))
    {
        return Vec::new();
    }
    vec![format!(
        "Cargo.toml: the workspace pin of `{crate_name}` no longer enables `{feature}` — its \
         vectorised backends then read past the end of what they were given, which is \
         undefined behaviour on a matcher this tree calls from a hook with an agent's own \
         query; restore the feature rather than the pin"
    )]
}

/// **The fuzzy matcher stays bounds-checked.**
///
/// The feature is a one-word entry in an array in the root manifest: it is
/// dropped by deleting eleven characters, nothing fails to compile when it
/// goes, and what it costs is a class of bug no test in this repository can
/// see. The pin is the whole safety argument for calling a SIMD matcher on
/// paths that come out of an agent's own query, and it is now called from a
/// hook — so it is asserted here rather than remembered.
///
/// Read as TOML rather than as text, so every spelling of the same dependency
/// declaration is the same fact: an inline table, a
/// `[workspace.dependencies.x]` section, `features` written on one line or
/// several.
///
/// # Errors
/// If the root manifest cannot be read or parsed.
pub fn fuzzy_matcher_reads_within_bounds(root: &Path) -> Result<Vec<String>, String> {
    Ok(matcher_pin_carries_bounds(&crate::read_toml(
        &root.join("Cargo.toml"),
    )?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace_root;

    /// The matcher's bounds-checking feature is asserted on this workspace,
    /// and its absence is a violation however the pin is spelled.
    #[test]
    fn a_fuzzy_matcher_pin_without_bounds_checked_reads_is_refused() {
        assert!(fuzzy_matcher_reads_within_bounds(&workspace_root())
            .unwrap()
            .is_empty());
        let (crate_name, feature) = FUZZY_MATCHER;
        let parsed = |body: &str| body.parse::<toml::Value>().unwrap();
        let refused = [
            format!("[workspace.dependencies]\n{crate_name} = \"=0.11.0\"\n"),
            format!(
                "[workspace.dependencies]\n{crate_name} = {{ version = \"=0.11.0\", features = [\"other\"] }}\n"
            ),
            format!("[workspace.dependencies.{crate_name}]\nversion = \"=0.11.0\"\n"),
        ];
        for body in &refused {
            assert_eq!(
                matcher_pin_carries_bounds(&parsed(body)).len(),
                1,
                "a pin without the feature must be refused: {body}"
            );
        }
        let kept = [
            format!(
                "[workspace.dependencies]\n{crate_name} = {{ version = \"=0.11.0\", features = [\"{feature}\"] }}\n"
            ),
            format!(
                "[workspace.dependencies.{crate_name}]\nversion = \"=0.11.0\"\nfeatures = [\n  \"{feature}\",\n]\n"
            ),
            // A workspace that does not use the matcher is held to nothing.
            "[workspace.dependencies]\nserde = \"1\"\n".to_owned(),
        ];
        for body in &kept {
            assert!(
                matcher_pin_carries_bounds(&parsed(body)).is_empty(),
                "the same fact, spelled another way: {body}"
            );
        }
    }
}
