#!/usr/bin/env bash
set -euo pipefail

experiment="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
if [[ "${1:-}" != --inside-native-shell ]]; then
  export PARENT_MOVE_ROOT="$experiment"
  export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-/tmp/waddle-parent-move-target-$(id -u)}"
  exec nix-shell --pure "$experiment/../grant-fence/native.nix" \
    --keep PARENT_MOVE_ROOT --keep CARGO_TARGET_DIR --keep PARENT_MOVE_DEVELOPMENT \
    --run 'exec bash "$PARENT_MOVE_ROOT/run-native.sh" --inside-native-shell'
fi
cd -- "$experiment/../../.."
export LC_ALL=C
# Only committed clean source is formal evidence. Development is explicit and
# cannot produce a formal claim. No service credential enters this controller.
if [[ "${PARENT_MOVE_DEVELOPMENT:-0}" != 1 ]]; then
  test -z "$(git status --porcelain=v1 --untracked-files=all)"
fi
source_digest() {
  git ls-files -z | sort -z | xargs -0 sha256sum | sha256sum | cut -d ' ' -f 1
}
head_before="$(git rev-parse HEAD)"
digest_before="$(source_digest)"
source "$experiment/pinned-rust.sh"
go version
"$CARGO" --version
"$RUSTC" --version
"$FENCE_RUST_BIN/cargo-clippy" --version
"$FENCE_RUST_BIN/clippy-driver" --version
bash "$experiment/check-toolchain.sh"
"$FENCE_RUST_BIN/cargo-fmt" fmt --manifest-path server/Cargo.toml --all --check
# Count the tests that the next command will actually run, not all tests in
# the module: losing an #[ignore] must fail this required native lane.
inventory="$("$CARGO" test --manifest-path server/Cargo.toml --locked -p waddle-server --lib parent_move_native_tests -- --ignored --list)"
selected="$(printf '%s\n' "$inventory" | grep '^permissions::parent_move_native_tests::.*: test$' | sort)"
expected="$(printf '%s\n' \
  'permissions::parent_move_native_tests::production_parent_move_revoke_before_cleanup: test' \
  'permissions::parent_move_native_tests::production_parent_move_revoke_during_cleanup: test')"
test "$selected" = "$expected"
# --ignored is mandatory: without native prerequisites these tests fail rather
# than skip. Capture only typed assertions/test output, never service logs.
"$CARGO" test --manifest-path server/Cargo.toml --locked -p waddle-server --lib parent_move_native_tests -- --ignored --nocapture --test-threads=1
"$CARGO" test --manifest-path server/experiments/grant-fence/Cargo.toml --locked -p waddle-native-fixture --lib -- --test-threads=1
"$FENCE_RUST_BIN/cargo-clippy" clippy --manifest-path server/Cargo.toml --locked -p waddle-server --lib --tests -- -D warnings
test "$head_before" = "$(git rev-parse HEAD)"
test "$digest_before" = "$(source_digest)"
if [[ "${PARENT_MOVE_DEVELOPMENT:-0}" != 1 ]]; then
  test -z "$(git status --porcelain=v1 --untracked-files=all)"
fi
printf 'parent_move_native complete head=%s source_sha256=%s formal=%s\n' "$head_before" "$digest_before" "$([[ "${PARENT_MOVE_DEVELOPMENT:-0}" == 1 ]] && echo false || echo true)"
