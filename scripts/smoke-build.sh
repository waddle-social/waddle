#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
cd "$ROOT_DIR"

echo "[smoke-build] Building shared packages"
bun run build:shared

echo "[smoke-build] Building Colony apps"
bun run build:colony

echo "[smoke-build] OK"
