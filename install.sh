#!/usr/bin/env bash

set -euo pipefail

APP_NAME="termphonic"
SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
BIN_DIR="$HOME/.local/bin"
DATA_DIR="$HOME/.local/share/$APP_NAME"
LIBEXEC_DIR="$HOME/.local/lib/$APP_NAME/libexec"
BUNDLE_BIN="$SCRIPT_DIR/termphonic"

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

require_commands() {
    local dependency
    for dependency in "$@"; do
        if ! command -v "$dependency" > /dev/null 2>&1; then
            echo "Missing required dependency: $dependency" >&2
            exit 1
        fi
    done
}

install_binary() {
    local binary="$1"
    mkdir -p "$BIN_DIR" "$DATA_DIR"
    pkill -x termphonic 2>/dev/null || true
    pkill -x bmusic 2>/dev/null || true
    run_step "Installing Termphonic" install -m 755 "$binary" "$BIN_DIR/$APP_NAME"
}

install_runtimes() {
    local yt_dlp="$1"
    local deno="$2"

    mkdir -p "$BIN_DIR" "$DATA_DIR" "$LIBEXEC_DIR"
    run_step "Installing media runtimes" \
        install -m 755 "$yt_dlp" "$deno" "$LIBEXEC_DIR"
}

echo
echo "Termphonic installer"
echo

if [ -x "$BUNDLE_BIN" ]; then
    require_commands ffmpeg
    install_binary "$BUNDLE_BIN"
else
    require_commands cargo curl ffmpeg unzip
    run_step "Building release binary" \
        cargo build --release --quiet --manifest-path "$SCRIPT_DIR/Cargo.toml"

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
            echo "Unsupported architecture: $(uname -m)" >&2
            exit 1
            ;;
    esac

    TEMP_DIR="$(mktemp -d)"
    trap 'rm -rf "$TEMP_DIR"' EXIT

    run_step "Downloading standalone yt-dlp" \
        curl --fail --location --silent --show-error \
        "https://github.com/yt-dlp/yt-dlp/releases/latest/download/$YT_DLP_ASSET" \
        --output "$TEMP_DIR/yt-dlp"

    run_step "Downloading Deno runtime" \
        curl --fail --location --silent --show-error \
        "https://github.com/denoland/deno/releases/latest/download/deno-$DENO_TARGET.zip" \
        --output "$TEMP_DIR/deno.zip"
    run_step "Extracting Deno runtime" \
        unzip -oq "$TEMP_DIR/deno.zip" -d "$TEMP_DIR/deno"

    install_binary "$SCRIPT_DIR/target/release/$APP_NAME"
    install_runtimes "$TEMP_DIR/yt-dlp" "$TEMP_DIR/deno/deno"
fi

echo
echo "Installed successfully."
echo "Run: termphonic"

case ":$PATH:" in
    *":$BIN_DIR:"*) ;;
    *) echo "Note: add $BIN_DIR to your PATH." ;;
esac
