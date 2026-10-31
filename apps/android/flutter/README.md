# Waddle Flutter presentation

The Android launcher hosts this Flutter module. `WaddleApplication` and its one
`AppGraph` still own the Rust XMPP client, account storage, outbox, reconnection,
read markers, encryption, push delivery, foreground services and LiveKit media.
Flutter owns navigation and Material 3 presentation. No XMPP implementation or
second session store is created in Dart.

## Build and check

Use Flutter 3.44 or newer with Dart 3.8 or newer, JDK 21 and the repository's
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

The initial migration was authored in an environment without Flutter, Dart or an
Android SDK. The official Flutter SDK download was denied by its network policy.
Source parsing/formatting, standalone Kotlin Detekt and repository whitespace
checks can be run there; they do **not** establish Dart type correctness, Android
compilation, visual parity or working device integration.

`test/bridge_test.dart` covers account invalidation, byte preservation, navigation
acknowledgement, errors and listener disposal. `test/app_test.dart` covers provider
selection, incoming-call presentation, route restoration, foldable layout and
persisted appearance commands. Conversation tests cover composer Unicode
offsets, threads, content insertion and view lifetime; theme tests cover signed
Android palette values. These tests
must run with the real Flutter toolchain before release. The existing Compose
goldens continue to guard retained reference components; they do not validate the
Flutter UI. Record and review new Flutter light/dark renderings, and exercise
notification launch/inline reply, background push for both flavours, encrypted
attachments, network churn and 1:1/group audio/video on a device before claiming
verified feature parity.
