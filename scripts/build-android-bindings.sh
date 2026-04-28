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
  # The workspace release profile has `strip = true`, which removes the
  # `UNIFFI_META_*` symbols uniffi-bindgen's `--library` mode scans for —
  # leaving bindgen to silently produce zero output. Override strip just
  # for this host build; it is only consumed by bindgen and not shipped.
  # No-op for debug builds (no strip there). The Android cargo-ndk
  # targets above stay stripped because metadata isn't read from them.
  # shellcheck disable=SC2086
  cargo build -p waddle-xmpp-client-ffi $CARGO_FLAG \
    --config 'profile.release.strip = false'
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
  # Build the bindgen binary in the same profile as the cdylib; on Linux a
  # debug-profile bindgen has been observed to exit silently without emitting
  # any output (whereas the release-profile binary works). Keeping the two
  # profiles aligned avoids any uniffi metadata/version mismatch.
  set -x
  # shellcheck disable=SC2086
  cargo run -p waddle-xmpp-client-ffi \
    --bin uniffi-bindgen \
    --features waddle-xmpp-client-ffi/uniffi-bindgen-bin \
    $CARGO_FLAG \
    -- generate \
    --library "$HOST_LIB" \
    --language kotlin \
    --out-dir "$KOTLIN_OUT" 2>&1
  set +x
)

echo "==> Verifying bindgen output"
echo "  KOTLIN_OUT=$KOTLIN_OUT"
ls -la "$KOTLIN_OUT" || true
find "$KOTLIN_OUT" -type f -name '*.kt' 2>/dev/null || true

if [[ ! -d "$KOTLIN_OUT/uniffi/waddle_xmpp_client" ]]; then
  echo "error: bindgen ran but did not produce $KOTLIN_OUT/uniffi/waddle_xmpp_client/" >&2
  echo "  searching the workspace for stray waddle_xmpp_client.kt files:" >&2
  find "$REPO_ROOT" -name 'waddle_xmpp_client.kt' -not -path '*/node_modules/*' -not -path '*/.git/*' 2>/dev/null >&2 || true
  exit 1
fi

echo "==> Done"
echo "    .so:    $JNI_LIBS/{arm64-v8a,armeabi-v7a,x86_64}/libwaddle_xmpp_client_ffi.so"
echo "    kotlin: $KOTLIN_OUT/uniffi/waddle_xmpp_client/waddle_xmpp_client.kt"
