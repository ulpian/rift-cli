#!/bin/sh
# Install an invited alpha without changing the current project or agent settings.
set -eu
umask 077

die() { printf 'rift: %s\n' "$*" >&2; exit 1; }
fetch() {
    printf '%s' "$1" | LC_ALL=C grep -Eq '^https://storage\.googleapis\.com/[^[:space:]#]+$' || die 'Use the Google Cloud download link from your invitation.'
    if ! curl --proto '=https' --proto-redir '=https' --tlsv1.2 --fail --location \
        --silent --show-error --connect-timeout 10 --max-time 120 --max-filesize "${3:-268435456}" --output "$2" "$1"; then
        die 'Download unavailable or invitation expired. Request a fresh link.'
    fi
}
digest() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{print $1}'
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$1" | awk '{print $1}'
    else
        die 'SHA-256 verification needs sha256sum or shasum.'
    fi
}

command -v curl >/dev/null 2>&1 || die 'curl is required.'
command -v tar >/dev/null 2>&1 || die 'tar is required.'
case "$(uname -s)" in
    Darwin) platform=apple-darwin ;;
    Linux) platform=unknown-linux-gnu ;;
    *) die 'Use install.ps1 for native Windows. This installer supports macOS and Linux.' ;;
esac
case "$(uname -m)" in
    arm64|aarch64) arch=aarch64 ;;
    x86_64|amd64) arch=x86_64 ;;
    *) die 'This alpha supports ARM64 and x86-64.' ;;
esac

bin_dir=${RIFT_BIN_DIR:-"${HOME:?HOME must be set}/.local/bin"}
case "$bin_dir" in /*) ;; *) die 'RIFT_BIN_DIR must be absolute.' ;; esac
[ ! -L "$bin_dir" ] || die 'Refusing a symbolic-link installation directory.'
mkdir -p "$bin_dir"
lock="$bin_dir/.rift-install.lock"
mkdir "$lock" 2>/dev/null || die 'Another installation is running, or a stale .rift-install.lock needs inspection.'
stage=
cleanup() { [ -z "$stage" ] || rm -rf "$stage"; rmdir "$lock" 2>/dev/null || :; }
trap cleanup EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM
stage=$(mktemp -d "$bin_dir/.rift-install.XXXXXX")
binary="$bin_dir/rift"
receipt="$bin_dir/rift.install-receipt"
[ ! -L "$binary" ] && [ ! -L "$receipt" ] || die 'Refusing symbolic-link binary or receipt; your existing installation was preserved.'

had_binary=0
# Only replace an executable whose bytes match this installer's receipt.
if [ -e "$binary" ]; then
    had_binary=1
    [ -f "$receipt" ] || die 'rift already exists without an installer receipt; choose another RIFT_BIN_DIR.'
    [ "$(sed -n '1p' "$receipt")" = 'rift-install-v1' ] || die 'Unrecognized installation receipt.'
    [ "$(digest "$binary")" = "$(sed -n '2p' "$receipt")" ] || die 'Installed binary differs from its receipt; refusing to overwrite it.'
    cp "$receipt" "$stage/previous-receipt"
elif [ -e "$receipt" ]; then
    die 'An orphaned receipt needs inspection before reinstalling.'
elif [ "${RIFT_UPDATE:-0}" = 1 ]; then
    die 'The managed executable is missing; reinstall explicitly.'
fi

recheck_installation() {
    [ ! -L "$binary" ] && [ ! -L "$receipt" ] || die 'Installation paths changed while downloading.'
    if [ "$had_binary" = 1 ]; then
        [ -f "$binary" ] && [ -f "$receipt" ] || die 'Installation disappeared while downloading.'
        cmp -s "$stage/previous-receipt" "$receipt" || die 'Installation receipt changed while downloading.'
        [ "$(digest "$binary")" = "$(sed -n '2p' "$stage/previous-receipt")" ] || die 'Installed executable changed while downloading; it was preserved.'
    else
        [ ! -e "$binary" ] && [ ! -e "$receipt" ] || die 'Installation paths appeared while downloading; they were preserved.'
    fi
}

[ -n "${RIFT_DOWNLOAD_MANIFEST:-}" ] || die 'A fresh invitation link is required. Use the install command from your invitation email.'
fetch "$RIFT_DOWNLOAD_MANIFEST" "$stage/manifest" 1048576
version=$(awk -F '\t' 'NR == 1 && NF == 2 && $1 == "rift-download-v1" {print $2}' "$stage/manifest")
printf '%s\n' "$version" | LC_ALL=C grep -Eq '^v1\.0\.0-alpha(\.[0-9]+)?$' || die 'Invalid or incompatible alpha invitation.'
target="$arch-$platform"
manifest_value() {
    awk -F '\t' -v target="$target" -v column="$1" '
        NR > 1 && NF == 3 && $1 == target { count++; value=$column }
        END { if (count != 1) exit 1; print value }
    ' "$stage/manifest"
}
archive_url=$(manifest_value 2) || die 'Invitation has no unique download for this platform.'
expected=$(manifest_value 3) || die 'Invitation has no unique checksum for this platform.'
printf '%s\n' "$expected" | LC_ALL=C grep -Eq '^[0-9a-f]{64}$' || die 'Invalid archive checksum in invitation.'
recheck_installation
if [ -f "$binary" ] && [ "$(sed -n '3p' "$receipt")" = "$version" ]; then
    printf 'Rift %s is already installed.\n' "$version"
    exit 0
fi

fetch "$archive_url" "$stage/archive.tar.gz"
[ "$(digest "$stage/archive.tar.gz")" = "$expected" ] || die 'Archive checksum mismatch; existing installation was preserved.'
[ "$(tar -tzf "$stage/archive.tar.gz")" = rift ] || die 'Unexpected archive contents.'
# Stream the named member into our own file: archive links never control a path.
tar -xOzf "$stage/archive.tar.gz" rift > "$stage/rift"
[ -s "$stage/rift" ] || die 'Empty executable in archive.'
chmod 755 "$stage/rift"
[ "$("$stage/rift" --version)" = "rift ${version#v}" ] || die 'Downloaded executable reports a different version.'
checksum=$(digest "$stage/rift")
printf 'rift-install-v1\n%s\n%s\n' "$checksum" "$version" > "$stage/receipt"

recheck_installation
# Keep a rollback copy until both replacements have landed.
if [ -f "$binary" ]; then cp -p "$binary" "$stage/previous"; fi
mv -f "$stage/rift" "$binary"
if ! mv -f "$stage/receipt" "$receipt"; then
    if [ -f "$stage/previous" ]; then mv -f "$stage/previous" "$binary"; else rm -f "$binary"; fi
    die 'Could not commit installation receipt; executable replacement was rolled back.'
fi
printf 'Installed Rift %s at %s\n' "$version" "$binary"
case ":${PATH:-}:" in
    *":$bin_dir:"*) printf 'Run rift inside your project to choose harnesses and permissions.\n' ;;
    *) printf 'Add %s to PATH, then run rift inside your project.\n' "$bin_dir" ;;
esac
