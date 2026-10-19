# P2.2 — durable mailbox core: scope

Status: scoping only. Nothing here is implemented. Written 2026-10-18 against
`main@44ae8a8`.

## Why this exists

The distributed-actors critical path is now P2 (#1661) → P3 (#1662) → P4
(#1663). #1659 and #1660 closed the P1 effect executors on 11 and 14 October.

P2 has four internal boundaries. Only P2.1, P2.3 and P2.4 are written down, on
#1661. The #1425 ledger says the P2.2 decision "remains accepted as recorded",
but it has no P2.2 section. The only trace is P2.1's rule: no endpoint or
mailbox DDL until P3/P2.2. A sharpening comment on #1661 (11 August) defines
P2.2 as the durable mailbox DDL and core that P2.3 and P2.4 both consume:

- ordered lane tables;
- durable sequence allocation;
- canonical payload references.

Without it, the staged lane-by-lane relay cutover has nothing durable to cut
over to. #1948 (remote room acceptance before origin SM acknowledgement) is
already filed and depends on it.

## What exists today

- **Ordered relay (in memory).**
  - The channel key is `OrderedRelayChannel { origin, origin_epoch, recipient, target_epoch }`.
  - Recipients are a bare JID, a full JID, or a room lane (`MucStanza`, `MujiSignaling`).
  - Sequences are `u64`, allocated sender-side in `OrderedRelaySenderState`.
  - Receivers track `next_expected`, pending reservations and the last 64 ACKs per channel.
  - #1891 made diversions expire and added gap resync and owner refresh.
  - Nothing survives a restart.
- **Canonical ingress work (durable).**
  - `ingress_messages`: `MessageKey`, envelope V1/V2 holding the frozen room manifest.
  - `ingress_effect_intents`, `ingress_effect_receipts` and `ingress_delivery_receipts`: per-resource child progress.
  - `sm_ingress_appends`, `ingress_send_attempts` and `ingress_effect_descendants`.
  - Typed keys: `MessageKey`, `DeliveryKey::{effect, resource, inbox_projection}`, `EffectReceiptKey`.
- **Durable allocator precedents.**
  - `mam_archive_sequences`: upsert-and-return, locked in canonical order.
  - `clustering_muc_room_effects`: per-(lifecycle, revision) ordinals with a FIFO head predicate.
  - `push_publication_orders`.
- **P1.0 unit of work.**
  - `IngressUnitOfWork` on the main pool takes locks in this order: epoch, then exact claim, then child rows.
  - The protocol-epoch proof is per transaction.
  - V1009 epoch guards cover the 12 tables in `EPOCH_GUARDED_TABLES`.
- **Clustered production.** It deliberately keeps the unsealed keyed relay path: `allow_clustered_room_manifests()` is false outside tests.

## Scope of the P2.2 slice

In:

1. **Lane identity.**
   - A typed `MailboxLane` derived from the ordered relay's recipient and room-lane semantics.
   - Decide whether claim epochs are part of the key, or fence columns on a stable key. See open question 1.
2. **Durable sequence allocation.**
   - A per-lane allocator inside the P1.0 unit of work, in the P3.3 global lock order: protocol guard, ordered lane, `UserClaim`, endpoint, resource/effects, P1 receipts, SM.
   - Allocation never awaits the network.
3. **Mailbox entries.**
   - One row per `(lane, sequence)`, carrying a canonical payload reference (`MessageKey` plus effect or delivery key), never stanza bytes.
   - A closed disposition set covering pending, accepted, consumed, dead-lettered and policy-discarded.
   - Acceptance evidence is a disposition transition on this row, so #1948 adds no second acceptance ledger.
4. **Schema discipline.**
   - Expand-only, append-only checksummed migrations in the ledger, with no init-time DDL.
   - Every new table in `EPOCH_GUARDED_TABLES` and the guard manifest.
5. **Repository seam.**
   - Typed, transaction-taking repositories, the shape #1655 used for MAM and inbox:
     - append, returning the allocated sequence;
     - a bounded read after an exclusive cursor;
     - an exact disposition update fenced by lane and sequence.
   - No raw-pool escape.
6. **Retention.** Mailbox rows join the ingress retention frontier (V1025 descendants), so a referenced message is never collected under a live entry.

Out (later slices):

- Hint transport, `AcceptanceUow`/`TargetConsumptionUow` and the target-first reconciler: P2.3.
- Lane-layout directory, `is_lane_head`, bounded workers and admission: P2.4.
- Cutting any relay consumer over, and deleting ordered-relay code: the P2.3 lane cutovers.
- Endpoint DDL: P3.1.
- Any production activation.

Dark by default: nothing writes these tables in production until a consumer
slice cuts a lane over under its own verified cutover.

## Dependencies

- **P2.1 (main-pool transaction authority).** This is a hard prerequisite. P2.2's allocator and repositories must run in the single typed main-pool transaction P2.1 establishes. P2.1 has no slice issue yet either; it should be filed alongside this one.
- **#1689 and epoch activation.** Not prerequisites for dark DDL. Both are required before any lane goes live.

## Open questions for the architecture review

1. **Lane key.**
   - Today a claim-epoch change mints a new channel.
   - A durable lane keyed by epochs strands entries on every owner move.
   - A stable key with epoch fence columns needs an explicit hand-off rule.
2. **Allocator form.**
   - A per-lane counter row with upsert-and-return gives gap-free sequences and serialises appends per lane.
   - A Postgres sequence is cheaper but gapped.
   - P2.3's exclusive cursor tolerates gaps only if dispositions are explicit.
3. **Payload reference granularity.**
   - Option A: message-level references, with fan-out at consumption.
   - Option B: delivery-key references, with fan-out at acceptance.
   - #1948 freezes child delivery keys at acceptance, which favours B for room lanes.
4. **Single-node.**
   - Does a single-node deployment ever write a lane?
   - If not, the tables can stay Postgres-only, like the clustering control plane.
5. **Room lanes.** Do `MucStanza` and `MujiSignaling` map one-to-one onto mailbox lanes, or does the Muji side-band keep its own ordering?

## Acceptance for the slice

- A reviewed design resolving the open questions, recorded on the issue.
- Migrations, typed lane, sequence and disposition types, and transaction-taking repositories.
- PostgreSQL tests for:
  - allocator monotonicity under concurrent appends;
  - lock-order compliance with the epoch and claim locks;
  - fenced disposition updates;
  - epoch-guard rejection of unguarded writes;
  - retention never collecting a referenced message.
- No production writer, and no change to relay wire IDs.
