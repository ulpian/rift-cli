# Private alpha distribution

GitHub holds the private source tag and owner-created prerelease. Google Cloud Storage
holds the downloadable archives. Testers receive bearer links that expire within
12 hours; no GitHub or Google account is needed. Invitations are sent manually.

The existing Rift Google Cloud project, bucket, region, and signer are recorded in
`storage.toml`. The bucket enforces public-access prevention and uniform IAM.
The dedicated signer has object-reader access to this bucket. The release
operator can impersonate only that signer; no service-account key is created.
`storage-lifecycle.json` removes invitation files after two days and preview
artifacts after thirty days. Released archives remain available for reissues.

## Publish after review and verification

Keep `release/alpha-version` (with its `v` prefix) aligned with
`[workspace.package].version` in `Cargo.toml`. All workspace packages inherit
that version; the packaging tool rejects a mismatch.

1. Complete the approved repository gate and review the working setup, Canopy,
   and `NOTES.md` before creating or pushing `v1.0.0-alpha`.
2. Pushing the tag starts the five-platform packaging workflow. It uploads a
   `rift-release-bundle` artifact with archives, checksums, installers, and release
   notes. It has read-only repository permissions and does not create or publish
   a GitHub Release.
3. After inspecting the platform results, download the bundle from GitHub Actions
   and create a prerelease for the existing tag through GitHub's web interface.
   Use `NOTES.md` as its description and attach the verified archives and checksums.
   Put the five archives and `SHA256SUMS` in local `dist/` for the upload step below.
4. Run `python3 release/downloads.py upload`. Every archive must match the CI
   checksum. Uploads preserve existing objects and independently read them back.
5. Run `python3 release/downloads.py invite`. It writes a private `INVITE.md`
   outside the repository containing install and update commands. Send that file's
   instructions to the selected testers yourself. Do not commit or log its links.
6. Reissue invitations with the same command when links expire. Delete the local
   invitation file after use; cloud invitation files expire through lifecycle.

An invitation carries a versioned TSV download manifest: its first line is
`rift-download-v1<TAB>version`; each remaining row is
`target<TAB>signed HTTPS archive URL<TAB>SHA-256`. Installers select one exact
platform row and verify the archive before executing or replacing anything.
The receipt contains only executable ownership, checksum, and version.

The owner tools require Python 3.11+, authenticated `gcloud`, and the source Git
tag. Testers need only the native installer prerequisites. No email is sent by
these tools. OS signing and notarization remain outside this alpha.
