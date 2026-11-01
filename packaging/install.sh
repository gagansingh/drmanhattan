#!/bin/sh
# Installs drmanhattan from the latest GitHub Release, with no Xcode or Rust toolchain needed:
#
#   curl -fsSL https://github.com/gagansingh/drmanhattan/releases/latest/download/install.sh | sh
#
# Downloads the archive for this machine (macOS universal, or Linux x86_64), checks its
# SHA-256 against the published checksum, and puts `drmanhattan` and `manhattan` into
# ~/.local/bin, or the directory given as the first argument or in INSTALL_DIR:
#
#   curl -fsSL .../install.sh | sh -s -- /usr/local/bin
#   INSTALL_DIR=~/bin sh install.sh
#
# A specific version: VERSION=v0.1.0 sh install.sh
set -eu

repo="gagansingh/drmanhattan"
dir="${1:-${INSTALL_DIR:-$HOME/.local/bin}}"
version="${VERSION:-}"

case "$(uname -s)" in
  Darwin) asset="macos-universal" ;;
  Linux)
    case "$(uname -m)" in
      x86_64|amd64) asset="linux-x86_64" ;;
      *) echo "install.sh: no prebuilt binary for Linux $(uname -m); use: cargo install --git https://github.com/$repo" >&2; exit 1 ;;
    esac ;;
  *) echo "install.sh: unsupported OS $(uname -s); use: cargo install --git https://github.com/$repo" >&2; exit 1 ;;
esac

if [ -z "$version" ]; then
  # The "latest" redirect carries the tag in its Location header; no API token needed.
  version="$(curl -fsSI "https://github.com/$repo/releases/latest" | tr -d '\r' | awk 'tolower($1) == "location:" { sub(/.*\/tag\//, "", $2); print $2 }' | tail -n 1)"
  [ -n "$version" ] || { echo "install.sh: could not determine the latest release" >&2; exit 1; }
fi

archive="drmanhattan-$version-$asset.tar.gz"
base="https://github.com/$repo/releases/download/$version"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

echo "downloading $archive"
curl -fsSL -o "$tmp/$archive" "$base/$archive"
curl -fsSL -o "$tmp/$archive.sha256" "$base/$archive.sha256"
expected="$(awk '{print $1}' "$tmp/$archive.sha256")"
if command -v shasum >/dev/null 2>&1; then
  actual="$(shasum -a 256 "$tmp/$archive" | awk '{print $1}')"
else
  actual="$(sha256sum "$tmp/$archive" | awk '{print $1}')"
fi
[ "$expected" = "$actual" ] || { echo "install.sh: checksum mismatch for $archive" >&2; exit 1; }

tar -xzf "$tmp/$archive" -C "$tmp"
mkdir -p "$dir"
install -m 755 "$tmp/drmanhattan/drmanhattan" "$tmp/drmanhattan/manhattan" "$dir/"
echo "installed drmanhattan $version to $dir (commands: drmanhattan, manhattan)"
case ":$PATH:" in
  *":$dir:"*) ;;
  *) echo "note: $dir is not on your PATH; add it, e.g.  export PATH=\"$dir:\$PATH\"" ;;
esac
echo "start the queue with:  drmanhattan --open"
