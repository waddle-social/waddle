# Waddle Apple App

Shared iOS + macOS SwiftUI app for Waddle.

## Prerequisites

- Xcode 15.0+
- Rust 1.78+ with `cargo`
- `rustup` targets installed:
  ```bash
  rustup target add aarch64-apple-darwin x86_64-apple-darwin aarch64-apple-ios aarch64-apple-ios-sim x86_64-apple-ios
  ```
- `xcodegen` (for project generation):
  ```bash
  brew install xcodegen
  ```

## Build Process

### Step 1: Build Rust FFI Framework

The iOS and macOS apps depend on a Rust FFI framework. Build it once:

```bash
cd apps/apple
bash ../../scripts/build-xcframework.sh
```

This creates:
- `apps/apple/Generated/WaddleXmppClientFFI.xcframework` (framework)
- `apps/apple/Waddle/RustClient/Generated/` (Swift bindings)

**Optional:** Use `--debug` flag for faster debug builds:
```bash
bash ../../scripts/build-xcframework.sh --debug
```

### Step 2: Generate Xcode Project

```bash
xcodegen generate
```

This creates `Waddle.xcodeproj` from `project.yml`.
Apple brand assets are synced from `chat/public/waddle-logo.svg` by a target pre-build script.

### Step 3: Build and Run

**Build macOS:**
```bash
xcodebuild -project Waddle.xcodeproj -scheme Waddle-macOS -destination 'platform=macOS' build
```

**Build iOS (Simulator):**
```bash
xcodebuild -project Waddle.xcodeproj -scheme Waddle-iOS -destination 'platform=iOS Simulator,name=iPhone 17 Pro' build
```

Or open in Xcode:
```bash
open Waddle.xcodeproj
```

## CI/CD: TestFlight and Xcode Cloud

### GitHub Actions (Recommended for TestFlight)

The `.github/workflows/apple-testflight.yml` workflow:
1. Builds the Rust FFI framework on `macos-latest-xlarge`
2. Uploads framework and bindings as artifacts
3. Downloads artifacts and builds iOS and macOS targets
4. Runs on push to main and PRs touching app code

**To push to TestFlight:**
- Add a new job to the workflow that downloads the built app and uploads to App Store Connect
- Configure `FASTLANE_USER`, `FASTLANE_PASSWORD`, and App Store Connect API key as secrets

### Xcode Cloud Setup

Xcode Cloud requires the Rust framework to be pre-built before building the Swift targets:

**Option 1: Commit Framework to Git (Not Recommended)**
- Run `bash scripts/build-xcframework.sh` locally
- Commit `apps/apple/Generated/WaddleXmppClientFFI.xcframework` to Git
- Xcode Cloud will find the framework during builds
- **Downside:** Large binary files in git, harder to update

**Option 2: Custom Build Script in Xcode Cloud (Recommended)**
1. In Xcode Cloud settings, add a pre-build script:
   ```bash
   cd apps/apple
   rustup target add aarch64-apple-darwin x86_64-apple-darwin aarch64-apple-ios aarch64-apple-ios-sim x86_64-apple-ios
   bash ../../scripts/build-xcframework.sh --release
   ```
2. Configure the script to run before the iOS and macOS build steps
3. Ensure the runner has Rust and `xcodegen` installed (Xcode Cloud provides Rust natively)

**Option 3: GitHub Actions with Xcode Cloud**
- Keep the GitHub Actions workflow for building the framework
- Download the artifact and commit it to a separate branch
- Configure Xcode Cloud to use that branch for building the Swift app

## Notes

- Native SwiftUI screens (not a full web wrapper).
- Uses native device auth flow against the selected Waddle server.
- Supports:
  - Provider sign-in
  - Session restore / sign-out
  - Public waddle search and join
  - Native create-waddle form
