# #2046 W1.9: caller inventory for the connection layer and routing decision points

Slice W1.9 of the #2046 plan (v9, §3 Wave 1). This document is the deletion and ownership source that B2a–f, D2b–e, D2p, D3s, E2, DEFprep, ACT-DEF and C7 cite. It inventories the following. **Production** means code compiled into `cargo build -p waddle-server --features clustering` (the flake's production build, `flake.nix:367`), excluding `cfg(test)` items and the `test-support` feature.

- Every production read, write and caller of `ConnectionRegistry` and `ConnectionEntry` (field by field).
- All 40 `WsConnState` fields.
- `dual_registration`.
- The user-resource mirrors.
- Room dispatch.
- The remote-resource relay endpoints.
- The local-versus-remote routing decision points outside `clustering/`.
- The producers of each `OrderedRelayMucProxyKind`.
- Every production producer that awaits capacity on a connection's outbound channel.

- Base: `main@db6e5338`. Line numbers refer to that commit.
- Nothing here changes code. Counts are regenerable with the commands in §0.
- Test code is counted separately and not itemised.

## Classification

Each read, write or call is classified relative to the [#2046 target authority model](https://github.com/waddle-social/waddle/issues/2046) (one owner per field):

| Class | Meaning | Typical fate |
|---|---|---|
| **A: authority** | The state is today's live source of truth for a decision, or the call reads or writes such state. Examples: presence, interest and carbons flags; SM state; pending subscribes. | Moves to exactly one owner: the connection actor (D2a) for socket-local truth, or the account actor (E2) for selection. Shared copies are deleted at ACT-DEF. |
| **I: derived index** | A lookup structure or published copy derivable from an authority. Examples: the `FullJid → ConnectionEntry` DashMap, the `sm_stream_owners` reverse index, entry copies of the SM id and occupancy generation, and ownership checks against the entry slot. | Stays as a derived, transport-only socket index after ACT-DEF, or is replaced by a control-channel query (DEFprep). |
| **T: transport concern** | Socket and channel plumbing. Examples: outbound sender, force-detach channel, write interruption, bind gate, socket lifecycle, send window, deferred inbound. | Owned by the D1 pumps and the D2a/D2r actor and retirement protocol. Producer semantics change in D2b–e/D2p. |
| **O: obsolete migration glue** | Exists only to keep two registries or two nodes' copies in step, or is dead. Examples: `dual_registration`, remote-hosted `ConnectionEntry` mirrors, `apply_remote_resource_state`, the carbons-flag ownership token, and the unused `StanzaRouter`. | Deleted: W1.3 for the token, ACT-DEF for mirrors and `dual_registration`, C7 for relay entry points. Dead code can be deleted at any time. |

Path abbreviations in the tables:

- `ws/` = `waddle-server/src/server/routes/websocket/`
- `interpret/` = `waddle-server/src/server/routes/interpret/`
- `clustering/` = `waddle-server/src/clustering/`
- `server/` = `waddle-server/src/server/`
- `srv/` = `waddle-server/src/`
- `xmpp/` = `waddle-xmpp/src/`

The "slice" column names the §3 slice that removes or keeps the item. "keep" means no plan slice touches it.

## 0. Method, headline counts and corrections

### 0.1 How the inventory was produced

Two independent passes were cross-checked.

1. **Type-resolved pass (authoritative for every production count below).**
   - The inventory targets were temporarily marked `#[deprecated(note = "W19|<tag>")]`:
     - every method of `impl ConnectionRegistry`, its 7 fields and the type itself;
     - every field and method of `ConnectionEntry`, and the type;
     - the 40 non-test `WsConnState` fields;
     - `dual_registration::{mirror_register, mirror_register_outcome, mirror_unregister}`;
     - every `pub` method of `impl OrderedRelayDeliveryBridge`;
     - every `OrderedRelayMucProxyKind` variant;
     - the `ClaimStore::current_claim` trait method.
   - The production feature set was then checked. Every `deprecated` warning is one use site, resolved by the compiler (no name collisions, no comments). `cargo check` without `--tests` does not compile `cfg(test)` items, so every hit is production code.
   - Warning lines were mapped back to `db6e5338` line numbers, and the markers were reverted.

   ```sh
   # from the repository root, on a clean checkout of db6e5338
   python3 patch_deprecate.py                       # Appendix B.2; inserts 255 marker lines
   rm -rf /tmp/w19-patched && mkdir -p /tmp/w19-patched/server \
     && cp -r server/crates /tmp/w19-patched/server/  # snapshot the marker-bearing tree
   cd server && PROTOC=$(command -v protoc) \
     cargo check -p waddle-server --features clustering --locked --message-format=json > ../check.json
   cd .. && git checkout -- server/crates           # remove the markers
   # PATCHED_ROOT = marker-bearing snapshot, ORIG_ROOT = this (unmodified) checkout
   python3 parse_check.py check.json /tmp/w19-patched . > uses.tsv   # Appendix B.3
   ```
   `parse_check.py` maps each compiled line back to the original by subtracting the marker lines above it in `PATCHED_ROOT`. If it is given the reverted tree as `PATCHED_ROOT`, sites in the 35 marker-bearing files come out up to 40 lines too low, with no warning. The committed tables used the correct two trees. Re-parsing the same `check.json` against a saved snapshot of the patched files reproduces `uses.tsv` byte for byte; parsing against the unpatched tree would change 583 rows. Result at `db6e5338`: 1,502 distinct use sites. By tag: `CR::` 259, `CRF` 199, `CE::` 125, `CEF` 191, `WS.` 430, `DUAL::` 15, `BRIDGE::` 179, `MUCKIND::` 63, `CLAIM::` 41.

2. **Textual pass (production/test split for counts the compiler cannot give, and a regression check).** `rg` lists hits. A filter (Appendix B.1) tags each hit as follows:
   - **test**: under a `tests/`, `benches/` or `examples/` directory; named `tests.rs`, `*_tests.rs`, `test_*.rs` or `tests_*.rs`; declared behind `#[cfg(test)]`; or inside an item gated by `#[cfg(test)]` / `#[cfg(all(test, …))]`.
   - **test-support**: behind `cfg(feature = "test-support")` or `cfg(any(test, feature = "test-support"))`.
   - **prod**: everything else.

   ```sh
   rg -n --no-heading 'ConnectionRegistry|connection_registry' server/crates -g '*.rs' | python3 testregions.py --tag
   rg -n --no-heading 'ConnectionEntry'                         server/crates -g '*.rs' | python3 testregions.py --tag
   rg -n --no-heading 'WsConnState'                             server/crates -g '*.rs' | python3 testregions.py --tag
   rg -n --no-heading 'dual_registration::mirror_(register|unregister)' server/crates -g '*.rs' | python3 testregions.py --tag
   rg -n --no-heading 'remote_hosted\(|apply_remote_resource_state\(' server/crates -g '*.rs' | python3 testregions.py --tag
   rg -n --no-heading 'OrderedRelayMucProxyKind::\w+'          server/crates -g '*.rs' | python3 testregions.py --tag
   rg -n --no-heading '\.current_claim(_after_pending_writes)?\(' server/crates -g '*.rs' | python3 testregions.py --prod | grep -v /clustering/
   rg -n --no-heading '\.send_to(_with_write_acceptance|_if_owner)?\s*\(' server/crates -g '*.rs' | python3 testregions.py --tag
   rg -n --no-heading 'RelayHandle::new|Relay(ExtensionRoomSend|MucAdminQuery)\s*\{' server/crates/waddle-server/src -g '*.rs' | python3 testregions.py --prod | grep -v /clustering/
   rg -n -A1 'remote_message\("waddle' server/crates/waddle-server/src/clustering
   ```

### 0.2 Headline counts (production unless stated)

| Item | Type-resolved | Textual prod (lines / files) | Textual test (lines / files) |
|---|---|---|---|
| `ConnectionRegistry` methods (72 `pub`/`pub(super)`, incl. `new`) | **228 call sites in 63 files** outside `registry/connection_registry*`. 71 files call a method or name the type. 17 methods have no caller outside the module; 12 of those have no production caller at all. | 411 / 105 (`ConnectionRegistry\|connection_registry`, incl. comments and pass-through fields) | 1,043 / 165 |
| `ConnectionEntry` (17 fields) | 136 field uses (plus 55 type-name references) and 125 accessor calls. 16 of 17 fields are touched outside the owning connection task; **1 (`pending_subscribes_flushed`) is not** | 92 / 29 | 218 / 43 (+1 test-support) |
| `WsConnState` (40 fields + 2 `cfg(test)` hooks) | 430 direct accesses, all in `server/routes/websocket/` (9 files), plus writes through destructured borrows (§3) | 76 / 13 | 343 / 31 |
| `dual_registration` | **15 call sites in 6 files** | 15 / 6 | 12 / 11 |
| User-resource mirrors | `remote_hosted` 1 site; `apply_remote_resource_state` 1 caller; 3 direct atomic writes in `owner_update.rs`; 2 snapshot builders (§5) | 2 / 2 and 2 / 2 | 9 / 5 |
| Bridge calls outside `clustering/` | 43 calls in 25 files: **41 routing decisions in 23 files**, 1 construction, 1 mirror janitor | 60 / 30 (`OrderedRelayDeliveryBridge`) | 181 / 46 |
| Inline `current_claim` branches outside `clustering/` in waddle-server | **9 in 8 files** (+16 inside waddle-xmpp actors: claim internals, out of scope) | 41 / 24 (all crates, incl. `clustering/`) | 285 / 42 |
| Local-vs-remote decision points outside `clustering/` (all forms, §8.5) | **59 in 31 files** | | |
| Awaiting producers into a connection channel | **38 live call sites in 18 files**, plus 4 dead (`StanzaRouter`). `send_to` + `send_to_with_write_acceptance` alone: **33 in 18 files** | 40 lines (`.send_to*(`) | 14 |
| `OrderedRelayMucProxyKind` producers | 4 of 8 kinds have producers (9 producer sites); `PrivateMessage`, `BareRoomIq`, `OccupantIq` and `FanoutChunk` have none | 50 / 13 | 58 / 8 |

### 0.3 Corrections to the plan's figures

| Plan statement (final.md) | Finding at `db6e5338` | Effect |
|---|---|---|
| `ConnectionRegistry`: "~120 non-test files" (§3 W1.9; grounding: 120 files / 552 matches) | The textual count included inline `mod tests`, comments and pass-through struct fields. Textual production count: 105 files / 411 lines. Type-resolved callers: **63 files / 228 sites**; 71 files touch the type at all. | B2/D2/E2 scoping uses §1.3, not 120. |
| `dual_registration`: "14 prod call sites" | **15 in 6 files**. The grounding count (14) is wrong: its own line list has 15 entries, including `ws/registration.rs:847` (`register_remote_clustered_resource`, the `NotRemote` fallback). | ACT-DEF deletion list is §4. |
| "~37 direct `OrderedRelayDeliveryBridge` calls in 18 handler/interpret files + ~12 inline `current_claim` branches in ~10 files (~50 points, ~28 files)" | Grounding's table lists 38 calls, not 37. Type-resolved: **41 decision-bearing bridge calls in 23 files**. The extra three are `recipient_inventory_remote`, `extension_room_handle` and `muc_admin_query_handle`. There are **9** inline `current_claim` branches in waddle-server outside `clustering/`, **7** helper-caller branches (`remote_owner`, `extension_room_owner`, `require_local_room`), and **2** direct `RelayHandle` asks (`demote`, `confirm_reply_receipt`). Total **59 in 31 files**. | Rule 1 freeze baseline is §8. The `muc_durable` demote ask and the ingress reply-receipt ask are not B2 moves: they are a surviving ask (DEC-2 lists `demote`) and a relay-internal receipt that C7 deletes (§8.4). |
| "33 production callers in 18 files" of `send_to`/`send_to_with_write_acceptance` (DEC-3) | Confirmed: **33 in 18 files**. Four more `send_to` calls in `waddle-xmpp/src/routing.rs` are dead: `StanzaRouter` is re-exported (`lib.rs:69`) but never constructed outside its tests. **Five more awaiting producers** exist: `send_to_if_owner` ×2, `send_pending_flush`, `send_pending_flush_if_owner`, `reserve_pending_flush` (all `.await` channel capacity, `sending.rs:30-53,230-345`). | D2p owns 11 of the 38 live sites (§10). The cycle-class claim ("no producer awaits a connection channel") requires all 38. |
| D3s "expected small or empty" | **Non-empty: one field.** `ConnectionEntry.pending_subscribes_flushed` is read and written only by its own connection: presence handler `regular.rs:134`, detach snapshot `cleanup.rs:987`, resume republish `registration.rs:665`. Every other duplicated field has a reader outside the connection (§2.1). | D3s = move the live CAS onto `WsConnState.pending_subscribes_flushed` and delete the entry field and its accessor. The move must keep an owner or generation check: today both CAS sites go through `entry_if_owner`, so a superseded socket neither consumes the claim nor delivers the RFC 6121 queue. |
| DEFprep reader list cites `session_janitors/ingress_custody.rs:77,103`, `implementation.rs:1613,4145`, `archive_dispatch.rs:30,50` | These read through **registry methods**, not the entry field: `active_sm_stream_ids` (`ingress_custody.rs:77,103`, `implementation.rs:4145`) and `unregister_if_sm_stream_id` (`implementation.rs:1613`). The methods still read the entry's SM id outside the owner, so **all of these sites stay on DEFprep's list**. That includes the second `unregister_if_sm_stream_id` caller, `ws/cleanup.rs:3865` (`cleanup_invalidated_detached_session`), which runs from *another* connection's bind or resume path (`registration.rs:173`, `stream_management/registration.rs:228`, `cleanup.rs:1120,1396`). `archive_dispatch.rs:30,50` is `waddle-xmpp/src/registry/connection_registry/archive_dispatch.rs` inside `ConnectionEntry::try_send_archive_ordered`, called from `UserActor::try_deliver` (`user_actor/delivery.rs:56`). **More direct field readers exist outside the actors**: `clustering/local_claims.rs:171`, `resume_bridge.rs:60,164`, `route_bridge/delivery/remote_socket.rs:498`, `pending_delivery/recovery.rs:184`, `interpret/route_to_connection/implementation.rs:1489`, `ws/cleanup.rs:1934`. | DEFprep's replacement-query list is §2.2 (`sm_stream_id`, `occupancy_session` rows) plus every registry method in §1.2 marked DEFprep, including `unregister_if_sm_stream_id` (`DEFprep / D2r`) and its two callers. D2r's own retirement protocol does not cover a janitor or another connection removing a detached entry by SM id. |
| Grounding: carbons, interest, presence and `pending_subscribes_flushed` are "duplicated mutable state" in `WsConnState` and `ConnectionEntry` | The **live** copy differs per field. For presence (`presence_available`/`show`/`status`/`priority`/`payloads`) and `pending_subscribes_flushed`, the `WsConnState` fields are **resume carriers only**: written at resume restore (`stream_management/implementation.rs:1343-1348`), reset at `stream_management/registration.rs:347-352`, and read to republish after resume (`registration.rs:399-416,646`) and for the terminal-recovery snapshot (`cleanup.rs:2284-2289`). That snapshot (`terminal_recovery_snapshot`, `cleanup.rs:2269-2291`) therefore carries the presence restored at resume (or the reset defaults), not the presence the client sent since. The ordinary detach snapshot does use live values. The presence handler never receives them; the live values are the entry atomics and `ConnectionRegistry.presence_states`, written by `regular.rs:70,97` (`update_presence_if_owner`, `update_presence_state_if_owner`) and read by the detach snapshot (`cleanup.rs:930-988`). Only carbons, roster and blocklist interest are written live on both copies: `sans_io.rs:171`, `roster/implementation.rs:112` and `blocking.rs:67` via `IqConnState`, plus the registry/entry setters. | The #2046 target authority model requires one owner: socket-local truth for presence is today on the entry, not the socket task. D2a must move the presence writes into the actor, not just wrap `WsConnState`. D2a/D2r should make the terminal-recovery snapshot read the live values. |
| C6a-pre: "variants with no production producer — `PrivateMessage`, `BareRoomIq`, `OccupantIq`, `FanoutChunk`, to be confirmed by W1.9" | **Confirmed**, and the owner has no consumer for them either (`ParseFailure`, `reserved.rs:73`). Deleting them changes `RemoteStanzaEnvelope`, so it needs a `deliver_ordered.vN` bump (`server/AGENTS.md:23`). | §9. |
| B2c: "carbons/roster/blocklist side effects + `roster_fanout`" | The **presence mirror** `regular.rs:202` (`update_remote_user_resource_if_owner`) is the same remote-resource-update path but is not named in any B2 cluster. | Assigned to B2c here. |
| E2: "presence, priority, carbons, interest" | Two more account-scoped, memory-only authorities live on `ConnectionRegistry`: XEP-0012 last activity (`record_last_activity`/`last_activity`/`clear_last_activity`) and the RFC 6121 pending-subscribe queue (`queue_pending_subscription_stanza` and 3 siblings; `UserActor.pending_subscriptions` duplicates the latter). | Name both in E2, or record them as staying on the registry. |

### 0.4 Limits

- **R/W labels are heuristic.** In §1.3, §2.2 and §3.2 they come from the source text: assignment, `.store`/CAS, `&mut` borrow, or a method name that implies mutation. A method call on a field such as `sm_state` is labelled by its name, so a few may be misclassified. Each row's location is exact.
- **Test counts are textual.** They come from `rg` plus the `cfg(test)` region filter, not from the compiler. The type-resolved pass compiled only the production feature set.
- **One feature set only.** `#[cfg(not(feature = "clustering"))]` code (the default-feature binary) and `test-support` code are excluded. Production builds with `clustering` (`flake.nix:367`).
- **Assignments are proposals.** The B2 cluster column in §8, the "Proposed slice" column in §10 and the D3s verdict in §2.1 are this inventory's proposals against final.md §3. The slices that implement them confirm or amend them.


## 1. `ConnectionRegistry` (`waddle-xmpp/src/registry/connection_registry.rs:41-61`, methods in 10 submodules)

### 1.1 State fields

All 7 fields are private. All 99 type-resolved field accesses are inside `registry/connection_registry*`. The type name itself is referenced at 100 production sites in 33 files: signatures, struct fields and imports.

| Field | Class | What it is | Slice |
|---|---|---|---|
| `connections: Arc<DashMap<FullJid, ConnectionEntry>>` | I | Local delivery index. Written first at bind (`ws/registration.rs:604`, `register_entry`) and read by `entry_if_owner`. Holds local and remote-hosted entries. | Stays as a derived, transport-only socket index after ACT-DEF. Its writers move into the connection actor (D2a/D2r). |
| `bind_slots` | T | Per-FullJid async bind gate plus `SocketLifecycle` incumbent | D2r |
| `pending_subscription_stanzas` | A | RFC 6121 queued inbound subscribes, memory only | E2 (name it; §0.3) |
| `presence_states` | A | show/status/priority/payloads per resource; answers probes | E2 / ACT-DEF |
| `last_activity` | A | XEP-0012 offline last-activity, memory only | E2 (name it; §0.3) |
| `started_at` | T | Process start, XEP-0012 uptime | keep |
| `sm_stream_owners` | I | Reverse index SM id → FullJid, best-effort | DEFprep / ACT-DEF |

### 1.2 Method summary (all 72 `pub` and `pub(super)` methods, incl. `new`)

"Prod sites" counts type-resolved production call sites outside `registry/connection_registry*`. "Internal" counts calls from inside the registry module. A method with 0 prod sites and 0 internal sites is unused in production.

| Method | Defined | Prod sites (files) | Internal | R/W | Class | Removing / keeping slice | Note |
|---|---|---|---|---|---|---|---|
| `new` | `xmpp/registry/connection_registry.rs:65` | 1 (1) | 1 | W | I | keep | construction |
| `set_sm_stream_id` | `xmpp/registry/connection_registry.rs:96` | 0 (0) | 0 | W | I | DEFprep / ACT-DEF | publishes connection-owned SM id + `sm_stream_owners` reverse index |
| `set_occupancy_session_if_owner` | `xmpp/registry/connection_registry.rs:109` | 1 (1) | 0 | W | I | W1.3 (token) / ACT-DEF | owner-gated occupancy publication |
| `occupancy_session_of` | `xmpp/registry/connection_registry.rs:128` | 2 (1) | 0 | R | I | DEFprep | occupancy generation read outside the connection |
| `set_sm_stream_id_if_owner` | `xmpp/registry/connection_registry.rs:140` | 2 (2) | 0 | W | I | W1.3 (token) / ACT-DEF | owner-gated SM id publication |
| `sm_stream_owner` | `xmpp/registry/connection_registry.rs:165` | 3 (2) | 0 | R | I | DEFprep | SM id -> FullJid reverse lookup (best-effort) |
| `pending_subscription_count` | `xmpp/registry/connection_registry.rs:174` | 1 (1) | 0 | R | I | keep (diagnostic) | `/debug/state-inventory` |
| `presence_state_count` | `xmpp/registry/connection_registry.rs:180` | 1 (1) | 0 | R | I | keep (diagnostic) | `/debug/state-inventory` |
| `last_activity_count` | `xmpp/registry/connection_registry.rs:186` | 1 (1) | 0 | R | I | keep (diagnostic) | `/debug/state-inventory` |
| `local_sm_stream` | `xmpp/registry/connection_registry/archive_dispatch.rs:103` | 2 (2) | 0 | R | I | DEFprep | SM id of a locally hosted entry |
| `try_lock_bind` | `xmpp/registry/connection_registry/binding.rs:181` | 1 (1) | 0 | W | T | D2r | per-FullJid bind gate |
| `probe_socket_lifecycle` | `xmpp/registry/connection_registry/binding.rs:195` | 1 (1) | 0 | R | T | D2r | `SocketLifecycle` probe (`CleanupPending` custody) |
| `lock_bind` | `xmpp/registry/connection_registry/binding.rs:218` | 1 (1) | 0 | W | T | D2r | per-FullJid bind gate |
| `prune_completed_bind` | `xmpp/registry/connection_registry/binding.rs:231` | 2 (2) | 0 | W | T | D2r | bind gate cleanup |
| `queue_pending_subscription_stanza` | `xmpp/registry/connection_registry/subscriptions.rs:7` | 3 (1) | 0 | W | A | E2 | RFC 6121 queued inbound subscribe (memory only) |
| `remove_pending_subscribe` | `xmpp/registry/connection_registry/subscriptions.rs:33` | 3 (1) | 0 | W | A | E2 |  |
| `drain_pending_subscription_stanzas` | `xmpp/registry/connection_registry/subscriptions.rs:53` | 0 (0) | 0 | W | A | E2 |  |
| `pending_subscription_stanzas` | `xmpp/registry/connection_registry/subscriptions.rs:66` | 1 (1) | 0 | R | A | E2 |  |
| `register` | `xmpp/registry/connection_registry/connections.rs:17` | 0 (0) | 0 | W | I | W1.3 (token) / D2a | installs a fresh entry; returns carbons handle as owner token |
| `register_with_carbons` | `xmpp/registry/connection_registry/connections.rs:26` | 0 (0) | 1 | W | I | W1.3 (token) / D2a |  |
| `register_with_stream_state` | `xmpp/registry/connection_registry/connections.rs:37` | 0 (0) | 1 | W | I | W1.3 (token) / D2a |  |
| `register_entry` | `xmpp/registry/connection_registry/connections.rs:66` | 1 (1) | 1 | W | I | W1.3 (token) / D2a | socket bind install (DashMap first, actor mirror second) |
| `register_entry_if_owner_or_absent` | `xmpp/registry/connection_registry/connections.rs:87` | 1 (1) | 0 | W | I | W1.3 (token) / D2a |  |
| `unregister` | `xmpp/registry/connection_registry/connections.rs:123` | 0 (0) | 1 | W | I | D2r | unconditional removal |
| `is_owned_by` | `xmpp/registry/connection_registry/connections.rs:142` | 5 (4) | 0 | R | I | W1.3 | `Arc::ptr_eq` on the carbons flag |
| `unregister_if_owner` | `xmpp/registry/connection_registry/connections.rs:151` | 18 (7) | 0 | W | I | W1.3 (token) / D2r |  |
| `unregister_if_sm_stream_id` | `xmpp/registry/connection_registry/connections.rs:184` | 2 (2) | 0 | W | I | DEFprep / D2r (SM-id read outside the owner) | reads `entry.sm_stream_id()` in `remove_if`; callers are the SM-expiry janitor and another connection's bind/resume path |
| `entry_if_owner` | `xmpp/registry/connection_registry/connections.rs:207` | 29 (11) | 0 | R | I | W1.3 | `Arc::ptr_eq` on the carbons flag |
| `is_connected` | `xmpp/registry/connection_registry/connections.rs:226` | 0 (0) | 0 | R | I | ACT-DEF (derived socket index) |  |
| `is_locally_hosted` | `xmpp/registry/connection_registry/connections.rs:232` | 4 (2) | 0 | R | I | ACT-DEF (derived socket index) | local vs remote-hosted mirror |
| `entry` | `xmpp/registry/connection_registry/connections.rs:243` | 25 (13) | 0 | R | I | ACT-DEF (derived socket index) | clone of the whole entry (sender + shared atomics) |
| `connection_count` | `xmpp/registry/connection_registry/connections.rs:248` | 1 (1) | 0 | R | I | keep (diagnostic) | metrics / inventory |
| `mark_roster_interested` | `xmpp/registry/connection_registry/resources.rs:10` | 1 (1) | 0 | W | A | E2 / ACT-DEF | interest flag written by the connection |
| `is_roster_interested` | `xmpp/registry/connection_registry/resources.rs:20` | 1 (1) | 0 | R | A | E2 / ACT-DEF | selection read |
| `roster_interested_resources_for_user` | `xmpp/registry/connection_registry/resources.rs:28` | 4 (3) | 0 | R | A | E2 / ACT-DEF | selection read (roster push audience) |
| `mark_blocklist_interested` | `xmpp/registry/connection_registry/resources.rs:43` | 1 (1) | 0 | W | A | E2 / ACT-DEF | interest flag written by the connection |
| `is_blocklist_interested` | `xmpp/registry/connection_registry/resources.rs:53` | 0 (0) | 0 | R | A | E2 / ACT-DEF | selection read |
| `blocklist_interested_resources_for_user` | `xmpp/registry/connection_registry/resources.rs:61` | 1 (1) | 0 | R | A | E2 / ACT-DEF | selection read (blocklist push audience) |
| `send_to_many` | `xmpp/registry/connection_registry/resources.rs:75` | 0 (0) | 0 | W | T | delete (no prod caller) | awaits capacity |
| `list_connections` | `xmpp/registry/connection_registry/resources.rs:96` | 4 (3) | 0 | R | I | ACT-DEF (derived socket index) |  |
| `active_sm_stream_ids` | `xmpp/registry/connection_registry/resources.rs:108` | 3 (2) | 0 | R | I | DEFprep | janitor inventory of live SM ids |
| `other_resources_for_user` | `xmpp/registry/connection_registry/resources.rs:121` | 0 (0) | 0 | R | A | E2 / ACT-DEF | selection read |
| `other_carbon_resources_for_user` | `xmpp/registry/connection_registry/resources.rs:145` | 2 (1) | 0 | R | A | E2 / ACT-DEF | carbons audience |
| `is_carbons_enabled` | `xmpp/registry/connection_registry/resources.rs:165` | 0 (0) | 0 | R | A | E2 / ACT-DEF |  |
| `set_carbons_enabled` | `xmpp/registry/connection_registry/resources.rs:175` | 1 (1) | 0 | W | A | E2 / ACT-DEF | mutates the ownership token object too (W1.3) |
| `update_presence` | `xmpp/registry/connection_registry/presence.rs:11` | 0 (0) | 0 | W | A | E2 / ACT-DEF |  |
| `update_presence_if_owner` | `xmpp/registry/connection_registry/presence.rs:33` | 3 (3) | 0 | W | A | E2 / ACT-DEF |  |
| `update_presence_state_if_owner` | `xmpp/registry/connection_registry/presence.rs:66` | 3 (3) | 0 | W | A | E2 / ACT-DEF |  |
| `update_presence_state` | `xmpp/registry/connection_registry/presence.rs:102` | 0 (0) | 0 | W | A | E2 / ACT-DEF | show/status/priority/payloads for probes |
| `presence_state` | `xmpp/registry/connection_registry/presence.rs:122` | 7 (5) | 0 | R | A | E2 / ACT-DEF | probe answer source |
| `clear_presence_state` | `xmpp/registry/connection_registry/presence.rs:127` | 0 (0) | 0 | W | A | E2 / ACT-DEF |  |
| `clear_presence_state_if_owner` | `xmpp/registry/connection_registry/presence.rs:139` | 2 (2) | 0 | W | A | E2 / ACT-DEF |  |
| `record_last_activity` | `xmpp/registry/connection_registry/presence.rs:151` | 1 (1) | 0 | W | A | E2 (not named in E2 row) | XEP-0012, memory only |
| `last_activity` | `xmpp/registry/connection_registry/presence.rs:162` | 1 (1) | 0 | R | A | E2 (not named in E2 row) |  |
| `clear_last_activity` | `xmpp/registry/connection_registry/presence.rs:169` | 1 (1) | 0 | W | A | E2 (not named in E2 row) |  |
| `server_uptime_seconds` | `xmpp/registry/connection_registry/presence.rs:174` | 2 (1) | 0 | R | T | keep (node fact, no plan slice) | process start time stored on the registry |
| `available_resources_for_user` | `xmpp/registry/connection_registry/presence.rs:179` | 11 (7) | 0 | R | A | E2 / ACT-DEF | selection read (available + priority) |
| `cleanup_stale` | `xmpp/registry/connection_registry/presence.rs:193` | 0 (0) | 0 | W | I | delete (no prod caller) | closed-sender sweep |
| `reserve_pending_flush` | `xmpp/registry/connection_registry/sending.rs:30` | 1 (1) | 0 | W | T | D2p | awaits capacity |
| `try_reserve_pending_flush` | `xmpp/registry/connection_registry/sending.rs:60` | 1 (1) | 0 | W | T | keep (already non-blocking) |  |
| `send_reserved_pending_flush` | `xmpp/registry/connection_registry/sending.rs:92` | 1 (1) | 0 | W | T | keep (already non-blocking) |  |
| `send_to_with_write_acceptance` | `xmpp/registry/connection_registry/sending.rs:125` | 1 (1) | 0 | W | T | per caller (§10) | awaits capacity |
| `send_to` | `xmpp/registry/connection_registry/sending.rs:179` | 36 (18) | 1 | W | T | per caller (§10) | awaits capacity |
| `send_to_if_owner` | `xmpp/registry/connection_registry/sending.rs:230` | 2 (2) | 0 | W | T | per caller (§10) | awaits capacity |
| `send_pending_flush` | `xmpp/registry/connection_registry/sending.rs:260` | 1 (1) | 0 | W | T | D2p | awaits capacity |
| `send_pending_flush_if_owner` | `xmpp/registry/connection_registry/sending.rs:300` | 1 (1) | 0 | W | T | D2p | awaits capacity |
| `try_send_to` | `xmpp/registry/connection_registry/sending.rs:349` | 16 (8) | 0 | W | T | keep (already non-blocking) |  |
| `try_send_outbound_to_locally_hosted` | `xmpp/registry/connection_registry/sending.rs:355` | 1 (1) | 0 | W | T | keep (already non-blocking) |  |
| `try_send_outbound_if_owner` | `xmpp/registry/connection_registry/sending.rs:429` | 5 (3) | 1 | W | T | W1.3 (token) | non-blocking |
| `capture_owned_enqueue_if_owner` | `xmpp/registry/connection_registry/sending.rs:473` | 1 (1) | 0 | W | T | W1.3 (token) | non-blocking |
| `remove_if_sender_closed` | `xmpp/registry/connection_registry/sending.rs:522` | 0 (0) | 1 | W | I | ACT-DEF | evicts closed sender |
| `remove_if_sender_closed_owner` | `xmpp/registry/connection_registry/sending.rs:543` | 0 (0) | 11 | W | I | ACT-DEF | evicts closed sender |

Methods with **no production caller outside the registry module**: `set_sm_stream_id`, `drain_pending_subscription_stanzas`, `register`, `register_with_carbons` (internal only, 1), `register_with_stream_state` (internal only, 1), `unregister` (internal only, 1), `is_connected`, `is_blocklist_interested`, `send_to_many`, `other_resources_for_user`, `is_carbons_enabled`, `update_presence`, `update_presence_state`, `clear_presence_state`, `cleanup_stale`, `remove_if_sender_closed` (internal only, 1), `remove_if_sender_closed_owner` (internal only, 11).

### 1.3 Production call sites outside the registry module

Grouped by file. The class and slice come from the method (§1.2) unless the row says otherwise. The slice for awaiting producers (`send_to*`, `send_pending_flush*`, `reserve_pending_flush`) is the per-caller assignment from §10.

228 call sites in 63 files.

| File | Line | Enclosing fn | Method | R/W | Class | Slice |
|---|---|---|---|---|---|---|
| `clustering/local_claims.rs` | 169 | `demote_stream` | `sm_stream_owner` | R | I | DEFprep |
| `clustering/local_claims.rs` | 170 | `demote_stream` | `entry` | R | I | ACT-DEF (derived socket index) |
| `clustering/local_claims.rs` | 742 | `connection_registry_resources` | `list_connections` | R | I | ACT-DEF (derived socket index) |
| `clustering/local_claims.rs` | 776 | `force_detach_resources` | `entry` | R | I | ACT-DEF (derived socket index) |
| `clustering/local_claims.rs` | 875 | `force_detach_entries` | `unregister_if_owner` | W | I | W1.3 (token) / D2r |
| `clustering/local_claims.rs` | 937 | `terminal_sweep` | `list_connections` | R | I | ACT-DEF (derived socket index) |
| `clustering/resume_bridge.rs` | 54 | `request_validated_local_resume` | `sm_stream_owner` | R | I | DEFprep |
| `clustering/resume_bridge.rs` | 57 | `request_validated_local_resume` | `entry` | R | I | ACT-DEF (derived socket index) |
| `clustering/resume_bridge.rs` | 155 | `request_local_forced_detach` | `sm_stream_owner` | R | I | DEFprep |
| `clustering/resume_bridge.rs` | 158 | `request_local_forced_detach` | `entry` | R | I | ACT-DEF (derived socket index) |
| `clustering/route_bridge/delivery/remote.rs` | 518 | `refresh_remote_resource_origin` | `entry_if_owner` | R | I | W1.3 |
| `clustering/route_bridge/delivery/remote_socket.rs` | 492 | `deliver_remote_resource_frame_on_socket` | `entry_if_owner` | R | I | W1.3 |
| `clustering/route_bridge/delivery/remote_socket.rs` | 547 | `deliver_remote_resource_frame_on_socket` | `try_send_outbound_if_owner` | W | T | W1.3 (token) |
| `clustering/route_bridge/delivery/remote_socket.rs` | 564 | `deliver_remote_resource_frame_on_socket` | `try_send_outbound_if_owner` | W | T | W1.3 (token) |
| `clustering/route_bridge/delivery/remote_socket.rs` | 609 | `deliver_remote_resource_write_accepted_frame_on_socket` | `try_send_outbound_if_owner` | W | T | W1.3 (token) |
| `clustering/route_bridge/presence.rs` | 49 | `resource_presence_local` | `is_locally_hosted` | R | I | ACT-DEF (derived socket index) |
| `clustering/route_bridge/presence.rs` | 67 | `resource_presence_local` | `is_locally_hosted` | R | I | ACT-DEF (derived socket index) |
| `clustering/route_bridge/registration/owner.rs` | 222 | `register_remote_user_resource_on_owner_locked` | `try_lock_bind` | W | T | D2r |
| `clustering/route_bridge/registration/owner.rs` | 262 | `register_remote_user_resource_on_owner_locked` | `is_owned_by` | R | I | W1.3 |
| `clustering/route_bridge/registration/owner.rs` | 425 | `register_remote_user_resource_on_owner_locked` | `register_entry_if_owner_or_absent` | W | I | W1.3 (token) / D2a |
| `clustering/route_bridge/registration/owner.rs` | 454 | `register_remote_user_resource_on_owner_locked` | `unregister_if_owner` | W | I | W1.3 (token) / D2r |
| `clustering/route_bridge/registration/owner.rs` | 465 | `register_remote_user_resource_on_owner_locked` | `unregister_if_owner` | W | I | W1.3 (token) / D2r |
| `clustering/route_bridge/registration/owner.rs` | 579 | `unregister_remote_user_resource_on_owner` | `unregister_if_owner` | W | I | W1.3 (token) / D2r |
| `clustering/route_bridge/registration/owner_retire.rs` | 98 | `schedule_local_incumbent_retirement` | `unregister_if_owner` | W | I | W1.3 (token) / D2r |
| `clustering/route_bridge/registration/owner_retire.rs` | 271 | `finish_remote_owner_registration_retire` | `unregister_if_owner` | W | I | W1.3 (token) / D2r |
| `clustering/route_bridge/registration/owner_retire.rs` | 303 | `finish_remote_owner_registration_retire` | `unregister_if_owner` | W | I | W1.3 (token) / D2r |
| `clustering/route_bridge/registration/owner_retire.rs` | 333 | `cleanup_remote_owner_resource_if_registration` | `unregister_if_owner` | W | I | W1.3 (token) / D2r |
| `clustering/route_bridge/registration/owner_sweep.rs` | 268 | `retire_remote_owner_mirror` | `unregister_if_owner` | W | I | W1.3 (token) / D2r |
| `clustering/route_bridge/registration/owner_update.rs` | 162 | `owner_remote_entry_if_current` | `entry_if_owner` | R | I | W1.3 |
| `clustering/route_bridge/registration/side_effects.rs` | 427 | `apply_remote_resource_presence_to_registry` | `update_presence_if_owner` | W | A | E2 / ACT-DEF |
| `clustering/route_bridge/registration/side_effects.rs` | 437 | `apply_remote_resource_presence_to_registry` | `update_presence_state_if_owner` | W | A | E2 / ACT-DEF |
| `clustering/route_bridge/registration/side_effects.rs` | 446 | `apply_remote_resource_presence_to_registry` | `clear_presence_state_if_owner` | W | A | E2 / ACT-DEF |
| `clustering/route_bridge/registration/socket.rs` | 253 | `try_register_remote_user_resource` | `presence_state` | R | A | E2 / ACT-DEF |
| `clustering/route_bridge/registration/socket.rs` | 322 | `try_register_remote_user_resource` | `entry_if_owner` | R | I | W1.3 |
| `clustering/route_bridge/registration/socket.rs` | 803 | `remote_socket_registration_if_current` | `entry_if_owner` | R | I | W1.3 |
| `clustering/route_bridge/registration/socket.rs` | 908 | `run_remote_state_resync_attempt` | `entry_if_owner` | R | I | W1.3 |
| `clustering/route_bridge/registration/socket.rs` | 935 | `run_remote_state_resync_attempt` | `entry_if_owner` | R | I | W1.3 |
| `clustering/route_bridge/registration/socket.rs` | 964 | `run_remote_state_resync_attempt` | `entry_if_owner` | R | I | W1.3 |
| `clustering/route_bridge/registration/socket.rs` | 1041 | `run_remote_state_resync_attempt` | `presence_state` | R | A | E2 / ACT-DEF |
| `clustering/route_bridge/registration/socket.rs` | 1104 | `run_remote_state_resync_attempt` | `presence_state` | R | A | E2 / ACT-DEF |
| `clustering/route_bridge/registration/socket.rs` | 1227 | `force_detach_remote_user_resource_on_socket` | `entry_if_owner` | R | I | W1.3 |
| `clustering/route_bridge/registration/socket.rs` | 1310 | `complete_fresh_remote_retirement` | `entry` | R | I | ACT-DEF (derived socket index) |
| `clustering/route_bridge/registration/socket.rs` | 1317 | `complete_fresh_remote_retirement` | `probe_socket_lifecycle` | R | T | D2r |
| `interpret/bot.rs` | 755 | `route_join_presences` | `entry` | R | I | ACT-DEF (derived socket index) |
| `interpret/carbons.rs` | 77 | `send_carbons` | `other_carbon_resources_for_user` | R | A | E2 / ACT-DEF |
| `interpret/carbons.rs` | 278 | `send_carbons_to_registry_with_capture` | `other_carbon_resources_for_user` | R | A | E2 / ACT-DEF |
| `interpret/carbons.rs` | 420 | `send_carbons_to_registry_with_capture` | `send_to` | W | T | D2c |
| `interpret/effects/delivery_immediate.rs` | 56 | `execute_with_received_at` | `entry` | R | I | ACT-DEF (derived socket index) |
| `interpret/effects/delivery_immediate.rs` | 58 | `execute_with_received_at` | `try_send_outbound_if_owner` | W | T | W1.3 (token) |
| `interpret/effects/invite.rs` | 55 | `execute` | `send_to` | W | T | D2b |
| `interpret/groupchat_archive.rs` | 1080 | `push_inbox_update_to` | `send_to` | W | T | D2e |
| `interpret/offline_delivery.rs` | 203 | `bounce_offline_quota` | `send_to` | W | T | D2e |
| `interpret/route_to_connection/implementation.rs` | 1432 | `deliver_direct_to_full_locally` | `try_send_outbound_to_locally_hosted` | W | T | keep (already non-blocking) |
| `interpret/route_to_connection/implementation.rs` | 1468 | `deliver_ordered_local_copy` | `entry` | R | I | ACT-DEF (derived socket index) |
| `interpret/route_to_connection/implementation.rs` | 1530 | `deliver_ordered_local_copy` | `capture_owned_enqueue_if_owner` | W | T | W1.3 (token) |
| `server/extension_host_adapter/queries.rs` | 159 | `presence` | `available_resources_for_user` | R | A | E2 / ACT-DEF |
| `server/extension_host_adapter/queries.rs` | 160 | `presence` | `presence_state` | R | A | E2 / ACT-DEF |
| `server/http.rs` | 691 | `create_websocket_state` | `new` | W | I | keep |
| `server/routes/muc_muji_clear.rs` | 367 | `broadcast_muji_clear` | `try_send_to` | W | T | keep (already non-blocking) |
| `server/session_janitors/implementation.rs` | 476 | `run_local_muc_departure_sweep` | `entry` | R | I | ACT-DEF (derived socket index) |
| `server/session_janitors/implementation.rs` | 1613 | `run_sm_expiry_sweep_with_custody_cursor` | `unregister_if_sm_stream_id` | W | I | DEFprep / D2r (SM-id read outside the owner) |
| `server/session_janitors/implementation.rs` | 4145 | `run_pending_delivery_claim_sweep` | `active_sm_stream_ids` | R | I | DEFprep |
| `server/session_janitors/implementation.rs` | 6272 | `collect_remote_muc_reconcile_candidates` | `occupancy_session_of` | R | I | DEFprep |
| `server/session_janitors/implementation.rs` | 6375 | `run_remote_muc_membership_sweep` | `occupancy_session_of` | R | I | DEFprep |
| `server/session_janitors/ingress_custody.rs` | 77 | `drain_page` | `active_sm_stream_ids` | R | I | DEFprep |
| `server/session_janitors/ingress_custody.rs` | 103 | `drain_page` | `active_sm_stream_ids` | R | I | DEFprep |
| `server/state_inventory.rs` | 171 | `collect_snapshot` | `connection_count` | R | I | keep (diagnostic) |
| `server/state_inventory.rs` | 172 | `collect_snapshot` | `pending_subscription_count` | R | I | keep (diagnostic) |
| `server/state_inventory.rs` | 173 | `collect_snapshot` | `presence_state_count` | R | I | keep (diagnostic) |
| `server/state_inventory.rs` | 174 | `collect_snapshot` | `last_activity_count` | R | I | keep (diagnostic) |
| `srv/admin/channels.rs` | 2820 | `broadcast_group_dm_leave` | `try_send_to` | W | T | keep (already non-blocking) |
| `srv/admin/channels.rs` | 2833 | `broadcast_group_dm_leave` | `try_send_to` | W | T | keep (already non-blocking) |
| `srv/admin/channels.rs` | 2845 | `broadcast_group_dm_config_change` | `try_send_to` | W | T | keep (already non-blocking) |
| `srv/admin/channels.rs` | 2979 | `broadcast_admin_config_change` | `send_to` | W | T | D2d |
| `srv/admin/channels.rs` | 3440 | `broadcast_presence_updates` | `send_to` | W | T | D2d |
| `srv/admin/channels.rs` | 5327 | `run_kick` | `send_to` | W | T | D2d |
| `srv/ingress/archive_dispatch.rs` | 288 | `effect_ready` | `local_sm_stream` | R | I | DEFprep |
| `srv/ingress/execute_detached.rs` | 121 | `execute` | `local_sm_stream` | R | I | DEFprep |
| `srv/ingress/execute_relay_copy.rs` | 33 | `attempt_before_readiness` | `entry` | R | I | ACT-DEF (derived socket index) |
| `srv/ingress/execute_relay_copy.rs` | 144 | `append_sealed_legacy` | `entry` | R | I | ACT-DEF (derived socket index) |
| `srv/ingress/recovery_reachability.rs` | 67 | `locally_reachable` | `is_locally_hosted` | R | I | ACT-DEF (derived socket index) |
| `srv/ingress/recovery_reachability.rs` | 89 | `locally_reachable` | `is_locally_hosted` | R | I | ACT-DEF (derived socket index) |
| `srv/pending_delivery/flush.rs` | 191 | `resume_flush_for_resource_with_retry` | `entry_if_owner` | R | I | W1.3 |
| `srv/pending_delivery/flush.rs` | 619 | `flush_for_resource_inner` | `try_reserve_pending_flush` | W | T | keep (already non-blocking) |
| `srv/pending_delivery/flush.rs` | 621 | `flush_for_resource_inner` | `reserve_pending_flush` | W | T | D2p |
| `srv/pending_delivery/flush.rs` | 701 | `flush_for_resource_inner` | `send_pending_flush_if_owner` | W | T | D2p |
| `srv/pending_delivery/flush.rs` | 712 | `flush_for_resource_inner` | `send_pending_flush` | W | T | D2p |
| `srv/pending_delivery/flush.rs` | 728 | `flush_for_resource_inner` | `send_to_if_owner` | W | T | D2p |
| `srv/pending_delivery/flush.rs` | 729 | `flush_for_resource_inner` | `send_to` | W | T | D2p |
| `srv/pending_delivery/flush.rs` | 804 | `flush_recovery_pass` | `entry_if_owner` | R | I | W1.3 |
| `srv/pending_delivery/flush.rs` | 853 | `attempt_offer` | `send_reserved_pending_flush` | W | T | keep (already non-blocking) |
| `srv/pending_delivery/recovery.rs` | 32 | `is_current` | `entry_if_owner` | R | I | W1.3 |
| `srv/pending_delivery/recovery.rs` | 171 | `tick` | `list_connections` | R | I | ACT-DEF (derived socket index) |
| `srv/pending_delivery/recovery.rs` | 172 | `tick` | `entry` | R | I | ACT-DEF (derived socket index) |
| `srv/room_effect_outbox/drain.rs` | 608 | `queue_local` | `send_to_with_write_acceptance` | W | T | D2p |
| `srv/sm_promotion/implementation.rs` | 1234 | `promote_one` | `send_to` | W | T | D2p |
| `srv/sm_promotion/live.rs` | 29 | `build_online_resources` | `entry` | R | I | ACT-DEF (derived socket index) |
| `srv/sm_promotion/live.rs` | 70 | `collect_live_targets` | `entry` | R | I | ACT-DEF (derived socket index) |
| `srv/sm_promotion/pending.rs` | 204 | `return_quota_bounce` | `send_to` | W | T | D2p |
| `srv/sm_promotion/stanza.rs` | 40 | `promote_iq` | `entry` | R | I | ACT-DEF (derived socket index) |
| `srv/sm_promotion/stanza.rs` | 43 | `promote_iq` | `send_to` | W | T | D2p |
| `srv/sm_promotion/stanza.rs` | 63 | `promote_presence` | `entry` | R | I | ACT-DEF (derived socket index) |
| `srv/sm_promotion/stanza.rs` | 66 | `promote_presence` | `send_to` | W | T | D2p |
| `ws/cleanup.rs` | 908 | `cleanup_connection_shutdown_inner` | `presence_state` | R | A | E2 / ACT-DEF |
| `ws/cleanup.rs` | 913 | `cleanup_connection_shutdown_inner` | `entry_if_owner` | R | I | W1.3 |
| `ws/cleanup.rs` | 1127 | `cleanup_connection_shutdown_inner` | `unregister_if_owner` | W | I | W1.3 (token) / D2r |
| `ws/cleanup.rs` | 1347 | `cleanup_connection_shutdown_inner` | `unregister_if_owner` | W | I | W1.3 (token) / D2r |
| `ws/cleanup.rs` | 1490 | `cleanup_connection_shutdown_inner` | `entry` | R | I | ACT-DEF (derived socket index) |
| `ws/cleanup.rs` | 1515 | `cleanup_connection_shutdown_inner` | `unregister_if_owner` | W | I | W1.3 (token) / D2r |
| `ws/cleanup.rs` | 1758 | `finish_socket_cleanup` | `prune_completed_bind` | W | T | D2r |
| `ws/cleanup.rs` | 1842 | `promote_terminal_recovery_inner` | `unregister_if_owner` | W | I | W1.3 (token) / D2r |
| `ws/cleanup.rs` | 1905 | `preferred_live_pending_flush_target` | `available_resources_for_user` | R | A | E2 / ACT-DEF |
| `ws/cleanup.rs` | 1918 | `preferred_live_pending_flush_target` | `entry` | R | I | ACT-DEF (derived socket index) |
| `ws/cleanup.rs` | 2016 | `redrive_terminal_pending_rows_to_live_resource_with_cancel` | `available_resources_for_user` | R | A | E2 / ACT-DEF |
| `ws/cleanup.rs` | 2019 | `redrive_terminal_pending_rows_to_live_resource_with_cancel` | `entry` | R | I | ACT-DEF (derived socket index) |
| `ws/cleanup.rs` | 2049 | `redrive_terminal_pending_rows_to_live_resource_with_cancel` | `entry_if_owner` | R | I | W1.3 |
| `ws/cleanup.rs` | 2084 | `redrive_terminal_pending_rows_to_live_resource_with_cancel` | `entry_if_owner` | R | I | W1.3 |
| `ws/cleanup.rs` | 2119 | `redrive_terminal_pending_rows_to_live_resource_with_cancel` | `entry_if_owner` | R | I | W1.3 |
| `ws/cleanup.rs` | 2193 | `spawn_terminal_pending_ordering_retry` | `entry_if_owner` | R | I | W1.3 |
| `ws/cleanup.rs` | 2211 | `spawn_terminal_pending_ordering_retry` | `entry_if_owner` | R | I | W1.3 |
| `ws/cleanup.rs` | 2628 | `refuse_detach_without_principal` | `unregister_if_owner` | W | I | W1.3 (token) / D2r |
| `ws/cleanup.rs` | 2737 | `refuse_detach_without_principal` | `entry` | R | I | ACT-DEF (derived socket index) |
| `ws/cleanup.rs` | 2824 | `broadcast_unavailable_if_no_replacement` | `entry` | R | I | ACT-DEF (derived socket index) |
| `ws/cleanup.rs` | 3847 | `cleanup_invalidated_detached_session` | `entry_if_owner` | R | I | W1.3 |
| `ws/cleanup.rs` | 3865 | `cleanup_invalidated_detached_session` | `unregister_if_sm_stream_id` | W | I | DEFprep / D2r (SM-id read outside the owner) |
| `ws/cleanup.rs` | 3890 | `cleanup_invalidated_detached_session` | `entry` | R | I | ACT-DEF (derived socket index) |
| `ws/connection.rs` | 828 | `finish_connection` | `entry_if_owner` | R | I | W1.3 |
| `ws/connection.rs` | 1570 | `handle_inbound_text` | `entry_if_owner` | R | I | W1.3 |
| `ws/frame.rs` | 548 | `handle_xmpp_frame_impl` | `is_owned_by` | R | I | W1.3 |
| `ws/frame.rs` | 1139 | `dispatch_authoritative_message` | `is_owned_by` | R | I | W1.3 |
| `ws/handlers/iq/blocking.rs` | 63 | `handle_blocking_iq` | `mark_blocklist_interested` | W | A | E2 / ACT-DEF |
| `ws/handlers/iq/blocking.rs` | 383 | `send_blocking_pushes` | `blocklist_interested_resources_for_user` | R | A | E2 / ACT-DEF |
| `ws/handlers/iq/blocking.rs` | 407 | `send_blocking_pushes` | `send_to` | W | T | D2c |
| `ws/handlers/iq/full_jid_forward.rs` | 101 | `route_full_jid_iq` | `send_to` | W | T | D2d |
| `ws/handlers/iq/last_activity.rs` | 32 | `handle_last_activity_iq` | `server_uptime_seconds` | R | T | keep (node fact, no plan slice) |
| `ws/handlers/iq/last_activity.rs` | 45 | `handle_last_activity_iq` | `server_uptime_seconds` | R | T | keep (node fact, no plan slice) |
| `ws/handlers/iq/last_activity.rs` | 103 | `handle_last_activity_iq` | `available_resources_for_user` | R | A | E2 / ACT-DEF |
| `ws/handlers/iq/last_activity.rs` | 113 | `handle_last_activity_iq` | `last_activity` | R | A | E2 (not named in E2 row) |
| `ws/handlers/iq/muc_admin.rs` | 1846 | `handle_muc_admin_iq` | `send_to` | W | T | D2b |
| `ws/handlers/iq/muc_admin.rs` | 1870 | `handle_muc_admin_iq` | `send_to` | W | T | D2b |
| `ws/handlers/iq/muc_owner_config.rs` | 428 | `run` | `try_send_to` | W | T | keep (already non-blocking) |
| `ws/handlers/iq/muc_owner_config.rs` | 452 | `run` | `try_send_to` | W | T | keep (already non-blocking) |
| `ws/handlers/iq/muc_owner_config.rs` | 856 | `apply_muc_owner_config` | `try_send_to` | W | T | keep (already non-blocking) |
| `ws/handlers/iq/muc_owner_config.rs` | 979 | `apply_muc_owner_config` | `try_send_to` | W | T | keep (already non-blocking) |
| `ws/handlers/iq/muc_owner_config.rs` | 1070 | `drain_or_broadcast_config_change` | `try_send_to` | W | T | keep (already non-blocking) |
| `ws/handlers/iq/muc_owner_moderation.rs` | 543 | `complete_destroy_snapshot` | `try_send_to` | W | T | keep (already non-blocking) |
| `ws/handlers/iq/muc_owner_moderation.rs` | 1955 | `handle_muc_owner_and_moderation_iq` | `try_send_to` | W | T | keep (already non-blocking) |
| `ws/handlers/iq/roster/implementation.rs` | 108 | `handle_roster_get` | `mark_roster_interested` | W | A | E2 / ACT-DEF |
| `ws/handlers/iq/roster/implementation.rs` | 314 | `handle_roster_set` | `is_roster_interested` | R | A | E2 / ACT-DEF |
| `ws/handlers/iq/roster/push.rs` | 104 | `send_roster_remove_subscription_stanza` | `roster_interested_resources_for_user` | R | A | E2 / ACT-DEF |
| `ws/handlers/iq/roster/push.rs` | 115 | `send_roster_remove_subscription_stanza` | `try_send_to` | W | T | keep (already non-blocking) |
| `ws/handlers/iq/roster/push.rs` | 157 | `send_roster_remove_subscription_stanza` | `try_send_to` | W | T | keep (already non-blocking) |
| `ws/handlers/iq/sans_io.rs` | 176 | `handle_sans_io_iq_with_relay_override` | `set_carbons_enabled` | W | A | E2 / ACT-DEF |
| `ws/handlers/message/dm_pin.rs` | 369 | `fanout_dm_pin_event` | `list_connections` | R | I | ACT-DEF (derived socket index) |
| `ws/handlers/presence/implementation.rs` | 262 | `handle_presence_impl` | `is_owned_by` | R | I | W1.3 |
| `ws/handlers/presence/muc/implementation.rs` | 81 | `join_connection_still_owns_slot` | `is_owned_by` | R | I | W1.3 |
| `ws/handlers/presence/muc/implementation.rs` | 453 | `deliver_room_presence_locally` | `try_send_to` | W | T | keep (already non-blocking) |
| `ws/handlers/presence/probe.rs` | 29 | `handle_presence_probe` | `available_resources_for_user` | R | A | E2 / ACT-DEF |
| `ws/handlers/presence/probe.rs` | 80 | `handle_presence_probe` | `send_to` | W | T | D2c |
| `ws/handlers/presence/probe.rs` | 116 | `handle_presence_probe` | `send_to` | W | T | D2c |
| `ws/handlers/presence/probe.rs` | 125 | `handle_presence_probe` | `presence_state` | R | A | E2 / ACT-DEF |
| `ws/handlers/presence/probe.rs` | 164 | `handle_presence_probe` | `send_to` | W | T | D2c |
| `ws/handlers/presence/probe.rs` | 200 | `send_unsubscribed_probe_response` | `send_to` | W | T | D2c |
| `ws/handlers/presence/regular.rs` | 70 | `handle_regular_presence_update` | `update_presence_if_owner` | W | A | E2 / ACT-DEF |
| `ws/handlers/presence/regular.rs` | 79 | `handle_regular_presence_update` | `clear_last_activity` | W | A | E2 (not named in E2 row) |
| `ws/handlers/presence/regular.rs` | 97 | `handle_regular_presence_update` | `update_presence_state_if_owner` | W | A | E2 / ACT-DEF |
| `ws/handlers/presence/regular.rs` | 133 | `handle_regular_presence_update` | `entry_if_owner` | R | I | W1.3 |
| `ws/handlers/presence/regular.rs` | 140 | `handle_regular_presence_update` | `pending_subscription_stanzas` | R | A | E2 |
| `ws/handlers/presence/regular.rs` | 166 | `handle_regular_presence_update` | `clear_presence_state_if_owner` | W | A | E2 / ACT-DEF |
| `ws/handlers/presence/regular.rs` | 172 | `handle_regular_presence_update` | `record_last_activity` | W | A | E2 (not named in E2 row) |
| `ws/handlers/presence/regular.rs` | 282 | `plan_offline_flush` | `entry_if_owner` | R | I | W1.3 |
| `ws/handlers/presence/regular.rs` | 344 | `spawn_session_recovery_delivery` | `send_to_if_owner` | W | T | D2p |
| `ws/handlers/presence/regular.rs` | 346 | `spawn_session_recovery_delivery` | `entry_if_owner` | R | I | W1.3 |
| `ws/handlers/presence/regular.rs` | 389 | `spawn_session_recovery_delivery` | `entry_if_owner` | R | I | W1.3 |
| `ws/handlers/presence/regular.rs` | 458 | `broadcast_presence_to_subscribers` | `available_resources_for_user` | R | A | E2 / ACT-DEF |
| `ws/handlers/presence/regular.rs` | 466 | `broadcast_presence_to_subscribers` | `send_to` | W | T | D2c |
| `ws/handlers/presence/regular.rs` | 547 | `resolve_caps_for_presence` | `send_to` | W | T | D2c |
| `ws/handlers/presence/subscription/delivery.rs` | 66 | `send_existing_subscription_ack` | `send_to` | W | T | D2c |
| `ws/handlers/presence/subscription/delivery.rs` | 99 | `send_existing_subscription_ack` | `send_to` | W | T | D2c |
| `ws/handlers/presence/subscription/delivery.rs` | 250 | `send_presence_stanza_to_jid` | `send_to` | W | T | D2c |
| `ws/handlers/presence/subscription/delivery.rs` | 354 | `available_live_and_detached_resources_for_user` | `available_resources_for_user` | R | A | E2 / ACT-DEF |
| `ws/handlers/presence/subscription/delivery.rs` | 387 | `presence_state_for_available_resource` | `presence_state` | R | A | E2 / ACT-DEF |
| `ws/handlers/presence/subscription/delivery.rs` | 442 | `send_stanza_to_available_user_resources_and_detached_available` | `available_resources_for_user` | R | A | E2 / ACT-DEF |
| `ws/handlers/presence/subscription/delivery.rs` | 452 | `send_stanza_to_available_user_resources_and_detached_available` | `send_to` | W | T | D2c |
| `ws/handlers/presence/subscription/delivery.rs` | 472 | `send_stanza_to_available_user_resources_and_detached_available` | `available_resources_for_user` | R | A | E2 / ACT-DEF |
| `ws/handlers/presence/subscription/delivery.rs` | 481 | `send_stanza_to_available_user_resources_and_detached_available` | `send_to` | W | T | D2c |
| `ws/handlers/presence/subscription/delivery.rs` | 633 | `record_stanza_for_detached_available_resources_excluding` | `send_to` | W | T | D2c |
| `ws/handlers/presence/subscription/delivery.rs` | 694 | `record_subscription_stanza_for_detached_resources_excluding` | `send_to` | W | T | D2c |
| `ws/handlers/presence/subscription/delivery.rs` | 714 | `subscription_presence_recipients` | `available_resources_for_user` | R | A | E2 / ACT-DEF |
| `ws/handlers/presence/subscription/delivery.rs` | 724 | `subscription_presence_recipients` | `roster_interested_resources_for_user` | R | A | E2 / ACT-DEF |
| `ws/handlers/presence/subscription/directed.rs` | 36 | `handle_directed_presence` | `send_to` | W | T | D2c |
| `ws/handlers/presence/subscription/directed.rs` | 62 | `handle_directed_presence` | `send_to` | W | T | D2c |
| `ws/handlers/presence/subscription/implementation.rs` | 125 | `try_handle_remote_subscription_presence` | `remove_pending_subscribe` | W | A | E2 |
| `ws/handlers/presence/subscription/implementation.rs` | 188 | `handle_subscription_presence` | `remove_pending_subscribe` | W | A | E2 |
| `ws/handlers/presence/subscription/implementation.rs` | 198 | `handle_subscription_presence` | `remove_pending_subscribe` | W | A | E2 |
| `ws/handlers/presence/subscription/implementation.rs` | 262 | `handle_subscription_presence` | `queue_pending_subscription_stanza` | W | A | E2 |
| `ws/handlers/presence/subscription/implementation.rs` | 297 | `handle_subscription_presence` | `queue_pending_subscription_stanza` | W | A | E2 |
| `ws/handlers/presence/subscription/implementation.rs` | 304 | `handle_subscription_presence` | `send_to` | W | T | D2c |
| `ws/handlers/presence/subscription/implementation.rs` | 324 | `handle_subscription_presence` | `queue_pending_subscription_stanza` | W | A | E2 |
| `ws/handlers/pubsub_fanout/caps.rs` | 177 | `deliver_to_user_resources` | `available_resources_for_user` | R | A | E2 / ACT-DEF |
| `ws/handlers/pubsub_fanout/caps.rs` | 219 | `deliver_to_user_resources` | `try_send_to` | W | T | keep (already non-blocking) |
| `ws/handlers/pubsub_fanout/implementation.rs` | 267 | `fan_out_event` | `try_send_to` | W | T | keep (already non-blocking) |
| `ws/handlers/roster_fanout.rs` | 114 | `try_queue_entry` | `try_send_outbound_if_owner` | W | T | W1.3 (token) |
| `ws/handlers/roster_fanout.rs` | 129 | `try_live_resource` | `entry` | R | I | ACT-DEF (derived socket index) |
| `ws/handlers/roster_fanout.rs` | 169 | `try_live_resource` | `entry` | R | I | ACT-DEF (derived socket index) |
| `ws/handlers/roster_fanout.rs` | 245 | `send_roster_push_to_resources` | `roster_interested_resources_for_user` | R | A | E2 / ACT-DEF |
| `ws/handlers/roster_fanout.rs` | 257 | `send_roster_push_to_resources` | `entry` | R | I | ACT-DEF (derived socket index) |
| `ws/handlers/roster_fanout.rs` | 315 | `send_roster_push_to_resources` | `roster_interested_resources_for_user` | R | A | E2 / ACT-DEF |
| `ws/registration.rs` | 52 | `prepare_socket_bind` | `lock_bind` | W | T | D2r |
| `ws/registration.rs` | 243 | `rollback_registered_connection` | `unregister_if_owner` | W | I | W1.3 (token) / D2r |
| `ws/registration.rs` | 368 | `publish_stream_id_and_presence` | `set_sm_stream_id_if_owner` | W | I | W1.3 (token) / ACT-DEF |
| `ws/registration.rs` | 401 | `publish_stream_id_and_presence` | `update_presence_if_owner` | W | A | E2 / ACT-DEF |
| `ws/registration.rs` | 402 | `publish_stream_id_and_presence` | `update_presence_state_if_owner` | W | A | E2 / ACT-DEF |
| `ws/registration.rs` | 604 | `register_bound_connection_after_frame_with_admission` | `register_entry` | W | I | W1.3 (token) / D2a |
| `ws/registration.rs` | 610 | `register_bound_connection_after_frame_with_admission` | `entry_if_owner` | R | I | W1.3 |
| `ws/registration.rs` | 620 | `register_bound_connection_after_frame_with_admission` | `set_occupancy_session_if_owner` | W | I | W1.3 (token) / ACT-DEF |
| `ws/registration.rs` | 663 | `register_bound_connection_after_frame_with_admission` | `entry_if_owner` | R | I | W1.3 |
| `ws/registration.rs` | 690 | `register_bound_connection_after_frame_with_admission` | `entry_if_owner` | R | I | W1.3 |
| `ws/registration.rs` | 732 | `register_bound_connection_after_frame_with_admission` | `unregister_if_owner` | W | I | W1.3 (token) / D2r |
| `ws/stream_management/implementation.rs` | 113 | `publish` | `set_sm_stream_id_if_owner` | W | I | W1.3 (token) / ACT-DEF |
| `ws/stream_management/implementation.rs` | 213 | `terminate_if_demoted_during_publication` | `entry_if_owner` | R | I | W1.3 |
| `ws/stream_management/registration.rs` | 299 | `reset_resume_attempt` | `unregister_if_owner` | W | I | W1.3 (token) / D2r |
| `ws/stream_management/registration.rs` | 321 | `reset_resume_attempt` | `prune_completed_bind` | W | T | D2r |
| `ws/stream_management/registration.rs` | 366 | `close_registered_resume_attempt` | `unregister_if_owner` | W | I | W1.3 (token) / D2r |
| `xmpp/routing.rs` | 275 | `route_presence_local` | `send_to` | W | O | delete |
| `xmpp/routing.rs` | 297 | `route_presence_local` | `send_to` | W | O | delete |
| `xmpp/routing.rs` | 373 | `route_iq_local_unchecked` | `send_to` | W | O | delete |
| `xmpp/routing.rs` | 390 | `route_iq_local_unchecked` | `send_to` | W | O | delete |

#### Call sites by area

| Area | Sites | Files |
|---|---|---|
| websocket handlers | 78 | 20 |
| websocket socket loop/registration/cleanup | 43 | 6 |
| clustering/route_bridge | 33 | 9 |
| interpret | 12 | 7 |
| pending_delivery | 12 | 2 |
| clustering (other) | 10 | 2 |
| other | 9 | 5 |
| sm_promotion | 8 | 4 |
| session janitors | 7 | 2 |
| admin | 6 | 1 |
| ingress | 6 | 4 |
| waddle-xmpp (other) | 4 | 1 |


## 2. `ConnectionEntry` (`waddle-xmpp/src/registry/connection_registry/outbound.rs:263-347`, `Clone`)

### 2.1 Ownership summary and D3s verdict

Clones of one entry, and therefore of its `sender` and every shared `Arc`, are held by four places:

- the registry DashMap;
- `UserActor.connections` (`user_actor/implementation.rs:30`);
- `UserRegistryActor` `resources` (`user_registry.rs:81`);
- `SocketLifecycle.entry` (`binding.rs:30`).

On an owner node, a remote-hosted mirror is held in all four as well. The **ownership token** is `carbons_enabled` itself: `register_entry` returns `carbons_handle()`, and every `*_if_owner` check is `Arc::ptr_eq(&entry.carbons_enabled, owner)`. The checks sit in the registry (`connection_registry.rs:118,149`, `connections.rs`, `sending.rs`) and outside it in:

- `user_actor/implementation.rs:73,133,139`
- `user_registry.rs:400`
- `owner_update.rs:159,166-167`
- `local_claims.rs:805`
- `owner.rs:262,367`
- `owner_retire.rs:89`
- `pending_delivery/recovery.rs:183`
- `roster_fanout.rs:116`
- `delivery_immediate.rs:60`
- `route_to_connection/implementation.rs:1532`
- `cleanup.rs:1933,3873`
- `session_janitors/implementation.rs:1624`

The flag is written by `ws/handlers/iq/sans_io.rs:176` (`set_carbons_enabled`), `UserActor::SetCarbonsEnabled` (`user_actor/implementation.rs:469`), `owner_update.rs:102` and `apply_remote_resource_state` (`side_effects.rs:457`). W1.3 replaces the token with `ConnectionRef` at every row in §2.2 marked `R(token)`.

**D3s test:** which fields duplicated between `WsConnState` and `ConnectionEntry` have no reader or writer outside the owning connection task?

| Duplicated field | Outside readers/writers (examples; full list §2.2) | D3s? |
|---|---|---|
| `carbons_enabled` | UserActor delivery and selection, owner mirror, `local_claims`, `pending_delivery`, interpret effects | no (ACT-DEF) |
| `roster_interested` | `roster_interested_resources_for_user` (registry selection), `route_bridge/types.rs:153` snapshot, `owner_update.rs:117`, resync `socket.rs:1148` | no (ACT-DEF) |
| `blocklist_interested` | `blocklist_interested_resources_for_user`, `types.rs:154`, `owner_update.rs:132`, `socket.rs:1151` | no (ACT-DEF) |
| `presence_available` / `presence_priority` | UserActor (`user_actor/implementation.rs:242-243,282-283,325-328`, `delivery.rs:105-106`), `pending_delivery/flush.rs:197,808`, `recovery.rs:35-36,176-177`, `sm_promotion/live.rs:30,33`, mirror | no (ACT-DEF) |
| `sm_stream_id` | `local_claims.rs:171`, `resume_bridge.rs:60,164`, `remote_socket.rs:498`, `pending_delivery/recovery.rs:184`, `route_to_connection/implementation.rs:1489`, UserActor via `try_send_archive_ordered` | no (DEFprep → ACT-DEF) |
| `occupancy_session` | `socket.rs:1239,1311,1327`, `types.rs:151`, mirror `side_effects.rs:454`, `occupancy_session_of` callers | no (DEFprep → ACT-DEF) |
| `pending_subscribes_flushed` | **none**: `regular.rs:134` (own presence handler, owner-gated CAS), `cleanup.rs:987` (own detach snapshot), `registration.rs:665` (own resume republish) | **yes: the only D3s field.** The move must keep an owner or generation check: both CAS sites are owner-gated today through `entry_if_owner` |

The non-duplicated shared fields are not D3s material:

- `offline_flushed` is written off-task by the session-recovery spawn (`regular.rs:390`) and by `pending_delivery/flush.rs:198`.
- `terminal_ordering_retry` is acquired on the *live target's* entry by another connection's cleanup (`cleanup.rs:2056,2215`).
- `write_interruption` is cancelled by `try_force_detach_and_interrupt` from `resume_bridge.rs:182`, `owner_retire.rs:64`, `socket.rs:1254` and `ws/registration.rs:95`.
- `archive_dispatch` is mutated from the UserActor turn via `try_send_archive_ordered`.

### 2.2 Per-field use sites

Type-resolved production uses of each field, either directly or through its accessor method. Uses inside `registry/connection_registry*` are folded into "registry-internal". "Outside owner?" is **yes** when some reader or writer runs outside the connection task that owns the entry. That covers waddle-xmpp actors, `clustering/`, janitors, `pending_delivery`, `sm_promotion`, interpret effects and spawned tasks. This is the D3s test.

| Field | Class | Slice | Writers (file:line) | Readers (file:line) | Outside owner? |
|---|---|---|---|---|---|
| `sender` | T | ACT-DEF (sender clones deleted); D2b-e/D2p (producer semantics) | `xmpp/registry/user_actor/delivery.rs:56` (try_deliver) W(try_send) | — + 22 registry-internal | yes |
| `carbons_enabled` | A | W1.3 (ownership token) + E2/ACT-DEF (flag) | `clustering/route_bridge/registration/owner_update.rs:102` (update_remote_user_resource_on_owner)<br>`clustering/route_bridge/registration/side_effects.rs:457` (apply_remote_resource_state)<br>`ws/registration.rs:590` (register_bound_connection_after_frame_with_admission)<br>`xmpp/registry/user_actor/implementation.rs:469` (handle) | `clustering/local_claims.rs:805` (force_detach_entries) R(token)<br>`clustering/route_bridge/registration/owner.rs:262` (register_remote_user_resource_on_owner_locked) R(token)<br>`clustering/route_bridge/registration/owner.rs:367` (register_remote_user_resource_on_owner_locked) R(token)<br>`clustering/route_bridge/registration/owner_retire.rs:89` (schedule_local_incumbent_retirement) R(token)<br>`clustering/route_bridge/registration/owner_update.rs:159` (owner_remote_entry_if_current) R(token)<br>`clustering/route_bridge/registration/owner_update.rs:166` (owner_remote_entry_if_current) R(token)<br>`clustering/route_bridge/registration/owner_update.rs:167` (owner_remote_entry_if_current) R(token)<br>`clustering/route_bridge/registration/socket.rs:1145` (build_full_resync_updates)<br>`clustering/route_bridge/types.rs:152` (from_entry)<br>`srv/pending_delivery/recovery.rs:183` (tick) R(token)<br>`interpret/effects/delivery_immediate.rs:60` (execute_with_received_at) R(token)<br>`interpret/route_to_connection/implementation.rs:1532` (deliver_ordered_local_copy) R(token)<br>`ws/cleanup.rs:1933` (preferred_live_pending_flush_target) R(token)<br>`ws/cleanup.rs:3873` (cleanup_invalidated_detached_session) R(token)<br>`ws/cleanup.rs:3876` (cleanup_invalidated_detached_session) R(token)<br>`ws/handlers/roster_fanout.rs:116` (try_queue_entry) R(token)<br>`server/session_janitors/implementation.rs:1624` (run_sm_expiry_sweep_with_custody_cursor) R(token)<br>`xmpp/registry/user_actor/delivery.rs:56` (try_deliver)<br>`xmpp/registry/user_actor/implementation.rs:73` (remove_resource_if_owner) R(token)<br>`xmpp/registry/user_actor/implementation.rs:133` (handle) R(token)<br>`xmpp/registry/user_actor/implementation.rs:139` (handle) R(token)<br>`xmpp/registry/user_actor/implementation.rs:244` (handle)<br>`xmpp/registry/user_actor/implementation.rs:489` (handle)<br>`xmpp/registry/user_registry.rs:400` (drain_pending_unregisters_for) R(token) + 35 registry-internal | yes |
| `presence_available` | A | E2 / ACT-DEF | `clustering/route_bridge/registration/side_effects.rs:466` (apply_remote_resource_state)<br>`xmpp/registry/user_actor/implementation.rs:325` (handle) | `clustering/route_bridge/registration/socket.rs:1140` (build_full_resync_updates)<br>`clustering/route_bridge/types.rs:155` (from_entry)<br>`srv/pending_delivery/flush.rs:197` (resume_flush_for_resource_with_retry)<br>`srv/pending_delivery/flush.rs:808` (flush_recovery_pass)<br>`srv/pending_delivery/recovery.rs:35` (is_current)<br>`srv/pending_delivery/recovery.rs:176` (tick)<br>`ws/cleanup.rs:930` (cleanup_connection_shutdown_inner)<br>`ws/cleanup.rs:1524` (cleanup_connection_shutdown_inner)<br>`ws/cleanup.rs:1843` (promote_terminal_recovery_inner)<br>`ws/cleanup.rs:1926` (preferred_live_pending_flush_target)<br>`ws/cleanup.rs:2021` (redrive_terminal_pending_rows_to_live_resource_with_cancel)<br>`ws/cleanup.rs:2195` (spawn_terminal_pending_ordering_retry)<br>`ws/cleanup.rs:2629` (refuse_detach_without_principal)<br>`ws/cleanup.rs:2825` (broadcast_unavailable_if_no_replacement)<br>`srv/sm_promotion/live.rs:30` (build_online_resources)<br>`xmpp/registry/user_actor/delivery.rs:105` (handle)<br>`xmpp/registry/user_actor/implementation.rs:242` (handle)<br>`xmpp/registry/user_actor/implementation.rs:282` (handle) + 6 registry-internal | yes |
| `presence_priority` | A | E2 / ACT-DEF | `clustering/route_bridge/registration/side_effects.rs:469` (apply_remote_resource_state)<br>`xmpp/registry/user_actor/implementation.rs:328` (handle) | `clustering/route_bridge/registration/socket.rs:1141` (build_full_resync_updates)<br>`clustering/route_bridge/types.rs:156` (from_entry)<br>`srv/pending_delivery/flush.rs:197` (resume_flush_for_resource_with_retry)<br>`srv/pending_delivery/flush.rs:809` (flush_recovery_pass)<br>`srv/pending_delivery/recovery.rs:36` (is_current)<br>`srv/pending_delivery/recovery.rs:177` (tick)<br>`srv/pending_delivery/recovery.rs:185` (tick)<br>`ws/cleanup.rs:979` (cleanup_connection_shutdown_inner)<br>`ws/cleanup.rs:1927` (preferred_live_pending_flush_target)<br>`ws/cleanup.rs:2022` (redrive_terminal_pending_rows_to_live_resource_with_cancel)<br>`ws/cleanup.rs:2196` (spawn_terminal_pending_ordering_retry)<br>`srv/sm_promotion/live.rs:33` (build_online_resources)<br>`xmpp/registry/user_actor/delivery.rs:106` (handle)<br>`xmpp/registry/user_actor/implementation.rs:243` (handle)<br>`xmpp/registry/user_actor/implementation.rs:283` (handle) + 6 registry-internal | yes |
| `roster_interested` | A | E2 / ACT-DEF | `clustering/route_bridge/registration/owner_update.rs:117` (update_remote_user_resource_on_owner)<br>`clustering/route_bridge/registration/side_effects.rs:460` (apply_remote_resource_state)<br>`ws/registration.rs:593` (register_bound_connection_after_frame_with_admission) | `clustering/route_bridge/registration/socket.rs:1148` (build_full_resync_updates)<br>`clustering/route_bridge/types.rs:153` (from_entry)<br>`ws/handlers/roster_fanout.rs:106` (try_queue_entry)<br>`ws/handlers/roster_fanout.rs:130` (try_live_resource) + 5 registry-internal | yes |
| `blocklist_interested` | A | E2 / ACT-DEF | `clustering/route_bridge/registration/owner_update.rs:132` (update_remote_user_resource_on_owner)<br>`clustering/route_bridge/registration/side_effects.rs:463` (apply_remote_resource_state)<br>`ws/registration.rs:596` (register_bound_connection_after_frame_with_admission) | `clustering/route_bridge/registration/socket.rs:1151` (build_full_resync_updates)<br>`clustering/route_bridge/types.rs:154` (from_entry) + 5 registry-internal | yes |
| `offline_flushed` | A | E2 (P1 presentation receipts) / ACT-DEF | `srv/pending_delivery/flush.rs:198` (resume_flush_for_resource_with_retry)<br>`ws/cleanup.rs:2121` (redrive_terminal_pending_rows_to_live_resource_with_cancel)<br>`ws/cleanup.rs:2199` (spawn_terminal_pending_ordering_retry)<br>`ws/handlers/presence/regular.rs:283` (plan_offline_flush) W(CAS)<br>`ws/handlers/presence/regular.rs:390` (spawn_session_recovery_delivery) | — + 3 registry-internal | yes |
| `pending_subscribes_flushed` | A | E2 / ACT-DEF | `ws/handlers/presence/regular.rs:134` (handle_regular_presence_update) W(CAS)<br>`ws/registration.rs:665` (register_bound_connection_after_frame_with_admission) W(CAS) | `ws/cleanup.rs:987` (cleanup_connection_shutdown_inner) + 2 registry-internal | **no** |
| `sm_stream_id` | I | DEFprep / ACT-DEF | registry-internal only: `set_sm_stream_id_if_owner` (`connection_registry.rs:153`) and `set_sm_stream_id` (`:101`); production callers in §1.3 (`ws/registration.rs:368`, `ws/stream_management/implementation.rs:113`) | `clustering/local_claims.rs:171` (demote_stream)<br>`clustering/resume_bridge.rs:60` (request_validated_local_resume)<br>`clustering/resume_bridge.rs:164` (request_local_forced_detach)<br>`clustering/route_bridge/delivery/remote_socket.rs:498` (deliver_remote_resource_frame_on_socket)<br>`srv/pending_delivery/recovery.rs:184` (tick)<br>`interpret/route_to_connection/implementation.rs:1489` (deliver_ordered_local_copy)<br>`ws/cleanup.rs:1934` (preferred_live_pending_flush_target)<br>`ws/cleanup.rs:3893` (cleanup_invalidated_detached_session)<br>`ws/handlers/presence/regular.rs:287` (plan_offline_flush)<br>`ws/registration.rs:588` (register_bound_connection_after_frame_with_admission) R(handle)<br>`xmpp/registry/user_actor/delivery.rs:56` (try_deliver) + 17 registry-internal | yes |
| `occupancy_session` | I | DEFprep / ACT-DEF | `clustering/route_bridge/registration/side_effects.rs:454` (apply_remote_resource_state) W(lock) | `clustering/route_bridge/registration/socket.rs:1239` (force_detach_remote_user_resource_on_socket)<br>`clustering/route_bridge/registration/socket.rs:1311` (complete_fresh_remote_retirement)<br>`clustering/route_bridge/registration/socket.rs:1327` (complete_fresh_remote_retirement)<br>`clustering/route_bridge/types.rs:151` (from_entry) + 4 registry-internal | yes |
| `force_detach_tx` | T | D2a (control channel) / D2r | `clustering/resume_bridge.rs:182` (request_local_forced_detach)<br>`clustering/route_bridge/registration/owner_retire.rs:64` (schedule_local_incumbent_retirement)<br>`clustering/route_bridge/registration/socket.rs:1254` (force_detach_remote_user_resource_on_socket)<br>`ws/registration.rs:95` (prepare_socket_bind) | `clustering/local_claims.rs:198` (demote_stream)<br>`clustering/local_claims.rs:813` (force_detach_entries)<br>`clustering/route_bridge/registration/owner_retire.rs:62` (schedule_local_incumbent_retirement)<br>`clustering/route_bridge/registration/socket.rs:1256` (force_detach_remote_user_resource_on_socket)<br>`ws/registration.rs:93` (prepare_socket_bind)<br>`ws/registration.rs:588` (register_bound_connection_after_frame_with_admission) R(handle)<br>`ws/stream_management/implementation.rs:228` (terminate_if_demoted_during_publication)<br>`xmpp/registry/user_registry.rs:762` (issue_stale_retirement_force_detach) + 4 registry-internal | yes |
| `force_detach_rx` | T | D2a (control channel) / D2r | `clustering/route_bridge/registration/owner.rs:368` (register_remote_user_resource_on_owner_locked)<br>`ws/connection.rs:1572` (handle_inbound_text) | — + 2 registry-internal | yes |
| `write_interruption` | T | D1 | `clustering/resume_bridge.rs:182` (request_local_forced_detach) W(cancel)<br>`clustering/route_bridge/registration/owner_retire.rs:64` (schedule_local_incumbent_retirement) W(cancel)<br>`clustering/route_bridge/registration/socket.rs:1254` (force_detach_remote_user_resource_on_socket) W(cancel)<br>`ws/registration.rs:95` (prepare_socket_bind) W(cancel)<br>`ws/registration.rs:587` (register_bound_connection_after_frame_with_admission) | — + 2 registry-internal | yes |
| `resume_control` | T | DEFprep / D2a (control channel) | `ws/registration.rs:588` (register_bound_connection_after_frame_with_admission) | `clustering/resume_bridge.rs:66` (request_validated_local_resume) + 3 registry-internal | yes |
| `hosting` | O | ACT-DEF (remote-hosted mirrors deleted) | `clustering/route_bridge/registration/owner.rs:365` (register_remote_user_resource_on_owner_locked) | `clustering/resume_bridge.rs:60` (request_validated_local_resume)<br>`clustering/resume_bridge.rs:164` (request_local_forced_detach)<br>`clustering/route_bridge/registration/socket.rs:1325` (complete_fresh_remote_retirement)<br>`srv/ingress/execute_relay_copy.rs:34` (attempt_before_readiness)<br>`srv/ingress/execute_relay_copy.rs:145` (append_sealed_legacy)<br>`srv/pending_delivery/flush.rs:194` (resume_flush_for_resource_with_retry)<br>`srv/pending_delivery/flush.rs:807` (flush_recovery_pass)<br>`srv/pending_delivery/recovery.rs:34` (is_current)<br>`srv/pending_delivery/recovery.rs:175` (tick)<br>`interpret/bot.rs:756` (route_join_presences)<br>`interpret/route_to_connection/implementation.rs:1469` (deliver_ordered_local_copy)<br>`ws/cleanup.rs:1925` (preferred_live_pending_flush_target)<br>`ws/cleanup.rs:2020` (redrive_terminal_pending_rows_to_live_resource_with_cancel)<br>`ws/cleanup.rs:2194` (spawn_terminal_pending_ordering_retry)<br>`ws/handlers/roster_fanout.rs:135` (try_live_resource)<br>`ws/handlers/roster_fanout.rs:258` (send_roster_push_to_resources) + 7 registry-internal | yes |
| `archive_dispatch` | A | ACT-DEF (moves with archive-ordered delivery into account/connection actor) | `xmpp/registry/user_actor/delivery.rs:56` (try_deliver) | — + 4 registry-internal | yes |
| `terminal_ordering_retry` | T | D2r: the terminal-cleanup redrive that acquires it is replaced by the retirement protocol; it is a shared atomic, so if D2r keeps the redrive it moves to ACT-DEF with the other shared atomics | `ws/cleanup.rs:2056` (redrive_terminal_pending_rows_to_live_resource_with_cancel)<br>`ws/cleanup.rs:2215` (spawn_terminal_pending_ordering_retry) | — + 3 registry-internal | yes |


## 3. `WsConnState` (`waddle-server/src/server/routes/websocket/state.rs:1720-1880`)

### 3.1 Findings

- **One writer task today.** `WsConnState` is owned by value by the connection task (`connection.rs:287`) and is `pub(super)`. All 430 direct accesses are in 9 files under `server/routes/websocket/`:
  - `batch_write.rs`, `cleanup.rs`, `connection.rs`, `frame.rs`, `outbound.rs`, `registration.rs`, `replay.rs`, `state.rs`, `stream_management/registration.rs`.

  Handlers reach fields only through borrows created in `handle_xmpp_frame_impl`, which destructures the struct at `frame.rs:344-372`. The borrows go into:
  - `SmCtx` (`stream_management/implementation.rs:326-358`);
  - `IqConnState` (`handlers/iq/conn_state.rs:2-17`);
  - `PresenceHandlerContext` (`handlers/presence/implementation.rs:46-49`);
  - the SASL and resource-binding functions.

  D2a can therefore move the struct into the actor without touching any reader outside `websocket/`.
- **Shared through handles.** Five fields leak out of the task:
  - `registry_owner` is the entry's `carbons_enabled` `Arc`.
  - `socket_lifecycle` is shared with the bind slot.
  - `write_interruption` is cloned into the entry (`registration.rs:587`).
  - `live_resume_control` is bound into the entry (`registration.rs:588`).
  - `sm_state` hands a `SmResumeCheckpointHandle` to `LiveResumeControl` (`state.rs:1888-1891`).
- **Resume carriers.** `presence_*` and `pending_subscribes_flushed` are not live state (§0.3). They are restored at resume and republished to the registry. Their live counterparts are on `ConnectionEntry` and `ConnectionRegistry.presence_states`. As a result, `terminal_recovery_snapshot` (`cleanup.rs:2269-2291`) carries presence that can be stale: what was restored at resume, or the reset defaults. The detach snapshot (`cleanup.rs:930-988`) reads the live values.
- **Test-only fields** (not counted): `pre_final_principal_recheck_test_hook` (:1785) and `post_sm_finalization_test_hook` (:1788).

### 3.2 Direct accesses per field

Type-resolved direct accesses (`conn.<field>`). The `WsConnState::new` initialiser and the `handle_xmpp_frame_impl` destructuring (`frame.rs:344-372`) are counted under "init/destructure" and not itemised. "W(&mut)" means a `&mut` borrow handed to a callee. "W(call)" means a method call that by name mutates. Plain field reads are "R". Every access is inside `server/routes/websocket/`: the struct is `pub(super)` and owned by value by the connection task (`connection.rs:287`).

| # | Field (state.rs line) | Class | Slice | Direct writes | Direct reads | Writes/borrows through destructured references | init/destructure |
|---|---|---|---|---|---|---|---|
| 1 | `ingress_generation` (:1721) | A | D2a; W1.3/F2 supersede | — | `frame.rs:1062` | — | 1 |
| 2 | `phase` (:1722) | A | D2a | `batch_write.rs:649`, `batch_write.rs:1009`, `cleanup.rs:815`, `cleanup.rs:2790`, `frame.rs:1112`, `frame.rs:1141`, `stream_management/registration.rs:324`, `stream_management/registration.rs:382` | `batch_write.rs:668`, `batch_write.rs:1036`, `cleanup.rs:814`, `cleanup.rs:839`, `cleanup.rs:853`, `cleanup.rs:890`, `cleanup.rs:1735`, `connection.rs:388`, `connection.rs:400`, `connection.rs:427`, `connection.rs:705`, `connection.rs:731`, `connection.rs:798`, `connection.rs:823`, `connection.rs:868`, `connection.rs:933`, `connection.rs:1090`, `connection.rs:1564`, `connection.rs:1724`, `connection.rs:1732`, `connection.rs:2103`, `frame.rs:1035`, `frame.rs:1112`, `frame.rs:1134`, `frame.rs:1141`, `outbound.rs:216`, `registration.rs:448`, `registration.rs:772`, `registration.rs:800`, `state.rs:2053`, `state.rs:2144` | `frame.rs:459,552`; `sasl.rs:101,194,300`; `resource_binding.rs:65`; SmCtx `stream_management/implementation.rs:490,531,929,1189,1352`; `&` to IQ/presence/message handlers | 2 |
| 3 | `stream_open_handled` (:1726) | T | D2a | `state.rs:2017`, `state.rs:2036` | `state.rs:2041` | — | 1 |
| 4 | `stream_open_wire_state` (:1731) | T | D1 | `state.rs:2018`, `state.rs:2028`, `state.rs:2037` | `state.rs:2025`, `state.rs:2042` | — | 1 |
| 5 | `authenticated_session` (:1734) | A | D2a | — | `cleanup.rs:946`, `cleanup.rs:956`, `cleanup.rs:1032`, `cleanup.rs:1233`, `cleanup.rs:2274`, `connection.rs:658`, `frame.rs:947`, `outbound.rs:217`, `replay.rs:235` | `sasl.rs:100,299`; SmCtx `stream_management/implementation.rs:1338` (resume); `&` to handlers | 2 |
| 6 | `occupancy_session` (:1735) | A | D2a (entry copy: ACT-DEF) | `stream_management/registration.rs:323` | `cleanup.rs:964`, `cleanup.rs:1452`, `cleanup.rs:1499`, `cleanup.rs:1536`, `cleanup.rs:1700`, `cleanup.rs:2280`, `cleanup.rs:2747`, `registration.rs:500`, `registration.rs:540`, `registration.rs:612`, `registration.rs:620`, `registration.rs:776` | SmCtx `stream_management/implementation.rs:1339` (resume restore); `&` via `IqConnState`, `PresenceHandlerContext` | 2 |
| 7 | `sm_state` (:1738) | A | D2a; F1b checkpoints | `batch_write.rs:649`, `batch_write.rs:799`, `batch_write.rs:812`, `batch_write.rs:816`, `batch_write.rs:1009`, `cleanup.rs:812`, `cleanup.rs:945`, `cleanup.rs:1232`, `cleanup.rs:2731`, `connection.rs:1796`, `connection.rs:1889`, `frame.rs:1107`, `frame.rs:1129`, `outbound.rs:67`, `outbound.rs:69`, `outbound.rs:74`, `state.rs:2056`, `stream_management/registration.rs:134`, `stream_management/registration.rs:145`, `stream_management/registration.rs:340` | `batch_write.rs:330`, `batch_write.rs:394`, `batch_write.rs:499`, `batch_write.rs:587`, `batch_write.rs:596`, `batch_write.rs:622`, `batch_write.rs:624`, `batch_write.rs:625`, `batch_write.rs:675`, `batch_write.rs:689`, `batch_write.rs:697`, `batch_write.rs:707`, `batch_write.rs:782`, `batch_write.rs:789`, `batch_write.rs:823`, `batch_write.rs:909`, `cleanup.rs:842`, `cleanup.rs:874`, `cleanup.rs:876`, `cleanup.rs:889`, `cleanup.rs:905`, `cleanup.rs:925`, `cleanup.rs:937`, `cleanup.rs:991`, `cleanup.rs:1008`, `cleanup.rs:1423`, `cleanup.rs:1564`, `cleanup.rs:1798`, `cleanup.rs:1800`, `cleanup.rs:1819`, `cleanup.rs:2615`, `cleanup.rs:2666`, `cleanup.rs:2706`, `cleanup.rs:2722`, `connection.rs:329`, `connection.rs:345`, `connection.rs:346`, `connection.rs:363`, `connection.rs:1585`, `connection.rs:1663`, `connection.rs:1679`, `connection.rs:1688`, `connection.rs:1698`, `connection.rs:1725`, `connection.rs:1726`, `connection.rs:1932`, `connection.rs:1933`, `connection.rs:2073`, `connection.rs:2081`, `frame.rs:938`, `frame.rs:944`, `outbound.rs:83`, `outbound.rs:84`, `outbound.rs:87`, `outbound.rs:92`, `outbound.rs:100`, `outbound.rs:207`, `outbound.rs:216`, `outbound.rs:270`, `outbound.rs:284`, `outbound.rs:285`, `registration.rs:371`, `registration.rs:379`, `registration.rs:384`, `registration.rs:484`, `registration.rs:519`, `registration.rs:567`, `registration.rs:751`, `state.rs:1950`, `state.rs:1958`, `state.rs:1959`, `state.rs:1993`, `stream_management/registration.rs:94`, `stream_management/registration.rs:132`, `stream_management/registration.rs:167`, `stream_management/registration.rs:226`, `stream_management/registration.rs:274` | SmCtx and `&mut` params in `stream_management/`, `replay.rs`, `frame.rs:728` | 2 |
| 8 | `sm_ingress_fence` (:1740) | A | D2a | `cleanup.rs:2730`, `state.rs:2057`, `stream_management/registration.rs:339` | `cleanup.rs:1783`, `frame.rs:943` | SmCtx (`stream_management/implementation.rs:329`) | 2 |
| 9 | `sm_inbound_completion` (:1741) | A | D2a (DEC-3 contiguous `h`) | `batch_write.rs:698`, `batch_write.rs:783`, `connection.rs:1841`, `connection.rs:1881`, `frame.rs:1106`, `frame.rs:1121`, `frame.rs:1128`, `stream_management/registration.rs:94` | `connection.rs:1722`, `connection.rs:1791`, `connection.rs:1795`, `connection.rs:1822`, `connection.rs:1825`, `connection.rs:1826`, `connection.rs:1831`, `connection.rs:1847`, `connection.rs:1854`, `connection.rs:1873`, `connection.rs:1875`, `connection.rs:1888`, `connection.rs:1976`, `connection.rs:2044`, `frame.rs:937` | SmCtx; `frame.rs` reserve/abandon on the destructured borrow | 2 |
| 10 | `inbound_frame_terminal` (:1744) | T | D1 | `connection.rs:1459`, `connection.rs:2018`, `frame.rs:1078` | — | `frame.rs` `*inbound_frame_terminal = …` on the destructured borrow | 2 |
| 11 | `ordered_relay_handoff_tx` (:1745) | T | D1 (bound) / C7 (delete) | `connection.rs:293` | — | `&` into interpret handoff (`frame.rs:588`) | 2 |
| 12 | `carbons_enabled` (:1750) | A | D2a; entry mirror ACT-DEF | `stream_management/registration.rs:344` | `cleanup.rs:929`, `cleanup.rs:2281`, `registration.rs:592`, `registration.rs:801` | `handlers/iq/sans_io.rs:171` via `IqConnState`; SmCtx `stream_management/implementation.rs:1340` (resume) | 2 |
| 13 | `roster_interested` (:1752) | A | D2a; entry mirror ACT-DEF | `stream_management/registration.rs:345` | `cleanup.rs:966`, `cleanup.rs:2282`, `registration.rs:595` | `handlers/iq/roster/implementation.rs:112` via `IqConnState`; SmCtx `:1341` (resume) | 2 |
| 14 | `blocklist_interested` (:1754) | A | D2a; entry mirror ACT-DEF | `stream_management/registration.rs:346` | `cleanup.rs:967`, `cleanup.rs:2283`, `registration.rs:597` | `handlers/iq/blocking.rs:67` via `IqConnState`; SmCtx `:1342` (resume) | 2 |
| 15 | `presence_available` (:1756) | A | D2a; entry mirror ACT-DEF | `stream_management/registration.rs:347` | `cleanup.rs:2284`, `registration.rs:399` | SmCtx `stream_management/implementation.rs:1343` (resume restore only; the presence handler does not receive it) | 2 |
| 16 | `presence_show` (:1757) | A | D2a; registry `presence_states` mirror E2 | `stream_management/registration.rs:348` | `cleanup.rs:2285`, `registration.rs:405` | SmCtx `:1344` (resume restore only) | 2 |
| 17 | `presence_status` (:1758) | A | D2a; registry `presence_states` mirror E2 | `stream_management/registration.rs:349` | `cleanup.rs:2286`, `registration.rs:409` | SmCtx `:1345` (resume restore only) | 2 |
| 18 | `presence_priority` (:1759) | A | D2a; entry mirror ACT-DEF | `stream_management/registration.rs:350` | `cleanup.rs:2287`, `registration.rs:401`, `registration.rs:410` | SmCtx `:1346` (resume restore only) | 2 |
| 19 | `presence_payloads` (:1763) | A | D2a; registry `presence_states` mirror E2 | `stream_management/registration.rs:351` | `cleanup.rs:2288`, `registration.rs:416` | SmCtx `:1347` (resume restore only) | 2 |
| 20 | `pending_subscribes_flushed` (:1769) | A | D2a; entry mirror ACT-DEF | `stream_management/registration.rs:352` | `cleanup.rs:2289`, `registration.rs:646` | SmCtx `:1348` (resume restore only) | 2 |
| 21 | `pending_resume_stream_id` (:1773) | A | D2a; F2 | `cleanup.rs:898`, `connection.rs:1768`, `stream_management/registration.rs:64`, `stream_management/registration.rs:278` | `connection.rs:1537`, `connection.rs:1541`, `registration.rs:459` | SmCtx | 2 |
| 22 | `pending_resume_h` (:1774) | A | D2a; F2 | `cleanup.rs:899`, `connection.rs:1769`, `stream_management/registration.rs:106`, `stream_management/registration.rs:279` | — | SmCtx | 2 |
| 23 | `pending_resume_claim` (:1776) | A | D2a; F2 | `cleanup.rs:900`, `connection.rs:1770`, `stream_management/registration.rs:253`, `stream_management/registration.rs:275` | — | SmCtx | 2 |
| 24 | `pending_finalized_resume_outcome` (:1782) | T | D2a | `connection.rs:1660`, `registration.rs:797`, `stream_management/registration.rs:281` | — | `handle_sm_stanza` parameter | 2 |
| 25 | `registry_owner` (:1791) | O | W1.3 | `registration.rs:251`, `registration.rs:605`, `registration.rs:745`, `stream_management/registration.rs:311`, `stream_management/registration.rs:381` | `cleanup.rs:894`, `cleanup.rs:902`, `cleanup.rs:1510`, `cleanup.rs:1836`, `cleanup.rs:2622`, `connection.rs:823`, `connection.rs:1564`, `frame.rs:1134`, `state.rs:2059` | `&` via `IqConnState`, `PresenceHandlerContext`, MUC presence contexts, `frame.rs` helpers | 2 |
| 26 | `socket_lifecycle` (:1792) | T | D2r | `registration.rs:612`, `stream_management/registration.rs:315` | `cleanup.rs:1735` | — | 1 |
| 27 | `write_interruption` (:1793) | T | D1 | — | `batch_write.rs:559`, `batch_write.rs:590`, `batch_write.rs:610`, `batch_write.rs:632`, `batch_write.rs:773`, `batch_write.rs:961`, `batch_write.rs:974`, `connection.rs:290`, `registration.rs:587` | — | 1 |
| 28 | `live_resume_control` (:1796) | T | D2a (control channel); DEFprep | `cleanup.rs:1736`, `cleanup.rs:1759` | `batch_write.rs:588`, `batch_write.rs:957`, `connection.rs:288`, `registration.rs:588` | — | 1 |
| 29 | `suppress_sm_record_next_batch` (:1804) | T | D1 (per-session output order) | `connection.rs:1636`, `connection.rs:2038`, `stream_management/registration.rs:353` | `connection.rs:1635`, `connection.rs:2037` | SmCtx | 2 |
| 30 | `pending_sm_enable_commit` (:1808) | A | D2a | `connection.rs:1767`, `state.rs:2050` | — | SmCtx | 2 |
| 31 | `deferred_inbound` (:1814) | T | D1 | `batch_write.rs:716`, `batch_write.rs:1040`, `connection.rs:485`, `connection.rs:2003`, `connection.rs:2095` | `batch_write.rs:571`, `batch_write.rs:604`, `batch_write.rs:768`, `batch_write.rs:779`, `batch_write.rs:967`, `connection.rs:378`, `connection.rs:2094` | — | 1 |
| 32 | `sm_recovery_required` (:1819) | A | D2r | `cleanup.rs:2780`, `state.rs:1949`, `state.rs:2008` | `batch_write.rs:890`, `cleanup.rs:838`, `cleanup.rs:875`, `cleanup.rs:891`, `cleanup.rs:895`, `cleanup.rs:915`, `cleanup.rs:934`, `cleanup.rs:1010`, `cleanup.rs:1837`, `cleanup.rs:2623`, `connection.rs:1986`, `connection.rs:2089`, `state.rs:1946`, `state.rs:2005` | — | 1 |
| 33 | `terminal_sm_recovery` (:1829) | A | D2r | `replay.rs:610`, `state.rs:1961`, `state.rs:1977`, `state.rs:2005` | `batch_write.rs:891`, `batch_write.rs:899`, `batch_write.rs:901`, `cleanup.rs:2429`, `connection.rs:1830`, `connection.rs:1853`, `connection.rs:2008`, `connection.rs:2090`, `state.rs:1973`, `state.rs:1989` | — | 1 |
| 34 | `pending_replay_completions` (:1833) | A | D2a | `connection.rs:2193`, `connection.rs:2208` | `connection.rs:2185`, `connection.rs:2203`, `connection.rs:2213` | — | 1 |
| 35 | `terminal_sm_recovery_dropped` (:1836) | A | D2r | `state.rs:1953`, `state.rs:1974` | `state.rs:1974`, `state.rs:1985`, `state.rs:1996` | — | 1 |
| 36 | `terminal_sm_recovery_drop_warned` (:1838) | T | D2r | `state.rs:1954`, `state.rs:1999` | `state.rs:1985` | — | 1 |
| 37 | `send_window_pause_deadline` (:1846) | T | D1 | `connection.rs:333`, `connection.rs:366` | `connection.rs:331`, `connection.rs:368` | — | 1 |
| 38 | `send_window_last_request_acked` (:1854) | T | D1 | `connection.rs:346` | `connection.rs:345` | — | 1 |
| 39 | `state_machine` (:1875) | A | D2a | `cleanup.rs:944`, `cleanup.rs:1231`, `connection.rs:305`, `connection.rs:662`, `frame.rs:1040`, `outbound.rs:221`, `replay.rs:304`, `state.rs:2080`, `state.rs:2087`, `state.rs:2128`, `state.rs:2143` | — | `&mut` via `IqConnState` and `handlers::message::handle_message` | 2 |
| 40 | `keepalive_config` (:1879) | T | D1 | `state.rs:2077` | `state.rs:2125`, `stream_management/registration.rs:337` | — | 1 |


## 4. `dual_registration` (`waddle-server/src/server/dual_registration.rs`, 395 lines)

There are **15 production call sites in 6 files**. Every one is class **O**: it keeps the DashMap and the `UserRegistryActor` tree in step and uses the carbons-flag owner token (`:33-37`). All are deleted at **ACT-DEF**; W1.3 changes their token argument first. There are 12 test call lines in 11 files.

| # | File:line | Enclosing fn | Call | Purpose |
|---|---|---|---|---|
| 1 | `ws/registration.rs:244` | `rollback_registered_connection` | `mirror_unregister` | roll back the actor mirror after a failed bind |
| 2 | `ws/registration.rs:694` | `register_bound_connection_after_frame_with_admission` | `mirror_register_outcome` | authoritative actor registration (resumed bind, busy retry) |
| 3 | `ws/registration.rs:702` | same | `mirror_register_outcome` | authoritative actor registration (fresh bind) |
| 4 | `ws/registration.rs:739` | same | `mirror_unregister` | rollback on registration failure |
| 5 | `ws/registration.rs:847` | `register_remote_clustered_resource` | `mirror_register_outcome` | `NotRemote` fallback from the remote-owner path (**missing from the plan's 14**) |
| 6 | `ws/stream_management/registration.rs:302` | `reset_resume_attempt` | `mirror_unregister` | resume attempt reset |
| 7 | `ws/stream_management/registration.rs:373` | `close_registered_resume_attempt` | `mirror_unregister` | resume attempt close |
| 8 | `ws/cleanup.rs:1211` | `cleanup_connection_shutdown_inner` | `mirror_unregister` | detach-for-resume prune |
| 9 | `ws/cleanup.rs:1470` | same | `mirror_unregister` | full teardown |
| 10 | `ws/cleanup.rs:1551` | same | `mirror_unregister` | teardown tail |
| 11 | `ws/cleanup.rs:2771` | `refuse_detach_without_principal` | `mirror_unregister` | refused detach |
| 12 | `ws/cleanup.rs:3870` | `cleanup_invalidated_detached_session` | `mirror_unregister` | invalidated detached session |
| 13 | `server/session_janitors/implementation.rs:1621` | `run_sm_expiry_sweep_with_custody_cursor` | `mirror_unregister` | SM expiry janitor |
| 14 | `clustering/route_bridge/registration/socket.rs:968` | `run_remote_state_resync_attempt` | `mirror_register_outcome` | remote resync falls back to local registration |
| 15 | `clustering/route_bridge/delivery/remote.rs:545` | `refresh_remote_resource_origin` | `mirror_register_outcome` | survivor takeover (#1985) |

Sites 1–12 are in files that B2f edits, but they are not decision points (they are `UserRegistryActor` asks, not bridge or `current_claim` branches; see §8). B2f does not move them. They keep their call shape until ACT-DEF deletes `dual_registration.rs`; W1.3 changes their token argument before then. Sites 13–15 are janitor and bridge internals with the same fate.

## 5. User-resource mirrors (cross-node copies of a socket's state on the account-owner node)

Every row is class **O** and is deleted at **ACT-DEF**: remote-hosted `ConnectionEntry` mirrors, `apply_remote_resource_state` and the `remote_resource_*` asks. W1.13 serialises the update handlers on the owner in the meantime.

| Site | Role | R/W |
|---|---|---|
| `ConnectionEntry::remote_hosted` (`outbound.rs:398-402`); sole production constructor call `clustering/route_bridge/registration/owner.rs:365` | Owner node mints an `mpsc(256)` mirror entry (`hosting = Remote`) | W |
| `apply_remote_resource_state` (`registration/side_effects.rs:450-472`); sole caller `owner.rs:366` | Copies `occupancy_session`, `carbons_enabled`, `roster_interested`, `blocklist_interested`, `presence_available` and `presence_priority` from the wire snapshot onto the mirror. It does **not** copy `sm_stream_id`, `offline_flushed` or `pending_subscribes_flushed`. | W |
| `owner_update.rs:102,117,132` (`update_remote_user_resource_on_owner`) | Direct atomic stores of carbons, roster and blocklist interest on the mirror; presence goes through `apply_remote_resource_presence_to_registry` (`side_effects.rs:427,437`) | W |
| `RegisterUserResourceIfOwnerOrAbsent` ask at `owner.rs:369-377`, then `register_entry_if_owner_or_absent` `owner.rs:425` | Installs the mirror in the UserActor tree and the DashMap | W |
| `spawn_remote_resource_forwarder` (`registration/socket_forwarder.rs:14-…`, spawned from `owner.rs:486`) | Drains the mirror's `mpsc(256)` and force-detach receiver; forwards frames to the socket node | T/O |
| `OrderedRelayDeliveryBridge.remote_owner_resources` (`route_bridge/mod.rs:240`) | Owner-side registration table (registration id, socket generation, socket node) | I/O |
| `RemoteResourceStateSnapshot::from_entry` (`route_bridge/types.rs:146-157`) | Socket node reads its entry's atomics into the wire snapshot | R |
| `build_full_resync_updates` (`registration/socket.rs:1140-1151`) | Socket node resync reads presence and interest atomics | R |
| Socket-side senders: `socket.rs:346,491,619` (unregister), `socket.rs:724,1048` (update), register via `route_bridge/mod.rs:123-148` builders | Socket → owner mirror maintenance | W |
| Remote-owner mirror janitor `server/session_janitors/remote_owner.rs:35` → `sweep_remote_owner_resources` (`owner_sweep.rs`) | Reconciles stale mirrors (#2017) | O |
| `UserActor.connections` (`user_actor/implementation.rs:30`), `UserRegistryActor` `resources` (`user_registry.rs:81`) | Entry clones (local and mirror) used for selection. Class I; E2 builds account-owned selection alongside them. | I |

## 6. Remote-resource relay endpoints

| Remote message ID | Receiver handler → bridge fn | Direction | Class | Fate |
|---|---|---|---|---|
| `remote_resource_register.v3` (`relay/implementation.rs:557`) | `register_remote_user_resource_on_owner` | socket → owner | O | ACT-DEF (endpoints, D4 **[A]**) |
| `remote_resource_unregister.v2` (`:600`) | `unregister_remote_user_resource_on_owner` | socket → owner | O | ACT-DEF |
| `remote_resource_update.v1` (`:643`) | `update_remote_user_resource_on_owner` | socket → owner | O | W1.13 narrows ordering; ACT-DEF deletes |
| `remote_user_side_effect.v4` (`:691`) | `apply_remote_user_side_effect_on_owner` (carbons / roster push / blocklist push) | socket → owner | T | C6d lane (ACT-LANES); C7 deletes |
| `remote_resource_write_accepted.v1` (`:788`) | `deliver_remote_resource_write_accepted_frame_on_socket` (room-effect outbox frames) | owner → socket | T | stays until ACT-DEF endpoint cutover; C7 |
| `remote_resource_force_detach.v3` (`:836`) | `force_detach_remote_user_resource_on_socket` | owner/bind node → socket | T (control) | survives (DEC-2 `force_detach`) |
| `remote_resource_route.v8` + `live_resource_route.v2` (`remote_resource_compat.rs:156,179`) | `route_on_owner` | socket → owner (stanza routing) | T | ACT-DEF endpoint cutover **[A]**; #1804 dual-version fallback preserved by B1 until then; C7 |
| `remote_resource_frame.v3` + `live_resource_frame.v2` (`:202,223`) | `deliver_remote_resource_frame_on_socket` | owner → socket | T | as above |
| `resource_presence.v1` (`relay/implementation.rs:1271`) | `resource_presence_local` (read-only absence probe; sender `relay/implementation.rs:2450`) | any → socket | I | C7 decides (listed as a diagnostic-class survivor or deleted) |

## 7. Room dispatch (`interpret/room_dispatch.rs`)

`dispatch_to_room` (`:89`) is called from `interpret/implementation.rs:309` and `interpret/relay_plan.rs:28`. The two routing branches (plan and execute) move to B1 in **B2a**. The local path and the fenced-fanout backstop stay in place and follow the slices in the table.

| Branch | Lines | Decision | Class | Slice |
|---|---|---|---|---|
| Plan phase | `:127-132` → `plan_remote_room` `:1064-1142` | Reads `current_claim(RoomActor)` (`:1087`). `None` → local (`:1095`). Lookup error or stale lease → `resource-constraint`. Owner = self → local. Otherwise `RoomExecutionPath::Remote`, captures `IngressEffectIntent::DispatchToRoomRemote` and plans `ExternalRoomEffect::RelayMucProxy{GroupchatMessage}` (`:1129-1140`), executed by `effects/room_immediate.rs:95`. | routing decision | B2a |
| Execute phase | `:134-244` | `try_proxy_muc_remote_decision` (`:152`). `Delivered` → replies. `Unavailable`/`Dropped` → bounce. `PendingFrames`/`MaybeCommitted`/`JoinMaybeCommitted` → capture route. `RoomClaimUnavailable`/`OriginUnavailable` → bounce. `LocalRoom`/`RoomUnclaimed` → fall through (`:241`). | routing decision | B2a |
| Local path | `:260-349` | `GetRoom` (`:261`) → `GetRoomSnapshot` (`:309`) → `RoomExecutionPath::Local{…}` (`:339`) | authority (room actor) | keeps; E3 |
| Fenced-fanout backstop | `:363-420` | `check_fenced_fanout` (`:372`). `Ok(false)` → `DemoteRoomIfExactActor` + bounce. `Err` **fails open** (`:411-418`). | authority check | W1.4 (fail closed); C2/C4 replace |
| Helpers | `capture_delivered_remote_room_route` `:852`, `push_sender_error_reply` `:867`, `bind_room_claim_fence` `:39` (also `bot.rs:240`), `order_subject_persistence_after_archive` `:64` (also `bot.rs:248`) | — | — | B2a moves the remote-route capture |

## 8. Local-versus-remote routing decision points outside `clustering/`

This is the baseline for program rule 1 (bypass freeze). A PR that adds a row here before B1 exists must list it as an exception against this table.

**Not counted here: `hosting`-based local-versus-mirror branches.** These are:

- the readers of `ConnectionEntry::is_locally_hosted` in the §2.2 `hosting` row, e.g. `route_to_connection/implementation.rs:1469`, `bot.rs:756`, `execute_relay_copy.rs:34,145`, `pending_delivery/flush.rs:194,807`;
- the four `ConnectionRegistry::is_locally_hosted` calls in §1.3: `recovery_reachability.rs:67,89` and `route_bridge/presence.rs:49,67`.

They distinguish a local socket from a remote-hosted mirror entry, not a local owner from a remote one. They are class O, are deleted with the mirrors at ACT-DEF, and are not rule-1 decision points or exceptions.

### 8.1 Direct `OrderedRelayDeliveryBridge` calls (type-resolved)

Rows 11 (`http.rs`, construction) and 12 (mirror janitor) are not routing decisions. The other **41 rows in 23 files** are. The plan's B2e ("extension room send, `muc_admin_relay`") covers rows 10 and 23.

| # | File:line | Enclosing fn | Bridge method | B2 cluster |
|---|---|---|---|---|
| 1 | `interpret/carbons.rs:170` | `relay_carbons_only` | `try_fanout_remote_user_carbons` | B2c |
| 2 | `interpret/carbons.rs:557` | `try_deliver_registered_remote_resource` | `try_deliver_registered_remote_resource` | B2c |
| 3 | `interpret/effects/room_immediate.rs:95` | `execute_external` | `try_proxy_muc_remote` | B2a |
| 4 | `interpret/recipient_selection.rs:136` | `recipient_inventory` | `recipient_inventory_remote` | B2b |
| 5 | `interpret/room_dispatch.rs:152` | `dispatch_to_room` | `try_proxy_muc_remote_decision` | B2a |
| 6 | `interpret/route_to_connection/implementation.rs:1225` | `deliver_full_jid_via_ordered_relay_at` | `try_deliver_full_jid_remote_at` | B2b |
| 7 | `interpret/route_to_connection/implementation.rs:1374` | `deliver_direct_to_full_with_registered_remote_at` | `try_deliver_processed_full_jid_remote` | B2b |
| 8 | `interpret/route_to_connection/implementation.rs:1611` | `deliver_registered_remote_resource_at` | `try_deliver_registered_remote_resource_at` | B2b |
| 9 | `interpret/route_to_connection/implementation.rs:1658` | `deliver_bare_jid_via_ordered_relay` | `try_deliver_bare_jid_remote` | B2b |
| 10 | `server/extension_host_adapter/remote_room.rs:202` | `relay_room_send` | `extension_room_handle` | B2e |
| 11 | `server/http.rs:1319` | `create_websocket_state` | `wire` | n/a (construction) |
| 12 | `server/session_janitors/remote_owner.rs:35` | `spawn_remote_owner_mirror_janitor` | `sweep_remote_owner_resources` | ACT-DEF (mirror janitor; O) |
| 13 | `ws/cleanup.rs:107` | `unregister_remote_user_resource_if_owner` | `unregister_remote_user_resource_if_owner` | B2f |
| 14 | `ws/cleanup.rs:3465` | `cleanup_remote_muc_presence` | `try_proxy_muc_remote_decision` | B2f |
| 15 | `ws/cleanup.rs:3485` | `cleanup_remote_muc_presence` | `cleanup_muc_via_user_owner` | B2f |
| 16 | `ws/cleanup.rs:3667` | `clustered_cleanup_origin` | `remote_resource_origin_if_owner` | B2f |
| 17 | `ws/frame.rs:774` | `ordered_relay_origin_for_inbound_stanza` | `remote_resource_origin_if_owner` | B2f |
| 18 | `ws/handlers/iq/blocking.rs:222` | `mirror_remote_blocklist_interest` | `update_remote_user_resource_if_owner` | B2c |
| 19 | `ws/handlers/iq/blocking.rs:429` | `try_deliver_registered_remote_resource` | `try_deliver_registered_remote_resource` | B2c |
| 20 | `ws/handlers/iq/blocking.rs:463` | `try_remote_owner_blocklist_push` | `try_fanout_remote_user_blocklist_push` | B2c |
| 21 | `ws/handlers/iq/full_jid_forward.rs:64` | `route_full_jid_iq` | `try_deliver_full_jid_remote` | B2d |
| 22 | `ws/handlers/iq/full_jid_forward.rs:77` | `route_full_jid_iq` | `try_deliver_registered_remote_resource` | B2d |
| 23 | `ws/handlers/iq/muc_admin_relay.rs:185` | `ask_owner` | `muc_admin_query_handle` | B2e |
| 24 | `ws/handlers/iq/muc_owner_moderation.rs:608` | `try_deliver_registered_remote_destroy_notification` | `try_deliver_registered_remote_resource` | B2d |
| 25 | `ws/handlers/iq/roster/implementation.rs:137` | `mirror_remote_roster_interest` | `update_remote_user_resource_if_owner` | B2c |
| 26 | `ws/handlers/iq/roster/implementation.rs:362` | `try_remote_owner_roster_push` | `try_fanout_remote_user_roster_push` | B2c |
| 27 | `ws/handlers/iq/roster/push.rs:187` | `try_deliver_registered_remote_resource` | `try_deliver_registered_remote_resource` | B2c |
| 28 | `ws/handlers/iq/sans_io.rs:807` | `relay_muji_to_room_owner` | `try_proxy_muc_remote_decision` | B2a (Muji) |
| 29 | `ws/handlers/iq/sans_io.rs:1112` | `mirror_remote_carbons_update` | `update_remote_user_resource_if_owner` | B2c (carbons mirror) |
| 30 | `ws/handlers/presence/implementation.rs:215` | `handle_presence_impl` | `try_proxy_muc_remote` | B2a |
| 31 | `ws/handlers/presence/muc/implementation.rs:504` | `registered_remote_resource_delivery` | `try_deliver_registered_remote_resource` | B2a (MUC presence fan-out to registered remote resource) |
| 32 | `ws/handlers/presence/muc/implementation.rs:544` | `registered_remote_resource_write_accepted_delivery` | `try_deliver_registered_remote_resource_write_accepted` | B2a (MUC presence fan-out to registered remote resource) |
| 33 | `ws/handlers/presence/muc/implementation.rs:1228` | `handle_muc_join_unlocked` | `try_proxy_muc_remote` | B2a |
| 34 | `ws/handlers/presence/muc/implementation.rs:1904` | `handle_muc_leave` | `try_proxy_muc_remote` | B2a |
| 35 | `ws/handlers/presence/regular.rs:202` | `mirror_remote_presence_update` | `update_remote_user_resource_if_owner` | B2c (presence mirror; not named in plan) |
| 36 | `ws/handlers/presence/subscription/delivery.rs:508` | `try_route_presence_to_bare_remote` | `try_deliver_bare_jid_remote` | B2b |
| 37 | `ws/handlers/presence/subscription/delivery.rs:541` | `try_route_presence_to_full_remote` | `try_deliver_full_jid_remote` | B2b |
| 38 | `ws/handlers/presence/subscription/implementation.rs:103` | `try_handle_remote_subscription_presence` | `try_deliver_bare_jid_remote` | B2b |
| 39 | `ws/handlers/roster_fanout.rs:147` | `try_live_resource` | `try_deliver_registered_remote_resource` | B2c |
| 40 | `ws/registration.rs:114` | `prepare_socket_bind` | `retire_remote_incumbent_before_local_bind` | B2f |
| 41 | `ws/registration.rs:268` | `unregister_remote_clustered_resource_if_owner` | `unregister_remote_user_resource_if_owner` | B2f |
| 42 | `ws/registration.rs:833` | `register_remote_clustered_resource` | `try_register_remote_user_resource` | B2f |
| 43 | `ws/stream_management/registration.rs:25` | `unregister_remote_user_resource_if_owner` | `unregister_remote_user_resource_if_owner` | B2f |

### 8.2 Inline `current_claim` branches (waddle-server, outside `clustering/`)

| # | File:line | Enclosing fn | Decision | B2 cluster |
|---|---|---|---|---|
| 1 | `interpret/room_dispatch.rs:1087` | `plan_remote_room` | room owner local/remote at plan time | B2a |
| 2 | `interpret/route_to_connection_plan.rs:88` | `remote_owner` | UserActor owner remote? (helper; callers in §8.3) | B2b |
| 3 | `interpret/bot.rs:802` | `extension_room_owner` | room owner (helper; callers in §8.3) | B2e |
| 4 | `ws/cleanup.rs:3781` | `remote_muc_cleanup_claim_available` | can a remote MUC cleanup proceed | B2f |
| 5 | `srv/call_teardown_outbox/drain.rs:367` | `room_is_globally_unclaimed` | durable drain ownership | B2e |
| 6 | `srv/call_teardown_outbox/drain.rs:433` | `room_has_no_claim_at_all` | durable drain ownership | B2e |
| 7 | `srv/ingress/recovery_departed.rs:578` | `hosted_elsewhere` | departed-room recovery target | B2e |
| 8 | `srv/room_effect_outbox/drain.rs:874` | `room_claim_presence` | durable drain ownership (with `LocalRoomJids`, `drain.rs:850-895`) | B2e |
| 9 | `server/routes/livekit_grant_relay.rs:144` | `reassert_on_claim_owner` | then a direct `RelayHandle::reassert_media_grants` (`:173-177`) | B2e |

The 16 further `current_claim` calls inside waddle-xmpp are claim-protocol internals of the room registry, user registry and SM session registry, not handler branches. They are out of scope for B2:

- `room_registry_actor/implementation.rs:1171,1533,1617`
- `user_registry.rs:233`
- `stream_management/session_registry/{append.rs:172, claims.rs:966,1078,1461,2342, core.rs:1863,2051, cross_node_resume.rs:351,996, resources.rs:186}`
- `ownership/observe.rs:287`
- `ownership/claims.rs:574`

### 8.3 Branches on helper results

| File:line | Helper | B2 cluster |
|---|---|---|
| `interpret/route_to_connection/implementation.rs:1181` | `remote_owner` → `RelayFullJid` | B2b |
| `interpret/route_to_connection/implementation.rs:1634` | `remote_owner` → `RelayBareJid` | B2b |
| `interpret/route_to_connection_plan.rs:220` | `remote_owner` → `RelayFullJid` | B2b |
| `interpret/bot.rs:376` → `require_local_room` (`:824-834`) | `extension_room_owner`; bot room actions refuse when remote | B2e ("bot room owner") |
| `server/extension_host_adapter/remote_room.rs:94` (`send_room_routed`, called from `host_tools.rs:304`) | `extension_room_owner` → `extension_room_send.v1` | B2e |
| `ws/handlers/iq/muc_admin_relay.rs:135` (`ask_current_owner`) | `extension_room_owner` → `muc_admin_query.v1` | B2e |

These count as 7 decision-point sites. `bot.rs:376` and `bot.rs:828` are **one decision at two sites**: `:376` calls `require_local_room`, which branches at `:828`. Counting decisions instead of sites gives 58, not 59.

Wrapper helpers that *contain* bridge calls (rows in §8.1) and are reused elsewhere (B2b):

- `deliver_full_jid_via_ordered_relay[_at]`, `deliver_bare_jid_via_ordered_relay` and `deliver_peer_to_full_with_registered_remote`;
- reused from `ingress/execute_relay_copy.rs:118,123,175`, `ingress/execute_detached.rs:487`, `interpret/effects/delivery_immediate.rs:123,150` and `route_to_connection/implementation.rs:472,478,546,741,954,1169`.

These callers inherit the decision and are not counted separately.

### 8.4 Direct relay asks outside `clustering/` (not through the bridge)

| File:line | Ask | Cluster / fate |
|---|---|---|
| `srv/muc_durable/implementation.rs:2535` | `RelayHandle::demote` (`demote.v1`) to the previous room owner | surviving ask (DEC-2 lists `demote`); not a B2 move |
| `srv/ingress/execute/implementation.rs:150` | `RelayHandle::confirm_reply_receipt` (`confirm_reply_receipt.v1`) | relay-internal receipt; C7 |
| `server/routes/livekit_grant_relay.rs:173` | `RelayHandle::reassert_media_grants` | counted in §8.2 row 9 |

### 8.5 Totals

| Form | Points | Files (new) |
|---|---|---|
| Bridge calls (§8.1, excluding construction and janitor) | 41 | 23 |
| Inline `current_claim` (§8.2) | 9 | 6 new (`route_to_connection_plan.rs`, `bot.rs`, `call_teardown_outbox/drain.rs`, `recovery_departed.rs`, `room_effect_outbox/drain.rs`, `livekit_grant_relay.rs`) |
| Helper-result branches (§8.3) | 7 | 0 new |
| Direct relay asks (§8.4, excluding the one already in §8.2) | 2 | 2 new |
| **Total** | **59** | **31** |

By B2 cluster (sums to 59):

| Cluster | Points | Made up of |
|---|---|---|
| B2a | 9 | 8 bridge calls + `room_dispatch.rs:1087` |
| B2b | 12 | 8 bridge + `route_to_connection_plan.rs:88` + 3 `remote_owner` branches |
| B2c | 11 | 11 bridge (incl. the presence mirror `regular.rs:202`, not named in the plan) |
| B2d | 3 | 3 bridge |
| B2e | 12 | 2 bridge + 6 `current_claim` + 4 helper branches |
| B2f | 10 | 9 bridge + `cleanup.rs:3781` |
| Surviving or relay-internal ask | 2 | §8.4 |

## 9. Producer inventory per `OrderedRelayMucProxyKind` (input to C6a-pre)

`OrderedRelayMucProxyKind` (`clustering/ordered_relay.rs:153-167`) has 8 variants. 7 ride `OrderedRelayRoomLane::MucStanza`; `MujiJingleIq` rides `MujiSignaling` (`:174-184`). A **producer** is a production expression that constructs a variant and hands it to the bridge. Pattern matches, validation (`ordered_relay.rs:186-205,1340-1368`), the compat wire mirror (`relay/remote_resource_compat/wire.rs:631-668`) and outcome mapping (`route_bridge/delivery/mod.rs:121`, `relay/implementation.rs:455`, `route_bridge/delivery/muc/proxy.rs:211,327`) are **not** producers.

| Kind | Lane | Production producers (file:line, fn) | Owner-side consumer | C6a-pre disposition |
|---|---|---|---|---|
| `JoinPresence` | MucStanza | `handlers/presence/muc/implementation.rs:1231` `handle_muc_join_unlocked` (via `try_proxy_muc_remote` :1228); `route_bridge/delivery/muc/proxy.rs:393` `try_proxy_muc_join_repair` (bridge-internal retry of a maybe-committed join on the UserActor repair channel) | `delivery/muc/reserved.rs:40-44` `deliver_reserved_muc_join` | durable record needed (join; `admission = None` today) |
| `OccupantPresence` | MucStanza | `handlers/presence/implementation.rs:218` `handle_presence_impl` (non-join presence to an occupant JID); `handlers/presence/muc/implementation.rs:1907` `handle_muc_leave`; `websocket/cleanup.rs:3468` `cleanup_remote_muc_presence` (disconnect leave); `route_bridge/delivery/muc/cleanup.rs:107` `cleanup_muc_on_user_owner` (user-owner side of `muc_cleanup.v1`) | `reserved.rs:60-66` `deliver_reserved_muc_occupant_presence` (leave or update, `:111-120`) | durable record needed (leave / presence; `admission = None`) |
| `GroupchatMessage` | MucStanza | `interpret/room_dispatch.rs:155` `dispatch_to_room` (execute-phase decision); `interpret/room_dispatch.rs:1133` `plan_remote_room` (planned `ExternalRoomEffect::RelayMucProxy`, executed by `effects/room_immediate.rs:95`) | `reserved.rs:45-58` `deliver_reserved_muc_groupchat` | already keyed (`MessageKey`); durable record = canonical message |
| `MujiJingleIq` | MujiSignaling | `handlers/iq/sans_io.rs:810` `relay_muji_to_room_owner` | `reserved.rs:67-71` `deliver_reserved_muji_iq` | C6b (Muji lane), not C6a-pre |
| `PrivateMessage` | MucStanza | **none** | none: `reserved.rs:73` returns `ParseFailure` | no production producer; delete or keep with a recorded reason |
| `BareRoomIq` | MucStanza | **none** (test-only: `tests/clustering_ordered_relay.rs:1114,1126`) | none: `ParseFailure` | as above |
| `OccupantIq` | MucStanza | **none** (test-only: `tests/clustering_ordered_relay.rs:1138,1150`) | none: `ParseFailure` | as above |
| `FanoutChunk` | MucStanza | **none** | none: `ParseFailure` | as above |

This confirms the plan's provisional list in C6a-pre: `PrivateMessage`, `BareRoomIq`, `OccupantIq` and `FanoutChunk` have no production producer **and no owner-side consumer**. An envelope carrying one is NACKed `ParseFailure` by `deliver_reserved` (`reserved.rs:73`).

Consequences for C6a-pre:

- **Deleting a variant is a wire change.** `OrderedRelayMucProxyKind` is serialized inside `RemoteStanzaEnvelope`, so per `server/AGENTS.md:23` the change needs a `deliver_ordered.vN` bump. The frozen `MucKind` mirror in `remote_resource_compat/wire.rs:631-641` (used by `remote_resource_route.v8` / `remote_resource_frame.v3`) must keep its variants until those IDs retire in C7, or it must map the deleted ones to an error.
- **Room-bound traffic outside the `MucStanza` lane.** This traffic reaches a remote room owner without a `MucStanza` kind, so a whole-lane MUC flip (ACT-ACCEPT) does **not** cover it:
  - `extension_room_send.v1`: extension/bot room messages (`extension_host_adapter/host_tools.rs:306`, `remote_room.rs:85-215`).
  - `muc_admin_query.v1`: read-only admin lists (`handlers/iq/muc_admin_relay.rs:85,135`).
  - `muc_cleanup.v1`: socket node to user owner, which then produces `OccupantPresence`.
  - `demote.v1`: `muc_durable/implementation.rs:2535`.
  - `reassert_media_grants.v1`: `livekit_grant_relay.rs:173-177`.

  C6a must either list them as surviving relay asks (DEC-2/C7) or give them lane forms.

## 10. Producers that await capacity on a connection's outbound channel (`try_send` conversion owners)

The registry methods below `.await` a bounded `mpsc` (`OutboundStanza`, capacity 256, `connection.rs:162,267`):

- `send_to` (`sending.rs:179`);
- `send_to_with_write_acceptance` (`:125`);
- `send_to_if_owner` (`:230`);
- `send_pending_flush` (`:260`);
- `send_pending_flush_if_owner` (`:300`);
- `reserve_pending_flush` (`reserve_owned().await`, `:30-53`);
- `send_to_many` (`resources.rs:75`, which has no production caller).

`try_send_to`, `try_send_outbound_*`, `capture_owned_enqueue_if_owner`, `try_reserve_pending_flush` and the UserActor's `try_send_archive_ordered` are already non-blocking and need no conversion.

No production code awaits `ConnectionEntry.sender` directly: the only out-of-registry use is `UserActor::try_deliver` → `try_send_archive_ordered` (`user_actor/delivery.rs:56`).

Slice rules applied:

- Handler-inline producers go to their family: D2b MUC/room; D2c roster/presence/subscription including presence probes and caps queries; D2d IQ forwards/PEP/pubsub including `admin/channels.rs`; D2e storage-heavy.
- Carbons copies and blocklist pushes go to **D2c**. final.md rule text also permits B2c. But B2c's row is "prod delta: none" and precedes D2a, whose row keeps producer semantics unchanged, so converting producers there would contradict both rows. B2c moves only their routing.
- Background and durable producers go to D2p.
- `StanzaRouter` (`waddle-xmpp/src/routing.rs`) is exported (`lib.rs:69`) but never constructed in production. Delete it instead of converting it.

| # | File:line | Enclosing fn | Method | Context | Proposed slice |
|---|---|---|---|---|---|
| 1 | `interpret/carbons.rs:420` | `send_carbons_to_registry_with_capture` | `send_to` | carbons copy (B2c moves its routing but is prod-delta none) | D2c |
| 2 | `interpret/effects/invite.rs:55` | `execute` | `send_to` | MUC mediated invite | D2b |
| 3 | `interpret/groupchat_archive.rs:1080` | `push_inbox_update_to` | `send_to` | inbox update push (storage path) | D2e |
| 4 | `interpret/offline_delivery.rs:203` | `bounce_offline_quota` | `send_to` | offline-quota bounce (storage path) | D2e |
| 5 | `srv/admin/channels.rs:2979` | `broadcast_admin_config_change` | `send_to` | admin HTTP broadcasts (plan assigns admin/channels.rs to D2d) | D2d |
| 6 | `srv/admin/channels.rs:3440` | `broadcast_presence_updates` | `send_to` | admin HTTP broadcasts (plan assigns admin/channels.rs to D2d) | D2d |
| 7 | `srv/admin/channels.rs:5327` | `run_kick` | `send_to` | admin HTTP broadcasts (plan assigns admin/channels.rs to D2d) | D2d |
| 8 | `srv/pending_delivery/flush.rs:621` | `flush_for_resource_inner` | `reserve_pending_flush` | pending_delivery flush (durable row stays offered/unoffered) | D2p |
| 9 | `srv/pending_delivery/flush.rs:701` | `flush_for_resource_inner` | `send_pending_flush_if_owner` | pending_delivery flush (durable row stays offered/unoffered) | D2p |
| 10 | `srv/pending_delivery/flush.rs:712` | `flush_for_resource_inner` | `send_pending_flush` | pending_delivery flush (durable row stays offered/unoffered) | D2p |
| 11 | `srv/pending_delivery/flush.rs:728` | `flush_for_resource_inner` | `send_to_if_owner` | pending_delivery flush (durable row stays offered/unoffered) | D2p |
| 12 | `srv/pending_delivery/flush.rs:729` | `flush_for_resource_inner` | `send_to` | pending_delivery flush (durable row stays offered/unoffered) | D2p |
| 13 | `srv/room_effect_outbox/drain.rs:608` | `queue_local` | `send_to_with_write_acceptance` | room-effect outbox drain (durable producer: keep custody, retry) | D2p |
| 14 | `srv/sm_promotion/implementation.rs:1234` | `promote_one` | `send_to` | SM promotion redelivery (background) | D2p |
| 15 | `srv/sm_promotion/pending.rs:204` | `return_quota_bounce` | `send_to` | SM promotion redelivery (background) | D2p |
| 16 | `srv/sm_promotion/stanza.rs:43` | `promote_iq` | `send_to` | SM promotion redelivery (background) | D2p |
| 17 | `srv/sm_promotion/stanza.rs:66` | `promote_presence` | `send_to` | SM promotion redelivery (background) | D2p |
| 18 | `ws/handlers/iq/blocking.rs:407` | `send_blocking_pushes` | `send_to` | blocklist push (roster/presence family; B2c is prod-delta none) | D2c |
| 19 | `ws/handlers/iq/full_jid_forward.rs:101` | `route_full_jid_iq` | `send_to` | IQ full-JID forward | D2d |
| 20 | `ws/handlers/iq/muc_admin.rs:1846` | `handle_muc_admin_iq` | `send_to` | MUC admin IQ notifications | D2b |
| 21 | `ws/handlers/iq/muc_admin.rs:1870` | `handle_muc_admin_iq` | `send_to` | MUC admin IQ notifications | D2b |
| 22 | `ws/handlers/presence/probe.rs:80` | `handle_presence_probe` | `send_to` | presence probe answers | D2c |
| 23 | `ws/handlers/presence/probe.rs:116` | `handle_presence_probe` | `send_to` | presence probe answers | D2c |
| 24 | `ws/handlers/presence/probe.rs:164` | `handle_presence_probe` | `send_to` | presence probe answers | D2c |
| 25 | `ws/handlers/presence/probe.rs:200` | `send_unsubscribed_probe_response` | `send_to` | presence probe answers | D2c |
| 26 | `ws/handlers/presence/regular.rs:344` | `spawn_session_recovery_delivery` | `send_to_if_owner` | spawned session-recovery delivery task | D2p |
| 27 | `ws/handlers/presence/regular.rs:466` | `broadcast_presence_to_subscribers` | `send_to` | presence broadcast / caps query | D2c |
| 28 | `ws/handlers/presence/regular.rs:547` | `resolve_caps_for_presence` | `send_to` | presence broadcast / caps query | D2c |
| 29 | `ws/handlers/presence/subscription/delivery.rs:66` | `send_existing_subscription_ack` | `send_to` | subscription delivery | D2c |
| 30 | `ws/handlers/presence/subscription/delivery.rs:99` | `send_existing_subscription_ack` | `send_to` | subscription delivery | D2c |
| 31 | `ws/handlers/presence/subscription/delivery.rs:250` | `send_presence_stanza_to_jid` | `send_to` | subscription delivery | D2c |
| 32 | `ws/handlers/presence/subscription/delivery.rs:452` | `send_stanza_to_available_user_resources_and_detached_available` | `send_to` | subscription delivery | D2c |
| 33 | `ws/handlers/presence/subscription/delivery.rs:481` | `send_stanza_to_available_user_resources_and_detached_available` | `send_to` | subscription delivery | D2c |
| 34 | `ws/handlers/presence/subscription/delivery.rs:633` | `record_stanza_for_detached_available_resources_excluding` | `send_to` | subscription delivery | D2c |
| 35 | `ws/handlers/presence/subscription/delivery.rs:694` | `record_subscription_stanza_for_detached_resources_excluding` | `send_to` | subscription delivery | D2c |
| 36 | `ws/handlers/presence/subscription/directed.rs:36` | `handle_directed_presence` | `send_to` | subscription delivery | D2c |
| 37 | `ws/handlers/presence/subscription/directed.rs:62` | `handle_directed_presence` | `send_to` | subscription delivery | D2c |
| 38 | `ws/handlers/presence/subscription/implementation.rs:304` | `handle_subscription_presence` | `send_to` | subscription delivery | D2c |
| 39 | `xmpp/routing.rs:275` | `route_presence_local` | `send_to` | `StanzaRouter` has no production constructor | delete |
| 40 | `xmpp/routing.rs:297` | `route_presence_local` | `send_to` | `StanzaRouter` has no production constructor | delete |
| 41 | `xmpp/routing.rs:373` | `route_iq_local_unchecked` | `send_to` | `StanzaRouter` has no production constructor | delete |
| 42 | `xmpp/routing.rs:390` | `route_iq_local_unchecked` | `send_to` | `StanzaRouter` has no production constructor | delete |

| Proposed slice | Sites |
|---|---|
| D2b | 3 |
| D2c | 18 |
| D2d | 4 |
| D2e | 2 |
| D2p | 11 |
| delete | 4 |

Totals: 42 awaiting call sites. **38 are live, in 18 files.** The other 4 are dead (`StanzaRouter`). Of the 38 live sites, 33 are `send_to`/`send_to_with_write_acceptance`, matching DEC-3's "33 production callers in 18 files". The other 5 are `send_to_if_owner` ×2, `send_pending_flush`, `send_pending_flush_if_owner` and `reserve_pending_flush`, all in `pending_delivery/flush.rs` and `presence/regular.rs`, both already counted among the 18 files. D2p's list in the plan ("`pending_delivery` flush, `sm_promotion`, `room_effect_outbox` drain") also needs `presence/regular.rs:344` (`spawn_session_recovery_delivery`, a spawned task). Test code has 14 textual `.send_to*(` lines.

## 11. Destination-classification guards in #2049, #2054, #2063 and #2064

These four #2047 follow-ups were not present on the inventoried `main@db6e5338`. They add **handler-level destination classification**: a foreign domain versus the local domain and configured service domains. Since the inventory snapshot, #2049 merged as `e3b22c39`; check the current status of the remaining PRs before using this section. Their changes:

- #2049 adds `handlers/iq/ping_destination.rs` (later renamed). It rejects a remote XEP-0199 ping with `cancel/not-allowed` before the local namespace handler answers.
- #2054 generalises it to `handlers/iq/remote_destination.rs`: `handle_sans_io_iq_with_remote_preflight`, routed through `iq/mod.rs` as `handle_sans_io_iq`, covers ping and carbons.
- #2063 adds `remote_iq_error` calls to `disco_info.rs` (after the target parse, `+12` lines) and `disco_items.rs` (`+11`).
- #2064 adds legacy `session` to the preflight set.

Production surface after #2064:

- one new module (`remote_destination.rs`, 98 lines);
- three call sites: `iq/mod.rs` dispatcher alias, `disco_info.rs`, `disco_items.rs`;
- a fixed allowlist of local domains: the server domain, the `muc`/`spaces`/`upload`/`community`/`extensions`/`push` services and the calls mixer.

Classification for this inventory: these are **routing decisions by destination domain, not by owning node**. They add no bridge call, no `current_claim` branch and no remote message ID, so they do not breach rule 1 as written. They are still per-handler destination branches, of the kind #2047's S2S `DeliveryService` route variant is meant to absorb. Recommendation: when B1 lands, move `remote_iq_error`'s domain classification into the B1 address type as a `Foreign` destination, and have these handlers consume the typed outcome. Record them as B2d rows (IQ family), not new exceptions.

To regenerate the PR diffs: `git diff origin/main...pr/<n> -- server/crates ':!*test*'`.

## Appendix A. Regenerating

1. Check out `db6e5338` and copy the three scripts below into a scratch directory.
2. Run the §0.1 commands. `parse_check.py` needs **two trees**:
   - `PATCHED_ROOT`: the marker-bearing snapshot taken after `patch_deprecate.py` and before the revert;
   - `ORIG_ROOT`: the unmodified `db6e5338` checkout.

   Passing the reverted tree as `PATCHED_ROOT` silently shifts every site in the 35 marker-bearing files by the number of markers above it.
3. Classes and slices per method or field are in the tables themselves. Rows that need judgement name the call site:
   - the slice for each awaiting producer (§10);
   - the B2 cluster for each decision point (§8);
   - the D3s verdict (§2.1).

`cargo check` with the markers took 22m41s on the shared 2-CPU builder. The JSON output has one `deprecated` warning per site; spans that start at the receiver expression are mapped to the field on the following lines.

## Appendix B. Scripts

<details><summary>B.1 <code>testregions.py</code>: production/test classifier for rg hits</summary>

```python
#!/usr/bin/env python3
"""Classify `path:line[:...]` rg hits as prod / test / test-support.

Usage: rg -n PATTERN server/crates ... | python3 testregions.py [--prod|--test|--tag]

A hit is TEST when any of:
  * the file sits under a `tests/`, `benches/` or `examples/` directory;
  * the file is `tests.rs`, `*_tests.rs`, `tests_*.rs`, `test_*.rs` or lives in
    a `tests/` module directory, or its module is declared behind
    `#[cfg(test)]` / `#[cfg(all(test, ...))]` in its parent;
  * the line falls inside an item (mod / fn / impl / use / struct ...) that is
    preceded by a `#[cfg(test)]`-family attribute.
A hit is TEST-SUPPORT when the gating attribute is
`#[cfg(any(test, feature = "test-support"))]` or `#[cfg(feature = "test-support")]`
(compiled into production builds only with the `test-support` feature).
Everything else is PROD.
"""
import os
import re
import sys
from functools import lru_cache

ROOT = os.getcwd()
TEST_ATTR = re.compile(r'#\[cfg\((test|all\(test\b[^\]]*|all\([^\]]*\btest\b[^\]]*)\)\]')
SUPPORT_ATTR = re.compile(r'#\[cfg\((any\(test, *feature *= *"test-support"\)|feature *= *"test-support"|any\(feature *= *"test-support", *test\))\)\]')


def strip_code(text):
    """Return text with comments/strings/char literals blanked (same length, newlines kept)."""
    out = []
    i, n = 0, len(text)
    while i < n:
        c = text[i]
        if c == '/' and i + 1 < n and text[i + 1] == '/':
            j = text.find('\n', i)
            j = n if j == -1 else j
            out.append(' ' * (j - i))
            i = j
        elif c == '/' and i + 1 < n and text[i + 1] == '*':
            depth, j = 1, i + 2
            while j < n and depth:
                if text.startswith('/*', j):
                    depth += 1; j += 2
                elif text.startswith('*/', j):
                    depth -= 1; j += 2
                else:
                    j += 1
            out.append(''.join(ch if ch == '\n' else ' ' for ch in text[i:j]))
            i = j
        elif c == 'r' and re.match(r'r#*"', text[i:i + 10]) and (i == 0 or not (text[i - 1].isalnum() or text[i - 1] == '_')):
            m = re.match(r'r(#*)"', text[i:])
            hashes = m.group(1)
            end = text.find('"' + hashes, i + len(m.group(0)))
            end = n if end == -1 else end + 1 + len(hashes)
            out.append(''.join(ch if ch == '\n' else ' ' for ch in text[i:end]))
            i = end
        elif c == '"':
            j = i + 1
            while j < n and text[j] != '"':
                j += 2 if text[j] == '\\' else 1
            j += 1
            out.append(''.join(ch if ch == '\n' else ' ' for ch in text[i:j]))
            i = j
        elif c == "'":
            if i + 1 < n and text[i + 1] == '\\':
                j = text.find("'", i + 2)
                j = n if j == -1 else j + 1
                out.append(' ' * (j - i)); i = j
            elif i + 2 < n and text[i + 2] == "'":
                out.append('   '); i += 3
            else:
                out.append(c); i += 1
        else:
            out.append(c); i += 1
    return ''.join(out)


def item_extent(code, start):
    """From offset `start` (just after attributes), return end offset of the item."""
    n = len(code)
    i = start
    depth_paren = 0
    while i < n:
        c = code[i]
        if c in '([':
            depth_paren += 1
        elif c in ')]':
            depth_paren -= 1
        elif c == ';' and depth_paren == 0:
            return i + 1
        elif c == '{' and depth_paren == 0:
            depth = 1; i += 1
            while i < n and depth:
                if code[i] == '{':
                    depth += 1
                elif code[i] == '}':
                    depth -= 1
                i += 1
            return i
        i += 1
    return n


@lru_cache(maxsize=None)
def regions(path):
    """List of (start_line, end_line, kind) gated ranges in file."""
    try:
        text = open(path, encoding='utf-8', errors='replace').read()
    except OSError:
        return ()
    code = strip_code(text)
    res = []
    for m in re.finditer(r'#\[cfg\([^\n]*\)\]', code):
        attr = text[m.start():m.end()]
        if SUPPORT_ATTR.fullmatch(attr):
            kind = 'test-support'
        elif TEST_ATTR.fullmatch(attr):
            kind = 'test'
        else:
            continue
        # skip further attributes / whitespace
        j = m.end()
        while True:
            k = j
            while k < len(code) and code[k] in ' \t\r\n':
                k += 1
            if code.startswith('#[', k):
                d, k2 = 0, k + 1
                while k2 < len(code):
                    if code[k2] == '[':
                        d += 1
                    elif code[k2] == ']':
                        d -= 1
                        if d == 0:
                            break
                    k2 += 1
                j = k2 + 1
                continue
            j = k
            break
        end = item_extent(code, j)
        sl = text.count('\n', 0, m.start()) + 1
        el = text.count('\n', 0, end) + 1
        res.append((sl, el, kind))
    return tuple(res)


@lru_cache(maxsize=None)
def gated_module_files():
    """Files whose `mod x;` declaration is cfg(test)/test-support gated."""
    out = {}
    for base, _dirs, files in os.walk(os.path.join(ROOT, 'server', 'crates')):
        for f in files:
            if not f.endswith('.rs'):
                continue
            p = os.path.join(base, f)
            text = open(p, encoding='utf-8', errors='replace').read()
            for m in re.finditer(r'(#\[cfg\([^\n]*\)\])\s*(?:#\[[^\n]*\]\s*)*(?:pub(?:\([a-z]+\))? )?mod (\w+);', text):
                attr = m.group(1)
                kind = 'test-support' if SUPPORT_ATTR.fullmatch(attr) else ('test' if TEST_ATTR.fullmatch(attr) else None)
                if not kind:
                    continue
                name = m.group(2)
                stem = f[:-3]
                d = base if stem in ('mod', 'lib', 'main') else os.path.join(base, stem)
                for cand in (os.path.join(d, name + '.rs'), os.path.join(d, name, 'mod.rs')):
                    out[os.path.normpath(cand)] = kind
                # whole subtree of name/ also gated
                out[os.path.normpath(os.path.join(d, name)) + os.sep] = kind
    return out


def file_kind(path):
    ap = os.path.normpath(os.path.join(ROOT, path))
    parts = path.split('/')
    if any(p in ('tests', 'benches', 'examples') for p in parts[:-1]):
        return 'test'
    fname = parts[-1]
    if fname == 'tests.rs' or fname.endswith('_tests.rs') or fname.startswith('tests_') or fname.startswith('test_'):
        return 'test'
    g = gated_module_files()
    if ap in g:
        return g[ap]
    for k, v in g.items():
        if k.endswith(os.sep) and ap.startswith(k):
            return v
    return 'prod'


def classify(path, line):
    fk = file_kind(path)
    if fk != 'prod':
        return fk
    best = 'prod'
    for sl, el, kind in regions(os.path.join(ROOT, path)):
        if sl <= line <= el:
            if kind == 'test':
                return 'test'
            best = kind
    return best


def main():
    mode = sys.argv[1] if len(sys.argv) > 1 else '--tag'
    for raw in sys.stdin:
        raw = raw.rstrip('\n')
        m = re.match(r'([^:]+):(\d+):(.*)', raw)
        if not m:
            continue
        kind = classify(m.group(1), int(m.group(2)))
        if mode == '--prod' and kind == 'prod':
            print(raw)
        elif mode == '--test' and kind != 'prod':
            print(raw)
        elif mode == '--tag':
            print(f'{kind}\t{raw}')


if __name__ == '__main__':
    main()
```
</details>

<details><summary>B.2 <code>patch_deprecate.py</code>: temporary deprecation markers</summary>

```python
#!/usr/bin/env python3
"""Temporarily mark inventory targets #[deprecated] so `cargo check` lists every
production use site (type-resolved). Run from the worktree root; revert with
`git checkout -- server/crates`.
"""
import re

X = 'server/crates/waddle-xmpp/src/'
S = 'server/crates/waddle-server/src/'


def read(p):
    return open(p).read()


def write(p, t):
    open(p, 'w').write(t)


def block_end(t, open_brace):
    d = 0
    i = open_brace
    while True:
        if t[i] == '{':
            d += 1
        elif t[i] == '}':
            d -= 1
            if d == 0:
                return i
        i += 1


def dep(tag):
    return f'#[deprecated(note = "W19|{tag}")]'


def mark_impl_fns(path, impl_re, tag, pub_only=False):
    t = read(path)
    out = []
    pos = 0
    for m in re.finditer(impl_re, t):
        ob = t.index('{', m.start())
        if ob < pos:
            continue
        end = block_end(t, ob)
        body = t[ob:end]
        def rep(fm):
            indent = fm.group(1)
            sig = fm.group(2)
            name = fm.group(3)
            return f'{indent}{dep(tag + "::" + name)}\n{indent}{sig}'
        pat = r'(?m)^(    )((?:pub(?:\([^)]*\))? )?(?:async )?fn (\w+))' if not pub_only else r'(?m)^(    )((?:pub(?:\([^)]*\))? )(?:async )?fn (\w+))'
        # skip fns directly under #[cfg(test)]
        lines = body.split('\n')
        newl = []
        for idx, line in enumerate(lines):
            mm = re.match(pat, line)
            if mm:
                prev = [l.strip() for l in newl[-3:]]
                if not any(p.startswith('#[cfg(test') for p in prev):
                    newl.append(f'    {dep(tag + "::" + mm.group(3))}')
            newl.append(line)
        out.append(t[pos:ob])
        out.append('\n'.join(newl))
        pos = end
    out.append(t[pos:])
    write(path, ''.join(out))


def mark_struct_fields(path, struct_re, tag, mark_type=False):
    t = read(path)
    m = re.search(struct_re, t)
    ob = t.index('{', m.start())
    end = block_end(t, ob)
    body = t[ob:end]
    lines = body.split('\n')
    newl = []
    for line in lines:
        mm = re.match(r'^(    )(pub(?:\([^)]*\))? )?(\w+):', line)
        if mm and mm.group(3) not in ('where',):
            prev = [l.strip() for l in newl[-1:]]
            if not any(p.startswith('#[cfg(test') for p in prev):
                newl.append(f'    {dep(tag + "." + mm.group(3))}')
        newl.append(line)
    head = t[:ob]
    if mark_type:
        head = head[:m.start()] + dep(tag + '::TYPE') + '\n' + head[m.start():]
    write(path, head + '\n'.join(newl) + t[end:])


def mark_enum_variants(path, enum_re, tag):
    t = read(path)
    m = re.search(enum_re, t)
    ob = t.index('{', m.start())
    end = block_end(t, ob)
    body = t[ob:end]
    lines = body.split('\n')
    newl = []
    for line in lines:
        mm = re.match(r'^(    )([A-Z]\w*)\s*[,({]?', line)
        if mm:
            newl.append(f'    {dep(tag + "::" + mm.group(2))}')
        newl.append(line)
    write(path, t[:ob] + '\n'.join(newl) + t[end:])


def mark_free_fns(path, names, tag):
    t = read(path)
    for n in names:
        t = re.sub(r'(?m)^(pub(?:\([^)]*\))? (?:async )?fn ' + n + r'\b)', dep(tag + '::' + n) + r'\n\1', t, count=1)
    write(path, t)


import os
cr = X + 'registry/connection_registry'
for f in [X + 'registry/connection_registry.rs'] + [f'{cr}/{n}' for n in sorted(os.listdir(cr)) if n != 'tests.rs']:
    mark_impl_fns(f, r'impl ConnectionRegistry \{', 'CR')
    if 'impl ConnectionEntry {' in read(f):
        mark_impl_fns(f, r'impl ConnectionEntry \{', 'CE')
mark_struct_fields(X + 'registry/connection_registry.rs', r'pub struct ConnectionRegistry \{', 'CRF', mark_type=True)
mark_struct_fields(cr + '/outbound.rs', r'pub struct ConnectionEntry \{', 'CEF', mark_type=True)
mark_struct_fields(S + 'server/routes/websocket/state.rs', r'pub\(super\) struct WsConnState \{', 'WS')
mark_free_fns(S + 'server/dual_registration.rs', ['mirror_register', 'mirror_register_outcome', 'mirror_unregister'], 'DUAL')
mark_enum_variants(S + 'clustering/ordered_relay.rs', r'pub enum OrderedRelayMucProxyKind \{', 'MUCKIND')
for root, _d, files in os.walk(S + 'clustering/route_bridge'):
    for n in files:
        p = os.path.join(root, n)
        if n.endswith('.rs') and 'impl OrderedRelayDeliveryBridge {' in read(p):
            mark_impl_fns(p, r'impl OrderedRelayDeliveryBridge \{', 'BRIDGE', pub_only=True)
# ClaimStore::current_claim (trait decl)
p = X + 'ownership/claims.rs'
t = read(p)
t = t.replace('    async fn current_claim(&self, entity: &Entity)', '    ' + dep('CLAIM::current_claim') + '\n    async fn current_claim(&self, entity: &Entity)', 1)
write(p, t)
print('patched')
```
</details>

<details><summary>B.3 <code>parse_check.py</code>: warning → use-site TSV</summary>

```python
#!/usr/bin/env python3
"""Parse `cargo check --message-format=json` deprecation warnings emitted for
the temporary W19 markers into TSV:
  tag  file  line(original)  col  enclosing_fn  source_line
Usage: parse_check.py check.json PATCHED_ROOT ORIG_ROOT > uses.tsv
PATCHED_ROOT = worktree that was compiled with markers; ORIG_ROOT = unmodified
checkout at the same SHA (line numbers are mapped back to it).
"""
import json
import os
import re
import sys
from functools import lru_cache

check, patched_root, orig_root = sys.argv[1:4]


@lru_cache(maxsize=None)
def patched_lines(rel):
    p = os.path.join(patched_root, rel)
    try:
        return open(p).read().split('\n')
    except OSError:
        return []


@lru_cache(maxsize=None)
def offsets(rel):
    """prefix count of marker lines, by patched line index (1-based)."""
    lines = patched_lines(rel)
    out = [0]
    c = 0
    for l in lines:
        if 'W19|' in l:
            c += 1
        out.append(c)
    return out


@lru_cache(maxsize=None)
def orig_lines(rel):
    try:
        return open(os.path.join(orig_root, rel)).read().split('\n')
    except OSError:
        return []


def enclosing(rel, ln):
    L = orig_lines(rel)
    for k in range(min(ln, len(L)) - 1, -1, -1):
        m = re.match(r'\s*(?:pub(?:\([^)]*\))? )?(?:const )?(?:async )?fn (\w+)', L[k])
        if m:
            return m.group(1)
    return '?'


def outer(span):
    while span.get('expansion') and not span['file_name'].startswith('crates/'):
        span = span['expansion']['span']
    return span


seen = set()
for raw in open(check):
    try:
        msg = json.loads(raw)
    except ValueError:
        continue
    if msg.get('reason') != 'compiler-message':
        continue
    m = msg['message']
    if not m.get('code') or m['code'].get('code') != 'deprecated':
        continue
    tm = re.search(r'W19\|([^"`]+)', m['message'])
    if not tm:
        continue
    tag = tm.group(1).strip()
    for span in m['spans']:
        if not span.get('is_primary'):
            continue
        span = outer(span)
        rel = os.path.join('server', span['file_name'])
        pl = span['line_start']
        ol = pl - offsets(rel)[pl] if offsets(rel) and pl < len(offsets(rel)) else pl
        key = (tag, rel, ol, span['column_start'])
        if key in seen:
            continue
        seen.add(key)
        L = orig_lines(rel)
        text = L[ol - 1].strip() if 0 < ol <= len(L) else ''
        print('\t'.join([tag, rel, str(ol), str(span['column_start']), enclosing(rel, ol), text[:160]]))
```
</details>
