//! Public object publishing in a separate bucket; local plans never mutate GCS.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::{artifacts, ensure, fixture::quote, process, signing, write, Options, Result};

const PUBLIC_READ_ROLE: &str = "roles/storage.legacyObjectReader";

struct Destination {
    bucket: String,
    prefix: String,
}

impl Destination {
    fn new(bucket: &str, prefix: &str) -> Result<Self> {
        ensure(
            (3..=63).contains(&bucket.len())
                && bucket
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
                && bucket.starts_with(|c: char| c.is_ascii_alphanumeric())
                && bucket.ends_with(|c: char| c.is_ascii_alphanumeric()),
            "invalid bucket name",
        )?;
        ensure(
            !prefix.is_empty()
                && prefix.split('/').all(|s| {
                    !s.is_empty()
                        && s != "."
                        && s != ".."
                        && s.bytes().all(|b| {
                            b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.'
                        })
                }),
            "prefix must contain only safe path segments; choose an opaque random prefix",
        )?;
        let config = crate::read_toml(&crate::workspace_root().join("release/storage.toml"))?;
        ensure(
            config.get("bucket").and_then(toml::Value::as_str) != Some(bucket),
            "use a separate public bucket; the private invitation bucket stays private",
        )?;
        Ok(Self {
            bucket: bucket.to_owned(),
            prefix: prefix.to_owned(),
        })
    }

    fn object(&self, name: &str) -> String {
        format!("gs://{}/{}/{name}", self.bucket, self.prefix)
    }

    fn url(&self, name: &str) -> String {
        format!(
            "https://storage.googleapis.com/{}/{}/{name}",
            self.bucket, self.prefix
        )
    }
}

pub(super) fn run(command: &str, opts: &Options<'_>) -> Result<()> {
    let flags = if command == "upload" {
        vec!["--dist", "--bucket", "--prefix", "--evidence"]
    } else {
        vec!["--dist", "--bucket", "--prefix"]
    };
    opts.validate(1, &flags)?;
    let destination = Destination::new(opts.value("--bucket")?, opts.value("--prefix")?)?;
    // `rift update` refuses links into any bucket but the one it was built to
    // trust, so publishing elsewhere would strand every installed binary.
    let recorded = crate::read_toml(&crate::workspace_root().join("release/public-storage.toml"))?;
    ensure(
        recorded.get("bucket").and_then(toml::Value::as_str) == Some(destination.bucket.as_str()),
        "the bucket must be the one in release/public-storage.toml, which `rift update` pins \
         (bins/rift/src/installation.rs RELEASE_BUCKET); change both together",
    )?;
    let dist = opts.path("--dist")?;
    let (tag, entries) = artifacts::assets(&dist)?;
    let stage = prepare(&destination, &tag, &entries)?;
    if command == "upload" {
        let commit = process::git(&["rev-parse", &format!("refs/tags/{tag}^{{commit}}")])?;
        // Publishing tools may be newer than the immutable binary source tag.
        // Native records still bind every archive to that exact tag commit.
        process::git(&["merge-base", "--is-ancestor", &commit, "HEAD"])
            .map_err(|_| "release tag must be an ancestor of the tooling checkout".to_owned())?;
        artifacts::admit(&opts.path("--evidence")?, &tag, &commit, &entries)?;
        upload(&destination, &dist, &stage, &entries)?;
    }
    println!(
        "{}: {}",
        if command == "plan" {
            "LOCAL PLAN ONLY"
        } else {
            "Published and verified"
        },
        stage.join("DOWNLOADS.md").display()
    );
    println!("{}", instructions(&destination, &tag));
    Ok(())
}

fn manifest(
    destination: &Destination,
    tag: &str,
    entries: &BTreeMap<String, String>,
) -> Result<String> {
    let mut text = format!("rift-download-v1\t{tag}\n");
    for target in artifacts::TARGETS {
        let name = artifacts::name(tag, target);
        let sha = entries.get(&name).ok_or("missing target checksum")?;
        text.push_str(&format!("{target}\t{}\t{sha}\n", destination.url(&name)));
    }
    Ok(text)
}

fn prepare(
    destination: &Destination,
    tag: &str,
    entries: &BTreeMap<String, String>,
) -> Result<PathBuf> {
    let stage = crate::workspace_root()
        .join("tmp/rift-download-plan")
        .join(&destination.bucket)
        .join(&destination.prefix);
    std::fs::create_dir_all(&stage).map_err(|e| e.to_string())?;
    let manifest_url = destination.url("downloads.tsv");
    let shell = process::git(&["show", &format!("{tag}:install.sh")])?;
    let powershell = process::git(&["show", &format!("{tag}:install.ps1")])?;
    ensure(
        shell.contains("RIFT_DOWNLOAD_MANIFEST") && powershell.contains("RIFT_DOWNLOAD_MANIFEST"),
        "tag has no manifest-aware installers",
    )?;
    let manifest = manifest(destination, tag, entries)?;
    // Installed binaries verify this signature before trusting the checksums
    // the manifest carries (`rift update`, the update prompt).
    write(
        &stage.join("downloads.tsv.sig"),
        format!("{}\n", signing::sign(manifest.as_bytes())?),
    )?;
    write(&stage.join("downloads.tsv"), manifest)?;
    write(
        &stage.join("install.sh"),
        format!(
            "#!/bin/sh\nexport RIFT_DOWNLOAD_MANIFEST={}\nexport RIFT_DOWNLOAD_BUCKET={}\n{shell}\n",
            quote(&manifest_url),
            quote(&destination.bucket)
        ),
    )?;
    write(
        &stage.join("install.ps1"),
        format!(
            "$RiftPreviousManifest = $env:RIFT_DOWNLOAD_MANIFEST\n\
        $RiftPreviousBucket = $env:RIFT_DOWNLOAD_BUCKET\ntry {{\n\
        $env:RIFT_DOWNLOAD_MANIFEST = '{manifest_url}'\n\
        $env:RIFT_DOWNLOAD_BUCKET = '{bucket}'\n{powershell}\n\
        }} finally {{ $env:RIFT_DOWNLOAD_MANIFEST = $RiftPreviousManifest; \
        $env:RIFT_DOWNLOAD_BUCKET = $RiftPreviousBucket }}\n",
            bucket = destination.bucket
        ),
    )?;
    write(&stage.join("DOWNLOADS.md"), instructions(destination, tag))?;
    // Reviewed notes may be updated after binary packaging without retagging code.
    write(
        &stage.join("NOTES.md"),
        process::git(&["show", &format!("HEAD:release/notes/{tag}.md")])?,
    )?;
    Ok(stage)
}

fn instructions(destination: &Destination, tag: &str) -> String {
    // The generated URL is literal HTTPS, and this command never follows redirects.
    // Downloads inside the bootstrap retain their strict HTTPS/checksum checks.
    format!("# Rift {tag}\n\nPublic bearer links: no account required, no expiry. Anyone can forward them.\n\n\
        macOS / Linux (OS and CPU selected automatically):\n\n```sh\n\
        curl -fsS {} | sh\n```\n\n\
        Windows PowerShell (x64):\n\n```powershell\nirm '{}' | iex\n```\n\n\
        Run `rift` inside your project and review setup. Reopen defaults with `rift install`.\n\n\
        ```sh\nrift update --from {}\nrift uninstall --dry-run\nrift uninstall\n```\n\n\
        [Release notes]({})\n", destination.url("install.sh"), destination.url("install.ps1"),
        quote(&destination.url("downloads.tsv")), destination.url("NOTES.md"))
}

/// Point `latest/` at this release: the signed manifest that `rift update`
/// and the update prompt read. Unlike a release prefix it is overwritten each
/// time, with a short cache life. The signature is written first; a client
/// that reads the pair mid-swap fails verification and simply retries later.
fn point_latest(destination: &Destination, stage: &Path) -> Result<()> {
    for name in ["downloads.tsv.sig", "downloads.tsv"] {
        let object = format!("gs://{}/latest/{name}", destination.bucket);
        gcloud(&[
            "storage",
            "cp",
            "--cache-control=public,max-age=60",
            &stage.join(name).to_string_lossy(),
            &object,
        ])?;
    }
    Ok(())
}

fn gcloud(args: &[&str]) -> Result<String> {
    let mut command = Command::new("gcloud");
    command
        .args(args)
        .arg("--quiet")
        .env("CLOUDSDK_COMPONENT_MANAGER_DISABLE_UPDATE_CHECK", "1");
    // gcloud owns its interpreter; release tools themselves run in Rust.
    let output = process::capture(&mut command, 180)?;
    ensure(
        output.status.success(),
        "gcloud failed; inspect credentials, object ownership and bucket controls",
    )?;
    String::from_utf8(output.stdout).map_err(|e| e.to_string())
}

fn bucket_ready(destination: &Destination) -> Result<()> {
    let bucket = format!("gs://{}", destination.bucket);
    let info: serde_json::Value = serde_json::from_str(&gcloud(&[
        "storage",
        "buckets",
        "describe",
        &bucket,
        "--format=json",
    ])?)
    .map_err(|e| e.to_string())?;
    ensure(
        info.get("uniform_bucket_level_access")
            .and_then(serde_json::Value::as_bool)
            == Some(true)
            && info
                .get("public_access_prevention")
                .and_then(serde_json::Value::as_str)
                == Some("inherited"),
        "download bucket must use uniform IAM and allow public reads; no existing controls will be changed",
    )?;
    let policy: serde_json::Value = serde_json::from_str(&gcloud(&[
        "storage",
        "buckets",
        "get-iam-policy",
        &bucket,
        "--format=json",
    ])?)
    .map_err(|e| e.to_string())?;
    public_policy(&policy)?;
    let role: serde_json::Value = serde_json::from_str(&gcloud(&[
        "iam",
        "roles",
        "describe",
        PUBLIC_READ_ROLE,
        "--format=json",
    ])?)
    .map_err(|e| e.to_string())?;
    ensure(
        role.get("includedPermissions") == Some(&serde_json::json!(["storage.objects.get"])),
        "public reader role permissions changed; publication stopped",
    )?;
    listing_denied(destination)
}

fn public_policy(policy: &serde_json::Value) -> Result<()> {
    for binding in policy
        .get("bindings")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
    {
        let public = binding
            .get("members")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .any(|m| matches!(m.as_str(), Some("allUsers" | "allAuthenticatedUsers")));
        if public {
            ensure(
                binding.get("role").and_then(serde_json::Value::as_str) == Some(PUBLIC_READ_ROLE)
                    && binding.get("condition").is_none(),
                "public IAM grants must allow object reads only, without listing or writes",
            )?;
        }
    }
    Ok(())
}

fn listing_denied(destination: &Destination) -> Result<()> {
    let listing = format!("https://storage.googleapis.com/{}/", destination.bucket);
    let code = process::must(
        Command::new("curl").args([
            "--proto",
            "=https",
            "--silent",
            "--show-error",
            "--max-time",
            "20",
            "--output",
            if cfg!(windows) { "NUL" } else { "/dev/null" },
            "--write-out",
            "%{http_code}",
            &listing,
        ]),
        25,
    )?;
    ensure(code == "403", "anonymous bucket listing must remain denied")
}

fn upload(
    destination: &Destination,
    dist: &Path,
    stage: &Path,
    entries: &BTreeMap<String, String>,
) -> Result<()> {
    bucket_ready(destination)?;
    let mut files: Vec<_> = entries
        .keys()
        .map(|name| (dist.join(name), name.clone()))
        .collect();
    files.push((dist.join("SHA256SUMS"), "SHA256SUMS".to_owned()));
    // Publish installer entrypoints last, after archives and manifest are ready.
    for name in [
        "NOTES.md",
        "downloads.tsv.sig",
        "downloads.tsv",
        "install.ps1",
        "install.sh",
    ] {
        files.push((stage.join(name), name.to_owned()));
    }
    let scratch = tempfile::tempdir().map_err(|e| e.to_string())?;
    let readback = scratch.path().join("object");
    for (source, name) in &files {
        let object = destination.object(name);
        gcloud(&[
            "storage",
            "cp",
            "--no-clobber",
            "--cache-control=public,max-age=300",
            &source.to_string_lossy(),
            &object,
        ])?;
        gcloud(&["storage", "cp", &object, &readback.to_string_lossy()])?;
        ensure(
            artifacts::digest(source)? == artifacts::digest(&readback)?,
            &format!("cloud readback differs: {name}; existing objects preserved"),
        )?;
        std::fs::remove_file(&readback).map_err(|e| e.to_string())?;
    }
    gcloud(&[
        "storage",
        "buckets",
        "add-iam-policy-binding",
        &format!("gs://{}", destination.bucket),
        "--member=allUsers",
        &format!("--role={PUBLIC_READ_ROLE}"),
    ])?;
    listing_denied(destination)?;
    for (source, name) in &files {
        process::must(
            Command::new("curl").args([
                "--proto",
                "=https",
                "--proto-redir",
                "=https",
                "--fail",
                "--location",
                "--silent",
                "--show-error",
                "--connect-timeout",
                "10",
                "--max-time",
                "120",
                "--output",
                &readback.to_string_lossy(),
                &destination.url(name),
            ]),
            130,
        )?;
        ensure(
            artifacts::digest(source)? == artifacts::digest(&readback)?,
            &format!("anonymous download differs: {name}"),
        )?;
        std::fs::remove_file(&readback).map_err(|e| e.to_string())?;
    }
    point_latest(destination, stage)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_access_allows_get_only_and_refuses_listing_and_admin_roles() {
        for role in [
            "roles/storage.objectViewer",
            "roles/storage.admin",
            "roles/storage.objectCreator",
        ] {
            let policy = serde_json::json!({"bindings":[{"role":role,"members":["allUsers"]}]});
            assert!(public_policy(&policy).is_err());
        }
        let policy =
            serde_json::json!({"bindings":[{"role":PUBLIC_READ_ROLE,"members":["allUsers"]}]});
        assert!(public_policy(&policy).is_ok());
        let private = serde_json::json!({"bindings":[{"role":"roles/storage.admin","members":["projectOwner:fixture"]}]});
        assert!(public_policy(&private).is_ok());
    }

    #[test]
    fn public_manifest_binds_all_platforms_and_instructions_need_no_accounts() {
        let dest = Destination::new("rift-public-fixture", "opaque-fixture").unwrap();
        let tag = "v1.0.0-alpha";
        let entries = artifacts::TARGETS
            .iter()
            .map(|t| (artifacts::name(tag, t), "a".repeat(64)))
            .collect();
        let text = manifest(&dest, tag, &entries).unwrap();
        assert_eq!(text.lines().count(), 6);
        assert!(text.starts_with("rift-download-v1\tv1.0.0-alpha\n"));
        for target in artifacts::TARGETS {
            assert!(text.contains(target));
        }
        let commands = instructions(&dest, tag);
        assert!(commands.contains("curl -fsS https://storage.googleapis.com/"));
        assert!(!commands.contains("-fsSL"));
        assert!(commands.contains("rift update --from"));
        assert!(commands.contains("no expiry"));
    }

    #[test]
    fn cloud_destinations_refuse_traversal_and_the_private_bucket() {
        assert!(Destination::new("rift-fixture", "releases/opaque/v1.0.0-alpha").is_ok());
        assert!(Destination::new("rift-fixture", ".").is_err());
        assert!(Destination::new("rift-fixture", "../escape").is_err());
        assert!(Destination::new("rift-fixture", "a?b").is_err());
        assert!(Destination::new("rift-fixture;bad", "valid").is_err());
        let config =
            crate::read_toml(&crate::workspace_root().join("release/storage.toml")).unwrap();
        assert!(Destination::new(config["bucket"].as_str().unwrap(), "valid").is_err());
    }
}
