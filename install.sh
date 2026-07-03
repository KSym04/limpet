#!/usr/bin/env bash
# limpet installer: download the latest release binary, verify its sha256,
# install it, and register it with Claude Code.
#
#   curl -fsSL https://raw.githubusercontent.com/KSym04/limpet/main/install.sh | bash
#
# Environment:
#   LIMPET_INSTALL_DIR  target directory (default: ~/.local/bin)
#   LIMPET_VERSION      release tag to install (default: latest)
set -euo pipefail

REPO="KSym04/limpet"
INSTALL_DIR="${LIMPET_INSTALL_DIR:-$HOME/.local/bin}"
VERSION="${LIMPET_VERSION:-latest}"

say()  { printf '\033[1;36mlimpet\033[0m %s\n' "$*"; }
fail() { printf '\033[1;31mlimpet\033[0m %s\n' "$*" >&2; exit 1; }

# --- platform -> release target ---------------------------------------------
os=$(uname -s)
arch=$(uname -m)
case "$os/$arch" in
  Darwin/arm64)          target="aarch64-apple-darwin" ;;
  Linux/x86_64|Linux/amd64) target="x86_64-unknown-linux-gnu" ;;
  *) fail "no prebuilt binary for $os/$arch — install Rust (https://rustup.rs) then run: cargo install limpet && limpet install" ;;
esac
asset="limpet-$target"

# --- download ----------------------------------------------------------------
if [ "$VERSION" = "latest" ]; then
  base="https://github.com/$REPO/releases/latest/download"
else
  base="https://github.com/$REPO/releases/download/$VERSION"
fi

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

fetch() {
  if command -v curl >/dev/null 2>&1; then
    curl -fsSL "$1" -o "$2"
  elif command -v wget >/dev/null 2>&1; then
    wget -q "$1" -O "$2"
  else
    fail "need curl or wget"
  fi
}

say "downloading $asset ($VERSION)"
fetch "$base/$asset" "$tmp/$asset"
fetch "$base/$asset.sha256" "$tmp/$asset.sha256"

# --- verify ------------------------------------------------------------------
say "verifying sha256"
(
  cd "$tmp"
  if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 -c "$asset.sha256" >/dev/null
  elif command -v sha256sum >/dev/null 2>&1; then
    sha256sum -c "$asset.sha256" >/dev/null
  else
    fail "need shasum or sha256sum to verify the download"
  fi
) || fail "sha256 mismatch — download corrupted or tampered, aborting"

# --- install -----------------------------------------------------------------
mkdir -p "$INSTALL_DIR"
install -m 755 "$tmp/$asset" "$INSTALL_DIR/limpet"
# macOS quarantines piped downloads in some setups; clearing is safe post-verify.
if [ "$os" = "Darwin" ] && command -v xattr >/dev/null 2>&1; then
  xattr -d com.apple.quarantine "$INSTALL_DIR/limpet" 2>/dev/null || true
fi
say "installed $INSTALL_DIR/limpet"

case ":$PATH:" in
  *":$INSTALL_DIR:"*) ;;
  *) say "note: $INSTALL_DIR is not on your PATH — add this to your shell profile:"
     say "  export PATH=\"$INSTALL_DIR:\$PATH\"" ;;
esac

# --- register with Claude Code -------------------------------------------------
say "registering with Claude Code"
"$INSTALL_DIR/limpet" install
say "done — restart Claude Code, then type /limpet in any project"
