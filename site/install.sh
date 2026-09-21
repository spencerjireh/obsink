#!/bin/sh
# Installs the obsink command line tool on macOS from the latest GitHub release:
#   curl -fsSL https://obsink.spencerjireh.com/install.sh | sh
# Downloads the universal tarball and its checksum, verifies, and installs
# `obsink` into /usr/local/bin (sudo when needed) or ~/.local/bin.
set -eu

REPO="spencerjireh/obsink"
ASSET_SUFFIX="universal-apple-darwin.tar.gz"
SERVER_URL="https://obsink-api.spencerjireh.com"

if [ "$(uname -s)" != "Darwin" ]; then
    echo "obsink ships for macOS only; see https://github.com/$REPO" >&2
    exit 1
fi
for tool in curl shasum tar; do
    command -v "$tool" >/dev/null || { echo "$tool is required" >&2; exit 1; }
done

echo "==> Looking up the latest release"
release=$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest")
tag=$(printf '%s' "$release" | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -1)
url=$(printf '%s' "$release" | sed -n "s/.*\"browser_download_url\": *\"\([^\"]*$ASSET_SUFFIX\)\".*/\1/p" | head -1)
if [ -z "$url" ]; then
    echo "no $ASSET_SUFFIX asset on the latest release; see https://github.com/$REPO/releases" >&2
    exit 1
fi

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
echo "==> Downloading obsink $tag"
curl -fsSL -o "$work/obsink.tar.gz" "$url"
curl -fsSL -o "$work/obsink.tar.gz.sha256" "$url.sha256"
(cd "$work" && sed 's# .*# obsink.tar.gz#' obsink.tar.gz.sha256 | shasum -a 256 -c - >/dev/null) \
    || { echo "checksum mismatch; not installing" >&2; exit 1; }
tar -C "$work" -xzf "$work/obsink.tar.gz" obsink

dest=/usr/local/bin
if [ -w "$dest" ]; then
    install -m 755 "$work/obsink" "$dest/obsink"
elif command -v sudo >/dev/null && [ -t 0 ]; then
    echo "==> Installing to $dest (sudo)"
    sudo install -m 755 "$work/obsink" "$dest/obsink"
else
    dest="$HOME/.local/bin"
    mkdir -p "$dest"
    install -m 755 "$work/obsink" "$dest/obsink"
    case ":$PATH:" in
        *":$dest:"*) ;;
        *) echo "add $dest to your PATH" ;;
    esac
fi

echo "==> Installed $("$dest/obsink" --version) at $dest/obsink"
echo "Next: obsink login --server-url $SERVER_URL --email you@example.com"
echo "      (a new account needs an invite code from an existing user: --invite-code)"
