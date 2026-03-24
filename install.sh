#!/bin/bash
set -euo pipefail

REPO="Rag0n/agentbox"
INSTALL_DIR="${INSTALL_DIR:-$HOME/.local/bin}"

# Check platform
ARCH=$(uname -m)
OS=$(uname -s)

if [ "$OS" != "Darwin" ]; then
    echo "Error: agentbox requires macOS. Detected: $OS" >&2
    exit 1
fi

if [ "$ARCH" != "arm64" ]; then
    echo "Error: agentbox requires Apple Silicon (arm64). Detected: $ARCH" >&2
    exit 1
fi

# Create install directory
mkdir -p "$INSTALL_DIR"

# Download and extract
ASSET="agentbox-darwin-arm64.tar.gz"
URL="https://github.com/$REPO/releases/latest/download/$ASSET"

echo "Downloading agentbox..."
TMPDIR=$(mktemp -d)
trap 'rm -rf "$TMPDIR"' EXIT

curl -fsSL "$URL" -o "$TMPDIR/$ASSET"
tar xzf "$TMPDIR/$ASSET" -C "$TMPDIR"
mv "$TMPDIR/agentbox" "$INSTALL_DIR/agentbox"
chmod +x "$INSTALL_DIR/agentbox"

echo "Installed agentbox to $INSTALL_DIR/agentbox"
"$INSTALL_DIR/agentbox" --version

# Check PATH
if ! echo "$PATH" | tr ':' '\n' | grep -qx "$INSTALL_DIR"; then
    echo ""
    echo "Note: $INSTALL_DIR is not in your PATH."
    echo "Add it with:"
    echo "  echo 'export PATH=\"$INSTALL_DIR:\$PATH\"' >> ~/.zshrc"
fi
