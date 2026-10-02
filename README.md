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

# Filter by exact process name (without .exe suffix)
portlens --process node

# Filter by substring match in process name
portlens --grep docker

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

When stdout is an interactive terminal, PortLens also prints a small shortcut footer to stderr after the table. Redirected and piped stdout stays clean. Hide the footer with `--no-tips` or by setting `PORTLENS_NO_TIPS=1`.

The table renderer now trims wide text columns to fit the current terminal width instead of overflowing past the right edge, and it falls back to the compact layout when border overhead alone cannot fit on a narrow terminal.

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

### Option C: Install via Cargo

```bash
cargo install portlens
```

## Benchmarking

The repository uses Criterion for microbenchmarks. Run the suite locally with:

```bash
cargo bench --bench benchmarks
```

Pull requests targeting `main` also run an advisory benchmark job in CI. That
job compares a baseline-compatible subset against the merge-base, then runs the
full PR-head suite and uploads a `benchmark-reports-<sha>` artifact containing
both raw console logs and the generated `target/criterion/` reports so you can
inspect exact timings and graphs from the Actions UI.

CI also applies coarse absolute budgets to a small set of benchmarked hot
paths. That separate budget layer catches large slowdowns for new benchmark
names even before they have historical baselines on `main`.

Windows builds embed an Explorer icon when `assets/icon.ico` is present. The
current `assets/icon.png` is source artwork only. Add a multi-size `.ico` file
at `assets/icon.ico` with at least `16x16`, `32x32`, `48x48`, and `256x256`
images so Windows can select the best size for Explorer and shell views.

---

## CLI Reference

| Flag               | Short | Description                                                                                |
| ------------------ | ----- | ------------------------------------------------------------------------------------------ |
| `--all`            | `-a`  | Show all ports (bypass developer-relevance filter)                                         |
| `--full`           | `-f`  | Show all columns (adds STATE, USER)                                                        |
| `--compact`        | `-c`  | Use compact borderless table style                                                         |
| `--tcp`            | `-t`  | Show only TCP sockets                                                                      |
| `--udp`            | `-u`  | Show only UDP sockets                                                                      |
| `--listen`         | `-l`  | Show only sockets in LISTEN state (TCP only)                                               |
| `--port <PORT>`    | `-p`  | Filter results to a port or range (e.g. `3000` or `3000-4000`) and bypass the smart filter |
| `--process <NAME>` |       | Filter by exact process name (case-insensitive, `.exe` suffix stripped)                    |
| `--grep <TEXT>`    |       | Filter by substring match in process name (case-insensitive)                               |
| `--no-header`      |       | Suppress the column header row                                                             |
| `--json`           |       | Output results as a JSON array                                                             |
| `--no-enrich`      |       | Disable Docker/Podman, project-root, and config-file enrichment                            |
| `--no-tips`        |       | Hide the tips panel shown after the table (same as setting `PORTLENS_NO_TIPS`)             |
| `--trace`          |       | Print diagnostic trace logs to stderr; also accepted with `kill` and `update`              |
| `--version`        | `-v`  | Print the version string and exit                                                          |
| `--help`           | `-h`  | Print usage information and exit                                                           |

**Note:** `--tcp` and `--udp` are mutually exclusive. `--listen` also conflicts with `--udp` because UDP sockets do not have a LISTEN state. `--process` and `--grep` are mutually exclusive.

### Subcommand: `kill`

Terminate processes by port or PID. Exactly one of `--port` or `--pid` must be provided.

```bash
portlens kill --port 3000          # Free local port :3000 (graceful on Unix)
portlens kill --port 3000-4000     # Free all listeners in a port range
portlens kill --pid 12345          # Kill a single PID
portlens kill --port 3000 --force  # SIGKILL on Unix (Windows is always forceful)
portlens kill --port 3000 --yes    # Skip the confirmation prompt
portlens kill --port 3000 --dry-run
portlens kill --pid 12345 --dry-run --json
portlens kill --pid 12345 --json
```

| Flag            | Short | Description                                                                             |
| --------------- | ----- | --------------------------------------------------------------------------------------- |
| `--port <PORT>` | `-p`  | Kill TCP listeners or UDP binders on a local port or range (e.g. `3000` or `3000-4000`) |
| `--pid <num>`   |       | Kill the specified PID                                                                  |
| `--force`       | `-f`  | Forceful termination (SIGKILL on Unix; no-op on Windows - already forceful)             |
| `--yes`         | `-y`  | Skip interactive confirmation (required when stdin is not a terminal)                   |
| `--dry-run`     |       | List resolved targets without signaling anything                                        |
| `--json`        |       | Emit the kill report or dry-run target list as JSON                                     |

Safety:

- PortLens never kills protected processes: PID 0 (kernel/idle), PID 1 (init) on Unix, PID 4 (System) on Windows, its own PID, and critical operating system processes (`csrss`, `wininit`, `winlogon`, `lsass`, `services`, `smss` on Windows; `init`, `systemd`, `launchd` on Unix). `--force` does not override this.
- A critical name alone is not enough to protect a process. On Windows its executable must live under `%SystemRoot%\System32`; on Unix it must be owned by root or have parent PID 0 or 1. When that cannot be read (for example, protected Windows processes when PortLens is not elevated), the process is treated as protected. A developer's own `services.exe` or `init` binary therefore stays killable.
- With `--pid`, a protected target is refused outright and the command fails. With `--port`, protected processes are skipped and every other target is still processed: each skipped process is reported as `skipped pid N (name): <reason>` (JSON status `protected`), in the confirmation prompt, the `--dry-run` output, and the final report. This keeps ranges such as the Windows dynamic RPC ports (49664 and up, partly owned by `lsass`, `wininit`, and `services`) usable. Skipped processes do not make the run fail; if every matching process is protected, nothing is signaled and `kill` exits 1, also with `--dry-run`.
- When stdin is not a terminal (scripts, pipes, editor tasks), `kill` refuses to run without `--yes` or `--dry-run` instead of skipping the prompt. The check happens before any targets are resolved, so it exits 2 even when nothing would match.
- Each target's process name and start time are captured when it is resolved and checked again right before it is signaled. If the PID now belongs to a different process, it is left alone and reported as `process-changed`. On Windows the check and the termination use the same process handle.
- Permission errors are reported per-PID with a hint to retry elevated; already-exited processes are treated as idempotent successes.

**Container-aware kill:** When `--port` targets a port published by a Docker or Podman container, PortLens stops the container via the daemon API (`POST /containers/{id}/stop`) instead of killing the proxy PID. This safely frees the port without disrupting the Docker/Podman daemon. With `--force`, it uses the kill endpoint for immediate termination. The confirmation prompt and `--dry-run` output will show the container name and short ID. If the stop does not succeed, the report says why. A listener is treated as a proxy when its process name or its executable name is one of the container runtime proxies listed under Duplicate suppression; if such a proxy holds the port but no container can be matched to it, `kill --port` refuses instead of killing a helper that other containers or the runtime may depend on. Use `--pid` if you genuinely need to signal the proxy process directly.

Container results and their `kill --json` status tokens:

| Status                      | Meaning                                                                                                       |
| --------------------------- | ------------------------------------------------------------------------------------------------------------- |
| `container-stopped`         | The daemon stopped (or, with `--force`, killed) the container                                                 |
| `container-already-stopped` | The container was already stopped                                                                             |
| `container-not-found`       | The daemon does not know the container; it may have been removed                                              |
| `container-unreachable`     | No container runtime daemon could be reached, so the request was never sent and the container was not touched |
| `container-no-response`     | The daemon received the stop request but did not confirm it; the container may still be stopping              |
| `container-rejected`        | The daemon refused the stop with an unexpected HTTP status, which the `hint` field names                      |
| `container-stop-failed`     | The stop failed for a reason this version of PortLens does not classify                                       |

Every container status except `container-stopped` and `container-already-stopped` makes `kill` exit 1. Before this release, unreachable, unconfirmed and refused stops were all reported as `container-stop-failed`.

### Subcommand: `update`

Check for a new release and optionally self-update the binary. Use `--check` to only check.

Every update is verified before the running binary is replaced:

- The downloaded asset's SHA-256 must match its entry in the release's `SHA256SUMS` file. If the release has no `SHA256SUMS`, or the file does not list the asset, the update is refused.
- Requests start only at this repository's release URLs for the exact release being installed; redirects must stay on HTTPS.
- The new binary is run with `--version` and must report the expected version.

To verify a download manually, run `sha256sum -c SHA256SUMS --ignore-missing` in the download folder, or check its build provenance with `gh attestation verify <file> -R ehsan18t/portlens`.

---

## Output Columns

Default columns:

| Column  | Description                                                                                                              |
| ------- | ------------------------------------------------------------------------------------------------------------------------ |
| PORT    | Local port number                                                                                                        |
| PROTO   | Protocol: TCP or UDP                                                                                                     |
| ADDRESS | Local bind IP address                                                                                                    |
| PROCESS | Process executable name                                                                                                  |
| PID     | Process identifier                                                                                                       |
| PROJECT | Project directory name or Docker container name                                                                          |
| APP     | Detected app/framework (e.g. Next.js, Express, NestJS, Spring Boot, Laravel, Symfony, Rails, Phoenix, PostgreSQL, Redis) |
| UPTIME  | Process uptime (e.g. 2h 15m, 1d 3h 15m)                                                                                  |

Additional columns with `--full`:

| Column  | Description                                                                                                                                                                                  |
| ------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| ADDRESS | Local bind IP address                                                                                                                                                                        |
| STATE   | Best-effort TCP state. On Windows each row gets the state of its own owning process; on Linux shared local sockets prefer `LISTEN`. Missing or ambiguous data shows `UNKNOWN`, UDP shows `-` |
| USER    | Owning user. Shows `-` if unavailable. On Windows, PortLens prefers the account name and falls back to a SID string when needed                                                              |

### JSON Output

`--json` prints a pretty-printed JSON array with one object per socket, or `[]` when nothing matches. Every object always has every field below, whatever the table flags: `--full`, `--compact` and `--no-header` do not affect JSON output. The filters (`--all`, `--tcp`, `--port`, `--grep` and so on) apply as usual.

| Field         | Type             | Meaning                                                                                                                         |
| ------------- | ---------------- | ------------------------------------------------------------------------------------------------------------------------------- |
| `port`        | number           | Local port number                                                                                                               |
| `local_addr`  | string           | Local bind address, IPv4 (`"127.0.0.1"`) or IPv6 (`"::1"`)                                                                      |
| `proto`       | string           | `"TCP"` or `"UDP"`                                                                                                              |
| `state`       | string           | TCP state such as `"LISTEN"`, `"ESTABLISHED"` or `"TIME_WAIT"`; `"UNKNOWN"` when it cannot be determined reliably; `"-"` for UDP |
| `pid`         | number           | Owning process ID                                                                                                               |
| `process`     | string           | Process name as reported by the OS (on Windows this includes the `.exe` suffix)                                                 |
| `user`        | string           | Owning user or account name (a SID string on Windows when no name resolves), or `"-"` if unavailable                            |
| `project`     | string or `null` | Project directory name or Docker/Podman container name, `null` when none was detected                                           |
| `app`         | string or `null` | Detected app or framework label (for example `"Next.js"` or `"PostgreSQL"`), `null` when none was detected                      |
| `uptime_secs` | number or `null` | Process uptime in seconds, `null` when unavailable                                                                              |

The other `state` values are `"SYN_SENT"`, `"SYN_RECV"`, `"FIN_WAIT1"`, `"FIN_WAIT2"`, `"CLOSE"`, `"CLOSE_WAIT"`, `"LAST_ACK"`, `"CLOSING"`, `"NEW_SYN_RECV"` (Linux) and `"DELETE_TCB"` (Windows). `kill --json` emits its own report format, not this one.

### Piped and Redirected Output

When stdout is a file or a pipe, PortLens never truncates rows to fit a width, even if `COLUMNS` is set, so `portlens | grep node` always sees full process names. `COLUMNS` only overrides the detected width when stdout is a terminal.

On Windows, redirected output uses ASCII borders (`+`, `-`, `|`) unless the console code page is UTF-8 (65001). Windows PowerShell 5.1 decodes a program's output with the console code page, so box-drawing characters in `portlens > ports.txt` would otherwise turn into mojibake. For scripts, prefer `--json`.

---

## Smart Features

**Developer-relevant filter:** By default, PortLens only shows ports belonging to known developer tools, detected projects, or Docker containers. Use `--all` to see everything.

**Explicit port queries:** `--port <PORT>` always shows matching sockets even when the owning process is not recognized as developer-relevant. Accepts a single port (`--port 3000`) or an inclusive range (`--port 3000-4000`), which is particularly useful when debugging microservice clusters assigned a port block.

**Process name filtering:** `--process <NAME>` filters by exact process name after stripping the `.exe` suffix, case-insensitively. For example, `--process node` matches both `node` and `node.exe`. `--grep <TEXT>` filters by substring match against the full process name, so `--grep docker` matches `com.docker.backend`. Both flags bypass the developer-relevance filter and are mutually exclusive.

**Interface awareness:** Listeners on the same port remain distinct when they bind to different local addresses, so `127.0.0.1:8080` and `0.0.0.0:8080` do not get merged into one row.

**Terminal-aware layout:** Wide text columns such as `PROJECT` shrink with an
ellipsis when the current terminal is narrow. If the bordered table cannot fit
cleanly, PortLens falls back to the compact layout instead of overflowing. The
interactive shortcut footer also switches between wide and compact layouts
based on available width.

**Project detection:** Walks upward from a process working directory looking for project markers (`package.json`, `Cargo.toml`, `go.mod`, `pyproject.toml`, etc.) to identify the project name.

**App/framework detection:** Identifies the technology behind a port using three strategies (in priority order):
1. Docker/Podman image name (e.g. `postgres:16` -> PostgreSQL)
2. Config files in the project root when the listener is a known runtime or a project-owned executable (e.g. `next.config.mjs` -> Next.js, `express` in the `package.json` dependencies -> Express, a `pom.xml` that uses `org.springframework.boot` -> Spring Boot, `mix.exs` depending on `:phoenix` -> Phoenix)
3. Process executable name (e.g. `nginx` -> Nginx)

**Low-overhead mode:** `--no-enrich` disables Docker/Podman probing, project-root walking, config-file scanning, and command-line path fallback. Core socket data, users, uptime, and process-name detection still remain available. Combine it with `--all` for the rawest view.

**Debug diagnostics:** Pass `--trace` to emit structured diagnostics for container probing (including why detection failed), rootless Podman lookup, and enrichment fallbacks to stderr. Example: `portlens --trace --all`. This flag is off by default.

**Docker/Podman support:** Automatically detects running containers and maps their published ports to container names and images. Works via Docker socket (Linux, including common rootless socket paths) or named pipe (Windows). Podman is supported via its compatible REST API. On Linux, auto-discovery merges results from all reachable runtimes instead of stopping at the first response, and rootless Podman `rootlessport` listeners can fall back to local Podman metadata when the API socket is unavailable to the current process. The `DOCKER_HOST` environment variable is honoured when it specifies a `unix://` socket path, an `npipe://` named pipe path, or a `tcp://` address. When a proxy-owned listener matches multiple distinct containers on the same `port + protocol`, PortLens now leaves the row unenriched instead of guessing. If Podman is installed without an active API socket, start `podman.socket` or point `DOCKER_HOST` at a running `podman system service` endpoint. If a runtime is running but PortLens may not connect to it (most often a Linux user outside the `docker` group), container names are missing from the listing and a one-line warning on stderr names the endpoint and the fix, for example `warning: container detection skipped: permission denied on /var/run/docker.sock (add your user to the docker group or run with sudo)`. The warning appears only when stderr is a terminal. Other detection failures, such as a timeout or an error reply from the daemon, are logged under `--trace`, and nothing is printed when no runtime is installed. A daemon whose published port ranges add up to more bindings than PortLens maps (131072 per daemon reply) also gets a one-line stderr warning, because some rows may then be missing their container name.

**Duplicate suppression:** Repeated rows from the same PID are collapsed, and duplicate rows of a known container runtime port proxy that belong to the same container are collapsed into one row. Recognized proxies are Docker's `docker-proxy`, Docker Desktop's `com.docker.backend`, `vpnkit` and `wslrelay`, the rootless Docker and Podman helpers (`rootlesskit`, `rootlessport`, `slirp4netns`, `pasta`), Podman machine's `gvproxy`, and Lima's `limactl` (also used by Colima and Rancher Desktop). Distinct worker PIDs and distinct non-proxy bind addresses on the same port stay visible.

---

## Permissions

PortLens runs without elevated privileges. Some sockets owned by other users or system processes may not appear in the output. Run with `sudo` (Linux) or as Administrator (Windows) for full visibility.

When stderr is attached to a terminal, PortLens warns at runtime if it detects that the current session is not elevated.

Deep enrichment may inspect executable paths, working directories, and absolute command-line paths to infer project roots. Use `--no-enrich` if you want to skip that extra metadata collection.

For environment-specific debugging, run with `--trace` to emit diagnostic output to stderr. That surfaces Docker/Podman probe failures and enrichment misses.

---

## Supported Platforms

| Platform             | Architecture | Status    |
| -------------------- | ------------ | --------- |
| Linux (kernel 4.x+)  | x86_64       | Supported |
| Windows 10 / 11      | x86_64       | Supported |
| Windows Server 2019+ | x86_64       | Supported |

---

## Environment Variables

| Variable           | Effect                                                                                                    |
| ------------------ | --------------------------------------------------------------------------------------------------------- |
| `PORTLENS_NO_TIPS` | Any non-empty value hides the tips panel, like `--no-tips`. An empty value is ignored                     |
| `COLUMNS`          | Overrides the detected terminal width when stdout is a terminal. Ignored for piped or redirected output   |

---

## Exit Codes

| Code | Meaning                                                                                                                                                                    |
| ---- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 0    | Success (including a `kill --dry-run`, and a `kill --port` run that skipped some protected processes)                                                                      |
| 1    | Runtime error, or for `kill`: a target failed, a PID was reused, a `--pid` target is protected, every `--port` match is protected, or the confirmation prompt was declined |
| 2    | Usage error (invalid flag combination, missing required argument, or `kill` without `--yes` when stdin is not a terminal)                                                  |
| 3    | `kill` selector matched no live process                                                                                                                                    |

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
