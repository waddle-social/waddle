# ADR-0017 Phase 1 Completion — Actor-Authoritative Registration

Companion design note to [ADR-0017](./0017-horizontal-scaling-remote-actors.md).
Status: **In progress** — Slices 0–1 landed; Slice 2 (delivery cutover) next.
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
  (tier-1 filtered-empty falls through to tier-2). Slice 2 retires this filter
  when delivery moves to the actor (whose `try_deliver` evicts closed channels).
- **Slice 2 — delivery + MUC fan-out cutover.** ⏳ **Next.** `deliver_peer_to_full`
  routes via the actor's `TrySendPeer` with **no DashMap send fallback**.
  Duplicate is impossible structurally (one `try_send` per recipient). Full
  design, locked:

  > **Merge note (main #1106 "shared fan-out recipient pass").** Since this note
  > was written, `main` added `run_fanout_recipient_pass` in
  > `route_to_connection.rs`, which runs the recipient pass ONCE over the DM
  > delivery set and writes each live target a `DirectFrame` (the per-target
  > `deliver_peer_to_full` loop now only serves the non-DM / groupchat-reflection
  > path). Slice 2 must cut over **both** the fan-out pass's per-target write AND
  > the `deliver_peer_to_full` loop to `TrySendPeer`, with the disposition +
  > headless-gate logic applied to the fan-out set. The selection feeding it is
  > already actor-authoritative (Slice 1).


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
  - **Retire the Slice 1 liveness filter.** `select_bare_jid_live_targets`
    tier 1 returns to the actor's `SelectRoutableResources` (drop
    `filter_deliverable`, the `registry` param, and the
    `GetAvailableResources` + post-filter-max logic): delivery now self-heals
    a stale extra via `TrySendPeer` → `DroppedClosed` eviction.
  - **Headless-persistence gate (keeps Finding 1 closed).** In the bare-JID
    else-branch, track whether *any* live target returned
    `DeliveredLive`/`QueuedDetached` or any detached target queued; if **none**
    reached a live/detached resource, run `run_headless_recipient_pass`
    (local-domain only) so a sole stale extra (`DroppedClosed` → drop) still
    archives.
  - **Empty-actor reaper (MUST land with this slice — Copilot review on PR
    #1177).** Slice 2 activates `try_deliver`'s closed-channel eviction in
    production. When that eviction removes a `UserActor`'s *last* resource, the
    now-empty actor is not pruned by the explicit
    `UnregisterConnectionAndReportEmpty` path, so an empty actor can accumulate
    in `UserRegistryActor.users` if a teardown's `mirror_unregister` was dropped
    (e.g. mailbox timeout). Land the lazy reaper (see "Availability (full
    version)" — a candidate set + periodic sweep mirroring
    `spawn_room_dormancy_janitor`) together with the delivery cutover so the
    eviction and pruning paths ship as one. Do NOT self-prune on empty: it
    races an in-flight re-registration and trips the crashed-actor poison path.
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
