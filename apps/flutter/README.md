# Waddle Flutter

An independent Flutter app backed directly by the shared Rust XMPP client. The
existing Kotlin application remains at `apps/android`, unchanged by this PR.
Both applications can be installed together: this app uses `social.waddle.flutter`,
its own credential namespace, storage, resource identity and push registration.

| Layer | Owner |
| --- | --- |
| Material You UI, navigation and presentation | Flutter/Dart |
| Authentication, account lifecycle, reconnect, views and durable outbox | Dart application runtime |
| XMPP authentication, protocol operations and signaling | Shared Rust XMPP client |
| Message identity, archive authority, deduplication and mutations | Shared Rust timeline |
| Android protocol binding | Direct `dart:ffi` to `waddle-flutter-runtime` |
| Browser protocol binding | Rust WASM, loaded through Dart JS interop |
| Files, secure credentials, notifications and call media | Flutter platform plugins and Dart adapters |

There is no dependency on the Kotlin application's AppGraph, stores, services,
JNI bindings, platform views or presentation channels. The small Java `FlutterActivity` shell lazily retains the Flutter engine across
activity destruction so a call service can keep it alive. Platform plugins may
contain their own native implementations. The original app's package and private files are
never opened by this app.

## Interface

Warm-paper/deep-teal light and dark themes use bundled Manrope typography,
Waddle's Fredoka wordmark and the shared penguin mark. Android wallpaper schemes,
text scaling, reduced animation, adaptive navigation and restored navigation are
supported. Room chat uses compact sender groups and a bottom composer in both
reading orders, with formatting, attachments and safety details on demand.

The UI includes device-flow sign-in, rooms/DMs/threads, rich composition,
mentions, reactions, message actions, moderation, encrypted attachments,
GIFs/stickers, profiles/status, room/group administration and voice/video calls.
Dart orchestrates these features through typed Rust operations; Rust remains the
protocol authority. LiveKit's Flutter SDK owns media rendering and capture.

## Runtime boundaries

`lib/transport` provides the same asynchronous RPC/event interface on native and
web. Native requests enter a typed C ABI dispatcher and Tokio tasks; Dart never
blocks on a network operation or receives callbacks from arbitrary Rust threads.
Opaque handles, request IDs, generations and event sequences fence retired
sessions. Buffers are explicitly freed. Browser builds load a matched pair of
`wasm-pack --target web` JavaScript and WASM files under `web/rust`.

`lib/runtime` owns independent session/application state. Credentials and durable
account data use this app's secure store. UI snapshots exclude credentials,
media tokens and attachment encryption keys. Account changes fence every async
operation and clear private views, media and cached files. Browser tabs use separate resource identities. Native XMPP stream
resumption is disabled until this application persists/reuses resume state.

The shared Rust timeline validates archive authority and mutation actors before
projecting a message. Conversation controllers preserve visible-view/read-edge
ownership, threads, paging, durable origin IDs, acknowledgement/echo races and
Unicode mention offsets. Encrypted transfers validate HTTPS slots, allowed
headers, AES-GCM authentication and hashes before exposing bytes to a viewer.

Notifications use independent push identities. Android supports UnifiedPush and
an optional foreground connection; the browser uses a separate Web Push worker.
A process owner coordinates plugin-created background engines with the visible
application before restoring credentials or opening a connection. Inline replies
use the same durable conversation send path. A new Firebase application has not
been configured for this separate package; Google Play push is not advertised.

## Development

Use the repository's pinned Rust/Nix toolchain and Flutter 3.47 (Dart 3.13),
JDK 21, CMake and Ninja. Android builds use NDK `28.2.13676358` and Android API 26
or newer. The SDK setup helper writes only this application's local configuration.

```sh
cd apps/flutter
flutter pub get --enforce-lockfile
../../scripts/build-flutter-rust.sh --host
WADDLE_FLUTTER_RUNTIME_LIBRARY="$PWD/../../server/target/debug/libwaddle_flutter_runtime.so" flutter test
```

For an independent Android APK:

```sh
scripts/setup-flutter-android.sh
scripts/build-flutter-rust.sh --android
scripts/flutter-ci.sh apk
```

The APK is `apps/flutter/build/app/outputs/flutter-apk/app-debug.apk`. Debug builds
use the committed public preview keystore for consistent upgrades across CI runs;
it is not a production signing key. Release signing must be configured separately.
For web:

```sh
scripts/build-flutter-rust.sh --web
scripts/flutter-ci.sh web
```

Serve `apps/flutter/build/web` over HTTPS or localhost. The Flutter application
and its Rust WASM assets must be served together. Do not copy the chat app's
Vite-specific WASM glue into this app.

## Verification and delivery

The standalone CI lane builds Rust for Android arm64/x86_64, host FFI and browser
WASM; checks Rust formatting/warnings/tests and Flutter analysis/tests; produces
the independent APK, web build and inspectable design previews. The boundary
check rejects changes to the original Kotlin app and its build pipeline.

Tests exercise real C ABI WebSocket authentication, messages, acknowledgements
and retirement; Dart/native ABI conversion and Rust timeline trust; account
handoff, authentication cancellation and stale operations; encrypted transfers;
call permission/SID races; and the UI in light/dark/wallpaper colors, wide panes,
320px/2× text, both reading orders and keyboard/reply/mention layouts. Android
emulator smoke verifies the standalone bootstrap, secure storage and packaged
Rust library. Each claim of passing verification must be tied to its run result.

To export synthetic UI previews:

```sh
flutter test test/layout_test.dart --dart-define=WADDLE_PREVIEW_DIR=build/design-previews
```

Builds and fixtures do not establish full physical-device feature parity.
Notification wakeups, distributor behavior, encrypted file viewers and 1:1/group
media need device testing. The PR records current evidence, limitations and the
latest standalone APK download.

Room edits and retractions use archive author identities and immutable presence
attribution captured when a live message arrives. A target with a known stable
author rejects an unstamped actor, even when a mutation arrives before history.
The Flutter adapter opts into this stronger policy; existing adapters keep their
prior default until they provide live identity stamps.
Genuinely anonymous histories retain nickname-bound behavior; the current client
facades do not expose XEP-0421 occupant IDs, so nickname reuse in those anonymous
histories cannot be proven or retroactively resolved.
