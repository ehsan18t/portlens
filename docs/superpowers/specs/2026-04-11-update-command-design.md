# Update Command Design Spec

**Date:** 2026-04-11
**Feature:** `portview update` — self-update from GitHub releases

## Overview

Add a `portview update` subcommand that checks GitHub for newer releases and,
on supported platforms, downloads and replaces the running binary in-place.

## CLI Interface

```
portview update            # Check + auto-update (if platform supports it)
portview update --check    # Check only, never download or replace
```

All arguments (existing and new) are **case-insensitive**: `portview UPDATE`,
`portview Update --CHECK`, `portview --TCP -P 3000` all work.

## Version Comparison

- Current version: compiled-in `env!("CARGO_PKG_VERSION")` (e.g. `0.1.0`)
- Remote version: latest GitHub release tag (e.g. `0.1.1`) — **no `v` prefix**
- Comparison: split on `.`, compare major/minor/patch numerically
- "Up to date" when remote <= current

## GitHub API

- Endpoint: `GET https://api.github.com/repos/ehsan18t/portview/releases/latest`
- Header: `Accept: application/vnd.github+json`
- Header: `User-Agent: portview/{version}`
- Parse JSON response for `tag_name` and `assets[].name` / `assets[].browser_download_url`

## Asset Selection

| Platform       | Asset pattern                        | Auto-update? |
| -------------- | ------------------------------------ | ------------ |
| Windows x86_64 | `portview-{tag}-x86_64.exe`          | Yes          |
| Linux x86_64   | `portview-{tag}-x86_64.tar.gz`       | Yes*         |
| Linux deb      | detected via `dpkg -S <binary_path>` | No — warn    |
| Linux rpm      | detected via `rpm -qf <binary_path>` | No — warn    |
| Other OS/arch  | —                                    | No — warn    |

*Linux tar.gz auto-update only when the binary is NOT managed by dpkg or rpm.

## Linux Install Method Detection

1. Resolve binary path via `std::env::current_exe()`
2. Run `dpkg -S <path>` — if exit code 0, it's a deb install
3. Run `rpm -qf <path>` — if exit code 0, it's an rpm install
4. Otherwise, assume tar.gz / manual install → allow auto-update

## Auto-Update Flow

1. Resolve `std::env::current_exe()` → canonical path of running binary
2. Query GitHub releases API for latest version
3. Compare versions; if up to date, print message and exit
4. Select the correct asset for the current platform
5. Download asset to a temp file in the same directory as the binary
   (same filesystem guarantees atomic rename)
6. Verify the downloaded size matches the GitHub release asset size metadata
7. **Windows:** rename current binary to `portview.old.exe`, rename temp to
   `portview.exe`, delete `portview.old.exe` (best-effort)
8. **Linux tar.gz:** extract the `portview` binary from the archive into temp,
   set executable permission (0o755), rename over current binary
9. Print success message with old → new version

## Check-Only Flow

1. Query GitHub releases API
2. Compare versions
3. Print result: "Up to date" or "New version available: {tag} (current: {current})"
4. For deb/rpm/unsupported: also print download URL

## Non-Auto-Update Platforms

When auto-update is not supported (deb, rpm, unsupported OS), the command:
1. Checks for updates (same API call)
2. Prints a warning: "Auto-update is not available for your installation method."
3. Prints: "A new version ({tag}) is available. Download it manually from: {url}"
4. Includes the specific asset URL for their platform when identifiable

## Error Handling

| Scenario                        | Behavior                                                                                 |
| ------------------------------- | ---------------------------------------------------------------------------------------- |
| No network / DNS failure        | "Failed to reach GitHub. Check your connection."                                         |
| GitHub API 403/429 (rate limit) | "GitHub API rate limit reached. Try again later."                                        |
| HTTP non-200                    | "GitHub API returned status {code}."                                                     |
| JSON parse failure              | "Failed to parse GitHub release data."                                                   |
| No matching asset in release    | "No compatible binary found. Download manually: {release_url}"                           |
| `current_exe()` fails           | "Cannot determine binary path."                                                          |
| Permission denied on replace    | "Permission denied. Try running with elevated privileges (sudo / Run as Administrator)." |
| Asset size mismatch             | "Download appears corrupt. Aborting update."                                             |
| Temp file write failure         | "Failed to write temporary file: {err}"                                                  |
| Rename failure                  | "Failed to replace binary: {err}"                                                        |
| Already up to date              | "portview is already up to date ({version})."                                            |
| Tar extraction failure (Linux)  | "Failed to extract update archive: {err}"                                                |

## Case-Insensitive Arguments

Preprocessing in `main()` before clap parsing:
- Collect `std::env::args_os()`
- Skip argv[0], lowercase all remaining args (`.to_ascii_lowercase()` on the
  Unicode string)
- This is safe because portview has no string-valued arguments (only numeric
  port values, which are unaffected by lowercasing)
- Clap then sees normalized lowercase args matching its defined flags/subcommands

## Dependencies

- **`ureq`** (blocking HTTP, rustls TLS) — minimal footprint, no async runtime
- **`tar` + `flate2`** — for extracting `.tar.gz` on Linux (conditional compile)

## Module Structure

New file: `src/update.rs`
- `pub fn run(check_only: bool) -> Result<()>` — main entry point
- Internal helpers: version comparison, GitHub API, platform detection,
  download, replace

## Exit Codes

- 0: success (updated, already up to date, or check-only with result shown)
- 1: runtime error (network, permissions, etc.)
