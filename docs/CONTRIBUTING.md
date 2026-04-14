# Contributing to PortLens

Thank you for your interest in contributing!

---

## Development Setup

### Prerequisites

- Rust stable toolchain (1.93+)
- `cargo-deny` (optional, for dependency audit)
- Supported lint targets:

```bash
rustup target add x86_64-unknown-linux-gnu x86_64-pc-windows-msvc
```

### Clone and Build

```bash
git clone https://github.com/ehsan18t/portlens.git
cd portlens
cargo build
```

Release builds use a size-focused Cargo profile (`opt-level = "z"`, LTO,
single codegen unit, symbol stripping, and `panic = "abort"`). Keep that bias
unless a maintainer explicitly decides a runtime-performance regression is more
important than artifact size.

Windows executable icon embedding expects `assets/icon.ico`. Keep source art in
`assets/icon.png` if you want, but the Windows resource step only embeds `.ico`
files. Use a multi-size `.ico` that includes at least `16x16`, `32x32`,
`48x48`, and `256x256` images.

### Install Git Hooks

**Windows (PowerShell):**

```powershell
.\scripts\install-hooks.ps1
```

**Linux / macOS:**

```bash
bash scripts/install-hooks.sh
```

Both scripts install pre-commit, pre-push, and commit-msg hooks that enforce
quality gates locally before CI.

The installers resolve Git's real hooks directory through Git metadata, so they
work from normal clones and linked worktrees instead of assuming `.git/hooks`
is always a plain directory under the working tree.

The Clippy gate uses `scripts/check-platform-clippy.sh` on shell-based setups
and `scripts/check-platform-clippy.ps1` on Windows PowerShell. The host target
still runs with `--all-targets`, while the other supported target lints
`--lib --bins` so Linux-only and Windows-only cfg issues fail locally without
requiring a foreign C toolchain for Criterion's benchmark dependency.

---

## Quality Gates

All of the following must pass before merging:

| Gate | Command                                                                  | Purpose                                             |
| ---- | ------------------------------------------------------------------------ | --------------------------------------------------- |
| 1    | `cargo fmt --check`                                                      | Consistent formatting                               |
| 2    | `scripts/check-platform-clippy.sh` / `scripts/check-platform-clippy.ps1` | Zero lint warnings across Linux + Windows cfg paths |
| 3    | `cargo test`                                                             | All tests pass                                      |
| 4    | `cargo bench --no-run`                                                   | Benchmarks compile                                  |
| 5    | `cargo build`                                                            | Debug build succeeds                                |
| 6    | `cargo doc --no-deps`                                                    | Documentation builds                                |
| 7    | `cargo deny check`                                                       | No vulnerable/banned deps                           |

CI runs on every push to `main` **and** on every pull request targeting `main`,
so cross-platform issues (Linux + Windows matrix) are caught before a PR is merged.

Pull requests also run an advisory benchmark job on Linux. CI first compares a
baseline-compatible subset against the merge-base on `main`, then runs the full
PR-head suite separately so new benchmark names still produce reports. CI
uploads a `benchmark-reports-<sha>` artifact that contains both raw console logs
plus the full `target/criterion/` report tree, so reviewers can inspect the
measured timings even when the job only raises a warning.

The benchmark job now also runs `scripts/check-benchmark-budget.sh` against the
full-suite `bench_output.txt` log. That script enforces coarse absolute budgets
for a small set of hot paths so brand-new benchmark names are still covered on
their first PR, even before they have a `main`-branch baseline for Criterion to
compare against.

Workflow dependencies in `.github/workflows/` are pinned to full commit SHAs.
When updating an action, keep the trailing version comment (for example `# v6`)
so reviewers can see the intended upstream release at a glance.

The current icon integration is intentionally Windows-only. Linux packages stay
CLI-only and do not install a desktop entry, so shipping only an icon image
would not surface a meaningful application icon in Linux menus.

For environment-specific diagnostics while developing, run the CLI with
`--trace` to enable tracing for Docker/Podman probing and enrichment
fallbacks. Example: `cargo run -- --trace --all`.

### Releasing

Releases are created via the **Release** workflow (`Actions` tab):

1. Go to `Actions` > `Release` > `Run workflow`.
2. Enter the version tag (e.g. `v0.2.0`).
  The published asset filenames intentionally drop any leading `v`, so the
  Windows binary is uploaded as `portlens-0.2.0-x86_64.exe`, the Linux
  archive as `portlens-0.2.0-x86_64.tar.gz`, and the package artifacts use
  the same normalized version segment.
3. GitHub groups auto-generated release notes using [.github/release.yml](.github/release.yml).
   Apply the appropriate PR labels before merging if you want a change to land
   in a specific section.
4. The workflow builds binaries for all targets, uploads `.tar.gz`, `.deb`,
  `.rpm`, and raw `.exe` assets, and creates a draft release with
  auto-generated release notes.
5. Review and publish the draft on the GitHub Releases page.

---

## Project Structure

```
src/
  main.rs        — CLI parsing, command dispatch
  lib.rs         — library crate root (module re-exports for benchmarks)
  types.rs       — PortEntry struct shared across all modules
  collector/     — socket enumeration, deduplication, and enrichment
  filter.rs      — user-specified and relevance filtering logic
  display/       — table, JSON, terminal-width, and tips rendering
  docker/        — Docker/Podman daemon transport and port matching
  kill/          — process/container target resolution and termination
  update.rs      — GitHub release checking and self-update logic
  framework.rs   — app/framework detection from images, configs, processes
  project.rs     — project root detection via cwd/cmd marker walk
```

### Architecture Boundaries

- The `collector/` module owns socket enumeration, deduplication, and process
  metadata lookup. No unrelated module should duplicate that work.
- `filter.rs` owns all filtering logic.
- The `display/` module owns all rendering logic.
- The `docker/` module owns container daemon communication and port matching.
- `framework.rs` and `project.rs` provide best-effort enrichment only.
- The `kill/` module owns target resolution and termination flows.

Interactive table output also emits a shortcut footer on stderr. Preserve that
stdout/stderr split when changing display behavior so piped data stays clean.
Keep the renderer terminal-width aware as well: wide columns should shrink
cleanly instead of forcing the table past the right edge, compact rows must
still honor the width budget, and the default bordered layout should fall back
to compact rendering when borders cannot fit.

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
- Current table output includes an `ADDRESS` column in both default and `--full` views; update README examples in the same commit if column order or naming changes

```bash
cargo test
```

---

## Dependency Policy

- Prefer `std` over external crates
- Only MIT / Apache-2.0 / BSD / MPL-2.0 licensed crates
- `cargo deny check` must pass
- Do not add new dependencies without maintainer approval
