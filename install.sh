#!/bin/sh
# Install an invited alpha, then choose per-user defaults without changing a project.
set -eu
# Staging and the receipt are private; the shell this may end in gets the user's own mask back.
user_umask=$(umask)
umask 077

die() { printf 'rift: %s\n' "$*" >&2; exit 1; }
fetch() {
    printf '%s' "$1" | LC_ALL=C grep -Eq '^https://storage\.googleapis\.com/[^[:space:]#]+$' || die 'Use the Google Cloud download link from your invitation.'
    # The published installer and `rift update` name the release bucket: a link
    # into any other bucket is refused, however valid its checksums look,
    # because the manifest that carries them came from the same place.
    if [ -n "${RIFT_DOWNLOAD_BUCKET:-}" ]; then
        # curl removes dot segments and decodes nothing, so `bucket/../other`
        # would pass a prefix test and fetch from another bucket: refuse any
        # dot segment, escape or backslash outright.
        case "$1" in
            *'/.'* | *%* | *\\*) die 'This link is not from the Rift release bucket. Use the link from your invitation.' ;;
        esac
        case "$1" in
            "https://storage.googleapis.com/$RIFT_DOWNLOAD_BUCKET/"*) ;;
            *) die 'This link is not from the Rift release bucket. Use the link from your invitation.' ;;
        esac
    fi
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

# The native wizard owns animated UI; the bootstrap reports real download milestones.
progress() { printf '  %s\n' "$1"; }
configure_terminal() {
    # Use the concrete terminal device: macOS kqueue cannot monitor /dev/tty.
    terminal=$(tty <&3) || die 'Cannot resolve terminal. Run rift install to resume.'
    "$binary" install <"$terminal" >"$terminal" 3<&- || die 'Binary installed; preferences incomplete. Run rift install to resume.'
}
preferences() {
    [ "${RIFT_UPDATE:-0}" != 1 ] || return 0
    if [ "${RIFT_NONINTERACTIVE:-0}" != 1 ] && [ -t 1 ]; then
        configure_terminal 3<&1
    elif [ "${RIFT_NONINTERACTIVE:-0}" != 1 ] && [ -t 2 ]; then
        configure_terminal 3<&2
    else
        printf 'Run rift install in a terminal to choose default harnesses.\n'
    fi
}

# A directory outside PATH is registered in the startup file the user's shell reads.
# The line guards itself, so reinstalling or nesting shells never duplicates the
# entry, and it ends in a marker that makes it easy to find and remove by hand.
on_path() { case ":${PATH:-}:" in *":$bin_dir:"*) return 0 ;; esac; return 1; }
append_line() {
    [ ! -d "$1" ] || return 1
    if [ -f "$1" ]; then
        ! grep -Fqx -- "$2" "$1" || return 0
        [ -z "$(tail -c 1 "$1" 2>/dev/null)" ] || { printf '\n' >>"$1"; } 2>/dev/null || return 1
    else
        mkdir -p "$(dirname "$1")" 2>/dev/null || return 1
    fi
    { printf '%s\n' "$2" >>"$1"; } 2>/dev/null
}
register_path() {
    [ "${RIFT_NO_MODIFY_PATH:-0}" != 1 ] && [ -n "${HOME:-}" ] || return 1
    case "$bin_dir" in
        "$HOME"/*) checked=${bin_dir#"$HOME"/}; shown="\$HOME/$checked" ;;
        *) checked=$bin_dir; shown=$bin_dir ;;
    esac
    # Another shell will evaluate this text: write only names that need no quoting.
    case "$checked" in *[!A-Za-z0-9_./@+-]*) return 1 ;; esac
    line="case \":\$PATH:\" in *\":$shown:\"*) ;; *) export PATH=\"$shown:\$PATH\" ;; esac # rift installer"
    shell_name=${SHELL:-}
    case "${shell_name##*/}" in
        zsh) profile="${ZDOTDIR:-$HOME}/.zshrc"; append_line "$profile" "$line" ;;
        bash)
            profile="$HOME/.bashrc"
            append_line "$profile" "$line" || return 1
            # A login bash (every macOS terminal) reads the first of these, not .bashrc.
            for login in .bash_profile .bash_login .profile; do
                [ -e "$HOME/$login" ] || continue
                append_line "$HOME/$login" "$line"
                return
            done
            append_line "$HOME/.profile" "$line" ;;
        fish)
            profile="${XDG_CONFIG_HOME:-$HOME/.config}/fish/conf.d/rift.fish"
            append_line "$profile" "contains -- \"$shown\" \$PATH; or set -gx PATH \"$shown\" \$PATH # rift installer" ;;
        *) profile="$HOME/.profile"; append_line "$profile" "$line" ;;
    esac
}
# A child process cannot change its parent's PATH. On a terminal the installer
# therefore ends by becoming the user's own shell with the directory exported,
# so rift resolves at once; the startup file covers every later terminal.
restart_shell() {
    [ "${RIFT_NONINTERACTIVE:-0}" != 1 ] && [ "${RIFT_NO_SHELL_RESTART:-0}" != 1 ] && [ -t 1 ] || return 0
    # Never for automation or root: a prompt would stall a pipeline, and a
    # surprise root shell is worse than one extra command.
    [ -z "${CI:-}" ] && [ "$(id -u 2>/dev/null || echo 0)" != 0 ] || return 0
    # Only a shell whose startup file was just registered: this also keeps out
    # nologin, false and anything else that is executable but not a shell.
    case "${SHELL:-}" in
        /*/zsh|/*/bash|/*/fish|/*/sh|/*/dash|/*/ksh) [ -x "$SHELL" ] || return 0 ;;
        *) return 0 ;;
    esac
    # Inside the substitution fd 1 is a pipe; fd 3 still names the terminal.
    terminal=$(tty <&3) || return 0
    printf 'Starting a fresh %s so rift works in this terminal now; exit returns to the previous shell.\n' "${SHELL##*/}"
    cleanup
    trap - EXIT
    umask "$user_umask"
    unset RIFT_DOWNLOAD_MANIFEST RIFT_DOWNLOAD_BUCKET
    PATH="$bin_dir:${PATH:-}"
    export PATH
    exec "$SHELL" <"$terminal" 3<&-
}
finish() {
    if on_path; then
        printf 'Run rift inside your project to choose harnesses and permissions.\n'
    elif [ "${RIFT_UPDATE:-0}" = 1 ]; then
        :
    elif register_path; then
        printf '%s is registered on PATH in %s.\n' "$bin_dir" "$profile"
        printf 'Run rift inside your project to choose harnesses and permissions.\n'
        restart_shell 3<&1
        printf 'Open a new terminal first: this one started before PATH changed.\n'
    else
        printf 'Add %s to PATH, then run rift inside your project.\n' "$bin_dir"
    fi
}

# Release tags this installer understands: vMAJOR.MINOR.PATCH-alpha[.N],
# each number one to six digits.
alpha_tag='^v[0-9]{1,6}\.[0-9]{1,6}\.[0-9]{1,6}-alpha(\.[0-9]{1,6})?$'
# Whether alpha tag $1 is newer than alpha tag $2: major, minor, patch, then N
# (a plain -alpha is N = 0), compared as fixed-width numbers.
alpha_newer() {
    awk -v a="$1" -v b="$2" 'function key(v, p, n) {
        n = split(substr(v, 2), p, /[.-]/)
        return sprintf("%06d%06d%06d%06d", p[1], p[2], p[3], n >= 5 ? p[5] : 0)
    } BEGIN { exit !(key(a) > key(b)) }'
}

command -v curl >/dev/null 2>&1 || die 'curl is required.'
if [ -n "${RIFT_DOWNLOAD_BUCKET:-}" ]; then
    printf '%s' "$RIFT_DOWNLOAD_BUCKET" | LC_ALL=C grep -Eq '^[a-z0-9][a-z0-9-]{1,61}[a-z0-9]$' || die 'Invalid release bucket name.'
fi
command -v tar >/dev/null 2>&1 || die 'tar is required.'
case "$(uname -s)" in
    Darwin) platform=apple-darwin ;;
    Linux)
        platform=unknown-linux-gnu
        # The Linux builds link glibc. On a musl system the loader is absent and the
        # failure would otherwise surface later as a wrong-version complaint.
        if (ldd --version 2>&1 || :) | grep -qi musl; then
            die 'This alpha needs a glibc-based Linux; musl systems such as Alpine are not supported yet.'
        fi ;;
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
# The lock records "PID EPOCH" of its holder, as `rift uninstall` and install.ps1
# do. One whose holder is gone, or older than fifteen minutes whoever holds it (a
# recycled PID must not pin it), was left by a killed run and is reclaimed.
lock_stale_seconds=900
lock_busy() { die "Another Rift install, update or uninstall is running. Wait for it to finish; if none is, remove $lock and try again (a lock left by a closed terminal is reclaimed after 15 minutes)."; }
lock_owner() { sed -n '1p' "$1/owner" 2>/dev/null || :; }
take_lock() {
    mkdir "$lock" 2>/dev/null || return 1
    printf '%s %s\n' "$$" "$(date -u +%s 2>/dev/null || :)" >"$lock/owner"
}
lock_stale() {
    [ -d "$lock" ] && [ ! -L "$lock" ] || return 1
    owner=$(lock_owner "$lock")
    pid=${owner%% *}
    started=${owner#* }
    case "$pid" in '' | *[!0-9]*)
        # No owner line (an older installer's lock): only its age speaks for it.
        [ -n "$(find "$lock" -prune -mmin +15 2>/dev/null)" ]
        return ;;
    esac
    kill -0 "$pid" 2>/dev/null || return 0
    now=$(date -u +%s 2>/dev/null) || return 1
    case "$now" in '' | *[!0-9]*) return 1 ;; esac
    case "$started" in '' | *[!0-9]*) return 1 ;; esac
    [ $((now - started)) -gt "$lock_stale_seconds" ]
}
reclaim_lock() {
    lock_stale || lock_busy
    seen=$(lock_owner "$lock")
    # Move it aside before deleting, so two runs reclaiming at once cannot both win.
    aside="$lock.stale-$$"
    mv "$lock" "$aside" 2>/dev/null || lock_busy
    if [ "$(lock_owner "$aside")" != "$seen" ]; then
        [ -e "$lock" ] || mv "$aside" "$lock" 2>/dev/null || :
        lock_busy
    fi
    rm -rf "$aside"
    printf 'rift: reclaimed a stale installation lock left by an interrupted run.\n' >&2
    take_lock || lock_busy
}
take_lock || reclaim_lock
stage=
cleanup() { [ -z "$stage" ] || rm -rf "$stage"; { rm -f "$lock/owner"; rmdir "$lock"; } 2>/dev/null || :; }
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
progress "@--o--o  Step 1/3: Download invitation and binary"
# `rift update` fetches the manifest itself, checks its signature against the
# key compiled into the binary, and hands over that exact copy, so nothing
# unverified is fetched between the check and the install.
if [ "${RIFT_UPDATE:-0}" = 1 ] && [ -n "${RIFT_VERIFIED_MANIFEST:-}" ]; then
    cp "$RIFT_VERIFIED_MANIFEST" "$stage/manifest" || die 'The verified release manifest could not be read.'
else
    fetch "$RIFT_DOWNLOAD_MANIFEST" "$stage/manifest" 1048576
fi
version=$(awk -F '\t' 'NR == 1 && NF == 2 && $1 == "rift-download-v1" {print $2}' "$stage/manifest")
printf '%s\n' "$version" | LC_ALL=C grep -Eq "$alpha_tag" || die 'Invalid or incompatible alpha invitation.'
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
if [ -f "$binary" ] && [ "${RIFT_ALLOW_DOWNGRADE:-0}" != 1 ]; then
    installed=$(sed -n '3p' "$receipt")
    # Compared only when the receipt names an alpha tag too, so both parse.
    if printf '%s\n' "$installed" | LC_ALL=C grep -Eq "$alpha_tag" &&
        alpha_newer "$installed" "$version"; then
        die "Rift $installed is newer than $version; nothing was changed. Set RIFT_ALLOW_DOWNGRADE=1 to install the older build."
    fi
fi
if [ -f "$binary" ] && [ "$(sed -n '3p' "$receipt")" = "$version" ]; then
    printf 'Rift %s is already installed.\n' "$version"
    preferences
    finish
    exit 0
fi

fetch "$archive_url" "$stage/archive.tar.gz"
progress "*--@--o  Step 2/3: Verify archive and executable"
[ "$(digest "$stage/archive.tar.gz")" = "$expected" ] || die 'Archive checksum mismatch; existing installation was preserved.'
[ "$(tar -tzf "$stage/archive.tar.gz")" = rift ] || die 'Unexpected archive contents.'
# Stream the named member into our own file: archive links never control a path.
tar -xOzf "$stage/archive.tar.gz" rift > "$stage/rift"
[ -s "$stage/rift" ] || die 'Empty executable in archive.'
chmod 755 "$stage/rift"
# A binary that cannot start (an older glibc than it was built against, a noexec
# mount) is a different fault from one that starts and names another version.
reported=$("$stage/rift" --version 2>/dev/null) || die 'The downloaded executable cannot run on this system (on Linux it needs a recent glibc, and the installation directory must allow execution). Nothing was changed.'
[ "$reported" = "rift ${version#v}" ] || die 'Downloaded executable reports a different version.'
checksum=$(digest "$stage/rift")
printf 'rift-install-v1\n%s\n%s\n' "$checksum" "$version" > "$stage/receipt"

progress "*--*--@  Step 3/3: Install verified binary"
recheck_installation
# Keep a rollback copy until both replacements have landed.
if [ -f "$binary" ]; then cp -p "$binary" "$stage/previous"; fi
mv -f "$stage/rift" "$binary"
if ! mv -f "$stage/receipt" "$receipt"; then
    if [ -f "$stage/previous" ]; then mv -f "$stage/previous" "$binary"; else rm -f "$binary"; fi
    die 'Could not commit installation receipt; executable replacement was rolled back.'
fi
progress "*--*--*  Binary installation complete"
printf 'Installed Rift %s at %s\n' "$version" "$binary"
preferences
finish
