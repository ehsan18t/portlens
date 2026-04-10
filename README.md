# portview

**A cross-platform CLI tool that lists open network ports and their associated processes.**

A fast, readable alternative to `netstat` and `ss`. Single binary with no external tool dependencies, works on Linux and Windows.

---

## Quick Start

```bash
# Show developer-relevant ports (default smart filter)
portview

# Show all open ports
portview --all

# Show all columns (adds STATE, USER)
portview --full

# Compact borderless table
portview --compact

# TCP only
portview --tcp

# UDP only
portview --udp

# Only listening sockets
portview --listen

# Filter to a specific port
portview --port 8080

# JSON output
portview --json

# No header (for piping)
portview --no-header
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

Full view (`portview --full`):

```
╭───────┬───────┬───────────┬────────┬──────────┬──────┬──────────┬────────────────────┬────────────┬────────╮
│ PORT  │ PROTO │ ADDRESS   │ STATE  │ PROCESS  │ PID  │ USER     │ PROJECT            │ APP        │ UPTIME │
├───────┼───────┼───────────┼────────┼──────────┼──────┼──────────┼────────────────────┼────────────┼────────┤
│ 3000  │ TCP   │ 127.0.0.1 │ LISTEN │ node     │ 8821 │ ehsan    │ my-nextjs-app      │ Next.js    │ 2h 15m │
│ 5432  │ TCP   │ 0.0.0.0   │ LISTEN │ postgres │ 902  │ postgres │ backend-postgres-1 │ PostgreSQL │ 1d 3h  │
╰───────┴───────┴───────────┴────────┴──────────┴──────┴──────────┴────────────────────┴────────────┴────────╯
```

---

## Installation

### Option A: Download Pre-built Binary

Download the latest release from the [Releases](https://github.com/ehsan18t/portview/releases) page.

| Platform            | Package                            |
| ------------------- | ---------------------------------- |
| Linux x86-64        | `portview-<version>-x86_64.tar.gz` |
| Linux x86-64 (.deb) | `portview-<version>-amd64.deb`     |
| Linux x86-64 (.rpm) | `portview-<version>-x86_64.rpm`    |
| Windows x86-64      | `portview-<version>-x86_64.exe`    |

For Debian/Ubuntu: `sudo dpkg -i portview-<version>-amd64.deb`
For Fedora/RHEL: `sudo rpm -i portview-<version>-x86_64.rpm`

### Option B: Build from Source

```bash
git clone https://github.com/ehsan18t/portview.git
cd portview
cargo build --release
# Binary is at: target/release/portview (or portview.exe on Windows)
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
cargo install portview
```

---

## CLI Reference

| Flag           | Short | Description                                                             |
| -------------- | ----- | ----------------------------------------------------------------------- |
| `--all`        | `-a`  | Show all ports (bypass developer-relevance filter)                      |
| `--full`       | `-f`  | Show all columns (adds STATE, USER)                                     |
| `--compact`    | `-c`  | Use compact borderless table style                                      |
| `--tcp`        | `-t`  | Show only TCP sockets                                                   |
| `--udp`        | `-u`  | Show only UDP sockets                                                   |
| `--listen`     | `-l`  | Show only sockets in LISTEN state (TCP only)                            |
| `--port <num>` | `-p`  | Filter results to the specified port number and bypass the smart filter |
| `--no-header`  |       | Suppress the column header row                                          |
| `--json`       |       | Output results as a JSON array                                          |
| `--version`    | `-V`  | Print the version string and exit                                       |
| `--help`       | `-h`  | Print usage information and exit                                        |

**Note:** `--tcp` and `--udp` are mutually exclusive. `--listen` also conflicts with `--udp` because UDP sockets do not have a LISTEN state.

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

| Column  | Description                                                                                                             |
| ------- | ----------------------------------------------------------------------------------------------------------------------- |
| ADDRESS | Local bind IP address                                                                                                   |
| STATE   | Best-effort TCP state; shared local sockets prefer `LISTEN`, ambiguous non-listener mixes show `UNKNOWN`, UDP shows `-` |
| USER    | Owning user. Shows `-` if unavailable. On Windows, this may fall back to a SID string instead of an account name        |

---

## Smart Features

**Developer-relevant filter:** By default, portview only shows ports belonging to known developer tools, detected projects, or Docker containers. Use `--all` to see everything.

**Explicit port queries:** `--port <num>` always shows matching sockets even when the owning process is not recognized as developer-relevant.

**Interface awareness:** Listeners on the same port remain distinct when they bind to different local addresses, so `127.0.0.1:8080` and `0.0.0.0:8080` do not get merged into one row.

**Project detection:** Walks upward from a process working directory looking for project markers (`package.json`, `Cargo.toml`, `go.mod`, `pyproject.toml`, etc.) to identify the project name.

**App/framework detection:** Identifies the technology behind a port using three strategies (in priority order):
1. Docker/Podman image name (e.g. `postgres:16` -> PostgreSQL)
2. Config files in the project root (e.g. `next.config.mjs` -> Next.js)
3. Process executable name (e.g. `nginx` -> Nginx)

**Docker/Podman support:** Automatically detects running containers and maps their published ports to container names and images. Works via Docker socket (Linux, including common rootless socket paths) or named pipe (Windows). Podman is supported via its compatible REST API. On Linux, auto-discovery merges results from all reachable runtimes instead of stopping at the first response, and rootless Podman `rootlessport` listeners can fall back to local Podman metadata when the API socket is unavailable to the current process. The `DOCKER_HOST` environment variable is honoured when it specifies a `unix://` socket path, an `npipe://` named pipe path, or a `tcp://` address. If Podman is installed without an active API socket, start `podman.socket` or point `DOCKER_HOST` at a running `podman system service` endpoint.

**Duplicate suppression:** Repeated rows from the same PID are collapsed, and known Docker proxy duplicates are collapsed into one row. Distinct worker PIDs and distinct non-proxy bind addresses on the same port stay visible.

---

## Permissions

portview runs without elevated privileges. Some sockets owned by other users or system processes may not appear in the output. Run with `sudo` (Linux) or as Administrator (Windows) for full visibility.

---

## Supported Platforms

| Platform             | Architecture | Status    |
| -------------------- | ------------ | --------- |
| Linux (kernel 4.x+)  | x86_64       | Supported |
| Windows 10 / 11      | x86_64       | Supported |
| Windows Server 2019+ | x86_64       | Supported |

---

## Exit Codes

| Code | Meaning                                                             |
| ---- | ------------------------------------------------------------------- |
| 0    | Success                                                             |
| 1    | Runtime error (failed to enumerate sockets or write output)         |
| 2    | Usage error (invalid flag combination or missing required argument) |

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
