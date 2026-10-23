//! Native installer onboarding. Only the per-user defaults file is writable.

use super::defaults::{default_path, Defaults, Snapshot};
use crate::cli::Outcome;
pub(super) fn run(ascii: bool) -> rift_core::Result<Outcome> {
    use std::io::IsTerminal;
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        return Err(rift_core::RiftError::Other(
            "installation preferences need a terminal; run `rift install` interactively".into(),
        ));
    }
    let snapshot = Snapshot::load(default_path())?;
    let mut view = rift_canopy_data::installation_settings_view();
    if let Some(defaults) = &snapshot.defaults {
        defaults.seed(&mut view);
    }
    let saved = rift_canopy::app::live::run_installation(
        view,
        &mut |plan| {
            snapshot
                .save(&Defaults::from_plan(plan))
                .map_err(Into::into)
        },
        ascii,
    )?;
    Ok(Outcome::ok(
        if saved {
            "Installation defaults saved. Run `rift` in your project to review setup."
        } else {
            "Installation preferences cancelled; existing defaults were preserved. Run `rift install` to return."
        },
        serde_json::json!({ "saved": saved, "path": default_path() }),
    ))
}
