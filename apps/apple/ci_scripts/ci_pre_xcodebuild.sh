#!/bin/bash
set -euo pipefail

# Xcode Cloud pre-xcodebuild script
# Runs before xcodebuild to ensure framework is built

echo "=== Pre-Xcodebuild: Building Rust FFI Framework ==="

# Get repository root (script is at apps/apple/ci_scripts/ci_pre_xcodebuild.sh)
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../../.." && pwd)"
SRCROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

echo "📍 Script location: $SCRIPT_DIR"
echo "📍 Repository root: $REPO_ROOT"
echo "📍 Source root: $SRCROOT"

# Check if framework already exists
XCFW_PATH="$SRCROOT/Generated/WaddleXmppClientFFI.xcframework"
if [ -d "$XCFW_PATH" ]; then
  echo "✅ XCFramework already exists, skipping build"
  exit 0
fi

# Framework doesn't exist, build it
echo "🔨 Building WaddleXmppClientFFI.xcframework..."
if [ ! -f "$REPO_ROOT/scripts/build-xcframework.sh" ]; then
  echo "❌ ERROR: build-xcframework.sh not found at $REPO_ROOT/scripts/"
  exit 1
fi

bash "$REPO_ROOT/scripts/build-xcframework.sh" --release
if [ $? -ne 0 ]; then
  echo "❌ Failed to build xcframework"
  exit 1
fi

# Verify framework was created
if [ ! -d "$XCFW_PATH" ]; then
  echo "❌ XCFramework not found at $XCFW_PATH after build"
  exit 1
fi

echo "✅ XCFramework ready at: $XCFW_PATH"

