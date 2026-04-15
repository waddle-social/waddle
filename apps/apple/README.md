# Waddle Apple App

Shared iOS + macOS SwiftUI app for Waddle.

## Generate project

```bash
brew install xcodegen
cd apps/apple
xcodegen generate
```

## Build from CLI

```bash
xcodebuild -project Waddle.xcodeproj -scheme Waddle-macOS -destination 'platform=macOS' build
xcodebuild -project Waddle.xcodeproj -scheme Waddle-iOS -destination 'generic/platform=iOS Simulator' build
```

If iOS build reports no simulator destination, install the iOS platform/runtime from **Xcode → Settings → Components**.

## Notes

- Native SwiftUI screens (not a full web wrapper).
- Uses native device auth flow against the selected Waddle server.
- Supports:
  - Provider sign-in
  - Session restore / sign-out
  - Public waddle search and join
  - Native create-waddle form
