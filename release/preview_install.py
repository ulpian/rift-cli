"""Exercise the real native installer and binary in a disposable directory.

Downloads use a local fixture transport; no cloud credentials, real installation,
or project setup is touched. All generated binaries and archives are cleaned up.
"""

import argparse
import hashlib
import os
from pathlib import Path
import platform
import subprocess
import tarfile
import tempfile

ROOT = Path(__file__).resolve().parent.parent
MANIFEST = "https://storage.googleapis.com/rift-install-preview/downloads.tsv"


def run(command, env, cwd):
    subprocess.run([str(part) for part in command], env=env, cwd=cwd, check=True, timeout=120)


def prepare(root, binary):
    assets = root / "assets"
    assets.mkdir()
    target = ("aarch64" if platform.machine() in {"arm64", "aarch64"} else "x86_64")
    target += "-apple-darwin" if platform.system() == "Darwin" else "-unknown-linux-gnu"
    version = subprocess.check_output([str(binary), "--version"], text=True, timeout=10).strip()
    tag = "v" + version.removeprefix("rift ")
    archive = assets / f"rift-{tag}-{target}.tar.gz"
    print("Preparing local download fixture (fast compression)...", flush=True)
    with tarfile.open(archive, "w:gz", compresslevel=1) as bundle:
        bundle.add(binary, arcname="rift", recursive=False)
    with archive.open("rb") as stream:
        checksum = hashlib.file_digest(stream, "sha256").hexdigest()
    (assets / "downloads.tsv").write_text(
        f"rift-download-v1\t{tag}\n{target}\thttps://storage.googleapis.com/rift-install-preview/{archive.name}\t{checksum}\n"
    )
    commands = root / "commands"
    commands.mkdir()
    curl = commands / "curl"
    curl.write_text(
        "#!/usr/bin/env python3\n"
        "import os, pathlib, shutil, sys, urllib.parse\n"
        "args = sys.argv[1:]\n"
        "name = urllib.parse.urlsplit(args[-1]).path.rsplit('/', 1)[-1]\n"
        "assets = pathlib.Path(os.environ['RIFT_PREVIEW_ASSETS'])\n"
        "source = assets / name\n"
        "assert source.parent == assets and source.is_file(), 'unexpected preview download'\n"
        "shutil.copyfile(source, args[args.index('--output') + 1])\n"
    )
    curl.chmod(0o755)
    return version


def preview(binary):
    if platform.system() not in {"Darwin", "Linux"}:
        raise SystemExit("This native installer preview supports macOS and Linux.")
    with tempfile.TemporaryDirectory(prefix="rift-install-preview-") as temporary:
        root = Path(temporary)
        version = prepare(root, binary)
        env = {key: value for key, value in os.environ.items()
               if not key.startswith(("RIFT_", "GIT_"))}
        env.update(RIFT_BIN_DIR=str(root / "bin"), RIFT_CONFIG_DIR=str(root / "config"),
                   RIFT_CACHE_DIR=str(root / "cache"), RIFT_NO_KEYCHAIN="1",
                   RIFT_NONINTERACTIVE="1", RIFT_DOWNLOAD_MANIFEST=MANIFEST, RIFT_PREVIEW_ASSETS=str(root / "assets"),
                   GIT_CONFIG_GLOBAL="/dev/null", GIT_CONFIG_NOSYSTEM="1",
                   PATH=str(root / "commands") + os.pathsep + os.environ["PATH"])
        installed = root / "bin/rift"
        print("Local installation preview: real installer and binary; downloads served from a local fixture.", flush=True)
        print("1/5 Install into an empty, disposable directory", flush=True)
        run(["sh", ROOT / "install.sh"], env, root)
        receipt = (root / "bin/rift.install-receipt").read_text().splitlines()
        with installed.open("rb") as stream:
            checksum = hashlib.file_digest(stream, "sha256").hexdigest()
        assert receipt == ["rift-install-v1", checksum, "v" + version.removeprefix("rift ")]
        print("2/5 Verify the installed executable and receipt", flush=True)
        run([installed, "--version"], env, root)
        assert not (root / ".rift").exists(), "installation started project setup"
        print("3/5 Verify a same-version managed update", flush=True)
        run([installed, "update", "--from", MANIFEST], env, root)
        print("4/5 Review uninstall without removing anything", flush=True)
        run([installed, "uninstall", "--dry-run"], env, root)
        assert installed.is_file()
        print("5/5 Uninstall the disposable executable", flush=True)
        run([installed, "uninstall"], env, root)
        assert not installed.exists() and not (root / "bin/rift.install-receipt").exists()
        print("PASS: install, receipt, version, same-version update, uninstall. Temporary artifacts cleaned up.", flush=True)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True, help="native Rift executable to install")
    args = parser.parse_args()
    preview(args.binary.expanduser().resolve(strict=True))
