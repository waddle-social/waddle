#!/usr/bin/env bash
# Regenerate both committed UniFFI language bindings from one host build.
#
# This intentionally does not cross-compile Android libraries or build the
# five Apple targets/XCFramework. It is the low-disk source-generation path
# after changing the Rust FFI surface.
#
# Outputs:
#   apps/android/core/client/src/main/kotlin/social/waddle/client/ffi/waddle_xmpp_client.kt
#   apps/apple/Waddle/RustClient/Generated/waddle_xmpp_client.swift
#   apps/apple/Waddle/RustClient/Generated/waddle_xmpp_clientFFI.h
#   apps/apple/Waddle/RustClient/Generated/waddle_xmpp_clientFFI.modulemap

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SERVER_ROOT="$REPO_ROOT/server"
CARGO_TARGET_ROOT="${CARGO_TARGET_DIR:-$SERVER_ROOT/target}"
if [[ "$CARGO_TARGET_ROOT" != /* ]]; then
  CARGO_TARGET_ROOT="$REPO_ROOT/$CARGO_TARGET_ROOT"
fi
export CARGO_TARGET_DIR="$CARGO_TARGET_ROOT"
TMP_ROOT="${TMPDIR:-/tmp}"
if [[ "$TMP_ROOT" != /* ]]; then
  TMP_ROOT="$REPO_ROOT/$TMP_ROOT"
fi
mkdir -p "$TMP_ROOT"
export TMPDIR="$TMP_ROOT"

ANDROID_BINDINGS_DIR="$REPO_ROOT/apps/android/core/client/src/main/kotlin"
ANDROID_BINDINGS_FILE="$ANDROID_BINDINGS_DIR/social/waddle/client/ffi/waddle_xmpp_client.kt"
APPLE_BINDINGS_DIR="$REPO_ROOT/apps/apple/Waddle/RustClient/Generated"
APPLE_SWIFT_FILE="$APPLE_BINDINGS_DIR/waddle_xmpp_client.swift"
APPLE_HEADER_FILE="$APPLE_BINDINGS_DIR/waddle_xmpp_clientFFI.h"

case "$(uname -s)" in
  Darwin) LIB_EXT="dylib" ;;
  Linux) LIB_EXT="so" ;;
  *) echo "unsupported host for UniFFI binding generation: $(uname -s)" >&2; exit 1 ;;
esac

mkdir -p "$ANDROID_BINDINGS_DIR" "$APPLE_BINDINGS_DIR"

cargo build \
  --manifest-path "$SERVER_ROOT/Cargo.toml" \
  --locked \
  -p waddle-xmpp-client-ffi

HOST_LIBRARY="$CARGO_TARGET_ROOT/debug/libwaddle_xmpp_client_ffi.${LIB_EXT}"

(cd "$SERVER_ROOT" && cargo run -p waddle-xmpp-client-ffi \
  --locked \
  --bin uniffi-bindgen \
  --features waddle-xmpp-client-ffi/uniffi-bindgen-bin \
  -- generate \
  --library "$HOST_LIBRARY" \
  --language kotlin \
  --no-format \
  --out-dir "$ANDROID_BINDINGS_DIR")

(cd "$SERVER_ROOT" && cargo run -p waddle-xmpp-client-ffi \
  --locked \
  --bin uniffi-bindgen \
  --features waddle-xmpp-client-ffi/uniffi-bindgen-bin \
  -- generate \
  --library "$HOST_LIBRARY" \
  --language swift \
  --out-dir "$APPLE_BINDINGS_DIR")

perl -pi -e 's/[ \t]+$//' \
  "$ANDROID_BINDINGS_FILE" \
  "$APPLE_SWIFT_FILE" \
  "$APPLE_HEADER_FILE"

echo "Regenerated Android and Apple UniFFI bindings."
