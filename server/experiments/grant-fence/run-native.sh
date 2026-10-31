#!/usr/bin/env bash
set -euo pipefail

export FENCE_EXPERIMENT_ROOT
FENCE_EXPERIMENT_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
if [[ "${1:-}" != "--inside-native-shell" ]]; then
  command -v nix-shell >/dev/null
  exec nix-shell --pure "$FENCE_EXPERIMENT_ROOT/native.nix" \
    --keep FENCE_EXPERIMENT_ROOT \
    --run 'exec bash "$FENCE_EXPERIMENT_ROOT/run-native.sh" --inside-native-shell'
fi

cd -- "$FENCE_EXPERIMENT_ROOT"
# Formal evidence requires a clean committed source tree; the Rust test also
# checks its source digest and Git state again after services have stopped.
export FENCE_REQUIRE_PINNED=1
cargo fmt --check
cargo test --locked -- --nocapture
cargo clippy --all-targets --locked -- -D warnings
