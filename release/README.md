# Release tooling

Release tools are Rust commands in `xtask/src/release/`. They reuse the actual
Rift executable and the top-level shell/PowerShell bootstraps. Testers do not need
Rust. The bootstraps must remain shell/PowerShell because Rift is not installed
when they run. Owner tooling invokes authenticated `gcloud` for cloud operations;
Google owns the SDK's interpreter dependency.

## Current publication status

`v1.0.0-alpha` is published under prefix `a` and `v1.1.0-alpha` under
prefix `v1.1.0-alpha` of the bucket in `public-storage.toml`. Each prefix holds the five
archives, `SHA256SUMS`, `downloads.tsv`, `NOTES.md`, `VERIFICATION.json` and
both installer wrappers. Only Intel macOS was exercised natively; the owner
authorised publishing the other four cross-builds before native checks, and
`VERIFICATION.json` records which checks ran. GitHub Actions packaging is
blocked by account billing; local tooling does not require Actions.

Cross-builds run from this Mac with the kit under `target/alpha-cross/`
(`cargo-zigbuild` with Linux glibc pinned to 2.28, `cargo-xwin` with the
static CRT for Windows), against a `git archive` of the tag.

The existing private bucket in `storage.toml` is retained unchanged. Its public
access prevention, uniform IAM and signing account belong to the earlier private
invitation flow. New public downloads use a **separate** bucket. Obscure paths
are not access control: anyone holding or finding a URL can download it, forward
it and continue using it until its object is removed. Public downloads incur
normal GCS storage and network charges.

## Test without downloading

`cargo build --workspace` builds local development binaries. `cargo run -p rift
-- --help` runs the current code. Neither command exercises the bootstrap,
network download, archive selection, checksum verification, installation receipt,
PATH, update or uninstall. Debug binaries must not become installed hook targets.

For real project setup use a durable release binary. `./skills.sh release` builds
the repository's production release path; rebuild only when you intend to update
that executable. The previews below select a binary matching the current release version under
`CARGO_TARGET_DIR` or `target/`: first `release/rift`, then
`<native-target>/release/rift`, then the local cross-build cache at
`alpha-cross/target/<native-target>/release/rift` (`rift.exe` on Windows).
Stale defaults are skipped; the selected path is printed. Use `--binary PATH`
for strict explicit selection. They never build or replace
your installed Rift, preserve HOME, and isolate configuration, cache,
projects, hooks and downloaded binaries. Project previews require a release
binary outside system temp and `target/debug`, preserving the hook safety guard.
An explicit version mismatch is refused. If no matching native build exists,
the command prints the build command and never rebuilds or replaces a binary
automatically.

```sh
# Requires a current release build; use --binary PATH to select another one.
cargo rift preview install
cargo rift preview setup
cargo rift preview dashboard

# Real shell installer, receipt, update and uninstall using a local transport:
cargo rift check-install

# Piped installer, shimmer, cancellation/defaults, setup and Settings through a PTY:
cargo rift check-flows

# Tiny offline ownership, checksum, concurrent-edit and symlink safety cases:
cargo rift check-safety
cargo test -p xtask release::
cargo test -p xtask --test release_install
```

Interactive previews inherit your actual terminal. Automated PTY checks use Apple
Terminal's palette environment, including a stale truecolor variable. They check
text and interaction, not screenshots or native Windows rendering. PTY and shell
installer previews currently require macOS/Linux. Temporary assets are removed
on normal exit or a handled flow failure. No cloud access is used.

The native installer chooses default harnesses, shows the shimmering R and
milestones, and saves the reviewed choice under
`<config-root>/rift/defaults.toml`. Run `rift install` to change it later.
It has no feature page: every feature ships on, and each project switches its
own off in Project Settings. The file's `features` table is still written so
older builds keep reading it, but nothing reads it back.
New project setup reviews permissions separately and preserves declared project
policy. `curl | sh` obtains wizard input from the terminal device; unattended
installs print the command for resuming preferences. Updates skip onboarding.

## Package and record native checks

Keep `release/alpha-version`, workspace version and the CLI version aligned.
The packaging command requires a native host matching the target, checks
`rift --version`, then creates one archive containing only `rift`/`rift.exe`:

```sh
cargo rift package x86_64-apple-darwin \
  --binary /absolute/durable/release/rift --out dist
```

Use `aarch64-apple-darwin`, `x86_64-apple-darwin`,
`aarch64-unknown-linux-gnu`, `x86_64-unknown-linux-gnu`, or
`x86_64-pc-windows-msvc` on the corresponding native machine.
Cross-built binaries are not runtime-verified merely by inspecting their format.

After exercising each package on its native OS/CPU, record a minimum smoke check:

```sh
cargo rift native-check x86_64-apple-darwin \
  --binary /absolute/durable/release/rift \
  --archive dist/rift-v1.0.0-alpha-x86_64-apple-darwin.tar.gz \
  --out tmp/native-evidence
```

This executes version/help and verifies that the archive contains exactly the
same executable. Records bind target, version, tag commit and archive SHA-256.
They are local operator evidence, not remote attestation or complete OS acceptance;
review actual install/setup behavior on each OS as well. Collect all five records
in one evidence directory. Changing archive bytes requires rerunning its check.

Official publication uses four Unix `.tar.gz` files and one Windows `.zip`.
Additional Unix ZIPs can remain in the development folder. The checksum command
writes exactly the five official entries and fails if any is missing:

```sh
cargo rift checksums --dist tmp/rift-alpha/cross-builds
```

## Prepare a public GCS plan

Choose a fresh random bucket name and a short release path. The following are **templates**, not
live download links. Use a separate bucket in the existing project; never disable
controls on the private bucket. Bucket creation is an explicit operator step:

```sh
export CLOUDSDK_PYTHON=/usr/local/bin/python3  # only if needed by your Cloud SDK
export CLOUDSDK_COMPONENT_MANAGER_DISABLE_UPDATE_CHECK=1
export RIFT_PUBLIC_BUCKET="rift-e59ea04337d9"  # reserved in public-storage.toml
export RIFT_PUBLIC_PREFIX="a"  # immutable shorthand for v1.0.0-alpha

# Local generation only; no cloud mutation:
cargo rift plan \
  --dist tmp/rift-alpha/cross-builds \
  --bucket "$RIFT_PUBLIC_BUCKET" --prefix "$RIFT_PUBLIC_PREFIX"

# The recorded bucket already exists. For a different new download bucket:
gcloud storage buckets create "gs://$RIFT_PUBLIC_BUCKET" \
  --project=gen-lang-client-0144758115 --location=europe-west1 \
  --uniform-bucket-level-access --no-public-access-prevention
```

This project requires uniform bucket-level access. After all native checks pass,
the uploader grants `allUsers` only `roles/storage.legacyObjectReader`. That
Google-managed role contains `storage.objects.get`, without listing or writing.
It verifies the role permissions before granting it and checks that anonymous
bucket listing stays denied. Do not substitute `roles/storage.objectViewer`,
which also allows listing. Only public release material belongs in this bucket.
Organization-level public access prevention can still prohibit public reads;
the uploader fails rather than changing organization policy.

The local plan lives under ignored `tmp/rift-download-plan/<bucket>/<prefix>/`.
Review `DOWNLOADS.md`, the platform/checksum manifest, and both installer wrappers.
Installers come from the exact binary source tag. Versioned release notes come
from the committed tooling checkout, allowing reviewed documentation corrections
without moving the binary tag. Plan URLs become usable only
after upload. A plan is not publication.

## Manifest signing and `latest/`

Every `downloads.tsv` is signed with an Ed25519 key. The private half is the
Secret Manager secret `rift-release-manifest-key` in the release project; it
was created once with `cargo rift signing-key create`, which pipes a fresh
seed straight into the secret and prints the public half. That public key is
`manifest_public_key` in `public-storage.toml` and `MANIFEST_PUBLIC_KEY` in
`bins/rift/src/installation/release_feed.rs` (a test keeps them equal).
`cargo rift plan` reads the secret through `gcloud` and writes
`downloads.tsv.sig` next to the manifest; it refuses a secret whose public half
differs from the recorded one. `RIFT_RELEASE_SIGNING_KEY_FILE` (64 hex digits)
signs without cloud access. Never replace the secret: installed binaries trust
only the recorded key, so a new key needs a release that carries it first.

`upload` publishes each release under its own prefix, never overwriting, and
then points `latest/downloads.tsv` and `latest/downloads.tsv.sig` at it (cache
life 60 seconds). `rift update` with no link reads `latest/`, verifies the
signature, and hands the installer that exact copy. `rift` and `rift init` do
the same check at most once a day and offer the update. Binaries from
`v1.1.0-alpha` and earlier do not check signatures and have no default link;
they update with `rift update --from` the `latest/downloads.tsv` link of the
release bucket.

The first `curl | sh` install does not verify the signature: the installer
script comes from the same bucket, so a check inside it would add nothing.
Signing protects every update after the first install.

## Upload after native verification and review

```sh
cargo rift upload \
  --dist tmp/rift-alpha/cross-builds --evidence tmp/native-evidence \
  --bucket "$RIFT_PUBLIC_BUCKET" --prefix "$RIFT_PUBLIC_PREFIX"
```

Upload requires the binary release tag to be an ancestor of the tooling checkout,
all five archive checksums, and all five matching native records. The records
bind archives to the binary tag, even when the tooling has newer commits. It preserves existing cloud objects with no-clobber,
reads every object back, and checks bytes before enabling public object reads
on the first publication. It then verifies anonymous HTTPS downloads and denied
anonymous listing. On later uploads, the get-only grant remains active; installers
are copied last so their archives and manifest are ready first. A failed upload can leave partial objects: rerunning the same verified plan
is safe for identical bytes; mismatches stop publication. No rollback deletes an
existing release. Use a fresh prefix to publish changed material.

`DOWNLOADS.md` contains the full concrete commands, including:

```sh
curl -fsS \
  "https://storage.googleapis.com/$RIFT_PUBLIC_BUCKET/$RIFT_PUBLIC_PREFIX/install.sh" | sh

rift update --from \
  "https://storage.googleapis.com/$RIFT_PUBLIC_BUCKET/$RIFT_PUBLIC_PREFIX/downloads.tsv"
```

The short curl command uses a literal HTTPS URL without following redirects.
The published wrappers also name the release bucket, and `rift update` pins
the same bucket at build time (`RELEASE_BUCKET` in
`bins/rift/src/installation.rs`, kept equal to `release/public-storage.toml` by
a test; `cargo rift plan` refuses any other bucket). A link into another bucket
is refused before anything is downloaded, because the manifest that carries
the checksums comes from the same place. An older alpha never replaces a newer
one unless `RIFT_ALLOW_DOWNGRADE=1` is set. The manifest is not yet signed.
The wrapper embeds the manifest URL. The bootstrap detects OS and CPU, selects
exactly one target row, verifies archive and executable versions, installs under
`~/.local/bin` (or `RIFT_BIN_DIR`), and opens native preferences. Windows users get
a PowerShell command instead. `rift uninstall --dry-run` previews removal;
`rift uninstall` applies it.

When the installation directory is not on PATH, the bootstrap registers it. On
macOS and Linux it appends one self-guarding line, ending in `# rift installer`,
to the startup file of the login shell (`.zshrc`; `.bashrc` plus the login
profile bash reads; a fish `conf.d/rift.fish`; `.profile` otherwise), then hands
the terminal to a fresh copy of that shell so `rift` resolves immediately:
a child process cannot change the PATH of the shell that ran `curl | sh`.
On Windows it prepends the directory to the user `Path` value, keeping
`%VARIABLE%` references unexpanded, and updates the PowerShell session that ran
`irm | iex`. A directory name that would need shell quoting is never written
into a startup file; the manual instruction is printed instead. Updates never
touch PATH. `RIFT_NO_MODIFY_PATH=1` opts out of both steps and
`RIFT_NO_SHELL_RESTART=1` keeps the startup-file line but skips the fresh shell;
unattended installs (`RIFT_NONINTERACTIVE=1`, `CI` set, root, or no terminal)
never start one, and the fresh shell gets the caller's own umask back.
Uninstalling removes the marked line, and on Windows the default `Path` entry.

Public links have no expiry and do not need renewal. To issue a new link, choose a
new prefix and repeat plan/upload using the same verified archives. Old
links keep working until you remove their objects. An obscure URL can leak through
logs and sharing; do not put secrets or user data in this bucket.

The current packages are unsigned/unnotarized. From `v1.1.0-alpha` the Linux
binaries are linked against glibc 2.28 (`v1.0.0-alpha` needed 2.35 / 2.39). Windows is x64 MSVC with the
static CRT. These constraints must appear in the release review.

## Pre-push and GitHub

Ask the owner before a broad verification sweep. Once authorized, serialize
`CARGO_BUILD_JOBS=2 ./skills.sh check`, then `./skills.sh gc`. Focused release
checks do not replace this gate. Commit and push only after owner approval.
RPF is the remaining active development work.

The packaging workflow now invokes Rust xtask commands, collects five archives
and checksums, and never creates a GitHub Release. Once verified, create the
prerelease for the approved tag using `notes/<tag>.md` as its description.
