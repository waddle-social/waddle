# #2046 completion contracts and activation order

Status: **proposal for the package A architecture gate; nothing here is
accepted or implemented.** Source references are to `main@3326dc735`. The
findings come from reading source; no build, test, workload or deployment was
run for this record.

[#2046](https://github.com/waddle-social/waddle/issues/2046) has 57 open
acceptance items. This record assigns each to a milestone that ends in an
activation and a deletion, proposes the decisions that package A requires
before implementation, and lists the concrete gaps to the first two
activations.

Abbreviations: `S/` is `server/crates/waddle-server/src/`, `W/` is
`S/server/routes/websocket/`, `REG/` is `server/crates/waddle-xmpp/src/registry/`,
`MIG/` is `S/db/migrations/`.

## Where main stands

| Boundary | State on main | Evidence |
| --- | --- | --- |
| Restricted database runtime | Refused before any pool escapes | `S/main.rs:94`, `S/server/lifecycle.rs:95-97` |
| Connection actor on `/ws` | No selector; the gate always returns the legacy admission. The dark driver admits one XEP-0198 `<r/>` and refuses bind, SM enable/resume, ordinary dispatch and peer delivery | `S/server/state.rs:259-271`, `W/connection_actor/prebind.rs:675-691`, `W/connection_actor_driver_task.rs:597-626` |
| Connection state | The dark core owns a whole `WsConnState` by value and calls the legacy handlers; there is no separate state type | `W/connection_actor_core.rs:34-37` |
| Durable mailbox (P2.2–P2.4) | Absent: `MailboxLane`, `mailbox_entries`, `AcceptanceUow` and `TargetConsumptionUow` have no hits under `server/crates` | grep |
| Clustered frozen room manifests | Off outside tests | `S/ingress/commit/commit_hooks.rs:24-34` |
| Hosted CI | Actions disabled, workflow files removed | [`2046-ci-pause.md`](2046-ci-pause.md) |

## Working rules

1. A change merges only if it selects a replacement, deletes what the
   replacement supersedes, or directly satisfies a named exit criterion below.
2. Each milestone has an independent architecture gate before implementation
   and an adversarial review of the final diff after its activation.
3. The deletion belongs to the same milestone as the activation. A milestone
   with a live replacement and a live predecessor is not finished.
4. The count of open acceptance items on the issue is the progress measure.

## Milestones

Every open item on the issue belongs to exactly one milestone.

| # | Milestone | Issues | Open items | Exit |
| --- | --- | --- | --- | --- |
| M0 | Contracts and baseline | package A | 5 | The decisions below pass the architecture gate; the field-owner table is reviewed; workload budgets are recorded |
| M1 | Restricted database runtime | #2116, #1689 | 8 | On PostgreSQL the server serves under the runtime role, correctness writes go through the typed transaction entry, and both `UnconvertedStores` refusals and the PostgreSQL owner-as-runtime startup path are deleted |
| M2 | Durable mailbox and remote-room acceptance | #1986, #2118, #1948 | 4 | The origin handled count advances only after owner acceptance; clustered frozen manifests are enabled in the production path |
| M3 | Connection, account and SM cutover | #2137, #2138, #2139, package B | 22 | The connection actor is the only `/ws` owner; `WsConnState` writers, `dual_registration` and registry authority are deleted |
| M4 | Relay lane cutovers | #2173, #2174, #2175 | 3 | Each lane is served from the mailbox; its ordered-relay path is deleted and its wire version bumped |
| M5 | SASL2 inline resume and clients | #2140 | 2 | Server, WASM and chat support with XEP-0388/0198 suites; advertisement keeps its own gate |
| M6 | Qualification and release | package G, definition of done | 13 | Workflows restored; failure matrix run on the release SHA; deployment verified |
| M7 | Schema baseline | new | — | One baseline schema per database replaces the migration history (see below) |

### Order

M1 → M2 → M4 is the database track. M3's single-node gaps (listed below) do
not depend on M1 or M2: they move existing behaviour behind the actor under a
test-selected configuration, and their proving suites run on single-node
SQLite. The two tracks therefore run in parallel and join at M3's production
activation, which needs M1's transaction authority for the durable SM and
account rows and M2's acceptance custody.

M3's production activation precedes M4. M3 gates and version-bumps
`deliver_ordered` for the actor endpoint; M4 then retires each lane from that
bumped version.

M5's client work and the
[#2106](https://github.com/waddle-social/waddle/issues/2106) qualification
environment have no upstream dependency and start immediately. M6 and M7 are
last, in that order.

## Proposed decisions

### D1. Cutover is coordinated, not rolling

Each incompatible activation uses the `Recreate` strategy with all server pods
stopped before the new version starts. Package G permits this in place of
mixed-version compatibility.

Stopping pods does not stop their backends: a session blocked on a lock held
by the owner migration can complete its write afterwards, and the code says so
(`S/db/runtime_startup.rs:361-367`, `S/db/bootstrap.rs:221-223`). The chart's
cutover guard inspects pods only. Every activation therefore runs in this
order:

1. Stop the server pods.
2. Rotate the credential of the login the old version used, so it cannot
   reconnect. For M1 that is the owner login; for later activations it is the
   runtime login.
3. Terminate the remaining backends of that login.
4. Verify zero such backends and zero rows in `pg_prepared_xacts`.
5. Run the owner phase and migrations.
6. Start the new version.

The new binary additionally requires an activation value that an old writer
fails on inside its own transaction. Today the only such fence is the protocol
epoch, and it covers the 13 guarded ingress tables
(`S/ingress_substrate/substrate.rs:38-50`). The SM, binding and mailbox tables
that M2 and M3 add are enrolled under the same guard when they are created, so
steps 2–4 are a second line of defence and not the only one.

Consequence: the open items that require continuous in-database fencing of old
writers during a transition (the V1014 claim reset, pending-delivery MAM
cleanup and inbox legacy-row deletion) reduce to this procedure. Steady-state
fences between live nodes are unaffected.

### D2. The restricted runtime serves on the attested, non-pinned pool

`AttestationMode::LegacyDark` already supports all 27 `attest_transaction`
call sites, and the global runtime attach is built on it
(`S/db/bootstrap.rs:224-357`). Pinned mode cannot serve: `attest_transaction`
returns `UnconvertedPinnedTransaction` (`S/db/physical_attestation.rs:251-253`),
and several sites attest after `BEGIN`/`LOCK`, which conflicts with the pinned
lock order (for example `S/clustering/lease.rs:1154-1181`).

Proposal: serve on the attested non-pinned pool and keep the pinned read-only
preflight (`S/db/runtime_startup.rs:353-367`) as a one-shot startup gate.
The preflight checks the epoch relation in the V1009 stage: the runtime holds
`SELECT` plus a column-level `UPDATE (id)`, which the registered guard's
`FOR SHARE` read needs and which cannot change the epoch.
Hostile-catalog and OID pin proofs defend against a privileged mutator rather
than the runtime role; under D1 they are a runbook precondition and are not
extended further.

What this mode does and does not give: each checkout attests login, schema and
lineage (`S/db/physical_attestation.rs:571-597,729-745`). It does not attest
the ledger or epoch manifest per transaction, and the pool still exposes raw
`execute`, `query` and `begin` (`S/db/backend.rs:826,847`). Most of the 27
`attest_transaction` sites are attach-time checks, not serving writes. Role
separation alone is therefore not P2.1.

P2.1 is complete in two parts:

- **Role separation.** The runtime role cannot own, alter or create schema
  objects, and every store attaches and operates under it.
- **Typed authority.** Ingress transactions that attest their connection
  begin in one typed entry on the serving pool,
  `S/ingress_substrate/typed_entry.rs`. It begins the transaction, installs
  the caller's typed wait bounds, takes the epoch relation prelock, runs
  `attest_transaction`, proves the epoch (including the V1009 `FOR SHARE`
  read) and refuses an unsupported epoch. Only the entry constructs the
  prepared-writer witness the ingress repositories take, and the ingress unit
  of work stores the entry's transaction, so it cannot wrap one that skipped
  the entry. `scripts/check-ingress-typed-entry.py` fails the clippy lanes
  when the ingress modules attest a transaction, open one directly on a
  database handle, take the epoch row lock without the entry's witness, or
  take an autocommit connection, outside an allowlist that names each exception. The closed authority in
  `S/db/physical_attestation/typed_authority.rs` remains a fixture and is not
  wired into startup. Every table that M2 and M3 add is reachable only
  through the entry; other existing writers convert when their owning
  milestone touches them, and M6 verifies none remains.

This supersedes packages 3 and 4 of
[`2116-single-node-serving-cutover.md`](2116-single-node-serving-cutover.md)
as M1 scope. Package 3 (typed operation identities) is the typed-authority
part above. Package 4 (the SM source and recipient interlock) needs pump
quiescence, which only the connection actor provides, so it moves to M3.

Open for the gate: whether an unknown `COMMIT` on the runtime ingress path
needs more than the duplicate-effect policy that owner mode has today.

### D3. Connection and binding identity

The merged types are the contract: `ConnectionRef { node_lease,
transport_generation }` identifies one physical transport incarnation and
`BindingRef { connection_ref, attempt_id }` is the install fence
(`server/crates/waddle-xmpp/src/ownership/connection_identity.rs:51-104`).

- The connection actor mints `ConnectionRef` at transport accept and a new
  `BindingRef` for each bind, each successful resume, and a fresh bind after a
  failed resume on the same socket.
- The account coordinator records the exact binding and is its only
  publisher. A full JID resolves to a `BindingRef` before delivery.
- The process-local `ConnectionGeneration` counter is deleted at the M3
  activation.
- `LocalConnectionBindingId` is deleted only after remote-hosted mirrors carry
  a `BindingRef`. Today a mirror's admission holds `binding_ref: None` and is
  fenced by the local ID alone (`REG/connection_registry/endpoint.rs:218-231`),
  and the v4 registration snapshot carries only `ConnectionRef`
  ([`2046-binding-ref-identity.md`](2046-binding-ref-identity.md)). M3
  therefore includes a registration-snapshot version bump and the production
  use of admission activation, which is test-only today (`endpoint.rs:233`).

### D4. Publication, compensation and old-writer transfer

- A binding is published non-routable, activated when its durable row commits,
  and only then acknowledged with the bind result or `<resumed/>`. Compensation
  retires the provisional binding by exact `BindingRef`.
- An unknown resume or registration commit stays unresolved under the same
  operation identity. Only a known rejection proceeds to a fresh bind.
- Same-node transfer waits for observed pump termination within a deadline. A
  missed deadline takes the terminal failure path (`S/clustering/node.rs:437,505`),
  not the recoverable fence (`node.rs:433`): a recoverable fence would re-admit
  connections while the unobserved pump could still write.
- The wait belongs to the connection actor's retirement, never to the account
  coordinator's mailbox turn. The coordinator records the pending replacement
  and returns; the retiring actor reports completion as a message. The retiring
  actor takes its detach snapshot from its own projection (D5), so it does not
  call back into a coordinator that is waiting for it.
- Cross-node transfer cannot observe the old pump, so the durable session
  records an exposure ceiling and the old node loses write authority with its
  lease.
- Node lease, user claim, binding, resource, SM session and operation outcome
  commit in one transaction through M1's typed entry.

### D5. Field owners

One owner per field after M3. Rows marked **C** had two plausible owners; the
proposed owner is stated.

| Struct | Fields | Owner after M3 |
| --- | --- | --- |
| `WsConnState` | `connection_ref`, `binding_ref` | Connection actor |
| | `phase`, `stream_open_*`, `authenticated_session`, `state_machine`, `inbound_frame_terminal`, `deferred_inbound`, `keepalive_config`, `send_window_*`, `socket_lifecycle`, `live_resume_control` | Connection actor |
| | `sm_inbound_completion`, `pending_replay_completions`, `suppress_sm_record_next_batch`, `pending_resume_*`, `pending_finalized_resume_outcome` | Connection actor |
| | `sm_state` | **C** Durable SM owns counters and unacked custody; the actor holds a projection hydrated under the session claim |
| | `sm_ingress_fence`, `pending_sm_enable_commit` | Durable SM; the actor holds the handle |
| | `sm_recovery_required`, `terminal_sm_recovery*` | **C** Deleted once append-before-accept is durable; durable SM until then |
| | `occupancy_session` | **C** The occupancy authority (`xmpp_occupancy_authority`), which also covers binds without SM and the remint after a failed resume; projection in the actor |
| | `carbons_enabled`, `roster_interested`, `blocklist_interested` | **C** Account coordinator is the authority; the actor keeps a read-only projection for its detach snapshot |
| | `presence_*`, `pending_subscribes_flushed` | **C** Account coordinator is the authority; the actor keeps a read-only projection, and the durable session is the resume snapshot source |
| | `local_io_relinquishment`, `write_interruption` | Transport pump |
| | `ingress_generation`, `registry_owner`, `recovered_pending_endpoint`, `resume_endpoint_handoff_blocked`, `ordered_relay_handoff_tx` | Deleted |
| `ConnectionEntry` | `connection_ref`, `binding_ref` | Account coordinator |
| | `endpoint`, `force_detach_*`, `resume_control` | **C** Connection actor: it owns outbound order; the pump receives serialized frames only |
| | `binding_permit`, `local_io_relinquishment`, `write_interruption` | **C** Transport pump; revocation is issued by the connection actor |
| | `carbons_enabled`, `roster_interested`, `blocklist_interested`, `presence_available`, `presence_priority`, `archive_dispatch` | Account coordinator |
| | `offline_flushed`, `pending_subscribes_flushed` | **C** Account coordinator, keyed by SM session where one exists and by binding otherwise. A resume mints a new binding, so a binding key alone would repeat the flush; `DetachedSession` does not carry `offline_flushed` today and must |
| | `sm_stream_id`, `occupancy_session` | **C** Durable session and occupancy authority respectively; the account coordinator holds projections |
| | `binding_id`, `hosting`, `terminal_ordering_retry` | Deleted |
| `ConnectionRegistry` | `connections`, `bind_slots` | Account coordinator |
| | `sm_stream_owners` | Durable SM |
| | `presence_states`, `last_activity`, `pending_subscription_stanzas` | **C** Account coordinator, process-local; durability stays with #1663 |
| `UserRegistryActor` | `users[].resources`, `pending_unregisters` | Deleted |
| `dual_registration` | `mirror_register_outcome`, `mirror_unregister` | Deleted |

[`2046-caller-inventory.md`](2046-caller-inventory.md) sections 2 and 3 have
drifted from source: `sender` is now `endpoint`, the ownership token is
`LocalConnectionBindingId`, six `WsConnState` fields are missing, and the line
numbers are stale. This table is the reference for field ownership; the
inventory remains the reference for call sites.

## M1 gaps: serve under the runtime role

In dependency order. Sizes are estimates from reading source.

1. **Owner entrypoint and grant matrix (L).** One command that creates roles,
   runs migrations and each store's owner preparation, installs the attestor
   and applies the runtime grants. Today the only non-fixture grants are the
   attestor installer, the owner-rejection grant and the lease tables
   (`S/db/physical_attestation/installer.rs:643-644`,
   `MIG/owner/owner_rejection_grants.rs:280`, `S/clustering/lease.rs:684`).
   Auth, sessions, roster, blocking and uploads have no grants or verifier
   (`S/db/physical_attestation/coverage_gap_gate.rs:117-131`).
2. **Six owner constructors at startup (M).** Room effect outbox, push
   registrations, VAPID, push service, notification outbox and notification
   activity each have an attach path, but startup calls the owner constructor
   (`S/server/http.rs:1079,1319,1329,1357,1439,1457`).
   `verify_attested_push_notification_stores` already composes five of them
   and discards the handles (`S/server/push_notification_runtime.rs:70-120`).
3. **Clustering under the runtime role (L).** Production runs two clustered
   replicas. The owner phase prepares and adopts the claim, allowlist,
   keypair-pool and durable room relations
   (`S/server/runtime_owner.rs`, `prepare_clustering`), and each clustering
   store has a verifying runtime attachment
   (`PostgresClaimStore::attach_runtime`,
   `PostgresKeypairSlotLease::attach_existing`,
   `PostgresMucRoomStore::attach_runtime`,
   `PostgresFencedSmPersistence::attach_runtime`). Startup does not select
   them: `start_if_enabled` runs schema DDL (`S/clustering/node.rs`), and
   clustered SM refuses under the attested pool (`S/server/http.rs`,
   `AttestedClusteredSmUnsupported`).
4. **Remaining startup writes and DDL (M).** `EnsureSchema`
   (`S/server/lifecycle.rs:180`), extension grant sync (`http.rs:1254`),
   account reconcile (`:1293`), notification ancestry adoption (`:1447`) and
   room observers (`:1615`).
5. **Request-time DDL and per-store URLs (S).** Webhook delivery creates its
   table lazily on first request (`S/server/routes/webhook_delivery.rs:112`,
   `livekit_webhook.rs:61`), which a WebSocket exit test does not reach. Seven
   child `*_database_url` settings (`S/server/config.rs:112-146`) are refused
   under attestation (`S/server/http.rs:2085`) and are removed.
6. **Typed authority (L).** The ingress unit of work and the pooled receipt
   writer with its post-commit readback begin through `begin_ingress_entry`
   (`S/ingress_substrate/typed_entry.rs`), which holds the two per-transaction
   `attest_transaction` calls (installed guard and relation guard). On an
   attested pool the raw ingress writers do too: `begin_raw_ingress_writer`
   (`S/ingress_uow/effect_descendants.rs`), used by the notification and push
   outboxes, and `PostgresIngressSubstrate::begin`
   (`S/ingress_substrate/substrate.rs`). The pooled receipt writer's other
   branch runs only on a handle without attestation. The alias GC's two
   writing transactions (`gc_candidate_batch` and `gc_orphan_proof_batch` in
   `S/ingress_substrate/substrate.rs`) also begin through the entry on an
   attested pool, under the GC's own lock and statement budgets. Every
   remaining direct attestation, direct begin, epoch lock and autocommit
   connection in the ingress modules is allowlisted with its reason
   (`server/scripts/ingress-typed-entry-allowlist.txt`); none of them writes
   an epoch-guarded relation on an attested pool. Still open: the entry does
   not attest the ledger or epoch manifest per transaction; the pool still
   exposes raw `execute`, `query` and `begin`; attach-time checks outside the
   ingress modules call `attest_transaction` directly; the `*_raw`
   descendant repositories in `S/ingress_uow/effect_descendants.rs` accept
   any transaction, and their callers in the notification and push outboxes
   are outside what the source check scans; and the clustered stores have no
   entry. See D2.
7. **Replace the refusal with the attach (S).** `S/main.rs:94` calls
   `attach_verified_global_runtime_with_control_plane`, which clustering
   needs, and takes the pool; delete `S/server/lifecycle.rs:95-97`.
8. **Deployment (M).** CNPG creates only the owner role
   (`infrastructure/waddle.cloud/gitops/waddle-server/postgresql-cluster.yaml:9-12`)
   and the chart has no owner job, runtime credential or attestation variable.

Exit test:
`restricted_startup_runs_pinned_preflight_then_refuses_unprepared_stores`
(`server/crates/waddle-server/tests/physical_attestation/runtime_startup.rs`)
covers the refusal of a database no owner phase prepared, and
`restricted_runtime_role_serves_without_ddl`
(`server/crates/waddle-server/tests/restricted_runtime_startup.rs`) runs a
PostgreSQL 17 WebSocket session under the runtime role; a session with
XEP-0198 under that role is still to be added.

SQLite keeps its single-role startup (`S/main.rs:96-99`). It has no role
model, it is the backend for development and for the `*_ws` suites that prove
M3, and it is not a production configuration. Only the PostgreSQL
owner-as-runtime path is deleted.

## M3 gaps: the connection actor owns a single-node `/ws` session

### The live loop runs on the supervised pumps

`LiveWebSocketConnection::run` starts the socket through
`LegacySocketAdmission::start_socket` (`S/websocket_transport.rs`). The owner
task reads through a demand-gated `WebSocketInput`, one frame per grant, and
writes through `WebSocketMessageSink`, whose flush completes when the write
receipt is terminal. The custodian observes pump termination and releases the
connection slot. `InterruptibleSink` wraps the message sink, so write
interruption and live-resume control end a pending write as before.

### The actor hosts the live loop

The dark connection actor already owns a whole `WsConnState` and calls the
legacy handlers (`W/connection_actor_core.rs`), but its driver admits only
`<r/>` and receives frames pushed into its mailbox by a forwarder. The handlers
need to pull: the mid-batch acknowledgement drain settles the reader and takes
a ready frame while a batch is being written (`W/batch_write.rs`,
`drain_ready_inbound`). Growing the dark driver to parity would rewrite the
batch writer and converge on the actor hosting the legacy handlers anyway.

The live loop therefore moves into the actor as a unit. Control does not need a
mailbox turn while a handler awaits a write: writes race the binding permit and
shutdown tokens, and the sink polls write interruption and live-resume control
on every poll. The reverse does not hold for the mailbox: Kameo handles a
message between two turns of the loop, with no control source, socket or timer
polled meanwhile, so a message handler on the connection actor must not await
unbounded work; such work runs as a turn of the loop instead.

| # | Slice | Deletes | Proof | Size | Gate |
| --- | --- | --- | --- | --- | --- |
| S1 | The loop is expressed as prepare turn, wait for one event, handle the event, settle (`LiveSocketState`, `LoopEvent`) | Inline arm bodies | Every `*_ws`, characterization and WebSocket unit suite | M | Behaviour-neutral |
| S2 | A Kameo actor hosts S1 by overriding `Actor::next`, with an explicit 256-entry mailbox that carries commands and lifecycle only; inbound stays pulled and outbound stays in the endpoint queue so the send-window pause can gate it; a kill is treated as a panic | Nothing | Every `*_ws` suite under the selector, `websocket_admission_live`, a saturation test for control progress | M | Selector |
| S3 | The actor is the default; the loop driver and the dark driver are removed | Legacy driver, `connection_actor_core.rs`, `connection_actor_driver_task.rs`, the fixture forwarder | All suites | M | Flip |
| S4 | One `retire(cause)`: typed cause, idempotent, fenced on `ConnectionRef`; a same-node transfer is acknowledged only after observed pump termination within a deadline, else the terminal path (D4) | Scattered close paths | `websocket_admission_live`, `clustering_shutdown_deadline`, `supervised_io`, `xep0198_cross_node_resume` | M | Selector |
| S5 | Actor-owned endpoint: the actor mints `BindingRef`; prepare, install, confirm and retire are mailbox messages; sink receipts map to typed attempt evidence; pump queues get real byte bounds | `pending_tx`, `registry_owner`, `recovered_pending_endpoint`, `resume_endpoint_handoff_blocked`, `ingress_generation` | `characterization_bind`, `characterization_routing`, `xep0198_attached_resume_ws`, `xep0198_replay_bytes`, `physical_continuation` | L | Selector, then flip |
| S6 | The D5 rows owned by the connection actor move out of `WsConnState` | `LiveSocketState` scaffolding | All suites | M | Behaviour-neutral |

Open before S4: the join deadline and its effect on force-detach
acknowledgement latency (`TRANSPORT_JOIN_TIMEOUT` is 10 s). Open before S5:
`TRANSPORT_QUEUE_BYTES` is `u32::MAX`, which is no byte bound; the
ordered-relay handoff receiver is unbounded; and how a terminal write receipt
maps to an uncertain attempt without stream management.

The regression for publication order
(`server/crates/waddle-server/tests/xep0198_attached_resume_ws.rs`) is ignored
until the remote enqueue paths are gated; that gating is part of S5.

After S6 the actor hosts the legacy handlers and `WsConnState`. What
follows, before the production activation:

- the field moves in D5, durable SM and exact account bindings;
- the SM source and recipient interlock moved here from #2116 (D2);
- the registration-snapshot version bump that carries `BindingRef` to remote
  mirrors, and production admission activation (D3);
- remote resource proxy activation, cross-node force-detach and the
  `deliver_ordered` version bump;
- enrolment of the new SM and binding tables under the activation guard (D1).

The production activation waits for M1 and M2.

## M7: schema baseline

After M6, the migration history is replaced by one baseline schema per
database, so later changes iterate from a single deployment schema.

Scope:

- The ledgered catalog: global V1–V15 (`MIG/global.rs:765-855`) and waddle
  V1001–V1030 (`MIG/waddle.rs:1744-1926`), each with SQLite and PostgreSQL SQL.
- The stores that build schema outside the ledger with their own `ALTER`
  chains: inbox, PubSub, SM persistence (portable and fenced), pending
  delivery, notification outbox and activity, push service, push registrations
  and VAPID, spaces metadata, channel links, call teardown outbox, room effect
  outbox, MUC destroy outbox, durable MUC, room observation, clustering claims,
  nodes, key slots and allowlist, and MAM. The baseline is incomplete unless
  these are folded in.
- "Per database" means the server's PostgreSQL database and its SQLite
  equivalent. M1 removes the per-store database URLs, so there is one. Colony,
  the website and SpiceDB keep their own schema tooling and are out of scope.
- Roles and grants from M1, so a fresh deployment is provisioned by the
  baseline alone.
- Whatever M1–M5 registered from the candidate SQL under `MIG/owner/`; unused
  candidates and laboratory schemas are deleted.

What it has to change: the ledger validation that rejects unknown versions,
checksum changes and schema without a ledger (`MIG/runner.rs:534-665`), the
version-specific branches (`runner.rs:195`, `MIG/owner/ledger_preflight.rs`),
the restricted-ingress manifest that pins the catalog source hashes and byte
offsets (`S/db/physical_attestation/restricted_ingress/manifest.rs`), and
roughly 85 runner tests plus the tests that name versions.

Hazards: every existing database fails validation against the new catalog, so
the cutover is either a fresh database or an explicit ledger rewrite under the
same lineage. A fresh database has a new system identifier and new OIDs, so
the attestation document, its pins and the attestor are reissued with it. The
baseline must be proven schema-equivalent to the migrated result on both
drivers before the history is deleted.

Open for the gate: authoring the baseline before M1 would let the role and
grant matrix land once rather than as forward migrations that M7 then folds
in, and would remove the legacy-row transitions from M1's scope. This record
keeps the baseline last.

## Not established here

- Writer sites were taken from the existing inventory and not re-traced at
  this commit.
- No store's actual DML needs were compared with the fixture grants.
- Janitors and schedulers were not checked under the runtime role.
- The M3 proof suites are matched by name; their contents were not read.
- Workload, latency and memory budgets need the #2106 environment and are not
  recorded yet.
- The mailbox design for M2 and M4, CNPG role management and credential
  rotation were not examined.
