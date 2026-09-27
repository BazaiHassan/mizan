#!/bin/sh
# Install the latest mzn release binary.
#
#   curl -fsSL https://raw.githubusercontent.com/BazaiHassan/mizan/main/install.sh | sh
#
# Environment:
#   MZN_VERSION      tag to install (default: latest release), e.g. v0.1.0
#   MZN_INSTALL_DIR  destination directory (default: ~/.local/bin)
#
# This script is the only part of mzn that uses the network, and only when
# you run it. mzn itself never makes network requests.
set -eu

REPO="BazaiHassan/mizan"
INSTALL_DIR="${MZN_INSTALL_DIR:-$HOME/.local/bin}"

say() { printf 'mzn-install: %s\n' "$*" >&2; }
die() { say "error: $*"; exit 1; }

need() { command -v "$1" >/dev/null 2>&1 || die "'$1' is required"; }
need curl
need tar
need uname

os=$(uname -s)
arch=$(uname -m)
case "$os" in
  Linux) os_part="unknown-linux-musl" ;;
  Darwin) os_part="apple-darwin" ;;
  *) die "unsupported OS '$os'. On Windows, download the .zip from https://github.com/$REPO/releases" ;;
esac
case "$arch" in
  x86_64 | amd64) arch_part="x86_64" ;;
  aarch64 | arm64) arch_part="aarch64" ;;
  *) die "unsupported architecture '$arch'" ;;
esac
target="$arch_part-$os_part"

tag="${MZN_VERSION:-}"
if [ -z "$tag" ]; then
  tag=$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" |
    sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -n 1)
  [ -n "$tag" ] || die "could not determine the latest release"
fi

name="mzn-$tag-$target"
base="https://github.com/$REPO/releases/download/$tag"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT INT TERM

say "downloading $name"
curl -fsSL -o "$tmp/$name.tar.gz" "$base/$name.tar.gz" || die "download failed: $base/$name.tar.gz"
curl -fsSL -o "$tmp/$name.tar.gz.sha256" "$base/$name.tar.gz.sha256" || die "checksum download failed"

expected=$(cut -d ' ' -f 1 "$tmp/$name.tar.gz.sha256")
if command -v sha256sum >/dev/null 2>&1; then
  actual=$(sha256sum "$tmp/$name.tar.gz" | cut -d ' ' -f 1)
elif command -v shasum >/dev/null 2>&1; then
  actual=$(shasum -a 256 "$tmp/$name.tar.gz" | cut -d ' ' -f 1)
else
  die "need sha256sum or shasum to verify the download"
fi
[ "$expected" = "$actual" ] || die "checksum mismatch (expected $expected, got $actual)"

tar -xzf "$tmp/$name.tar.gz" -C "$tmp"
mkdir -p "$INSTALL_DIR"
install -m 755 "$tmp/$name/mzn" "$INSTALL_DIR/mzn" 2>/dev/null || {
  cp "$tmp/$name/mzn" "$INSTALL_DIR/mzn"
  chmod 755 "$INSTALL_DIR/mzn"
}
say "installed $INSTALL_DIR/mzn ($tag)"

case ":$PATH:" in
  *":$INSTALL_DIR:"*) ;;
  *) say "note: $INSTALL_DIR is not on your PATH; add it to your shell profile" ;;
esac

if ! command -v rtk >/dev/null 2>&1; then
  say "RTK is not installed. mzn works with RTK; see https://github.com/rtk-ai/rtk#installation"
fi
say "next: cd into a project and run 'mzn doctor' then 'mzn analyze'"
