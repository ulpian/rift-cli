//! Owner release tools and isolated previews. Never builds or publishes implicitly.

mod artifacts;
mod cloud;
mod fixture;
mod flows;
mod preview;
mod process;
mod safety;
mod signing;
mod terminal;

#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};

type Result<T> = std::result::Result<T, String>;

const HELP: &str = "cargo rift COMMAND [OPTIONS]
  package TARGET --binary PATH --out DIR   Verify a native binary and package it
  checksums --dist DIR                     Require the five official archives
  native-check TARGET --binary PATH --archive PATH --out DIR
                                          Record native version/help proof
  plan --dist DIR --bucket NAME --prefix PATH
                                          Generate public installers locally
  upload --dist DIR --bucket NAME --prefix PATH --evidence DIR
                                          Verify native evidence; publish/read back,
                                          then point latest/ at this release
  signing-key create                      Create the manifest signing key (once)
  preview install|setup|dashboard [--binary PATH]
                                          Interactive disposable preview
  check-install [--binary PATH]            Real install/update/uninstall locally
  check-flows [--binary PATH]              Bounded native terminal regression
  check-safety                            Offline installer refusal/rollback checks
  Previews select a matching native release build under CARGO_TARGET_DIR or target.
  --binary PATH selects one explicitly; no automatic fallback for an explicit path.
  Plan signs the manifest with the key in Secret Manager.
  No command commits, pushes, creates a bucket, or sends messages.";

struct Options<'a>(&'a [String]);

impl Options<'_> {
    fn value(&self, key: &str) -> Result<&str> {
        let index = self
            .0
            .iter()
            .position(|arg| arg == key)
            .ok_or_else(|| format!("missing {key}\n{HELP}"))?;
        self.0
            .get(index + 1)
            .map(String::as_str)
            .filter(|s| !s.starts_with("--"))
            .ok_or_else(|| format!("missing value for {key}"))
    }

    fn path(&self, key: &str) -> Result<PathBuf> {
        Ok(PathBuf::from(self.value(key)?))
    }

    fn binary(&self) -> Result<PathBuf> {
        let path = if self.0.iter().any(|arg| arg == "--binary") {
            self.path("--binary")?
        } else {
            let target = std::env::var_os("CARGO_TARGET_DIR")
                .map_or_else(|| PathBuf::from("target"), PathBuf::from);
            crate::workspace_root()
                .join(target)
                .join("release")
                .join(if cfg!(windows) { "rift.exe" } else { "rift" })
        };
        path.canonicalize().map_err(|e| {
            format!(
                "{}: {e}; build a release binary or pass --binary PATH",
                path.display()
            )
        })
    }

    fn preview_binary(&self) -> Result<PathBuf> {
        let tag = artifacts::version()?;
        if self.0.iter().any(|arg| arg == "--binary") {
            let path = self.binary()?;
            artifacts::verify_version(&path, &tag)?;
            return Ok(path);
        }
        let target = std::env::var_os("CARGO_TARGET_DIR")
            .map_or_else(|| PathBuf::from("target"), PathBuf::from);
        let target = crate::workspace_root().join(target);
        let host = artifacts::host()?;
        let name = if cfg!(windows) { "rift.exe" } else { "rift" };
        preview::select_binary(
            &[
                target.join("release").join(name),
                target.join(&host).join("release").join(name),
                target
                    .join("alpha-cross/target")
                    .join(host)
                    .join("release")
                    .join(name),
            ],
            &tag,
        )
    }

    fn validate(&self, positional: usize, flags: &[&str]) -> Result<()> {
        let mut seen = std::collections::BTreeSet::new();
        let rest = self.0.get(positional..).ok_or(HELP)?;
        for chunk in rest.chunks(2) {
            let key = chunk.first().ok_or(HELP)?;
            if chunk.len() != 2 || !flags.contains(&key.as_str()) || !seen.insert(key) {
                return Err(format!(
                    "unexpected, duplicate, or incomplete option {key}\n{HELP}"
                ));
            }
        }
        Ok(())
    }
}

/// Dispatch release commands; uploading always requires explicit native evidence.
///
/// # Errors
/// Invalid input, failed native checks, or failed filesystem/cloud operations.
pub fn run(args: &[String]) -> Result<()> {
    let opts = Options(args);
    let command = args.first().map_or("help", String::as_str);
    let operand = || {
        args.get(1)
            .map(String::as_str)
            .ok_or_else(|| HELP.to_owned())
    };
    match command {
        "help" | "--help" => {
            println!("{HELP}");
            Ok(())
        }
        "package" => {
            opts.validate(2, &["--binary", "--out"])?;
            artifacts::package(operand()?, &opts.binary()?, &opts.path("--out")?)
        }
        "checksums" => {
            opts.validate(1, &["--dist"])?;
            artifacts::checksums(&opts.path("--dist")?)
        }
        "native-check" => {
            opts.validate(2, &["--binary", "--archive", "--out"])?;
            artifacts::native_check(
                operand()?,
                &opts.binary()?,
                &opts.path("--archive")?,
                &opts.path("--out")?,
            )
        }
        "plan" | "upload" => cloud::run(command, &opts),
        "signing-key" => {
            opts.validate(2, &[])?;
            ensure(operand()? == "create", HELP)?;
            signing::create()
        }
        "preview" => {
            opts.validate(2, &["--binary"])?;
            preview::interactive(&opts.preview_binary()?, operand()?)
        }
        "check-install" | "check-flows" => {
            opts.validate(1, &["--binary"])?;
            let binary = opts.preview_binary()?;
            if command == "check-install" {
                preview::lifecycle(&binary)
            } else {
                flows::check(&binary)
            }
        }
        "check-safety" => {
            opts.validate(1, &[])?;
            safety::check()
        }
        // Only generated preview wrappers invoke this; no network is used.
        "fixture-fetch" => fixture::fetch(args.get(1..).unwrap_or_default()),
        _ => Err(HELP.to_owned()),
    }
}

fn read(path: &Path) -> Result<String> {
    std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))
}

fn write(path: &Path, data: impl AsRef<[u8]>) -> Result<()> {
    std::fs::write(path, data).map_err(|e| format!("write {}: {e}", path.display()))
}

fn ensure(condition: bool, message: &str) -> Result<()> {
    if condition {
        Ok(())
    } else {
        Err(message.to_owned())
    }
}
