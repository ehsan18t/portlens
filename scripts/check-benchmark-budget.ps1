param(
    [string]$InputPath = "bench_output.txt"
)

if (-not (Test-Path -LiteralPath $InputPath)) {
    Write-Error "benchmark budget input not found: $InputPath"
    exit 1
}

$budgetNs = @{
    "filter_show_all_500" = 25000
    "filter_tcp_only_500" = 35000
    "filter_relevance_500" = 35000
    "filter_port_500" = 45000
    "filter_combined_500" = 35000
    "filter_process_500" = 75000
    "filter_grep_broad_500" = 35000
    "filter_grep_narrow_500" = 75000
    "filter_grep_tcp_500" = 75000
    "filter_scale/show_all/4096" = 70000
    "filter_scale/tcp_only/4096" = 140000
    "filter_scale/port_mid/4096" = 160000
    "filter_hit_rates/process_exact/4096" = 260000
    "filter_hit_rates/grep_all_hits/4096" = 160000
    "filter_hit_rates/grep_sparse_hits/4096" = 260000
    "filter_hit_rates/grep_no_hits/4096" = 250000
    "filter_hit_rates/grep_sparse_hits_tcp_only/4096" = 280000
    "docker_parse_4_containers" = 15000
}

function Convert-ToNanoseconds {
    param(
        [double]$Value,
        [string]$Unit
    )

    switch ($Unit.Trim()) {
        "ns" { return [double]$Value }
        "us" { return [double]$Value * 1000 }
        "µs" { return [double]$Value * 1000 }
        "μs" { return [double]$Value * 1000 }
        "Âµs" { return [double]$Value * 1000 }
        "Î¼s" { return [double]$Value * 1000 }
        "┬Ás" { return [double]$Value * 1000 }
        "ms" { return [double]$Value * 1000000 }
        "s" { return [double]$Value * 1000000000 }
        default { throw "unsupported benchmark unit: $Unit" }
    }
}

$seen = @{}
$pendingName = $null
$failed = $false

foreach ($line in Get-Content -LiteralPath $InputPath) {
    if ($line -match '^Benchmarking ') {
        continue
    }

    if ($line -match '^[A-Za-z0-9_/.-]+$') {
        $pendingName = $line
        continue
    }

    if ($line -match '^([^ ]+)\s+time:\s+\[([0-9.]+)\s+([^ ]+)\s+([0-9.]+)\s+([^ ]+)\s+([0-9.]+)\s+([^\]]+)\]') {
        $name = $Matches[1]
        $highNs = Convert-ToNanoseconds -Value ([double]$Matches[6]) -Unit $Matches[7]
        $pendingName = $null
    }
    elseif ($pendingName -and $line -match '^\s+time:\s+\[([0-9.]+)\s+([^ ]+)\s+([0-9.]+)\s+([^ ]+)\s+([0-9.]+)\s+([^\]]+)\]') {
        $name = $pendingName
        $highNs = Convert-ToNanoseconds -Value ([double]$Matches[5]) -Unit $Matches[6]
        $pendingName = $null
    }
    else {
        continue
    }

    if (-not $budgetNs.ContainsKey($name)) {
        continue
    }

    $seen[$name] = $true
    if ($highNs -gt $budgetNs[$name]) {
        Write-Error "budget exceeded: $name high=$([math]::Round($highNs))ns budget=$($budgetNs[$name])ns"
        $failed = $true
    }
    else {
        Write-Host "budget ok: $name high=$([math]::Round($highNs))ns budget=$($budgetNs[$name])ns"
    }
}

foreach ($name in $budgetNs.Keys) {
    if (-not $seen.ContainsKey($name)) {
        Write-Error "missing budget benchmark: $name"
        $failed = $true
    }
}

if ($failed) {
    exit 1
}