# Rust large-file split tracking list

Inventory: 2026-10-25, base `6f2c65b59`. Paths below are relative to `server/crates/`.
This is the follow-up list for issue #2023; implement one seam at a time, beginning
with `session_janitors/implementation.rs`, with existing behavior covered before
extraction. Extracted paths below name their current files; their line counts
are approximate.

Each future split should preserve the current public module and existing test names, or deliberately update nextest filters and source-reading tests with inventory evidence.

1. `waddle-server/src/server/session_janitors/implementation.rs` (~6400): first split operational loops by lifetime (session expiry, orphan occupants, detached send attempts, node drain); retain shared scheduler/cancellation wiring. The original `session_janitors.rs` facade still holds ~9250 lines of inline tests and test helpers; split those by the same seams while preserving externally filtered test paths or updating filters atomically.
2. `waddle-server/src/server/routes/websocket/tests/stream_management.rs` (14176): group handshake/resume, ack/window accounting, detach/replay and persistence scenarios; shared fixtures stay in test support.
3. `waddle-xmpp/src/muc/room_registry_actor/tests.rs` (12285): split registration/ownership, durable restore, joins and teardown cases.
4. `waddle-xmpp/src/muc/room_actor/tests.rs` (11401): split membership/presence, messages/history, invites and owner/admin commands.
5. `waddle-server/src/server/routes/websocket/tests/muc.rs` (9787): split joins/presence, configuration/admin, invites, message/archive and restart scenarios.
6. `waddle-server/src/admin/channels.rs` (9618): separate channel CRUD/membership operations, durable cleanup/cascade integration and test fixtures.
7. `waddle-xmpp/src/stream_management/session_registry/tests.rs` (9148): separate registration/lookup, resume claims, ack/replay and expiration/ownership scenarios.
8. `waddle-server/src/muc_durable.rs` (~5670): separate durable-room persistence, hydration, orphan cleanup and authority tests retained in the original facade. Production implementation now lives in `muc_durable/implementation.rs` (~2550), which is below the large-file threshold.
9. `waddle-xmpp/src/muc/room_registry_actor/implementation.rs` (~7140): separate actor messages, ownership transitions and room lifecycle handlers; existing durable_restore extraction is a seam to extend.
10. `waddle-server/src/server/routes/websocket/cleanup.rs` (7094): separate disconnect cleanup, detached ingress drain, MUC teardown and replay proof paths.
11. `waddle-server/src/pending_delivery/tests.rs` (6524): split custody/ack, recovery/reconstruction, cross-node and malformed-storage cases.
12. `waddle-server/src/server/routes/websocket/tests/messages.rs` (6360): separate direct/group messaging, delivery receipts, offline/recovery and negative cases.
13. `waddle-server/src/clustering/claims.rs` (6338): separate SQL claim operations, batch/hydration behavior and CAS/fencing integration tests.
14. `waddle-server/src/server/routes/websocket/tests/iq.rs` (5715): group roster/discovery, account/private-storage and extension IQ scenarios.
15. `waddle-xmpp/src/ingress/effect_intent.rs` (5538): separate typed representations, stanza/error normalization, canonical serialization and conformance fixtures.
16. `waddle-server/src/db/migrations/tests.rs` (5482): group migration generations/domains and shared schema fixtures.


## Additional production parent-module inventory

The AST audit also checks self-named source files. The following 43 files
previously declared at least one production child module and contained more
than 300 lines of direct implementation, excluding imports, module declarations,
and inline modules. Test-only child modules and test files are excluded. Counts
record the inventory before the mechanical facade extraction.

All 43 now keep production implementation in a private `implementation.rs` child
leaf. Their existing child modules and tests remain in the original facade,
along with the imports those consumers need. Reexports preserve established
paths and visibility boundaries. The
extraction preserves explicit clustering remote-actor/message identifiers,
serialization shapes and macro behavior. Moving a whole implementation into
one leaf does not finish the responsibility-level splits listed above,
beginning with `session_janitors.rs`.

| Source (relative to `server/`) | Direct implementation lines | Production children |
| --- | ---: | ---: |
| `crates/waddle-xmpp/src/muc/presence.rs` | 621 | 2 |
| `crates/waddle-xmpp/src/muc/room_registry_actor.rs` | 6920 | 1 |
| `crates/waddle-xmpp/src/muc/durable.rs` | 537 | 3 |
| `crates/waddle-xmpp/src/muc/room_actor.rs` | 3208 | 6 |
| `crates/waddle-xmpp/src/error.rs` | 489 | 1 |
| `crates/waddle-xmpp/src/protocol/machine.rs` | 438 | 3 |
| `crates/waddle-xmpp/src/protocol/handlers/jingle.rs` | 1468 | 1 |
| `crates/waddle-xmpp/src/registry/user_actor.rs` | 539 | 1 |
| `crates/waddle-xmpp/src/telemetry/attributes.rs` | 1743 | 1 |
| `crates/waddle-xmpp-core/src/disco/info.rs` | 616 | 1 |
| `crates/waddle-xmpp-core/src/pubsub/stanzas.rs` | 500 | 1 |
| `crates/waddle-xmpp-client/src/runtime.rs` | 764 | 2 |
| `crates/waddle-server/src/notification_outbox/legacy_ancestry.rs` | 625 | 1 |
| `crates/waddle-server/src/telemetry.rs` | 512 | 2 |
| `crates/waddle-server/src/notification_settings_projection.rs` | 382 | 1 |
| `crates/waddle-server/src/ingress/commit.rs` | 1097 | 1 |
| `crates/waddle-server/src/ingress/restamp.rs` | 446 | 1 |
| `crates/waddle-server/src/ingress/maintenance.rs` | 905 | 2 |
| `crates/waddle-server/src/ingress/execute.rs` | 1188 | 5 |
| `crates/waddle-server/src/ingress/recorded.rs` | 1141 | 3 |
| `crates/waddle-server/src/ingress/recovery_rebuild.rs` | 474 | 3 |
| `crates/waddle-server/src/ingress/append_authority.rs` | 466 | 1 |
| `crates/waddle-server/src/sm_persistence_fenced.rs` | 1622 | 2 |
| `crates/waddle-server/src/sm_persistence.rs` | 856 | 7 |
| `crates/waddle-server/src/clustering/relay.rs` | 2450 | 3 |
| `crates/waddle-server/src/server/extension_host_adapter.rs` | 351 | 8 |
| `crates/waddle-server/src/server/session_janitors.rs` | 8199 | 2 |
| `crates/waddle-server/src/server/routes/websocket/stream_management.rs` | 1361 | 2 |
| `crates/waddle-server/src/server/routes/websocket/handlers/presence.rs` | 482 | 5 |
| `crates/waddle-server/src/server/routes/websocket/handlers/presence/subscription.rs` | 304 | 4 |
| `crates/waddle-server/src/server/routes/websocket/handlers/presence/muc.rs` | 2158 | 2 |
| `crates/waddle-server/src/server/routes/websocket/handlers/iq/roster.rs` | 420 | 1 |
| `crates/waddle-server/src/server/routes/websocket/handlers/pubsub_fanout.rs` | 467 | 1 |
| `crates/waddle-server/src/server/routes/interpret.rs` | 709 | 29 |
| `crates/waddle-server/src/server/routes/auth.rs` | 445 | 3 |
| `crates/waddle-server/src/server/routes/uploads.rs` | 393 | 1 |
| `crates/waddle-server/src/server/routes/interpret/route_to_connection.rs` | 1896 | 1 |
| `crates/waddle-server/src/server/routes/interpret/direct_archive.rs` | 680 | 1 |
| `crates/waddle-server/src/muc_durable.rs` | 2496 | 1 |
| `crates/waddle-server/src/pending_delivery/database.rs` | 1832 | 4 |
| `crates/waddle-server/src/pubsub.rs` | 532 | 6 |
| `crates/waddle-server/src/inbox.rs` | 735 | 3 |
| `crates/waddle-server/src/sm_promotion.rs` | 1325 | 5 |
