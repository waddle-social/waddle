# ADR-0017 Phase 1 Completion — Actor-Authoritative Registration

Companion design note to [ADR-0017](./0017-horizontal-scaling-remote-actors.md).
Status: **In progress** — Slices 0–2 landed. Slice 2 delivery cutover (1:1/DM
via the actor's `TrySend*`, liveness filter KEPT) and its empty-actor reaper
(`ReapUserIfEmpty` + `spawn_user_actor_reaper`, required because production
delivery activates `try_deliver`'s closed-channel eviction) are both in. Slice 3
(deleting the now-unused DashMap delivery/selection methods) is next.
See the slice order below for per-slice status and issue #1195 (ADR-0017
horizontal-scaling epic) for the live tracker.

## Why this exists

Phase 1 landed the additive foundation: the `UserActor` delivery surface and
dual-registration mirror. Two attempts to *read* the actor tree in production
were then reverted:

1. **Bare-JID selection cutover** — the actor could hold a coherent-but-partial
   set (resource A mirrored, B's mirror still in flight); a non-empty `[A]`
   looks complete, so an `is_empty()`-gated DashMap fallback never fired and B
   (live, available, top-priority) was silently missed.
2. **MUC fan-out delivery cutover** — routing through the actor with a DashMap
   fallback over the *same* shared channel duplicated delivery when a
   timed-out ask still ran while the fallback also fired.

Both trace to one root cause: the mirror is **best-effort async**, so a reader
cannot distinguish "the actor's set is complete" from "the actor's set is
mid-population."

## The asymmetry that drives the design

- **Register-lag is a false-negative** (a live resource is *missing*). Silent
  and unrecoverable — the reader has no signal that the missing resource
  exists. This is what killed both cutovers.
- **Unregister-lag is a false-positive** (a dead resource is *still present*).
  Self-healing: `SelectRoutableResources` filters on `is_presence_available()`,
  and `try_deliver` evicts a closed-channel entry on first send
  (`DroppedClosed`). The shared `mpsc` closes the instant the socket receiver
  drops, so a stale-present entry is caught the moment anyone touches it.

**Therefore: register must become synchronous/authoritative; unregister stays
best-effort.** That asymmetry is the whole trick — an authoritative-enough
actor without a synchronous round-trip on every teardown path.

The shared `Arc<ConnectionEntry>` (one allocation held by both DashMap and
`UserActor`) means presence/carbons never drift; only (a) set-membership
timing and (b) the replacement-ownership race can diverge. The design closes
exactly those two.

## Decision

**Synchronous, fail-closed register at bind; best-effort ownership-gated
unregister; no reader fallback.**

- **Register (bind)** becomes a bounded, fail-closed actor write *before* the
  connection is considered registered. On failure the DashMap insert is rolled
  back (`unregister_if_owner`) and the bind returns
  `SessionInitializationFailed` (reusing the existing blocklist-load failure
  surface → RFC 6120 internal-server-error + close; client reconnects with
  backoff). This eliminates the register-lag false-negative: a resource is in
  the actor before its own bind returns, so before its client is "available"
  and before any sender could route to it. kameo's per-actor FIFO mailbox is
  the ordering guarantee.
- **Ownership gating** via the owner token (the shared `Arc<AtomicBool>`
  already used as the DashMap ownership key, compared with `Arc::ptr_eq`).
  `RegisterConnection`/`UnregisterConnection` carry it so a lagging unregister
  for a superseded connection cannot evict the newer resource — matching the
  DashMap's `unregister_if_owner` semantics exactly.
- **Unregister** stays a best-effort bounded `tell` at all teardown sites
  (teardown-lag is self-healing), now ownership-gated.

### Availability (full version)

To keep a wedged single `UserActor` from failing *server-wide* binds, split the
global registry out of the blocking path at the bind site:

1. `user_registry.ask(GetOrCreateUser)` — O(1), never awaits a child, so a
   wedged child cannot stall the registry mailbox.
2. `childref.ask(RegisterConnection).mailbox_timeout(T).reply_timeout(T)` —
   bounded, directed at the per-user actor. A wedged `UserActor` fails only
   *that* user's binds.

The split opens a get-or-create/register prune race, closed by **reaping empty
`UserActor`s lazily** (a candidate set + a small reaper janitor, mirroring
`spawn_room_dormancy_janitor`) instead of pruning synchronously on empty.

### Reduced-scope first slice (recommended starting point)

Keep register **atomic inside `RegisterUserResource`** (registry handler does
GetOrCreate + child `RegisterConnection` in one mailbox turn) and keep
synchronous prune. This is **race-free without a reaper** (register is atomic
in the registry mailbox) and gets the core authoritative-register property. Its
only cost is the bounded SPOF: a genuinely-wedged child stalls the global
registry up to `CHILD_ACTOR_TIMEOUT` (2s) for other binds. Since `UserActor`
handlers never await I/O, wedging is rare and the damage is bounded. The
get-or-create/reaper split is a fast follow-up availability slice.

## Slice order

- **Slice 0 — authoritative registration.** ✅ **Landed** (commit `12328df`).
  Owner tokens, synchronous fail-closed bind register + rollback, best-effort
  ownership-gated unregister. *No read cutover* — pure strengthening of the
  mirror into an authority. Entire e2e suite stays green unchanged. Follow-up
  fix (commit `4e3bbd3`): SM-detach also mirror-unregisters, so a
  detached-then-expired session no longer leaks its `UserActor` entry.
- **Slice 1 — selection cutover.** ✅ **Landed** (commit `5367ec2`).
  `route_to_connection` bare-JID selection
  reads *both* tiers (`SelectRoutableResources`, then `GetResources`) from the
  *same* authoritative actor — no DashMap *fallback* for the candidate set. The
  prior revert mixed sources (tier-1 actor, tier-2 DashMap), which is why a
  partial `[A]` never consulted tier 2.

  **Transitional DashMap-liveness intersection (council review on PR #1177).**
  While delivery still reads the DashMap (`deliver_peer_to_full`, cut over in
  Slice 2), each tier's actor result is intersected with DashMap membership
  (`is_connected`). This is required to avoid a stale-extra message-loss window:
  the best-effort, owner-gated unregister mirror can leave a resource in the
  actor whose DashMap entry was already removed at teardown (teardown does not
  flip the shared presence atomic, so the actor may still report it available).
  Selecting such a stale resource and handing it to DashMap delivery would find
  it gone and — if it were the only/top candidate — skip the offline pass,
  silently losing or delaying the message. The intersection is a *pure
  strengthening*, not a mixed-source fallback: since Slice 0 makes registration
  authoritative, `DashMap ⊆ actor` for live resources, so actor-selection ∩
  DashMap == the DashMap live set ranked by the actor — provably equal to the
  legacy selection, no false-negative. Applying the filter *inside* each tier
  keeps a stale high-priority extra from masking a live lower-priority resource
  (tier-1 filtered-empty falls through to tier-2). The filter is deliberately
  **KEPT through Slice 2** (the delivery cutover): keeping it holds this
  stale-extra window closed without needing the headless-persistence gate, and
  keeps selection byte-for-byte identical to the legacy behaviour. A later slice
  retires it once teardown flips the shared ownership atomic (or the empty-actor
  reaper eagerly reaps stale extras), returning tier 1 to the actor's
  `SelectRoutableResources`.
- **Slice 2 — delivery + MUC fan-out cutover.** ✅ **Landed** (delivery cutover
  + empty-actor reaper). `deliver_peer_to_full`
  routes via the actor's `TrySendPeer` with **no DashMap send fallback**.
  Duplicate is impossible structurally (one `try_send` per recipient). Full
  design, locked:

  > **Merge note (main #1106 "shared fan-out recipient pass").** Since this note
  > was written, `main` added `run_fanout_recipient_pass` in
  > `route_to_connection.rs`, which runs the recipient pass ONCE over the DM
  > delivery set and returns a `processed` wire copy for the CALLER to deliver
  > (it does not itself deliver). Slice 2 must cut over the **two caller-side
  > delivery loops**, both in `route_to_connection.rs`'s bare-JID `Err(bare)`
  > arm, NOT `run_fanout_recipient_pass` itself:
  >
  > 1. **DM path** (`is_dm_message && !live_targets.is_empty()`, ~L397): the
  >    `for full in &live_targets { registry.send_to(full, (*processed).clone()) }`
  >    loop — a **blocking `DirectFrame`** send of the already-recipient-passed
  >    stanza. Cut over to the actor's **`TrySendDirect`** (DirectFrame, not
  >    PeerStanza — the pass already ran). `SendResult::NotConnected/ChannelClosed`
  >    → `deliver_to_detached` today; map `BroadcastOutcome`/SendError per the
  >    disposition rules below. `queue_processed_for_detached` for detached
  >    targets is unchanged (SM registry, not the actor).
  > 2. **Non-DM / groupchat + the `FanoutPassResult::Unavailable` fallback**
  >    (~L474): the `for full in live_targets { deliver_peer_to_full(...) }` loop.
  >    Cut `deliver_peer_to_full` over to **`TrySendPeer`** (PeerStanza).
  >
  > Both loops currently take `registry: &ConnectionRegistry`; thread
  > `deps.user_registry` in and keep the DashMap path only for `None`-Deps
  > (tests). The full-JID `Ok(full)` arm (~L271) also calls `deliver_peer_to_full`
  > and must be cut over too. The selection feeding all of this is already
  > actor-authoritative (Slice 1). NB: the DM `send_to` is *blocking* today, so
  > the 1:1 backpressure change below applies to DM delivery as well.


  - **Backpressure decision (maintainer-visible behaviour change).** 1:1
    delivery moves from the **blocking** `send_peer_to` to the actor's
    **non-blocking** `TrySendPeer`. On a full recipient channel,
    `BroadcastOutcome::DroppedFull` → **log + Prometheus dropped-full count +
    drop the frame** — the same treatment groupchat fan-out already gives, and
    what the ADR-0017 non-blocking delivery surface mandates. Rationale: a
    wedged/zombie recipient can no longer stall global dispatch (issue #699).
    Cost: a severely backpressured 1:1 recipient can lose a *live* frame under
    sustained load (the sender-side MAM copy still exists; the recipient catches
    up via MAM). This is a deliberate, called-out behaviour change from the
    current "1:1 keeps blocking backpressure" comment.
  - **Disposition + error mapping.** `deliver_peer_to_full` gains a
    `user_registry` param and returns a `PeerDelivery { DeliveredLive,
    QueuedDetached, Dropped }`. Production (`Some`): `GetUser(bare)` then
    `TrySendPeer` (mailbox+reply timeout). Map `Ok(Delivered)` → `DeliveredLive`;
    `Ok(DroppedFull)` → `Dropped`; `Ok(NotConnected | DroppedClosed)` →
    `deliver_to_detached` (→ `QueuedDetached` if it queued, else `Dropped`);
    `Err(SendError)` (mailbox-full / reply-timeout) → **`Dropped`, never route
    to detached** (kameo does not cancel the enqueued handler — it may still
    deliver post-timeout, so routing to detached would double-deliver).
    `GetUser` `Ok(None)`/`Err` → `deliver_to_detached` (no delivery attempted,
    safe). Test fixtures without an actor (`None`) keep the existing DashMap
    delivery path until Slice 3 migrates them. `deliver_to_detached` returns a
    `bool` (queued).
  - **Slice 1 liveness filter — KEPT (not retired) in this slice.** The
    shipped cutover deliberately keeps `select_bare_jid_live_targets`'
    DashMap-liveness intersection (`filter_deliverable`, the `registry` param,
    the `GetAvailableResources` + post-filter-max logic). Keeping it holds the
    stale-extra window closed without the headless-persistence gate below and
    keeps selection byte-for-byte legacy-identical. Retiring it — and switching
    to self-healing via `TrySendPeer` → `DroppedClosed` eviction — is deferred
    to a later slice, once teardown flips the shared ownership atomic or the
    empty-actor reaper eagerly reaps stale extras.
  - **Headless-persistence gate — deferred with the filter retirement.** Only
    needed once the liveness filter is retired: with the filter in place a sole
    stale extra can never be selected, so there is no `DroppedClosed`-only
    delivery that would skip archiving. When the filter is later retired, the
    bare-JID else-branch must track whether *any* live target returned
    `DeliveredLive`/`QueuedDetached` or any detached target queued, and run
    `run_headless_recipient_pass` (local-domain only) when none did.
  - **Empty-actor reaper (LANDED — Copilot review on PR #1177).** Slice 2
    activates `try_deliver`'s closed-channel eviction in production. When that
    eviction removes a `UserActor`'s *last* resource, the now-empty actor is not
    pruned by the explicit `UnregisterConnectionAndReportEmpty` path, so an empty
    actor can accumulate in `UserRegistryActor.users` if a teardown's
    `mirror_unregister` was dropped (e.g. mailbox timeout). Shipped as the
    atomic `ReapUserIfEmpty` registry message (single-handler check-and-remove,
    so a concurrent re-registration cannot orphan a live resource) driven by the
    periodic `spawn_user_actor_reaper` (mirrors `spawn_room_dormancy_janitor`;
    every registry ask bounded by `REAPER_ASK_TIMEOUT`). The `UserActor`
    deliberately does NOT self-prune on empty: that races an in-flight
    re-registration and trips the crashed-actor poison path.
  - **Council review** (concurrency + correctness) before commit: 1:1
    backpressure loss, Finding-1 gate soundness, no MUC duplicate (no
    fallback), terminal-timeout no-double-deliver, ordering.
- **Slice 3 — delete the DashMap *delivery* methods** (`send_peer_to`,
  `try_send_peer_to`, `select_routable_resources_for_user`,
  `get_resources_for_user`) once Slices 1–2 are green in prod and the remaining
  `None`-path delivery tests are migrated onto the actor.

## After Phase 1

- **Phase 2** — owned libp2p swarm subsystem behind the `clustering` flag
  (node discovery only; peer allowlist).
- **Phase 3** — Postgres-authoritative ownership claims (epoch-fenced),
  fenced SM-session persistence, cross-node XEP-0198 resume via claim-steal,
  durable MUC room ownership, XEP-0397 ISR.
- **Phase 4** — cross-node routing GA; unlock Helm `replicaCount > 1` and
  `clustering.enabled`.

Tracking issue: #1195 (ADR-0017 horizontal-scaling epic) for live slice/phase
status.

## What survives Phase 1

The DashMap `connections` map itself is **not** deleted at Phase 1 completion:
it still backs carbons fan-out, roster/blocklist-interested reads,
`active_sm_stream_ids`, SM stream-id publication, and presence-probe state —
all reading the *same shared `ConnectionEntry`*. Dual-write into the DashMap
remains (now synchronous and ownership-correct, so it cannot drift). Deleting
the DashMap struct entirely is a separate, later effort once those consumers
migrate. "Delete the DashMap **delivery** path" is the achievable Phase 1
milestone.

## Test / e2e gates

- `dm_delivery_mam.cue` — bare-JID DM lands on the highest-priority available
  resource, archived once (Slices 1+2).
- `muc_groupchat_fanout.cue` — every occupant receives exactly one reflection;
  no duplicate, no miss (Slice 2).
- `multi_device_carbons.cue` — multiple same-user resources all selected/served
  (Slice 1).
- `xep_0198_stream_management.cue` — resume re-registers the same FullJid with
  no selection/delivery gap; ownership-gated register/unregister survives
  detach→resume replacement.
- New unit tests: (a) actor set complete synchronously after bind returns;
  (b) ownership-gated unregister for a superseded owner does not evict the
  replacement; (c) actor-register failure rolls the DashMap back (neither map
  retains the resource); (d) [full version] reaper prunes an empty actor only
  after grace and never one with a re-registration in flight.
