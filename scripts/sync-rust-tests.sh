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
queue_paths="$repo_root/ci/rust-tests/queue-paths.json"
generated="$(mktemp)"
generated_paths="$(mktemp)"
trap 'rm -f "$generated" "$generated_paths"' EXIT

# The workflow is evaluated through server/env.cue, which imports cuenv 0.81.1's
# v0.18.0 schema (see the cue pin in flake.nix). Refuse any other language
# version rather than failing with an unrelated-looking evaluation error.
required_cue=v0.18.0
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

cue export ./ci/rust-tests -e queueTasks | jq '
  def inputs($prefix):
    if (.inputs | length) == 0 then "**"
    else .inputs[] |
      if type == "string" then $prefix + .
      elif .cuenvFiles == "module" then .paths[]
      else error("unsupported queue task input: " + tostring) end
    end;
  with_entries(.key as $name | .value = ([
    "*.cue", "server/*.cue", "ci/*.cue", "ci/contributors/*.cue", "cue.mod/**",
    "cuenv.lock", (".github/workflows/" + $name + ".yml"),
    (.value[] | .prefix as $prefix |
      (.extraInputs[]? | $prefix + .),
      (.tasks[] | recurse(.dependsOn[]?) | inputs($prefix)))
  ] | unique))
' > "$generated_paths"

if [[ "$mode" = --check ]]; then
  if ! diff -u "$workflow" "$generated" || ! diff -u "$queue_paths" "$generated_paths"; then
    echo "Rust workflow drift detected. Run: bash scripts/sync-rust-tests.sh" >&2
    exit 1
  fi
else
  install -m 0644 "$generated" "$workflow"
  install -m 0644 "$generated_paths" "$queue_paths"
fi
