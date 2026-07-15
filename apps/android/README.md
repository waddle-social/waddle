# Waddle Android

Native Android client (Kotlin + Jetpack Compose), driven by the shared Rust
XMPP client via UniFFI — the sibling of `apps/apple`. CLI-only: no Android
Studio required anywhere.

## First-time setup (macOS)

```sh
# JDK via sdkman (or use the repo nix devshell, which exports JAVA_HOME):
sdk install java 21.0.7-tem

# Pinned SDK packages (honors an existing ~/Library/Android/sdk via ANDROID_HOME):
bash scripts/setup-android-sdk.sh

# Cross-compile the Rust client + regenerate the committed Kotlin bindings:
bash scripts/build-android-rust.sh --debug
```

Pair the phone once (Settings → Developer options → Wireless debugging →
*Pair device with pairing code*):

```sh
adb pair <ip>:<pair-port>     # enter the code shown on the phone
adb connect <ip>:<connect-port>
```

## Daily loop

```sh
cd apps/android
./gradlew :app:installDebug                        # Kotlin-only changes
bash ../../scripts/build-android-rust.sh --debug   # after Rust client changes
./gradlew testDebugUnitTest
adb logcat --pid $(adb shell pidof -s social.waddle.android)
```

cuenv equivalents (same tasks CI runs): `cuenv task build --path apps/android`,
`checkBindingsDrift`, `test`.

## Install-in-place artifacts

Debug builds are signed with the **checked-in** `debug.keystore`
(deliberately not a secret; never used for release), so every APK —
local or CI — carries the same signature and `adb install -r` upgrades
in place without uninstalling (app data survives). Every merge to main
refreshes the `android-latest` GitHub prerelease with
`waddle-android-debug.apk` (versionCode = commit-timestamp minutes, so
upgrades are always monotonic):

```sh
gh release download android-latest --pattern waddle-android-debug.apk
adb install -r waddle-android-debug.apk
```

## Testing in CI

Three layers, cheapest first:

1. **JVM** (every PR, `test` task): unit tests for the bridge, stores,
   auth API (MockWebServer), and ViewModels — no emulator, no SDK quirks.
2. **Instrumented smoke** (`deviceTests` pipeline, manual until the
   runner's KVM support is proven): Gradle-managed headless ATD emulator
   (`:app:atdApi34DebugAndroidTest`) boots the real APK — `.so` loading,
   JNA, and first-frame composition on device.
3. **On device**: `./gradlew :app:installDebug` against the S24 Ultra
   for anything interactive (calls, notifications, battery behavior).

## Layout

- `:app` — Compose UI. Currently the M0 toolchain spike (`MainActivity`
  exercises `parseJid`, a tokio-backed `connect`, and `on_event` callbacks).
- `:core:client` — committed UniFFI bindings
  (`social.waddle.client.ffi`, drift-guarded by
  `scripts/check-android-ffi-bindings.sh`), JNA, R8 consumer rules, and the
  gitignored `src/main/jniLibs` built by `scripts/build-android-rust.sh`.

Version pins live in `gradle/libs.versions.toml`; the SDK/NDK pins in
`scripts/setup-android-sdk.sh` and `core/client/build.gradle.kts`
(`ndkVersion`) must move together.
