//! Runs the shipped bootstrap against the Rust fixture transport, offline.
#![forbid(unsafe_code)]

#[test]
#[cfg(unix)]
fn bootstrap_preserves_foreign_modified_and_concurrently_edited_installations(
) -> Result<(), std::io::Error> {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["release", "check-safety"])
        .output()?;
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}
