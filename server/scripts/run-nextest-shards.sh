#!/usr/bin/env bash
set -euo pipefail

# Compile once, then use the same immutable archives in four local Nix
# sandboxes. Each shard pins itself to eight distinct CPUs before starting
# PostgreSQL or nextest. No archive crosses a runner or trust boundary.
isolation=(--option builders '' --option sandbox true --option sandbox-fallback false)
nix build --print-build-logs --no-link "${isolation[@]}" ../#waddle-ci-sandbox-probe
archive="$(bash scripts/build-nextest-archive.sh)"
diagnostics=.ci/nextest-archive
mkdir -p "$diagnostics"
cp "$archive/ci-performance/cargo-timing.html" "$diagnostics/cargo-timing.html"
cp "$archive/partition-coverage.json" "$diagnostics/partition-coverage.json"
cp "$archive/plan.json" "$diagnostics/plan.json"

shards=()
for partition in 1 2 3 4; do
  shards+=("../#waddle-server-test-shard-$partition")
done

# Requiring real sandboxes preserves private network/IPC namespaces, not
# just distinct database directories. --keep-going runs the other selected
# shards after a failure; nix still exits nonzero if any shard fails.
nix build --print-build-logs --no-link --json --keep-going \
  --max-jobs 4 --cores 8 "${isolation[@]}" \
  "${shards[@]}" > "$diagnostics/shard-results.json"

for partition in 1 2 3 4; do
  output="$(nix eval --raw "../#waddle-server-test-shard-$partition.outPath")"
  mkdir -p "$diagnostics/shard-$partition"
  for report in whole-inventory.json shared-inventory.json whole-coverage.json shared-coverage.json; do
    cp "$output/$report" "$diagnostics/shard-$partition/$report"
  done
done
