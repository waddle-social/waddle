#!/usr/bin/env bash
# Contract tests for the Postgres shard lane's CI helpers and the source
# structure checks that run in the clippy lanes. The PR rustTests workflow runs
# this before compiling; on main the publish-gating `ciHelperContracts` task
# runs it, so a helper regression cannot ship an image.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."
# Tests that run scripts in temporary directories must not leave bytecode
# caches in the checkout.
export PYTHONDONTWRITEBYTECODE=1

python3 ../tests/ci-timings-test.py
for contract in \
  test_check_nextest_shards \
  test_nextest_archive_transfer \
  test_build_nextest_archive \
  test_plan_nextest_binary_shards \
  test_pin_nextest_shard \
  test_run_nextest_shards \
  test_check_required_tests \
  test_check_test_postgres_env \
  test_check_rust_structure \
  test_check_libp2p_rollout; do
  python3 "scripts/${contract}.py"
done

# The PR rustTests workflow runs this helper outside the dev shell. Use the
# pinned CUE from the flake when the ambient CLI is absent or a different
# version; evaluating this graph with CUE 0.17 can hang (#1763).
if command -v cue >/dev/null 2>&1 && [[ "$(cue version 2>/dev/null)" == *$'\nCUE language version v0.16.1\n'* ]]; then
  python3 scripts/test_default_publish_graph.py
else
  (cd .. && nix develop --no-write-lock-file --command python3 server/scripts/test_default_publish_graph.py)
fi
