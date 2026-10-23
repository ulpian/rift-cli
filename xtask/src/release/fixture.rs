//! Disposable download transport, user roots and Git projects. HOME is preserved.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::{artifacts, ensure, process, read, write, Result};

pub(super) const MANIFEST: &str =
    "https://storage.googleapis.com/rift-install-preview/downloads.tsv";

pub(super) struct Fixture {
    directory: tempfile::TempDir,
    pub(super) env: BTreeMap<OsString, OsString>,
}

impl Fixture {
    pub(super) fn new() -> Result<Self> {
        ensure(
            cfg!(unix),
            "native previews currently require macOS or Linux",
        )?;
        let directory = tempfile::Builder::new()
            .prefix("rift-release-preview-")
            .tempdir()
            .map_err(|e| e.to_string())?;
        let root = directory.path();
        let mut env: BTreeMap<OsString, OsString> = std::env::vars_os()
            .filter(|(k, _)| {
                let key = k.to_string_lossy();
                !key.starts_with("RIFT_") && !key.starts_with("GIT_")
            })
            .collect();
        for (key, value) in [
            ("RIFT_CONFIG_DIR", root.join("config")),
            ("RIFT_CACHE_DIR", root.join("cache")),
            ("RIFT_BIN_DIR", root.join("bin")),
            ("GIT_TEMPLATE_DIR", root.join("templates")),
        ] {
            env.insert(key.into(), value.into_os_string());
        }
        for (key, value) in [
            ("RIFT_NO_KEYCHAIN", "1"),
            ("GIT_CONFIG_GLOBAL", "/dev/null"),
            ("GIT_CONFIG_SYSTEM", "/dev/null"),
            ("GIT_CONFIG_NOSYSTEM", "1"),
        ] {
            env.insert(key.into(), value.into());
        }
        std::fs::create_dir(root.join("templates")).map_err(|e| e.to_string())?;
        Ok(Self { directory, env })
    }

    pub(super) fn root(&self) -> &Path {
        self.directory.path()
    }

    pub(super) fn set(&mut self, key: &str, value: impl Into<OsString>) {
        self.env.insert(key.into(), value.into());
    }

    pub(super) fn command(&self, program: impl AsRef<std::ffi::OsStr>, cwd: &Path) -> Command {
        let mut command = Command::new(program);
        command.current_dir(cwd).env_clear().envs(&self.env);
        command
    }

    pub(super) fn project(&self, name: &str, worktree: bool) -> Result<PathBuf> {
        let repo = self.root().join(name);
        process::must(
            self.command("git", self.root())
                .args(["init", "--quiet", "--initial-branch=main"])
                .arg(&repo),
            20,
        )?;
        write(&repo.join("README.md"), "# Disposable Rift preview\n")?;
        process::must(self.command("git", &repo).args(["add", "README.md"]), 20)?;
        process::must(
            self.command("git", &repo).args([
                "-c",
                "user.name=Rift Preview",
                "-c",
                "user.email=preview@localhost",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "--quiet",
                "-m",
                "Preview fixture",
            ]),
            20,
        )?;
        if worktree {
            process::must(
                self.command("git", &repo)
                    .args(["worktree", "add", "--quiet", "-b", "feature-demo"])
                    .arg(self.root().join("feature-demo")),
                20,
            )?;
        }
        Ok(repo)
    }

    pub(super) fn downloads(&mut self, binary: &Path, tag: &str) -> Result<()> {
        let assets = self.root().join("assets");
        let commands = self.root().join("commands");
        std::fs::create_dir_all(&assets).map_err(|e| e.to_string())?;
        std::fs::create_dir_all(&commands).map_err(|e| e.to_string())?;
        let target = artifacts::host()?;
        let filename = artifacts::name(tag, &target);
        let archive = assets.join(&filename);
        artifacts::archive(binary, &archive, false)?;
        write(
            &assets.join("downloads.tsv"),
            format!(
                "rift-download-v1\t{tag}\n{target}\t\
            https://storage.googleapis.com/rift-install-preview/{filename}\t{}\n",
                artifacts::digest(&archive)?
            ),
        )?;
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let script = format!(
            "#!/bin/sh\nexec {} release fixture-fetch \"$@\"\n",
            quote(&exe.to_string_lossy())
        );
        write(&commands.join("curl"), script)?;
        executable(&commands.join("curl"))?;
        let mut path = vec![commands];
        path.extend(std::env::split_paths(
            self.env
                .get(std::ffi::OsStr::new("PATH"))
                .ok_or("missing PATH")?,
        ));
        self.set(
            "PATH",
            std::env::join_paths(path).map_err(|e| e.to_string())?,
        );
        self.set("RIFT_PREVIEW_ASSETS", assets.into_os_string());
        self.set("RIFT_DOWNLOAD_MANIFEST", MANIFEST);
        Ok(())
    }
}

pub(super) fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

pub(super) fn executable(path: &Path) -> Result<()> {
    // Windows executability comes from the file type rather than Unix mode bits.
    #[cfg(not(unix))]
    let _ = path;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

pub(super) fn fetch(args: &[String]) -> Result<()> {
    let url = args.last().ok_or("missing fixture URL")?;
    let filename = url
        .strip_prefix("https://storage.googleapis.com/rift-install-preview/")
        .ok_or("unexpected fixture host")?;
    ensure(
        !filename.is_empty()
            && !filename.contains(['/', '\\', '?', '#'])
            && filename != "."
            && filename != "..",
        "invalid fixture filename",
    )?;
    let index = args
        .iter()
        .position(|s| s == "--output")
        .ok_or("missing fixture output")?;
    let destination = args.get(index + 1).ok_or("missing fixture output path")?;
    let assets =
        PathBuf::from(std::env::var_os("RIFT_PREVIEW_ASSETS").ok_or("missing fixture assets")?);
    if filename.ends_with(".tar.gz") && std::env::var_os("RIFT_PREVIEW_EDIT_DURING_FETCH").is_some()
    {
        let bin = PathBuf::from(std::env::var_os("RIFT_BIN_DIR").ok_or("missing fixture bin")?);
        write(&bin.join("rift"), "edited while downloading")?;
    }
    // Stream into the requested output, including /dev/stdout for curl | sh.
    // fs::copy can also change destination permissions, which a terminal must keep.
    let mut source = std::fs::File::open(assets.join(filename)).map_err(|e| e.to_string())?;
    let mut output = std::fs::File::create(destination).map_err(|e| e.to_string())?;
    std::io::copy(&mut source, &mut output).map_err(|e| e.to_string())?;
    Ok(())
}

pub(super) fn defaults(fixture: &Fixture) -> PathBuf {
    fixture.root().join("config/rift/defaults.toml")
}

pub(super) fn initialize(fixture: &Fixture, binary: &Path, repo: &Path) -> Result<()> {
    std::fs::create_dir(repo.join(".rift")).map_err(|e| e.to_string())?;
    write(
        &repo.join(".rift/config.toml"),
        "schema_version=1\n[project]\nname=\"Rift preview\"\n\
        [features]\ntotal_recall=false\n[capture]\nraw_transcripts=false\n",
    )?;
    process::must(
        fixture.command(binary, repo).args(["init", "--no-recall"]),
        120,
    )?;
    Ok(())
}

pub(super) fn policy(path: &Path) -> Result<toml::Value> {
    read(path)?
        .parse()
        .map_err(|e| format!("invalid preview policy: {e}"))
}
