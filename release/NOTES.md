# Rift v1.0.0-alpha

Rift brings agent-session capture, provenance, Recall, and usage visibility into
your terminal. This is a private alpha for invited testers. Expect rough edges and
report problems with your operating system, harness version, and `rift doctor`
output after checking that it contains no private project information.

## Start in your project

Install a prebuilt executable; Rust is not required. Git is required for projects.

Use the expiring installation command in your invitation email. macOS and Linux
use `curl … | sh`; Windows uses PowerShell. Your invitation chooses the correct
platform automatically. No GitHub or Google account is required, and testers
receive no source-repository access.

Links expire within 12 hours. Request a fresh invitation when needed. Anyone
holding a link can use it until expiry, so keep it private.

Follow the installer's PATH instructions, change to your project, and run `rift`.
Review the detected harnesses, project settings, and supported permission choices
before setup is applied. Outside Git, Rift offers to initialize a repository and
does not create an initial commit. Historical session import is a separate opt-in.

## A project-focused Canopy

- Home stacks compact Repository and Recall summaries in one full-width view.
- Top tabs open Models, Activity, Token usage, Recall viewer, and Settings.
- The left-arrow project drawer groups repositories and their linked worktrees.
  Launching inside a worktree selects it; switching projects retargets commands.
- The bottom command panel keeps the existing CLI-backed workflow.
- Setup welcomes you with a white tiled R and a gentle staggered shimmer.
  Established projects open directly into Canopy.

Dispatch, Recap, and System no longer occupy dashboard navigation. Existing CLI
commands remain available. Model information is read-only.

## Installation lifecycle

- Use `rift update --from <fresh update link>` from a release invitation.
  No background updater or stored download credentials.
- Run `rift uninstall` to remove the managed installation and owned integrations
  while preserving captured data. Purging local data requires explicit confirmation.
- SHA-256 checksums accompany all release archives. OS publisher signing and
  notarization are not included in this alpha.

## Compatibility and limits

Capture support in this build is limited to Claude Code, Codex, and Antigravity.
Other detected adapters are labelled according to their actual support status;
Pi is an interface reference and has no new adapter in this release.

Packaging targets ARM64 and Intel macOS, ARM64 and x86-64 Linux, and x64 Windows.
The Windows package uses x64 emulation on ARM64 Windows. Linux x86-64 packages
require glibc 2.35 or newer; Linux ARM64 packages require glibc 2.39 or newer.
Alpine/musl Linux is not covered by these packages.

Rift is local software. Its canonical capture records are append-only and pass
through the redaction gate. Recorded usage and savings may be unavailable where
the harness supplies no suitable evidence; unavailable values are not zero.
Uninstalling does not rewrite captured Git history or remove repository policy.

## Validation and availability

Focused tests and native terminal checks passed on Intel macOS: staged setup,
Git initialization consent, cancellation, the tiled R, tab navigation, and
launching in linked worktrees. Private Cloud Storage downloads were checked for
unsigned-access denial, signed retrieval, archive integrity, installation,
same-version update, and uninstall/purge in a disposable installation.

The full workspace format/Clippy/test gate and five-platform optimized release
builds are not yet verified. The available Intel Mac preview uses reduced CLI
optimization with optimized dependencies. Treat other platforms as unverified
until their build results and release archives are available.
