# Enriched Port Display - Design Specification

**Date:** 2026-04-09
**Status:** Approved

---

## 1. Overview

Add project detection, app/framework identification, uptime tracking, and
Docker/Podman container awareness to portview. The goal is to transform raw
socket listings into developer-relevant context: which project owns a port,
what technology stack it belongs to, and how long it has been running.

Default output hides system noise and shows only developer-relevant ports.

---

## 2. Output Columns

### 2.1 Default Columns

| Column  | Source              | Description                                      |
|---------|---------------------|--------------------------------------------------|
| PORT    | existing            | Local port number                                |
| PROTO   | existing            | TCP or UDP                                       |
| PROCESS | existing            | Process executable name                          |
| PID     | existing            | Process identifier                               |
| PROJECT | new                 | Project folder name or Docker container name     |
| APP     | new                 | Detected framework/service (Next.js, PostgreSQL) |
| UPTIME  | new                 | Human-readable process uptime                    |

### 2.2 Full Columns (`--full`)

Adds STATE and USER to the default set, inserted after PROTO:

PORT, PROTO, STATE, PROCESS, PID, USER, PROJECT, APP, UPTIME

---

## 3. Smart Default Filter

By default, portview shows only developer-relevant ports. A port is included
if **any** of these conditions are true:

1. It belongs to a Docker or Podman container
2. A project root was detected (marker file found in cwd ancestry)
3. The process name matches a known developer-relevant process
4. `--all` is passed (disables the filter entirely, shows everything)

### 3.1 Developer-Relevant Process Names

An allowlist of process names considered relevant:

**Runtimes:** node, python, python3, ruby, java, go, deno, bun, dotnet, php,
perl, cargo, rustc, erlang, elixir

**Databases:** postgres, mysqld, mariadbd, mongod, mongos, redis-server,
memcached, clickhouse-server, cockroach

**Web servers:** nginx, apache2, httpd, caddy, traefik, envoy, haproxy

**Search/messaging:** elasticsearch, opensearch, rabbitmq-server, kafka

**Dev tools:** webpack, vite, next-server, nuxt, hugo, jekyll

This list lives as a `const` array in the filter module.

---

## 4. Project Detection

Determines the project name for a port entry. Three strategies in priority
order:

### 4.1 Docker Container Name (highest priority)

If the port maps to a Docker/Podman container, the project name is the
container name (e.g., `backend-postgres-1`).

### 4.2 Working Directory Walk

1. Obtain the process working directory via `sysinfo::Process::cwd()`
2. Walk upward from that directory, looking for project marker files
3. The directory containing the first marker found is the project root
4. The project name is the folder name of that root

**Marker files** (checked in order):

| Marker               | Ecosystem          |
|----------------------|--------------------|
| `package.json`       | Node.js / JS / TS  |
| `Cargo.toml`         | Rust               |
| `go.mod`             | Go                 |
| `pyproject.toml`     | Python             |
| `requirements.txt`   | Python             |
| `pom.xml`            | Java (Maven)       |
| `build.gradle`       | Java (Gradle)      |
| `build.gradle.kts`   | Kotlin (Gradle)    |
| any file ending `.csproj` | .NET          |
| any file ending `.fsproj` | .NET (F#)     |
| `composer.json`      | PHP                |
| `Gemfile`            | Ruby               |
| `mix.exs`            | Elixir             |
| `deno.json`          | Deno               |
| `bun.lockb`          | Bun                |

### 4.3 Command-Line Argument Fallback

If cwd is unavailable (permissions, OS limitations), parse the process
command-line arguments (`sysinfo::Process::cmd()`) to extract file paths and
apply the same marker-walk logic from the resolved path.

### 4.4 No Match

Show `-` if no project can be determined.

---

## 5. App/Framework Detection

Determines the technology behind a port. Three strategies in priority order.
**No well-known port fallback** -- if detection fails, show `-` rather than
guess.

### 5.1 Docker Image Name (highest priority)

Extract the base image name from the Docker container metadata:

| Image pattern        | App label    |
|----------------------|--------------|
| `postgres*`          | PostgreSQL   |
| `mysql*`             | MySQL        |
| `mariadb*`           | MariaDB      |
| `mongo*`             | MongoDB      |
| `redis*`             | Redis        |
| `memcached*`         | Memcached    |
| `nginx*`             | Nginx        |
| `httpd*` / `apache*` | Apache       |
| `rabbitmq*`          | RabbitMQ     |
| `localstack*`        | LocalStack   |
| `elasticsearch*`     | Elasticsearch|
| `clickhouse*`        | ClickHouse   |
| `caddy*`             | Caddy        |
| `traefik*`           | Traefik      |
| `node*`              | Node.js      |
| `python*`            | Python       |
| `ruby*`              | Ruby         |
| `golang*` / `go*`    | Go           |
| `rust*`              | Rust         |
| `openjdk*`/`eclipse-temurin*` | Java |
| `mcr.microsoft.com/dotnet*` | .NET  |

### 5.2 Config File Detection

If a project root was found (section 4.2), check for framework-specific
config files inside it:

| Config file pattern       | App label      |
|---------------------------|----------------|
| `next.config.*`           | Next.js        |
| `nuxt.config.*`           | Nuxt           |
| `angular.json`            | Angular        |
| `svelte.config.*`         | SvelteKit      |
| `astro.config.*`          | Astro          |
| `vite.config.*`           | Vite           |
| `remix.config.*`          | Remix          |
| `gatsby-config.*`         | Gatsby         |
| `vue.config.*`            | Vue CLI        |
| `webpack.config.*`        | Webpack        |
| `Cargo.toml`              | Rust           |
| `go.mod`                  | Go             |
| `manage.py`               | Django         |
| `app.py` or `wsgi.py`     | Flask          |
| `pyproject.toml`          | Python         |
| `pom.xml`                 | Java (Maven)   |
| `build.gradle`            | Java (Gradle)  |
| `build.gradle.kts`        | Kotlin (Gradle)|
| any file ending `.csproj` | .NET           |
| `composer.json`           | PHP            |
| `Gemfile` + `config.ru`   | Ruby (Rack)    |
| `mix.exs`                 | Elixir         |
| `deno.json`               | Deno           |

Detection stops at first match. More specific files (e.g., `next.config.js`)
are checked before generic ones (e.g., `package.json`).

### 5.3 Process Name Matching

If no Docker image or config file matched, map the process name:

| Process name pattern      | App label    |
|---------------------------|--------------|
| `postgres`                | PostgreSQL   |
| `mysqld`                  | MySQL        |
| `mariadbd`                | MariaDB      |
| `mongod` / `mongos`       | MongoDB      |
| `redis-server`            | Redis        |
| `memcached`               | Memcached    |
| `nginx`                   | Nginx        |
| `apache2` / `httpd`       | Apache       |
| `caddy`                   | Caddy        |
| `traefik`                 | Traefik      |
| `envoy`                   | Envoy        |
| `haproxy`                 | HAProxy      |
| `rabbitmq-server`         | RabbitMQ     |
| `elasticsearch`           | Elasticsearch|
| `clickhouse-server`       | ClickHouse   |
| `hugo`                    | Hugo         |
| `jekyll`                  | Jekyll       |
| `node`                    | Node.js      |

### 5.4 No Match

Show `-`.

---

## 6. Docker/Podman Container Detection

### 6.1 Implementation: Raw HTTP over Socket

Zero new dependencies. The Docker Engine API is HTTP over a Unix domain
socket (Linux) or named pipe (Windows). We need a single endpoint:

```
GET /v1.45/containers/json HTTP/1.0
Host: localhost

```

Response: JSON array of container objects with `Names`, `Image`, and `Ports`
fields.

### 6.2 Socket Paths

Tried in order. First successful connection wins:

1. **Docker (Linux):** `/var/run/docker.sock`
2. **Docker (Windows):** `//./pipe/docker_engine`
3. **Podman rootless (Linux):** `/run/user/{uid}/podman/podman.sock`
4. **Podman root (Linux):** `/run/podman/podman.sock`

### 6.3 Container-to-Port Mapping

From the API response, extract:

```json
{
  "Names": ["/backend-postgres-1"],
  "Image": "postgres:16",
  "Ports": [
    { "PrivatePort": 5432, "PublicPort": 5432, "Type": "tcp" }
  ]
}
```

Build a `HashMap<u16, ContainerInfo>` mapping `PublicPort` to container
name + image. The collector uses this map to enrich port entries.

### 6.4 Graceful Degradation

If the socket is unavailable, permission is denied, or the API call fails,
Docker detection is silently skipped. No error is printed. The tool
continues with local process detection only.

---

## 7. Uptime

### 7.1 Data Source

`sysinfo::Process::start_time()` returns the Unix timestamp when the process
started. Subtract from current time to get duration.

### 7.2 Display Format

| Duration        | Display  |
|-----------------|----------|
| < 1 minute      | `< 1m`   |
| < 1 hour        | `Xm`     |
| < 1 day         | `Xh Ym`  |
| >= 1 day        | `Xd Yh`  |

Show `-` if start time is unavailable.

---

## 8. Display Modes

### 8.1 Default: Bordered Table

Uses `comfy_table` with a bordered preset for a polished look:

```
+-------+-------+---------+-------+--------------------+-----------+--------+
| PORT  | PROTO | PROCESS | PID   | PROJECT            | APP       | UPTIME |
+-------+-------+---------+-------+--------------------+-----------+--------+
| 3000  | TCP   | node    | 42872 | frontend           | Next.js   | 1d 9h  |
| 5432  | TCP   | postgres| 902   | -                  | PostgreSQL| 10d 3h |
+-------+-------+---------+-------+--------------------+-----------+--------+
```

### 8.2 `--compact`: Borderless Table

Current behavior. Clean, netstat-like output with the NOTHING preset.

### 8.3 `--json`: JSON Output

Includes all fields (default + full) regardless of `--full` flag. The
`--compact` flag has no effect on JSON.

### 8.4 `--no-header`: Suppress Header Row

Works with both bordered and compact modes.

---

## 9. CLI Flags (updated)

| Flag           | Short | Description                                     |
|----------------|-------|-------------------------------------------------|
| `--tcp`        | `-t`  | Show only TCP sockets                           |
| `--udp`        | `-u`  | Show only UDP sockets                           |
| `--listen`     | `-l`  | Show only LISTEN state (TCP only)               |
| `--port <num>` | `-p`  | Filter to specific port number                  |
| `--all`        | `-a`  | Show all ports (disable developer-relevant filter)|
| `--full`       | `-f`  | Show all columns (adds STATE, USER)             |
| `--compact`    | `-c`  | Use borderless table (netstat-like)              |
| `--no-header`  |       | Suppress header row                             |
| `--json`       |       | Output as JSON array                            |
| `--version`    | `-V`  | Print version and exit                          |
| `--help`       | `-h`  | Print usage and exit                            |

---

## 10. Module Structure

```
src/
  main.rs        -- CLI args, orchestration
  lib.rs         -- public module exports
  types.rs       -- PortEntry (updated with new fields), enums
  collector.rs   -- socket enumeration, enrichment orchestration
  filter.rs      -- relevance filter (default on), protocol/port filters
  display.rs     -- bordered/compact table, column selection, uptime format
  project.rs     -- NEW: project root detection via marker file walk
  framework.rs   -- NEW: app detection (Docker image, config, process name)
  docker.rs      -- NEW: Docker/Podman socket API, container-port mapping
```

### 10.1 Data Flow

```
main.rs
  |
  v
collector::collect()
  |-- listeners::get_all()        -> raw sockets
  |-- sysinfo::System             -> PID, process name, cwd, cmd, start_time, user
  |-- docker::detect_containers() -> HashMap<port, ContainerInfo>
  |-- For each socket entry:
  |     |-- If port in Docker map -> project = container name, app from image
  |     |-- Else -> project::detect(cwd, cmd) -> project name
  |     |           framework::detect(project_root, process_name) -> app label
  |     |-- uptime from start_time
  |
  v
filter::apply()                   -> relevance filter + protocol/port filters
  |
  v
display::print_table() / print_json()
```

### 10.2 sysinfo Refresh Updates

The current `ProcessRefreshKind` only refreshes user info. It must be
extended to also refresh:

- `with_cwd()` -- for project detection
- `with_cmd()` -- for command-line fallback
- `with_start_time()` -- for uptime calculation

---

## 11. Error Handling

- Docker socket unavailable: silently skip, no error output
- Process cwd unavailable (permissions): fall back to cmd args, then `-`
- Process start_time unavailable: show `-` for uptime
- All new detection is best-effort. The tool never fails due to enrichment.
  Only the core socket enumeration (`listeners::get_all()`) can produce a
  fatal error.

---

## 12. Testing Strategy

### 12.1 Unit Tests

- **project.rs:** test marker file walk with temp directories, test
  fallback to cmd args, test no-match returns None
- **framework.rs:** test config file detection, process name matching,
  Docker image parsing. All use static data, no I/O.
- **docker.rs:** test JSON response parsing (container list to HashMap).
  Socket connection is not unit-tested (integration concern).
- **filter.rs:** test relevance filter with known/unknown process names,
  Docker entries, project-detected entries
- **display.rs:** test uptime formatting, column selection for default
  vs full mode
- **types.rs:** test new field serialization

### 12.2 Integration Concerns

Docker detection depends on a running daemon. These paths are tested
manually or in CI with Docker available. The code is structured so that
the JSON parsing logic is independently testable without a socket.

---

## 13. Future Enhancements

### 13.1 Podman Edge Cases

If Podman compatibility proves too complex due to API differences in
container naming or port mapping format, ship Docker-only and add Podman
in a follow-up release.

### 13.2 User-Defined Framework Rules

Allow users to define custom detection rules in a config file
(e.g., `~/.config/portview/frameworks.toml`) mapping process names,
config files, or Docker images to custom app labels. This would let
users extend detection without modifying source code.

### 13.3 Named Pipe Support Refinement (Windows)

Windows named pipe I/O differs from Unix domain sockets. If the initial
implementation encounters edge cases with Docker Desktop for Windows,
this can be refined in a follow-up.
