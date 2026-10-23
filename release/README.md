# Private alpha distribution

GitHub holds the private source tag and owner-created prerelease. Google Cloud Storage
holds the downloadable archives. Testers receive bearer links that expire within
12 hours; no GitHub or Google account is needed. Invitations are sent manually.

The existing Rift Google Cloud project, bucket, region, and signer are recorded in
`storage.toml`. The bucket enforces public-access prevention and uniform IAM.
The dedicated signer has object-reader access to this bucket. The release
operator can impersonate only that signer; no service-account key is created.
`storage-lifecycle.json` removes invitation files after two days and preview
artifacts after thirty days. Released archives and their versioned notes remain available for reissues.

## Publish after review and verification

Keep `release/alpha-version` (with its `v` prefix) aligned with
`[workspace.package].version` in `Cargo.toml`. All workspace packages inherit
that version; the packaging tool rejects a mismatch.

1. Complete the approved repository gate and review the working setup, Canopy,
   and `notes/v1.0.0-alpha.md` before creating or pushing `v1.0.0-alpha`.
2. Pushing the tag starts the five-platform packaging workflow. A `release/**`
   preparation branch runs the same workflow, allowing packages to be checked
   before a tag is published or moved. It uploads a
   `rift-release-bundle` artifact with archives, checksums, installers, and release
   notes. It has read-only repository permissions and does not create or publish
   a GitHub Release.
3. After inspecting the platform results, download the bundle from GitHub Actions
   and create a prerelease for the existing tag through GitHub's web interface.
   Use `notes/v1.0.0-alpha.md` as its description and attach the verified archives and checksums.
   Put the five archives and `SHA256SUMS` in local `dist/` for the upload step below.
4. Run `python3 release/downloads.py upload`. Every archive must match the CI
   checksum. Uploads preserve existing objects and independently read them back.
5. Run `python3 release/downloads.py invite`. It writes a private `INVITE.md`
   outside the repository containing install and update commands. Send that file's
   instructions to the selected testers yourself. Do not commit or log its links.
6. Reissue invitations with the same command when links expire. Renewal reads
   the release manifest from GCS, so local platform binaries are not needed. Delete the local
   invitation file after use; cloud invitation files expire through lifecycle.

An invitation carries a versioned TSV download manifest: its first line is
`rift-download-v1<TAB>version`; each remaining row is
`target<TAB>signed HTTPS archive URL<TAB>SHA-256`. Installers select one exact
platform row and verify the archive before executing or replacing anything.
The receipt contains only executable ownership, checksum, and version.

The owner tools require Python 3.11+, authenticated `gcloud`, and the source Git
tag. Testers need only the native installer prerequisites. No email is sent by
these tools. OS signing and notarization remain outside this alpha.

## Test installation locally

Run the real installer and native executable in a disposable directory:

```sh
python3 release/preview_install.py --binary /absolute/path/to/release/rift
```

This checks installation, receipt/checksum, version, a same-version update,
and dry-run plus actual uninstall. Downloads are served from a local fixture;
it does not test hosted GCS links. The existing installation, HOME, projects,
and hooks stay untouched. Temporary executables and archives are removed.

The small offline installer safety suite is `python3 release/test_install.py`.

## Preview onboarding and Settings

Use a freshly built **native release executable** from this checkout. These
scripts do not build, replace your installed Rift, change HOME, or force terminal
colors. Python 3.11+ is required; the interactive previews support macOS/Linux.

```sh
# Actual installer, local download fixture, then animated defaults wizard:
python3 release/preview_flows.py --binary target/release/rift install

# New disposable project; standalone setup before Canopy:
python3 release/preview_flows.py --binary target/release/rift setup

# Initialized demo with a worktree; Shift-Tab opens Settings, Left opens projects:
python3 release/preview_flows.py --binary target/release/rift dashboard

# Bounded native PTY regression: piped installer, shimmer, saved defaults,
# project setup/cancellation, final confirmation and a real Settings toggle:
python3 release/test_flows.py --binary target/release/rift
```

Project setup uses the supplied durable release path: the hook installer correctly
refuses executables under system temporary directories. Installer copies remain
disposable and never become project hook targets.

The interactive previews clean their projects, config, caches, downloaded binaries,
and archives when you exit. Installer previews use local assets in place of GCS;
they are not a hosted-download check. The PTY check verifies terminal output and
interaction, not screenshots or native Windows rendering. It explicitly emulates
Apple Terminal with a stale truecolor environment to check palette compatibility.

Fresh terminal installations open the native wizard after archive/executable
verification. It shows the shimmering R, version, description, update/setup
commands, and a milestone line. Choose multiple default harnesses and feature
switches, review, then confirm. `rift install` reopens those preferences later.
Project setup shows its own milestones, `rift doctor`, and `rift --help`.

Defaults live at `<config-root>/rift/defaults.toml`, beside the project registry
(normally `~/.config/rift/defaults.toml`; `RIFT_CONFIG_DIR` overrides the root).
They preselect a new project's wizard, including harnesses for a configured clone.
Committed policy and pending setup plans
win; permissions and raw-transcript consent are still reviewed per project.
Cancelling preferences preserves the existing file. Invalid or concurrently
changed defaults produce a refusal instead of being overwritten.

`curl | sh` reads wizard input from the terminal device, independently of the
piped script source. On macOS it resolves the concrete device so the event
reader can monitor it. Without a terminal, or with
`RIFT_NONINTERACTIVE=1`, installation finishes and prints `rift install` for later.
Updates preserve defaults and skip onboarding. PowerShell follows the same
interactive/noninteractive rule; native Windows execution requires Windows
validation before release.

## Create Fresh Private Links

After a verified upload, run from the release checkout:

```sh
python3 release/downloads.py invite
```

If needed, select the Cloud SDK's Python interpreter explicitly (this worked on
the release machine without changing cloud configuration or authentication):

```sh
CLOUDSDK_PYTHON="$(command -v python3)" python3 release/downloads.py invite
```

The command prints the path to a private `INVITE.md` outside the repository. It
contains the macOS/Linux curl command, Windows PowerShell command, update command,
and release notes link. Send those instructions manually to the intended testers.
Re-run the command whenever links expire; it does not rebuild or re-upload the
release archives. Keep the invitation file private and delete it after use.

The bucket is private: public access is prevented and uniform bucket IAM is on.
A dedicated service account has read access to the bucket; the release operator
uses impersonation to sign URLs without creating a service-account key. Each
invitation gets a unique prefix with a platform manifest and platform installers.
Those files and the release archives use signed URLs valid for 12 hours. Invite
objects are removed by the two-day bucket lifecycle rule. An email address or
GitHub account is not checked: these are bearer links, so recipients can forward
them and a forwarded link works until it expires. Creating a fresh invitation
does not invalidate earlier links before their expiry.
