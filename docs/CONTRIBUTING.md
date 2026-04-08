# Contributing to portview

Thank you for your interest in contributing!

---

## Development Setup

### Prerequisites

- Rust stable toolchain (1.93+)
- `cargo-deny` (optional, for dependency audit)

### Clone and Build

```bash
git clone https://github.com/ehsan18t/portview.git
cd portview
cargo build
```

### Install Git Hooks

```powershell
.\scripts\install-hooks.ps1
```

This installs pre-commit, pre-push, and commit-msg hooks that enforce quality
gates locally before CI.

---

## Quality Gates

All of the following must pass before merging:

| Gate | Command                       | Purpose                   |
| ---- | ----------------------------- | ------------------------- |
| 1    | `cargo fmt --check`           | Consistent formatting     |
| 2    | `cargo clippy -- -D warnings` | Zero lint warnings        |
| 3    | `cargo test`                  | All tests pass            |
| 4    | `cargo bench --no-run`        | Benchmarks compile        |
| 5    | `cargo build`                 | Debug build succeeds      |
| 6    | `cargo doc --no-deps`         | Documentation builds      |
| 7    | `cargo deny check`            | No vulnerable/banned deps |

---

## Project Structure

```
src/
  main.rs        — CLI parsing, command dispatch
  lib.rs         — library crate root (module re-exports for benchmarks)
  types.rs       — PortEntry struct shared across all modules
  collector.rs   — socket enumeration via listeners + sysinfo
  filter.rs      — user-specified filtering logic
  display.rs     — table and JSON rendering
```

### Architecture Boundaries

- `collector.rs` owns all OS interaction. No platform-specific code elsewhere.
- `filter.rs` owns all filtering logic.
- `display.rs` owns all rendering logic.

---

## Coding Standards

- **Clippy:** `all + pedantic + nursery` at deny level
- **Error handling:** `anyhow::Result` with `.context()`
- **No `unwrap()`** outside of tests
- **Doc comments** on every public item
- **Functions ≤ 100 lines**, cognitive complexity ≤ 30
- **No `dbg!()`, `todo!()`, `unimplemented!()`**

---

## Commit Messages

Follow [Conventional Commits](https://www.conventionalcommits.org):

```
<type>(<scope>): <description>
```

Types: `feat`, `fix`, `docs`, `style`, `refactor`, `perf`, `test`, `build`,
`ci`, `chore`, `revert`.

---

## Testing

- Unit tests live in `#[cfg(test)] mod tests` inside each module
- Use `assert_eq!` with descriptive messages
- Tests requiring network access or elevated privileges should be `#[ignore]`-d

```bash
cargo test
```

---

## Dependency Policy

- Prefer `std` over external crates
- Only MIT / Apache-2.0 / BSD / MPL-2.0 licensed crates
- `cargo deny check` must pass
- Do not add new dependencies without maintainer approval
