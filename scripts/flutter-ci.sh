#!/usr/bin/env bash
# Independent CI entrypoints for the alternative Flutter client.
set -euo pipefail
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MODE="${1:-}"
case "$MODE" in
  rust)
    python3 "$REPO_ROOT/server/crates/waddle-flutter-runtime/scripts/generate_dispatch.py" --check
    cd "$REPO_ROOT/server"
    cargo fmt --check -p waddle-flutter-runtime -p waddle-flutter-timeline -p waddle-client-session -p waddle-xmpp-client -p waddle-xmpp-client-ffi -p waddle-xmpp-client-wasm
    cargo clippy --locked -p waddle-flutter-runtime -p waddle-flutter-timeline -p waddle-xmpp-client -p waddle-xmpp-client-ffi --features waddle-xmpp-client-ffi/serde --all-targets -- -D warnings
    cargo test --locked -p waddle-flutter-runtime -p waddle-flutter-timeline -p waddle-client-session -p waddle-xmpp-client -p waddle-xmpp-client-ffi --features waddle-xmpp-client-ffi/serde --all-targets
    cargo test --locked -p waddle-xmpp-client-wasm --lib
    cargo clippy --locked -p waddle-xmpp-client-wasm --target wasm32-unknown-unknown -- -D warnings
    ;;
  analyze)
    cd "$REPO_ROOT/apps/flutter"
    dart format --output=none --set-exit-if-changed lib test integration_test
    flutter analyze --fatal-infos
    ;;
  test)
    cd "$REPO_ROOT/apps/flutter"
    FLUTTER_PREVIEW_DIR="${WADDLE_FLUTTER_PREVIEW_DIR:-$REPO_ROOT/apps/flutter/build/design-previews}"
    export WADDLE_FLUTTER_RUNTIME_LIBRARY="${WADDLE_FLUTTER_RUNTIME_LIBRARY:-$REPO_ROOT/server/target/debug/libwaddle_flutter_runtime.so}"
    python3 "$REPO_ROOT/server/crates/waddle-flutter-runtime/scripts/smoke_library.py" "$WADDLE_FLUTTER_RUNTIME_LIBRARY"
    flutter test --reporter expanded --dart-define="WADDLE_PREVIEW_DIR=$FLUTTER_PREVIEW_DIR"
    ;;
  apk)
    cd "$REPO_ROOT/apps/flutter"
    rm -f build/artifacts/waddle-flutter.apk
    # Timestamp minutes let reviewers upgrade this app's earlier PR APKs.
    FLUTTER_COMMIT_TIME="$(git show -s --format=%ct HEAD)"
    FLUTTER_VERSION_CODE=$((FLUTTER_COMMIT_TIME / 60))
    FLUTTER_VERSION_NAME="pr-$(git rev-parse --short=12 HEAD)"
    flutter build apk --debug --no-pub --target-platform android-arm64,android-x64 \
      --build-number "$FLUTTER_VERSION_CODE" --build-name "$FLUTTER_VERSION_NAME"
    python3 "$REPO_ROOT/scripts/flutter-verify-apk.py" \
      build/app/outputs/flutter-apk/app-debug.apk "$FLUTTER_VERSION_CODE" "$FLUTTER_VERSION_NAME"
    # Device integration tests rebuild app-debug.apk. Preserve this deliverable
    # with its intended entrypoint and upgradeable version before that rebuild.
    mkdir -p build/artifacts
    cp build/app/outputs/flutter-apk/app-debug.apk build/artifacts/waddle-flutter.apk
    ;;
  web)
    cd "$REPO_ROOT/apps/flutter"
    flutter build web --no-pub
    ;;
  *) echo "Usage: $0 {rust|analyze|test|apk|web}" >&2; exit 2 ;;
esac
