<div align="center">
  <img src="assets/icon.png" height="96" alt="PortLens" />
  <h1>PortLens</h1>
  <p><strong>A cross-platform CLI tool that lists open network ports and their associated processes</strong></p>

  <a href="https://app.codacy.com/gh/ehsan18t/portlens/dashboard?utm_source=gh&utm_medium=referral&utm_content=&utm_campaign=Badge_grade">
    <img src="https://app.codacy.com/project/badge/Grade/e452100983664ea3a02da32b5c4bb21f" alt="Code Quality" />
  </a>
  <a href="https://github.com/ehsan18t/portlens/releases/latest">
    <img src="https://img.shields.io/github/v/tag/ehsan18t/portlens?color=blue&label=Release" alt="Release" />
  </a>
  <a href="https://github.com/ehsan18t/portlens/releases">
    <img src="https://img.shields.io/github/downloads/ehsan18t/portlens/total?label=Downloads&color=brightgreen" alt="Downloads" />
  </a>
  <img src="https://img.shields.io/badge/Platform-Linux%20%7C%20Windows-informational" alt="Platform" />
  <img src="https://img.shields.io/badge/License-MIT-success" alt="License" />
</div>
</br>


## Quick Start

```bash
# Show developer-relevant ports (default smart filter)
portlens

# Show all open ports
portlens --all

# Show all columns (adds STATE, USER)
portlens --full

# Compact borderless table
portlens --compact

# TCP only
portlens --tcp

# UDP only
portlens --udp

# Only listening sockets
portlens --listen

# Filter to a specific port
portlens --port 8080

# Filter to a port range (useful for microservice clusters)
portlens --port 3000-4000

# Disable Docker/Podman and project-root enrichment
portlens --no-enrich

# Lowest-overhead raw view
portlens --all --no-enrich

# JSON output
portlens --json

# No header (for piping)
portlens --no-header
```

---

## Example Output

Default view (developer-relevant ports with enrichment):

```
╭───────┬───────┬───────────┬──────────┬──────┬────────────────────┬────────────┬────────╮
│ PORT  │ PROTO │ ADDRESS   │ PROCESS  │ PID  │ PROJECT            │ APP        │ UPTIME │
├───────┼───────┼───────────┼──────────┼──────┼────────────────────┼────────────┼────────┤
│ 3000  │ TCP   │ 127.0.0.1 │ node     │ 8821 │ my-nextjs-app      │ Next.js    │ 2h 15m │
│ 5432  │ TCP   │ 0.0.0.0   │ postgres │ 902  │ backend-postgres-1 │ PostgreSQL │ 1d 3h  │
│ 6379  │ TCP   │ 127.0.0.1 │ redis    │ 1201 │ backend-redis-1    │ Redis      │ 1d 3h  │
│ 8080  │ TCP   │ 0.0.0.0   │ node     │ 9102 │ api-server         │ Vite       │ 45m    │
╰───────┴───────┴───────────┴──────────┴──────┴────────────────────┴────────────┴────────╯
```

Full view (`portlens --full`):

```
╭───────┬───────┬───────────┬────────┬──────────┬──────┬──────────┬────────────────────┬────────────┬────────╮
│ PORT  │ PROTO │ ADDRESS   │ STATE  │ PROCESS  │ PID  │ USER     │ PROJECT            │ APP        │ UPTIME │
├───────┼───────┼───────────┼────────┼──────────┼──────┼──────────┼────────────────────┼────────────┼────────┤
│ 3000  │ TCP   │ 127.0.0.1 │ LISTEN │ node     │ 8821 │ ehsan    │ my-nextjs-app      │ Next.js    │ 2h 15m │
│ 5432  │ TCP   │ 0.0.0.0   │ LISTEN │ postgres │ 902  │ postgres │ backend-postgres-1 │ PostgreSQL │ 1d 3h  │
╰───────┴───────┴───────────┴────────┴──────────┴──────┴──────────┴────────────────────┴────────────┴────────╯
```

When stdout is an interactive terminal, PortLens also prints a small shortcut
footer to stderr after the table. Redirected and piped stdout stays clean.
The table renderer now trims wide text columns to fit the current terminal
width instead of overflowing past the right edge, and it falls back to the
compact layout when border overhead alone cannot fit on a narrow terminal.

---

## Installation

### Option A: Download Pre-built Binary

Download the latest release from the [Releases](https://github.com/ehsan18t/portlens/releases) page.

| Platform            | Package                            |
| ------------------- | ---------------------------------- |
| Linux x86-64        | `portlens-<version>-x86_64.tar.gz` |
| Linux x86-64 (.deb) | `portlens-<version>-amd64.deb`     |
| Linux x86-64 (.rpm) | `portlens-<version>-x86_64.rpm`    |
| Windows x86-64      | `portlens-<version>-x86_64.exe`    |

Release tags may include a leading `v`, but published asset filenames omit it.
For example, release tag `v0.2.0` uploads `portlens-0.2.0-x86_64.exe`.

For Debian/Ubuntu: `sudo dpkg -i portlens-<version>-amd64.deb`
For Fedora/RHEL: `sudo rpm -i portlens-<version>-x86_64.rpm`

### Option B: Build from Source

```bash
git clone https://github.com/ehsan18t/portlens.git
cd portlens
cargo build --release
# Binary is at: target/release/portlens (or portlens.exe on Windows)
```

Release builds use a size-focused profile (`opt-level = "z"`, LTO, symbol
stripping, single codegen unit, and `panic = "abort"`) so the shipped CLI
stays compact, especially on Windows.

Windows builds embed an Explorer icon when `assets/icon.ico` is present. The
current `assets/icon.png` is source artwork only. Add a multi-size `.ico` file
at `assets/icon.ico` with at least `16x16`, `32x32`, `48x48`, and `256x256`
images so Windows can select the best size for Explorer and shell views.

### Option C: Install via Cargo

```bash
cargo install portlens
```

---

## CLI Reference

| Flag            | Short | Description                                                                                |
| --------------- | ----- | ------------------------------------------------------------------------------------------ |
| `--all`         | `-a`  | Show all ports (bypass developer-relevance filter)                                         |
| `--full`        | `-f`  | Show all columns (adds STATE, USER)                                                        |
| `--compact`     | `-c`  | Use compact borderless table style                                                         |
| `--tcp`         | `-t`  | Show only TCP sockets                                                                      |
| `--udp`         | `-u`  | Show only UDP sockets                                                                      |
| `--listen`      | `-l`  | Show only sockets in LISTEN state (TCP only)                                               |
| `--port <PORT>` | `-p`  | Filter results to a port or range (e.g. `3000` or `3000-4000`) and bypass the smart filter |
| `--no-header`   |       | Suppress the column header row                                                             |
| `--json`        |       | Output results as a JSON array                                                             |
| `--no-enrich`   |       | Disable Docker/Podman, project-root, and config-file enrichment                            |
| `--version`     | `-v`  | Print the version string and exit                                                          |
| `--help`        | `-h`  | Print usage information and exit                                                           |

**Note:** `--tcp` and `--udp` are mutually exclusive. `--listen` also conflicts with `--udp` because UDP sockets do not have a LISTEN state.

### Subcommand: `kill`

Terminate processes by port or PID. Exactly one of `--port` or `--pid` must be provided.

```bash
portlens kill --port 3000          # Free local port :3000 (graceful on Unix)
portlens kill --pid 12345          # Kill a single PID
portlens kill --port 3000 --force  # SIGKILL on Unix (Windows is always forceful)
portlens kill --port 3000 --yes    # Skip the confirmation prompt
portlens kill --port 3000 --dry-run
portlens kill --pid 12345 --dry-run --json
portlens kill --pid 12345 --json
```

| Flag           | Short | Description                                                                         |
| -------------- | ----- | ----------------------------------------------------------------------------------- |
| `--port <num>` | `-p`  | Kill TCP listeners or UDP binders on this local port (dedups IPv4/IPv6 and workers) |
| `--pid <num>`  |       | Kill the specified PID                                                              |
| `--force`      | `-f`  | Forceful termination (SIGKILL on Unix; no-op on Windows - already forceful)         |
| `--yes`        | `-y`  | Skip interactive confirmation                                                       |
| `--dry-run`    |       | List resolved targets without signaling anything                                    |
| `--json`       |       | Emit the kill report or dry-run target list as JSON                                 |

Safety: PortLens refuses to kill PID 0 (kernel/idle), PID 1 (init) on Unix, PID 4 (System) on Windows, and its own PID. Permission errors are reported per-PID with a hint to retry elevated; already-exited processes are treated as idempotent successes.

**Container-aware kill:** When `--port` targets a port published by a Docker or Podman container, PortLens stops the container via the daemon API (`POST /containers/{id}/stop`) instead of killing the proxy PID. This safely frees the port without disrupting the Docker/Podman daemon. With `--force`, it uses the kill endpoint for immediate termination. The confirmation prompt and `--dry-run` output will show the container name and short ID. If the daemon is unreachable, the failure is reported explicitly. Use `--pid` if you genuinely need to signal the proxy process directly.

### Subcommand: `update`

Check for a new release and optionally self-update the binary. Use `--check` to only check.

---

## Output Columns

Default columns:

| Column  | Description                                              |
| ------- | -------------------------------------------------------- |
| PORT    | Local port number                                        |
| PROTO   | Protocol: TCP or UDP                                     |
| ADDRESS | Local bind IP address                                    |
| PROCESS | Process executable name                                  |
| PID     | Process identifier                                       |
| PROJECT | Project directory name or Docker container name          |
| APP     | Detected app/framework (e.g. Next.js, PostgreSQL, Redis) |
| UPTIME  | Process uptime (e.g. 2h 15m, 1d 3h 15m)                  |

Additional columns with `--full`:

| Column  | Description                                                                                                                        |
| ------- | ---------------------------------------------------------------------------------------------------------------------------------- |
| ADDRESS | Local bind IP address                                                                                                              |
| STATE   | Best-effort TCP state; shared local sockets prefer `LISTEN`, missing or ambiguous non-listener data shows `UNKNOWN`, UDP shows `-` |
| USER    | Owning user. Shows `-` if unavailable. On Windows, PortLens prefers the account name and falls back to a SID string when needed    |

---

## Smart Features

**Developer-relevant filter:** By default, PortLens only shows ports belonging to known developer tools, detected projects, or Docker containers. Use `--all` to see everything.

**Explicit port queries:** `--port <PORT>` always shows matching sockets even when the owning process is not recognized as developer-relevant. Accepts a single port (`--port 3000`) or an inclusive range (`--port 3000-4000`), which is particularly useful when debugging microservice clusters assigned a port block.

**Interface awareness:** Listeners on the same port remain distinct when they bind to different local addresses, so `127.0.0.1:8080` and `0.0.0.0:8080` do not get merged into one row.

**Terminal-aware layout:** Wide text columns such as `PROJECT` shrink with an
ellipsis when the current terminal is narrow. If the bordered table cannot fit
cleanly, PortLens falls back to the compact layout instead of overflowing. The
interactive shortcut footer also switches between wide and compact layouts
based on available width.

**Project detection:** Walks upward from a process working directory looking for project markers (`package.json`, `Cargo.toml`, `go.mod`, `pyproject.toml`, etc.) to identify the project name.

**App/framework detection:** Identifies the technology behind a port using three strategies (in priority order):
1. Docker/Podman image name (e.g. `postgres:16` -> PostgreSQL)
2. Config files in the project root when the listener is a known runtime or a project-owned executable (e.g. `next.config.mjs` -> Next.js)
3. Process executable name (e.g. `nginx` -> Nginx)

**Low-overhead mode:** `--no-enrich` disables Docker/Podman probing, project-root walking, config-file scanning, and command-line path fallback. Core socket data, users, uptime, and process-name detection still remain available. Combine it with `--all` for the rawest view.

**Debug diagnostics:** Set `RUST_LOG=debug` in your shell before running PortLens to emit structured diagnostics for container probing, rootless Podman lookup, and enrichment fallbacks. In PowerShell, use `$env:RUST_LOG = 'debug'; portlens --all`. This stays off by default.

**Docker/Podman support:** Automatically detects running containers and maps their published ports to container names and images. Works via Docker socket (Linux, including common rootless socket paths) or named pipe (Windows). Podman is supported via its compatible REST API. On Linux, auto-discovery merges results from all reachable runtimes instead of stopping at the first response, and rootless Podman `rootlessport` listeners can fall back to local Podman metadata when the API socket is unavailable to the current process. The `DOCKER_HOST` environment variable is honoured when it specifies a `unix://` socket path, an `npipe://` named pipe path, or a `tcp://` address. When a proxy-owned listener matches multiple distinct containers on the same `port + protocol`, PortLens now leaves the row unenriched instead of guessing. If Podman is installed without an active API socket, start `podman.socket` or point `DOCKER_HOST` at a running `podman system service` endpoint.

**Duplicate suppression:** Repeated rows from the same PID are collapsed, and known Docker proxy duplicates are collapsed into one row. Distinct worker PIDs and distinct non-proxy bind addresses on the same port stay visible.

---

## Permissions

PortLens runs without elevated privileges. Some sockets owned by other users or system processes may not appear in the output. Run with `sudo` (Linux) or as Administrator (Windows) for full visibility.

When stderr is attached to a terminal, PortLens warns at runtime if it detects that the current session is not elevated.

Deep enrichment may inspect executable paths, working directories, and absolute command-line paths to infer project roots. Use `--no-enrich` if you want to skip that extra metadata collection.

For environment-specific debugging, enable tracing with `RUST_LOG=debug` and rerun the command. That surfaces Docker/Podman probe failures and `/proc`-based enrichment misses on stderr.

---

## Supported Platforms

| Platform             | Architecture | Status    |
| -------------------- | ------------ | --------- |
| Linux (kernel 4.x+)  | x86_64       | Supported |
| Windows 10 / 11      | x86_64       | Supported |
| Windows Server 2019+ | x86_64       | Supported |

---

## Exit Codes

| Code | Meaning                                                                     |
| ---- | --------------------------------------------------------------------------- |
| 0    | Success                                                                     |
| 1    | Runtime error (socket enumeration, I/O, or at least one kill target failed) |
| 2    | Usage error (invalid flag combination or missing required argument)         |
| 3    | `kill` selector matched no live process                                     |

---

## Contributing

See [docs/CONTRIBUTING.md](docs/CONTRIBUTING.md) for development setup and guidelines.
Install both supported lint targets once before using the local Clippy hooks or
helper scripts:

```bash
rustup target add x86_64-unknown-linux-gnu x86_64-pc-windows-msvc
```

The local cross-target Clippy helpers in `scripts/check-platform-clippy.sh` and
`scripts/check-platform-clippy.ps1` lint the host target with full coverage and
lint the other supported target's library and binary code so Linux-only and
Windows-only cfg issues fail locally instead of waiting for CI.

Release builds intentionally favor binary size over peak runtime throughput.
That keeps the distributed Windows executable substantially smaller while
preserving the existing CLI surface and output formats.

Windows executable icon embedding is handled in `build.rs` with the
`winresource` build dependency. If `assets/icon.ico` is missing, Windows builds
still succeed but emit a warning instead of embedding an icon.

CI workflow actions are pinned to full commit SHAs for supply-chain security;
preserve the trailing version comments when updating them.

---

## License

[MIT](LICENSE)
