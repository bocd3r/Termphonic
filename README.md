# Termphonic

<p align="center">
  <img src="assets/termphonic-icon-256.png" alt="Termphonic logo" width="160">
</p>

<p align="center">
  A responsive terminal music player for searching and streaming audio from YouTube.
</p>

See the [changelog](CHANGELOG.md) for release notes.

## Features

- Search YouTube without leaving the terminal.
- Paginated results with 20 items per page.
- Playback queue with remove and direct-play controls.
- Play, pause, stop, volume control and 10-second seeking.
- Off, shuffle and single-track repeat modes.
- Responsive layout for narrow terminals.
- CAVA-style audio visualizer.
- Automatic session restore with queue and playback position.
- Compact duration labels for long videos and live streams.
- Standalone `yt-dlp` integration without Python or pip.
- Bundled standalone Deno runtime for YouTube JavaScript challenges.

## Requirements

- Linux
- Rust toolchain with Cargo
- FFmpeg
- Curl and unzip, for the source installer

The installer downloads standalone copies of both `yt-dlp` and Deno. No
system-wide Python, Node.js or JavaScript runtime is required.

### Debian and Ubuntu

```bash
sudo apt install cargo curl ffmpeg unzip
```

### Fedora

```bash
sudo dnf install cargo curl ffmpeg unzip
```

### Arch Linux

```bash
sudo pacman -S rust curl ffmpeg unzip
```

## Installation

### One-Line Install

Install the latest Linux release directly:

```bash
curl -fsSL https://termphonic.github.io/install.sh | bash
```

To install a specific release tag:

```bash
curl -fsSL https://termphonic.github.io/install.sh | bash -s v0.1.0
```

The script downloads the release binary, verifies the checksum, and
installs it to `~/.local/bin/termphonic`.

### Portable Release

Download the single release binary, make it executable if needed, then run:

```bash
./termphonic
```

The binary contains the media runtimes internally. On first launch it extracts
`yt-dlp` and Deno into `~/.local/share/termphonic/runtime/` and reuses them on
subsequent runs. Only FFmpeg is required from the operating system.

To install the portable binary under `~/.local`:

```bash
./install.sh
```

To remove a local installation:

```bash
./uninstall.sh
```

### From Source

Clone the repository and run:

```bash
git clone https://github.com/bocd3r/Termphonic.git termphonic
cd termphonic
./install.sh
```

In source mode, the installer:

1. Checks required system dependencies.
2. Builds the optimized Rust binary.
3. Installs it at `~/.local/bin/termphonic`.
4. Downloads the official standalone `yt-dlp` executable.
5. Installs `yt-dlp` and Deno under `~/.local/lib/termphonic/libexec`.

When run from a portable release, the same installer uses the bundled
executables and does not require Cargo, curl, unzip or network access.

Run the application with:

```bash
termphonic
```

If the command is not found, add `~/.local/bin` to your `PATH`.

## Build From Source

```bash
cargo build --release
./target/release/termphonic
```

For development:

```bash
cargo run
```

## Create a Portable Release

```bash
./scripts/package-release.sh
```

The generated binary and checksum are written to `dist/`. The release asset is
just a single executable:

```text
termphonic
```

Tagged commits matching `v*` are packaged automatically by GitHub Actions and
uploaded to the corresponding GitHub release.

## Controls

| Key | Action |
| --- | --- |
| `/` or `i` | Focus the search input |
| `Enter` | Search or play the selected item |
| `Esc` | Return to the results list |
| `Up` / `Down` | Move the selection |
| `PageUp` / `PageDown` | Change search result page |
| `Tab` | Switch between results and queue |
| `Space` | Play or pause |
| `Left` / `Right` | Seek backward or forward 10 seconds |
| `+` / `-` | Increase or decrease volume |
| `r` | Cycle Off, Shuffle and Single repeat modes |
| `s` | Stop playback |
| `d` or `Delete` | Remove the selected queue item |
| `q` | Quit |

## How It Works

Termphonic uses:

- A standalone `yt-dlp` runtime embedded in the release binary to search YouTube and resolve stream URLs.
- A private Deno runtime embedded in the release binary to solve current YouTube player challenges.
- FFmpeg to decode remote media into stereo PCM audio.
- Rodio for audio output.
- Ratatui and Crossterm for the terminal interface.

Search pages are fetched incrementally, avoiding a fixed five-result limit.
Active playback is saved periodically to
`~/.local/share/termphonic/session.json`. Reopening Termphonic resolves a fresh
stream URL and resumes from the saved position.

## Troubleshooting

### Missing or outdated runtime

```bash
./install.sh
```

This refreshes the local installation. The release binary manages its own
embedded runtimes automatically.

### Requested format is not available

Termphonic prefers audio-only streams and falls back to combined HLS streams
when necessary.

### No audio output

Check that FFmpeg and your system audio output are available:

```bash
ffmpeg -version
```

### Command not found

Add the local binary directory to your shell configuration:

```bash
export PATH="$HOME/.local/bin:$PATH"
```

## Project Assets

Logo and icon files are stored in [`assets/`](assets/).

## License

Termphonic is distributed under the [MIT License](LICENSE).
