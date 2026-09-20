#!/usr/bin/env bash
set -euo pipefail

mode="${1:-write}"
if [[ "$#" -gt 1 || ( "$mode" != write && "$mode" != --check ) ]]; then
  echo "Usage: $0 [--check]" >&2
  exit 2
fi

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$script_dir/.." && pwd)"
workflow="$repo_root/.github/workflows/waddle-server-rusttests.yml"
generated="$(mktemp)"
trap 'rm -f "$generated"' EXIT

cd "$repo_root"
{
  printf '%s\n' \
    '# Generated from ci/rust-tests/workflow.cue; do not edit manually.' \
    '# Regenerate with: bash scripts/sync-rust-tests.sh' \
    ''
  cue export ./ci/rust-tests -e workflow --out yaml
} > "$generated"

if [[ "$mode" = --check ]]; then
  if ! diff -u "$workflow" "$generated"; then
    echo "Rust workflow drift detected. Run: bash scripts/sync-rust-tests.sh" >&2
    exit 1
  fi
else
  install -m 0644 "$generated" "$workflow"
fi
