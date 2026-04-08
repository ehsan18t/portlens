# portview

**A cross-platform CLI tool that lists open network ports and their associated processes.**

A fast, readable alternative to `netstat` and `ss`. Single binary, no dependencies, works on Linux and Windows.

---

## Quick Start

```bash
# List all open ports
portview

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

```
PORT    PROTO  STATE        PID    PROCESS         USER
22      TCP    LISTEN       1024   sshd            root
3000    TCP    LISTEN       8821   node            ehsan
5432    TCP    LISTEN       902    postgres        postgres
51820   UDP    -            1103   wg-quick        root
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

| Flag           | Short | Description                                  |
| -------------- | ----- | -------------------------------------------- |
| `--tcp`        | `-t`  | Show only TCP sockets                        |
| `--udp`        | `-u`  | Show only UDP sockets                        |
| `--listen`     | `-l`  | Show only sockets in LISTEN state (TCP only) |
| `--port <num>` | `-p`  | Filter results to the specified port number  |
| `--no-header`  |       | Suppress the column header row               |
| `--json`       |       | Output results as a JSON array               |
| `--version`    | `-V`  | Print the version string and exit            |
| `--help`       | `-h`  | Print usage information and exit             |

**Note:** `--tcp` and `--udp` are mutually exclusive.

---

## Output Columns

| Column  | Description                                                |
| ------- | ---------------------------------------------------------- |
| PORT    | Local port number                                          |
| PROTO   | Protocol: TCP or UDP                                       |
| STATE   | Connection state (e.g. LISTEN, ESTABLISHED). UDP shows `-` |
| PID     | Process identifier. Shows `-` if inaccessible              |
| PROCESS | Process name. Shows `restricted` if PID is inaccessible    |
| USER    | Owning user. Shows `-` if unavailable                      |

---

## Permissions

portview runs without elevated privileges. Some sockets owned by other users or system accounts will show as restricted. Run with `sudo` (Linux) or as Administrator (Windows) for full visibility.

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
| 1    | Usage error (invalid flag combination or missing required argument) |
| 2    | Runtime error (failed to enumerate sockets)                         |

---

## Contributing

See [docs/CONTRIBUTING.md](docs/CONTRIBUTING.md) for development setup and guidelines.

---

## License

[MIT](LICENSE)
