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
- Setup is a standalone terminal wizard before the dashboard registry, data bus,
  or view starts. Settings never embeds the setup plan. Cancel exits without
  loading Canopy; successful verified setup is the only path into the dashboard.
- Selecting Set up project expands Confirm setup and Back below a blank line. Both
  work by keyboard and mouse; buffered input never confirms a newly shown step.
  Inactive review text turns gray. Enter on Confirm is the final Enter, including
  when raw transcript retention is enabled: its warning is visible beforehand,
  and Confirm explicitly acknowledges keeping raw transcripts locally.
- Show a step-based progress bar from native setup reports, including skipped
  steps. Reserve 100% for verified success; show failures and require confirmation
  again before a retry.
- Setup choices are compact keyboard- and mouse-operated rows with white empty
  circles when unselected and solid green circles when selected. There are no
  option frames; harness selection remains multi-select. Review settings use
  leading green/red circles for on/off. Permission labels use yellow for Keep
  current, blue for Restricted, and orange for Workspace edits.
- Welcome/setup uses a compact, abstract 6x4 white block-tile R. Tiles gently fade between gray and white
  at staggered times, keeping the R readable. Respect reduced motion and terminal
  capabilities. Established projects open directly, without a startup splash.

## Canopy

- Replace the permanent project rail and side-by-side content columns with a
  full-width main view, top tabs, and the existing bottom command panel.
- Tabs: Home, Models, Activity, Token usage, Recall viewer, Settings. Home first.
- A clickable left-arrow opens a project drawer with nested worktrees. Keyboard
  navigation reaches the same acts. This is a full-screen picker, with repository
  path headings, nested worktrees, status, and a details preview. Arrows highlight;
  Enter/click selects and retargets commands. Esc keeps the active project.
- Tab/Shift-Tab cycle All, Repositories, and Worktrees filters while the project
  picker is open. Elsewhere they cycle the top views, including Recall and Settings.
  Keep project information in the cards; omit the extra path and scope header rows.
- Use the terminal's default background. The onboarding R visibly shimmers at a
  bounded cadence; it becomes solid white with reduced motion. No logo animation
  runs behind the dashboard or Settings. Apple Terminal uses its 256-color
  palette with diffed updates; never advertise or send unsupported RGB colors.
- Launch selects the actual launch worktree. Display activity/usage for the selected
  worktree; do not relabel family aggregates as local values.
- Slash opens command suggestions; Backspace/Delete at the empty prompt closes
  them. Suggestion scrolling and clicks share the visible window. Recall period
  chips (24h, 7d, 30d, all) are clickable and also select with keys 1–4 on Home.
- Repository has no Recall Viewer or Project Settings buttons; use the top tabs.
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

The 2026-10-23 UI correction follows Codex's separate
[onboarding host](https://github.com/openai/codex/blob/main/codex-rs/tui/src/onboarding/onboarding_screen.rs)
and [picker geometry](https://github.com/openai/codex/blob/main/codex-rs/tui/src/resume_picker/layout.rs).
Rift retains its own render backend and project lifecycle manager.
