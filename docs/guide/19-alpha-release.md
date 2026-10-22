# v1.0.0-alpha release contract

This is the product contract agreed during the release interview on 2026-10-22.
Publication waits for review of the working onboarding, Canopy, and release notes.
Ask the owner before broad test sweeps; use focused checks during implementation.

## Distribution and lifecycle

- Private Git tag and GitHub prerelease named `v1.0.0-alpha`. Source stays private.
- Selected testers receive expiring Google Cloud Storage links by email; they need
  no GitHub or Google account. Links are bearer credentials and can be forwarded.
- Use keyless signing with a dedicated, bucket-scoped signer. Links expire within
  12 hours and are reissued when needed. Do not create a service-account key.
- The owner sends invitations manually; this work does not send email.
- Invitation shell and PowerShell installers download checksum-verified binaries for
  macOS (ARM64/x86-64), Linux (ARM64/x86-64), and native Windows.
- Publish SHA-256 checksums. OS publisher signing/notarization is deferred.
- Installation installs the executable. Interactive setup happens on first `rift`
  in a project, never implicitly in the installer's working directory.
- Updates use `rift update --from <fresh update link>` from a release invitation.
  No background checks or persisted download credentials.
- `rift uninstall` removes the managed installation and Rift-owned integrations,
  preserving data. Explicit confirmed purge also removes machine-local data and
  settings. Never rewrite captured Git history or remove unrelated integrations.

## Project onboarding

- Resolve the enclosing repository/worktree from the launch directory. Provisioning
  is per machine, not inferred from the presence of tracked `.rift/` configuration.
- Outside Git, offer `git init` with explicit consent before setup.
- Detect existing supported harnesses, suggest a multi-selection, explain changes,
  and confirm before applying. Pi is design inspiration, not a new adapter.
- Offer supported harness-native permission presets with exact changes shown;
  keep current permissions by default. Prefer project scope; a global-only change
  requires separate explicit consent. Preserve unrelated settings and refuse conflicts.
- Capture new sessions by default. Historical import is a separate opt-in action.
- Reuse the existing setup/settings services and immutable/redacted storage gates.
- Welcome/setup uses a white block-tile R. Tiles gently fade between gray and white
  at staggered times, keeping the R readable. Respect reduced motion and terminal
  capabilities. Established projects open directly, without a startup splash.

## Canopy

- Replace the permanent project rail and side-by-side content columns with a
  full-width main view, top tabs, and the existing bottom command panel.
- Tabs: Home, Models, Activity, Token usage, Recall viewer, Settings. Home first.
- A clickable left-arrow opens a project drawer with nested worktrees. Keyboard
  navigation reaches the same acts. Selecting switches every project command and
  view to that project. Keep active repository/path and branch visible.
- Launch selects the actual launch worktree. Display activity/usage for the selected
  worktree; do not relabel family aggregates as local values.
- Home has compact stacked Repository and Recall boxes. Repository summarizes
  identity/path, branch, Git state, capture/integration health, active sessions and
  latest checkpoint. Recall summarizes enabled state, readiness/degradation,
  activity and measured usage/savings where available; unknowns remain explicit.
- Reuse existing tab content/actions and adapt layout and scope. Models is read-only.
- Settings retains all existing project controls and adds supported permission
  presets. Group Capture, Recall, Security/Policy and integrations clearly.
- Dispatch, Recap and System disappear from dashboard navigation; their CLI and
  applicable command-palette capabilities remain available.
- Preserve terminal resizing, small-screen behavior, keyboard/mouse parity,
  command text editing, modal close controls, and safe error/cancellation paths.

## Execution and review

- Work in an isolated worktree; preserve unrelated dirty work on main.
- Inspect binary/hook references before cleanup. Preserve the active release binary
  and trust hashes; do not point hooks at debug or disposable preview builds.
- Bounded commands and focused proofs only until a broad gate is approved.
- Present the working experience and honest release notes before tagging/publishing.
- Required repository release checks remain release gates, not implied approvals
  to run a broad sweep. Platform proof and known limitations must be reported.
