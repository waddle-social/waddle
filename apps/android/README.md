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
