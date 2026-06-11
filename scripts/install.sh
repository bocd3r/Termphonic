#!/usr/bin/env bash

set -euo pipefail

APP_NAME="termphonic"
REPO_OWNER="${TERMPHONIC_REPO_OWNER:-bocd3r}"
REPO_NAME="${TERMPHONIC_REPO_NAME:-Termphonic}"
VERSION="${1:-latest}"
INSTALL_DIR="${TERMPHONIC_INSTALL_DIR:-$HOME/.local/bin}"
TMP_DIR="$(mktemp -d)"

cleanup() {
    rm -rf "$TMP_DIR"
}
trap cleanup EXIT

require_command() {
    local command_name="$1"
    if ! command -v "$command_name" >/dev/null 2>&1; then
        echo "Missing required dependency: $command_name" >&2
        exit 1
    fi
}

detect_arch() {
    case "$(uname -m)" in
        x86_64) echo "x86_64" ;;
        aarch64 | arm64) echo "aarch64" ;;
        *)
            echo "Unsupported architecture: $(uname -m)" >&2
            exit 1
            ;;
    esac
}

release_url_base() {
    local version="$1"
    if [ "$version" = "latest" ]; then
        echo "https://github.com/$REPO_OWNER/$REPO_NAME/releases/latest/download"
    else
        echo "https://github.com/$REPO_OWNER/$REPO_NAME/releases/download/$version"
    fi
}

main() {
    require_command curl
    require_command sha256sum
    require_command install

    local arch
    arch="$(detect_arch)"
    local base_url
    base_url="$(release_url_base "$VERSION")"

    local binary_path="$TMP_DIR/$APP_NAME"
    local checksum_path="$TMP_DIR/$APP_NAME.sha256"

    echo "Downloading Termphonic ${VERSION} for linux-${arch}..."
    curl --fail --location --silent --show-error \
        "$base_url/$APP_NAME" \
        --output "$binary_path"
    curl --fail --location --silent --show-error \
        "$base_url/$APP_NAME.sha256" \
        --output "$checksum_path"

    (
        cd "$TMP_DIR"
        sha256sum -c "$(basename "$checksum_path")"
    )

    mkdir -p "$INSTALL_DIR"
    install -m 755 "$binary_path" "$INSTALL_DIR/$APP_NAME"

    echo "Installed $APP_NAME to $INSTALL_DIR/$APP_NAME"
    case ":$PATH:" in
        *":$INSTALL_DIR:"*) ;;
        *) echo "Note: add $INSTALL_DIR to your PATH." ;;
    esac
}

main
