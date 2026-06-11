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

for dependency in cargo curl tar unzip sha256sum python3; do
    if ! command -v "$dependency" > /dev/null 2>&1; then
        echo "Missing packaging dependency: $dependency" >&2
        exit 1
    fi
done

VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT_DIR/Cargo.toml" | head -1)"
BINARY_PATH="$DIST_DIR/termphonic"
CHECKSUM_PATH="$DIST_DIR/termphonic.sha256"
TEMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TEMP_DIR"' EXIT

mkdir -p "$DIST_DIR"

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

PAYLOAD_DIR="$TEMP_DIR/payload"
mkdir -p "$PAYLOAD_DIR/libexec" "$PAYLOAD_DIR/licenses"
install -m 755 "$TEMP_DIR/yt-dlp" "$PAYLOAD_DIR/libexec/yt-dlp"
install -m 755 "$TEMP_DIR/deno/deno" "$PAYLOAD_DIR/libexec/deno"
install -m 644 "$ROOT_DIR/LICENSE" "$PAYLOAD_DIR/LICENSE"
install -m 644 "$ROOT_DIR/THIRD_PARTY_NOTICES.md" "$PAYLOAD_DIR/THIRD_PARTY_NOTICES.md"
install -m 644 \
    "$TEMP_DIR/yt-dlp-THIRD_PARTY_LICENSES.txt" \
    "$PAYLOAD_DIR/licenses/yt-dlp-THIRD_PARTY_LICENSES.txt"
install -m 644 \
    "$TEMP_DIR/deno-LICENSE.md" \
    "$PAYLOAD_DIR/licenses/deno-LICENSE.md"

PAYLOAD_ARCHIVE="$TEMP_DIR/termphonic-payload.tar.gz"
tar -C "$PAYLOAD_DIR" -czf "$PAYLOAD_ARCHIVE" .
PAYLOAD_SIZE="$(stat -c '%s' "$PAYLOAD_ARCHIVE")"
PAYLOAD_SHA256="$(sha256sum "$PAYLOAD_ARCHIVE" | awk '{print $1}')"

rm -f "$BINARY_PATH" "$CHECKSUM_PATH"
install -m 755 "$ROOT_DIR/target/release/termphonic" "$BINARY_PATH"
cat "$PAYLOAD_ARCHIVE" >> "$BINARY_PATH"
python3 - "$BINARY_PATH" "$PAYLOAD_SIZE" "$PAYLOAD_SHA256" <<'PY'
import struct
import sys

binary_path = sys.argv[1]
payload_size = int(sys.argv[2])
payload_sha256 = bytes.fromhex(sys.argv[3])
magic = b"TPKGv1\0\0"
footer = magic + struct.pack("<Q", payload_size) + payload_sha256

with open(binary_path, "ab") as handle:
    handle.write(footer)
PY

(
    cd "$DIST_DIR"
    sha256sum "$(basename "$BINARY_PATH")" > "$(basename "$CHECKSUM_PATH")"
)

echo
echo "Created:"
echo "  $BINARY_PATH"
echo "  $CHECKSUM_PATH"
