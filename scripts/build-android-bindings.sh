#!/usr/bin/env bash
# Build the Android JNI shared libraries and uniffi-generated Kotlin
# bindings for the Waddle XMPP client.
#
# Usage:
#   ./scripts/build-android-bindings.sh [--debug]
#
# Outputs:
#   apps/android/ffi/src/main/jniLibs/arm64-v8a/libwaddle_xmpp_client_ffi.so
#   apps/android/ffi/src/main/jniLibs/armeabi-v7a/libwaddle_xmpp_client_ffi.so
#   apps/android/ffi/src/main/jniLibs/x86_64/libwaddle_xmpp_client_ffi.so
#   apps/android/ffi/src/main/kotlin/uniffi/waddle_xmpp_client/waddle_xmpp_client.kt
#
# Prerequisites (all in the `nix develop .#android` shell):
#   - rustup target add aarch64-linux-android armv7-linux-androideabi x86_64-linux-android
#   - cargo-ndk on PATH
#   - ANDROID_NDK_HOME pointing at NDK r27+
#
# Note on uniffi callback threading:
#   The generated `WaddleEventListener` Kotlin interface methods run on
#   uniffi's internal thread pool — NOT the Android main thread. The :ffi
#   Kotlin facade must do nothing in those callbacks beyond emitting to a
#   MutableSharedFlow.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SERVER="$REPO_ROOT/server"
ANDROID="$REPO_ROOT/apps/android"
JNI_LIBS="$ANDROID/ffi/src/main/jniLibs"
KOTLIN_OUT="$ANDROID/ffi/src/main/kotlin"

PROFILE="release"
CARGO_FLAG="--release"
if [[ "${1:-}" == "--debug" ]]; then
  PROFILE="debug"
  CARGO_FLAG=""
fi

ANDROID_API="${ANDROID_API_LEVEL:-36}"

if ! command -v cargo-ndk >/dev/null 2>&1; then
  echo "error: cargo-ndk not found on PATH" >&2
  echo "       run \`nix develop .#android\` or \`cargo install cargo-ndk\`" >&2
  exit 1
fi

echo "==> Building Android targets via cargo-ndk (profile: $PROFILE, API: $ANDROID_API)"
mkdir -p "$JNI_LIBS"

# cargo-ndk places the resulting .so under <output>/<abi>/lib<name>.so,
# which matches Android's jniLibs layout exactly. The CI workflow pins
# cargo-ndk 3.5.x (4.x has linker-flag regressions on modern NDK
# sysroots); 3.5 uses lowercase `-p` for the API level.
(
  cd "$SERVER"
  # shellcheck disable=SC2086 # CARGO_FLAG is intentionally word-split when empty
  cargo ndk \
    -t arm64-v8a \
    -t armeabi-v7a \
    -t x86_64 \
    -p "$ANDROID_API" \
    -o "$JNI_LIBS" \
    build -p waddle-xmpp-client-ffi $CARGO_FLAG
)

echo "==> Building host cdylib (uniffi-bindgen reads metadata from it)"
(
  cd "$SERVER"
  # shellcheck disable=SC2086
  cargo build -p waddle-xmpp-client-ffi $CARGO_FLAG
)

case "$(uname -s)" in
  Darwin) HOST_LIB_EXT="dylib" ;;
  Linux)  HOST_LIB_EXT="so" ;;
  *)      echo "error: unsupported host OS $(uname -s)" >&2; exit 1 ;;
esac

HOST_LIB="$SERVER/target/$PROFILE/libwaddle_xmpp_client_ffi.$HOST_LIB_EXT"

if [[ ! -f "$HOST_LIB" ]]; then
  echo "error: host library not found at $HOST_LIB" >&2
  exit 1
fi

echo "==> Generating Kotlin bindings"
mkdir -p "$KOTLIN_OUT"
(
  cd "$SERVER"
  cargo run -p waddle-xmpp-client-ffi \
    --bin uniffi-bindgen \
    --features waddle-xmpp-client-ffi/uniffi-bindgen-bin \
    -- generate \
    --library "$HOST_LIB" \
    --language kotlin \
    --out-dir "$KOTLIN_OUT"
)

echo "==> Done"
echo "    .so:    $JNI_LIBS/{arm64-v8a,armeabi-v7a,x86_64}/libwaddle_xmpp_client_ffi.so"
echo "    kotlin: $KOTLIN_OUT/uniffi/waddle_xmpp_client/waddle_xmpp_client.kt"
