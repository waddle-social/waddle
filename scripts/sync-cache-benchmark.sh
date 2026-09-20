#!/usr/bin/env bash
set -euo pipefail

mode="${1:-write}"
if [[ "$#" -gt 1 || ( "$mode" != write && "$mode" != --check ) ]]; then
  echo "Usage: $0 [--check]" >&2
  exit 2
fi
script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$script_dir/.." && pwd)"
workflow="$repo_root/.github/workflows/waddle-ci-cache-benchmark.yml"
generated="$(mktemp)"
trap 'rm -f "$generated"' EXIT
cd "$repo_root"
{
  printf '%s\n' \
    '# Generated from ci/cache-benchmark/workflow.cue; do not edit manually.' \
    '# Regenerate with: bash scripts/sync-cache-benchmark.sh' \
    ''
  cue export ./ci/cache-benchmark -e workflow --out yaml
} > "$generated"
if [[ "$mode" = --check ]]; then
  diff -u "$workflow" "$generated"
else
  install -m 0644 "$generated" "$workflow"
fi
