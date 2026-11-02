#!/usr/bin/env bash
set -euo pipefail

# Compile once, then use the same immutable archives in four local Nix
# sandboxes. Each shard pins itself to eight distinct CPUs before starting
# PostgreSQL or nextest. No archive crosses a runner or trust boundary.
diagnostics=.ci/nextest-archive
mkdir -p "$diagnostics"
# A failed rerun must not leave a prior passing sidecar looking current.
rm -f "$diagnostics/qualification-evidence.json"
# Reject a dirty source tree before Nix reads it. The evidence checker repeats
# this check before writing the sidecar, catching persistent changes during the
# build. A mutation restored before that second check cannot be detected here;
# fully closing that race requires building from an immutable source snapshot.
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
isolation=(--option builders '' --option sandbox true --option sandbox-fallback false)
nix build --print-build-logs --no-link "${isolation[@]}" ../#waddle-ci-sandbox-probe
archive="$(bash scripts/build-nextest-archive.sh)"
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
python3 "$(dirname "${BASH_SOURCE[0]}")/check_required_tests.py" \
  --required .config/required-cluster-tests.txt \
  --executed-out "$diagnostics/executed-tests.txt" \
  --evidence-out "$diagnostics/qualification-evidence.json" \
  --candidate-sha "$candidate_sha" \
  "${junit_reports[@]}"
