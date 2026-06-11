# Changelog

All notable changes to Termphonic are documented here.

## [0.1.1] - 2026-06-11

### Added
- Single-file Linux release: `termphonic` plus `termphonic.sha256`.
- Embedded `yt-dlp` and Deno inside the release binary, with first-run extraction to the user cache.
- One-click installation from the project site at `https://termphonic.github.io/install.sh`.

### Changed
- Moved the curl-based installer out of the app repository and into the project site repository.
- Simplified the installer output to show progress and the final `termphonic` command.

### Fixed
- Install flow now validates the downloaded binary with `sha256sum` before installing it.

## [0.1.0] - 2026-06-11

### Added
- Initial public release of Termphonic.
- Terminal search, queue, playback controls, shuffle and repeat modes.
- CAVA-style visualization, session restore, and YouTube playback integration.
