#!/usr/bin/env bash

set -euo pipefail

APP_NAME="termphonic"
BIN_DIR="$HOME/.local/bin"
DATA_DIR="$HOME/.local/share/$APP_NAME"
LIBEXEC_DIR="$HOME/.local/lib/$APP_NAME/libexec"

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
for dependency in cargo curl ffmpeg; do
    if ! command -v "$dependency" > /dev/null 2>&1; then
        echo "failed"
        echo "Missing required dependency: $dependency" >&2
        exit 1
    fi
done
echo "done"

run_step "[2/4] Building release binary" cargo build --release --quiet

mkdir -p "$BIN_DIR" "$DATA_DIR" "$LIBEXEC_DIR"
pkill -x termphonic 2>/dev/null || true
pkill -x bmusic 2>/dev/null || true
run_step "[3/4] Installing executable" \
    install -m 755 "target/release/$APP_NAME" "$BIN_DIR/$APP_NAME"

case "$(uname -m)" in
    x86_64)
        YT_DLP_ASSET="yt-dlp_linux"
        ;;
    aarch64 | arm64)
        YT_DLP_ASSET="yt-dlp_linux_aarch64"
        ;;
    *)
        echo "Unsupported architecture for bundled yt-dlp: $(uname -m)" >&2
        exit 1
        ;;
esac

YT_DLP_URL="https://github.com/yt-dlp/yt-dlp/releases/latest/download/$YT_DLP_ASSET"
YT_DLP_TEMP="$(mktemp)"
trap 'rm -f "$YT_DLP_TEMP"' EXIT
run_step "[4/4] Installing standalone yt-dlp" \
    curl --fail --location --silent --show-error "$YT_DLP_URL" --output "$YT_DLP_TEMP"
install -m 755 "$YT_DLP_TEMP" "$LIBEXEC_DIR/yt-dlp"

echo
echo "Installed successfully."
echo "Run: termphonic"

case ":$PATH:" in
    *":$BIN_DIR:"*) ;;
    *) echo "Note: add $BIN_DIR to your PATH." ;;
esac
