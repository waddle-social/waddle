#!/usr/bin/env bash
set -euo pipefail

experiment="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
source "$experiment/pinned-rust.sh"
probe="$(mktemp -d "${TMPDIR:-/tmp}/waddle-parent-move-toolchain.XXXXXX")"
trap 'rm -rf -- "$probe"' EXIT
mkdir -p "$probe/home/bin" "$probe/crate/src"
cat > "$probe/crate/Cargo.toml" <<'EOF'
[package]
name = "native-toolchain-probe"
version = "0.0.0"
edition = "2024"
[workspace]
EOF
printf 'pub fn identity(value: u8) -> u8 {\n    value\n}\n' > "$probe/crate/src/lib.rs"
cat > "$probe/home/bin/cargo-clippy" <<'EOF'
#!/bin/sh
printf 'host cargo-clippy selected\n'
exit 73
EOF
cat > "$probe/home/bin/cargo-fmt" <<'EOF'
#!/bin/sh
printf 'host cargo-fmt selected\n'
exit 74
EOF
cat > "$probe/host-wrapper" <<'EOF'
#!/bin/sh
printf 'host compiler wrapper selected\n' >&2
exit 75
EOF
chmod +x "$probe/home/bin/cargo-clippy" "$probe/home/bin/cargo-fmt" "$probe/host-wrapper"
printf '[build]\nrustc-wrapper="%s/host-wrapper"\nrustc-workspace-wrapper="%s/host-wrapper"\n' \
  "$probe" "$probe" > "$probe/home/config.toml"

(
  export CARGO_HOME="$probe/home"
  export CARGO_TARGET_DIR="$probe/target"
  # Negative controls establish that the hostile configuration is effective.
  status=0
  "$CARGO" clippy --version > "$probe/shadow-clippy.log" 2>&1 || status=$?
  test "$status" = 73
  status=0
  "$CARGO" fmt --version > "$probe/shadow-fmt.log" 2>&1 || status=$?
  test "$status" = 74
  status=0
  (unset RUSTC_WRAPPER RUSTC_WORKSPACE_WRAPPER
    "$CARGO" check --offline --manifest-path "$probe/crate/Cargo.toml"
  ) > "$probe/shadow-wrapper.log" 2>&1 || status=$?
  test "$status" != 0
  grep -q 'host compiler wrapper selected' "$probe/shadow-wrapper.log"

  # Exercise the same binding used by the real proof, not a test substitute.
  source "$experiment/pinned-rust.sh"
  "$CARGO" generate-lockfile --offline --manifest-path "$probe/crate/Cargo.toml"
  "$CARGO" check --locked --offline --manifest-path "$probe/crate/Cargo.toml"
  "$FENCE_RUST_BIN/cargo-fmt" fmt --manifest-path "$probe/crate/Cargo.toml" --all --check
  "$FENCE_RUST_BIN/cargo-clippy" clippy --locked --offline --verbose \
    --manifest-path "$probe/crate/Cargo.toml" -- -D warnings > "$probe/clippy.log" 2>&1
  grep -Fq "$FENCE_RUST_BIN/clippy-driver" "$probe/clippy.log"
  grep -Fq "$RUSTC" "$probe/clippy.log"

  # Disabling host wrappers must not accidentally disable Clippy itself.
  printf 'pub fn len(value: &Vec<u8>) -> usize { value.len() }\n' > "$probe/crate/src/lib.rs"
  status=0
  "$FENCE_RUST_BIN/cargo-clippy" clippy --locked --offline \
    --manifest-path "$probe/crate/Cargo.toml" -- -D warnings > "$probe/expected-lint.log" 2>&1 || status=$?
  test "$status" != 0
  grep -q 'clippy::ptr_arg' "$probe/expected-lint.log"
)
printf 'pinned Rust host-shadow/wrapper controls passed; intentional Clippy lint rejected\n'
