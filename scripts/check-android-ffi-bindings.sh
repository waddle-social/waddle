#!/usr/bin/env bash
# Fail when the committed Kotlin UniFFI bindings drift from the Rust FFI
# crate. Mirror of scripts/check-xmpp-client-ffi-bindings.sh (Swift).
# Host-only: needs cargo, no Android SDK/NDK.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
SERVER_ROOT="$REPO_ROOT/server"
COMMITTED="$REPO_ROOT/apps/android/core/client/src/main/kotlin/social/waddle/client/ffi/waddle_xmpp_client.kt"

case "$(uname -s)" in
  Darwin) LIB_EXT="dylib" ;;
  Linux) LIB_EXT="so" ;;
  *) echo "unsupported host for UniFFI binding drift check: $(uname -s)" >&2; exit 1 ;;
esac

TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT

cargo build \
  --manifest-path "$SERVER_ROOT/Cargo.toml" \
  --locked \
  -p waddle-xmpp-client-ffi

(cd "$SERVER_ROOT" && cargo run -p waddle-xmpp-client-ffi \
  --locked \
  --bin uniffi-bindgen \
  --features waddle-xmpp-client-ffi/uniffi-bindgen-bin \
  -- generate \
  --library "target/debug/libwaddle_xmpp_client_ffi.${LIB_EXT}" \
  --language kotlin \
  --no-format \
  --out-dir "$TMP_DIR")

GENERATED="$TMP_DIR/social/waddle/client/ffi/waddle_xmpp_client.kt"
perl -pi -e 's/[ \t]+$//' "$GENERATED"

if ! diff -u "$COMMITTED" "$GENERATED"; then
  echo "Android UniFFI binding drift detected in waddle_xmpp_client.kt." >&2
  echo "Run scripts/build-android-rust.sh and commit the regenerated bindings." >&2
  exit 1
fi

echo "Android UniFFI bindings are up to date."
