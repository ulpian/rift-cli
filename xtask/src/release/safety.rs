//! Adversarial installer checks using tiny executable fixtures and no network.

use std::path::PathBuf;

use super::{artifacts, ensure, fixture, process, read, write, Result};
use fixture::Fixture;

fn prepare() -> Result<(Fixture, PathBuf)> {
    let mut fixture = Fixture::new()?;
    fixture.set("RIFT_NONINTERACTIVE", "1");
    let stub = fixture.root().join("stub");
    make_version(&mut fixture, &stub, "v1.0.0-alpha")?;
    Ok((fixture, stub))
}

fn make_version(fixture: &mut Fixture, stub: &std::path::Path, tag: &str) -> Result<()> {
    write(
        stub,
        format!(
            "#!/bin/sh\nprintf 'rift {}\\n'\n",
            tag.trim_start_matches('v')
        ),
    )?;
    fixture::executable(stub)?;
    fixture.downloads(stub, tag)
}

fn install(fixture: &Fixture, success: bool) -> Result<()> {
    let result = process::capture(
        fixture
            .command("sh", fixture.root())
            .arg(crate::workspace_root().join("install.sh")),
        20,
    )?;
    ensure(
        result.status.success() == success,
        &format!(
            "unexpected installer result:\n{}{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        ),
    )?;
    let bin = fixture.root().join("bin");
    for item in std::fs::read_dir(bin).map_err(|e| e.to_string())? {
        let item = item.map_err(|e| e.to_string())?;
        ensure(
            !item
                .file_name()
                .to_string_lossy()
                .starts_with(".rift-install."),
            "staging/lock leaked",
        )?;
    }
    Ok(())
}

fn versions() -> Result<()> {
    let (mut fixture, stub) = prepare()?;
    install(&fixture, true)?;
    install(&fixture, true)?;
    let installed = fixture.root().join("bin/rift");
    let before = artifacts::digest(&installed)?;
    make_version(&mut fixture, &stub, "v1.0.0-alpha.1")?;
    fixture.set("RIFT_UPDATE", "1");
    install(&fixture, true)?;
    let after = artifacts::digest(&installed)?;
    ensure(
        before != after
            && read(&fixture.root().join("bin/rift.install-receipt"))?
                == format!("rift-install-v1\n{after}\nv1.0.0-alpha.1\n"),
        "update receipt mismatch",
    )
}

fn foreign() -> Result<()> {
    let (fixture, _) = prepare()?;
    std::fs::create_dir(fixture.root().join("bin")).map_err(|e| e.to_string())?;
    let binary = fixture.root().join("bin/rift");
    write(&binary, "developer-owned")?;
    install(&fixture, false)?;
    ensure(
        read(&binary)? == "developer-owned",
        "foreign executable overwritten",
    )?;
    std::fs::remove_file(&binary).map_err(|e| e.to_string())?;
    install(&fixture, true)?;
    write(&binary, "modified")?;
    install(&fixture, false)?;
    ensure(
        read(&binary)? == "modified",
        "modified executable overwritten",
    )
}

fn race() -> Result<()> {
    let (mut fixture, stub) = prepare()?;
    install(&fixture, true)?;
    let receipt = fixture.root().join("bin/rift.install-receipt");
    let before = read(&receipt)?;
    make_version(&mut fixture, &stub, "v1.0.0-alpha.1")?;
    fixture.set("RIFT_PREVIEW_EDIT_DURING_FETCH", "1");
    install(&fixture, false)?;
    ensure(
        read(&fixture.root().join("bin/rift"))? == "edited while downloading"
            && read(&receipt)? == before,
        "concurrent edit or receipt overwritten",
    )
}

fn corrupt_and_linked() -> Result<()> {
    let (fixture, _) = prepare()?;
    let manifest = fixture.root().join("assets/downloads.tsv");
    let source = read(&manifest)?;
    let (prefix, _) = source.rsplit_once('\t').ok_or("invalid fixture manifest")?;
    write(&manifest, format!("{prefix}\t{}\n", "0".repeat(64)))?;
    install(&fixture, false)?;
    ensure(
        !fixture.root().join("bin/rift").exists(),
        "bad checksum installed",
    )?;
    let unrelated = fixture.root().join("unrelated");
    write(&unrelated, "preserve")?;
    #[cfg(unix)]
    std::os::unix::fs::symlink(&unrelated, fixture.root().join("bin/rift"))
        .map_err(|e| e.to_string())?;
    install(&fixture, false)?;
    ensure(
        read(&unrelated)? == "preserve",
        "symlink target overwritten",
    )
}

pub(super) fn check() -> Result<()> {
    versions()?;
    foreign()?;
    race()?;
    corrupt_and_linked()?;
    println!("PASS offline installer safety: fresh/update, ownership, concurrent edits, checksum and symlink refusal");
    Ok(())
}
