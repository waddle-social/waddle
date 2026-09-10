# RFC 0018 — Ingress authority cutover with canonical identity (#1657)

Status: implementing (PR opened 2026-09-06). Reviewed in four rounds by an
independent high-reasoning reviewer before implementation (REJECT ×3 →
APPROVE-WITH-CHANGES; the last change is folded into §3.2).

## 1. Behavioral delta

Committed ingress decisions determine message responsibility. For every
inbound `<message/>` the XEP-0198 handled count `h` advances only after the
message's ingress transaction commits; every effect that runs after commit is
recorded first as a durable, payload-complete intent. Origin-id duplicates are
decided by the cluster-global alias (sender bare JID, target, origin-id) and
repaired inside the transaction; the MAM-layer origin dedupe is deleted. The
shadow scaffolding (#1656/#1695) is deleted; there is one ingress path.

Stated limitations (strict non-regressions against `main`, owned by later
roadmap slices): (i) lost post-commit effects are durable (envelope,
intents, receipts) but not executed by a recovery executor (#1658);
(ii) non-idempotent fan-out to non-senders remains suppressed on a repaired
duplicate except for unfinished recorded direct resources tracked below;
per-resource detached delivery now guarantees one durable queue allocation per
(recorded obligation, resource), with no retry-induced duplicate (§3.3a), rather
than at-least-once queue allocation; (iii) live full-JID delivery keeps the
destination connection's own recipient archive/inbox pipeline (#1658, now tracked as #1759);
(iv) subject/pin/membership supersession keeps `main`'s semantics
(#1659/#1660); (v) non-resumable streams have no durable
connection-generation fence (follow-up issue); (vi) extension-host dispatch runs outside ingress: offline rows and candidates are written immediately without receipts, and groupchat notification recovery rows are not created; a typed Extension ingress identity is the follow-up.

### Recovery follow-ups from combined review

Five gaps were inherited from the pre-review implementation and filed under
#1658. All five are resolved in PR #1752: per-resource detached delivery
progress (#1739, §3.3a), per-plugin observer obligations (#1740, §3.3b),
atomic notification recovery settlement (#1743, §3.3c), ordinary pending-row
reconstruction (#1742, §3.3d) and periodic terminalization maintenance
(#1741, §3.6a).

### Settlement contract (#1752)

An effect whose completion evidence is a row in the global database (delivery
progress, notification candidate, recovery completion) commits that evidence
and the receipts of the recorded intents it discharges in ONE ingress
transaction: attested epoch → canonical row `FOR UPDATE` → effect tables →
`settle_recorded` → commit. No actor, registry, extension or socket call runs
while that transaction is open. Such effects are executed by unit-of-work arms
(`execute_uow`) selected by exact effect variant; the generic executor never
writes a receipt owned by an arm (`arm_owned_receipts`). Evidence discharges a
recorded intent only by exact identity, with two typed coalescences: a
`Duplicate` candidate insertion discharges the recorded `Inserted` obligation,
and a `Completed` recovery discharges a recorded `DeferredPolicy` obligation.

## 2. Three phases per inbound message

**Phase A — plan** (no locks, bounded, read-only). The whole message handler
path (including the early handlers: group-DM/MUC invitations, DM pins, MUC
direct) and the interpreter feedback loop run in *plan mode*: reads execute
(MAM lookups, enrichment, blocklist, room-actor snapshot asks with an explicit
bounded timeout, room execution-path resolution local vs remote, recipient
resolution) but every write and every external effect is captured as a typed
`PlannedEffect`. Output: `IngressPlan { sanitized message, digest input
(authority set from message shape), room execution path, durable effects,
external effects, intents, standard error reply }`. The plan is computed once;
transaction attempts reuse it.

- Identity precedes writes. `AliasOutcome::Inserted|NoOrigin`: the
  canonicalize-minted stanza-ids are authoritative. `Existing`: the
  transaction reads the recorded `ArchiveAuthoritative` intents and re-stamps
  the plan with those trusted ids (a pure transform; handlers are not re-run).
- `DispatchToRoom` local: bounded snapshot ask → room chain (pure) → its
  events planned recursively (room archive + occupant inbox projections are
  durable; reflection fan-out + push are external). Remote owner:
  `ExternalEffect::RelayToRoomOwner` + intent `DispatchToRoomRemote`.
- `RouteToConnection` is split: recipient *preparation* (offline bare JID
  headless pass, live/detached bare JID shared pass, detached full JID) is
  durable in Phase B; *delivery* (live peer enqueue, pending-delivery insert,
  carbons, push candidates) is external.

**Phase B — commit** (the ingress transaction; `run_with_retry`, fresh unit of
work and fresh per-attempt state each attempt; lock timeout 100 ms, statement
timeout 250 ms; no actor asks, no extension calls, no socket writes). Lock
order: epoch `FOR SHARE` (begin) → principal `FOR SHARE` → [resumable] exact
SM claim `FOR SHARE` → [resumable] `ingress_sm_streams` `FOR UPDATE`, wire
binding lookup, ordinal := `handled_ordinal + 1` → alias resolution with the
canonical row `FOR UPDATE` → [guarded local room effect] durable room claim
`FOR SHARE` (owner/epoch only; no admission-snapshot revalidation, and no
room revalidation for unfenced single-node rooms) → load recorded intents → reconcile → durable effects via
the transaction-taking repositories with typed failures → intents → envelope →
receipts for durable effects → sm ref (`FOR UPDATE`) → frontier CAS +
checkpoint → commit.

**Phase C — execute** (post-commit, cancellable, never authority).
(1) `settle_inbound_dispatch(Handled)` → `h` advances (contiguity tracker
retained for mixed stanza types; the ordered-relay socket-acceptance deferral
is removed for messages); (2) external effects run under their own bounded
budget, each reporting `ExternalOutcome::{Done, Failed, Uncertain}` and
writing a receipt on `Done`; (3) frames are written; (4) if every recorded
intent has a receipt, a follow-up transaction terminalizes the canonical row
(`FOR UPDATE`); otherwise the row stays non-terminal (protected from GC,
metered `ingress.effects.unresolved`). A Phase-C timeout never changes the
disposition: `StanzaTimeout` maps to `Unhandled` only before commit.

Full-JID room reflections to occupants owned by another node ride the ordered
full-JID relay (`deliver_ordered.v10`), with the room's `RoomActor` claim as
both origin and sender claim; XEP-0045 occupant-copy semantics are unchanged.
`IngressNonTerminalBacklog` alerts on canonical rows older than 10 minutes
that remain non-terminal (#1749/#1750), including missing receipts and
receipt-complete rows awaiting terminalization.

Remote carbon replies preserve completed targets even on typed `Incomplete`
results. The origin durably records those full-JID targets in
`ingress_carbon_receipts`, keyed by the recorded `RelayCarbons` intent and
recipient. Same-origin retries add confirmed targets to the existing typed
exclusion request, so only unfinished targets receive a carbon. The whole
`RelayCarbons` intent receives its normal effect receipt only on the owner's
complete `Applied` reply; incomplete replies leave that intent unresolved.



## 3. Identity

### 3.1 Admission identity
`IngressStreamIdentity::{Resumable { sm_ingress_id, SmClaimFence },
Ephemeral { connection_generation, principal }, Relayed { canonical_ref,
room fence }}`. Principal `FOR SHARE` is always asserted. Resumable adds the
SM claim, stream row, ordinal, checkpoint. Ephemeral has no refs/frontier;
registry ownership is re-checked before Phase C. Relayed (owner side of a
relayed groupchat) has no SM parts and is fenced by the room claim.

Non-advancing outcome: Resumable → ordinary hole (`abandon`) → transport ends,
session resumable before the hole. Ephemeral → typed `<stream:error>` then
close (`internal-server-error` for storage/serialization/timeout/ambiguous
commit/lineage/epoch; `not-authorized` for principal loss; `conflict` for fence
or registry loss). Committed semantic denials are standard stanza errors
(advancing) on every identity.

### 3.2 Receive identity and checkpoint
No in-memory ordinal mirror; `sm_sessions.shadow_ordinal` is dropped.
- `ingress_sm_streams.checkpoint_h` (u32 widened to BIGINT): Phase B writes
  the contiguous handled count that becomes exposable once this message is
  handled — `seq` when no lower sequence is pending, otherwise the current
  contiguous count (a pending IQ hole is never acknowledged).
- `ingress_sm_refs.(wire_generation, wire_h)` +
  `UNIQUE (sm_ingress_id, wire_generation, wire_h)`: the binding
  from the message's reserved wire position to its canonical row and ordinal.
  Phase B looks the position up first: bound → `ExistingCommitted` (crash after
  commit, ambiguous commit, or the hole case) → no new row/ordinal, reconcile
  as a duplicate, `h` advances; unbound → fresh ordinal.
- Resume (local and cross-node) restores
  `h := max_in_window(sm_sessions.inbound_count, checkpoint_h)`.
- **Checkpoint before ACK exposure.** When a deferred completion (an
  asynchronously forwarded IQ) makes previously committed message positions
  contiguous, the tracker marks the checkpoint dirty; every ACK path (`<a/>`
  on `<r/>`, batch-writer acks, `<resumed/>`) flushes `checkpoint_h := h` to
  the stream row before exposing the count. Otherwise a crash after that ACK
  would resume below a count the client already discarded and fresh stanzas
  would collide with retained bindings.
- `ingress_sm_streams.wire_generation` increments when the checkpoint moves
  forward across an XEP-0198 counter wrap. A reserved position resolves to the
  checkpoint generation or its adjacent generation using `max_in_window`, so a
  pending IQ hole can hold the checkpoint before a wrapped message, and an
  exact pre-wrap replay still selects its original binding. Reusing `h` in a
  later generation creates a fresh binding; refs survive until stream retirement.

### 3.3 Durable payload
`ingress_messages.envelope` = the post-transform typed message for the
sender's target (after sanitization, room canonicalization, enrichment),
serialized once at the storage boundary; persisted for accepted and rejected
rows. Per-recipient copies and error replies are reconstructed by a pure
function over `(envelope, intent)` tested without actors, extensions or
policy lookups.

### 3.3a Per-resource detached delivery progress (#1739)

A recorded direct route freezes its capture identity and resource fanout.
`ingress_delivery_receipts` records each successful resource under the canonical
message key and complete effect receipt key. Duplicate planning intersects
available targets with that frozen fanout and removes completed resources,
including when a formerly detached resource is now live. Every replay uses the
canonical envelope to rebuild its recipient payload. Unavailable recorded
resources remain unresolved; newly available resources outside the recorded
fanout are never added.

Each resource append runs before its progress transaction. That transaction
attests the epoch, locks the canonical message, records resource progress, and
settles the aggregate route only when all recorded resources are covered.
The progress and aggregate receipt commit atomically. No registry or socket
operation runs while this transaction is open.

**ONE DURABLE QUEUE ALLOCATION PER (recorded obligation, resource), WITH NO
RETRY-INDUCED DUPLICATE** is guaranteed by a stream-independent ledger keyed by
`(message_key, receipt_kind, semantic_identity_hash, resource)`. Its database
constraint is the gate, and the ledger entry is written in the same transaction
as the SM snapshot. Concurrent replay and retries after an append but before
resource-progress settlement therefore cannot allocate another durable queue
entry for the same recorded obligation and resource.

The remaining limits are explicit:

- XEP-0198 itself still permits client-observed duplicates after an uncertain
  acknowledgement: an unacknowledged stanza may already have been received,
  so retransmission can duplicate it (see
  [`xeps/xep-0198.xml`](../../../xeps/xep-0198.xml), §4 Acks, duplicate warning
  near line 367).
- When no unexpired session exists, the append does not happen at all and the
  obligation stays unresolved for its recorded route to retry or degrade.
- The `RegistryFrame` live-transport branch is not durable queue delivery and
  is out of scope.

MUC groupchat occupant fanout is outside this mechanism. `QueueDetached`
effects without a matching recorded direct route retain generic execution and
receipt ownership; the variant is shared by MUC occupant delivery.

### 3.3b Per-plugin observer obligations (#1740)

Room observer work is one typed intent and one external effect per eligible
plugin: `RoomObserver { room, requester, sender, plugin: PluginId }` (codec tag
27; the semantic key includes the plugin). Eligibility is frozen at planning
from the extension manager's exact predicates (valid hook body, declared
`MessageObserve` capability, grant). Ready observer effects execute
concurrently under the shared Phase C deadline and are receipted independently,
so a slow plugin cannot starve a fast plugin's receipt. On replay the recorded
plugin set is authoritative: unrecorded plugins never run historical messages,
recorded plugins missing from the fresh plan are rebuilt from the canonical
envelope, and a plugin that is missing or revoked at execution stays
unresolved. Host-warning replies keep their non-proving semantics.

### 3.3c Atomic notification recovery settlement (#1743)

`groupchat_notification_recovery` rows carry the canonical `message_key` and
are inserted only inside the ingress transaction. Phase C executes the
`NotificationCandidate` effect through a unit-of-work arm: candidate insertion,
recovery completion and the receipts for the recorded
`NotificationActivityPreview` and `GroupchatNotificationRecovery` intents commit
together. A T0 push-policy error records a typed `DeferredPolicy` obligation
instead of nothing. The background sweep is obligations-driven: it reads
recorded intents under the canonical lock, rebuilds candidates purely from the
canonical envelope (no MAM read, no T0 re-evaluation for recorded `Inserted`
work), evaluates policy only for `DeferredPolicy` rows and only outside the
lock, then re-validates and settles. Completed-but-unreceipted orphans settle
the recovery obligation alone; pruning skips rows whose canonical message is
still non-terminal; a recovery row whose canonical message is gone is deleted.

### 3.3d Ordinary pending-row reconstruction (#1742)

When T0 push-policy preparation returns `RetryLater`, ingress records only the pending-delivery obligation, leaves `notification_outboxed_at_ms` unset on execution and replay, and defers notification preparation to the existing XEP-0357 janitor.

`QueueOfflineDelivery` executes through a unit-of-work arm: under the canonical
lock the arm skips insertion when the recorded `PendingDelivery` receipt exists,
otherwise checks for the recorded row id before the quota-predicated insert
(Postgres recipient advisory lock), inserts the frozen notification candidate
and marks `notification_outboxed_at_ms` in the same transaction, and settles
exactly the recorded pending and notification obligations. Quota exhaustion is
a typed outcome; the XEP-0160 `<service-unavailable/>` bounce is sent after the
transaction closes. On replay, ordinary pending obligations (never invitation-
owned rows) are rebuilt purely from the canonical envelope, the recorded row id,
the canonical acceptance time and the recorded notification intents: a fresh
offline effect for the same recipient is replaced, a fresh live route for an
originally offline recipient is refused (recorded audience wins), and when the
row is already receipted only the unfinished notification work runs. T0 push
policy is never re-evaluated for recorded `Inserted` candidates.

### 3.4 Alias-only dedupe, MAM identity, reconciliation
- Deleted: `origin_dedup.rs`, the `origin_dedup_*` columns and both partial
  unique indexes, `StoreOutcome::Deduplicated`, pool and transaction dedupe
  paths, the in-memory fake's dedupe. Kept: `origin_id` column and
  `get_message_by_sender_and_origin_id` (corrections/retractions).
- `MamArchiveRepository::store{,_fenced}(tx, archive, message,
  ArchiveExpectation::{Fresh, Existing { stanza_id, archived_at }})`. `Fresh`
  + primary-key hit → typed `Conflict` (non-advancing). `Existing` → row
  present → `Existing`; tombstoned → `TombstoneHit`; absent and inside MAM
  retention and not deleted → repair-insert with the recorded `archived_at`
  → `Repaired`; outside retention → `Expired` (no insert).
- Reconcile before apply: `ReconcileVerdict::{FirstCommit, Consistent,
  Repaired { omissions }, Contradiction }`. Identity is compared per assigning
  authority (a different stanza-id under the same `(by, archive)` is a
  contradiction). Contradictions on immutable identities → non-advancing
  `IntentContradiction`; audience/policy drift → recorded wins,
  `ExistingDivergent` (advancing). A missing sender-side `ArchiveAuthoritative`
  on an `Existing` alias is unreachable by construction → `Storage`.
- Inbox: each projection is applied once, keyed in `ingress_deliveries`
  (`DeliveryKey::InboxProjection`); the upsert is monotonic on the entry
  timestamp (whole seconds, ties keep application order — archive ids can be
  client-chosen, so they are not an ordering), so repairing an older message
  never rewinds a newer row.
- Replay on a duplicate: durable effects are repaired in Phase B; Phase C
  re-applies idempotent fenced effects through the existing guarded handler
  code (membership grants never demote; subject re-apply **and rebroadcast**
  to all occupants per XEP-0045 §8.1), sends the sender's reflection/reply,
  and suppresses non-idempotent fan-out to non-senders except for unfinished
  recorded direct resources with per-resource progress.
- Owner side of a relayed groupchat runs the same pipeline with the
  `Relayed` identity; the proxy envelope carries `IngressCanonicalRef
  { message_key, sender_bare, origin_id }` (relay ask/reply version bumped).
  No room `ArchiveAuthoritative` intent yet → `OwnerFirstAcceptance` (full
  fan-out); present → `OwnerDuplicate` (repair, sender-only reflection +
  subject exception, `WriteAccepted { stanza_id: recorded }`).
- Deposed-owner scenario: first message commits under the live room claim;
  claim stolen; retry → `ClaimFenceMissing` before alias resolution.

### 3.5 Decision matrix
Advancing (committed): `Accepted`, `ExistingCommitted`, `ExistingConsistent`,
`ExistingRepaired`, `ExistingDivergent`, `OwnerFirstAcceptance`,
`OwnerDuplicate`, `AliasConflict` (rejection row: own message key, digest of
the offered stanza, no alias binding, envelope, `ErrorReply(<conflict/>)`),
`SemanticMalformed` (rejection row + `<bad-request/>`), `AuthorizationDenied`
/ `PolicyDenied` (rejection row + the handler's standard error),
`CaptureOverflow` (rejection row + `<resource-constraint/>`; the cap is sized
so a room at maximum occupancy never overflows). Non-advancing (rolled back):
`PrincipalMissing`, `ClaimFenceMissing`, `RoomGenerationStale`,
`FrontierStale`, `SmOrdinalConflict`, `IntentContradiction`, `Storage`,
`SerializationExhaustion`, `Timeout` (pre-commit), `AmbiguousCommit`,
`Lineage`, `EpochUnsupported`; `RoomGenerationStale` means the local room
fence context no longer matches its durable claim, never admission-revision
or audience drift.

### 3.6 Locks and retention
Alias resolution and sm-ref/delivery insertion lock the canonical row
`FOR UPDATE` (no share→update upgrades on the write path);
`terminalize_message` takes an unconditional `FOR UPDATE`; GC keeps
`FOR UPDATE SKIP LOCKED`. Retention: eight days from `terminal_at`.

### 3.6a Periodic bounded maintenance (#1741)

The retention coordinator runs a maintenance pass at startup, after committed
decisions and on a jittered 30 s tick, continuing with capped exponential
backoff after any `Partial`, `Failed` or `TimedOut` pass. A pass attests the
epoch, then terminalizes receipt-complete non-terminal rows older than a 60 s
grace through a keyset cursor over `(created_at, message_key)` (each row is
locked and its receipt completeness re-checked; contended rows are skipped and
retried on the continuation), then runs retention GC. Each phase is bounded
inside a hard pass deadline; maintenance shares the ingress pool and holds at
most one connection at a time. Metrics: `ingress.maintenance.runs{phase,
outcome}` and `ingress.maintenance.terminalized_messages`; alert
`IngressMaintenanceFailing`.

## 4. Backends
The unit of work is dialect-aware through `Database`: SQLite uses
`BEGIN IMMEDIATE`, no lock clauses, no epoch GUC proof, `IngressFencing::
SingleNode`; Postgres is unchanged. Repositories, reads, GC and admission are
ported; MAM/global database co-location is checked at boot on every backend.
Clustering stays a cargo feature (fences compile only with it).

## 5. Schema (V1012, Postgres + SQLite arms; `ensure_schema` for store-owned
tables)
- `ingress_messages`: `envelope_version SMALLINT NULL`, `envelope BYTEA NULL`,
  `CHECK ((envelope IS NULL) = (envelope_version IS NULL))`.
- `ingress_sm_streams`: `checkpoint_h BIGINT NOT NULL DEFAULT 0`,
  `wire_generation BIGINT NOT NULL DEFAULT 0`.
- `ingress_sm_refs`: `wire_h BIGINT NOT NULL`, `wire_generation BIGINT NOT NULL`,
  `UNIQUE (sm_ingress_id, wire_generation, wire_h)`.
- `ingress_effect_receipts (message_key, kind, semantic_identity_hash,
  applied_at)` with PK = FK → `ingress_effect_intents` `ON DELETE CASCADE`;
  epoch guard triggers, manifest row and `pg_monitor` grant.
- `ingress_carbon_receipts (message_key, kind, semantic_identity_hash, recipient)`
  with a composite primary key and FK to `ingress_effect_intents`
  `ON DELETE CASCADE`; epoch guards, manifest row and `pg_monitor` grant.
- Epoch-0 reset of the soak rows in the runbook lock order (DELETE).
- SQLite arm: real DDL for every ingress table.
- `ensure_schema`: `sm_sessions` drops `shadow_ordinal`; `mam_messages` drops
  the dedupe columns and indexes.

## 6. Deployment
Recreate hard cutover rides in this PR (prod HelmRelease `updateStrategy:
Recreate`, precedent #1596 → flipped back by #1605); all old writers stop
before V1012 runs; old binaries refuse the unknown ledger version.

**V1014 cutover (#1752).** The recovery follow-ups ride a second one-shot
Recreate. V1014 installs an epoch-safe transaction-local proof (valid at live
epoch 0 and 1; no epoch-zero requirement), then resets ingress and SM state in
child-before-parent order: effect receipts, carbon receipts, effect intents,
deliveries, SM refs, origin aliases, invite claims, canonical messages, SM
streams, retained SM sessions and their unacked outbound frames. Consequences:
in-flight ingress obligations at cutover are abandoned (observer runs,
notification candidates, not-yet-inserted pending rows); retained sessions
cannot resume and their unacked outbound frames are discarded; queued
`pending_delivery` rows and archives are unaffected, and pending rows claimed by
a discarded session are released once by the store-owned startup step
(`reset_claims_for_ingress_v1014_once`) so they deliver on the first reconnect.
`groupchat_notification_recovery` rows without a canonical `message_key` are
deleted once by the inbox schema step. Roll-forward only: no pre-V1014 binary
can start after the ledger advances.

## 7. Scaffolding removal
`ingress_shadow` → `ingress`; worker, queue, parking map, candidate ladder,
decision markers, `IngressShadowConfig`/`WADDLE_INGRESS_SHADOW_*`
(replaced by `WADDLE_INGRESS_DB_POOL_SIZE`, `WADDLE_INGRESS_RETRY_ATTEMPTS`),
`HandledFrontierRepository`, the soak runbook, the `waddle-ingress-shadow`
Mimir group, the dashboard row and the prod flag are deleted. Kept under new
names: `ingress.decisions`, `ingress.alias.outcomes`, `ingress.tx.retries`,
`ingress.gc.runs`, `ingress.gc.reclaimed_messages`, `ingress.tx.duration`,
plus new `ingress.effects.unresolved`; a small `waddle-ingress` alert group.
