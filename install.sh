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

printf "%-42s" "[1/5] Checking dependencies"
for dependency in cargo curl ffmpeg unzip; do
    if ! command -v "$dependency" > /dev/null 2>&1; then
        echo "failed"
        echo "Missing required dependency: $dependency" >&2
        exit 1
    fi
done
echo "done"

run_step "[2/5] Building release binary" cargo build --release --quiet

mkdir -p "$BIN_DIR" "$DATA_DIR" "$LIBEXEC_DIR"
pkill -x termphonic 2>/dev/null || true
pkill -x bmusic 2>/dev/null || true
run_step "[3/5] Installing executable" \
    install -m 755 "target/release/$APP_NAME" "$BIN_DIR/$APP_NAME"

case "$(uname -m)" in
    x86_64)
        YT_DLP_ASSET="yt-dlp_linux"
        DENO_TARGET="x86_64-unknown-linux-gnu"
        ;;
    aarch64 | arm64)
        YT_DLP_ASSET="yt-dlp_linux_aarch64"
        DENO_TARGET="aarch64-unknown-linux-gnu"
        ;;
    *)
        echo "Unsupported architecture for bundled yt-dlp: $(uname -m)" >&2
        exit 1
        ;;
esac

TEMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TEMP_DIR"' EXIT

YT_DLP_TEMP="$TEMP_DIR/yt-dlp"
YT_DLP_URL="https://github.com/yt-dlp/yt-dlp/releases/latest/download/$YT_DLP_ASSET"
run_step "[4/5] Installing standalone yt-dlp" \
    curl --fail --location --silent --show-error "$YT_DLP_URL" --output "$YT_DLP_TEMP"
install -m 755 "$YT_DLP_TEMP" "$LIBEXEC_DIR/yt-dlp"

DENO_URL="https://github.com/denoland/deno/releases/latest/download/deno-$DENO_TARGET.zip"
run_step "[5/5] Downloading Deno runtime" \
    curl --fail --location --silent --show-error "$DENO_URL" --output "$TEMP_DIR/deno.zip"
run_step "      Installing Deno runtime" \
    unzip -oq "$TEMP_DIR/deno.zip" -d "$TEMP_DIR/deno"
install -m 755 "$TEMP_DIR/deno/deno" "$LIBEXEC_DIR/deno"

echo
echo "Installed successfully."
echo "Run: termphonic"

case ":$PATH:" in
    *":$BIN_DIR:"*) ;;
    *) echo "Note: add $BIN_DIR to your PATH." ;;
esac
