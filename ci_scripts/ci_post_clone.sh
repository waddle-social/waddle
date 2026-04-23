#!/bin/bash
set -e

# Xcode Cloud pre-build script to prepare dependencies for iOS/macOS build
# This script runs after the repository is cloned but before building

echo "=== Xcode Cloud Pre-Build Setup ==="

# Get repository root (Xcode Cloud clones to /Volumes/workspace/repository)
REPO_ROOT="$(pwd)"
echo "Repository root: $REPO_ROOT"

# Install Rust if not present
if ! command -v cargo &> /dev/null; then
  echo "📦 Installing Rust via Homebrew..."
  brew install rust
else
  echo "✅ Rust already installed"
fi

# Add Apple targets
echo "🎯 Adding Rust Apple targets..."
rustup target add aarch64-apple-darwin aarch64-apple-ios aarch64-apple-ios-sim

# Build the xcframework from repository root
echo "🔨 Building WaddleXmppClientFFI.xcframework..."
bash "$REPO_ROOT/scripts/build-xcframework.sh" --release
if [ $? -ne 0 ]; then
  echo "❌ Failed to build xcframework"
  exit 1
fi

# Verify the framework was created
XCFW_PATH="$REPO_ROOT/apps/apple/Generated/WaddleXmppClientFFI.xcframework"
if [ ! -d "$XCFW_PATH" ]; then
  echo "❌ XCFramework not found at $XCFW_PATH"
  exit 1
fi
echo "✅ XCFramework created at: $XCFW_PATH"

echo "✅ Xcode Cloud pre-build setup complete"
