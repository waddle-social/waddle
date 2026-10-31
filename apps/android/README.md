# Waddle Android

Flutter presentation with Material 3 / Material You, hosted by the Android
application and driven by the shared Rust XMPP client via the existing Kotlin
session layer. The application id, session stores, notification receivers,
push distributions, encrypted attachment handling and LiveKit media remain
native. CLI-only: no Android Studio required.

The Dart module lives in `flutter/`. Kotlin `FlutterBridge` exposes commands and
state from the process-level `AppGraph`; it does not start another XMPP client.
The remaining Compose sources support native feature surfaces and their existing
tests while the Flutter interface is verified against the Android quality matrix.

## First-time setup (macOS)

```sh
# JDK via sdkman (or use the repo nix devshell, which exports JAVA_HOME):
sdk install java 21.0.7-tem

# Install Flutter 3.44 or later on PATH, or use the repo nix
# devshell, which provides Flutter. FLUTTER_ROOT selects an existing SDK.
# Generate the ignored add-to-app wrapper and resolve Dart packages:
bash scripts/setup-android-flutter.sh

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
bash ../../scripts/setup-android-flutter.sh         # first checkout / dependency changes
./gradlew :app:installFossDebug                    # host + Dart + native libraries
bash ../../scripts/build-android-rust.sh --debug   # after Rust client changes
./gradlew :core:client:testDebugUnitTest :app:testFossDebugUnitTest :app:testPlayDebugUnitTest
adb logcat --pid $(adb shell pidof -s social.waddle.android)

cd flutter
flutter analyze
flutter test
flutter attach                                   # hot reload an installed debug host
```

cuenv equivalents (same tasks CI runs): `cuenv task build --path apps/android`,
`checkBindingsDrift`, `test`, `flutterCheck`. Gradle tasks depend on `setupFlutter`
so the generated module exists before Gradle configures the host.

Do not run `flutter build apk` in the module: its generated host does not contain
Waddle's application graph or push flavours. Build and install through the
existing Android Gradle host. `.android/`, `.dart_tool/` and module build outputs
are generated locally; `pubspec.yaml` and the resolved lockfile describe the
module dependencies. Gradle stops with a setup instruction if its generated
Flutter loader is missing. SDK, Android package and Maven availability are
required to validate an APK; Dart analysis alone does not prove host integration.
Flutter 3.44+ is required for the host's AGP 9 toolchain. The locked Nix package
currently provides Flutter 3.47.0 with Dart 3.13.0.

## Install-in-place artifacts

Pull requests upload the FOSS debug APK as **waddle-android-apk**, retained for
seven days. Open the PR's **waddle-android-pullRequest** check, then use the
**Android APK** link in the run summary or download it from **Artifacts**. Sign
in to GitHub, extract `app-foss-debug.apk` from the downloaded ZIP, and install
it on Android 14 or later. The artifact uses the existing repository debug key.
Its version code uses commit-timestamp minutes, matching the rolling preview,
so a newer PR build can upgrade an older preview without clearing app data.

```sh
gh run download <run-id> --name waddle-android-apk
adb install -r app-foss-debug.apk
```

Independent test failures do not prevent building or uploading an APK. The run
still fails when any required check fails; review its check results before
treating a downloadable build as validated. A missing APK fails the upload step.

Every APK — local or CI — is signed with the **checked-in**
`debug.keystore` (deliberately not a secret; never used for a store
release), so `adb install -r` upgrades in place without uninstalling (app
data survives). Every merge to main builds the **preview** variant —
release code with R8, the baseline profile, and the Rust core at
`opt-level = 3` (`--profile mobile`) — as `waddle-android.apk`
(versionCode = commit-timestamp minutes, so upgrades are always
monotonic) and publishes it as the repo's **latest** release under a
unique `android-b<versionCode>-<sha>` tag:

```sh
gh release download --pattern waddle-android.apk   # latest release
adb install -r waddle-android.apk
# or straight from the browser/curl on the phone:
#   https://github.com/waddle-social/waddle/releases/latest/download/waddle-android.apk
```

Build it locally with `bash ../../scripts/build-android-rust.sh` (mobile
profile) then `./gradlew :app:assembleFossPreview`. A Play upload key, when
one exists, is read from `WADDLE_ANDROID_UPLOAD_*` environment variables
for the `release` build type.

(A single reused tag like `android-latest` is deliberately avoided:
GitHub's immutable releases permanently reserve a tag name once it has
backed a release, so a reused tag cannot be recreated. Unique tags per
build sidestep that; older rolling releases are pruned to the newest
10.)

## Testing in CI

Validation layers:

1. **Flutter** (`flutterCheck`): Dart analysis and widget/model tests.
2. **JVM** (every PR, `test` task): unit tests for the bridge, stores,
   auth API (MockWebServer), and ViewModels — no emulator, no SDK quirks.
3. **Instrumented smoke** (`deviceTests` pipeline): Gradle-managed headless ATD
   emulator (`:app:atdApi34FossDebugAndroidTest`) boots the real APK — `.so`
   loading, JNA, native application setup and Flutter's first rendered frame.
4. **On device**: `./gradlew :app:installFossDebug` against the S24 Ultra
   for anything interactive (calls, notifications, battery behavior).

Screenshot goldens (Roborazzi, JVM, light and dark) live in
`app/src/test/screenshots` and cover the retained Compose surfaces. They do not
validate Flutter rendering. Record them on Linux x86_64, matching CI:
native rendering on other platforms can differ by one or two RGB levels
around glows even when layout and content match. After an intended visual
change, re-record with
`./gradlew :app:testFossDebugUnitTest -Proborazzi.test.record=true` and
review the PNG diff. Run the Android checks independently with
`gh workflow run waddle-android-pullrequest.yml --ref <branch>`; verify that
the unit-test tasks actually ran before treating the run as evidence.
Failed checks upload actual images, visual diffs, and test reports.
`QUALITY.md` tracks what each feature is guarded by.

## Distributions

One codebase, two product flavours with the same application id (they
are signed differently, so switching between them means reinstalling):

| | `foss` (Obtainium, later F-Droid) | `play` (Google Play) |
|---|---|---|
| Push | UnifiedPush | FCM; UnifiedPush when a distributor is picked in Settings |
| Background connection | Foreground service (no distributor, or by choice) | None |
| Google code | None (`:app:verifyFossRuntimeClasspath`) | Firebase Messaging only (`:app:verifyPlayRuntimeClasspath`) |
| Variants | `fossDebug`, `fossPreview` (rolling GitHub release), `fossRelease` | `playDebug`, `playRelease` (no `playPreview`) |

The Play build reads its Firebase configuration from Gradle properties
or environment variables, never `google-services.json`:
`WADDLE_FIREBASE_PROJECT_ID`, `WADDLE_FIREBASE_APP_ID`,
`WADDLE_FIREBASE_API_KEY` and `WADDLE_FIREBASE_SENDER_ID`. Without them
it has no FCM, and `playRelease` refuses to build. FCM carries only the
Push Service's RFC 8291 ciphertext of the push envelope, encrypted to a
key pair the app generates (its private key sealed by the Android
Keystore), so Google sees no conversation or message. Firebase
Messaging itself still talks to Google: Firebase Installations (token
registration) and its heartbeat header. Delivery-metrics export and
Analytics collection are switched off in the Play manifest.

## Background delivery

With a UnifiedPush distributor installed (ntfy, NextPush, Sunup…),
Waddle registers its endpoint with the server's XEP-0357 Push Service
(the Web Push provider; no Google Play services) and connects only
while something needs the stream: the app in the foreground, a call, or
a notification action. In the FOSS build, without a distributor or with
**Settings → Stay connected** on, a foreground service keeps the
connection open. The Play build has no such service: FCM wakes it.
In push mode, calls ring only while the app is open, and DM pushes wait
while another session of the account is online.

## Layout

- `flutter/` / `:flutter` — Flutter add-to-app module, Material You UI and
  channel client. `.android/` is generated by `flutter pub get`.
- `:app` — Flutter activity, channel bridge, retained native feature surfaces,
  notifications, calls, and push
  (`push/`: `PushCoordinator` picks UnifiedPush or FCM). Flavour code
  lives in `src/foss` and `src/play` (`flavour.Distribution`).
- `:core:client` — the session core (`XmppSessionManager`,
  `ConnectionLoop`, stores, push registration), committed UniFFI bindings
  (`social.waddle.client.ffi`, drift-guarded by
  `scripts/check-android-ffi-bindings.sh`), JNA, R8 consumer rules, and the
  gitignored `src/main/jniLibs` built by `scripts/build-android-rust.sh`.

Version pins live in `gradle/libs.versions.toml`; the SDK/NDK pins in
`scripts/setup-android-sdk.sh` and `core/client/build.gradle.kts`
(`ndkVersion`) must move together.
