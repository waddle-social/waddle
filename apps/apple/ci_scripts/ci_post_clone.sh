#!/bin/bash
# Xcode Cloud runs this after cloning, in every action's fresh environment.
# It installs the pinned Rust toolchain and builds the XCFramework the app
# targets link against.

set -euo pipefail

REPO_ROOT="${CI_PRIMARY_REPOSITORY_PATH:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)}"

# Homebrew's rustup formula is keg-only, so install rustup from upstream with
# no default toolchain; server/rust-toolchain.toml supplies the version.
if ! command -v rustup >/dev/null 2>&1; then
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
    | sh -s -- -y --no-modify-path --profile minimal --default-toolchain none
fi
export PATH="$HOME/.cargo/bin:$PATH"
(cd "$REPO_ROOT/server" && rustup toolchain install)

# An archive only needs the device slice(s) of the platform it archives, so
# skip the rest: each extra slice is another fat-LTO release build. Build and
# test actions keep every slice because they may target the Simulator.
PLATFORM="all"
if [[ "${CI_XCODEBUILD_ACTION:-}" == "archive" ]]; then
  case "${CI_PRODUCT_PLATFORM:-}" in
    iOS) PLATFORM="ios" ;;
    macOS) PLATFORM="macos" ;;
    *) PLATFORM="all" ;;
  esac
fi

bash "$REPO_ROOT/scripts/build-xcframework.sh" --platform "$PLATFORM"
