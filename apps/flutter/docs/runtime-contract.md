# Independent Flutter runtime contract

The Kotlin app at apps/android stays byte-for-byte identical to PR base. apps/flutter
is a separate installable application (social.waddle.flutter), with private storage.
UI and app orchestration are Dart; protocol semantics are existing shared Rust.

## Rust transport

`abstract interface class RustTransport` exposes:
- `Stream<Map<String, dynamic>> get events`
- `Future<dynamic> rpc(String method, [Map<String, dynamic> args = const {}])`
- `Future<void> close()`

Factory: `Future<RustTransport> createRustTransport()` via conditional imports.
Contract file: lib/transport/transport.dart. Native/web implementations live alongside.
`connect` args: server_url, jid, access_token, resource, resumable:false. Calling
connect constructs/configures a fresh underlying Rust client; readiness is an event.
Other RPC names/argument keys follow existing native Rust WaddleClient method and
parameter names (snake_case). Results use snake_case DTO fields. No raw XML RPC.

Events: {type, data}. Types: message, presence, connected, sessionLifecycle,
disconnected, error, deliveryAcked, deliveryFailed, messageRejected, mdsDisplayed,
avatarChanged, pubsub, call, streamManagement. Raw DTO fields are snake_case.
Native FFI additionally carries handle/generation/sequence/request IDs internally;
adapter strips result envelopes, resolves request futures and drops retired handles.
Calls never block Flutter UI waiting for network or emit Dart callbacks from Rust.
close completes/cancels pending requests, disconnects, retires handles, frees buffers.

## Dart application runtime

UI-facing WaddleBridge keeps current topic/invoke contract for existing widgets.
Replace MethodChannel defaults with injected `AppRuntime` command and event stream.
New runtime owns independent secure authentication, preferences and account stores.
UI snapshots never contain credentials, media tokens or encryption keys.

`SessionRuntime` (runtime/session_runtime.dart) owns account generation, auth,
secure credential restore, transport connection/reconnect, presence/home state.
It implements `Future<dynamic> invoke(String method, Map<String,dynamic> args)`
and `Stream<Map<String,dynamic>> get events` (topic/data/generation envelopes).
It exposes current transport through `RustTransport? get transport`, session
metadata getters (jid, generation), and `emit(topic,data)` for child controllers.
SessionRuntime dispatches conversation.* to ConversationRuntime, media to
PlatformServices, and other surface commands to SurfaceRuntime. Dependencies
are injected for deterministic integration tests. No import of Kotlin app code.

`ConversationRuntime` owns view leases, paged history and message projection;
shared Rust timeline validates trusted identity/mutations where available. It
accepts a RuntimeContext (runtime/context.dart) exposing:
- RustTransport? get transport; String? get jid; int get generation;
- Map<String,dynamic> get preferences; Map<String,dynamic> get session;
- void emit(String topic, Map<String,dynamic> data);
- Future<dynamic> rpc(String method,[Map<String,dynamic> args=const {}]);
- Future<dynamic> platform(String method,[Map<String,dynamic> args=const {}]);
- Future<void> persist();
- Map<String,dynamic> get persistent;
ConversationRuntime(context) exposes invoke(method,args), onEvent(event),
onAccountChanged(), dispose(). Session owns durable account-scoped persistent map
and reconnect; conversation owns outbox queue/retry/read cursors within it.

SurfaceRuntime(context) exposes invoke(method,args), onEvent(event),
onAccountChanged(), dispose(); handles profile/directory/admin/rooms/groups.
Session owns login/session/home/preferences directly. Surface can expose refreshHome
for topology/inbox aggregation, coordinated with session by callback or events.

PlatformServices in runtime/platform_services.dart owns files, authenticated encrypted
media transfer and LiveKit media. It exposes invoke(method,args), dispose(),
events; uses Rust only for upload slots and XMPP call signaling. Public plugin
platform implementations are allowed; Kotlin AppGraph/MethodChannel adapter is not.
Native Android shell is standard generated FlutterActivity only.

Every async operation is account+generation fenced. Native resumption stays off
until the new app actually persists/reuses resume state. Unknown/unsupported verbs
throw explicit errors; never return fabricated success/empty results.
