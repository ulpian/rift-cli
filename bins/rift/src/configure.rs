//! `rift configure`: the checkpoint remote in `.rift/config.toml`, and the
//! global splash preference — the two writers the palette's `/configure`
//! verbs share with the command line.

use crate::cli::{self, ConfigureArgs, Outcome};

/// `rift configure`: manage `.rift/config.toml` (the checkpoint remote / policy)
/// and, via subcommands, the global canopy preferences.
pub(crate) fn cmd_configure(
    root: &std::path::Path,
    args: &ConfigureArgs,
) -> rift_core::Result<Outcome> {
    if let Some(cli::ConfigureCmd::SplashMode(m)) = &args.cmd {
        return configure_splash_mode(m.mode);
    }
    let rift_dir = root.join(".rift");
    std::fs::create_dir_all(&rift_dir)?;
    let cfg = rift_dir.join("config.toml");
    if let Some(remote) = &args.checkpoint_remote {
        // A PATCH, never a replacement. This used to `std::fs::write` a
        // two-line document over `.rift/config.toml`, which discarded every
        // sensitive path, model rule and retention class the project had
        // declared — and left behind a file the strict loader then refused, so
        // the loss was silent. The shared editor scaffolds what is missing,
        // preserves what is there, validates the prospective document before it
        // replaces anything, and refuses a symlinked or non-owned policy file.
        let snapshot = rift_config::load_project_policy(root)
            .map_err(|e| rift_core::RiftError::config(e.to_string()))?;
        rift_config::set_checkpoint_remote(root, &snapshot.revision, Some(remote))
            .map_err(|e| rift_core::RiftError::config(e.to_string()))?;
        Ok(Outcome::ok(
            format!("configured checkpoint remote: {remote}"),
            serde_json::json!({ "checkpoint_remote": remote, "config": cfg.display().to_string() }),
        ))
    } else {
        let doc = std::fs::read_to_string(&cfg).unwrap_or_default();
        Ok(Outcome::ok(
            format!("config: {}\n{doc}", cfg.display()),
            serde_json::json!({ "config": cfg.display().to_string() }),
        ))
    }
}

/// `rift configure splash-mode <always|first-run|never>`:
/// a read-modify-write of the **global** canopy preferences that changes only
/// the splash key — `seen_splash`, `reduced_motion`, and `mouse` ride through
/// untouched (the prefs read is default-safe, so a missing or garbage file
/// starts from defaults rather than erroring).
pub(crate) fn configure_splash_mode(mode: cli::SplashModeArg) -> rift_core::Result<Outcome> {
    use rift_canopy::splash::prefs;
    let mut p = prefs::load();
    p.splash_mode = mode.label().to_string();
    prefs::save(&p)?;
    let path = prefs::default_path();
    Ok(Outcome::ok(
        format!("configured splash mode: {}", mode.label()),
        serde_json::json!({
            "splash_mode": mode.label(),
            "prefs": path.display().to_string(),
        }),
    ))
}
