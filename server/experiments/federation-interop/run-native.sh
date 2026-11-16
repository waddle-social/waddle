#!/usr/bin/env bash
# Federation interoperability harness (issue #2047, plan slice W10).
#
#   bash server/experiments/federation-interop/run-native.sh --check
#       The workspace's static checks only: rustfmt and Clippy.
#   bash server/experiments/federation-interop/run-native.sh [harness options]
#       Build waddle-server and the harness, start the environment and
#       write the evidence log to docs/evidence/2047/. Options are passed
#       to the harness: --only <prefix>[,<prefix>...], --port-base <port>,
#       --work-dir <dir>, --keep.
#
# Everything runs inside the pinned native shell (native.nix). The harness
# binds 127.0.0.1 ports from 47100 (or --port-base) to base + 81.
set -euo pipefail

export INTEROP_EXPERIMENT_ROOT
INTEROP_EXPERIMENT_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
if [[ "${1:-}" != "--inside-native-shell" ]]; then
  command -v nix-shell >/dev/null
  quoted_args=""
  if (($#)); then
    quoted_args="$(printf '%q ' "$@")"
  fi
  export INTEROP_ARGS="$quoted_args"
  # The --run script is expanded by the shell inside nix-shell, not here.
  # shellcheck disable=SC2016
  exec nix-shell --pure "$INTEROP_EXPERIMENT_ROOT/native.nix" \
    --keep INTEROP_EXPERIMENT_ROOT --keep INTEROP_ARGS --keep INTEROP_LANDED \
    --keep CARGO_TARGET_DIR --keep CARGO_BUILD_JOBS --keep HOME \
    --run 'eval "set -- $INTEROP_ARGS"; exec bash "$INTEROP_EXPERIMENT_ROOT/run-native.sh" --inside-native-shell "$@"'
fi
shift

server_root="$(cd -- "$INTEROP_EXPERIMENT_ROOT/../.." && pwd)"
repository_root="$(cd -- "$server_root/.." && pwd)"
cd -- "$INTEROP_EXPERIMENT_ROOT"

cargo fmt --all -- --check
cargo clippy --all-targets --locked -- -D warnings
if [[ "${1:-}" == "--check" ]]; then
  exit 0
fi

# The pinned peers, as the native shell provides them.
[[ "$INTEROP_PROSODY_VERSION" == "13.0.6" ]]
[[ "$INTEROP_EJABBERD_VERSION" == "26.07" ]]
[[ "$("$INTEROP_POSTGRES_BIN/postgres" --version)" == "postgres (PostgreSQL) 17.11" ]]

cargo test --locked
cargo build --locked --bin federation-interop
harness_target="${CARGO_TARGET_DIR:-$INTEROP_EXPERIMENT_ROOT/target}"

(cd -- "$server_root" && cargo build --locked -p waddle-server --bin waddle-server)
server_target="${CARGO_TARGET_DIR:-$server_root/target}"

export INTEROP_WADDLE_BIN="$server_target/debug/waddle-server"
INTEROP_GIT_SHA="$(git -C "$repository_root" rev-parse HEAD)"
export INTEROP_GIT_SHA
exec "$harness_target/debug/federation-interop" \
  --evidence-dir "$repository_root/docs/evidence/2047" "$@"
