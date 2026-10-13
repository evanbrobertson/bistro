#!/bin/sh
# Install Bistro from GitHub Releases.
#
#   BISTRO_CHANNEL      stable (default) or beta
#   BISTRO_INSTALL_DIR  where to put the binary (default: ~/.local/bin)
#   BISTRO_VERSION      a tag such as v0.2.0 (overrides the channel)
set -eu

REPO="evanbrobertson/bistro"
CHANNEL="${BISTRO_CHANNEL:-stable}"
DEST="${BISTRO_INSTALL_DIR:-$HOME/.local/bin}"

say() { printf '%s\n' "$*"; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }

command -v curl >/dev/null 2>&1 || die "curl is required"
command -v tar >/dev/null 2>&1 || die "tar is required"

case "$(uname -s)-$(uname -m)" in
  Linux-x86_64) target="x86_64-unknown-linux-musl" ;;
  Linux-aarch64 | Linux-arm64) target="aarch64-unknown-linux-musl" ;;
  Darwin-arm64) target="aarch64-apple-darwin" ;;
  Darwin-x86_64) target="x86_64-apple-darwin" ;;
  *) die "no build for $(uname -s) $(uname -m)" ;;
esac

api="https://api.github.com/repos/$REPO/releases"
if [ -n "${BISTRO_VERSION:-}" ]; then
  tag="$BISTRO_VERSION"
else
  case "$CHANNEL" in
    stable) url="$api/latest" ;;
    beta) url="$api?per_page=1" ;;
    *) die "BISTRO_CHANNEL must be stable or beta" ;;
  esac
  tag="$(curl -fsSL "$url" | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -n 1)"
  [ -n "$tag" ] || die "no $CHANNEL release found"
fi

asset="bistro-$tag-$target.tar.gz"
base="https://github.com/$REPO/releases/download/$tag"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

say "Installing Bistro $tag ($target)"
curl -fsSL "$base/$asset" -o "$tmp/$asset" || die "could not download $asset"
curl -fsSL "$base/$asset.sha256" -o "$tmp/$asset.sha256" || die "could not download the checksum"

expected="$(cut -d ' ' -f 1 "$tmp/$asset.sha256")"
if command -v sha256sum >/dev/null 2>&1; then
  actual="$(sha256sum "$tmp/$asset" | cut -d ' ' -f 1)"
else
  actual="$(shasum -a 256 "$tmp/$asset" | cut -d ' ' -f 1)"
fi
[ "$expected" = "$actual" ] || die "checksum does not match"

tar -xzf "$tmp/$asset" -C "$tmp" bistro
mkdir -p "$DEST"
install -m 755 "$tmp/bistro" "$DEST/bistro"

say "Installed to $DEST/bistro"
case ":$PATH:" in
  *":$DEST:"*) ;;
  *) say "Add $DEST to your PATH to run bistro." ;;
esac
