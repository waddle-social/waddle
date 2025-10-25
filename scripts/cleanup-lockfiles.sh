#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
cd "$ROOT_DIR"

FOUND=false
for f in package-lock.json yarn.lock pnpm-lock.yaml bun.lockb; do
  if [ -f "$f" ]; then
    echo "[cleanup] Found $f"
    FOUND=true
  fi
done

if [ "$FOUND" = true ]; then
  echo "[cleanup] Removing legacy lockfiles (keeps bun.lock)"
  rm -f package-lock.json yarn.lock pnpm-lock.yaml bun.lockb || true
  echo "[cleanup] Done."
else
  echo "[cleanup] No legacy lockfiles found."
fi

