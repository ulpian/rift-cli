//! Interactive app previews and a real installer lifecycle, using local assets.

use std::path::{Path, PathBuf};

use super::{artifacts, ensure, fixture, process, read, Result};
use fixture::{Fixture, MANIFEST};

pub(super) fn select_binary(candidates: &[PathBuf], tag: &str) -> Result<PathBuf> {
    let mut failures = Vec::new();
    for path in candidates {
        if !path.exists() {
            continue;
        }
        match artifacts::verify_version(path, tag) {
            Ok(()) => {
                let path = path.canonicalize().map_err(|e| e.to_string())?;
                println!("Preview binary: {}", path.display());
                return Ok(path);
            }
            Err(error) => failures.push(error),
        }
    }
    Err(format!(
        "No native release binary matching {tag} was found. {}\n\
        Build one with `cargo build --release -p rift`, or pass `--binary PATH`.\n\
        Your installed Rift was left unchanged.",
        failures.join("\n")
    ))
}

pub(super) fn interactive(binary: &Path, flow: &str) -> Result<()> {
    artifacts::verify_version(binary, &artifacts::version()?)?;
    ensure(
        ["install", "setup", "dashboard"].contains(&flow),
        "choose install, setup or dashboard",
    )?;
    let mut fixture = Fixture::new()?;
    let mut command = if flow == "install" {
        fixture.downloads(binary, &artifacts::version()?)?;
        let mut command = fixture.command("sh", fixture.root());
        command.arg(crate::workspace_root().join("install.sh"));
        command
    } else {
        durable(binary)?;
        let repo = fixture.project("project", flow == "dashboard")?;
        if flow == "dashboard" {
            fixture::initialize(&fixture, binary, &repo)?;
        }
        fixture.command(binary, &repo)
    };
    println!("Disposable {flow} preview. Ctrl-C exits; temporary data is removed on exit.");
    println!("HOME and your installed Rift stay unchanged; downloads use a local fixture.");
    let status = command.status().map_err(|e| e.to_string())?;
    ensure(status.success(), "preview exited unsuccessfully")
}

pub(super) fn durable(binary: &Path) -> Result<()> {
    ensure(
        !binary.components().any(|c| c.as_os_str() == "debug")
            && !binary.starts_with(
                std::env::temp_dir()
                    .canonicalize()
                    .map_err(|e| e.to_string())?,
            ),
        "project previews need a durable release binary outside system temp and target/debug",
    )
}

pub(super) fn lifecycle(binary: &Path) -> Result<()> {
    let mut fixture = Fixture::new()?;
    let tag = artifacts::version()?;
    artifacts::verify_version(binary, &tag)?;
    fixture.downloads(binary, &tag)?;
    fixture.set("RIFT_NONINTERACTIVE", "1");
    println!("1/5 Install: real installer and binary, local download transport");
    process::must(
        fixture
            .command("sh", fixture.root())
            .arg(crate::workspace_root().join("install.sh")),
        120,
    )?;
    let installed = fixture.root().join("bin/rift");
    let receipt = fixture.root().join("bin/rift.install-receipt");
    println!("2/5 Verify executable and receipt");
    artifacts::verify_version(&installed, &tag)?;
    ensure(
        read(&receipt)?
            == format!(
                "rift-install-v1\n{}\n{tag}\n",
                artifacts::digest(&installed)?
            ),
        "installed receipt differs",
    )?;
    ensure(
        !fixture.root().join(".rift").exists(),
        "installer started project setup",
    )?;
    println!("3/5 Same-version managed update");
    process::must(
        fixture
            .command(&installed, fixture.root())
            .args(["update", "--from", MANIFEST]),
        120,
    )?;
    println!("4/5 Uninstall dry-run");
    process::must(
        fixture
            .command(&installed, fixture.root())
            .args(["uninstall", "--dry-run"]),
        30,
    )?;
    ensure(installed.is_file(), "dry-run removed the executable")?;
    println!("5/5 Uninstall");
    process::must(
        fixture.command(&installed, fixture.root()).arg("uninstall"),
        30,
    )?;
    ensure(
        !installed.exists() && !receipt.exists(),
        "uninstall left managed files behind",
    )?;
    println!("PASS install, receipt, update and uninstall; temporary files cleaned up.");
    Ok(())
}
