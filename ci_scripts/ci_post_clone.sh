#!/bin/bash
set -e

# Xcode Cloud pre-build script to prepare dependencies for iOS/macOS build
# This script runs after the repository is cloned but before building

echo "=== Xcode Cloud Pre-Build Setup ==="

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

# Build the xcframework
echo "🔨 Building WaddleXmppClientFFI.xcframework..."
cd apps/apple
bash ../../scripts/build-xcframework.sh --release
if [ $? -ne 0 ]; then
  echo "❌ Failed to build xcframework"
  exit 1
fi
cd ../..

echo "✅ Xcode Cloud pre-build setup complete"
