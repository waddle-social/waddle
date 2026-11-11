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
| M1 | Restricted database runtime | #2116, #1689 | 8 | The server serves under the runtime role; both `UnconvertedStores` refusals and the owner-as-runtime startup path are deleted |
| M2 | Durable mailbox and remote-room acceptance | #1986, #2118, #1948 | 4 | The origin handled count advances only after owner acceptance; clustered frozen manifests are enabled in the production path |
| M3 | Connection, account and SM cutover | #2137, #2138, #2139, package B | 22 | The connection actor is the only `/ws` owner; `WsConnState` writers, `dual_registration` and registry authority are deleted |
| M4 | Relay lane cutovers | #2173, #2174, #2175 | 3 | Each lane is served from the mailbox; its ordered-relay path is deleted and its wire version bumped |
| M5 | SASL2 inline resume and clients | #2140 | 2 | Server, WASM and chat support with XEP-0388/0198 suites; advertisement keeps its own gate |
| M6 | Qualification and release | package G, definition of done | 13 | Workflows restored; failure matrix run on the release SHA; deployment verified |
| M7 | Schema baseline | new | — | One baseline schema per database replaces the migration history (see below) |

### Order

M1 → M2 → M4 is the database track. M3's single-node gaps (listed below) do
not depend on M1 or M2: they move existing behaviour behind the actor under a
test-selected configuration. The two tracks therefore run in parallel and join
at M3's production activation, which needs M1's transaction authority for the
durable SM and account rows and M2's acceptance custody. M5's client work and
the [#2106](https://github.com/waddle-social/waddle/issues/2106) qualification
environment have no upstream dependency and start immediately. M6 and M7 are
last, in that order.

## Proposed decisions

### D1. Cutover is coordinated, not rolling

Each incompatible activation uses the `Recreate` strategy with all server pods
stopped before the new version starts. Package G permits this in place of
mixed-version compatibility. The cutover runbook verifies that no old backend
or prepared transaction remains, and that the owner login is disabled for the
runtime, before the first runtime pod starts.

Consequence: the open items that require continuous in-database fencing of old
writers during a transition (the V1014 claim reset, pending-delivery MAM
cleanup and inbox legacy-row deletion) reduce to that runbook precondition
plus one startup check. Steady-state fences between live nodes are unaffected.

### D2. The restricted runtime serves on the attested, non-pinned pool

`AttestationMode::LegacyDark` already supports all 26 `attest_transaction`
call sites and most store attach paths
(`S/db/bootstrap.rs:224-357`). Pinned mode cannot serve: `attest_transaction`
returns `UnconvertedPinnedTransaction` (`S/db/physical_attestation.rs:251-253`),
and several sites attest after `BEGIN`/`LOCK`, which conflicts with the pinned
lock order (for example `S/clustering/lease.rs:1154-1181`).

Proposal: serve on the attested non-pinned pool and keep the pinned read-only
preflight (`S/db/runtime_startup.rs:353-367`) as a one-shot startup gate.
P2.1 is complete when the runtime role cannot own, alter or create schema
objects, every store attaches and operates under it, and the typed transaction
entry is the only route to a correctness transaction. Hostile-catalog and OID
pin proofs defend against a privileged mutator rather than the runtime role;
under D1 they are a runbook precondition and are not extended further.

Open for the gate: whether the `SECURITY DEFINER` attestor pins its own
`search_path`, and whether an unknown `COMMIT` on the runtime ingress path
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
- `LocalConnectionBindingId` and the process-local `ConnectionGeneration`
  counter are deleted at the M3 activation.

### D4. Publication, compensation and old-writer transfer

- A binding is published non-routable, activated when its durable row commits,
  and only then acknowledged with the bind result or `<resumed/>`. Compensation
  retires the provisional binding by exact `BindingRef`.
- An unknown resume or registration commit stays unresolved under the same
  operation identity. Only a known rejection proceeds to a fresh bind.
- Same-node transfer waits for observed pump termination within a deadline; a
  missed deadline fences the node. Cross-node transfer cannot observe the old
  pump, so the durable session records an exposure ceiling and the old node
  loses write authority with its lease.
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
| | `occupancy_session` | **C** Durable session, because it survives resume; projection in the actor |
| | `carbons_enabled`, `roster_interested`, `blocklist_interested` | **C** Account coordinator; the per-stream copy is deleted |
| | `presence_*`, `pending_subscribes_flushed` | **C** Deleted as copies; the durable session is the resume snapshot source |
| | `local_io_relinquishment`, `write_interruption` | Transport pump |
| | `ingress_generation`, `registry_owner`, `recovered_pending_endpoint`, `resume_endpoint_handoff_blocked`, `ordered_relay_handoff_tx` | Deleted |
| `ConnectionEntry` | `connection_ref`, `binding_ref` | Account coordinator |
| | `endpoint`, `force_detach_*`, `resume_control` | **C** Connection actor: it owns outbound order; the pump receives serialized frames only |
| | `binding_permit`, `local_io_relinquishment`, `write_interruption` | **C** Transport pump; revocation is issued by the connection actor |
| | `carbons_enabled`, `roster_interested`, `blocklist_interested`, `presence_available`, `presence_priority`, `archive_dispatch` | Account coordinator |
| | `offline_flushed`, `pending_subscribes_flushed` | **C** Account coordinator, keyed by binding and carried across resume by the durable session |
| | `sm_stream_id`, `occupancy_session` | **C** Durable session; the account coordinator holds a projection |
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
   replicas. `start_if_enabled` runs schema DDL (`S/clustering/node.rs:1166`);
   durable MUC `open` refuses attested pools
   (`S/muc_durable/implementation.rs:400`); clustered SM refuses
   (`S/server/http.rs:2118-2120`).
4. **Remaining startup writes and DDL (M).** `EnsureSchema`
   (`S/server/lifecycle.rs:180`), extension grant sync (`http.rs:1254`),
   account reconcile (`:1293`), notification ancestry adoption (`:1447`) and
   room observers (`:1615`).
5. **Replace the refusal with the attach (S).** `S/main.rs:94` calls
   `attach_verified_global_runtime` and takes the pool; delete
   `S/server/lifecycle.rs:95-97`.
6. **Deployment (M).** CNPG creates only the owner role
   (`infrastructure/waddle.cloud/gitops/waddle-server/postgresql-cluster.yaml:9-12`)
   and the chart has no owner job, runtime credential or attestation variable.

Exit test: invert
`restricted_startup_runs_pinned_preflight_then_refuses_store_activation`
(`server/crates/waddle-server/tests/physical_attestation/runtime_startup.rs:22`)
and add a PostgreSQL 17 WebSocket session with XEP-0198 under the runtime role.

## M3 gaps: the connection actor owns a single-node `/ws` session

In dependency order. Each step keeps the legacy path selected in production
and is proven by the named suite under a test-selected actor.

| # | Gap | Where | Size | Proof |
| --- | --- | --- | --- | --- |
| 1 | Production composition root: reader, prebind initialisation, `TransportReady`, timers, cleanup index in application state | `S/websocket_transport.rs:109-111`, `W/connection.rs:479-558` | M | `canonical_actor_bridge` |
| 2 | Generic inbound dispatch on the driver with a bounded response continuation | `W/connection_actor_driver_task.rs:597-619,1734` | L | driver tests |
| 3 | Production output writer: SM record, send-window pause, mid-batch acknowledgement | `W/batch_write.rs:196` | L | `physical_continuation`, `batch_write` |
| 4 | Prebind through the shared handler, covering stream open and every SASL mechanism | `W/connection_actor/prebind.rs` | M | `sasl1_identity_ws`, `sasl2_dark_ws` |
| 5 | Bind and binding publication against the actor endpoint | `W/actor_local_endpoint.rs:1020`, `W/registration.rs:448` | L | `characterization_bind` |
| 6 | Peer stanza admission, interpretation and write | `W/connection_actor_driver_task.rs:621` | L | `characterization_routing`, `rfc6121_presence_ws` |
| 7 | SM enable, same-node resume, replay and completion settlement | `W/connection.rs:1920-2034` | L | `xep0198_attached_resume_ws` |
| 8 | Force-detach, live-resume control, deferred inbound, relay handoff, keepalive | `W/connection.rs:677-1000` | M | `rfc7395_keepalive_ws` |
| 9 | Terminal cleanup after join: detach, MUC sweep, terminal drain | `W/cleanup.rs:783-834` | L | `graceful_shutdown_ws` |
| 10 | Drain-guard and panic-fence parity | `W/connection.rs:349-403` | S | owner panic test |
| 11 | Selector in the upgrade handler and admission gate | `W/connection.rs:130,157`, `S/server/state.rs:259` | S | every `*_ws` suite under the selector |

After step 11 the actor hosts the legacy handlers and `WsConnState`. The
field moves in D5, durable SM, exact account bindings and the clustered paths
(remote resource proxy activation, cross-node force-detach, the
`deliver_ordered` version bump) follow, and the production activation waits
for M1 and M2.

## M7: schema baseline

After M6, the migration history is replaced by one baseline schema per
database, so later changes iterate from a single deployment schema.

Scope:

- The ledgered catalog: global V1–V15 (`MIG/global.rs:765-855`) and waddle
  V1001–V1030 (`MIG/waddle.rs:1744-1926`), each with SQLite and PostgreSQL SQL.
- The stores that build schema outside the ledger with their own `ALTER`
  chains: inbox, PubSub, SM persistence, pending delivery, notification outbox
  and activity, spaces metadata, channel links, call teardown outbox, room
  effect outbox and MAM. The baseline is incomplete unless these are folded in.
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
same lineage. The baseline must be proven schema-equivalent to the migrated
result on both drivers before the history is deleted.

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
