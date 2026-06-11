#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
DIST_DIR="$ROOT_DIR/dist"

case "$(uname -m)" in
    x86_64)
        ARCH="x86_64"
        YT_DLP_ASSET="yt-dlp_linux"
        DENO_TARGET="x86_64-unknown-linux-gnu"
        ;;
    aarch64 | arm64)
        ARCH="aarch64"
        YT_DLP_ASSET="yt-dlp_linux_aarch64"
        DENO_TARGET="aarch64-unknown-linux-gnu"
        ;;
    *)
        echo "Unsupported packaging architecture: $(uname -m)" >&2
        exit 1
        ;;
esac

for dependency in cargo curl tar unzip sha256sum; do
    if ! command -v "$dependency" > /dev/null 2>&1; then
        echo "Missing packaging dependency: $dependency" >&2
        exit 1
    fi
done

VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT_DIR/Cargo.toml" | head -1)"
PACKAGE_NAME="termphonic-${VERSION}-linux-${ARCH}"
PACKAGE_DIR="$DIST_DIR/$PACKAGE_NAME"
ARCHIVE_PATH="$DIST_DIR/$PACKAGE_NAME.tar.gz"
TEMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TEMP_DIR"' EXIT

echo "Building Termphonic $VERSION for linux-$ARCH..."
cargo build --release --locked --manifest-path "$ROOT_DIR/Cargo.toml"

echo "Downloading standalone runtimes..."
curl --fail --location --silent --show-error \
    "https://github.com/yt-dlp/yt-dlp/releases/latest/download/$YT_DLP_ASSET" \
    --output "$TEMP_DIR/yt-dlp"
curl --fail --location --silent --show-error \
    "https://github.com/denoland/deno/releases/latest/download/deno-$DENO_TARGET.zip" \
    --output "$TEMP_DIR/deno.zip"
curl --fail --location --silent --show-error \
    "https://raw.githubusercontent.com/yt-dlp/yt-dlp/master/THIRD_PARTY_LICENSES.txt" \
    --output "$TEMP_DIR/yt-dlp-THIRD_PARTY_LICENSES.txt"
curl --fail --location --silent --show-error \
    "https://raw.githubusercontent.com/denoland/deno/main/LICENSE.md" \
    --output "$TEMP_DIR/deno-LICENSE.md"
unzip -oq "$TEMP_DIR/deno.zip" -d "$TEMP_DIR/deno"

rm -rf "$PACKAGE_DIR"
mkdir -p "$PACKAGE_DIR/libexec" "$PACKAGE_DIR/assets" "$PACKAGE_DIR/licenses"

install -m 755 "$ROOT_DIR/target/release/termphonic" "$PACKAGE_DIR/termphonic"
install -m 755 "$ROOT_DIR/install.sh" "$PACKAGE_DIR/install.sh"
install -m 755 "$TEMP_DIR/yt-dlp" "$PACKAGE_DIR/libexec/yt-dlp"
install -m 755 "$TEMP_DIR/deno/deno" "$PACKAGE_DIR/libexec/deno"
install -m 644 "$ROOT_DIR/README.md" "$PACKAGE_DIR/README.md"
install -m 644 "$ROOT_DIR/LICENSE" "$PACKAGE_DIR/LICENSE"
install -m 644 "$ROOT_DIR/THIRD_PARTY_NOTICES.md" "$PACKAGE_DIR/THIRD_PARTY_NOTICES.md"
install -m 644 \
    "$ROOT_DIR/assets/termphonic-icon-256.png" \
    "$PACKAGE_DIR/assets/termphonic-icon-256.png"
install -m 644 \
    "$TEMP_DIR/yt-dlp-THIRD_PARTY_LICENSES.txt" \
    "$PACKAGE_DIR/licenses/yt-dlp-THIRD_PARTY_LICENSES.txt"
install -m 644 \
    "$TEMP_DIR/deno-LICENSE.md" \
    "$PACKAGE_DIR/licenses/deno-LICENSE.md"

tar -C "$DIST_DIR" -czf "$ARCHIVE_PATH" "$PACKAGE_NAME"
(
    cd "$DIST_DIR"
    sha256sum "$(basename "$ARCHIVE_PATH")" > "$(basename "$ARCHIVE_PATH").sha256"
)

echo
echo "Created:"
echo "  $ARCHIVE_PATH"
echo "  $ARCHIVE_PATH.sha256"
