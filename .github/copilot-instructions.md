# Copilot & AI Agent Instructions — portview

> This document defines how AI coding agents (GitHub Copilot, Cursor, Windsurf,
> Claude, etc.) must interact with this codebase. Treat every rule here as a
> hard constraint unless the human operator explicitly overrides it.

---

## 1 · Project Identity

| Field      | Value                                                 |
| ---------- | ----------------------------------------------------- |
| Language   | Rust (edition **2024**)                               |
| Platform   | Cross-platform (Linux x86-64/aarch64, Windows x86-64) |
| Binary     | Single CLI executable                                 |
| License    | MIT                                                   |
| Min Rust   | latest stable (currently 1.93+)                       |
| Repository | `https://github.com/ehsan18t/portview`                |

---

## 2 · Coding Philosophy (non-negotiable)

1. **Zero-tolerance linting.** Clippy `all + pedantic + nursery` at **deny** level.
   Every lint violation is a compile error. Never `#[allow(...)]` a lint without a
   neighbouring comment explaining _why_.
2. **Error handling via `anyhow`.** Use `anyhow::Result` for fallible functions.
   Provide context with `.context()` / `.with_context()`. Never `unwrap()` in
   non-test code.
3. **Doc comments on every public item.** Clippy's `missing_docs` lint is active.
   Write idiomatic `///` doc comments.
4. **Functions ≤ 100 lines** (`too_many_lines` at deny). Split large blocks into
   well-named helpers.
5. **Cognitive complexity ≤ 30** per function. Prefer early returns and guard
   clauses over deep nesting.
6. **No disallowed macros:** `dbg!()`, `todo!()`, `unimplemented!()` are banned.
   Use `anyhow::bail!` or proper error handling instead.

- if you think something is wrong or what I am saying or my plan describing not matching about what you think, make sure to do a web search to find the correct information. if you find something that contradicts what I am saying, make sure to tell me about it and update the plan accordingly.
- if I report you about a issue, don't just assume I am a normal noob user who is doing things wrong. Make sure to check even if it seems what I am saying shouldn't happen. BECAUSE I AM REPORTING AFTER SEEING, AND YOU ARE ONLY ASSUMING BASED ON THE CODES.
- do super deep research always
- make the most comprehensive plan
- split the work into small tasks. you can split the tasks into todos if needed.
- maintain todos. make as many todos as needed to cover all the work that needs to be done. make sure to update the todos as you work on the project.
- commit properly. commit by task, not by file. commit right after the task is done, not the end of the session. DO NOT PROCEED TO ANOTHER TASK BEFORE YOU COMMIT THE PREVIOUS TASK. 1 session might have tasks since we are splitting the work into small tasks.

---

## 3 · Architecture Rules

```
src/
  main.rs        — entry point: CLI parsing, command dispatch
  lib.rs         — library crate root: module re-exports for benchmarks
  types.rs       — PortEntry struct shared across all modules
  collector.rs   — calls listeners + sysinfo to build Vec<PortEntry>
  filter.rs      — applies user-specified filters before display
  display.rs     — renders Vec<PortEntry> as a table or JSON
```

- **Do not create new modules** without explicit human approval.
- **Do not add new dependencies** without explicit human approval.
  If a feature can be implemented with `std` or existing deps, do that.
- `collector.rs` owns all OS interaction (socket enumeration, process lookup).
  Never scatter platform-specific calls across other modules.
- `filter.rs` owns all filtering logic. `display.rs` owns all rendering logic.
  Respect these boundaries.

---

## 4 · Core Dependencies

| Crate       | Purpose                                                    |
| ----------- | ---------------------------------------------------------- |
| listeners   | Cross-platform socket enumeration with process association |
| sysinfo     | Process metadata lookup (name, user) by PID                |
| clap        | CLI argument parsing with derive macros                    |
| comfy-table | Terminal table rendering with automatic alignment          |
| anyhow      | Error handling with context                                |
| serde/json  | JSON serialization for `--json` output                     |

---

## 5 · Formatting & Style

- **rustfmt** with `edition = "2024"`, `max_width = 100`.
- Run `cargo fmt` before every commit.
- Use `snake_case` for functions/variables, `PascalCase` for types/enums,
  `SCREAMING_SNAKE_CASE` for constants.
- Prefer `const` over `static` where possible.
- Line comments (`//`) for implementation notes; doc comments (`///`) for API docs.

---

## 6 · Testing

- Write unit tests for all pure/deterministic logic (filtering, formatting,
  type conversions).
- Tests live in `#[cfg(test)] mod tests` inside each module.
- Integration tests requiring network access or elevated privileges should be
  `#[ignore]`-d with a comment.
- Use `assert_eq!` with descriptive messages: `assert_eq!(result, expected, "reason")`.
- Run `cargo test` locally before pushing.

---

## 7 · Commit Rules

### 7.1 · Commit After Every Completed Task

Agents **must** commit immediately after completing each discrete task or fix.
Do not batch multiple unrelated changes into a single commit.

### 7.2 · Conventional Commits Format

```
<type>(<optional-scope>): <lowercase description>
```

Allowed types: `feat`, `fix`, `docs`, `style`, `refactor`, `perf`, `test`,
`build`, `ci`, `chore`, `revert`.

Rules:

- Description starts lowercase, 5–200 characters.
- No trailing period.
- Scope is optional, lowercase, alphanumeric + hyphens.

### 7.3 · Commit Description Quality

Commit messages must be **concise yet descriptive**.

Good examples:

```
feat(collector): enumerate TCP and UDP sockets via listeners crate
fix(filter): apply listen flag only to TCP entries
perf(display): avoid redundant string allocation in table render
docs: update README with installation instructions
```

---

## 8 · Git Hooks (install once)

```powershell
.\scripts\install-hooks.ps1
```

| Hook         | Gates                                                                         |
| ------------ | ----------------------------------------------------------------------------- |
| `pre-commit` | `cargo fmt --check`, `cargo clippy`, `cargo test`                             |
| `pre-push`   | 6-gate quality gate (fmt, clippy, test, docs, deny if installed, debug build) |
| `commit-msg` | Conventional Commits format validation                                        |

---

## 9 · Documentation Update Rule ⚠️

**When you change behaviour, you MUST update documentation in the same commit.**

| What changed             | Update these                 |
| ------------------------ | ---------------------------- |
| New CLI flag             | `--help` text (clap), README |
| Output format change     | README, docs/CONTRIBUTING.md |
| Build / CI change        | docs/CONTRIBUTING.md, README |
| New module               | This file, README            |
| Dependency added/removed | Cargo.toml, deny.toml        |

---

## 10 · CI Pipeline

CI runs on **pull requests to `main`** only. Two jobs:

1. **quality-gate** (Linux + Windows matrix) — fmt, clippy, test, bench compile,
   debug build, cargo doc
2. **audit** — `cargo deny check`

All gates must pass before merge. See `.github/workflows/ci.yml`.

---

## 11 · Dependency Policy

- Prefer `std` over external crates.
- Only MIT / Apache-2.0 / BSD / MPL-2.0 licensed crates.
- `cargo deny check` must pass (see `deny.toml`).
- Pin major versions in `Cargo.toml` (e.g., `"4"` not `"*"`).

---

## 12 · What NOT to Do

- ❌ Use `std::process::exit()` — return `anyhow::Result` and let `main()` handle it.
- ❌ Introduce async/await — the tool is synchronous and simple.
- ❌ Add a GUI or TUI — this is a CLI-only tool for v1.0.
- ❌ Use `unwrap()` or `expect()` outside of tests.
- ❌ Add `#[allow(clippy::*)]` without a comment justifying it.
- ❌ Commit without running all quality gates.
- ❌ Change architecture without human approval.
- ❌ Skip doc updates when behaviour changes.
- ❌ Spawn external subprocesses (netstat, ss, lsof) — use library calls only.

---

## 13 · Quick Reference for Common Tasks

### Adding a new CLI flag:

1. Add the field to the `Cli` struct in `main.rs` with clap attributes.
2. Pass it through to the relevant module (filter, display, etc.).
3. Update `--help` text, README.md.
4. Add tests for the new behaviour.
5. Run `cargo run -- --help` to verify.

### Adding a new output column:

1. Add the field to `PortEntry` in `types.rs`.
2. Populate it in `collector.rs`.
3. Render it in `display.rs` (both table and JSON).
4. Update README.md example output.
5. Add tests.
