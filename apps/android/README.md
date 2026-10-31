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
./gradlew :app:installFossDebug                    # Kotlin-only changes
bash ../../scripts/build-android-rust.sh --debug   # after Rust client changes
./gradlew :core:client:testDebugUnitTest :app:testFossDebugUnitTest :app:testPlayDebugUnitTest
adb logcat --pid $(adb shell pidof -s social.waddle.android)
```

cuenv equivalents (same tasks CI runs): `cuenv task build --path apps/android`,
`checkBindingsDrift`, `test`.

## Install-in-place artifacts

The trusted main-branch CI build signs the **preview** variant with a
repository Actions secret; local builds and pull requests use the machine's
Android debug keystore. CI preview APKs can update one another with
`adb install -r`. A local APK may need to be uninstalled before installing a
CI preview, and vice versa.

The preview signing certificate has rotated from the previously checked-in
debug keystore. If you have an older preview installed, uninstall it before
installing the new one. Android removes the app's local data on uninstall.
After this one-time reinstall, new CI previews can update in place. See
[`docs/signing.md`](docs/signing.md) for the key boundary.

Every merge to main builds the **preview** variant — release code with R8,
the baseline profile, and the Rust core at `opt-level = 3`
(`--profile mobile`) — as `waddle-android.apk`
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

Three layers, cheapest first:

1. **JVM** (every PR, `test` task): unit tests for the bridge, stores,
   auth API (MockWebServer), and ViewModels — no emulator, no SDK quirks.
2. **Instrumented smoke** (`deviceTests` pipeline, manual until the
   runner's KVM support is proven): Gradle-managed headless ATD emulator
   (`:app:atdApi34FossDebugAndroidTest`) boots the real APK — `.so` loading,
   JNA, and first-frame composition on device.
3. **On device**: `./gradlew :app:installFossDebug` against the S24 Ultra
   for anything interactive (calls, notifications, battery behavior).

Screenshot goldens (Roborazzi, JVM, light and dark) live in
`app/src/test/screenshots`. Record them on Linux x86_64, matching CI:
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

- `:app` — Compose UI (Material 3 driven by the generated
  `WaddleTokens.kt`), navigation, notifications, calls, and push
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
