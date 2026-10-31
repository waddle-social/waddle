# Waddle Flutter presentation

The Android launcher hosts this Flutter module. `WaddleApplication` and its one
`AppGraph` still own the Rust XMPP client, account storage, outbox, reconnection,
read markers, encryption, push delivery, foreground services and LiveKit media.
Flutter owns navigation and Material 3 presentation. No XMPP implementation or
second session store is created in Dart.

## Build and check

Use Flutter 3.47 or newer with Dart 3.8 or newer, JDK 21 and the repository's
Android SDK/NDK setup. The repository Nix development shell includes Flutter.
From the repository root:

```sh
bash scripts/setup-android-sdk.sh
bash scripts/setup-android-flutter.sh
bash scripts/build-android-rust.sh --debug
cd apps/android/flutter
flutter analyze
flutter test
cd ..
./gradlew :app:assembleFossDebug :app:assemblePlayDebug
./gradlew :core:client:testDebugUnitTest :app:testFossDebugUnitTest :app:testPlayDebugUnitTest
./gradlew detekt :app:lintFossDebug :app:lintPlayDebug :app:verifyFossRuntimeClasspath :app:verifyPlayRuntimeClasspath
```

`flutter pub get` generates the ignored `.android` add-to-app wrapper used by
the host Gradle settings. The Android host remains the installable app; retain
its signing key, application ID, product flavours and manifest services. Use
`./gradlew :app:installFossDebug` to install it. A standalone `flutter run`
does not provide the host's graph or method channels.

## Features and ownership

| Flutter presentation | Existing native owner |
| --- | --- |
| Device authorization, restore and sign-out | `LoginViewModel`, `SessionBootstrap`, `AppGraph` |
| Home, direct/group messages, people, unread activity | `HomeViewModel`, `DmListViewModel`, directory and unread stores |
| Timelines, threads, replies, reactions, edit/retract, pin, history and typing | `ConversationViewModel` and channel/DM/thread factories |
| Rich composer, mentions, uploads, stickers, GIFs and extension forms | Existing composer, uploader, sticker, GIF and extension services |
| Profiles, avatar and mood/activity/tune | Existing profile APIs and image processor |
| Room/group administration and community users | Existing XMPP administration APIs |
| Incoming/active voice and video calls | `CallStore`, `CallSessionController`, native LiveKit PlatformViews |
| Appearance, notifications, background delivery | Existing `UserPrefs`, push coordinator and Android services |

The shared visual system in `lib/design.dart` pairs warm-paper and deep-teal
surfaces with Manrope UI typography, Waddle's Fredoka wordmark and the shared
penguin mark. Fonts are bundled with their SIL Open Font Licenses; no runtime
font download is required. Community rooms use grouped rows and contextual
creation actions. Conversations and the layered composer share a bounded reading
lane on wide screens. Profiles, preferences, people and calls use the same
spacing, tonal roles and typography.

Material You uses Material 3 controls, a generated teal scheme, stored light/dark
overrides, and Android wallpaper schemes when the existing wallpaper preference
is enabled. The existing preference default remains off. Flutter receives the
device's text scaling and reduced-animation settings. Wallpaper roles come from
the host's existing Android Material 3 API, without an additional Android plugin.
Navigation switches from
a bottom bar to a rail and list/detail panes at tablet widths.
Conversation pages keep the same state when a foldable changes width. Named
routes and the selected community tab participate in Android state restoration.

## Bridge contract

`social.waddle.android/commands` carries method calls with string-keyed maps.
`social.waddle.android/events` carries `{topic, data, generation}` snapshots.
Account generation changes invalidate account content and stale command results.
The bridge normalizes codec maps while preserving typed in-memory image bytes.
Errors are displayed with retry or dismissal affordances.

Conversation views have unique `viewId` ownership. They suspend read visibility
under routes and sheets, and restore their native ViewModel when revisited.
Closing an obsolete view cannot close its successor. Notification navigation
waits for session readiness and remains pending until Dart acknowledges the JID.
Call answer handling obtains capture permissions and checks the incoming call
identity atomically before accepting it; a delayed answer cannot accept a
successor call.

Encrypted images request decrypted bytes from the existing memory-only native
downloader. Other encrypted files use the native cache/FileProvider viewer path.
Session IDs, credentials, media tokens and encryption keys are never included in
presentation projections.

## Verification status

Flutter analysis and all 46 widget/bridge/layout tests pass locally on Flutter 3.44.0
and Dart 3.12.0. The Android CI contract suite also passes all 11 checks.
Standalone Kotlin Detekt and repository whitespace checks pass. The baseline
migration compiled both APK distributions and passed Android unit tests in CI
with the pinned Flutter 3.47 toolchain. The visual redesign requires its own APK
build. Device integration and feature parity require runtime evidence beyond
these source, widget and compilation checks.

`test/bridge_test.dart` covers account invalidation, byte preservation, navigation
acknowledgement, errors and listener disposal. `test/app_test.dart` covers provider
selection, incoming-call presentation, route restoration, foldable layout and
persisted appearance commands. Conversation tests cover composer Unicode
offsets, threads, content insertion and view lifetime; theme tests cover signed
Android palette values. Dialog tests cover controller ownership through closing
animations. These tests run with the real Flutter toolchain in CI. The existing
Compose goldens continue to guard retained reference components; they do not
validate the Flutter UI. `test/layout_test.dart` loads the real bundled fonts
and checks light/dark phone layouts, wide Home/conversation panes, and all main
screens at 320px with doubled text. Theme tests enforce normal-text contrast.
To export inspectable previews from the module directory:

```sh
flutter test test/layout_test.dart --dart-define=WADDLE_PREVIEW_DIR=build/design-previews
```

Before claiming verified device feature parity, exercise notification launch and
inline reply, background push for both flavours, encrypted
attachments, network churn and 1:1/group audio/video on a device before claiming
verified feature parity.
