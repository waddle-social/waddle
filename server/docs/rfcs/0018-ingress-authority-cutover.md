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
(ii) non-idempotent fan-out to non-senders is suppressed on a repaired
duplicate, as today (#1658); (iii) live full-JID delivery keeps the
destination connection's own recipient archive/inbox pipeline (#1658);
(iv) subject/pin/membership supersession keeps `main`'s semantics
(#1659/#1660); (v) non-resumable streams have no durable
connection-generation fence (follow-up issue).

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
- `ingress_sm_refs.wire_h` + `UNIQUE (sm_ingress_id, wire_h)`: the binding
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
- Wire wrap colliding with a retained binding fails the unique insert →
  `Storage` (non-advancing); refs are deleted at stream retirement.

### 3.3 Durable payload
`ingress_messages.envelope` = the post-transform typed message for the
sender's target (after sanitization, room canonicalization, enrichment),
serialized once at the storage boundary; persisted for accepted and rejected
rows. Per-recipient copies and error replies are reconstructed by a pure
function over `(envelope, intent)` tested without actors, extensions or
policy lookups.

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
  and suppresses non-idempotent fan-out to non-senders.
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
- `ingress_sm_streams`: `checkpoint_h BIGINT NOT NULL DEFAULT 0`.
- `ingress_sm_refs`: `wire_h BIGINT NOT NULL`, `UNIQUE (sm_ingress_id, wire_h)`.
- `ingress_effect_receipts (message_key, kind, semantic_identity_hash,
  applied_at)` with PK = FK → `ingress_effect_intents` `ON DELETE CASCADE`;
  epoch guard triggers, manifest row and `pg_monitor` grant.
- Epoch-0 reset of the soak rows in the runbook lock order (DELETE).
- SQLite arm: real DDL for every ingress table.
- `ensure_schema`: `sm_sessions` drops `shadow_ordinal`; `mam_messages` drops
  the dedupe columns and indexes.

## 6. Deployment
Recreate hard cutover rides in this PR (prod HelmRelease `updateStrategy:
Recreate`, precedent #1596 → flipped back by #1605); all old writers stop
before V1012 runs; old binaries refuse the unknown ledger version.

## 7. Scaffolding removal
`ingress_shadow` → `ingress`; worker, queue, parking map, candidate ladder,
decision markers, `IngressShadowConfig`/`WADDLE_INGRESS_SHADOW_*`
(replaced by `WADDLE_INGRESS_DB_POOL_SIZE`, `WADDLE_INGRESS_RETRY_ATTEMPTS`),
`HandledFrontierRepository`, the soak runbook, the `waddle-ingress-shadow`
Mimir group, the dashboard row and the prod flag are deleted. Kept under new
names: `ingress.decisions`, `ingress.alias.outcomes`, `ingress.tx.retries`,
`ingress.gc.runs`, `ingress.gc.reclaimed_messages`, `ingress.tx.duration`,
plus new `ingress.effects.unresolved`; a small `waddle-ingress` alert group.
