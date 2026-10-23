"""Offline installer safety checks; executables are disposable and HOME is preserved."""

import hashlib
import io
import os
from pathlib import Path
import subprocess
import tarfile
import tempfile
import unittest


ROOT = Path(__file__).resolve().parent.parent


class InstallerSafety(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.commands = self.root / "commands"
        self.commands.mkdir()
        self.assets = self.root / "assets"
        self.assets.mkdir()
        self.bin = self.root / "bin"
        self.bin.mkdir()
        curl = self.commands / "curl"
        curl.write_text(
            "#!/usr/bin/env python3\n"
            "import os, pathlib, shutil, sys\n"
            "args = sys.argv[1:]\n"
            "destination = args[args.index('--output') + 1]\n"
            "name = args[-1].rsplit('/', 1)[-1].split('?', 1)[0]\n"
            "if name.endswith('.tar.gz') and os.environ.get('FIXTURE_EDIT_DURING_FETCH'):\n"
            " pathlib.Path(os.environ['RIFT_BIN_DIR'], 'rift').write_bytes(b'edited while downloading')\n"
            "shutil.copyfile(pathlib.Path(os.environ['FIXTURE_ASSETS']) / name, destination)\n"
        )
        curl.chmod(0o755)
        self.env = dict(os.environ, RIFT_NONINTERACTIVE="1", RIFT_BIN_DIR=str(self.bin),
                        RIFT_DOWNLOAD_MANIFEST="https://storage.googleapis.com/fixture/downloads.tsv", FIXTURE_ASSETS=str(self.assets),
                        PATH=str(self.commands) + os.pathsep + os.environ["PATH"])
        self.make_archive("v1.0.0-alpha")

    def make_archive(self, version):
        system = os.uname()
        arch = "aarch64" if system.machine in {"arm64", "aarch64"} else "x86_64"
        platform = "apple-darwin" if system.sysname == "Darwin" else "unknown-linux-gnu"
        archive = self.assets / f"rift-{version}-{arch}-{platform}.tar.gz"
        data = f"#!/bin/sh\nprintf 'rift {version[1:]}\\n'\n".encode()
        with tarfile.open(archive, "w:gz") as bundle:
            member = tarfile.TarInfo("rift")
            member.size = len(data)
            member.mode = 0o755
            bundle.addfile(member, io.BytesIO(data))
        checksum = hashlib.sha256(archive.read_bytes()).hexdigest()
        (self.assets / "downloads.tsv").write_text(
            f"rift-download-v1\t{version}\n{arch}-{platform}\thttps://storage.googleapis.com/fixture/{archive.name}\t{checksum}\n"
        )
        return archive

    def install(self, success=True):
        result = subprocess.run(["sh", str(ROOT / "install.sh")], env=self.env,
                                text=True, capture_output=True, timeout=15)
        self.assertEqual(result.returncode == 0, success, result.stdout + result.stderr)
        self.assertFalse((self.bin / ".rift-install.lock").exists())
        self.assertEqual(list(self.bin.glob(".rift-install.*")), [])
        return result

    def test_fresh_install_and_update_leave_matching_receipts(self):
        self.install()
        before = (self.bin / "rift").read_bytes()
        self.install()  # Same-version updates are safe and idempotent.
        self.make_archive("v1.0.0-alpha.1")
        self.env.update(RIFT_VERSION="v1.0.0-alpha.1", RIFT_UPDATE="1")
        self.install()
        binary = (self.bin / "rift").read_bytes()
        receipt = (self.bin / "rift.install-receipt").read_text().splitlines()
        self.assertNotEqual(before, binary)
        self.assertEqual(receipt, ["rift-install-v1", hashlib.sha256(binary).hexdigest(),
                                   "v1.0.0-alpha.1"])

    def test_foreign_and_modified_executables_are_preserved(self):
        binary = self.bin / "rift"
        binary.write_bytes(b"developer-owned")
        self.install(success=False)
        self.assertEqual(binary.read_bytes(), b"developer-owned")
        binary.unlink()
        self.install()
        binary.write_bytes(b"modified")
        self.install(success=False)
        self.assertEqual(binary.read_bytes(), b"modified")

    def test_edits_during_download_are_preserved(self):
        self.install()
        receipt = (self.bin / "rift.install-receipt").read_bytes()
        self.make_archive("v1.0.0-alpha.1")
        self.env["FIXTURE_EDIT_DURING_FETCH"] = "1"
        self.install(success=False)
        self.assertEqual((self.bin / "rift").read_bytes(), b"edited while downloading")
        self.assertEqual((self.bin / "rift.install-receipt").read_bytes(), receipt)

    def test_bad_checksum_and_linked_destinations_are_refused(self):
        manifest = self.assets / "downloads.tsv"
        manifest.write_text(manifest.read_text().rsplit("\t", 1)[0] + "\t" + "0" * 64 + "\n")
        self.install(success=False)
        self.assertFalse((self.bin / "rift").exists())
        target = self.root / "unrelated"
        target.write_bytes(b"preserve")
        (self.bin / "rift").symlink_to(target)
        self.install(success=False)
        self.assertEqual(target.read_bytes(), b"preserve")


if __name__ == "__main__":
    unittest.main()
