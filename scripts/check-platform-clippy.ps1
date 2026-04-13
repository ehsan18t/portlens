#!/usr/bin/env pwsh
# PortLens - Cross-target Clippy gate
# Validates the supported Linux and Windows target triples so cfg-specific
# lint failures are caught locally before CI.

$ErrorActionPreference = "Stop"

$targets = @(
    "x86_64-unknown-linux-gnu",
    "x86_64-pc-windows-msvc"
)

function Assert-Command {
    param([string]$Name)

    if (Get-Command $Name -ErrorAction SilentlyContinue) {
        return
    }

    Write-Host ""
    Write-Host "X REQUIRED COMMAND NOT FOUND"
    Write-Host "  '$Name' is required to run the cross-target Clippy gate."
    exit 1
}

Assert-Command cargo
Assert-Command rustc
Assert-Command rustup

$installedTargets = @(rustup target list --installed | ForEach-Object { $_.Trim() })
if ($LASTEXITCODE -ne 0) {
    exit $LASTEXITCODE
}

$hostTargetLine = rustc -vV | Where-Object { $_ -like 'host:*' } | Select-Object -First 1
if ($LASTEXITCODE -ne 0) {
    exit $LASTEXITCODE
}

$hostTarget = $hostTargetLine -replace '^host:\s*', ''

$missingTargets = @($targets | Where-Object { $_ -notin $installedTargets })
if ($missingTargets.Count -gt 0) {
    Write-Host ""
    Write-Host "X MISSING RUST TARGETS"
    Write-Host "  Install the supported lint targets first:"
    Write-Host "    rustup target add x86_64-unknown-linux-gnu x86_64-pc-windows-msvc"
    Write-Host ""
    Write-Host "  Missing targets:"
    foreach ($target in $missingTargets) {
        Write-Host "    - $target"
    }
    exit 1
}

if ($targets -contains $hostTarget) {
    Write-Host "Detected supported host target: $hostTarget"
} else {
    Write-Host "Host target '$hostTarget' is not one of the supported release targets."
    Write-Host "Running lib+bins Clippy for both supported targets."
}

foreach ($target in $targets) {
    if ($target -eq $hostTarget) {
        Write-Host "-> Running native clippy for $target (all-targets)..."
        cargo clippy --locked --all-targets --target $target -- -D warnings
    } else {
        Write-Host "-> Running cross-target clippy for $target (lib + bins)..."
        cargo clippy --locked --lib --bins --target $target -- -D warnings
    }

    if ($LASTEXITCODE -ne 0) {
        exit $LASTEXITCODE
    }

    Write-Host "  OK $target"
}