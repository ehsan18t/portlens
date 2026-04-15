# Crate Extraction Guide

This document captures the analysis of portlens modules that are candidates
for extraction into standalone crates. Use this as a reference when
separating each module in the future, so you don't need to re-read
the full codebase for context.

---

## 1. Docker Module -> `nanodock` (DONE)

Extracted to `../nanodock/`. See that crate's README for details.

---

## 2. Framework Detection -> `what-stack`

**Source:** `src/framework.rs` (~600 lines)

**What it does:**
Detects what application, framework, or technology stack is associated
with a process/project/container. Three detection strategies:

1. **Image-based** (`detect_from_image`): Parses Docker image names to
   identify databases (PostgreSQL, MySQL, Redis, MongoDB, etc.),
   web servers (Nginx, Apache, Caddy, Traefik), message brokers
   (RabbitMQ), search engines (Elasticsearch, OpenSearch), and
   language runtimes (Node.js, Python, Ruby, Go, Rust, Java, .NET).

2. **Config-based** (`detect_from_config`): Scans a project root
   directory for marker config files. Detects: Next.js, Nuxt, Angular,
   SvelteKit, Astro, Vite, Remix, Gatsby, Vue, Webpack, Cargo, Go,
   Maven, Gradle, PHP/Composer, Elixir/Mix, Deno, Django, FastAPI,
   Flask, Starlette, Litestar, Rails, .NET. For Python, it reads
   entry files (app.py, main.py, etc.) and dependency files
   (pyproject.toml, requirements.txt) to detect specific frameworks.

3. **Process-based** (`detect_from_process`): Matches known executable
   names (nginx, postgres, mysqld, redis-server, node, ruby, python,
   etc.) to their application labels.

**Key types:**
- `ConfigMatchKind` enum (Exact, Prefix)
- `ProjectFiles` struct (cached `HashSet<String>` of filenames with
  `contains_exact()`, `contains_prefix()`, `read_text()` methods)
- Returns `Option<AppLabel>` where `AppLabel = Cow<'static, str>`

**Dependencies from portlens:**
- `crate::docker::ContainerInfo` (for `detect_from_image` - only uses
  the `image` field as a string)
- `crate::types::AppLabel` (just a type alias for `Cow<'static, str>`)

**External crate deps:** None (pure `std::fs` + `std::path`)

**Consumers in portlens:**
- `collector/entry.rs` calls all three detection functions
- The `ContainerInfo` dependency is the only coupling to the docker
  module - could be replaced with a simple `&str` image parameter

**Extraction notes:**
- Near-zero external dependencies, making it an ideal standalone crate
- Replace `detect_from_image(info: &ContainerInfo)` with
  `detect_from_image(image: &str)` to remove the docker coupling
- `AppLabel` type alias can live in the new crate
- `ProjectFiles` struct and all detection logic are fully self-contained
- Could be useful for any tool that needs to identify project stacks

---

## 3. Project Root Detection -> standalone crate

**Source:** `src/project.rs` (~400 lines)

**What it does:**
Walks upward from a starting directory to find the project root by
checking for marker files (package.json, Cargo.toml, go.mod,
pyproject.toml, pom.xml, build.gradle, Gemfile, etc.). Also provides
`home_dir()` for resolving the user's home directory on all platforms.

**Key types/functions:**
- `PROJECT_MARKERS` (HashSet of 13 marker filenames)
- `PROJECT_MARKER_EXTENSIONS` (array: csproj, fsproj)
- `MAX_WALK_DEPTH` = 64
- `find_from_dir(start, home) -> Option<PathBuf>` - main entry point
- `walk_ancestors(start, home) -> impl Iterator<Item = &Path>` - lazy
  upward walk with home ceiling and depth limit
- `home_dir() -> Option<PathBuf>` - cross-platform home directory
  resolution (Unix: passwd DB with SUDO_UID/SUDO_HOME fallback;
  Windows: USERPROFILE)
- `absolute_cmd_parents(cmd) -> impl Iterator<Item = &Path>` - extracts
  parent dirs from absolute paths in command-line arguments

**Dependencies from portlens:** None (fully self-contained)

**External crate deps:** `libc` (Unix only, for `getpwuid_r`)

**Consumers in portlens:**
- `collector/entry.rs` calls `find_from_dir()` for project root
- `collector/resolve.rs` calls `find_from_dir()` for project caching
- `collector/mod.rs` calls `home_dir()` for context
- `docker/mod.rs` calls `home_dir()` for Unix socket paths (after
  nanodock extraction, portlens passes home_dir as a parameter)
- `kill/resolve.rs` calls `home_dir()`

**Extraction notes:**
- Completely self-contained with zero coupling to other portlens modules
- Only external dep is `libc` for Unix home dir resolution
- The `home_dir()` function alone is valuable as a robust cross-platform
  home directory resolver (handles sudo, passwd DB, USERPROFILE)
- Could split into two crates or keep together: project-root-finding
  + home-dir resolution

---

## 4. Self-Update Module -> standalone crate

**Source:** `src/update.rs` (~800 lines)

**What it does:**
Checks GitHub releases for newer versions of the CLI tool and
optionally downloads and installs the update. Supports multiple
installation methods per platform.

**Key types/functions:**
- `run(check_only: bool) -> Result<()>` - orchestrates the full flow
- `Release` struct (tag_name, html_url, assets)
- `Asset` struct (name, browser_download_url, size_bytes)
- `LinuxInstallMethod` enum (Deb, Rpm, TarGz)
- `fetch_latest_release() -> Result<Option<Release>>` - GitHub API
- `download_and_replace_windows(asset, binary_path)` - Windows update
- `download_and_replace_linux_tar(asset, binary_path)` - Linux tarball
- `detect_linux_install_method(binary_path)` - dpkg/rpm probe

**Dependencies from portlens:** None (fully self-contained, hardcodes
the GitHub repo URL as a constant)

**External crate deps:** `serde`, `serde_json` (GitHub API parsing).
Also shells out to `curl` for downloads and `tar` for extraction.

**Consumers in portlens:**
- `main.rs` dispatches to `update::run()` for the `update` subcommand

**Extraction notes:**
- Currently hardcodes the GitHub owner/repo. To make generic, accept
  configuration: `UpdateConfig { owner, repo, binary_name, ... }`
- Uses `std::process::Command` to call curl and tar - this is an
  exception to the "no subprocess" rule because it's for self-update,
  not for core socket enumeration
- Platform-specific binary replacement logic (Windows needs rename
  dance, Linux direct replace or package manager)
- Would be useful for any Rust CLI that wants GitHub-release-based
  self-update

---

## 5. Display/Terminal Utilities -> standalone crate

**Source:** `src/display/` (~1200 lines total)

**What it does:**
Renders formatted tables to the terminal with Unicode-aware column
width calculation, adaptive layout, and box-drawing borders.

**Submodules:**
- `render.rs` (~200 lines) - Unicode display width calculation,
  truncation with ellipsis, border styles (UTF-8 and ASCII)
- `terminal.rs` (~200 lines) - Terminal width detection (ioctl on Unix,
  GetConsoleScreenBufferInfo on Windows, COLUMNS env var fallback),
  UTF-8 support detection on Windows
- `table.rs` (~500 lines) - Column definition, width measurement,
  adaptive fitting, bordered and compact table rendering
- `tips.rs` (~250 lines) - Quick actions footer with adaptive layout

**Key types:**
- `DisplayOptions` struct (show_header, full, compact)
- `Alignment` enum (Left, Right)
- `BorderStyle` struct (box-drawing characters)
- `Column` enum with heading/value/alignment/min-width/shrink-priority

**Dependencies from portlens:**
- `types::PortEntry` (table.rs Column::value() extracts fields)
- `types::Protocol`, `types::State` (for display formatting)
- The table rendering is tightly coupled to PortEntry's field set

**External crate deps:** `libc` (Unix terminal width), Windows FFI

**Extraction notes:**
- `render.rs` (Unicode width) and `terminal.rs` (terminal detection)
  are fully generic and immediately extractable
- `table.rs` is domain-specific (Column enum maps to PortEntry fields)
  but the rendering engine (measure widths, fit columns, draw borders)
  could be made generic with a trait-based Column system
- `tips.rs` is portlens-specific UI content
- Recommendation: Extract render.rs + terminal.rs as a lightweight
  terminal utilities crate, leave table.rs in portlens or make it
  generic later

---

## Cross-cutting type: `Protocol`

The `Protocol` enum (Tcp/Udp) is used across multiple modules:
- Defined in `types.rs` with derives: Debug, Clone, Copy, PartialEq,
  Eq, PartialOrd, Ord, Hash, Serialize, Display
- Used in: types.rs, filter.rs, collector/*, display/*, docker/*,
  kill/*

After nanodock extraction, portlens re-exports `nanodock::Protocol`
in types.rs. Future crate extractions should either:
- Accept Protocol as a generic/trait parameter
- Re-export from nanodock
- Define their own if they don't need the full type
