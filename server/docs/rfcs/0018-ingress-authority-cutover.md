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
roadmap slices): (i) maintenance recovery (#1755, §3.6b) re-executes
provenance-proven direct routes, direct pending delivery and notification
previews, observers with recorded envelopes, delegated groupchat notification
recovery, routes of receipted DM pin mutations, and MUC ledger declines.
Delegated live full-JID routes (including detached full-target no-store routes
without archive evidence), headline routes, unreceipted DM pin mutations and
their routes, carbons and DM call state remain deferred. Remote-owner-only
resources and families lacking reconstructible payloads (including room pin
chains without a recorded pinner nick) remain pending. Keyless live sends and
observer invocations are at-least-once, including after send-before-receipt
failures and against concurrent client retransmission;
(ii) repaired duplicates retry unfinished recorded direct resources (§3.3a)
and non-sender MUC occupant copies (§3.3e), preserving the frozen audience and
payload. Keyed detached delivery uses the same `sm_ingress_appends` ledger
locally and on authorized cross-node receiver appends (#1778), including direct
routes and recorded MUC occupant copies, and on the registered-remote-socket
and local UserActor detach drains (#1789, #1805). The authorization-failure
fallback remains unkeyed and at-least-once;
#1760 custody failures still let proofs outlive payloads and suppress recovery
(§3.3a). Live sends remain at-least-once, and maintenance never relays
remote-hosted resources; (iii) live full-JID delivery keeps the
destination connection's own recipient archive/inbox pipeline (#1658, now tracked as #1759);
(iv) subject/pin/membership supersession keeps `main`'s semantics
(#1659/#1660); (v) non-resumable streams have no durable
connection-generation fence (follow-up issue); (vi) ~~extension-host dispatch runs outside ingress: offline rows and candidates are written immediately without receipts, and groupchat notification recovery rows are not created; a typed Extension ingress identity is the follow-up.~~ Resolved by #1753: typed extension ingress covers direct and local-room bot sends (§3.1).
(vii) archive ordinals do not yet enforce concurrent live dispatch order (#1770, §3.7).

### Recovery convergence (#1782)

Recovery (§3.6b) parks a row for 15 minutes after three attempts that provably
added neither an effect receipt nor a delivery-progress row. At most one attempt
per 60-second sample interval counts toward that streak, so parking follows at
least two minutes of continuous no durable progress. Maintenance also runs at
startup and after every committed decision, and without that interval a burst of
commits would park a row still waiting for its recipient.
Storage errors, elapsed row deadlines, uncertain settlement and failed
accounting reads are inconclusive and reset the streak. Parking writes no
receipts or terminal state: obligations remain pending, GC-protected and
included in the non-terminal backlog gauge.

The row is eligible after the cooldown, attempted on a subsequent scan. The
scheduler's 27–33 s jittered ticks and partial-continuation backoff from 1 s
to 30 s give no upper bound on that attempt. Progress by another replica
changes the row's evidence and re-arms it on the next scan.

Streak and classification caches are per-process and bounded to 4096 entries
with FIFO eviction. A stalled row is classified once per episode as
`ingress_maintenance_unrecoverable_obligations_total{kind=...,reason="no_durable_progress"}`.
Both replicas can classify the same row; eviction or restart permits
re-attempt and re-classification. The counter is a diagnostic, not an exact
queue depth. Parking limits recovery churn; it does not discharge the
remaining obligations or close the recovery gaps listed above.

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
full-JID relay (`deliver_ordered.v11`), with the room's `RoomActor` claim as
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
`IngressStreamIdentity` distinguishes `Resumable`, `Ephemeral { principal }`,
`Relayed { canonical, room, room_fence }` (room fence under clustering), and
`Extension { plugin, requester }`. `IngressPrincipal::Authenticated` asserts
the persisted principal `FOR SHARE`. Resumable adds the SM claim, stream row,
ordinal and checkpoint. Ephemeral has no refs/frontier; registry ownership is
re-checked before Phase C. Relayed (owner side of a relayed groupchat) has no
SM parts and is fenced by the room claim.

`IngressPrincipal::Extension(ExtensionPrincipal { grant, requester, sender })`
asserts a durable `extension_grants` row instead of an authenticated session.
The exact grant id must be active and match the plugin and scope; the identity's
plugin and requester must match the principal. A provider-room grant must match
the target room. When present, the requester's account is asserted in either
`users` (by JID) or `native_users` (by username and domain), covering OIDC and
native SCRAM registration.
PostgreSQL holds the grant and account `FOR SHARE` through commit; SQLite uses
its ingress transaction's write serialization. Missing, revoked or mismatched
grants and deleted requester accounts refuse admission as `principal_missing`.
Existing manifest, roster and room permission checks still run before planning.
Local-room fencing remains driven by the plan's room execution path. Grant
revocation is configuration-driven only: startup `sync_configured` reconciles
the complete plugin/capability/provider-room set. No runtime unload or grant
revocation API exists.

The effective sender is the requester for direct sends and the plugin actor
bare JID for groupchat. Both paths carry a typed XEP-0359 origin-id derived from
the offered stanza id. Alias and digest authorities use this effective sender:
different bots have separate origin namespaces, while a direct extension send
shares its requester's namespace. `TransportGeneration::Host` explicitly has
no socket generation or registry ownership recheck, SM refs or handled frontier.

`NestedIngressOperation` acquires one admission permit before planning without
waiting behind a drain writer. Before the first commit await, it transfers the
submission and continuation to an authority-owned task. That task commits,
executes and settles without reacquiring admission; caller cancellation cannot
cancel committed work, and drain waits for its permit. `Deps.host_sender`
captures sender-directed frames during planning. The continuation consumes those
frames (including bot reflection) as the host transport and settles their
obligations, retrying receipt persistence within a five-second budget without
re-dispatching effects. Copies to other bot occupants on the configured extensions
domain, including copies of real users' groupchat messages, are typed
`HostOwnedCopy` effects completed and receipted without transport I/O because bots
observe through hooks; duplicate admission suppresses these non-sender copies.

The host reports the first typed stanza error when the settlement outcome is
available, even if its receipt persistence failed. A known rejection is
independent of receipt durability; `cancel` errors map to the plugin
`Denied` code, so plugins are not instructed to retry them.
Offline quota refusal is carried by `SettledRefusal::OfflineQuotaExceeded` and
mapped to the existing XEP-0160 `cancel` / `service-unavailable` error; pending
and notification obligations settle before that response, without inserting a
pending row or candidate. The plugin API is unchanged: if the two-second
settlement-response deadline expires after commit, or settlement persistence
fails without a known rejection, the adapter returns acceptance. An enclosing caller timeout instead
produces no response while the authority-owned task continues. Exhausting frame
settlement retries can leave a committed non-terminal row; maintenance cannot
reconstruct frame-only obligations. This is the host-transport form of the
existing at-least-once frame limitation, not an exactly-once delivery guarantee.
A plugin retry after a caller timeout generates a fresh origin and is a new
message; reusing the same origin at the adapter boundary uses alias replay.

`plan_extension_bot_groupchat` captures trusted bot groupchat effects, including
archive identity/ordinal, occupant routes, inbox projections and notification
recovery linked to the canonical `message_key`. It keeps server-authored sender
authority, no sender inbox projection, no enrichment and no observers. Existing
bot occupancy (nickname and session generation) is reused; join and initial
presence only run for an absent bot, as authorized lifecycle work outside message
receipts. A shared in-process guard keyed by plugin and room serializes the
snapshot/join/admission sequence across adapters, preventing concurrent first
sends from joining twice. If Phase B refuses the grant/requester assertion after
this dispatch joined the bot, it compensates that join through the normal room
leave path (including unavailable presence) before releasing the guard and
returning `NotAuthorized`; a reused occupancy is left intact. The digest uses the
offered unsigned envelope before validation and clock-dependent signing; the signed envelope is persisted and sent. Occupant
copies retain XEP-0045, thread, reply, markup and stanza-id semantics, with the
added origin-id. Remote-owned rooms receive the typed
`ExtensionRemoteRoomUnsupported` planning refusal; extension admission still uses
`IngressRelayAdmission`, and the ordered relay now uses `deliver_ordered.v11`.

Non-advancing outcome: Resumable → ordinary hole (`abandon`) → transport ends,
session resumable before the hole. Ephemeral → typed `<stream:error>` then
close (`internal-server-error` for storage/serialization/timeout/ambiguous
commit/lineage/epoch; `not-authorized` for principal loss; `conflict` for fence
or registry loss). Extension → host `NotAuthorized` for principal/grant loss,
`Storage` for authority or other non-advancing failures; typed remote-room plan
refusal maps to `Unsupported`. Committed semantic denials are standard stanza
errors, consumed by the host for Extension identity.

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
- The obligation identity now crosses nodes in `deliver_ordered.v11` and
  `remote_resource_route.v7` (#1778). The typed `IngressAppendObligationRef`
  carries `message_key`, `sender_bare`, the effect receipt key and `received_at`;
  it is covered by the ordered envelope signature and payload fingerprint.
  Both direct routes and recorded MUC groupchat occupant copies carry it.
  Before using it, the receiver checks that `sender_bare` matches the validated
  sender claim and stanza `from`, and that the canonical ingress row for
  `message_key` exists and names that sender. Authorized detached appends on
  the receiving node use the same `sm_ingress_appends` ledger as local appends.
- **Registered remote sockets (#1789):** the owner-to-socket frame
  (`remote_resource_frame.v2`) carries the obligation, queued unverified on the
  socket node's live outbound entry. It is authorized lazily, only when a detach
  drain is about to append it, and the drain reads the ledger *before* counting
  the frame so a duplicate never occupies a sequence the client cannot
  acknowledge. First-drain entries are proven in the session snapshot's
  transaction; later ones commit with their proof individually.
- **UserActor delivery (#1805):** local `TrySendPeer` and `TrySendDirect`
  carry the same typed obligation onto the outbound entry. This covers both
  same-node sockets and owner mirrors, whose forwarder preserves the obligation
  on the existing `remote_resource_frame.v2` envelope. Local delivery retains
  the bounded canonical-sender check at detach; the actor mailbox does not
  confer a separate authorization bypass. A lost progress receipt followed by
  detach and recovery therefore reuses the recorded queue allocation, subject
  to the authorization and custody limits below.
- **Still at-least-once:** a drained entry that loses the ledger race between
  the drain's read and the session store keeps its queue entry and only its proof
  is withheld; entries past a drain's 2 s authorization budget, or after one
  indeterminate canonical read, drain unkeyed.
- **Live writes (#1789):** the handler records a frame into the SM queue before the
  transport write, so the obligation moves onto that recovery-owned entry and the
  detach proves it with the session snapshot, whether the write failed or went
  unacknowledged. After the client acknowledges the entry there is nothing left to
  key; a later re-execution is the lost-receipt duplicate of #1760 direction 2.
- **Still at-least-once:** failed receiver-side authorization degrades to an
  unkeyed append, with a warning and counter. Delivery never fails because
  this check failed; availability does not depend on authorization succeeding.
- Proof and resource progress commit in two transactions: the ledger row
  with the SM snapshot, the progress row afterwards under the canonical lock.
  A retry between them reads `AlreadyAppended`. The #1760 custody limitation
  is unchanged: quarantine deletes a session's queue but retires only
  gap-covered proofs, so a retained entry's proof can outlive its payload and
  turn later recovery into a false `AlreadyAppended`. Cross-node keying extends
  these proof-suppresses-recovery failure modes to remote deliveries; it does
  not provide lifecycle-safe exactly-once delivery.

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

### 3.3e Per-occupant room fan-out progress (#1757)

`RouteMucGroupchat` and `RouteMucSystemBroadcast` share storage kind 2 and the
existing `room|route_identity` semantic key. `ProgressObligation` retains the
verbatim intent evidence separately from the derived `RouteProgress.fanout`;
`RouteProgress::settle_evidence` reconstructs the exact recorded obligation.
Groupchat fanout is the frozen occupant set minus the sender reflection;
system broadcast fanout includes every recorded occupant. An empty fanout
settles inside the acceptance or recovery-freeze transaction under the canonical
lock, without manufacturing delivery progress. The canonical row terminalizes
only when every sibling obligation is receipted; MUC delivery never discharges
an inbox `RouteDirect` intent.

Each completed occupant is recorded in `ingress_delivery_receipts` under the
MUC receipt key and full JID. Local detached copies use that same receipt key
plus the occupant resource in `SmIngressAppendContext`, so append-before-progress
rollback and concurrent retries share one durable queue allocation. Delivery
happens before the progress transaction; progress and the aggregate kind-2
receipt commit together under the canonical row lock. The aggregate is arm-owned,
not generic all-or-nothing fanout evidence.

The sender reflection remains `Always`: every duplicate can resend it, including
a relayed-owner frame. It carries no kind-2 receipt identity, contributes no
occupant progress, and supplies no aggregate proof through frame completion or
`owner_receipts`. Its delivery proof remains the sender's XEP-0198 stream.
Ordinary cross-node occupant copies use `deliver_ordered.v11`; a definite
`Delivered` ACK proves that occupant's copy. The MUC-only `RelayFullJid` executor
arm records progress and preserves the MUC append context when ownership becomes
local before execution or during relay fallback. Declined or uncertain delivery
leaves the occupant pending. Both direct-route and MUC groupchat obligations
carry their append identity through ordered relay and the
`remote_resource_route.v7` full-JID second hop. Receiver-authorized detached
appends and the registered-socket detach drain (#1789) are keyed, subject to the
authorization-failure fallback and unchanged #1760 custody limits in §3.3a.

Phase B freezes the room-canonical groupchat envelope at first owner acceptance,
independently of observer eligibility, retaining observer request context when
present. Recorded MUC authority, including archive-free broadcasts, prevents a
later retry from replacing it. Replay and recovery require `Groupchat`, a
`from` of `room/nick`, the exact recorded room stanza-id, and the XEP-0421
occupant-id (`missing_canonical_provenance` otherwise). Copies personalize only
`to`, preserving the XEP-0045 §7.4 room sender, content and XEP-0359/0421 stamps.
System broadcasts instead carry their exact room-bare-sender message in
`system_message: Option<StoredMessagePayload>` on the recorded intent. The
additive storage field defaults to `None`; those older rows remain pending with
`missing_payload`, never reconstructed from a triggering command. Neither the
kind/semantic key nor the relay wire shape changes; no schema reset is needed.

Ordinary duplicate planning keeps only `(fresh audience ∩ frozen fanout) −
completed`, restoring frozen payloads and room identities even for archive-free
messages. Subject rebroadcast remains a separate path: pending recorded copies
can discharge progress; completed or newly joined occupants receive state
reapplication through generic execution without a MUC append context or widened
historical proof. Subject/pin dependencies still gate delivery.

Maintenance recovers kind-2 obligations from frozen payloads as one
`QueueDetached` per unfinished occupant, without rerunning room MAM or inbox
projections. Subject copies require a receipted `RoomSubjectMutation`; system
broadcasts require receipted room `Pin` intents and the exact correlated
`SystemMessageArchive`. Missing or pending prerequisites yield
`prerequisite_pending`; maintenance does not perform those mutations or archives.
Remote-owned occupants stay pending with `Unavailable`: maintenance never relays.
The existing one-second row and four-second recovery budgets are unchanged.

After relay reachability returns, a client retransmission with the same
origin-id can complete the unfinished remote copy, record its progress, settle
the aggregate receipt and terminalize the row once all obligations are complete.
The two-process regression checks this recovery without assuming that the
failed attempt diverted the occupant-copy channel: a timeout of the fault-control
ask alone does not establish that the separate delivery ask timed out.

An actual diversion has no time-based expiry. A changed room-origin or
target-user ownership epoch selects a fresh channel. Owner refresh during an
in-flight send explicitly forgets the old channel when the target becomes local
or its target epoch changes; it cannot rescue an already-diverted unchanged
channel, which is rejected before sending. A relay lookup miss (`NotFound`)
instead rolls back the sequence without diverting and permits delivery fallback.
Successful ping or resource re-registration alone does not clear a diversion.
The regression's recovered socket delivery and receipts do not identify which
of these paths occurred; they do not prove a same-channel diversion reset.
Maintenance never relays. Ownership is relative to the recovering node: the
destination node's own maintenance can still deliver the globally recorded copy
through its local registry and settle its progress.

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
  recorded direct resources and MUC occupant copies with per-resource progress
  (§3.3a, §3.3e).
- Owner side of a relayed groupchat runs the same pipeline with the
  `Relayed` identity; the proxy envelope carries `IngressCanonicalRef
  { message_key, sender_bare, origin_id }` (relay ask/reply version bumped).
  For MUC fanout, no recorded MUC authority yet → `OwnerFirstAcceptance`
  (full fanout); present → `OwnerDuplicate` (repair, remaining recorded
  occupants, sender reflection and subject rebroadcast,
  `WriteAccepted { stanza_id: recorded }`). Plans without MUC fanout retain
  the room-archive authority check.
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
retried on the continuation), then runs recovery (§3.6b) and retention GC. Each
phase is bounded inside a hard pass deadline; maintenance shares the ingress pool and holds at
most one connection at a time. Metrics: `ingress.maintenance.runs{phase,
outcome}` and `ingress.maintenance.terminalized_messages`; alert
`IngressMaintenanceFailing`.

### 3.6b Maintenance recovery phase (#1755)

Between terminalization and retention GC, recovery freezes the canonical
envelope, recorded and unreceipted intents, and route progress under the
canonical lock; releases the lock; rebuilds a synthetic decision through the
existing restorers; then executes through the unchanged `execute_effects` /
`execute_uow` arms and settlement contract. Groupchat notification recovery
delegates to its existing settlement. No actor call runs under the freeze lock.

Recorded wins: never invent audience or payload. The plan's provenance gate
admits generic direct routes only for `Chat`/`Normal` messages whose bare
`to` equals the recorded recipient, with non-empty fanout, proven Phase B
recipient preparation and no delegated live full-JID route. Pin-owned
`StanzaId` routes, specialized invitations and recorded offline audiences
belong to their restorers. Receipted DM mutations permit route-only recovery;
unreceipted mutations and their routes remain pending. Unsupported families
are metered when evaluated; see the runbook's "What recovery handles" table.

| Intent family | Automatic recovery or reason it stays pending |
| --- | --- |
| `route_muc` (kind 2, including system broadcasts) | Rebuilds unfinished frozen non-sender groupchat copies or system broadcast copies (§3.3e), with canonical provenance and receipted subject/pin/archive prerequisites. Missing payload/provenance and pending prerequisites remain unresolved. Remote-owned occupants are never relayed; groupchat inbox `RouteDirect` siblings remain unsupported. |

Recovery has a 4 s phase budget, a 1 s absolute per-row deadline covering
freeze, execution, delegation and recount, 64-key scan pages and at most 64
attempted rows per pass; the hard pass deadline is 13 s. It is skipped and
unrecorded until the websocket state binds `RecoveryEnvironment`. Receipts
and durably keyed sinks are exactly-once; keyless live sends and observers
retain the at-least-once limitation in (i).

### 3.7 Archive order (#1770 stage 1)

Planning-time timestamps cannot order concurrent senders: a message planned
first can commit after a later message. XEP-0313 §3.1 (in-tree
[`xeps/xep-0313.xml`](../../../xeps/xep-0313.xml), line 301) requires:

> Order within the archive MUST be preserved, where the order of messages is
> the same as the order that the client originally received them (or would
> have received them if online).

The ordering authority is a per-archive commit ordinal, `archive_seq`, allocated
from `mam_archive_sequences` in the insert's transaction by every writer,
including the Phase B transaction writer and the pool writer. The archive's
counter row serializes allocation through commit. Phase B locks every archive
counter the plan will write in canonical archive order before any durable
mutation, so a message and its reply (plan orders differ) queue instead of
deadlocking; a counter wait that exceeds the Phase B lock timeout is the
ordinary non-advancing `Timeout`, never a storage fault. MAM reads, RSM cursors and
newest/first lookups order by the ordinal; `<delay>` keeps the receive timestamp
and time-range filters keep their time semantics. Archive UIDs remain opaque.
Repair re-inserts at the recorded ordinal carried on `ArchiveAuthoritative` and
`SystemMessageArchive` intents, not at the tail. `UNIQUE (room_jid, archive_seq)`
protects positions; the counter is a never-lowered high-water mark, including
after deletion and repair. Timestamp-based `delete_before` is removed: with
receive time independent of archive order, it can delete holes rather than the
prefix required by XEP-0313 retention.

**Stated limitation.** Dispatch enforcement (wire order equals ordinal order
across concurrent senders) is not implemented. An in-process lane is unsound:
A can partially deliver, time out and release the lane; B delivers; retrying A
then delivers its earlier ordinal after B. Waiting inside the connection loop
also creates a backpressure cycle: a predecessor's carbon blocks on that
connection's full outbound channel while the connection waits for the lane,
unable to drain output or process the SM acknowledgements that release its
send window. Stage 2 needs a durable predecessor/release gate that executes
lost predecessor obligations through the #1755 recovery executor, with waiting
off the connection loop. #1755's executor now recovers the recorded families
in §3.6b, but does not close the remote-owner or delegated-live-route gaps.
It must bring every producer outside ingress under
that authority: live full-JID delivery (#1759), extension-host dispatch (#1753),
pending flush, remote-owner bare relay, disconnect drains, and the remote
full-JID detached raw-append bypass that can omit recipient archival entirely.
#1770 remains open; stage 1 supplies durable archive order, not the complete
XEP-0313 live-order guarantee.

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
- Store-owned `ensure_schema` (#1770 stage 1): `mam_messages.archive_seq
  BIGINT NOT NULL`, `UNIQUE (room_jid, archive_seq)` and
  `mam_archive_sequences (archive_jid TEXT PRIMARY KEY, next_seq BIGINT NOT NULL)`;
  legacy rows are backfilled by `(timestamp, id)` within each archive and
  counters are seeded without lowering an existing high-water mark. Ledger
  **V1017** records the same idempotent counter-table DDL as the cutover
  marker so a pre-cutover binary fails closed instead of starting with
  failing inserts.

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

**V1018 extension ingress (#1753).** `extension_grants` and its partial unique
indexes are additive on SQLite and PostgreSQL; no existing ingress, archive,
queue or session state is reset. This change rolls with `RollingUpdate`.
There is an explicit enforcement window: until every pod runs the new binary,
extension sends from old pods still bypass ingress. Those binaries never read
`extension_grants`; their existing path does not corrupt the new grant state,
but it does not honor revocation or create the new ingress receipts. The new
guarantees become global only when rollout completes; record that timestamp.
Use the same complete extension configuration across replicas because each new
process reconciles the configured grant set at startup.

The existing ledger guard applies at startup: a pre-V1018 binary with the
migration ledger guard cannot restart against the advanced ledger. It does not
stop an old process that is already running. Roll forward after V1018; the
runbook's prohibition on rollback to pre-ledger images remains in force.

## 7. Scaffolding removal
`ingress_shadow` → `ingress`; worker, queue, parking map, candidate ladder,
decision markers, `IngressShadowConfig`/`WADDLE_INGRESS_SHADOW_*`
(replaced by `WADDLE_INGRESS_DB_POOL_SIZE`, `WADDLE_INGRESS_RETRY_ATTEMPTS`),
`HandledFrontierRepository`, the soak runbook, the `waddle-ingress-shadow`
Mimir group, the dashboard row and the prod flag are deleted. Kept under new
names: `ingress.decisions`, `ingress.alias.outcomes`, `ingress.tx.retries`,
`ingress.gc.runs`, `ingress.gc.reclaimed_messages`, `ingress.tx.duration`,
plus new `ingress.effects.unresolved`; a small `waddle-ingress` alert group.
