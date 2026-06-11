#!/usr/bin/env bash

set -euo pipefail

APP_NAME="termphonic"
BIN_DIR="$HOME/.local/bin"
DATA_DIR="$HOME/.local/share/$APP_NAME"
LIB_DIR="$HOME/.local/lib/$APP_NAME"
LIBEXEC_DIR="$LIB_DIR/libexec"

remove_if_exists() {
    local path="$1"
    if [ -e "$path" ]; then
        rm -rf "$path"
    fi
}

echo
echo "Termphonic uninstaller"
echo

pkill -x termphonic 2>/dev/null || true
pkill -x bmusic 2>/dev/null || true

remove_if_exists "$BIN_DIR/$APP_NAME"
remove_if_exists "$DATA_DIR"
remove_if_exists "$LIBEXEC_DIR"

if [ -d "$LIB_DIR" ] && [ -z "$(find "$LIB_DIR" -mindepth 1 -maxdepth 1 2>/dev/null)" ]; then
    rmdir "$LIB_DIR" 2>/dev/null || true
fi

echo "Removed Termphonic from:"
echo "  $BIN_DIR/$APP_NAME"
echo "  $DATA_DIR"
echo "  $LIBEXEC_DIR"
echo
echo "Done."
