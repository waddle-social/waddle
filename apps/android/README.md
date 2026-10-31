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

Local debug APKs use the checked-in `debug.keystore` for development only.
Distributed GitHub previews use a separate private RSA-4096 key, stored in
GitHub Actions secrets. The preview build refuses to build without private
signing material, and publication verifies the APK signature and pinned
certificate fingerprint before upload.

**Migration:** previews signed with the old public debug key cannot upgrade
in place to the private key. Uninstall the old app first; uninstalling erases
local app data. Subsequent privately signed previews upgrade in place.
The signing change does not establish or resolve Play Protect's detection reason.

Every merge to main builds `fossPreview` (R8, baseline profile, mobile-profile
Rust) as `waddle-android.apk`, with a monotonic commit-timestamp version code:

```sh
gh release download --pattern waddle-android.apk
adb install -r waddle-android.apk
```

GitHub repository secrets:

- `WADDLE_ANDROID_PREVIEW_KEYSTORE_BASE64`: base64-encoded PKCS12 keystore,
  alias `waddle-preview`.
- `WADDLE_ANDROID_PREVIEW_STORE_PASSWORD`: the keystore and key password.

The workflow materializes the key under `RUNNER_TEMP` with private file
permissions and removes it in an always-run Actions post-step. Never commit the
keystore or password. Keep a separate encrypted backup: losing this key
prevents updates to installed private-key previews.

The public preview certificate SHA-256 is
`ae705e4a0f2f14946a712b21373589680ffe8871ded17ecadb60beacc504de0d`.
For a local preview build, set `WADDLE_ANDROID_PREVIEW_SIGNING_DIR` to a private
directory containing `preview.p12` and `store-password`, then run
`./gradlew :app:assembleFossPreview`. Play store releases continue to use the
separate `WADDLE_ANDROID_UPLOAD_*` credentials.

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
