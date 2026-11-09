//! Native installer/setup/settings regressions, never using cloud or user state.

use std::path::{Path, PathBuf};
use std::time::Duration;

use super::{artifacts, ensure, fixture, preview, read, terminal::Terminal, write, Result};
use fixture::Fixture;

pub(super) fn check(binary: &Path) -> Result<()> {
    preview::durable(binary)?;
    let mut fixture = Fixture::new()?;
    fixture.env.remove(std::ffi::OsStr::new("NO_COLOR"));
    for (key, value) in [
        ("TERM", "xterm-256color"),
        ("TERM_PROGRAM", "Apple_Terminal"),
        ("COLORTERM", "truecolor"),
        ("LANG", "en_US.UTF-8"),
    ] {
        fixture.set(key, value);
    }
    let installed = installer(&mut fixture, binary)?;
    configured_clone(&fixture, &installed)?;
    project(&fixture, binary)?;
    println!("PASS terminal flows; temporary projects, binaries and caches cleaned up.");
    Ok(())
}

fn compatible(term: &Terminal) -> Result<()> {
    ensure(
        !term.raw.windows(5).any(|s| s == b"38;2;" || s == b"48;2;"),
        "Apple Terminal received RGB escapes",
    )
}

fn installer(fixture: &mut Fixture, binary: &Path) -> Result<PathBuf> {
    fixture.downloads(binary, &artifacts::version()?)?;
    let script = crate::workspace_root().join("install.sh");
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    // A real pipe: stdin is script source; wizard input must use the terminal device.
    let pipe = format!(
        "{} release fixture-fetch --output /dev/stdout \
        https://storage.googleapis.com/rift-install-preview/install.sh | sh",
        fixture::quote(&exe.to_string_lossy())
    );
    std::fs::copy(&script, fixture.root().join("assets/install.sh")).map_err(|e| e.to_string())?;
    let defaults = fixture::defaults(fixture);
    {
        let mut term = Terminal::new(fixture, &["sh", "-c", &pipe], fixture.root())?;
        let text = term.wait("Choose your harnesses", 15)?;
        ensure(
            text.contains("Step 2/5")
                && text.contains("rift update --from")
                && text.contains("Rift v"),
            "installer header or milestones missing",
        )?;
        let before = term.raw.len();
        term.capture(Duration::from_millis(800))?;
        ensure(
            term.raw
                .get(before..)
                .unwrap_or_default()
                .windows(3)
                .any(|s| s == "█".as_bytes()),
            "logo did not shimmer",
        )?;
        compatible(&term)?;
        term.send(b"\x03")?;
        term.finish()?;
    }
    ensure(
        !defaults.exists() && !fixture.root().join(".rift").exists(),
        "cancel wrote preferences/project",
    )?;
    let installed = fixture.root().join("bin/rift");
    save_defaults(fixture, &installed, &defaults)?;
    println!("PASS piped installer, shimmer, cancellation and reviewed defaults");
    Ok(installed)
}

fn save_defaults(fixture: &Fixture, binary: &Path, defaults: &Path) -> Result<()> {
    let mut term = Terminal::new(
        fixture,
        &[&binary.to_string_lossy(), "install"],
        fixture.root(),
    )?;
    let text = term.wait("Choose your harnesses", 15)?;
    let selected: Vec<String> = text
        .lines()
        .filter_map(|line| {
            line.trim()
                .trim_start_matches('>')
                .trim()
                .strip_prefix("● ")
                .map(str::trim)
                .map(str::to_owned)
        })
        .collect();
    for label in selected {
        term.click(&label)?;
    }
    term.click("Codex")?;
    term.advance("Review installation defaults")?;
    ensure(!defaults.exists(), "defaults written before review")?;
    term.advance("Confirm defaults")?;
    ensure(!defaults.exists(), "defaults written before confirmation")?;
    term.send(b"\r")?;
    term.finish()?;
    let data = fixture::policy(defaults)?;
    ensure(
        data.get("harnesses").and_then(toml::Value::as_array)
            == Some(&vec![toml::Value::String("codex".to_owned())]),
        "wrong saved harnesses",
    )?;
    for key in ["total_recall", "raw_transcripts"] {
        ensure(
            data.get("features")
                .and_then(|v| v.get(key))
                .and_then(toml::Value::as_bool)
                == Some(true),
            "a feature was saved off although the installer has no feature page",
        )?;
    }
    Ok(())
}

fn configured_clone(fixture: &Fixture, binary: &Path) -> Result<()> {
    let repo = fixture.project("configured-clone", false)?;
    std::fs::create_dir(repo.join(".rift")).map_err(|e| e.to_string())?;
    let path = repo.join(".rift/config.toml");
    let source =
        "schema_version=1\n[project]\nname=\"Configured clone\"\n[features]\ntotal_recall=true\n";
    write(&path, source)?;
    let mut term = Terminal::new(fixture, &[&binary.to_string_lossy()], &repo)?;
    term.wait("Choose your harnesses", 15)?;
    ensure(
        term.checked("Codex")?,
        "saved harness did not seed clone setup",
    )?;
    term.advance("Review harness permissions")?;
    term.advance("Choose project settings")?;
    ensure(
        term.checked("Total Recall")?,
        "global defaults overrode declared policy",
    )?;
    term.send(b"\x03")?;
    term.finish()?;
    ensure(read(&path)? == source, "cancel changed cloned policy")?;
    println!("PASS configured clone preserves declared policy");
    Ok(())
}

fn project(fixture: &Fixture, binary: &Path) -> Result<()> {
    let repo = fixture.project("project", false)?;
    {
        let mut term = Terminal::new(fixture, &[&binary.to_string_lossy()], &repo)?;
        let text = term.wait("Choose your harnesses", 15)?;
        ensure(
            text.contains("Step 1/5")
                && text.contains("rift doctor")
                && text.contains("rift --help"),
            "missing setup header or milestones",
        )?;
        project_choices(&mut term)?;
        term.send(b"\x03")?;
        term.finish()?;
    }
    ensure(
        !repo.join(".rift").exists(),
        "cancel created project policy",
    )?;
    let mut term = Terminal::new(fixture, &[&binary.to_string_lossy()], &repo)?;
    term.wait("Choose your harnesses", 15)?;
    project_choices(&mut term)?;
    term.advance("Review and set up")?;
    term.advance("Confirm setup")?;
    term.send(b"\r")?;
    term.wait("[Home]", 25)?;
    term.send(b"\x1b[Z")?;
    term.wait("Project Settings", 15)?;
    let before = term.checked("Tool-Call Capture")?;
    term.click("Tool-Call Capture")?;
    term.capture(Duration::from_secs(1))?;
    ensure(
        term.checked("Tool-Call Capture")? != before,
        "Settings toggle did not change",
    )?;
    let data = fixture::policy(&repo.join(".rift/config.toml"))?;
    ensure(
        data.get("capture")
            .and_then(|v| v.get("tool_calls"))
            .and_then(toml::Value::as_bool)
            == Some(!before),
        "Settings did not persist capture policy",
    )?;
    compatible(&term)?;
    println!("PASS project defaults, cancel, final confirmation, milestones and Settings toggle");
    Ok(())
}

fn project_choices(term: &mut Terminal) -> Result<()> {
    ensure(term.checked("Codex")?, "saved harness did not seed setup")?;
    term.click("Codex")?;
    term.advance("Review harness permissions")?;
    term.advance("Choose project settings")?;
    // Features are a per-project choice that starts on: nothing saved by the
    // installer may arrive here switched off.
    for label in ["Total Recall", "Raw Transcripts"] {
        ensure(
            term.checked(label)?,
            "a new project did not start with every feature on",
        )?;
    }
    Ok(())
}
