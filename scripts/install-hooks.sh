#!/bin/sh
# portview - Install git hooks
# Run once after cloning: ./scripts/install-hooks.sh

set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
HOOKS_DIR="$REPO_ROOT/hooks"
GIT_HOOKS_DIR="$REPO_ROOT/.git/hooks"

# Ensure .git/hooks directory exists
mkdir -p "$GIT_HOOKS_DIR"

install_hook() {
    hook_name="$1"
    source="$HOOKS_DIR/$hook_name"
    target="$GIT_HOOKS_DIR/$hook_name"

    if [ -f "$target" ]; then
        echo "  $hook_name: overwriting existing hook"
    fi

    cp "$source" "$target"
    chmod +x "$target"
    echo "  $hook_name: installed"
}

echo ""
echo "Installing portview git hooks..."
echo ""

install_hook "pre-commit"
install_hook "pre-push"
install_hook "commit-msg"

echo ""
echo "Git hooks installed successfully!"
echo ""
echo "Quality gates will now run automatically:"
echo ""
echo "  Commit-msg (validates commit message format):"
echo "    Format: <type>(<scope>): <description>"
echo "    Types:  feat, fix, docs, style, refactor, perf, test, build, ci, chore"
echo ""
echo "  Pre-commit (fast checks before each commit):"
echo "    1. cargo fmt --check    (formatting)"
echo "    2. cargo clippy         (lints)"
echo "    3. cargo test           (tests)"
echo ""
echo "  Pre-push (full CI-equivalent checks before each push):"
echo "    1. cargo fmt --check    (formatting)"
echo "    2. cargo clippy -D warnings (lints)"
echo "    3. cargo test           (tests)"
echo "    4. cargo bench --no-run (benchmark compile)"
echo "    5. cargo build          (debug build)"
echo "    6. cargo doc            (strict rustdoc checks)"
echo "    7. cargo deny check     (dependency audit)"
