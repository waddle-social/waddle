# Delete multi-waddle code paths

## Context

Each Waddle server instance is now a single Waddle — no directory, no discovery, no waddle-switcher. The in-progress migration (visible in `git status` and commit `2e13f34`) has stripped the most visible multi-waddle UI but left a long tail of residue: array-of-waddles state, `waddle_id` parameters, a DB table modeling waddles as entities, a `{waddle_id}_{channel_id}` JID prefix, and per-waddle abstractions in the server's DB pool. The chat client's half-migrated state currently has runtime-breaking call-site mismatches.

Goal: finish the job. Delete every multi-waddle code path so the data model, the wire protocol, and the UI all reflect "this server IS the Waddle." Apps may still connect to multiple *different* servers (kept), but each connection is to exactly one Waddle.

User decisions captured:
- **DB**: delete `waddles` table; rename `waddle_members` → `members`. Space metadata (name, description, icon) moves to server config.
- **JID**: drop `waddle_id` prefix — managed rooms become `{channel_id}@muc.domain`. Wire-breaking (allowed: no prod data; CLAUDE.md breaking-changes-by-default rule).
- **Apple multi-server**: keep session-map; a user can still point the app at different Waddle servers. Multi-*server* is preserved; multi-*waddle-per-server* is gone.
- **XEP-0503**: reduce to single-space for now. Full "server-side spaces" implementation is a follow-up.

## Server (`server/crates/waddle-server`, `server/crates/waddle-xmpp*`)

### Database (`src/db/migrations.rs`, `src/db/pool.rs`)
- Delete `waddles` table migration; remove `waddles` CREATE, indexes, and any seed rows.
- Rename `waddle_members` table → `members`. Drop the `waddle_id` foreign-key column. Primary key becomes `user_id`.
- Rewrite migrations as fresh (no prod data — CLAUDE.md rule).
- `DatabasePool` (`src/db/pool.rs` lines 26–87): remove `get_waddle_actor`, `get_waddle_db`, `create_waddle_db`, `unload_waddle_db`. Replace with a single `actor()` / `db()` accessor. Delete the per-waddle HashMap abstraction entirely.

### Routes (`src/server/routes/waddles.rs`, `src/server/routes/channels.rs`)
- Rename `waddles.rs` → `space.rs`.
- Delete all `obsolete_*_handler` stubs (lines 82–98, 404–431) — routes and their 404 stubs both go.
- Delete `WaddleResponse`, `WaddleState.single_tenant` flag, `list_all_waddles_from_db`, `list_user_waddles`, `insert_waddle`, `list_waddle_members`, `get_waddle_member`, `add_waddle_member_with_timestamp`, `remove_waddle_member`, `update_waddle_member_role`, `get_waddle_from_db`, `get_canonical_waddle_from_db`.
- Keep `SpaceResponse`, `get_space_handler`, `create_space_handler`, `update_space_handler`, `delete_space_handler`, and the four `space/members` handlers — drop the internal `waddle_id` threading; each operates on the implicit space.
- `channels.rs`: delete imports of `get_canonical_waddle_from_db` (line 18) and the lookup logic at lines 213–228. Remove `waddle_id` from `get_waddle_actor()` call sites (lines 248, 344, 779) — use the single-actor accessor. Delete any remaining `waddle_id` query-param or struct field.
- `src/server/routes/websocket/mod.rs` (line 3507–3512): delete `INSERT INTO waddles` path. Any waddle-id-routed WS handler loses the parameter.

### Permissions (Zanzibar tuples)
- Every tuple currently using `ObjectType::Waddle` + `waddle_id` (lines 531, 554, 701, 1204, etc.) → single `ObjectType::Space` with a fixed/implicit id (or drop the object entirely if single-tenant makes role lookups trivial). Simplify role checks accordingly.

### XMPP core (`server/crates/waddle-xmpp-core/src/domain.rs`)
- Delete `managed_room_localpart(waddle_id, channel_id)`, `parse_managed_room_localpart`, `parse_managed_room_jid`, `managed_room_jid(waddle_id, channel_id, …)` (lines 66–96).
- Replace with single-arg versions: `managed_room_jid(channel_id, muc_domain)` returning `{channel_id}@muc.domain`.
- Audit every caller (routing, XEP implementations, tests) and drop the `waddle_id` argument.

### XMPP state / routing (`src/server/xmpp_state.rs`, `crates/waddle-xmpp/src/routing.rs`)
- `xmpp_state.rs`: remove `actor_for_waddle()` (line 34) and `list_all_waddles()` (lines 460–490).
- `routing.rs`: keep single-domain routing (already single). Delete any waddle-id dispatch.

### XEP-0503 spaces (`crates/waddle-xmpp/src/xep/xep0503.rs`)
- Reduce to single space. `build_channel_item`, `build_spaces_metadata_form`, `build_spaces_type_form`: keep, but driven by config-derived space metadata, not DB rows.
- Drop the multi-space pubsub-node-per-waddle pattern. Per CLAUDE.md XEP rule: update the XEP's Rust test suite in the same PR (not just delete tests — rewrite to cover the single-space shape).
- If full rework is deferred (user mentioned "server-side spaces" is follow-up), mark XEP-0503 advertisement off until reimplemented, per the "implement or un-advertise" rule.

### Config (`src/config.rs`, `src/server/mod.rs`)
- Move space metadata (name, description, icon_url) from DB into `ServerConfig` (new `ServerConfig.space: SpaceConfig { name, description, icon_url }`).
- Delete `single_tenant` boolean from both `ServerConfig` (line 89) and `XmppConfig` (line 133) — it's now the only mode.
- Delete the boot-time "no waddles exist" guard in `src/server/mod.rs` lines 460–471.

## Chat client (`chat/`)

### Critical-first fixes (these currently break at runtime)
- `chat/src/components/ChatApp.vue:602` calls `loadCanonicalWaddle()`; `chat/src/composables/useWaddles.ts:404` exports `loadSpace`. Rename ChatApp's call site → `loadSpace()`, or settle on a final name during the rename below.
- `chat/src/lib/waddle-api.ts:1–9` `WaddleSummary` is missing `id`, but `ChatApp.vue:550–552` and `useRouting.ts:86` use `w.id`. These will be deleted wholesale in the steps below (no `id` needed once multi-waddle state is gone).

### State and API
- Rename `useWaddles.ts` → `useSpace.ts`. Collapse the array-of-waddles model to a single `space: Ref<Space | null>`. Delete `waddles`, `activeWaddleId`, `currentWaddle`, and `resolveWaddle` (in `useRouting.ts`).
- `chat/src/lib/waddle-api.ts`:
  - Drop `WaddleSummary` (or rename to `Space` and strip `id`).
  - Change `listMessages(waddleId, channelId, before)` → `listMessages(channelId, before)`; path becomes `/v1/space/channels/{channelId}/messages` (or similar — align with server's `/v1/space/*` routes).
  - Rename any `/v1/waddles/*` client paths → `/v1/space/*`. (The server is dropping the old routes, not 404-stubbing them.)
- `chat/src/composables/useMembers.ts` lines 75, 94, 108: drop `waddleId` from `addMember` / `updateMemberRole` / `removeMember` call sites. Remove the waddleId extraction at lines 69, 88, 102.
- `chat/src/composables/useWaddles.ts` (→ `useSpace.ts`): remove `waddleId` parameter from `loadStructure`, `updateChannel`, `deleteChannel`, `createChannel`. Remove `resolvedWaddleId` fallback logic (line 153).

### XMPP client (`chat/src/lib/xmpp/client.ts`, `discovery.ts`, `types.ts`)
- `client.ts`: delete `canonicalWaddleId` tracking (line 677), the `switchRoom`-sets-canonical-waddle-id logic (line 685), and the `spaceId` getter (line 730). `discoverSpaceChannels()` (line 712) loses its space inference — just calls disco on the fixed MUC domain.
- `discovery.ts`: `discoverChannels` loses its optional `waddleId` param. Remove `spaceId` from returned channels (lines 56–60, 90, 883, 891).
- `types.ts`: remove `spaceId` from `DiscoveredChannel` (line 202).

### Routing (`chat/src/composables/useRouting.ts`)
- URLs become `/{channelSlug}[?thread=...]`. Drop the first-segment waddleSlug parsing (lines 49, 74, 81–87). Delete `resolveWaddle`. `ChatApp.vue:547` (`resolveWaddle(route.waddleSlug, waddles.waddles.value)`) goes with it.

### Components
- `WaddlesSidebar.vue`: delete the component and its imports. A single-space app has no waddle rail. The `empty state "No space"` text (line 213) becomes moot. Any "switch waddle" affordance in `ChatApp.vue` (including the `v-for="waddle in waddles"` at line 165) is deleted along with the sidebar.
- `ChatApp.vue`: delete `@select-waddle` handlers, `activeWaddleId` binding, and anything reading `waddles.waddles.value`. The only workspace concept left is the channel list.

## Apple app (`apps/apple/Waddle/`)

### Preserved
- `AppConfig.sessionMapKey` and related per-server session storage (user explicitly keeps multi-server). Each entry still maps `serverURL → sessionID`; only the in-server waddle multiplicity is removed.

### AppModel (`App/AppModel.swift`)
Delete:
- `publicWaddles: [WaddleSummary]` (line 65)
- `selectedWaddleID: String?` (line 66)
- `joinedWaddleIDs: Set<String>` (line 70)
- `isLoadingWaddles` (line 74)
- `accessibleWaddles: [WaddleSummary]` (line 90)
- `selectedWaddle` computed (lines 125–127)
- `selectWaddle(_:)` (lines 246–264)
- `refreshAccessibleWaddles()` (lines 1009–1033)
- `mergeVisibleWaddles()` (lines 1858–1864)
- `isJoined(_:)` (lines 786–787)
- `loadStructure(for waddleID:)` (lines 1035–1053): replace with `loadStructure()` — no waddle ID.
- Auto-select-waddle-on-login flow (lines 958–973)

Replace with:
- `@Published var space: Space?` holding the single connected server's space (name, description, icon, role). Populated from the new `/v1/space` endpoint on login.

### XMPP layer (`RustClient/RustXmppClient.swift`, `XMPP/XMPPTypes.swift`)
- `fetchCanonicalWaddle() -> XMPPDiscoveredWaddle?` (lines 132–135): rename to `fetchSpace() -> XMPPSpace?`. Return model loses `id`.
- `discoverChannels(waddleID:)` (lines 137–145): drop the parameter.
- `XMPPDiscoveredWaddle` → `XMPPSpace` (lines 235–240). `XMPPDiscoveredChannel` stays but any `waddleId` field is removed.
- `roomBareJID(accountJID:waddleID:channelID:)` (line 284): change to `roomBareJID(accountJID:channelID:)`. Every caller in `AppModel.swift` (lines 1118, 1163, 1515, 1869) drops the `waddleID` argument.

### UI — delete switcher/picker surfaces
- `App/MobileSlackShellView.swift` lines 4–14 (`MobileWorkspaceFilter` enum), 21/38–42 (filter AppStorage), 45–47 (`joinedWaddles`), 150–152 (`switcherWaddles`), 166–180 ("Switch fast" section), 307–388 (`MobileWorkspaceBrowserTab`). The browser tab disappears from bottom tab-bar entirely.
- `App/DesktopAuthenticatedShell.swift` lines 54–70 (`orderedWaddles`), 93–114 (avatar ScrollView + ForEach + `DesktopWorkspaceAvatarButton`). The left rail either collapses to nothing (channels become the primary sidebar) or keeps a single static space-identity header.
- `App/ContentView.swift` lines 248–317 (`WaddleListView`), 372–396 (`WaddleDetailView`). iPad split view's leading pane becomes channel list directly.
- `mobileWorkspaceFilterKey` in `AppConfig.swift`: delete.

### Models (`Models/APIModels.swift`)
- `WaddleSummary` (lines 77–99): rename to `Space`, drop `id`, keep `name`, `description`, `iconURL`, `role`, timestamps.

### Chat store (`Chat/ChatStore.swift`)
- `replaceRooms` (line 128): keep, but it's no longer called on "waddle switch" — only on connect. Any `selectedWaddleID`-dependent guard in `AppModel.syncChatRooms()` (lines 1866–1883) goes away.

## Critical files

Server:
- `server/crates/waddle-server/src/db/migrations.rs`
- `server/crates/waddle-server/src/db/pool.rs`
- `server/crates/waddle-server/src/server/routes/waddles.rs` → rename `space.rs`
- `server/crates/waddle-server/src/server/routes/channels.rs`
- `server/crates/waddle-server/src/server/routes/websocket/mod.rs`
- `server/crates/waddle-server/src/server/xmpp_state.rs`
- `server/crates/waddle-server/src/server/mod.rs`
- `server/crates/waddle-server/src/config.rs`
- `server/crates/waddle-xmpp-core/src/domain.rs`
- `server/crates/waddle-xmpp/src/routing.rs`
- `server/crates/waddle-xmpp/src/xep/xep0503.rs` (+ its test suite per CLAUDE.md XEP rule)

Chat:
- `chat/src/components/ChatApp.vue`
- `chat/src/components/chat/WaddlesSidebar.vue` (delete)
- `chat/src/composables/useWaddles.ts` → rename `useSpace.ts`
- `chat/src/composables/useMembers.ts`
- `chat/src/composables/useRouting.ts`
- `chat/src/lib/waddle-api.ts`
- `chat/src/lib/xmpp/client.ts`
- `chat/src/lib/xmpp/discovery.ts`
- `chat/src/lib/xmpp/types.ts`

Apple:
- `apps/apple/Waddle/App/AppModel.swift`
- `apps/apple/Waddle/App/AppConfig.swift`
- `apps/apple/Waddle/App/MobileSlackShellView.swift`
- `apps/apple/Waddle/App/DesktopAuthenticatedShell.swift`
- `apps/apple/Waddle/App/ContentView.swift`
- `apps/apple/Waddle/Chat/ChatStore.swift`
- `apps/apple/Waddle/RustClient/RustXmppClient.swift`
- `apps/apple/Waddle/XMPP/XMPPTypes.swift`
- `apps/apple/Waddle/Models/APIModels.swift`

## Suggested execution order

1. **Server DB + routes + config** — foundational. Land `/v1/space/*` as the only space/member API, delete `waddles` table, drop `single_tenant` toggle.
2. **Server XMPP JID flattening + XEP-0503 single-space** — wire-protocol change; coordinate with clients.
3. **Chat client** — fix the broken call sites first, then rename `useWaddles`→`useSpace`, delete `WaddlesSidebar`, flatten routing.
4. **Apple app** — last, because it has the biggest UI surface and the JID change affects room joins.

Each step must keep `bun test && bun run lint` (chat) and `cargo test` (server) green.

## Verification

Per-layer checks:

- **Server**: `cargo test -p waddle-server -p waddle-xmpp -p waddle-xmpp-core` passes. `cargo run --bin waddle-server` starts without a seeded waddle row. `curl` against the running server: `GET /v1/space` returns the configured space; legacy `GET /v1/waddles`/`/v1/waddles/:id` return 404 via normal routing (not stubs). `grep -R "waddle_id\|waddle_members\|table waddles\b" server/crates` returns no hits.
- **Chat**: `cd chat && bun test && bun run lint` (knip clean). Load app, confirm URL is `/{channelSlug}` only, channel switch works, sending messages works, no console errors. `grep -R "waddleId\|WaddleSummary\|BrowsePublicWaddles\|CreateWaddleDialog" chat/src` returns no hits.
- **Apple**: `xcodebuild test -scheme Waddle` passes. Launch iOS app: sign in, channels list loads, switching channels works, no workspace switcher is visible on mobile/iPad/macOS. Sign in to a second server (different URL): session map preserves both. `grep -R "waddleID\|selectedWaddleID\|publicWaddles\|WaddleListView\|WaddleDetailView\|MobileWorkspaceBrowserTab" apps/apple` returns no hits.
- **End-to-end**: start fresh server with config-provided space metadata, connect chat + Apple clients, create a channel via XMPP, confirm it appears in both clients with the flattened `{channelId}@muc.domain` JID.
