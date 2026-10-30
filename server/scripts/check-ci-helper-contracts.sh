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
  test_check_libp2p_rollout \
  test_check_postgres_test_group; do
  python3 "scripts/${contract}.py"
done

# Every Postgres-backed test must run in nextest's serialized `postgres`
# group unless it is listed as schema-isolated.
python3 scripts/check_postgres_test_group.py
