#!/usr/bin/env bash
# Build WaddleXmppClientFFI.xcframework from the Rust sources.
#
# Usage:
#   ./scripts/build-xcframework.sh [--debug] [--platform all|ios|macos]
#
#   --platform all    iOS device, iOS Simulator and macOS slices (default)
#   --platform ios    iOS device slice only, enough to archive Waddle-iOS
#   --platform macos  universal macOS slice only, enough to archive Waddle-macOS
#
# Outputs:
#   apps/apple/Generated/WaddleXmppClientFFI.xcframework
#   apps/apple/Waddle/RustClient/Generated/{waddle_xmpp_client.swift,*.h,*.modulemap}
#
# Prerequisites:
#   rustup (the toolchain and Apple targets are installed from server/rust-toolchain.toml)

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SERVER="$REPO_ROOT/server"
APPLE="$REPO_ROOT/apps/apple"
OUT="$APPLE/Generated"
BINDINGS_DIR="$APPLE/Waddle/RustClient/Generated"
XCFW="$OUT/WaddleXmppClientFFI.xcframework"
LIB="libwaddle_xmpp_client_ffi"

# Keep Rust object deployment targets aligned with the Apple app targets.
# Xcode Cloud invokes this script before Xcode's build settings can affect
# cargo's Apple-target compilation.
export IPHONEOS_DEPLOYMENT_TARGET="${IPHONEOS_DEPLOYMENT_TARGET:-17.0}"
export MACOSX_DEPLOYMENT_TARGET="${MACOSX_DEPLOYMENT_TARGET:-14.0}"

PROFILE="release"
CARGO_FLAG="--release"
PLATFORM="all"
while [[ $# -gt 0 ]]; do
  case "$1" in
    --debug) PROFILE="debug"; CARGO_FLAG=""; shift ;;
    --platform)
      [[ $# -ge 2 ]] || { echo "--platform needs a value (all, ios or macos)" >&2; exit 64; }
      PLATFORM="$2"; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 64 ;;
  esac
done

case "$PLATFORM" in
  all) TARGETS=(aarch64-apple-darwin x86_64-apple-darwin aarch64-apple-ios aarch64-apple-ios-sim x86_64-apple-ios) ;;
  ios) TARGETS=(aarch64-apple-ios) ;;
  macos) TARGETS=(aarch64-apple-darwin x86_64-apple-darwin) ;;
  *) echo "unknown platform: $PLATFORM (expected all, ios or macos)" >&2; exit 64 ;;
esac

target_lib() {
  echo "$SERVER/target/$1/$PROFILE/$LIB.$2"
}

echo "==> Building Rust targets (profile: $PROFILE, platform: $PLATFORM)"
# Run cargo from the server workspace so rustup honours server/rust-toolchain.toml,
# and make sure the pinned toolchain has every Apple target this script builds.
cd "$SERVER"
rustup target add "${TARGETS[@]}"

for target in "${TARGETS[@]}"; do
  cargo build -p waddle-xmpp-client-ffi $CARGO_FLAG \
    --locked \
    --target "$target" \
    --manifest-path "$SERVER/Cargo.toml"
done

echo "==> Staging libraries"
rm -rf "$OUT/macos" "$OUT/ios" "$OUT/ios-sim"
XCFW_ARGS=()

if [[ "$PLATFORM" == "all" || "$PLATFORM" == "macos" ]]; then
  echo "==> Creating universal macOS library (arm64 + x86_64)"
  mkdir -p "$OUT/macos"
  lipo -create \
    "$(target_lib aarch64-apple-darwin a)" \
    "$(target_lib x86_64-apple-darwin a)" \
    -output "$OUT/macos/$LIB.a"
  XCFW_ARGS+=(-library "$OUT/macos/$LIB.a" -headers "$BINDINGS_DIR")
fi

if [[ "$PLATFORM" == "all" || "$PLATFORM" == "ios" ]]; then
  mkdir -p "$OUT/ios"
  cp "$(target_lib aarch64-apple-ios a)" "$OUT/ios/$LIB.a"
  XCFW_ARGS+=(-library "$OUT/ios/$LIB.a" -headers "$BINDINGS_DIR")
fi

if [[ "$PLATFORM" == "all" ]]; then
  echo "==> Creating universal iOS Simulator library (arm64 + x86_64)"
  mkdir -p "$OUT/ios-sim"
  lipo -create \
    "$(target_lib aarch64-apple-ios-sim a)" \
    "$(target_lib x86_64-apple-ios a)" \
    -output "$OUT/ios-sim/$LIB.a"
  XCFW_ARGS+=(-library "$OUT/ios-sim/$LIB.a" -headers "$BINDINGS_DIR")
fi

echo "==> Generating Swift bindings"
# uniffi-bindgen reads metadata from the library file without loading it, and
# the bindings are identical for every target, so the first slice built serves.
mkdir -p "$BINDINGS_DIR"
(cd "$SERVER" && cargo run -p waddle-xmpp-client-ffi \
  --locked \
  --bin uniffi-bindgen \
  --features waddle-xmpp-client-ffi/uniffi-bindgen-bin \
  -- generate \
  --library "$(target_lib "${TARGETS[0]}" dylib)" \
  --language swift \
  --out-dir "$BINDINGS_DIR")

perl -pi -e 's/[ \t]+$//' \
  "$BINDINGS_DIR/waddle_xmpp_client.swift" \
  "$BINDINGS_DIR/waddle_xmpp_clientFFI.h"

echo "==> Assembling XCFramework"
rm -rf "$XCFW"
xcodebuild -create-xcframework "${XCFW_ARGS[@]}" -output "$XCFW"

echo "==> Done: $XCFW"
