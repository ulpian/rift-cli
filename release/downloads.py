"""Owner-only GCS publishing and expiring invitations. Never sends email."""

import argparse
import hashlib
import os
from pathlib import Path
import shlex
import subprocess
import tempfile
import tomllib
import uuid

import package as packaging

ROOT = Path(__file__).resolve().parent.parent
CONFIG = tomllib.loads((ROOT / "release/storage.toml").read_text())


def command(*args):
    result = subprocess.run(args, cwd=ROOT, capture_output=True, text=True, timeout=180)
    if result.returncode:
        # Signed links are credentials: never echo captured command output.
        raise RuntimeError(f"{args[0]} operation failed; inspect cloud access and object ownership")
    return result.stdout.strip()


def remote(path):
    return f"gs://{CONFIG['bucket']}/{path}"


def signed(path):
    # The signer reads objects only; use the recorded location instead of
    # granting bucket-metadata access just for gcloud's region discovery.
    url = command("gcloud", "storage", "sign-url", remote(path), "--duration=12h", "--path-style-url",
                  f"--region={CONFIG['location']}",
                  f"--impersonate-service-account={CONFIG['signer']}",
                  "--format=value(signed_url)", "--quiet")
    if not url.startswith("https://storage.googleapis.com/") or any(c.isspace() for c in url):
        raise ValueError("signer returned an unexpected download URL")
    return url


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def release_checksums(text):
    tag = packaging.version()
    expected = {f"rift-{tag}-{target}." + ("zip" if "windows" in target else "tar.gz")
                for target in packaging.TARGETS}
    entries = {}
    for line in text.splitlines():
        sha, name = line.split("  ", 1)
        if name in entries or len(sha) != 64 or any(c not in "0123456789abcdef" for c in sha):
            raise ValueError("invalid or duplicate release checksum")
        entries[name] = sha
    if set(entries) != expected:
        raise ValueError("all five current-version platform archives are required")
    return tag, entries


def assets(dist):
    tag, entries = release_checksums((dist / "SHA256SUMS").read_text())
    for name, sha in entries.items():
        if digest(dist / name) != sha:
            raise ValueError(f"release archive checksum mismatch: {name}")
    return tag, entries


def upload(dist):
    tag = packaging.version()
    if command("git", "rev-parse", "HEAD") != command("git", "rev-parse", f"refs/tags/{tag}^{{commit}}"):
        raise ValueError("check out the release tag before uploading its archives")
    tag, entries = assets(dist)
    with tempfile.TemporaryDirectory(prefix="rift-upload-readback-") as temp:
        readback = Path(temp) / "object"
        notes = ROOT / "release/notes" / f"{tag}.md"
        if not notes.is_file():
            raise ValueError("versioned release notes are missing")
        files = [(dist / name, name) for name in [*sorted(entries), "SHA256SUMS"]]
        files.append((notes, "NOTES.md"))
        for source, name in files:
            target = remote(f"releases/{tag}/{name}")
            command("gcloud", "storage", "cp", "--no-clobber", str(source), target, "--quiet")
            command("gcloud", "storage", "cp", target, str(readback), "--quiet")
            if digest(readback) != digest(source):
                raise ValueError(f"cloud readback differs: {name}; existing objects were preserved")
            readback.unlink()
    print(f"Uploaded and independently verified {len(entries)} archives for {tag}.")


def manifest(tag, entries):
    lines = [f"rift-download-v1\t{tag}\n"]
    for target in sorted(packaging.TARGETS):
        suffix = "zip" if "windows" in target else "tar.gz"
        name = f"rift-{tag}-{target}.{suffix}"
        lines.append(f"{target}\t{signed(f'releases/{tag}/{name}')}\t{entries[name]}\n")
    return "".join(lines)


def invitation():
    tag = packaging.version()
    recorded = command("gcloud", "storage", "cat", remote(f"releases/{tag}/SHA256SUMS"))
    _, entries = release_checksums(recorded)
    existing = command("gcloud", "storage", "ls", remote(f"releases/{tag}/")).splitlines()
    required = {remote(f"releases/{tag}/{name}") for name in [*entries, "SHA256SUMS", "NOTES.md"]}
    if not required.issubset(set(existing)):
        raise ValueError("uploaded release is incomplete; upload and verify first")
    shell_source = command("git", "show", f"{tag}:install.sh")
    powershell_source = command("git", "show", f"{tag}:install.ps1")
    if "RIFT_DOWNLOAD_MANIFEST" not in shell_source or "RIFT_DOWNLOAD_MANIFEST" not in powershell_source:
        raise ValueError("release tag does not contain the invitation installers")
    with tempfile.TemporaryDirectory(prefix="rift-invite-upload-") as temp:
        stage = Path(temp)
        prefix = f"invites/{uuid.uuid4().hex}"
        (stage / "downloads.tsv").write_text(manifest(tag, entries), encoding="utf-8")
        command("gcloud", "storage", "cp", str(stage / "downloads.tsv"), remote(f"{prefix}/downloads.tsv"), "--quiet")
        update_link = signed(f"{prefix}/downloads.tsv")
        shell = "#!/bin/sh\nexport RIFT_DOWNLOAD_MANIFEST=" + shlex.quote(update_link) + "\n"
        shell += shell_source
        powershell = "$RiftInvitePreviousManifest = $env:RIFT_DOWNLOAD_MANIFEST\ntry {\n"
        powershell += "$env:RIFT_DOWNLOAD_MANIFEST = '" + update_link.replace("'", "''") + "'\n"
        powershell += powershell_source
        powershell += "\n} finally { $env:RIFT_DOWNLOAD_MANIFEST = $RiftInvitePreviousManifest }\n"
        (stage / "install.sh").write_text(shell, encoding="utf-8")
        (stage / "install.ps1").write_text(powershell, encoding="utf-8")
        links = {"notes": signed(f"releases/{tag}/NOTES.md")}
        for name in ["install.sh", "install.ps1"]:
            command("gcloud", "storage", "cp", str(stage / name), remote(f"{prefix}/{name}"), "--quiet")
            links[name] = signed(f"{prefix}/{name}")
    destination = Path(tempfile.mkdtemp(prefix="rift-alpha-invite-")) / "INVITE.md"
    destination.write_text(invitation_text(tag, links, update_link), encoding="utf-8")
    print(f"Private invitation instructions: {destination}")
    print("Links expire in 12 hours and can be forwarded. No email was sent.")


def invitation_text(tag, links, update_link):
    return (f"# Rift {tag}\n\nThese private download links expire in 12 hours. "
            "Keep them private; anyone holding a link can use it until expiry.\n\n"
            "macOS / Linux:\n\n```sh\n"
            f"curl --proto '=https' -fsSL {shlex.quote(links['install.sh'])} | sh\n```\n\n"
            "Windows PowerShell:\n\n```powershell\n"
            f"irm '{links['install.ps1']}' | iex\n```\n\n"
            "Run `rift` in your project and review setup before applying it.\n\n"
            "Update using the fresh update link provided with a release:\n\n```sh\n"
            f"rift update --from {shlex.quote(update_link)}\n```\n\n"
            "No GitHub or Google account is required. An expired invitation needs a new link.\n"
            + (f"\n[Read the release notes]({links['notes']})\n" if "notes" in links else ""))


def main():
    os.umask(0o077)
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("operation", choices=["upload", "invite"])
    parser.add_argument("--dist", type=Path, default=ROOT / "dist")
    args = parser.parse_args()
    if args.operation == "upload":
        upload(args.dist)
    else:
        invitation()


if __name__ == "__main__":
    main()
