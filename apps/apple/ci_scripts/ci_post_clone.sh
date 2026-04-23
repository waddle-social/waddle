#!/bin/bash
set -euo pipefail

# Xcode Cloud pre-build script to prepare dependencies for iOS/macOS build
# This script runs after the repository is cloned but before building

echo "=== Xcode Cloud Pre-Build Setup ==="

# Get repository root
# Xcode Cloud runs this script from the workspace, script is at apps/apple/ci_scripts/ci_post_clone.sh
# Going from apps/apple/ci_scripts up to repository root: ../../../
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../../.." && pwd)"
echo "📍 Script location: $SCRIPT_DIR"
echo "📍 Working directory: $(pwd)"
echo "📍 Repository root: $REPO_ROOT"

# Verify we're in the right place
if [ ! -f "$REPO_ROOT/scripts/build-xcframework.sh" ]; then
  echo "❌ ERROR: scripts/build-xcframework.sh not found at $REPO_ROOT"
  echo "   Current directory contents:"
  ls -la "$REPO_ROOT" | head -20
  exit 1
fi

# Install Rust if not present
if ! command -v cargo &> /dev/null; then
  echo "📦 Installing Rust via Homebrew..."
  brew install rust
else
  echo "✅ Rust already installed: $(rustc --version)"
fi

# Add Apple targets
echo "🎯 Adding Rust Apple targets..."
rustup target add aarch64-apple-darwin aarch64-apple-ios aarch64-apple-ios-sim

# Build the xcframework from repository root
echo "🔨 Building WaddleXmppClientFFI.xcframework..."
cd "$REPO_ROOT"
bash "$REPO_ROOT/scripts/build-xcframework.sh" --release
if [ $? -ne 0 ]; then
  echo "❌ Failed to build xcframework"
  exit 1
fi

# Verify the framework was created
XCFW_PATH="$REPO_ROOT/apps/apple/Generated/WaddleXmppClientFFI.xcframework"
if [ ! -d "$XCFW_PATH" ]; then
  echo "❌ XCFramework not found at $XCFW_PATH"
  ls -la "$REPO_ROOT/apps/apple/Generated/" || echo "   Generated directory does not exist"
  exit 1
fi
echo "✅ XCFramework created at: $XCFW_PATH"
ls -lh "$XCFW_PATH" 2>/dev/null | head -5

echo "✅ Xcode Cloud pre-build setup complete"
