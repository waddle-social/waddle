#!/usr/bin/env bash
# Build WaddleXmppClientFFI.xcframework from the Rust sources.
#
# Usage:
#   ./scripts/build-xcframework.sh [--debug]
#
# Outputs:
#   apps/apple/Generated/WaddleXmppClientFFI.xcframework
#   apps/apple/Waddle/RustClient/Generated/{waddle_xmpp_client.swift,*.h,*.modulemap}
#
# Prerequisites:
#   rustup target add aarch64-apple-darwin aarch64-apple-ios aarch64-apple-ios-sim x86_64-apple-ios-sim

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SERVER="$REPO_ROOT/server"
APPLE="$REPO_ROOT/apps/apple"
OUT="$APPLE/Generated"
BINDINGS_DIR="$APPLE/Waddle/RustClient/Generated"
XCFW="$OUT/WaddleXmppClientFFI.xcframework"

PROFILE="release"
CARGO_FLAG="--release"
if [[ "${1:-}" == "--debug" ]]; then
  PROFILE="debug"
  CARGO_FLAG=""
fi

echo "==> Building Rust targets (profile: $PROFILE)"
cargo build -p waddle-xmpp-client-ffi $CARGO_FLAG \
  --target aarch64-apple-darwin \
  --manifest-path "$SERVER/Cargo.toml"

cargo build -p waddle-xmpp-client-ffi $CARGO_FLAG \
  --target aarch64-apple-ios \
  --manifest-path "$SERVER/Cargo.toml"

cargo build -p waddle-xmpp-client-ffi $CARGO_FLAG \
  --target aarch64-apple-ios-sim \
  --manifest-path "$SERVER/Cargo.toml"

cargo build -p waddle-xmpp-client-ffi $CARGO_FLAG \
  --target x86_64-apple-ios-sim \
  --manifest-path "$SERVER/Cargo.toml"

echo "==> Staging libraries"
mkdir -p "$OUT/macos" "$OUT/ios" "$OUT/ios-sim" "$OUT/ios-sim-x86"

cp "$SERVER/target/aarch64-apple-darwin/$PROFILE/libwaddle_xmpp_client_ffi.a" \
   "$OUT/macos/libwaddle_xmpp_client_ffi.a"

cp "$SERVER/target/aarch64-apple-ios/$PROFILE/libwaddle_xmpp_client_ffi.a" \
   "$OUT/ios/libwaddle_xmpp_client_ffi.a"

cp "$SERVER/target/aarch64-apple-ios-sim/$PROFILE/libwaddle_xmpp_client_ffi.a" \
   "$OUT/ios-sim/libwaddle_xmpp_client_ffi.a"

cp "$SERVER/target/x86_64-apple-ios-sim/$PROFILE/libwaddle_xmpp_client_ffi.a" \
   "$OUT/ios-sim-x86/libwaddle_xmpp_client_ffi.a"

echo "==> Generating Swift bindings"
mkdir -p "$BINDINGS_DIR"
(cd "$SERVER" && cargo run -p waddle-xmpp-client-ffi \
  --bin uniffi-bindgen \
  --features waddle-xmpp-client-ffi/uniffi-bindgen-bin \
  -- generate \
  --library "target/aarch64-apple-darwin/$PROFILE/libwaddle_xmpp_client_ffi.dylib" \
  --language swift \
  --out-dir "$BINDINGS_DIR")

echo "==> Assembling XCFramework"
rm -rf "$XCFW"
xcodebuild -create-xcframework \
  -library "$OUT/macos/libwaddle_xmpp_client_ffi.a"      -headers "$BINDINGS_DIR" \
  -library "$OUT/ios/libwaddle_xmpp_client_ffi.a"        -headers "$BINDINGS_DIR" \
  -library "$OUT/ios-sim/libwaddle_xmpp_client_ffi.a"    -headers "$BINDINGS_DIR" \
  -library "$OUT/ios-sim-x86/libwaddle_xmpp_client_ffi.a" -headers "$BINDINGS_DIR" \
  -output "$XCFW"

echo "==> Done: $XCFW"
