# Waddle Project Tasks

## 1. Workspace Foundation
- [x] Create the workspace root `Cargo.toml` with `crates/*` members and shared workspace metadata.
- [x] Scaffold all crate directories (`core`, `storage`, `xmpp`, `roster`, `messaging`, `presence`, `mam`, `plugins`, `notifications`, `tui`, `gui-backend`) with minimal `Cargo.toml` and `lib.rs`/`main.rs` stubs.
- [x] Add workspace dependency versions and feature flags (`native`/`web`) aligned with `specs/01-architecture/dependency-map.md`.
- [x] Add root linting and formatting configuration (`rustfmt.toml`, `clippy.toml`).

## 2. Core Infrastructure (`waddle-core`)
- [x] Create `waddle-core` module structure with shared `WaddleError` enum and `Result` type alias. Verify zero workspace dependencies.
- [x] Implement the `Event` envelope and `EventPayload` enum from `specs/04-events/schema.md`, including `EventSource`, correlation ID support, and all payload data structs.
- [x] Implement hierarchical channel validation (dot-delimited `{domain}.{entity}.{action}` format) and channel helper utilities matching naming rules in `specs/04-events/schema.md`.
- [x] Implement `EventBus` with one `tokio::sync::broadcast` channel per top-level domain (`system`, `xmpp`, `ui`, `plugin`) and glob-pattern subscription filtering.
- [x] Add `EventBus` tests for routing correctness, glob filtering, per-domain ordering, correlation ID tracking, and lagged subscriber recovery behaviour.
- [x] Implement config loading (`config_path`, TOML parse, environment overrides, validation) per `specs/03-components/core-config.md`.
- [x] Implement i18n module (Fluent locale negotiation, message resolution) per `specs/03-components/i18n.md`.
- [x] Implement theming module (built-in themes, custom theme loading, CSS variable token generation) per `specs/03-components/themes.md`.

## 3. Storage Infrastructure (`waddle-storage`)
- [x] Create `waddle-storage` with `native` and `web` feature flags and the `Database` trait API from `specs/03-components/core-storage.md`. Depends on `waddle-core` only.
- [x] Implement native SQLite backend (`rusqlite`, WAL mode, serialised write path through a single writer task).
- [x] Implement migration runner and initial schema migration (`001_initial.sql`: `_migrations`, `messages`, `roster`, `muc_rooms`, `plugin_kv` tables with indices).
- [x] Add migration `002_add_mam_sync_state.sql` for the `mam_sync_state` table.
- [x] Add migration `003_add_offline_queue.sql` for the `offline_queue` table with status enum and indices, matching `specs/03-components/offline.md`.
- [x] Add web backend stub (`wa-sqlite`/`sql.js` compile-safe placeholder behind `web` feature flag).
- [x] Add storage tests for migration sequencing, query/transaction behaviour, and offline queue operations.

## 4. XMPP Infrastructure (`waddle-xmpp`)
- [x] Create `waddle-xmpp` with `native`/`web` feature flags. Depends on `waddle-core` only. Define the `XmppTransport` trait.
- [x] Implement feature-gated native TCP/TLS transport (`tokio-xmpp` + `rustls`) and web WebSocket transport (`tokio-tungstenite` / `web-sys`, RFC 7395, XEP-0156 discovery).
- [x] Implement `ConnectionManager` state machine (`Disconnected` → `Connecting` → `Connected` → `Reconnecting`) with exponential backoff (1s–60s cap) and lifecycle event emission per `specs/03-components/xmpp-connection.md`.
- [x] Implement SASL negotiation with SCRAM-SHA-256 preference, SCRAM-SHA-1 fallback, and explicit non-retryable `AuthenticationFailed` handling.
- [x] Implement XEP-0198 (Stream Management) for stream resumption after network interruption.
- [x] Implement XEP-0280 (Message Carbons) and XEP-0352 (Client State Indication) as core protocol features.
- [x] Implement stanza parse/serialise boundary using `xmpp-parsers`.
- [x] Implement the `StanzaProcessor` trait and `StanzaPipeline` with priority ordering and plugin hook insertion points (pre-process < 10, post-process > 50) per `specs/03-components/xmpp-stanza-pipeline.md`.
- [x] Implement the 7 built-in stanza processors: `RosterProcessor` (10), `MessageProcessor` (10), `PresenceProcessor` (10), `MamProcessor` (10), `MucProcessor` (10), `ChatStateProcessor` (20), `DebugProcessor` (100, debug builds only). Map outputs to `xmpp.*` events from `specs/04-events/catalog.md`.
- [x] Implement outbound routing: subscribe to `ui.*` command events (`ui.message.send`, `ui.presence.set`, roster/MUC commands) and convert them to stanza sends through the outbound pipeline.

## 5. Domain Components
- [x] Scaffold `waddle-roster` (depends: `core`, `storage`, `xmpp`), `waddle-messaging` (depends: `core`, `storage`, `xmpp`), `waddle-presence` (depends: `core`, `xmpp`), `waddle-mam` (depends: `core`, `storage`, `xmpp`), and `waddle-notifications` (depends: `core` only). Enforce dependency rules from `specs/01-architecture/workspace-layout.md` — domain crates never depend on each other.
- [ ] Implement roster management: initial fetch on connection, roster push handling, CRUD operations, group management, subscription state machine (`subscribe`/`subscribed`/`unsubscribe`/`unsubscribed`). Emit `xmpp.roster.*` and `xmpp.subscription.*` events.
- [ ] Implement presence management: local presence publish on connection, contact presence tracking (available/away/dnd/xa/unavailable), connection-state reactions (send unavailable on disconnect). Emit `xmpp.presence.*` events.
- [ ] Implement 1:1 messaging: send/receive with persistence to storage, delivery receipts (XEP-0184), chat state notifications (XEP-0085). Emit `xmpp.message.*` and `xmpp.chatstate.*` events.
- [ ] Implement MUC messaging: join/leave/send, subject updates, occupant tracking, room persistence to `muc_rooms` table. Emit `xmpp.muc.*` events.
- [ ] Implement MAM sync: `sync_since` using `mam_sync_state` table, paginated history fetch with RSM (XEP-0059), deduplication by message ID, sync progress events (`system.sync.started`/`system.sync.completed` with correlation IDs).
- [ ] Implement offline-first orchestration: enqueue outbound writes to `offline_queue` when disconnected, FIFO drain on reconnect, status lifecycle (`pending` → `sent` → `confirmed`/`failed`), reconcile with MAM per `specs/03-components/offline.md`.
- [ ] Implement notifications manager: global toggle, focused-conversation suppression, per-conversation mute, notification aggregation rules. Emit via platform-native APIs (`notify-rust`).

## 6. Plugin System (`waddle-plugins`)
- [ ] Scaffold `waddle-plugins` (depends: `core`, `storage` — not `xmpp`). Create modules for runtime, registry, and plugin KV storage.
- [ ] Implement plugin manifest parsing/validation and capability-based permission policy checks from `specs/05-plugin-api/packaging.md` and `specs/05-plugin-api/permissions.md`.
- [ ] Implement plugin KV storage with namespace isolation (`plugin_kv` table, keyed by plugin ID) and quota enforcement.
- [ ] Implement Wasmtime runtime: fuel metering, epoch interruption, memory caps, dedicated blocking thread pool. Implement plugin lifecycle: load → init → unload, with 5-error auto-disable threshold.
- [ ] Implement plugin event and stanza integration: WIT host functions for event subscribe/publish, enforcing `plugin.{id}.*` namespace restrictions on publish.
- [ ] Implement OCI plugin registry operations (install, uninstall, update, search/list) and local plugin index/cache management per `specs/03-components/plugins-registry.md`.

## 7. User Interfaces
- [ ] Implement `waddle-tui` shell: four-panel layout (sidebar, conversation, status bar, input), keyboard input loop, event bus subscribe/publish wiring per `specs/03-components/tui-shell.md`.
- [ ] Wire TUI command mode (`:` prefix) to domain actions (presence set, theme switching, room join/leave) with i18n message resolution and theme-driven rendering.
- [ ] Implement `waddle-gui-backend` Tauri v2 command handlers, app lifecycle wiring (startup sequence from `specs/01-architecture/overview.md`), and event forwarding to the Vue frontend.
- [ ] Initialise `gui/` with Vue 3 (Composition API) + Vite + Tailwind CSS + Ark UI Vue + Pinia + Vue Router. Create route views and Pinia stores for conversations, roster, settings, and plugins.
- [ ] Implement the `useWaddle()` composable abstracting Tauri IPC (`invoke()`) vs direct WASM imports (`wasm-bindgen`). Implement plugin SFC dynamic loading and CSS custom property theme switching.

## 8. Integration, Testing, and Delivery
- [ ] Implement startup/shutdown orchestration matching the deterministic sequences in `specs/04-events/lifecycle.md` (config → storage → event bus → plugins → XMPP connection → roster → presence → MAM sync).
- [ ] Create `waddle-test-support` crate with fixture loading helpers and test stanza/roster/config data in `tests/fixtures/`.
- [ ] Add Prosody testcontainers harness with pre-configured test users (alice, bob, charlie) and modules (MAM, MUC, Carbons) per `specs/07-testing/infrastructure.md`.
- [ ] Add integration tests for cross-crate flows: connection/auth, roster sync, 1:1 messaging, MUC messaging, MAM sync, offline queue drain.
- [ ] Add cucumber-rs BDD runner with step definitions covering: authentication, roster/presence, messaging (1:1 + MUC), MAM/offline sync, plugins, i18n/theming, and notifications features from `specs/06-bdd/features/`.
- [ ] Add property-based tests (proptest) for event ordering guarantees, storage consistency invariants, and stanza parsing robustness.
- [ ] Add coverage tooling (`cargo-llvm-cov` with threshold checks) aligned to `specs/07-testing/coverage-targets.md`.
- [ ] Add CI workflows (GitHub Actions): `cargo fmt`, `cargo clippy`, unit tests (all platforms), integration/BDD/coverage (Linux), and E2E test stubs. Document local developer run/test commands.
