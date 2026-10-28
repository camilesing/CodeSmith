#!/usr/bin/env bash
set -euo pipefail
# CodeSmith Unix installer
# Copies codesmith to ~/.local/bin (or $PREFIX/bin)

PREFIX="${PREFIX:-$HOME/.local}"
BIN_DIR="${PREFIX}/bin"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

mkdir -p "$BIN_DIR"

echo "Installing codesmith to $BIN_DIR ..."

for bin in codesmith; do
    src="$SCRIPT_DIR/$bin"
    dst="$BIN_DIR/$bin"
    if [[ ! -f "$src" ]]; then
        echo "ERROR: $src not found in archive"
        exit 1
    fi
    cp "$src" "$dst"
    chmod +x "$dst"
    echo "  $dst"
done

# `codes` shorthand: a relative symlink, so it keeps pointing at `codesmith`
# when `codesmith update` replaces the real file. (macOS reports the link path
# from current_exe, which the updater canonicalizes before writing.)
ln -sf codesmith "$BIN_DIR/codes"
echo "  $BIN_DIR/codes -> codesmith"

echo ""
echo "Done. codesmith (and its codes alias) installed to $BIN_DIR."

# Check if BIN_DIR is on PATH
if [[ ":$PATH:" != *":$BIN_DIR:"* ]]; then
    echo ""
    echo "Add $BIN_DIR to your PATH:"
    echo ""
    SHELL_NAME="$(basename "${SHELL:-/bin/sh}")"
    case "$SHELL_NAME" in
        zsh)  RC="$HOME/.zshrc" ;;
        bash) RC="$HOME/.bashrc" ;;
        fish) RC="$HOME/.config/fish/config.fish" ;;
        *)    RC="your shell profile" ;;
    esac
    echo "  echo 'export PATH=\"$BIN_DIR:\$PATH\"' >> $RC"
    echo "  source $RC"
fi

echo ""
echo "Then run: codesmith   (shorthand: codes)"
