# portview - Software Requirements Specification

**Version:** 1.0
**Date:** April 2026
**Status:** Draft

---

## Table of Contents

1. [Introduction](#1-introduction)
2. [Functional Requirements](#2-functional-requirements)
3. [Non-Functional Requirements](#3-non-functional-requirements)
4. [Technology Stack](#4-technology-stack)
5. [Constraints and Assumptions](#5-constraints-and-assumptions)
6. [Future Considerations](#6-future-considerations)

---

## 1. Introduction

### 1.1 Purpose

This document defines the software requirements for portview, a cross-platform command-line utility that lists open network ports and their associated processes. It serves as the reference for what the application must do, how it should behave, and which technologies it relies on.

### 1.2 Product Overview

portview is a single-binary CLI tool written in Rust. When executed, it collects information about all open TCP and UDP sockets on the host system, resolves the owning process for each socket, and prints a formatted table to standard output. The tool then exits. It targets developers, system administrators, and power users who need a fast and readable alternative to platform-native utilities like netstat or ss.

### 1.3 Scope

**In scope for v1.0:**

- Enumerating listening and established TCP/UDP sockets on the local machine
- Resolving PID and process name for each socket
- Resolving the owning user for each socket
- Displaying results in a clean, aligned table
- Filtering results via command-line flags
- Outputting results as JSON when requested

**Out of scope for v1.0:**

- Live or auto-refreshing output (watch mode)
- Remote host inspection
- Graphical or TUI interface
- Network traffic capture or packet inspection
- macOS support (may be added in a future version)

### 1.4 Target Platforms

| Platform             | Architecture | Status    |
| -------------------- | ------------ | --------- |
| Linux (kernel 4.x+)  | x86_64       | Supported |
| Linux (kernel 4.x+)  | aarch64      | Supported |
| Windows 10 / 11      | x86_64       | Supported |
| Windows Server 2019+ | x86_64       | Supported |

---

## 2. Functional Requirements

### 2.1 Default Behavior

When invoked with no arguments, portview must:

- Collect all open TCP and UDP sockets on the system, both IPv4 and IPv6
- For each socket, resolve: local address and port, protocol (TCP or UDP), connection state, PID, process name, and owning username
- Print the full results as an aligned table to stdout
- Exit with code 0 on success

### 2.2 Output Table

#### 2.2.1 Columns

The output table must include the following columns in this order:

| #   | Header  | Description                                                                                                  |
| --- | ------- | ------------------------------------------------------------------------------------------------------------ |
| 1   | PORT    | Local port number                                                                                            |
| 2   | PROTO   | Protocol: TCP or UDP                                                                                         |
| 3   | STATE   | Connection state (e.g. LISTEN, ESTABLISHED). UDP sockets display a placeholder since state is not applicable |
| 4   | PID     | Process identifier. Displays a placeholder if inaccessible due to permissions                                |
| 5   | PROCESS | Process executable name. Displays "restricted" if the PID is inaccessible                                    |
| 6   | USER    | Owning user or account name. Displays a placeholder if unavailable                                           |

#### 2.2.2 Formatting Rules

- Columns must be left-aligned and padded with spaces for consistent alignment across all rows
- A header row is printed above the data rows, separated by a blank line or a thin separator
- Rows are sorted by port number in ascending order by default
- If a process name exceeds 20 characters, it is truncated with an ellipsis
- IPv6 addresses are displayed in standard condensed notation, for example [::1]
- The table is printed to stdout so it can be piped to other tools

#### 2.2.3 Example Output

```
PORT    PROTO  STATE        PID    PROCESS         USER
22      TCP    LISTEN       1024   sshd            root
3000    TCP    LISTEN       8821   node            ehsan
5432    TCP    LISTEN       902    postgres        postgres
51820   UDP    -            1103   wg-quick        root
```

### 2.3 CLI Flags and Arguments

All flags are combinable unless noted otherwise.

| Flag           | Short | Behavior                                                                               |
| -------------- | ----- | -------------------------------------------------------------------------------------- |
| --tcp          | -t    | Show only TCP sockets                                                                  |
| --udp          | -u    | Show only UDP sockets                                                                  |
| --listen       | -l    | Show only sockets in LISTEN state (TCP only)                                           |
| --port \<num\> | -p    | Filter results to the specified port number                                            |
| --no-header    | none  | Suppress the column header row, useful when piping output                              |
| --json         | none  | Output results as a JSON array. Each entry is an object with all column fields as keys |
| --version      | -V    | Print the version string and exit                                                      |
| --help         | -h    | Print usage information and exit                                                       |

**Mutually exclusive:** --tcp and --udp may not be used together. If both are passed, the tool prints an error message to stderr and exits with code 1.

### 2.4 Permissions and Graceful Degradation

portview must run without requiring elevated privileges. However, certain socket-to-process associations may be inaccessible for sockets owned by other users or system accounts. The tool must handle this gracefully:

- If a PID cannot be resolved due to permission restrictions, the PID column displays a placeholder and PROCESS displays "restricted"
- If a username cannot be resolved, the USER column displays a placeholder
- The tool must never crash or exit with an error solely because one or more sockets are inaccessible
- A note is printed below the table when any restricted rows are present: "(*) some processes require elevated privileges to inspect"

### 2.5 Exit Codes

| Code | Meaning                                                                              |
| ---- | ------------------------------------------------------------------------------------ |
| 0    | Success. Results printed normally, even if some rows are restricted                  |
| 1    | Runtime error. Failed to enumerate sockets, for example if the OS API is unavailable |
| 2    | Usage error. Invalid flag combination or missing required argument (clap default)    |

---

## 3. Non-Functional Requirements

### 3.1 Performance

- The tool must complete and print results within 500ms on a typical desktop machine under normal load
- It must not spawn external subprocesses. No calls to netstat, ss, lsof, or similar system utilities at runtime
- All socket and process information must be retrieved directly from OS APIs or kernel interfaces

### 3.2 Portability

- The output binary must have no runtime dependencies, either statically linked or fully self-contained
- The binary must be a single executable file with no installer or companion files required
- Behavior must be consistent across supported platforms except where OS differences make it unavoidable, in which case the difference must be documented

### 3.3 Reliability

- The tool must not panic under any normal operating conditions
- All error paths must be handled with descriptive messages printed to stderr
- The tool must behave correctly on systems with a large number of open sockets (1000+) without truncating or skipping rows

### 3.4 Usability

- The default output with no flags must be immediately useful to someone familiar with netstat
- Error messages must be human-readable and suggest corrective action where possible
- Help text (--help) must list all flags with a brief description

### 3.5 Distribution

- The project must ship pre-built binaries for Linux x86_64, Linux aarch64, and Windows x86_64 via GitHub Releases
- The project must be installable via `cargo install` for users who prefer to build from source

---

## 4. Technology Stack

### 4.1 Language

The application is written entirely in Rust on the stable toolchain. The minimum supported Rust version will be established at project start and documented in the repository.

### 4.2 Core Dependencies

| Crate              | Version | Purpose                                                                                                                                                                                                                       |
| ------------------ | ------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| listeners          | latest  | Cross-platform socket enumeration with process association. Primary data source for port-to-process mapping on both Linux and Windows. Maintained by the Sniffnet project, which ensures active upkeep and real-world testing |
| sysinfo            | latest  | Process metadata lookup by PID, including executable name and owning username. Supplements listeners with user resolution                                                                                                     |
| clap               | 4.x     | CLI argument parsing. Provides flag definitions, help text generation, and input validation                                                                                                                                   |
| comfy-table        | latest  | Terminal table rendering with automatic column alignment and padding                                                                                                                                                          |
| serde + serde_json | latest  | Serialization support for --json output mode                                                                                                                                                                                  |

### 4.3 Build and Toolchain

- **Build system:** Cargo (standard Rust build tool)
- **Cross-compilation:** cargo-cross for producing Linux aarch64 and Windows x86_64 binaries from a single build machine
- **CI/CD:** GitHub Actions with matrix builds across all three target triples
- **Release artifacts:** Uploaded to GitHub Releases as .tar.gz for Linux and .zip for Windows

### 4.4 Project Structure

```
portview/
├── src/
│   ├── main.rs        # Entry point. Parses CLI args, calls collector, applies filters, renders output
│   ├── types.rs       # PortEntry struct shared across all modules
│   ├── collector.rs   # Calls listeners and sysinfo to build Vec<PortEntry>
│   ├── filter.rs      # Applies user-specified filters before display
│   └── display.rs     # Renders Vec<PortEntry> as a table or JSON
├── Cargo.toml
├── Cargo.lock
└── README.md
```

There is no platform-specific branching in main.rs or display.rs. All OS differences are encapsulated inside collector.rs, which delegates to the listeners crate.

### 4.5 Data Flow

```
CLI args (clap)
      |
      v
collector.rs  <-- listeners crate (sockets)
      |        <-- sysinfo crate (process name, user)
      v
Vec<PortEntry>
      |
      v
filter.rs  <-- applies --tcp, --udp, --listen, --port flags
      |
      v
display.rs  --> stdout (table or JSON)
```

---

## 5. Constraints and Assumptions

### 5.1 Constraints

- The tool only inspects the local machine. Remote socket inspection is out of scope
- The tool does not modify system state in any way. No process killing, no port closing
- No external binaries may be called at runtime. All data must come from library calls or direct OS API access
- The tool produces no persistent output such as log files or config files unless the user explicitly redirects stdout or stderr

### 5.2 Assumptions

- Linux kernel 4.0 or later is assumed, which guarantees availability of /proc/net/tcp and /proc/net/udp
- Windows 10 or later is assumed, which guarantees availability of GetExtendedTcpTable and GetExtendedUdpTable
- The user running portview has at minimum read access to their own process entries. Full visibility across all processes requires elevated privileges but is not required for the tool to function

### 5.3 Known Limitations

- Sockets owned by other users, including root-owned services, will appear as restricted unless portview is run with administrator or sudo privileges
- UDP sockets do not have a meaningful connection state, so the STATE column will always display a placeholder for UDP entries
- The --listen flag filters by TCP LISTEN state only. UDP has no equivalent concept and will be excluded when this flag is used

---

## 6. Future Considerations (Post v1.0)

The following features are noted for potential future versions and are not part of this specification.

| Feature                    | Notes                                                                                                                |
| -------------------------- | -------------------------------------------------------------------------------------------------------------------- |
| --watch live mode          | Auto-refreshing output at a configurable interval, similar to running watch -n1 portview                             |
| TUI interface              | An interactive terminal UI built with ratatui, with keyboard navigation and inline filtering                         |
| macOS support              | The listeners crate already supports macOS, so adding it to the target matrix would be straightforward               |
| --sort flag                | Allow sorting by columns other than port number, such as by PID or process name                                      |
| Well-known port labels     | An optional annotation column showing the common service name for known ports, for example 22 as SSH or 443 as HTTPS |
| Package manager publishing | Publishing to Homebrew, winget, and popular Linux package repositories such as the AUR                               |
| Custom framework rules     | A user config file (e.g. ~/.config/portview/frameworks.toml) allowing custom detection rules that map process names, config files, or Docker images to app labels without modifying source code |

