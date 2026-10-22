"""Package a verified native CLI using only Python's standard library."""

import hashlib
import os
import pathlib
import re
import subprocess
import sys
import tarfile
import tomllib
import zipfile

ROOT = pathlib.Path(__file__).resolve().parent.parent


def version():
    tag = (ROOT / "release/alpha-version").read_text().strip()
    if not re.fullmatch(r"v1\.0\.0-alpha(?:\.[0-9]+)?", tag):
        raise ValueError("invalid alpha version")
    package = tomllib.loads((ROOT / "bins/rift/Cargo.toml").read_text())["package"]
    if package["version"] != tag[1:]:
        raise ValueError("CLI version and release channel disagree")
    return tag


TARGETS = {
    "aarch64-apple-darwin", "x86_64-apple-darwin",
    "aarch64-unknown-linux-gnu", "x86_64-unknown-linux-gnu",
    "x86_64-pc-windows-msvc",
}


def package(target):
    if target not in TARGETS:
        raise ValueError("unsupported release target")
    tag = version()
    name = "rift.exe" if "windows" in target else "rift"
    target_root = pathlib.Path(os.environ.get("CARGO_TARGET_DIR", ROOT / "target"))
    if not target_root.is_absolute():
        target_root = ROOT / target_root
    binary = target_root / target / "release" / name
    reported = subprocess.check_output([str(binary), "--version"], text=True, timeout=10).strip()
    if reported != "rift " + tag[1:]:
        raise ValueError("built executable has the wrong version")
    out = ROOT / "dist"
    out.mkdir(exist_ok=True)
    stem = f"rift-{tag}-{target}"
    if name.endswith(".exe"):
        archive = out / (stem + ".zip")
        with zipfile.ZipFile(archive, "w", compression=zipfile.ZIP_DEFLATED) as bundle:
            bundle.write(binary, name)
    else:
        archive = out / (stem + ".tar.gz")
        with tarfile.open(archive, "w:gz") as bundle:
            bundle.add(binary, arcname=name, recursive=False)
    print(archive.name)


def checksums():
    out = ROOT / "dist"
    assets = sorted([*out.glob("*.tar.gz"), *out.glob("*.zip")])
    tag = version()
    expected = {f"rift-{tag}-{target}." + ("zip" if "windows" in target else "tar.gz")
                for target in TARGETS}
    if {asset.name for asset in assets} != expected:
        raise ValueError("expected the five current-version platform archives before drafting a release")
    lines = []
    for asset in assets:
        with asset.open("rb") as stream:
            digest = hashlib.file_digest(stream, "sha256").hexdigest()
        lines.append(f"{digest}  {asset.name}\n")
    (out / "SHA256SUMS").write_text("".join(lines), encoding="ascii", newline="\n")


if __name__ == "__main__":
    if sys.argv[1:] == ["--checksums"]:
        checksums()
    elif len(sys.argv) == 2:
        package(sys.argv[1])
    else:
        raise SystemExit("usage: package.py TARGET | --checksums")
