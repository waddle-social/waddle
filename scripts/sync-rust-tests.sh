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

# The workflow is evaluated through server/env.cue, which cue 0.17.x cannot
# evaluate (#1763; see the cue hold in flake.nix). Refuse any other version
# rather than failing with an unrelated-looking evaluation error.
required_cue=v0.16.1
# A missing cue must reach the message below, not abort under pipefail.
cue_language="$(cue version 2>/dev/null | sed -n 's/^CUE language version //p' | head -n 1 || true)"
if [[ "$cue_language" != "$required_cue" ]]; then
  echo "scripts/sync-rust-tests.sh needs cue $required_cue (got: ${cue_language:-no cue on PATH or unreadable version})." >&2
  echo "Run it from the repository dev shell: nix develop -c bash scripts/sync-rust-tests.sh${1:+ $1}" >&2
  exit 2
fi

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
