#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
cd "$ROOT_DIR"

echo "[verify-install] Checking for lockfile changes..."

BASE_STATUS=$(git status --porcelain bun.lock bun.lockb 2>/dev/null || true)

bun install --silent

POST_STATUS=$(git status --porcelain bun.lock bun.lockb 2>/dev/null || true)

if [ -z "$BASE_STATUS" ] && [ -z "$POST_STATUS" ]; then
  echo "[verify-install] OK: No lockfile changes"
  exit 0
fi

if [ "$BASE_STATUS" = "$POST_STATUS" ]; then
  echo "[verify-install] OK: Lockfile unchanged"
  exit 0
fi

echo "[verify-install] FAIL: Lockfile changed after install"
git --no-pager diff -- bun.lock bun.lockb || true
exit 1

