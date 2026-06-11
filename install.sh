#!/usr/bin/env bash

set -euo pipefail

APP_NAME="termphonic"
BIN_DIR="$HOME/.local/bin"
DATA_DIR="$HOME/.local/share/$APP_NAME"
VENV_DIR="$DATA_DIR/venv"

run_step() {
    local label="$1"
    shift

    local log_file
    log_file="$(mktemp)"
    printf "%-42s" "$label"

    if "$@" >"$log_file" 2>&1; then
        echo "done"
        rm -f "$log_file"
    else
        echo "failed"
        cat "$log_file" >&2
        rm -f "$log_file"
        exit 1
    fi
}

echo
echo "Termphonic installer"
echo

printf "%-42s" "[1/4] Checking dependencies"
for dependency in cargo ffmpeg python3; do
    if ! command -v "$dependency" > /dev/null 2>&1; then
        echo "failed"
        echo "Missing required dependency: $dependency" >&2
        exit 1
    fi
done
echo "done"

run_step "[2/4] Building release binary" cargo build --release --quiet

mkdir -p "$BIN_DIR" "$DATA_DIR"
pkill -x termphonic 2>/dev/null || true
pkill -x bmusic 2>/dev/null || true
run_step "[3/4] Installing executable" \
    install -m 755 "target/release/$APP_NAME" "$BIN_DIR/$APP_NAME"

if [ ! -x "$VENV_DIR/bin/pip" ]; then
    run_step "[4/4] Creating Python environment" python3 -m venv "$VENV_DIR"
    run_step "      Installing yt-dlp" \
        "$VENV_DIR/bin/pip" install --quiet --disable-pip-version-check "yt-dlp[default]"
else
    run_step "[4/4] Updating yt-dlp" \
        "$VENV_DIR/bin/pip" install --quiet --disable-pip-version-check --upgrade "yt-dlp[default]"
fi

echo
echo "Installed successfully."
echo "Run: termphonic"

case ":$PATH:" in
    *":$BIN_DIR:"*) ;;
    *) echo "Note: add $BIN_DIR to your PATH." ;;
esac
