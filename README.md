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
╭───────┬───────┬──────────┬──────┬────────────────────┬────────────┬────────╮
│ PORT  │ PROTO │ PROCESS  │ PID  │ PROJECT            │ APP        │ UPTIME │
├───────┼───────┼──────────┼──────┼────────────────────┼────────────┼────────┤
│ 3000  │ TCP   │ node     │ 8821 │ my-nextjs-app      │ Next.js    │ 2h 15m │
│ 5432  │ TCP   │ postgres │ 902  │ backend-postgres-1 │ PostgreSQL │ 1d 3h  │
│ 6379  │ TCP   │ redis    │ 1201 │ backend-redis-1    │ Redis      │ 1d 3h  │
│ 8080  │ TCP   │ node     │ 9102 │ api-server         │ Vite       │ 45m    │
╰───────┴───────┴──────────┴──────┴────────────────────┴────────────┴────────╯
```

Full view (`portview --full`):

```
╭───────┬───────┬────────┬──────────┬──────┬──────────┬────────────────────┬────────────┬────────╮
│ PORT  │ PROTO │ STATE  │ PROCESS  │ PID  │ USER     │ PROJECT            │ APP        │ UPTIME │
├───────┼───────┼────────┼──────────┼──────┼──────────┼────────────────────┼────────────┼────────┤
│ 3000  │ TCP   │ LISTEN │ node     │ 8821 │ ehsan    │ my-nextjs-app      │ Next.js    │ 2h 15m │
│ 5432  │ TCP   │ LISTEN │ postgres │ 902  │ postgres │ backend-postgres-1 │ PostgreSQL │ 1d 3h  │
╰───────┴───────┴────────┴──────────┴──────┴──────────┴────────────────────┴────────────┴────────╯
```

---

## Installation

### Option A: Download Pre-built Binary

Download the latest release from the [Releases](https://github.com/ehsan18t/portview/releases) page.

- **Linux x86-64:** `portview-linux-x86_64.tar.gz`
- **Linux aarch64:** `portview-linux-aarch64.tar.gz`
- **Windows x86-64:** `portview-windows-x86_64.zip`

### Option B: Build from Source

```bash
git clone https://github.com/ehsan18t/portview.git
cd portview
cargo build --release
# Binary is at: target/release/portview (or portview.exe on Windows)
```

### Option C: Install via Cargo

```bash
cargo install portview
```

---

## CLI Reference

| Flag           | Short | Description                                        |
| -------------- | ----- | -------------------------------------------------- |
| `--all`        | `-a`  | Show all ports (bypass developer-relevance filter) |
| `--full`       | `-f`  | Show all columns (adds STATE, USER)                |
| `--compact`    | `-c`  | Use compact borderless table style                 |
| `--tcp`        | `-t`  | Show only TCP sockets                              |
| `--udp`        | `-u`  | Show only UDP sockets                              |
| `--listen`     | `-l`  | Show only sockets in LISTEN state (TCP only)       |
| `--port <num>` | `-p`  | Filter results to the specified port number        |
| `--no-header`  |       | Suppress the column header row                     |
| `--json`       |       | Output results as a JSON array                     |
| `--version`    | `-V`  | Print the version string and exit                  |
| `--help`       | `-h`  | Print usage information and exit                   |

**Note:** `--tcp` and `--udp` are mutually exclusive. `--listen` also conflicts with `--udp` because UDP sockets do not have a LISTEN state.

---

## Output Columns

Default columns:

| Column  | Description                                              |
| ------- | -------------------------------------------------------- |
| PORT    | Local port number                                        |
| PROTO   | Protocol: TCP or UDP                                     |
| PROCESS | Process executable name                                  |
| PID     | Process identifier                                       |
| PROJECT | Project directory name or Docker container name          |
| APP     | Detected app/framework (e.g. Next.js, PostgreSQL, Redis) |
| UPTIME  | Process uptime (e.g. 2h 15m, 1d 3h 15m)                  |

Additional columns with `--full`:

| Column | Description                                                |
| ------ | ---------------------------------------------------------- |
| STATE  | Best-effort TCP state; ambiguous sockets show `UNKNOWN`, UDP shows `-` |
| USER   | Owning user. Shows `-` if unavailable                      |

---

## Smart Features

**Developer-relevant filter:** By default, portview only shows ports belonging to known developer tools, detected projects, or Docker containers. Use `--all` to see everything.

**Project detection:** Walks upward from a process working directory looking for project markers (`package.json`, `Cargo.toml`, `go.mod`, `pyproject.toml`, etc.) to identify the project name.

**App/framework detection:** Identifies the technology behind a port using three strategies (in priority order):
1. Docker/Podman image name (e.g. `postgres:16` -> PostgreSQL)
2. Config files in the project root (e.g. `next.config.mjs` -> Next.js)
3. Process executable name (e.g. `nginx` -> Nginx)

**Docker/Podman support:** Automatically detects running containers and maps their published ports to container names and images. Works via Docker socket (Linux) or named pipe (Windows). Podman is supported via its compatible REST API.

**Duplicate suppression:** Repeated rows from the same PID are collapsed, and known Docker proxy duplicates are collapsed into one row. Distinct worker PIDs on the same port stay visible.

---

## Permissions

portview runs without elevated privileges. Some sockets owned by other users or system processes may not appear in the output. Run with `sudo` (Linux) or as Administrator (Windows) for full visibility.

---

## Supported Platforms

| Platform             | Architecture | Status    |
| -------------------- | ------------ | --------- |
| Linux (kernel 4.x+)  | x86_64       | Supported |
| Linux (kernel 4.x+)  | aarch64      | Supported |
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
CI workflow actions are pinned to full commit SHAs for supply-chain security;
preserve the trailing version comments when updating them.

---

## License

[MIT](LICENSE)
