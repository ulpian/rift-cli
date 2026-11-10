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

/// The startup-file line is written once, on its own line, for the shell the
/// user logs in with, and never when they opted out. HOME is the fixture root
/// here, so nothing outside the disposable directory is touched.
fn path_registration() -> Result<()> {
    const MARK: &str = "# rift installer";
    let (mut fixture, _) = prepare()?;
    let home = fixture.root().to_path_buf();
    fixture.set("HOME", home.as_os_str());
    fixture.set("SHELL", "/bin/zsh");
    write(&home.join(".zshrc"), "alias keep=1")?;
    install(&fixture, true)?;
    ensure(
        !home.join(".profile").exists() && read(&home.join(".zshrc"))? == "alias keep=1",
        "opted-out install edited a startup file",
    )?;
    fixture.set("RIFT_NO_MODIFY_PATH", "0");
    install(&fixture, true)?;
    install(&fixture, true)?;
    let zshrc = read(&home.join(".zshrc"))?;
    let entry =
        "case \":$PATH:\" in *\":$HOME/bin:\"*) ;; *) export PATH=\"$HOME/bin:$PATH\" ;; esac";
    ensure(
        zshrc == format!("alias keep=1\n{entry} {MARK}\n"),
        &format!("unexpected zsh startup file:\n{zshrc}"),
    )?;
    fixture.set("SHELL", "/bin/bash");
    install(&fixture, true)?;
    for name in [".bashrc", ".profile"] {
        ensure(
            read(&home.join(name))?.matches(MARK).count() == 1,
            &format!("bash startup file {name} not registered exactly once"),
        )?;
    }
    ensure(
        read(&home.join(".zshrc"))? == zshrc,
        "another shell's startup file was edited",
    )
}

/// A named release bucket refuses links into any other, and an older alpha
/// never replaces a newer one unless the operator says so.
fn pinning_and_downgrade() -> Result<()> {
    let (mut fixture, stub) = prepare()?;
    fixture.set("RIFT_DOWNLOAD_BUCKET", "another-bucket");
    install(&fixture, false)?;
    ensure(
        !fixture.root().join("bin/rift").exists(),
        "a link outside the named bucket installed",
    )?;
    fixture.set("RIFT_DOWNLOAD_BUCKET", "rift-install-preview");
    // curl would resolve the dot segment and fetch from the other bucket.
    fixture.set(
        "RIFT_DOWNLOAD_MANIFEST",
        "https://storage.googleapis.com/rift-install-preview/../evil/downloads.tsv",
    );
    install(&fixture, false)?;
    ensure(
        !fixture.root().join("bin/rift").exists(),
        "a dot segment escaped the named bucket",
    )?;
    fixture.set("RIFT_DOWNLOAD_MANIFEST", fixture::MANIFEST);
    make_version(&mut fixture, &stub, "v1.0.0-alpha.2")?;
    install(&fixture, true)?;
    let receipt = fixture.root().join("bin/rift.install-receipt");
    let newer = read(&receipt)?;
    make_version(&mut fixture, &stub, "v1.0.0-alpha.1")?;
    fixture.set("RIFT_UPDATE", "1");
    install(&fixture, false)?;
    ensure(
        read(&receipt)? == newer,
        "an older alpha replaced a newer one",
    )?;
    fixture.set("RIFT_ALLOW_DOWNGRADE", "1");
    install(&fixture, true)?;
    ensure(
        read(&receipt)?.ends_with("v1.0.0-alpha.1\n"),
        "an explicit downgrade was refused",
    )
}

pub(super) fn check() -> Result<()> {
    versions()?;
    foreign()?;
    race()?;
    corrupt_and_linked()?;
    path_registration()?;
    pinning_and_downgrade()?;
    println!("PASS offline installer safety: fresh/update, ownership, concurrent edits, checksum, symlink refusal, PATH registration, bucket pin and downgrade refusal");
    Ok(())
}
