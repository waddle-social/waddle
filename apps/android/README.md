# Waddle Android App

Native Kotlin + Jetpack Compose client for Waddle. Targets Android 16 only
(`minSdk = compileSdk = targetSdk = 36`). No backwards-compat layers.

Links a Rust XMPP client through uniffi-generated Kotlin bindings; the
underlying crate is shared with the Apple app
(`server/crates/waddle-xmpp-client-ffi`).

## Prerequisites

- **JDK 25** (LTS) and **Gradle 9.5** — install via [SDKMAN](https://sdkman.io):
  ```bash
  sdk install java 25.0.3-tem
  sdk install gradle 9.5.0
  ```
- **Android SDK** with platform 36, build-tools 36.0.0, and **NDK 27**
  (install via Android Studio's SDK Manager or `sdkmanager`).
- **Rust** (the toolchain is pinned in `server/rust-toolchain.toml`).
- **Three Android Rust targets**:
  ```bash
  rustup target add aarch64-linux-android armv7-linux-androideabi x86_64-linux-android
  ```
- **`cargo-ndk`** (3.5+) on `PATH`:
  ```bash
  cargo install cargo-ndk
  ```

Set `ANDROID_HOME` (or `ANDROID_SDK_ROOT`) and `ANDROID_NDK_HOME` so
`cargo-ndk` and Gradle can find the toolchain.

## Build process

### Step 1 — Build the Rust FFI artefacts

The `:ffi` Gradle module expects three pre-built `.so`s and the
uniffi-generated Kotlin sources:

```bash
bash scripts/build-android-bindings.sh
```

This produces:

- `apps/android/ffi/src/main/jniLibs/{arm64-v8a,armeabi-v7a,x86_64}/libwaddle_xmpp_client_ffi.so`
- `apps/android/ffi/src/main/kotlin/uniffi/waddle_xmpp_client/waddle_xmpp_client.kt`

Both paths are gitignored. Pass `--debug` for faster non-release builds.

### Step 2 — Assemble the app

```bash
cd apps/android
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

| Module    | Purpose                                                       |
|-----------|---------------------------------------------------------------|
| `:ffi`    | uniffi-generated Kotlin + JNI loader. Sealed.                 |
| `:domain` | Repositories, models, coroutine wiring. Strict explicit-API.  |
| `:app`    | Compose UI, navigation, DI (Koin), Application + services.    |

Feature **packages** inside `:app` (`auth`, `rooms`, `dms`, `compose`,
`profile`, `theme`) match the repo-wide "group by function, not by type"
convention. We deliberately do **not** split per-feature modules until the
surface area justifies it.

## Connection lifecycle

`WaddleClient` is a long-lived async XMPP connection. It lives in an
Application-scoped `WaddleConnectionManager` and is hosted by a
`ConnectionService` declared as `foregroundServiceType="dataSync"`.

Background delivery without push is best-effort on Android 16. A future
revision will plumb XEP-0357 + FCM for true push delivery.

The uniffi-generated `WaddleEventListener` callbacks run on uniffi's
internal thread pool — **not** the Android main thread. The `:ffi` facade
converts them into a `MutableSharedFlow<WaddleEvent>` (DROP_OLDEST) so
they're safe to consume from any dispatcher.

## CI

- `.github/workflows/waddle-android-default.yml` runs on push to `main`.
- `.github/workflows/waddle-android-pullrequest.yml` runs on PRs touching
  `apps/android/**`, `server/crates/waddle-xmpp-client-ffi/**`, or the
  build script.

Both workflows install JDK 25 (Temurin), Android SDK + NDK 27,
`cargo-ndk`, and the Android Rust targets via standard GitHub Actions; the
build is otherwise identical to the local flow.

## Conventions

- Conventional Commits with the dominant scope: `feat(android/ui): …`,
  `feat(android/ffi): …`, `feat(android/ci): …`, `fix(android): …`.
- Same XEP/Typed-payloads/clippy hard rules apply when this repo's Rust
  surface is touched. See root `AGENTS.md`.
