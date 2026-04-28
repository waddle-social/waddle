# Waddle Android App

Native Kotlin + Jetpack Compose client for Waddle. Targets Android 16 only
(`minSdk = compileSdk = targetSdk = 36`). No backwards-compat layers.

Links a Rust XMPP client through uniffi-generated Kotlin bindings; the
underlying crate is shared with the Apple app
(`server/crates/waddle-xmpp-client-ffi`).

## Prerequisites

- JDK 21 (Temurin recommended)
- Android SDK 36 + NDK 27 + cmdline-tools
- Rust toolchain (pinned in `server/rust-toolchain.toml`)
- `cargo-ndk` on `PATH`
- Three Android Rust targets:
  ```bash
  rustup target add aarch64-linux-android armv7-linux-androideabi x86_64-linux-android
  ```

The repo's Nix flake exposes a dedicated dev shell with all of this:

```bash
nix develop .#android
```

## Build process

### Step 1 — Build the Rust FFI artefacts

The `:ffi` Gradle module expects three pre-built `.so`s and the
uniffi-generated Kotlin sources:

```bash
bash ../../scripts/build-android-bindings.sh
```

This produces:

- `apps/android/ffi/src/main/jniLibs/{arm64-v8a,armeabi-v7a,x86_64}/libwaddle_xmpp_client_ffi.so`
- `apps/android/ffi/src/main/kotlin/uniffi/waddle_xmpp_client/waddle_xmpp_client.kt`

Both paths are gitignored. Pass `--debug` for faster non-release builds.

### Step 2 — Assemble the app

```bash
./gradlew :app:assembleDebug
```

Output APK: `app/build/outputs/apk/debug/app-debug.apk`.

For release App Bundles (unsigned for now):

```bash
./gradlew :app:bundleRelease
```

### Step 3 — Install and run

Create an Android 16 emulator (one-time):

```bash
sdkmanager "system-images;android-36;google_apis;arm64-v8a"
avdmanager create avd -n waddle-pixel -k "system-images;android-36;google_apis;arm64-v8a"
emulator -avd waddle-pixel &
./gradlew :app:installDebug
```

## Module layout

| Module    | Purpose                                                      |
|-----------|--------------------------------------------------------------|
| `:ffi`    | uniffi-generated Kotlin + JNI loader. Sealed.                |
| `:domain` | Pure Kotlin/JVM repositories, models, coroutine wiring.      |
| `:app`    | Compose UI, navigation, DI (Koin), Application + services.   |

We deliberately do **not** split per-feature modules — feature **packages**
inside `:app` (`auth`, `rooms`, `dms`, `compose`, `profile`, `theme`) match
the repo-wide "group by function, not by type" convention.

## Connection lifecycle

`WaddleClient` is a long-lived async XMPP connection. It lives in an
Application-scoped `WaddleConnectionManager` and is hosted by a
`ConnectionService` declared as `foregroundServiceType="dataSync"`.

Background delivery without push is best-effort on Android 16. A future
revision will plumb XEP-0357 + FCM for true push delivery.

## CI

- `.github/workflows/waddle-android-default.yml` runs on push to `main`.
- `.github/workflows/waddle-android-pullrequest.yml` runs on PRs touching
  `apps/android/**`, `server/crates/waddle-xmpp-client-ffi/**`, or the
  build script.

Both are generated from `apps/android/env.cue` by `cuenv ci --format github`
and validated by the existing `ci-drift` workflow.

## Conventions

- Conventional Commits with the dominant scope: `feat(android/ui): …`,
  `feat(android/ffi): …`, `feat(android/ci): …`, `fix(android): …`.
- Same XEP/Typed-payloads/clippy hard rules apply when this repo's Rust
  surface is touched. See root `AGENTS.md`.
