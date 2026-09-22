#!/bin/sh
# Installs the obsink command line tool on macOS from the latest GitHub release:
#   curl -fsSL https://obsink.spencerjireh.com/install.sh | sh
# Downloads the universal tarball and its checksum, verifies, and installs
# `obsink` into /usr/local/bin (asking for your password on the terminal when
# needed) or, when it cannot ask, into ~/.local/bin.
#
# Overrides, used by scripts/test-install-sh.sh:
#   OBSINK_GITHUB_API   base URL of the GitHub API (default https://api.github.com)
#   OBSINK_INSTALL_DIR  install into this directory instead of the logic above
set -eu

REPO="spencerjireh/obsink"
ASSET_SUFFIX="universal-apple-darwin.tar.gz"
SERVER_URL="https://obsink-api.spencerjireh.com"
GITHUB_API="${OBSINK_GITHUB_API:-https://api.github.com}"
RELEASES="https://github.com/$REPO/releases"

die() {
    echo "$*" >&2
    exit 1
}

main() {
    [ "$(uname -s)" = "Darwin" ] || die "obsink ships for macOS only; see https://github.com/$REPO"
    for tool in curl shasum tar; do
        command -v "$tool" >/dev/null || die "$tool is required"
    done

    echo "==> Looking up the latest release"
    release=$(curl -fsSL "$GITHUB_API/repos/$REPO/releases/latest") \
        || die "Could not reach the GitHub API (offline, or its rate limit). Download from $RELEASES instead."
    tag=$(printf '%s' "$release" | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -1)
    url=$(printf '%s' "$release" | sed -n "s/.*\"browser_download_url\": *\"\([^\"]*$ASSET_SUFFIX\)\".*/\1/p" | head -1)
    [ -n "$url" ] || die "The latest release has no $ASSET_SUFFIX asset; see $RELEASES"

    work=$(mktemp -d)
    trap 'rm -rf "$work"' EXIT
    trap 'exit 130' INT TERM HUP
    echo "==> Downloading obsink ${tag:-(latest)}"
    curl -fsSL -o "$work/obsink.tar.gz" "$url" || die "Download failed: $url"
    curl -fsSL -o "$work/obsink.tar.gz.sha256" "$url.sha256" \
        || die "The release has no checksum next to the tarball ($url.sha256); not installing."
    # The checksum file is `shasum -a 256 <asset>` output: rebuild its one line
    # for the local file name (two spaces, as shasum -c requires).
    (cd "$work" && awk '{ print $1 "  obsink.tar.gz" }' obsink.tar.gz.sha256 | shasum -a 256 -c - >/dev/null) \
        || die "Checksum mismatch; not installing"
    tar -C "$work" -xzf "$work/obsink.tar.gz" obsink

    path_hint=""
    if [ -n "${OBSINK_INSTALL_DIR:-}" ]; then
        dest="$OBSINK_INSTALL_DIR"
        mkdir -p "$dest"
        install -m 755 "$work/obsink" "$dest/obsink"
    elif [ -d /usr/local/bin ] && [ -w /usr/local/bin ]; then
        dest=/usr/local/bin
        install -m 755 "$work/obsink" "$dest/obsink"
    elif [ -t 1 ] && [ -r /dev/tty ] && command -v sudo >/dev/null; then
        # Under `curl | sh` stdin is the script itself, so the password prompt
        # reads from the terminal directly.
        dest=/usr/local/bin
        echo "==> Installing to $dest (sudo)"
        # shellcheck disable=SC2024  # the redirect is for sudo's own prompt
        sudo -p "Password (sudo, to install into $dest): " -v </dev/tty \
            || die "sudo failed. Rerun with OBSINK_INSTALL_DIR=\$HOME/.local/bin to install without it."
        sudo mkdir -p "$dest"
        sudo install -m 755 "$work/obsink" "$dest/obsink"
    else
        dest="$HOME/.local/bin"
        mkdir -p "$dest"
        install -m 755 "$work/obsink" "$dest/obsink"
        case ":$PATH:" in
            *":$dest:"*) ;;
            *) path_hint="Add $dest to your PATH: export PATH=\"\$HOME/.local/bin:\$PATH\"" ;;
        esac
    fi

    version=$("$dest/obsink" --version 2>/dev/null || echo obsink)
    echo "==> Installed $version at $dest/obsink"
    [ -z "$path_hint" ] || echo "$path_hint"
    echo "Next: obsink login --email you@example.com   (talks to $SERVER_URL; --server-url for your own)"
    echo "      (a new account needs an invite code from an existing user: --invite-code)"
}

main "$@"
