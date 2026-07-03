#!/usr/bin/env bash
#
# install.sh -- download the right claude-go binary for this platform
# from the latest GitHub release and install it to ~/.local/bin.
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/Abdk4Moura/claude-go/main/install.sh | bash
#   curl -fsSL https://raw.githubusercontent.com/Abdk4Moura/claude-go/main/install.sh | bash -s -- v0.1.0
#   curl -fsSL ... | bash -s -- --system  # install to /usr/local/bin (requires sudo)
#
# Environment:
#   CLAUDE_GO_REPO  Override the GitHub repo (default Abdk4Moura/claude-go).
#   CLAUDE_GO_BIN   Override the install path (default ~/.local/bin/claude-go).

set -euo pipefail

REPO="${CLAUDE_GO_REPO:-Abdk4Moura/claude-go}"
VERSION="${1:-}"

case "${VERSION:-}" in
  "")
    VERSION="latest"
    ;;
  --system)
    VERSION="latest"
    PREFIX="/usr/local/bin"
    ;;
  -h|--help)
    cat <<EOF
claude-go installer

Usage:
  curl -fsSL https://raw.githubusercontent.com/${REPO}/main/install.sh | bash
  curl -fsSL https://raw.githubusercontent.com/${REPO}/main/install.sh | bash -s -- v0.1.0

Environment:
  CLAUDE_GO_REPO   GitHub repo (default: ${REPO})
  CLAUDE_GO_BIN    Install path (default: ~/.local/bin/claude-go)
EOF
    exit 0
    ;;
  v*)
    : # ok, use as-is
    ;;
  *)
    echo "error: unknown argument '$VERSION' (expected a version like v0.1.0)" >&2
    exit 2
    ;;
esac

# Pick the right binary for this OS/arch.
OS="$(uname -s)"
ARCH="$(uname -m)"

case "$OS" in
  Linux)
    case "$ARCH" in
      x86_64|amd64)  ARTIFACT="claude-go-linux-x86_64" ;;
      aarch64|arm64) ARTIFACT="claude-go-linux-aarch64" ;;
      *) echo "error: unsupported Linux arch: $ARCH" >&2; exit 1 ;;
    esac
    ;;
  Darwin)
    case "$ARCH" in
      x86_64)        ARTIFACT="claude-go-macos-x86_64" ;;
      arm64|aarch64) ARTIFACT="claude-go-macos-aarch64" ;;
      *) echo "error: unsupported macOS arch: $ARCH" >&2; exit 1 ;;
    esac
    ;;
  MINGW*|MSYS*|CYGWIN*)
    case "$ARCH" in
      x86_64|amd64) ARTIFACT="claude-go-windows-x86_64.exe" ;;
      *) echo "error: unsupported Windows arch: $ARCH" >&2; exit 1 ;;
    esac
    ;;
  *)
    echo "error: unsupported OS: $OS" >&2
    exit 1
    ;;
esac

# Resolve install path.
PREFIX="${PREFIX:-$HOME/.local/bin}"
DEST="${CLAUDE_GO_BIN:-$PREFIX/claude-go}"

# Build the download URL.
if [[ "$VERSION" == "latest" ]]; then
  URL="https://github.com/${REPO}/releases/latest/download/${ARTIFACT}"
else
  URL="https://github.com/${REPO}/releases/download/${VERSION}/${ARTIFACT}"
fi

echo "==> installing claude-go ($VERSION) to $DEST"
echo "    from: $URL"

# Make sure the install dir exists.
mkdir -p "$(dirname "$DEST")"

# Download to a temp file in the same dir, then move into place
# (atomic on the same filesystem).
TMP="$(mktemp "$(dirname "$DEST")/.claude-go.XXXXXX")"
trap 'rm -f "$TMP"' EXIT

if command -v curl >/dev/null 2>&1; then
  curl -fsSL --retry 3 -o "$TMP" "$URL"
elif command -v wget >/dev/null 2>&1; then
  wget -q -O "$TMP" "$URL"
else
  echo "error: need either curl or wget" >&2
  exit 1
fi

chmod +x "$TMP"
mv "$TMP" "$DEST"

echo "==> installed: $DEST"
echo
echo "Verify:"
echo "  $DEST --version"
echo
if [[ ":$PATH:" != *":$(dirname "$DEST"):"* ]]; then
  echo "Note: $(dirname "$DEST") is not on your PATH. Add it with:"
  echo "  echo 'export PATH=\"$(dirname "$DEST"):\$PATH\"' >> ~/.bashrc"
  echo
fi
echo "Then launch the TUI:"
echo "  claude-go"
