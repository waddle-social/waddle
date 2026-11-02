#!/usr/bin/env bash
set -euo pipefail

# Compile once, then use the same immutable archives in four local Nix
# sandboxes. Each shard pins itself to eight distinct CPUs before starting
# PostgreSQL or nextest. No archive crosses a runner or trust boundary.
diagnostics=.ci/nextest-archive
mkdir -p "$diagnostics"
# A failed rerun must not leave a prior passing sidecar looking current.
rm -f "$diagnostics/qualification-evidence.json"
# Reject a dirty candidate before constructing the committed source snapshot.
# The evidence checker repeats this check before writing the sidecar.
if ! git diff --quiet HEAD --; then
  echo "candidate worktree is dirty before nextest build" >&2
  exit 1
fi
untracked="$(git ls-files --others --exclude-standard -z -- . ':(exclude).ci/nextest-archive/**')"
if [[ -n "$untracked" ]]; then
  echo "candidate worktree has untracked files before nextest build" >&2
  exit 1
fi
candidate_sha="$(git rev-parse --verify 'HEAD^{commit}')"
snapshot_directory="$(mktemp -d)"
trap 'rm -rf "$snapshot_directory"' EXIT
git -C "$(git rev-parse --show-toplevel)" archive --format=tar "$candidate_sha" \
  | tar -xf - -C "$snapshot_directory"
# Verify the materialized store path, not the writable extraction directory.
# This also fails closed on gitlinks and archive export transformations.
source_path="$(nix store add-path --name waddle-candidate-source "$snapshot_directory")"
python3 "$(dirname "${BASH_SOURCE[0]}")/verify_candidate_snapshot.py" "$candidate_sha" "$source_path"
flake="path:$source_path"
isolation=(--option builders '' --option sandbox true --option sandbox-fallback false)
nix build --print-build-logs --no-link "${isolation[@]}" "$flake#waddle-ci-sandbox-probe"
archive="$(cd "$source_path/server" && bash scripts/build-nextest-archive.sh "$flake#waddle-server-test-archive")"
cp "$archive/ci-performance/cargo-timing.html" "$diagnostics/cargo-timing.html"
cp "$archive/partition-coverage.json" "$diagnostics/partition-coverage.json"
cp "$archive/plan.json" "$diagnostics/plan.json"

shards=()
for partition in 1 2 3 4; do
  shards+=("$flake#waddle-server-test-shard-$partition")
done

# Requiring real sandboxes preserves private network/IPC namespaces, not
# just distinct database directories. --keep-going runs the other selected
# shards after a failure; nix still exits nonzero if any shard fails.
nix build --print-build-logs --no-link --json --keep-going \
  --max-jobs 4 --cores 8 "${isolation[@]}" \
  "${shards[@]}" > "$diagnostics/shard-results.json"

for partition in 1 2 3 4; do
  output="$(nix eval --raw "$flake#waddle-server-test-shard-$partition.outPath")"
  mkdir -p "$diagnostics/shard-$partition"
  for report in whole-inventory.json shared-inventory.json whole-coverage.json shared-coverage.json \
    junit-whole.xml junit-shared.xml; do
    cp "$output/$report" "$diagnostics/shard-$partition/$report"
  done
done

# A passing exit code does not show which tests ran: a filter or feature gate
# can drop a test, and an early return reports as a pass. Emit every executed
# test name and fail unless each required test ran and passed in some shard.
junit_reports=()
for partition in 1 2 3 4; do
  junit_reports+=("$diagnostics/shard-$partition/junit-whole.xml" "$diagnostics/shard-$partition/junit-shared.xml")
done
python3 "$source_path/server/scripts/check_required_tests.py" \
  --required .config/required-cluster-tests.txt \
  --executed-out "$diagnostics/executed-tests.txt" \
  --evidence-out "$diagnostics/qualification-evidence.json" \
  --candidate-sha "$candidate_sha" \
  "${junit_reports[@]}"
