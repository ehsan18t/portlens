#!/usr/bin/env bash
set -euo pipefail

input_path="${1:-bench_output.txt}"

if [[ ! -f "$input_path" ]]; then
  echo "benchmark budget input not found: $input_path" >&2
  exit 1
fi

python3 - "$input_path" <<'PY'
import pathlib
import re
import sys


def unit_factor(unit: str) -> int:
    normalized = unit.strip().rstrip("]")
    if normalized == "ns":
        return 1
    if normalized in {"us", "µs", "μs", "Âµs", "Î¼s", "┬Ás"}:
        return 1000
    if normalized == "ms":
        return 1_000_000
    if normalized == "s":
        return 1_000_000_000
    raise ValueError(f"unsupported benchmark unit: {unit}")


budgets = {
    "filter_show_all_500": 25_000,
    "filter_tcp_only_500": 35_000,
    "filter_relevance_500": 35_000,
    "filter_port_500": 45_000,
    "filter_combined_500": 35_000,
    "filter_process_500": 75_000,
    "filter_grep_broad_500": 35_000,
    "filter_grep_narrow_500": 75_000,
    "filter_grep_tcp_500": 75_000,
    "filter_scale/show_all/4096": 70_000,
    "filter_scale/tcp_only/4096": 140_000,
    "filter_scale/port_mid/4096": 160_000,
    "filter_hit_rates/process_exact/4096": 260_000,
    "filter_hit_rates/grep_all_hits/4096": 160_000,
    "filter_hit_rates/grep_sparse_hits/4096": 260_000,
    "filter_hit_rates/grep_no_hits/4096": 250_000,
    "filter_hit_rates/grep_sparse_hits_tcp_only/4096": 280_000,
    "docker_parse_4_containers": 15_000,
}

inline_pattern = re.compile(
    r'^([^ ]+)\s+time:\s+\[([0-9.]+)\s+([^ ]+)\s+([0-9.]+)\s+([^ ]+)\s+([0-9.]+)\s+([^\]]+)\]'
)
pending_pattern = re.compile(
    r'^\s+time:\s+\[([0-9.]+)\s+([^ ]+)\s+([0-9.]+)\s+([^ ]+)\s+([0-9.]+)\s+([^\]]+)\]'
)
name_pattern = re.compile(r'^[A-Za-z0-9_/.-]+$')

pending_name = None
seen = set()
failed = False

for raw_line in pathlib.Path(sys.argv[1]).read_text(encoding="utf-8", errors="replace").splitlines():
    if raw_line.startswith("Benchmarking "):
        continue

    if name_pattern.fullmatch(raw_line):
        pending_name = raw_line
        continue

    match = inline_pattern.match(raw_line)
    if match:
        name = match.group(1)
        high_value = float(match.group(6))
        high_unit = match.group(7)
        pending_name = None
    else:
        pending_match = pending_pattern.match(raw_line)
        if not pending_match or pending_name is None:
            continue
        name = pending_name
        high_value = float(pending_match.group(5))
        high_unit = pending_match.group(6)
        pending_name = None

    if name not in budgets:
        continue

    high_ns = round(high_value * unit_factor(high_unit))
    seen.add(name)

    if high_ns > budgets[name]:
        print(
            f"budget exceeded: {name} high={high_ns}ns budget={budgets[name]}ns",
            file=sys.stderr,
        )
        failed = True
    else:
        print(f"budget ok: {name} high={high_ns}ns budget={budgets[name]}ns")

missing = sorted(set(budgets) - seen)
for name in missing:
    print(f"missing budget benchmark: {name}", file=sys.stderr)
    failed = True

if failed:
    raise SystemExit(1)
PY