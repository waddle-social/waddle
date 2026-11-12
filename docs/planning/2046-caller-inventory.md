# #2046 W1.9: caller inventory for the connection layer and routing decision points

Slice W1.9 of the #2046 plan (v9, §3 Wave 1). This document is the deletion and ownership source that B2a–f, D2b–e, D2p, D3s, E2, DEFprep, ACT-DEF and C7 cite. It inventories the following. **Production** means code compiled into `cargo build -p waddle-server --features clustering` (the flake's production build, `flake.nix:367`), excluding `cfg(test)` items and the `test-support` feature.

- Every production read, write and caller of `ConnectionRegistry` and `ConnectionEntry` (field by field).
- Every `WsConnState` field (46 at the generated commit).
- `dual_registration`.
- The user-resource mirrors.
- Room dispatch.
- The remote-resource relay endpoints.
- The local-versus-remote routing decision points outside `clustering/`.
- The producers of each `OrderedRelayMucProxyKind`.
- Every production producer that awaits capacity on a connection's outbound channel.

This document has two layers, and each section says which it is.

- **Current, generated.** Sections 1 to 3 and the site table before
  section 4 are generated from the source by
  [`2046-current-caller-scan.py`](2046-current-caller-scan.py) and name the
  commit they were generated at, in their first line. That commit is
  `origin/main@b31a39dbd` plus the metric call sites added in the same
  change set, which touch `ws/cleanup.rs` and no declaration listed here.
  They are the reference for fields, methods, classes and call sites. The
  scan is textual: it does not resolve types, so a method row also counts
  calls of a same-named method on another type, and a write through a
  destructured borrow is not seen. Regenerate with the command in
  Appendix A.
- **Baseline, type-resolved.** Sections 0 and 4 to 11 were produced at
  `main@db6e5338` with a compiler-marker pass. Their analysis and
  classifications stand; their line numbers, counts and description of the
  ownership token do not: the entry's `sender` is now `endpoint`, and the
  ownership token is `LocalConnectionBindingId`, not the carbons flag.
  Where the two layers disagree about a site, the generated layer is right.

The connection files (`ws/connection*.rs`, `ws/connection/`,
`ws/supervised_io*`, the connection registry endpoint files, the ordered
relay and stream-management persistence files) are being rewritten on other
branches. The generated sections describe main as it is, not those
branches; regenerate after each of them merges.

Nothing here changes code. Test code is excluded from the generated
sections and counted separately in section 0.

## Classification

Each read, write or call is classified relative to the [#2046 target authority model](https://github.com/waddle-social/waddle/issues/2046) (one owner per field):

| Class | Meaning | Typical fate |
|---|---|---|
| **A: authority** | The state is today's live source of truth for a decision, or the call reads or writes such state. Examples: presence, interest and carbons flags; SM state; pending subscribes. | Moves to exactly one owner: the connection actor (D2a) for socket-local truth, or the account actor (E2) for selection. Shared copies are deleted at ACT-DEF. |
| **I: derived index** | A lookup structure or published copy derivable from an authority. Examples: the `FullJid → ConnectionEntry` DashMap, the `sm_stream_owners` reverse index, entry copies of the SM id and occupancy generation, and ownership checks against the entry slot. | Stays as a derived, transport-only socket index after ACT-DEF, or is replaced by a control-channel query (DEFprep). |
| **T: transport concern** | Socket and channel plumbing. Examples: outbound sender, force-detach channel, write interruption, bind gate, socket lifecycle, send window, deferred inbound. | Owned by the D1 pumps and the D2a/D2r actor and retirement protocol. Producer semantics change in D2b–e/D2p. |
| **O: obsolete migration glue (legacy bypass)** | Exists only to keep two registries or two nodes' copies in step, or is dead. Examples: `dual_registration`, remote-hosted `ConnectionEntry` mirrors, `apply_remote_resource_state`, the carbons-flag ownership token, and the unused `StanzaRouter`. | Deleted: W1.3 for the token, ACT-DEF for mirrors and `dual_registration`, C7 for relay entry points. Dead code can be deleted at any time. |

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


<!-- generated by 2046-current-caller-scan.py: start -->

Generated at `e3a51bfee` by `docs/planning/2046-current-caller-scan.py --inventory`.
Sites are source lines that match textually outside test code, written
`file:line,line`. "Write-like" counts the sites where the field is assigned,
borrowed mutably on the same expression, or has a mutating method called on it.

## 1. `ConnectionRegistry` (`xmpp/registry/connection_registry.rs`)

### 1.1 Fields

| Field | Class | Declared | Sites | Write-like | Files | Every site |
| --- | --- | --- | --- | --- | --- | --- |
| `bind_slots` | A/T | `xmpp/registry/connection_registry.rs:74` | 6 | 2 | 1 | `xmpp/registry/connection_registry/binding.rs:203,209,220,240,261,268` |
| `connections` | I/T | `xmpp/registry/connection_registry.rs:78` | 59 | 4 | 6 | `xmpp/registry/connection_registry.rs:137,155,172,186,240`; `xmpp/registry/connection_registry/archive_dispatch.rs:195`; `xmpp/registry/connection_registry/connections.rs:25,70,93,173,211,265,301,323,336,372,398,414,466,472,483,488`; `xmpp/registry/connection_registry/presence.rs:12,40,81,144,184,200`; `xmpp/registry/connection_registry/resources.rs:14,29,41,49,65,79,91,99,134,146,163,187,203,214,232`; `xmpp/registry/connection_registry/sending.rs:151,185,221,255,276,326,345,391,424,469,524,540,608,654,702,731` |
| `pending_subscription_stanzas` | A | `xmpp/registry/connection_registry.rs:80` | 5 | 2 | 2 | `xmpp/registry/connection_registry.rs:215`; `xmpp/registry/connection_registry/subscriptions.rs:9,34,54,67` |
| `presence_states` | A/I | `xmpp/registry/connection_registry.rs:82` | 13 | 4 | 4 | `xmpp/registry/connection_registry.rs:221`; `xmpp/registry/connection_registry/connections.rs:100,305,343,379`; `xmpp/registry/connection_registry/presence.rs:87,110,123,128,150`; `xmpp/registry/connection_registry/sending.rs:655,709,732` |
| `last_activity` | A | `xmpp/registry/connection_registry.rs:84` | 4 | 2 | 2 | `xmpp/registry/connection_registry.rs:227`; `xmpp/registry/connection_registry/presence.rs:156,167,174` |
| `started_at` | T | `xmpp/registry/connection_registry.rs:86` | 1 | 0 | 1 | `xmpp/registry/connection_registry/presence.rs:179` |
| `sm_stream_owners` | I | `xmpp/registry/connection_registry.rs:92` | 16 | 2 | 3 | `xmpp/registry/connection_registry.rs:140,143,192,195,206`; `xmpp/registry/connection_registry/connections.rs:36,99,181,233,277,304,342,378`; `xmpp/registry/connection_registry/sending.rs:656,708,733` |

### 1.2 Methods

"Outside" counts call sites outside `xmpp/registry/connection_registry*`;
"inside" counts those within it. A receiver of another type with the same
method name is counted.

| Method | Class | R/W | Declared | Outside | Inside | Every site outside |
| --- | --- | --- | --- | --- | --- | --- |
| `activate_provisional_if_confirmed` | A/T | W | `xmpp/registry/connection_registry/connections.rs:60` | 0 | 0 | none |
| `active_sm_stream_ids` | I | R | `xmpp/registry/connection_registry/resources.rs:145` | 3 | 0 | `server/session_janitors/implementation.rs:4170`; `server/session_janitors/ingress_custody.rs:77,103` |
| `available_resources_for_user` | A | R | `xmpp/registry/connection_registry/presence.rs:183` | 11 | 0 | `server/extension_host_adapter/queries.rs:159`; `ws/cleanup.rs:1911,2022`; `ws/handlers/iq/last_activity.rs:103`; `ws/handlers/presence/probe.rs:29`; `ws/handlers/presence/regular.rs:458`; `ws/handlers/presence/subscription/delivery.rs:354,442,472,714`; `ws/handlers/pubsub_fanout/caps.rs:177` |
| `blocklist_interested_resources_for_user` | A | R | `xmpp/registry/connection_registry/resources.rs:98` | 1 | 0 | `ws/handlers/iq/blocking.rs:393` |
| `capture_owned_enqueue_if_owner` | T | W | `xmpp/registry/connection_registry/sending.rs:648` | 1 | 0 | `interpret/route_to_connection/implementation.rs:1530` |
| `cleanup_stale` | I | W | `xmpp/registry/connection_registry/presence.rs:197` | 0 | 0 | none |
| `clear_last_activity` | A | W | `xmpp/registry/connection_registry/presence.rs:173` | 1 | 0 | `ws/handlers/presence/regular.rs:79` |
| `clear_presence_state` | A | W | `xmpp/registry/connection_registry/presence.rs:127` | 0 | 0 | none |
| `clear_presence_state_if_owner` | A | W | `xmpp/registry/connection_registry/presence.rs:139` | 2 | 0 | `clustering/route_bridge/registration/side_effects.rs:446`; `ws/handlers/presence/regular.rs:166` |
| `connection_count` | I | R | `xmpp/registry/connection_registry/connections.rs:487` | 1 | 0 | `server/state_inventory.rs:171` |
| `drain_pending_subscription_stanzas` | A | W | `xmpp/registry/connection_registry/subscriptions.rs:53` | 0 | 0 | none |
| `entry` | I | R | `xmpp/registry/connection_registry/connections.rs:482` | 131 | 8 | `server/crates/waddle-client-session/src/timeline.rs:800,872,899,985,1035,1143,1148,1154`; `srv/admin/channels.rs:2694`; `srv/bin/identity_audit/analysis.rs:54,63,78`; `clustering/local_claims.rs:170,804`; `clustering/ordered_relay.rs:1267`; `clustering/resume_bridge.rs:57,166`; `clustering/route_bridge/delivery/channels.rs:25`; `clustering/route_bridge/registration/owner.rs:638`; `clustering/route_bridge/registration/socket.rs:1334`; `clustering/route_bridge/types.rs:501`; `clustering/swarm.rs:698`; `srv/db/migrations/runner.rs:637`; `srv/db/roster.rs:53`; `srv/ingress/execute_relay_copy.rs:33,144`; `srv/ingress/execute_test_hooks.rs:43,53,96,129,143,176,199`; `srv/ingress/frame_receipt_retry.rs:30`; `srv/ingress/maintenance_stalls.rs:145`; `srv/ingress_uow/room_observation/schema.rs:1625,1732`; `srv/pending_delivery/database/ack_windows.rs:44`; `srv/pending_delivery/recovery.rs:172`; `srv/profile/avatar_source.rs:159`; `srv/room_observation/scheduler.rs:55`; `server/extension_bot_avatar.rs:156`; `server/extension_host_adapter/groupchat.rs:68`; `server/extension_host_adapter/remote_room.rs:230`; `server/routes/auth/state.rs:118`; `server/routes/call_thread_end.rs:31`; `interpret/bot.rs:756`; `interpret/effects/delivery_immediate.rs:56`; `interpret/route_to_connection/implementation.rs:1468`; `server/routes/sfu_voice_reconcile.rs:155`; `ws/cleanup.rs:1496,1924,2025,2733,2820,3886`; `ws/connection.rs:3033`; `ws/handlers/iq/muc_owner_moderation.rs:157`; `ws/handlers/message/dm_pin.rs:586`; `ws/handlers/roster_fanout.rs:68,125,165,253`; `ws/remote_muc_retry.rs:24`; `ws/state.rs:922,1097,1343`; `server/session_janitors/implementation.rs:476,5527`; `srv/sm_persistence/implementation.rs:198`; `srv/sm_persistence_fenced/implementation.rs:422`; `srv/sm_promotion/live.rs:29,70`; `srv/sm_promotion/stanza.rs:40,63`; `server/crates/waddle-sfu/src/livekit.rs:1108,1120,1224,1390,1911,1970,2018,2019,2054,2418,2434,2667,2850,3007,3137,3271,3307`; `xmpp/inbox/storage.rs:275,291`; `xmpp/muc/affiliation/list.rs:223`; `xmpp/muc/room.rs:357,470,533,616`; `xmpp/muc/room_actor/implementation.rs:1642,1679,1692,1841,1846`; `xmpp/muc/room_affiliations.rs:146,200`; `xmpp/muc/room_broadcast.rs:127`; `xmpp/muc/room_registry_actor/implementation.rs:589,997,1018,1043,1069,1411,3272,5941`; `xmpp/pending_delivery/storage.rs:1076`; `xmpp/protocol/handlers/jingle/implementation.rs:500`; `xmpp/protocol/handlers/session_initiate_rate_limit.rs:83`; `xmpp/pubsub/storage/memory.rs:145,199,237`; `xmpp/push/store.rs:70`; `xmpp/registry/user_registry.rs:905`; `xmpp/stream_management/persistence.rs:1072,1205,1281,1456`; `xmpp/xep/xep0437.rs:32`; `xmpp/xep/xep0452.rs:228`; `xmpp/xep/xep0470.rs:337`; `xmpp/xep/xep0502.rs:67` |
| `entry_if_owner` | I | R | `xmpp/registry/connection_registry/connections.rs:393` | 31 | 1 | `clustering/route_bridge/delivery/remote.rs:518`; `clustering/route_bridge/delivery/remote_socket.rs:48,517`; `clustering/route_bridge/registration/owner_update.rs:279`; `clustering/route_bridge/registration/socket.rs:334,821,931,958,987,1250`; `srv/pending_delivery/flush.rs:191,804`; `srv/pending_delivery/recovery.rs:32`; `ws/cleanup.rs:913,2055,2090,2125,2199,2217,3843`; `ws/connection.rs:1566,2364`; `ws/handlers/presence/regular.rs:133,282,346,389`; `ws/registration.rs:683,736,763`; `ws/stream_management/implementation.rs:220`; `ws/stream_management/registration.rs:381` |
| `is_blocklist_interested` | A | R | `xmpp/registry/connection_registry/resources.rs:90` | 0 | 0 | none |
| `is_carbons_enabled` | A | R | `xmpp/registry/connection_registry/resources.rs:202` | 1 | 3 | `xmpp/registry/user_actor/implementation.rs:799` |
| `is_connected` | I | R | `xmpp/registry/connection_registry/connections.rs:465` | 0 | 0 | none |
| `is_locally_hosted` | I | R | `xmpp/registry/connection_registry/connections.rs:471` | 21 | 6 | `clustering/resume_bridge.rs:60,172`; `clustering/route_bridge/presence.rs:49,67`; `clustering/route_bridge/registration/socket.rs:1349`; `srv/ingress/execute_relay_copy.rs:34,145`; `srv/ingress/recovery_reachability.rs:67,89`; `srv/pending_delivery/flush.rs:194,807`; `srv/pending_delivery/recovery.rs:34,175`; `interpret/bot.rs:757`; `interpret/route_to_connection/implementation.rs:1469`; `ws/cleanup.rs:1931,2026,2200`; `ws/handlers/roster_fanout.rs:131,254`; `xmpp/registry/user_actor/delivery.rs:108` |
| `is_owned_by` | I | R | `xmpp/registry/connection_registry/connections.rs:322` | 5 | 0 | `clustering/route_bridge/registration/owner.rs:280`; `ws/frame.rs:1200,1823`; `ws/handlers/presence/implementation.rs:264`; `ws/handlers/presence/muc/implementation.rs:81` |
| `is_roster_interested` | A | R | `xmpp/registry/connection_registry/resources.rs:40` | 1 | 0 | `ws/handlers/iq/roster/implementation.rs:321` |
| `last_activity` | A | R | `xmpp/registry/connection_registry/presence.rs:166` | 1 | 0 | `ws/handlers/iq/last_activity.rs:113` |
| `last_activity_count` | I | R | `xmpp/registry/connection_registry.rs:226` | 1 | 0 | `server/state_inventory.rs:174` |
| `list_connections` | I | R | `xmpp/registry/connection_registry/resources.rs:133` | 4 | 0 | `clustering/local_claims.rs:770,966`; `srv/pending_delivery/recovery.rs:171`; `ws/handlers/message/dm_pin.rs:369` |
| `local_binding_permit_if_owner` | T | R | `xmpp/registry/connection_registry/connections.rs:409` | 0 | 1 | none |
| `local_sm_enable_binding_permit_if_owner` | T | R | `xmpp/registry/connection_registry/connections.rs:436` | 1 | 0 | `ws/batch_write.rs:343` |
| `local_sm_stream` | I | R | `xmpp/registry/connection_registry/archive_dispatch.rs:194` | 2 | 0 | `srv/ingress/archive_dispatch.rs:288`; `srv/ingress/execute_detached.rs:121` |
| `lock_bind` | T | W | `xmpp/registry/connection_registry/binding.rs:238` | 3 | 0 | `ws/actor_local_endpoint.rs:219,236`; `ws/registration.rs:55` |
| `mark_blocklist_interested` | A | W | `xmpp/registry/connection_registry/resources.rs:64` | 0 | 0 | none |
| `mark_blocklist_interested_if_owner` | A | W | `xmpp/registry/connection_registry/resources.rs:74` | 1 | 0 | `ws/handlers/iq/blocking.rs:67` |
| `mark_roster_interested` | A | W | `xmpp/registry/connection_registry/resources.rs:13` | 0 | 0 | none |
| `mark_roster_interested_if_owner` | A | W | `xmpp/registry/connection_registry/resources.rs:24` | 1 | 0 | `ws/handlers/iq/roster/implementation.rs:111` |
| `new` | I | W | `xmpp/registry/connection_registry.rs:97` | 1 | 0 | `server/http.rs:990` |
| `occupancy_session_of` | I | R | `xmpp/registry/connection_registry.rs:168` | 2 | 0 | `server/session_janitors/implementation.rs:6317,6420` |
| `other_carbon_resources_for_user` | A | R | `xmpp/registry/connection_registry/resources.rs:182` | 2 | 0 | `interpret/carbons.rs:77,278` |
| `other_resources_for_user` | A | R | `xmpp/registry/connection_registry/resources.rs:158` | 0 | 0 | none |
| `pending_subscription_count` | I | R | `xmpp/registry/connection_registry.rs:214` | 1 | 0 | `server/state_inventory.rs:172` |
| `pending_subscription_stanzas` | A | R | `xmpp/registry/connection_registry/subscriptions.rs:66` | 1 | 0 | `ws/handlers/presence/regular.rs:140` |
| `presence_state` | A | R | `xmpp/registry/connection_registry/presence.rs:122` | 7 | 0 | `clustering/route_bridge/registration/socket.rs:259,1064,1127`; `server/extension_host_adapter/queries.rs:160`; `ws/cleanup.rs:908`; `ws/handlers/presence/probe.rs:125`; `ws/handlers/presence/subscription/delivery.rs:387` |
| `presence_state_count` | I | R | `xmpp/registry/connection_registry.rs:220` | 1 | 0 | `server/state_inventory.rs:173` |
| `probe_socket_lifecycle` | T | R | `xmpp/registry/connection_registry/binding.rs:215` | 1 | 0 | `clustering/route_bridge/registration/socket.rs:1341` |
| `prune_completed_bind` | T | W | `xmpp/registry/connection_registry/binding.rs:267` | 2 | 0 | `ws/cleanup.rs:1764`; `ws/stream_management/registration.rs:488` |
| `publish_provisional_if_expected` | I/T | W | `xmpp/registry/connection_registry/connections.rs:15` | 0 | 0 | none |
| `queue_pending_subscription_stanza` | A | W | `xmpp/registry/connection_registry/subscriptions.rs:7` | 3 | 0 | `ws/handlers/presence/subscription/implementation.rs:262,297,324` |
| `record_last_activity` | A | W | `xmpp/registry/connection_registry/presence.rs:155` | 1 | 0 | `ws/handlers/presence/regular.rs:172` |
| `register` | I | W | `xmpp/registry/connection_registry/connections.rs:118` | 45 | 1 | `srv/admin/channels.rs:373,382,394,406,415,424,436,457,469,482,507`; `srv/admin/push.rs:53`; `srv/admin/spaces.rs:252,261,270,279,288,297`; `srv/admin/users_list.rs:103`; `clustering/claims.rs:1684`; `clustering/relay/implementation.rs:224`; `srv/push_service/commands.rs:264,272`; `server/extension_commands/mod.rs:31,144`; `server/fixed_account.rs:116`; `ws/connection_actor.rs:1395`; `srv/websocket_transport.rs:152,214`; `server/crates/waddle-xmpp-client/src/runtime/implementation.rs:240`; `xmpp/protocol/room/mod.rs:56,57,58,59,60,61,62,63,82,83,84,85,86,87,88` |
| `register_entry` | I | W | `xmpp/registry/connection_registry/connections.rs:171` | 0 | 1 | none |
| `register_entry_if_owner_or_absent` | I | W | `xmpp/registry/connection_registry/connections.rs:255` | 1 | 0 | `clustering/route_bridge/registration/owner.rs:454` |
| `register_socket_entry` | I | W | `xmpp/registry/connection_registry/connections.rs:203` | 1 | 0 | `ws/registration.rs:673` |
| `register_with_carbons` | I | W | `xmpp/registry/connection_registry/connections.rs:131` | 0 | 1 | none |
| `register_with_stream_state` | I | W | `xmpp/registry/connection_registry/connections.rs:142` | 0 | 1 | none |
| `remove_closed_owner_entry` | I | W | `xmpp/registry/connection_registry/sending.rs:739` | 0 | 0 | none |
| `remove_if_sender_closed` | I | W | `xmpp/registry/connection_registry/sending.rs:701` | 0 | 1 | none |
| `remove_if_sender_closed_owner` | I | W | `xmpp/registry/connection_registry/sending.rs:725` | 0 | 11 | none |
| `remove_pending_subscribe` | A | W | `xmpp/registry/connection_registry/subscriptions.rs:33` | 3 | 0 | `ws/handlers/presence/subscription/implementation.rs:125,188,198` |
| `remove_retired_presence` | A | W | `xmpp/registry/connection_registry.rs:127` | 0 | 0 | none |
| `remove_sm_stream_owner` | I | W | `xmpp/registry/connection_registry.rs:115` | 0 | 0 | none |
| `reserve_pending_flush` | T | W | `xmpp/registry/connection_registry/sending.rs:145` | 1 | 1 | `srv/pending_delivery/flush.rs:621` |
| `retire_provisional_if_exact` | I/T | W | `xmpp/registry/connection_registry/connections.rs:84` | 2 | 0 | `ws/actor_local_endpoint.rs:222,239` |
| `roster_interested_resources_for_user` | A | R | `xmpp/registry/connection_registry/resources.rs:48` | 4 | 0 | `ws/handlers/iq/roster/push.rs:104`; `ws/handlers/presence/subscription/delivery.rs:724`; `ws/handlers/roster_fanout.rs:241,311` |
| `send_pending_flush` | T | W | `xmpp/registry/connection_registry/sending.rs:417` | 1 | 0 | `srv/pending_delivery/flush.rs:712` |
| `send_pending_flush_if_owner` | T | W | `xmpp/registry/connection_registry/sending.rs:461` | 1 | 0 | `srv/pending_delivery/flush.rs:701` |
| `send_reserved_pending_flush` | T | W | `xmpp/registry/connection_registry/sending.rs:214` | 1 | 0 | `srv/pending_delivery/flush.rs:852` |
| `send_to` | T | W | `xmpp/registry/connection_registry/sending.rs:324` | 36 | 1 | `srv/admin/channels.rs:2979,3440,5327`; `srv/pending_delivery/flush.rs:729`; `interpret/carbons.rs:420`; `interpret/effects/invite.rs:55`; `interpret/groupchat_archive.rs:1080`; `interpret/offline_delivery.rs:203`; `ws/handlers/iq/blocking.rs:417`; `ws/handlers/iq/full_jid_forward.rs:171`; `ws/handlers/iq/muc_admin.rs:1846,1870`; `ws/handlers/presence/probe.rs:80,116,164,200`; `ws/handlers/presence/regular.rs:466,547`; `ws/handlers/presence/subscription/delivery.rs:66,99,250,452,481,633,694`; `ws/handlers/presence/subscription/directed.rs:36,62`; `ws/handlers/presence/subscription/implementation.rs:304`; `srv/sm_promotion/implementation.rs:1311`; `srv/sm_promotion/pending.rs:204`; `srv/sm_promotion/stanza.rs:43,66`; `xmpp/routing.rs:275,297,373,390` |
| `send_to_if_owner` | T | W | `xmpp/registry/connection_registry/sending.rs:385` | 2 | 0 | `srv/pending_delivery/flush.rs:728`; `ws/handlers/presence/regular.rs:344` |
| `send_to_many` | T | W | `xmpp/registry/connection_registry/resources.rs:112` | 0 | 0 | none |
| `send_to_with_write_acceptance` | T | W | `xmpp/registry/connection_registry/sending.rs:248` | 1 | 0 | `srv/room_effect_outbox/drain.rs:610` |
| `server_uptime_seconds` | T | R | `xmpp/registry/connection_registry/presence.rs:178` | 2 | 0 | `ws/handlers/iq/last_activity.rs:32,45` |
| `set_carbons_enabled` | A | W | `xmpp/registry/connection_registry/resources.rs:213` | 0 | 0 | none |
| `set_carbons_enabled_if_owner` | A | W | `xmpp/registry/connection_registry/resources.rs:226` | 0 | 0 | none |
| `set_occupancy_session_if_owner` | I | W | `xmpp/registry/connection_registry.rs:149` | 1 | 0 | `ws/registration.rs:693` |
| `set_sm_stream_id` | I | W | `xmpp/registry/connection_registry.rs:136` | 0 | 2 | none |
| `set_sm_stream_id_if_owner` | I | W | `xmpp/registry/connection_registry.rs:180` | 2 | 0 | `ws/registration.rs:376`; `ws/stream_management/implementation.rs:120` |
| `sm_stream_owner` | I | R | `xmpp/registry/connection_registry.rs:205` | 3 | 0 | `clustering/local_claims.rs:169`; `clustering/resume_bridge.rs:54,163` |
| `try_lock_bind` | T | W | `xmpp/registry/connection_registry/binding.rs:201` | 1 | 0 | `clustering/route_bridge/registration/owner.rs:227` |
| `try_reserve_pending_flush` | T | W | `xmpp/registry/connection_registry/sending.rs:179` | 1 | 1 | `srv/pending_delivery/flush.rs:619` |
| `try_send_outbound_if_owner` | T | W | `xmpp/registry/connection_registry/sending.rs:602` | 5 | 1 | `clustering/route_bridge/delivery/remote_socket.rs:572,589,639`; `interpret/effects/delivery_immediate.rs:58`; `ws/handlers/roster_fanout.rs:114` |
| `try_send_outbound_to_locally_hosted` | T | W | `xmpp/registry/connection_registry/sending.rs:518` | 1 | 0 | `interpret/route_to_connection/implementation.rs:1432` |
| `try_send_to` | T | W | `xmpp/registry/connection_registry/sending.rs:512` | 16 | 0 | `srv/admin/channels.rs:2820,2833,2845`; `server/routes/muc_muji_clear.rs:508`; `ws/handlers/iq/muc_owner_config.rs:428,452,856,979,1070`; `ws/handlers/iq/muc_owner_moderation.rs:543,2010`; `ws/handlers/iq/roster/push.rs:115,157`; `ws/handlers/presence/muc/implementation.rs:453`; `ws/handlers/pubsub_fanout/caps.rs:219`; `ws/handlers/pubsub_fanout/implementation.rs:267` |
| `try_send_to_matching` | T | W | `xmpp/registry/connection_registry/sending.rs:534` | 0 | 1 | none |
| `unregister` | I | W | `xmpp/registry/connection_registry/connections.rs:300` | 0 | 1 | none |
| `unregister_if_owner` | I | W | `xmpp/registry/connection_registry/connections.rs:331` | 18 | 0 | `clustering/local_claims.rs:904`; `clustering/route_bridge/registration/owner.rs:483,494,608`; `clustering/route_bridge/registration/owner_retire.rs:102,275,307,337`; `clustering/route_bridge/registration/owner_sweep.rs:273`; `ws/cleanup.rs:1132,1353,1521,1848,2624`; `ws/registration.rs:251,806`; `ws/stream_management/registration.rs:385,534` |
| `unregister_if_sm_stream_id` | I | W | `xmpp/registry/connection_registry/connections.rs:367` | 2 | 0 | `ws/cleanup.rs:3861`; `server/session_janitors/implementation.rs:1624` |
| `update_presence` | A | W | `xmpp/registry/connection_registry/presence.rs:11` | 0 | 0 | none |
| `update_presence_if_owner` | A | W | `xmpp/registry/connection_registry/presence.rs:33` | 3 | 0 | `clustering/route_bridge/registration/side_effects.rs:427`; `ws/handlers/presence/regular.rs:70`; `ws/registration.rs:409` |
| `update_presence_state` | A | W | `xmpp/registry/connection_registry/presence.rs:102` | 0 | 0 | none |
| `update_presence_state_if_owner` | A | W | `xmpp/registry/connection_registry/presence.rs:66` | 3 | 0 | `clustering/route_bridge/registration/side_effects.rs:437`; `ws/handlers/presence/regular.rs:97`; `ws/registration.rs:410` |

## 2. `ConnectionEntry` (`xmpp/registry/connection_registry/outbound.rs`)

### 2.1 Fields

| Field | Class | Declared | Sites | Write-like | Files | Every site |
| --- | --- | --- | --- | --- | --- | --- |
| `binding_id` | A | `xmpp/registry/connection_registry/outbound.rs:320` | 5 | 0 | 1 | `xmpp/registry/connection_registry/outbound.rs:640,645,670,688,917` |
| `connection_ref` | A | `xmpp/registry/connection_registry/outbound.rs:321` | 65 | 1 | 18 | `clustering/route_bridge/delivery/remote_socket.rs:44,49`; `clustering/route_bridge/registration/owner.rs:260,308,375,400,445,793`; `clustering/route_bridge/registration/owner_sweep.rs:122`; `clustering/route_bridge/registration/owner_update.rs:241,299`; `clustering/route_bridge/registration/socket.rs:814,822`; `ws/actor_local_endpoint.rs:916`; `ws/batch_write.rs:350`; `ws/connection_actor.rs:1676,2010,2630,2975,3032`; `ws/connection_actor/prebind.rs:496,707`; `ws/connection_actor_core.rs:100`; `ws/connection_actor_driver_task.rs:592,712,1169,1173,1174,1240,1288,1462,1478,1483`; `ws/registration.rs:502,510`; `ws/supervised_io.rs:959,964,1498,1503,1529,1702,1707,1837,1872,1877,1883,2302,2959,3234`; `ws/supervised_io/relinquishment.rs:52,57`; `srv/websocket_transport.rs:183,522,740,1074,1700,1868,1873,2645`; `xmpp/ownership/account_resource_transition.rs:462`; `xmpp/ownership/connection_identity.rs:132,142,148`; `xmpp/registry/connection_registry/outbound.rs:646,699` |
| `binding_ref` | A | `xmpp/registry/connection_registry/outbound.rs:324` | 49 | 5 | 11 | `ws/batch_write.rs:337`; `ws/connection.rs:1120,1140,1160,1534,1615,1625`; `ws/connection_actor.rs:2193,2356,2633,2897,2979,2985,2993,3036,3069,3078,3106`; `ws/registration.rs:463,645`; `ws/stream_management/registration.rs:374`; `xmpp/ownership/account_resource_transition.rs:458`; `xmpp/ownership/endpoint_transition.rs:175,327,430,439,467,488,499,706,875`; `xmpp/registry/connection_registry/endpoint.rs:230,470,630`; `xmpp/registry/connection_registry/live_resume.rs:128,186,225,245`; `xmpp/registry/connection_registry/outbound.rs:555,639,647,670,688,706,759,830,856`; `xmpp/registry/connection_registry/sending.rs:116,120` |
| `binding_permit` | A/T | `xmpp/registry/connection_registry/outbound.rs:325` | 30 | 0 | 3 | `xmpp/registry/connection_registry/connections.rs:33,50,97,104,177,195,215,248,273,291,302,309,340,347,376,383,422,429,454`; `xmpp/registry/connection_registry/outbound.rs:609,613,648,772,786,864,877`; `xmpp/registry/connection_registry/sending.rs:706,713,750,757` |
| `local_io_relinquishment` | T | `xmpp/registry/connection_registry/outbound.rs:327` | 4 | 2 | 3 | `ws/connection.rs:723`; `ws/registration.rs:646`; `xmpp/registry/connection_registry/outbound.rs:459,712` |
| `archive_dispatch` | A | `xmpp/registry/connection_registry/outbound.rs:328` | 1 | 1 | 1 | `xmpp/registry/connection_registry/archive_dispatch.rs:142` |
| `hosting` | I/O | `xmpp/registry/connection_registry/outbound.rs:330` | 2 | 1 | 1 | `xmpp/registry/connection_registry/outbound.rs:586,728` |
| `resume_control` | T | `xmpp/registry/connection_registry/outbound.rs:331` | 5 | 2 | 2 | `ws/send.rs:29,34`; `xmpp/registry/connection_registry/outbound.rs:560,564,768` |
| `terminal_ordering_retry` | T | `xmpp/registry/connection_registry/outbound.rs:333` | 2 | 0 | 1 | `xmpp/registry/connection_registry/outbound.rs:736,739` |
| `endpoint` | T | `xmpp/registry/connection_registry/outbound.rs:336` | 47 | 1 | 16 | `srv/permissions/spicedb.rs:24`; `srv/push_service/dispatch.rs:351,361`; `server/crates/waddle-xmpp-client-wasm/src/client_core.rs:180`; `server/crates/waddle-xmpp-client-wasm/src/types.rs:1128`; `server/crates/waddle-xmpp-client/src/config.rs:113,122`; `server/crates/waddle-xmpp-client/src/runtime/implementation.rs:610`; `server/crates/waddle-xmpp-client/src/transport/native.rs:321,323`; `xmpp/ownership/account_resource_transition.rs:1558`; `xmpp/push/limiter.rs:193,195`; `xmpp/push/sender.rs:116`; `xmpp/registry/connection_registry/archive_dispatch.rs:102,117,138`; `xmpp/registry/connection_registry/connections.rs:417`; `xmpp/registry/connection_registry/endpoint.rs:446`; `xmpp/registry/connection_registry/outbound.rs:599,610,632,640,649,653,658,718,724`; `xmpp/registry/connection_registry/presence.rs:202`; `xmpp/registry/connection_registry/sending.rs:117,119,125,155,189,220,256,282,327,346,392,425,470,541,635,681,703,747` |
| `carbons_enabled` | A/I | `xmpp/registry/connection_registry/outbound.rs:338` | 36 | 8 | 24 | `clustering/route_bridge/registration/owner_update.rs:176,283,284`; `clustering/route_bridge/registration/side_effects.rs:458,459`; `clustering/route_bridge/registration/socket.rs:1168`; `clustering/route_bridge/types.rs:156`; `interpret/carbons.rs:69`; `interpret/recipient_selection.rs:78`; `ws/cleanup.rs:929,2287`; `ws/registration.rs:660,661,886`; `ws/stream_management/implementation.rs:1468`; `ws/stream_management/registration.rs:511`; `srv/sm_persistence/atomic_store.rs:314`; `srv/sm_persistence/implementation.rs:366`; `srv/sm_persistence/revisioned/digest.rs:151`; `srv/sm_persistence_fenced/atomic_store.rs:112`; `srv/sm_persistence_fenced/implementation.rs:654,1660`; `srv/sm_promotion/implementation.rs:281`; `xmpp/ownership/account_resource_transition.rs:403`; `xmpp/registry/connection_registry/connections.rs:152`; `xmpp/registry/connection_registry/outbound.rs:922`; `xmpp/registry/connection_registry/resources.rs:217,238`; `xmpp/registry/user_actor/implementation.rs:554,779`; `xmpp/stream_management/session_registry/persistence_codec.rs:41,142`; `xmpp/stream_management/session_registry/resources.rs:609,878,895`; `xmpp/stream_management/state.rs:631` |
| `presence_available` | A/I | `xmpp/registry/connection_registry/outbound.rs:340` | 34 | 5 | 22 | `clustering/route_bridge/registration/owner.rs:507`; `clustering/route_bridge/registration/side_effects.rs:467,468`; `clustering/route_bridge/registration/socket.rs:1163`; `clustering/route_bridge/types.rs:159`; `ws/cleanup.rs:2290,3909`; `ws/registration.rs:407`; `ws/stream_management/implementation.rs:1471`; `ws/stream_management/registration.rs:514`; `server/session_janitors/implementation.rs:1596`; `srv/sm_persistence/atomic_store.rs:317`; `srv/sm_persistence/implementation.rs:369`; `srv/sm_persistence/revisioned/digest.rs:154`; `srv/sm_persistence_fenced/atomic_store.rs:115`; `srv/sm_persistence_fenced/implementation.rs:657,1663`; `srv/sm_promotion/implementation.rs:284`; `xmpp/ownership/account_resource_transition.rs:395`; `xmpp/registry/connection_registry/outbound.rs:927`; `xmpp/registry/connection_registry/presence.rs:15,48`; `xmpp/registry/user_actor/implementation.rs:636`; `xmpp/stream_management/session_registry/persistence_codec.rs:44,145`; `xmpp/stream_management/session_registry/resources.rs:919,934,950,957,986,998,1018,1034`; `xmpp/stream_management/state.rs:634` |
| `presence_priority` | A/I | `xmpp/registry/connection_registry/outbound.rs:342` | 26 | 5 | 21 | `clustering/route_bridge/registration/owner.rs:508`; `clustering/route_bridge/registration/side_effects.rs:470,471`; `clustering/route_bridge/registration/socket.rs:1164`; `clustering/route_bridge/types.rs:160`; `ws/cleanup.rs:2293`; `ws/registration.rs:409,418`; `ws/stream_management/implementation.rs:1474`; `ws/stream_management/registration.rs:517`; `srv/sm_persistence/atomic_store.rs:320`; `srv/sm_persistence/implementation.rs:372`; `srv/sm_persistence/revisioned/digest.rs:157`; `srv/sm_persistence_fenced/atomic_store.rs:118`; `srv/sm_persistence_fenced/implementation.rs:660,1666`; `srv/sm_promotion/implementation.rs:287`; `xmpp/ownership/account_resource_transition.rs:398`; `xmpp/registry/connection_registry/outbound.rs:932`; `xmpp/registry/connection_registry/presence.rs:19,52`; `xmpp/registry/user_actor/implementation.rs:639`; `xmpp/stream_management/session_registry/persistence_codec.rs:47,148`; `xmpp/stream_management/session_registry/resources.rs:52`; `xmpp/stream_management/state.rs:637` |
| `roster_interested` | A/I | `xmpp/registry/connection_registry/outbound.rs:344` | 34 | 7 | 22 | `clustering/route_bridge/registration/owner_update.rs:196`; `clustering/route_bridge/registration/side_effects.rs:461,462`; `clustering/route_bridge/registration/socket.rs:1171`; `clustering/route_bridge/types.rs:157`; `ws/cleanup.rs:971,2288`; `ws/handlers/iq/dispatch.rs:182`; `ws/handlers/roster_fanout.rs:106,126`; `ws/registration.rs:663,664`; `ws/stream_management/implementation.rs:1469`; `ws/stream_management/registration.rs:512`; `srv/sm_persistence/atomic_store.rs:315`; `srv/sm_persistence/implementation.rs:367`; `srv/sm_persistence/revisioned/digest.rs:152`; `srv/sm_persistence_fenced/atomic_store.rs:113`; `srv/sm_persistence_fenced/implementation.rs:655,1661`; `srv/sm_promotion/implementation.rs:282`; `xmpp/ownership/account_resource_transition.rs:404`; `xmpp/registry/connection_registry/connections.rs:155`; `xmpp/registry/connection_registry/resources.rs:17,35,43,53`; `xmpp/stream_management/session_registry/persistence_codec.rs:42,143`; `xmpp/stream_management/session_registry/resources.rs:221,236,289,296`; `xmpp/stream_management/state.rs:632` |
| `blocklist_interested` | A/I | `xmpp/registry/connection_registry/outbound.rs:346` | 32 | 8 | 21 | `clustering/route_bridge/registration/owner_update.rs:217`; `clustering/route_bridge/registration/side_effects.rs:464,465`; `clustering/route_bridge/registration/socket.rs:1174`; `clustering/route_bridge/types.rs:158`; `ws/cleanup.rs:972,2289`; `ws/handlers/iq/blocking.rs:76`; `ws/registration.rs:665,666`; `ws/stream_management/implementation.rs:1470`; `ws/stream_management/registration.rs:513`; `srv/sm_persistence/atomic_store.rs:316`; `srv/sm_persistence/implementation.rs:368`; `srv/sm_persistence/revisioned/digest.rs:153`; `srv/sm_persistence_fenced/atomic_store.rs:114`; `srv/sm_persistence_fenced/implementation.rs:656,1662`; `srv/sm_promotion/implementation.rs:283`; `xmpp/ownership/account_resource_transition.rs:405`; `xmpp/registry/connection_registry/connections.rs:158`; `xmpp/registry/connection_registry/resources.rs:68,85,93,103`; `xmpp/stream_management/session_registry/persistence_codec.rs:43,144`; `xmpp/stream_management/session_registry/resources.rs:258,273,311,318`; `xmpp/stream_management/state.rs:633` |
| `offline_flushed` | A | `xmpp/registry/connection_registry/outbound.rs:351` | 2 | 1 | 1 | `xmpp/registry/connection_registry/outbound.rs:940,966` |
| `pending_subscribes_flushed` | A | `xmpp/registry/connection_registry/outbound.rs:359` | 10 | 3 | 9 | `ws/cleanup.rs:993,2295`; `ws/registration.rs:719`; `ws/stream_management/implementation.rs:1476`; `ws/stream_management/registration.rs:519`; `srv/sm_promotion/implementation.rs:289`; `xmpp/registry/connection_registry/outbound.rs:951`; `xmpp/stream_management/session_registry/inventory.rs:185`; `xmpp/stream_management/session_registry/reconciliation.rs:160`; `xmpp/stream_management/state.rs:639` |
| `sm_stream_id` | I | `xmpp/registry/connection_registry/outbound.rs:371` | 3 | 2 | 1 | `xmpp/registry/connection_registry/outbound.rs:554,979,990` |
| `occupancy_session` | I | `xmpp/registry/connection_registry/outbound.rs:375` | 83 | 4 | 30 | `clustering/route_bridge/registration/owner.rs:218`; `clustering/route_bridge/registration/owner_retire.rs:161,197,231`; `clustering/route_bridge/registration/side_effects.rs:454,455`; `clustering/route_bridge/registration/socket.rs:1261,1262,1331`; `clustering/route_bridge/registration/socket_forwarder.rs:186`; `ws/cleanup.rs:969,1458,1505,1542,1706,2286,2743,3917`; `ws/handlers/presence/implementation.rs:160,191,220,287`; `ws/handlers/presence/muc/implementation.rs:59,145`; `ws/registration.rs:555,595,685,693,861`; `ws/stream_management/implementation.rs:1467`; `ws/stream_management/registration.rs:490`; `server/session_janitors/implementation.rs:1674,1688`; `srv/sm_persistence/atomic_store.rs:307`; `srv/sm_persistence/implementation.rs:359`; `srv/sm_persistence/incremental.rs:44`; `srv/sm_persistence/revisioned/digest.rs:141`; `srv/sm_persistence/revisioned/implementation.rs:426,472,535,543,554,569`; `srv/sm_persistence_fenced/atomic_store.rs:106`; `srv/sm_persistence_fenced/implementation.rs:648,1654`; `srv/sm_persistence_fenced/incremental.rs:46`; `srv/sm_promotion/implementation.rs:273,694,710`; `xmpp/registry/connection_registry.rs:161`; `xmpp/registry/connection_registry/outbound.rs:975`; `xmpp/stream_management/session_registry/acknowledgements.rs:49`; `xmpp/stream_management/session_registry/claims.rs:283,601,1279,1806,1813,1959,1973,1982,2054,2230,2248,2587,3243,3334,3396,3402,3426,3447,3463`; `xmpp/stream_management/session_registry/inventory.rs:52`; `xmpp/stream_management/session_registry/persistence_codec.rs:20,134`; `xmpp/stream_management/session_registry/resources.rs:88,111,137,162,781,808`; `xmpp/stream_management/session_registry/trait_impl.rs:255`; `xmpp/stream_management/state.rs:623` |
| `force_detach_tx` | T | `xmpp/registry/connection_registry/outbound.rs:406` | 5 | 1 | 1 | `xmpp/registry/connection_registry/outbound.rs:556,747,773,795,865` |
| `force_detach_rx` | T | `xmpp/registry/connection_registry/outbound.rs:407` | 3 | 3 | 2 | `ws/connection.rs:1000,1467`; `xmpp/registry/connection_registry/outbound.rs:911` |
| `write_interruption` | T | `xmpp/registry/connection_registry/outbound.rs:411` | 11 | 1 | 5 | `ws/batch_write.rs:635,666,686,708,855,1053,1069`; `ws/connection.rs:716`; `ws/registration.rs:654`; `xmpp/registry/connection_registry/connections.rs:210`; `xmpp/registry/connection_registry/outbound.rs:833` |

### 2.2 Methods

"Outside" counts call sites outside `xmpp/registry/connection_registry*`;
"inside" counts those within it. A receiver of another type with the same
method name is counted.

| Method | Class | R/W | Declared | Outside | Inside | Every site outside |
| --- | --- | --- | --- | --- | --- | --- |
| `activate_binding` | T | W | `xmpp/registry/connection_registry/outbound.rs:594` | 0 | 2 | none |
| `actor_custody` | T | R | `xmpp/registry/connection_registry/outbound.rs:652` | 0 | 3 | none |
| `binding_id` | A | R | `xmpp/registry/connection_registry/outbound.rs:916` | 26 | 35 | `clustering/local_claims.rs:833`; `clustering/route_bridge/registration/owner.rs:280,378`; `clustering/route_bridge/registration/owner_retire.rs:93`; `clustering/route_bridge/registration/owner_update.rs:276`; `srv/pending_delivery/recovery.rs:183`; `interpret/effects/delivery_immediate.rs:60`; `interpret/route_to_connection/implementation.rs:1532`; `ws/actor_local_endpoint.rs:228,288,327,345`; `ws/cleanup.rs:1939,3869,3872`; `ws/handlers/roster_fanout.rs:114`; `server/session_janitors/implementation.rs:1635`; `xmpp/registry/user_actor/delivery.rs:113`; `xmpp/registry/user_actor/implementation.rs:151,209,251,263,274,287,305,370` |
| `binding_ref` | A | R | `xmpp/registry/connection_registry/outbound.rs:705` | 19 | 10 | `clustering/local_claims.rs:195,837`; `clustering/resume_bridge.rs:66`; `clustering/route_bridge/registration/owner_retire.rs:58`; `clustering/route_bridge/registration/socket.rs:1272`; `ws/registration.rs:92`; `ws/stream_management/implementation.rs:232`; `ws/stream_management/registration.rs:383`; `xmpp/ownership/account_resource_transition.rs:769,1049,1819`; `xmpp/registry/user_actor/delivery.rs:114`; `xmpp/registry/user_actor/implementation.rs:211,253,277,307,371,375`; `xmpp/registry/user_registry.rs:787` |
| `claim_offline_flush` | A | W | `xmpp/registry/connection_registry/outbound.rs:939` | 1 | 0 | `ws/handlers/presence/regular.rs:283` |
| `claim_pending_subscribes_flush` | A | W | `xmpp/registry/connection_registry/outbound.rs:950` | 2 | 0 | `ws/handlers/presence/regular.rs:134`; `ws/registration.rs:738` |
| `commit_pending_flush` | T | W | `xmpp/registry/connection_registry/sending.rs:107` | 0 | 1 | none |
| `connection_ref` | A | R | `xmpp/registry/connection_registry/outbound.rs:698` | 49 | 9 | `clustering/route_bridge/delivery/remote_socket.rs:49`; `clustering/route_bridge/registration/socket.rs:242,328,335,345,822`; `clustering/route_bridge/types.rs:154`; `ws/actor_local_endpoint.rs:384,900,916,939,946,1123,1157,1158,1166`; `ws/connection.rs:532,655,695,713,1177`; `ws/connection/live_actor.rs:216`; `ws/connection_actor.rs:489,623,738,2355,2630,2908,2909,2975,3032,3070,3105`; `ws/connection_actor/prebind.rs:496,643,707`; `ws/registration.rs:502`; `ws/supervised_io.rs:1245`; `srv/websocket_transport.rs:1919,1945,2065,2325`; `xmpp/ownership/account_resource_transition.rs:454,773,1742`; `xmpp/ownership/endpoint_transition.rs:488`; `xmpp/stream_management/lifecycle_model.rs:378,565,591` |
| `force_detach_sender` | T | R | `xmpp/registry/connection_registry/outbound.rs:746` | 2 | 0 | `clustering/local_claims.rs:199`; `ws/stream_management/implementation.rs:236` |
| `into_socket_outbound_endpoint_if_empty` | T | W | `xmpp/registry/connection_registry/outbound.rs:665` | 1 | 0 | `ws/stream_management/registration.rs:395` |
| `into_socket_outbound_endpoint_if_receiver_empty` | T | W | `xmpp/registry/connection_registry/outbound.rs:682` | 1 | 0 | `ws/stream_management/registration.rs:391` |
| `is_carbons_enabled` | A | R | `xmpp/registry/connection_registry/outbound.rs:921` | 1 | 3 | `xmpp/registry/user_actor/implementation.rs:799` |
| `is_locally_hosted` | I | R | `xmpp/registry/connection_registry/outbound.rs:727` | 21 | 6 | `clustering/resume_bridge.rs:60,172`; `clustering/route_bridge/presence.rs:49,67`; `clustering/route_bridge/registration/socket.rs:1349`; `srv/ingress/execute_relay_copy.rs:34,145`; `srv/ingress/recovery_reachability.rs:67,89`; `srv/pending_delivery/flush.rs:194,807`; `srv/pending_delivery/recovery.rs:34,175`; `interpret/bot.rs:757`; `interpret/route_to_connection/implementation.rs:1469`; `ws/cleanup.rs:1931,2026,2200`; `ws/handlers/roster_fanout.rs:131,254`; `xmpp/registry/user_actor/delivery.rs:108` |
| `is_presence_available` | A | R | `xmpp/registry/connection_registry/outbound.rs:926` | 16 | 2 | `srv/pending_delivery/flush.rs:197,808`; `srv/pending_delivery/recovery.rs:35,176`; `ws/cleanup.rs:930,1530,1849,1932,2027,2201,2625,2821`; `srv/sm_promotion/live.rs:30`; `xmpp/registry/user_actor/delivery.rs:207`; `xmpp/registry/user_actor/implementation.rs:552,592` |
| `is_provisional` | T | R | `xmpp/registry/connection_registry/outbound.rs:657` | 1 | 4 | `xmpp/registry/user_actor/implementation.rs:374` |
| `local_io_relinquishment` | T | R | `xmpp/registry/connection_registry/outbound.rs:711` | 1 | 0 | `ws/connection.rs:725` |
| `new` | T | W | `xmpp/registry/connection_registry/outbound.rs:420` | 0 | 1 | none |
| `new_actor_provisional_for_binding` | T | W | `xmpp/registry/connection_registry/outbound.rs:484` | 0 | 0 | none |
| `new_entry` | T | W | `xmpp/registry/connection_registry/outbound.rs:507` | 0 | 0 | none |
| `new_for_binding` | T | W | `xmpp/registry/connection_registry/outbound.rs:438` | 1 | 0 | `ws/registration.rs:652` |
| `new_for_binding_with_local_io` | T | W | `xmpp/registry/connection_registry/outbound.rs:453` | 1 | 0 | `ws/registration.rs:647` |
| `new_for_connection` | T | W | `xmpp/registry/connection_registry/outbound.rs:429` | 0 | 0 | none |
| `new_provisional_for_binding` | T | W | `xmpp/registry/connection_registry/outbound.rs:468` | 0 | 0 | none |
| `occupancy_session` | I | R | `xmpp/registry/connection_registry/outbound.rs:974` | 3 | 1 | `clustering/route_bridge/registration/socket.rs:1262,1351`; `clustering/route_bridge/types.rs:155` |
| `presence_priority` | A | R | `xmpp/registry/connection_registry/outbound.rs:931` | 13 | 2 | `srv/pending_delivery/flush.rs:197,809`; `srv/pending_delivery/recovery.rs:36,177,185`; `ws/cleanup.rs:984,1933,2028,2202`; `srv/sm_promotion/live.rs:33`; `xmpp/registry/user_actor/delivery.rs:208`; `xmpp/registry/user_actor/implementation.rs:553,593` |
| `remote_hosted` | O | W | `xmpp/registry/connection_registry/outbound.rs:567` | 0 | 0 | none |
| `remote_hosted_for_connection` | O | W | `xmpp/registry/connection_registry/outbound.rs:576` | 1 | 0 | `clustering/route_bridge/registration/owner.rs:373` |
| `reset_offline_flush` | A | W | `xmpp/registry/connection_registry/outbound.rs:965` | 4 | 0 | `srv/pending_delivery/flush.rs:198`; `ws/cleanup.rs:2127,2205`; `ws/handlers/presence/regular.rs:390` |
| `resume_control` | T | R | `xmpp/registry/connection_registry/outbound.rs:563` | 1 | 1 | `clustering/resume_bridge.rs:69` |
| `same_exact_local_binding` | I | R | `xmpp/registry/connection_registry/outbound.rs:644` | 0 | 5 | none |
| `seal_actor_binding_if_any` | T | W | `xmpp/registry/connection_registry/outbound.rs:638` | 4 | 10 | `xmpp/registry/user_actor/implementation.rs:119,213,297,394` |
| `seal_binding` | T | W | `xmpp/registry/connection_registry/outbound.rs:607` | 1 | 2 | `ws/actor_local_endpoint.rs:269` |
| `seal_binding_for_failed_resume` | T | W | `xmpp/registry/connection_registry/outbound.rs:621` | 1 | 0 | `ws/stream_management/registration.rs:384` |
| `set_sm_stream_id` | I | W | `xmpp/registry/connection_registry/outbound.rs:978` | 0 | 2 | none |
| `sm_stream_id` | I | R | `xmpp/registry/connection_registry/outbound.rs:989` | 9 | 8 | `clustering/local_claims.rs:171`; `clustering/resume_bridge.rs:60,172`; `clustering/route_bridge/delivery/remote_socket.rs:523`; `srv/pending_delivery/recovery.rs:184`; `interpret/route_to_connection/implementation.rs:1489`; `ws/cleanup.rs:1940,3889`; `ws/handlers/presence/regular.rs:287` |
| `take_force_detach_rx` | T | W | `xmpp/registry/connection_registry/outbound.rs:910` | 2 | 0 | `clustering/route_bridge/registration/owner.rs:379`; `ws/connection.rs:2366` |
| `try_acquire_terminal_ordering_retry` | T | W | `xmpp/registry/connection_registry/outbound.rs:735` | 2 | 0 | `ws/cleanup.rs:2062,2221` |
| `try_force_detach_and_interrupt` | T | W | `xmpp/registry/connection_registry/outbound.rs:815` | 4 | 0 | `clustering/resume_bridge.rs:191`; `clustering/route_bridge/registration/owner_retire.rs:67`; `clustering/route_bridge/registration/socket.rs:1278`; `ws/registration.rs:101` |
| `try_force_detach_and_interrupt_after_offer` | T | W | `xmpp/registry/connection_registry/outbound.rs:822` | 0 | 2 | none |
| `try_force_detach_and_revoke` | T | W | `xmpp/registry/connection_registry/outbound.rs:844` | 2 | 1 | `clustering/route_bridge/registration/owner_retire.rs:63`; `ws/registration.rs:97` |
| `try_force_detach_and_revoke_after_reserve` | T | W | `xmpp/registry/connection_registry/outbound.rs:851` | 0 | 2 | none |
| `try_force_detach_for_jid` | T | W | `xmpp/registry/connection_registry/outbound.rs:754` | 3 | 0 | `clustering/local_claims.rs:842`; `clustering/route_bridge/registration/socket.rs:1280`; `xmpp/registry/user_registry.rs:791` |
| `try_force_detach_raw` | T | W | `xmpp/registry/connection_registry/outbound.rs:791` | 0 | 4 | none |
| `try_send_archive_ordered` | T | W | `xmpp/registry/connection_registry/archive_dispatch.rs:78` | 1 | 2 | `xmpp/registry/user_actor/delivery.rs:59` |
| `try_send_archive_ordered_with_refusal` | T | W | `xmpp/registry/connection_registry/archive_dispatch.rs:90` | 1 | 1 | `xmpp/registry/user_actor/delivery.rs:120` |
| `wire_resume_control` | T | W | `xmpp/registry/connection_registry/outbound.rs:547` | 1 | 0 | `ws/registration.rs:655` |

## 3. `WsConnState` (`ws/state.rs`)

### 3.1 Fields

| Field | Class | Declared | Sites | Write-like | Files | Every site |
| --- | --- | --- | --- | --- | --- | --- |
| `ingress_generation` | A | `ws/state.rs:1757` | 1 | 0 | 1 | `ws/frame.rs:1746` |
| `connection_ref` | A | `ws/state.rs:1760` | 38 | 1 | 9 | `ws/actor_local_endpoint.rs:916`; `ws/batch_write.rs:350`; `ws/connection_actor.rs:1676,2010,2630,2975,3032`; `ws/connection_actor/prebind.rs:496,707`; `ws/connection_actor_core.rs:100`; `ws/connection_actor_driver_task.rs:592,712,1169,1173,1174,1240,1288,1462,1478,1483`; `ws/registration.rs:502,510`; `ws/supervised_io.rs:959,964,1498,1503,1529,1702,1707,1837,1872,1877,1883,2302,2959,3234`; `ws/supervised_io/relinquishment.rs:52,57` |
| `local_io_relinquishment` | T | `ws/state.rs:1763` | 2 | 1 | 2 | `ws/connection.rs:723`; `ws/registration.rs:646` |
| `binding_ref` | A | `ws/state.rs:1765` | 21 | 4 | 5 | `ws/batch_write.rs:337`; `ws/connection.rs:1120,1140,1160,1534,1615,1625`; `ws/connection_actor.rs:2193,2356,2633,2897,2979,2985,2993,3036,3069,3078,3106`; `ws/registration.rs:463,645`; `ws/stream_management/registration.rs:374` |
| `phase` | A | `ws/state.rs:1768` | 98 | 35 | 13 | `ws/actor_local_endpoint.rs:391,430,431,466,472,484,1098`; `ws/batch_write.rs:334,727,750,1106,1137`; `ws/cleanup.rs:814,815,839,853,890,1741,2786`; `ws/connection.rs:647,658,665,740,765,795,804,1081,1090,1099,1119,1405,1425,1529,1561,1611,1624,1725,2358,2527,2535,2906`; `ws/connection_actor.rs:1445,1467`; `ws/connection_actor/output_continuation.rs:184,200,208,249,288,293,302,305,385,388`; `ws/connection_actor/prebind.rs:262,271,481,491,524,530,538,541,565,567,572,581,583,592,600,607,615,623,631,652,677,682,692,696,705,735,747,799,800`; `ws/frame.rs:400,402,1702,1796,1818,1825`; `ws/handlers/iq/link_preview_resolver.rs:236`; `ws/handlers/presence/implementation.rs:149,295`; `ws/outbound.rs:70,220`; `ws/registration.rs:470,857,885`; `ws/stream_management/registration.rs:491,550` |
| `stream_open_handled` | T | `ws/state.rs:1772` | 0 | 0 | 0 | none |
| `stream_open_from` | T | `ws/state.rs:1774` | 0 | 0 | 0 | none |
| `stream_open_wire_state` | T | `ws/state.rs:1779` | 0 | 0 | 0 | none |
| `authenticated_session` | A | `ws/state.rs:1782` | 15 | 2 | 8 | `ws/cleanup.rs:950,962,1037,1238,2281`; `ws/connection.rs:1356`; `ws/connection_actor/prebind.rs:268,734`; `ws/frame.rs:401,1613`; `ws/handlers/presence/implementation.rs:286`; `ws/handlers/presence/muc/implementation.rs:60,147`; `ws/outbound.rs:221`; `ws/replay.rs:254` |
| `occupancy_session` | A | `ws/state.rs:1783` | 21 | 1 | 6 | `ws/cleanup.rs:969,1458,1505,1542,1706,2286,2743,3917`; `ws/handlers/presence/implementation.rs:160,191,220,287`; `ws/handlers/presence/muc/implementation.rs:59,145`; `ws/registration.rs:555,595,685,693,861`; `ws/stream_management/implementation.rs:1467`; `ws/stream_management/registration.rs:490` |
| `sm_state` | A | `ws/state.rs:1786` | 104 | 10 | 8 | `ws/batch_write.rs:364,433,575,663,672,698,700,701,726,757,771,779,789,864,871,881,897,907,1001,1105`; `ws/cleanup.rs:812,842,874,876,889,905,925,939,949,996,1013,1237,1429,1570,1804,1806,1825,2611,2663,2702,2718,2727`; `ws/connection.rs:880,896,899,916,2379,2466,2482,2491,2501,2528,2529,2599,2692,2735,2736,2876,2884`; `ws/connection_actor.rs:406,627,631,736,921,1843,1847,2353,2401,2672,2690,2725,3108,3109`; `ws/frame.rs:1605,1610,1791,1813`; `ws/outbound.rs:66,79,90,91,97,105,211,220,274,288,289`; `ws/registration.rs:379,387,392,536,574,622,825`; `ws/stream_management/registration.rs:107,145,147,159,187,215,287,335,507` |
| `sm_ingress_fence` | A | `ws/state.rs:1788` | 6 | 2 | 4 | `ws/batch_write.rs:729,1108`; `ws/cleanup.rs:1789,2726`; `ws/frame.rs:1609`; `ws/stream_management/registration.rs:506` |
| `sm_inbound_completion` | A | `ws/state.rs:1789` | 34 | 5 | 5 | `ws/batch_write.rs:780,865`; `ws/connection.rs:2525,2594,2598,2625,2628,2629,2634,2644,2650,2657,2676,2678,2684,2691,2779,2847`; `ws/connection_actor.rs:2339,2352,2654,2671,2677,2717,2724,2892,2900,2989,3071`; `ws/frame.rs:1603,1790,1805,1812`; `ws/stream_management/registration.rs:107` |
| `inbound_frame_terminal` | T | `ws/state.rs:1792` | 3 | 3 | 2 | `ws/connection.rs:2252,2821`; `ws/frame.rs:1762` |
| `ordered_relay_handoff_tx` | T | `ws/state.rs:1793` | 1 | 1 | 1 | `ws/connection.rs:729` |
| `carbons_enabled` | A/I | `ws/state.rs:1798` | 7 | 2 | 4 | `ws/cleanup.rs:929,2287`; `ws/registration.rs:660,661,886`; `ws/stream_management/implementation.rs:1468`; `ws/stream_management/registration.rs:511` |
| `roster_interested` | A/I | `ws/state.rs:1800` | 9 | 2 | 6 | `ws/cleanup.rs:971,2288`; `ws/handlers/iq/dispatch.rs:182`; `ws/handlers/roster_fanout.rs:106,126`; `ws/registration.rs:663,664`; `ws/stream_management/implementation.rs:1469`; `ws/stream_management/registration.rs:512` |
| `blocklist_interested` | A/I | `ws/state.rs:1802` | 7 | 3 | 5 | `ws/cleanup.rs:972,2289`; `ws/handlers/iq/blocking.rs:76`; `ws/registration.rs:665,666`; `ws/stream_management/implementation.rs:1470`; `ws/stream_management/registration.rs:513` |
| `presence_available` | I | `ws/state.rs:1804` | 5 | 1 | 4 | `ws/cleanup.rs:2290,3909`; `ws/registration.rs:407`; `ws/stream_management/implementation.rs:1471`; `ws/stream_management/registration.rs:514` |
| `presence_show` | I | `ws/state.rs:1805` | 5 | 1 | 5 | `ws/cleanup.rs:2291`; `ws/handlers/presence/muc/implementation.rs:144`; `ws/registration.rs:413`; `ws/stream_management/implementation.rs:1472`; `ws/stream_management/registration.rs:515` |
| `presence_status` | I | `ws/state.rs:1806` | 4 | 1 | 4 | `ws/cleanup.rs:2292`; `ws/registration.rs:417`; `ws/stream_management/implementation.rs:1473`; `ws/stream_management/registration.rs:516` |
| `presence_priority` | I | `ws/state.rs:1807` | 5 | 1 | 4 | `ws/cleanup.rs:2293`; `ws/registration.rs:409,418`; `ws/stream_management/implementation.rs:1474`; `ws/stream_management/registration.rs:517` |
| `presence_payloads` | I | `ws/state.rs:1811` | 4 | 1 | 4 | `ws/cleanup.rs:2294`; `ws/registration.rs:424`; `ws/stream_management/implementation.rs:1475`; `ws/stream_management/registration.rs:518` |
| `pending_subscribes_flushed` | I | `ws/state.rs:1817` | 5 | 1 | 4 | `ws/cleanup.rs:993,2295`; `ws/registration.rs:719`; `ws/stream_management/implementation.rs:1476`; `ws/stream_management/registration.rs:519` |
| `pending_resume_stream_id` | A | `ws/state.rs:1821` | 8 | 4 | 4 | `ws/cleanup.rs:898`; `ws/connection.rs:2331,2335,2571`; `ws/registration.rs:462,499`; `ws/stream_management/registration.rs:66,339` |
| `pending_resume_h` | A | `ws/state.rs:1822` | 4 | 4 | 3 | `ws/cleanup.rs:899`; `ws/connection.rs:2572`; `ws/stream_management/registration.rs:119,340` |
| `pending_resume_claim` | A | `ws/state.rs:1824` | 4 | 4 | 3 | `ws/cleanup.rs:900`; `ws/connection.rs:2573`; `ws/stream_management/registration.rs:314,336` |
| `pending_finalized_resume_outcome` | T | `ws/state.rs:1830` | 3 | 3 | 3 | `ws/connection.rs:2463`; `ws/registration.rs:882`; `ws/stream_management/registration.rs:351` |
| `registry_owner` | A | `ws/state.rs:1839` | 21 | 5 | 10 | `ws/batch_write.rs:336`; `ws/cleanup.rs:894,902,1516,1842,2618`; `ws/connection.rs:1561,2358`; `ws/frame.rs:1818`; `ws/handlers/iq/blocking.rs:62`; `ws/handlers/iq/dispatch.rs:183`; `ws/handlers/presence/implementation.rs:258,288,421`; `ws/handlers/presence/muc/implementation.rs:61,146`; `ws/registration.rs:259,678,819`; `ws/stream_management/registration.rs:468,549` |
| `socket_lifecycle` | T | `ws/state.rs:1840` | 3 | 2 | 3 | `ws/cleanup.rs:1741`; `ws/registration.rs:685`; `ws/stream_management/registration.rs:472` |
| `recovered_pending_endpoint` | T | `ws/state.rs:1844` | 5 | 2 | 2 | `ws/registration.rs:484,491,639`; `ws/stream_management/registration.rs:415,418` |
| `resume_endpoint_handoff_blocked` | T | `ws/state.rs:1848` | 5 | 3 | 2 | `ws/registration.rs:473`; `ws/stream_management/registration.rs:416,420,430,461` |
| `write_interruption` | T | `ws/state.rs:1849` | 9 | 1 | 3 | `ws/batch_write.rs:635,666,686,708,855,1053,1069`; `ws/connection.rs:716`; `ws/registration.rs:654` |
| `live_resume_control` | T | `ws/state.rs:1852` | 9 | 0 | 4 | `ws/batch_write.rs:347,486,664,1049`; `ws/cleanup.rs:1742,1765`; `ws/connection.rs:714,1010`; `ws/registration.rs:655` |
| `suppress_sm_record_next_batch` | T | `ws/state.rs:1860` | 5 | 3 | 2 | `ws/connection.rs:2438,2439,2840,2841`; `ws/stream_management/registration.rs:520` |
| `pending_sm_enable_commit` | A | `ws/state.rs:1864` | 1 | 1 | 1 | `ws/connection.rs:2570` |
| `deferred_inbound` | T | `ws/state.rs:1870` | 12 | 6 | 2 | `ws/batch_write.rs:647,680,798,851,861,1059,1141`; `ws/connection.rs:931,1188,2806,2897,2898` |
| `sm_recovery_required` | A | `ws/state.rs:1875` | 13 | 1 | 3 | `ws/batch_write.rs:975`; `ws/cleanup.rs:838,875,891,895,915,934,1015,1843,2619,2776`; `ws/connection.rs:2789,2892` |
| `terminal_sm_recovery` | A | `ws/state.rs:1885` | 9 | 0 | 4 | `ws/batch_write.rs:976,986,988`; `ws/cleanup.rs:2426`; `ws/connection.rs:2633,2656,2811,2893`; `ws/replay.rs:687` |
| `pending_replay_completions` | A | `ws/state.rs:1889` | 5 | 2 | 1 | `ws/connection.rs:2989,2996,3006,3011,3016` |
| `terminal_sm_recovery_dropped` | A | `ws/state.rs:1892` | 0 | 0 | 0 | none |
| `terminal_sm_recovery_drop_warned` | T | `ws/state.rs:1894` | 0 | 0 | 0 | none |
| `send_window_pause_deadline` | T | `ws/state.rs:1902` | 4 | 2 | 1 | `ws/connection.rs:882,884,919,921` |
| `send_window_last_request_acked` | T | `ws/state.rs:1910` | 2 | 1 | 1 | `ws/connection.rs:896,899` |
| `state_machine` | A | `ws/state.rs:1931` | 8 | 0 | 6 | `ws/cleanup.rs:948,1236`; `ws/connection.rs:843,1360`; `ws/frame.rs:1707`; `ws/handlers/iq/blocking.rs:199`; `ws/outbound.rs:225`; `ws/replay.rs:344` |
| `keepalive_config` | T | `ws/state.rs:1935` | 1 | 0 | 1 | `ws/stream_management/registration.rs:504` |

## Current sites of sections 4 to 7

Every production line that names one of the identifiers each section is
about, definitions included. The analysis in sections 4 to 7 below was
written against an earlier commit; its line numbers are not current, these
are.

| Section | Family | Class | Sites | Files | Every site |
| --- | --- | --- | --- | --- | --- |
| 4 | Dual registration | O | 17 | 7 | `clustering/route_bridge/delivery/remote.rs:545`; `clustering/route_bridge/registration/socket.rs:991`; `server/dual_registration.rs:120,187`; `ws/cleanup.rs:1216,1476,1557,2767,3866`; `ws/registration.rs:252,767,775,813,932`; `ws/stream_management/registration.rs:451,541`; `server/session_janitors/implementation.rs:1632` |
| 5 | User and resource mirrors | A/I/O | 201 | 44 | `srv/admin/channels.rs:366,648,2196`; `clustering/local_claims.rs:41,665,677,696,713,944,967,1064,1187,1201,1211,1283`; `clustering/ordered_relay.rs:403,418,433,1329`; `clustering/relay.rs:52`; `clustering/relay/implementation.rs:33,544`; `clustering/route_bridge/delivery/receiver.rs:461`; `clustering/route_bridge/mod.rs:29,100,114,239`; `clustering/route_bridge/recipient_inventory.rs:14,52`; `clustering/route_bridge/registration/mod.rs:14`; `clustering/route_bridge/registration/owner.rs:3,5,45,373,377,382,515`; `clustering/route_bridge/registration/owner_sweep.rs:37`; `clustering/route_bridge/registration/owner_update.rs:65,72,261`; `clustering/route_bridge/registration/side_effects.rs:6,450,452`; `clustering/route_bridge/registration/socket.rs:14,257`; `clustering/route_bridge/registration/socket_forwarder.rs:14`; `clustering/route_bridge/types.rs:135,148`; `server/dual_registration.rs:47,121,134,188`; `server/http.rs:1013,1014`; `interpret/deps.rs:252`; `interpret/groupchat_archive.rs:1056`; `interpret/route_to_connection_plan.rs:85`; `interpret/routing.rs:629,846,899,917,986`; `ws/actor_local_endpoint.rs:44,209`; `ws/cleanup.rs:3630,3667,3694`; `ws/frame.rs:1424,1514`; `ws/handlers/iq/link_preview_lookup.rs:30,255,408`; `ws/handlers/presence/subscription/implementation.rs:28`; `ws/state.rs:1490`; `server/session_janitors/implementation.rs:1665,2221,6079`; `server/session_janitors/remote_owner.rs:35`; `srv/sm_promotion.rs:47`; `srv/sm_promotion/implementation.rs:17,44,67,297,567,1243`; `srv/sm_promotion/live.rs:4,21,57`; `srv/sm_promotion/pending.rs:9,21`; `xmpp/ingress/mailbox.rs:296,351`; `xmpp/ownership/claims.rs:18,28,40`; `xmpp/registry/connection_registry/outbound.rs:568,576`; `xmpp/registry/mod.rs:52,53,54,55`; `xmpp/registry/selection.rs:26,27,50,52,74,76,89,112,131,160,190`; `xmpp/registry/user_actor.rs:34,121,162,177,228`; `xmpp/registry/user_actor/delivery.rs:35,48,196,233,269,292,307,333,355`; `xmpp/registry/user_actor/implementation.rs:23,48,194,243,344,358,438,471,499,539,562,582,603,626,656,683,700,717,732,749,770,789,807,836,861,898`; `xmpp/registry/user_registry.rs:27,74,123,139,195,286,287,288,303,458,533,590,934,989,1008,1009,1039,1040,1059,1060,1085,1090,1095,1150,1156,1161,1220,1225,1230,1248,1272,1304,1345,1357,1415,1442,1461,1491,1529,1556,1573,1616,1678,1693,1697`; `xmpp/routing.rs:43,126,141` |
| 6 | Remote resource relay | T/O | 140 | 22 | `clustering/mod.rs:82`; `clustering/node.rs:8,778,786,1162,1171`; `clustering/relay.rs:50,103,125,138,150,151`; `clustering/relay/implementation.rs:31,539,541,569,574,587,589,612,617,630,632,655,660,675,677,705,710,725,747,748,824,2002,2015,2041,2054,2080,2093,2119,2132,2187,2189,2202,2445`; `clustering/relay/remote_resource_compat.rs:12,15,91`; `clustering/relay/remote_resource_compat/wire.rs:5,7,8,17,234,240,241,242,243,264,275,291,428,438,453,464`; `clustering/relay/resource_identity_wire.rs:9,48,82,96`; `clustering/resource_presence.rs:61,139,215,228`; `clustering/route_bridge/delivery/mod.rs:15,21,22`; `clustering/route_bridge/delivery/remote_socket.rs:8,13,14,433,685,704,892,897,898`; `clustering/route_bridge/mod.rs:47,49,55,57,112,235,236`; `clustering/route_bridge/registration/owner.rs:5,8,176,211,558`; `clustering/route_bridge/registration/owner_retire.rs:6,314`; `clustering/route_bridge/registration/owner_sweep.rs:7,172`; `clustering/route_bridge/registration/owner_update.rs:4,28,74`; `clustering/route_bridge/registration/owner_update_order.rs:22,39`; `clustering/route_bridge/registration/side_effects.rs:4,77,163,215`; `clustering/route_bridge/registration/socket.rs:7,11,13,248,265,359,502,630,735,1071`; `clustering/route_bridge/registration/socket_forwarder.rs:4,8,17,61,129,175,235,237,250`; `clustering/route_bridge/types.rs:24,26,227,229,242,403,413,426,440`; `srv/ingress/recovery_reachability.rs:139,147`; `server/http.rs:1689` |
| 7 | Room dispatch | A/T | 16 | 7 | `clustering/route_bridge/delivery/muc/proxy.rs:45,53`; `server/routes/interpret.rs:231`; `interpret/implementation.rs:7,309`; `interpret/relay_plan.rs:28`; `interpret/room_dispatch.rs:22,171,211,218,1162,1172`; `interpret/room_routing.rs:15,31,86`; `ws/cleanup.rs:3461` |
<!-- generated by 2046-current-caller-scan.py: end -->

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

### Generated sections (1 to 3 and the site table)

From the repository root, with `rg` on `PATH`:

```sh
PYTHONPATH=docs/planning/2046-type-resolved python3 -B \
  docs/planning/2046-current-caller-scan.py \
  --inventory docs/planning/2046-caller-inventory.md
```

The same run rewrites
[`2046-current-caller-candidates.md`](2046-current-caller-candidates.md),
the linked navigation index. The scan fails, changing nothing, when a field
of the three structs or a method of `ConnectionRegistry` or
`ConnectionEntry` has no class in the script, or the script classifies one
that no longer exists. Classify it in `classes` or `method_classes` and run
again.

### Baseline sections (0 and 4 to 11)

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
