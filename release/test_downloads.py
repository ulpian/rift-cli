"""Offline checks for invitation contents and release admission."""

from pathlib import Path
import shutil
from types import SimpleNamespace
import tempfile
import unittest
from unittest.mock import patch

import downloads


class PrivateDownloads(unittest.TestCase):
    def test_manifest_binds_each_platform_to_its_checksum_and_link(self):
        tag = "v1.0.0-alpha"
        entries = {}
        for target in downloads.packaging.TARGETS:
            suffix = "zip" if "windows" in target else "tar.gz"
            entries[f"rift-{tag}-{target}.{suffix}"] = "a" * 64
        with patch.object(downloads, "signed", side_effect=lambda p: "https://storage.googleapis.com/fixture/" + p):
            text = downloads.manifest(tag, entries)
        lines = text.splitlines()
        self.assertEqual(lines[0], "rift-download-v1\tv1.0.0-alpha")
        self.assertEqual(len(lines), 6)
        for line in lines[1:]:
            target, url, sha = line.split("\t")
            self.assertIn(target, downloads.packaging.TARGETS)
            self.assertTrue(url.startswith("https://storage.googleapis.com/fixture/releases/"))
            self.assertEqual(sha, "a" * 64)

    def test_invitations_need_no_github_credentials_and_explain_renewal(self):
        text = downloads.invitation_text("v1.0.0-alpha", {
            "install.sh": "https://storage.googleapis.com/fixture/install.sh?signature=example",
            "install.ps1": "https://storage.googleapis.com/fixture/install.ps1?signature=example",
        }, "https://storage.googleapis.com/fixture/downloads.tsv?signature=example")
        self.assertIn("12 hours", text)
        self.assertIn("rift update --from", text)
        self.assertIn("No GitHub or Google account is required", text)
        self.assertNotIn("gh auth", text)

    def test_renewal_uses_cloud_release_without_local_archives(self):
        tag = "v1.0.0-alpha"
        names = [f"rift-{tag}-{target}." + ("zip" if "windows" in target else "tar.gz")
                 for target in sorted(downloads.packaging.TARGETS)]
        checksums = "".join(f"{'a' * 64}  {name}\n" for name in names)
        recorded_paths = []
        original_mkdtemp = tempfile.mkdtemp

        def temporary(*args, **kwargs):
            path = original_mkdtemp(*args, **kwargs)
            recorded_paths.append(Path(path))
            self.addCleanup(shutil.rmtree, path, True)
            return path

        def cloud_or_tag(*args):
            if args[:3] == ("gcloud", "storage", "cat"):
                return checksums
            if args[:3] == ("gcloud", "storage", "ls"):
                return "\n".join(downloads.remote(f"releases/{tag}/{name}")
                                 for name in [*names, "SHA256SUMS", "NOTES.md"])
            if args[:2] == ("git", "show"):
                return "# installer: RIFT_DOWNLOAD_MANIFEST\n"
            if args[:3] == ("gcloud", "storage", "cp"):
                return ""
            raise AssertionError(args)

        with patch.object(downloads, "command", side_effect=cloud_or_tag), \
             patch.object(downloads, "assets", side_effect=AssertionError("local archives read")), \
             patch.object(downloads, "signed", side_effect=lambda path: "https://storage.googleapis.com/fixture/" + path), \
             patch.object(downloads.uuid, "uuid4", return_value=SimpleNamespace(hex="fixture")), \
             patch.object(downloads.tempfile, "mkdtemp", side_effect=temporary), \
             patch("builtins.print"):
            downloads.invitation()
        invites = [path / "INVITE.md" for path in recorded_paths if (path / "INVITE.md").exists()]
        self.assertEqual(len(invites), 1)
        text = invites[0].read_text()
        self.assertIn("curl --proto", text)
        self.assertIn("Read the release notes", text)
        self.assertIn("rift update --from", text)

    def test_upload_refuses_a_checkout_that_does_not_match_the_release_tag(self):
        with patch.object(downloads, "command", side_effect=["current", "old-tag"]), \
             patch.object(downloads, "assets") as assets:
            with self.assertRaisesRegex(ValueError, "check out the release tag"):
                downloads.upload(Path("not-read"))
            assets.assert_not_called()

    def test_missing_platforms_are_refused_before_upload(self):
        with tempfile.TemporaryDirectory() as temp:
            dist = Path(temp)
            (dist / "SHA256SUMS").write_text("")
            with self.assertRaisesRegex(ValueError, "all five"):
                downloads.assets(dist)


if __name__ == "__main__":
    unittest.main()
