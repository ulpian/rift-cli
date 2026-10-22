"""Offline checks for invitation contents and release admission."""

from pathlib import Path
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

    def test_missing_platforms_are_refused_before_upload(self):
        with tempfile.TemporaryDirectory() as temp:
            dist = Path(temp)
            (dist / "SHA256SUMS").write_text("")
            with self.assertRaisesRegex(ValueError, "all five"):
                downloads.assets(dist)


if __name__ == "__main__":
    unittest.main()
