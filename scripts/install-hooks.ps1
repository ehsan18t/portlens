#!/usr/bin/env pwsh
# PortLens - Install git hooks
# Run once after cloning: .\scripts\install-hooks.ps1

$ErrorActionPreference = "Stop"

$hooksDir = Join-Path (Join-Path $PSScriptRoot "..") "hooks"
$gitHooksDir = Join-Path (Join-Path (Join-Path $PSScriptRoot "..") ".git") "hooks"

# Ensure .git/hooks directory exists
if (-not (Test-Path $gitHooksDir)) {
    New-Item -ItemType Directory -Path $gitHooksDir -Force | Out-Null
}

# Install pre-commit hook
$preCommitSource = Join-Path $hooksDir "pre-commit"
$preCommitTarget = Join-Path $gitHooksDir "pre-commit"

if (Test-Path $preCommitTarget) {
    Write-Host "Pre-commit hook already exists, overwriting..." -ForegroundColor Yellow
}
Copy-Item $preCommitSource $preCommitTarget -Force
Write-Host "Pre-commit hook installed." -ForegroundColor Green

# Install pre-push hook
$prePushSource = Join-Path $hooksDir "pre-push"
$prePushTarget = Join-Path $gitHooksDir "pre-push"

if (Test-Path $prePushTarget) {
    Write-Host "Pre-push hook already exists, overwriting..." -ForegroundColor Yellow
}
Copy-Item $prePushSource $prePushTarget -Force
Write-Host "Pre-push hook installed." -ForegroundColor Green

# Install commit-msg hook
$commitMsgSource = Join-Path $hooksDir "commit-msg"
$commitMsgTarget = Join-Path $gitHooksDir "commit-msg"

if (Test-Path $commitMsgTarget) {
    Write-Host "Commit-msg hook already exists, overwriting..." -ForegroundColor Yellow
}
Copy-Item $commitMsgSource $commitMsgTarget -Force
Write-Host "Commit-msg hook installed." -ForegroundColor Green

Write-Host ""
Write-Host "Git hooks installed successfully!" -ForegroundColor Cyan
Write-Host ""
Write-Host "Quality gates will now run automatically:" -ForegroundColor Cyan
Write-Host "  Commit-msg (validates commit message format):" -ForegroundColor White
Write-Host "    Format: <type>(<scope>): <description>"
Write-Host "    Types:  feat, fix, docs, style, refactor, perf, test, build, ci, chore"
Write-Host ""
Write-Host "  Pre-commit (fast checks before each commit):" -ForegroundColor White
Write-Host "    1. cargo fmt --check    (formatting)"
Write-Host "    2. cross-target clippy  (Linux + Windows cfg lints)"
Write-Host "    3. cargo test           (tests)"
Write-Host ""
Write-Host "  Pre-push (full CI-equivalent checks before each push):" -ForegroundColor White
Write-Host "    1. cargo fmt --check    (formatting)"
Write-Host "    2. cross-target clippy  (Linux + Windows cfg lints)"
Write-Host "    3. cargo test           (tests)"
Write-Host "    4. cargo bench --no-run (benchmark compile)"
Write-Host "    5. cargo build          (debug build)"
Write-Host "    6. cargo doc            (strict rustdoc checks)"
Write-Host "    7. cargo deny check     (dependency audit)"
Write-Host ""
Write-Host "Install the supported lint targets once:" -ForegroundColor Cyan
Write-Host "  rustup target add x86_64-unknown-linux-gnu x86_64-pc-windows-msvc"
