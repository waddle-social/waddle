# #2046 capability matrix: compiled, wired, enabled, tested, executed, live

Package A of [#2046](https://github.com/waddle-social/waddle/issues/2046)
asks for current feature and configuration wiring to be reconciled with
#1661, #1662, #1986 and #1948, "distinguishing compiled, wired, enabled,
tested and live-verified behavior", with a complete matrix and executable
evidence. This is the matrix for every capability in #2046's scope, read
from the source at `origin/main@b31a39dbd` plus the metric call sites of
the change set this record ships in.

[`2046-wiring-reconciliation.md`](2046-wiring-reconciliation.md) is the
earlier record for the #1661/#1662/#1986/#1948 rows at `db6e533`; where the
two disagree about a capability listed here, this one is current. That
record's description of the test lanes still applies.

Nothing was built, run or deployed for this record, and no production
access was used.

## Columns

| Column | Yes means | Evidence |
|---|---|---|
| Compiled | The code is in the production build: `--bin waddle-server --features clustering`, without `test-support` (`flake.nix:392`) | A path |
| Wired | A non-test caller reaches it when its gate is open | The caller |
| Enabled | The production configuration opens the gate | `infrastructure/waddle.cloud/gitops/waddle-server/helmrelease.yaml`, the chart defaults, or a non-test constant |
| Tested | A test exists that exercises it | Binary and test name; `lib` is the `waddle-server` library test crate |
| Executed since last change | A recorded run of those tests after the last commit that changed the capability | See below |
| Live-verified | Observed working in production | Needs production access |

**Executed since last change.** The repository holds no test-run reports,
and hosted CI is paused ([`2046-ci-pause.md`](2046-ci-pause.md)). Every
change merged as #2334 or later was merged without running its tests, so
any capability last changed there is "no". For a capability last changed
before #2334 this record cannot show a run either way from the tree; it
says "not recorded" and names the last change. A "not recorded" is not a
"yes".

**Live-verified is "no" in every row.** No row has an observation from the
running production system.

`S/` is `server/crates/waddle-server/src/`, `W/` is
`S/server/routes/websocket/`, `X/` is `server/crates/waddle-xmpp/src/`.

## Matrix

| # | Capability | Compiled | Wired | Enabled in production | Tested | Executed since last change | Live-verified |
|---|---|---|---|---|---|---|---|
| 1 | Restricted database runtime, one node | Yes: `S/db/runtime_startup.rs`, `S/db/bootstrap.rs` | Yes: `S/main.rs` takes the attested branch when `WADDLE_DB_RUNTIME_ATTESTATION` is set | No: the HelmRelease sets no handoff and connects as the owning login (`containerExtraEnv` `WADDLE_DATABASE_URL` from `postgresql-app`); chart `restrictedRuntime.enabled: false` | `restricted_runtime_startup::restricted_runtime_role_serves_without_ddl`, `restricted_runtime_refuses_instead_of_creating_a_missing_relation`, `runtime_locks_the_epoch_row_but_cannot_change_it` | No: last changed in #2334 | No |
| 2 | Restricted database runtime, clustered | Yes: `S/clustering/node.rs`, `S/clustering/swarm.rs`, `S/sm_persistence_fenced.rs` attach paths | Yes: `S/main.rs` adds the control-plane pool when clustering is enabled; `S/clustering/node.rs` attaches the claim authority under the attested pool | No: as row 1 | `restricted_clustered_startup::two_restricted_nodes_form_a_cluster_and_serve_without_ddl`; `restricted_runtime_startup::clustered::clustered_restricted_startup_refuses_an_unprepared_or_pending_database` | No: `S/clustering/lease.rs` and `S/clustering/swarm.rs` changed in this change set | No |
| 3 | Owner phase (schema preparation and runtime grants) | Yes: `S/server/runtime_owner.rs` | Yes: `S/owner_cli/owner_phase.rs`, reached from `S/main.rs` when the binary is given a subcommand | No: nothing in the chart or GitOps runs it unless `restrictedRuntime.ownerJob.enabled` is set, and the component that sets it is not listed in `kustomization.yaml` | `restricted_runtime_startup::owner_phase_prepares_the_stores_the_runtime_role_attaches`, `owner_phase_is_idempotent_and_fails_closed_on_unclassified_state`, the `clustered::clustered_owner_phase_*` tests | No: last changed in #2334 | No |
| 4 | Owner entrypoint subcommands (`owner-phase`, `install-attestor`, `emit-handoff`) | Yes: `S/owner_cli.rs`, `S/owner_cli/` | Yes: `S/main.rs` dispatches on any argument before listener adoption | No: as row 3 | `restricted_runtime_startup::owner_entrypoint::subcommands_prepare_a_database_the_restricted_server_serves`, `owner_entrypoint::clustered::clustered_owner_phase_reports_pending_activation_and_refusals`, `owner_entrypoint::subcommands_do_not_echo_credentials_under_password_authentication` (skipped without `initdb` on `PATH`) | No: added in #2334 | No |
| 5 | Provisioning procedure for a database one login owns | Not code in the binary: `server/crates/waddle-server/sql/restricted_runtime_provisioning.sql` | No caller: an operator runs it ([runbook](../runbooks/restricted-runtime-cutover.md)) | No | `restricted_runtime_startup::owner_entrypoint::provisioning_refuses_what_it_would_rewrite_and_repeats_without_change`, `owner_entrypoint::clustered::production_shaped_public_schema_is_provisioned_and_served` | No: the procedure was rewritten in #2334 and neither test has passed against the rewritten file | No |
| 6 | Cutover manifests (chart `restrictedRuntime`, owner CronJob, GitOps components) | Not code: `server/charts/waddle-server/templates/owner-job.yaml`, `restricted-handoff-configmap.yaml`, `deployment.yaml`; `infrastructure/waddle.cloud/gitops/waddle-server/restricted-runtime/` | Chart: yes, rendered when its values switch is on. GitOps: no, `kustomization.yaml` lists neither component | No | No: nothing in the repository renders or asserts these templates. `server/scripts/test_chart_shutdown_budget.py` and `test_cutover_guard.py` cover other chart behaviour | No | No |
| 7 | Typed ingress transaction entry | Yes: `S/ingress_substrate/typed_entry.rs` | Yes: the ingress unit of work, the pooled receipt writer, `begin_raw_ingress_writer` and `PostgresIngressSubstrate::begin` call `begin_ingress_entry` | Yes for the unattested authority (production's pool is not attested); the attested authorities are reached only with row 1 | No test targets the entry alone. Every ingress suite passes through it on the unattested authority; `restricted_runtime_startup::alias_gc_collects_an_expired_message_under_the_runtime_role` and `runtime_locks_the_epoch_row_but_cannot_change_it` use the attested one. `server/scripts/check-ingress-typed-entry.py` is a source check, not a test | No: `typed_entry.rs` changed in #2335 | No |
| 8 | Supervised transport pumps | Yes: `W/supervised_io.rs`, `S/websocket_transport.rs` | Yes: the live loop in `W/connection.rs` reads and writes through the pump queues under a `LegacySocketAdmission` | Yes: unconditional on the `/ws` path | `supervised_io`, `websocket_transport`, `socket_admission`, `websocket_admission_live` (whole binaries); `characterization_bind::local_io_witness_tracks_each_production_websocket` | Not recorded: last changed in #2327 | No |
| 9 | Connection actor hosting the live loop | Yes: `W/connection/live_actor.rs`, `W/connection_actor*.rs` | Yes: `W/connection.rs` branches on `LiveConnectionHost` | No: `LiveConnectionHost` defaults to `PlainTask` (`S/server/state.rs`); only the `test-support` variable `WADDLE_TEST_LIVE_CONNECTION_HOST` selects `Actor`, and the production build has no `test-support` | `websocket_admission_live` (actor-host cases), `characterization_bind::*_under_the_actor_host`, `graceful_shutdown_ws::sigterm_closes_live_session_with_system_shutdown_and_exits_under_the_actor_host`, `canonical_actor_bridge`, `prebind_actor_transcript` | Not recorded: last changed in #2327 | No |
| 10 | Clustered frozen room manifests | Yes: `S/ingress/commit_room.rs` | Yes: `new_manifests_enabled` | Yes, in a transaction at protocol epoch 2 or later on clustered PostgreSQL with the mailbox selector (the default); a transaction below that epoch or under the relay selector records none | `ingress_commit::mailbox_acceptance` (written, not yet run) | #1948 |
| 11 | Room owner decision repository (`RoomOwnerDecision`, `ingress_messages.owner_decision`) | Yes: `S/ingress_uow/repositories.rs`, migration in `S/db/migrations/waddle.rs` | Used inside the ingress unit of work only; no caller outside `S/ingress_uow/` names `RoomOwnerDecision` | Not determined here: whether a production path writes a decision depends on row 10's gate | `xep0045_xep0198_remote_acceptance` (two of its contract tests are `#[ignore]`d for #1948); `lib` `ingress_uow` tests | Not recorded: last changed in #2307 | No |
| 12 | Durable mailbox value types | Yes: `X/ingress/mailbox.rs` | No: nothing outside that file uses it | No | 23 unit tests in `X/ingress/mailbox.rs` | Not recorded: added in #2326 | No |
| 13 | SASL2 server handshake (dark) | Yes: `W/sasl.rs`, `W/connection_actor/prebind.rs` | Yes, behind `sasl2_test_capability` | No: `S/server/http.rs:1582` sets `sasl2_test_capability: false`; only test state sets it | `sasl2_dark_ws`; `prebind_actor_transcript` | Not recorded: last changed before #2334 (#2299 for `W/sasl.rs`) | No |
| 14 | SASL2 and inline resume, client side | Yes, in the client crate: `server/crates/waddle-xmpp-client/src/runtime/sm.rs`, `bootstrap.rs` | Yes within the client | Not a server configuration; the server does not advertise SASL2 (row 13) | `waddle-xmpp-client` `tests/xep0388_sasl2.rs`, `tests/xep0388_inline_resume.rs` | Not recorded: last changed in #2320 | No |
| 15 | Ordered relay lanes | Yes: `S/clustering/ordered_relay.rs`, `S/clustering/route_bridge/delivery/ordered*.rs`, relay message `deliver_ordered.v16` | Yes: the route bridge sends through it | Yes: `clustering.enabled: true` and `replicaCount: 2` in the HelmRelease | `clustering_ordered_relay`, `clustering_cluster_e2e`, `xep0045_cross_node_reflection` | Not recorded: `ordered_relay.rs` last changed before #2334 | No |
| 16 | Durable stream management: portable store | Yes: `S/sm_persistence/` | Yes: `S/server/http.rs` opens or attaches it | Yes on a single node; with clustering the fenced store of row 17 is used | `xep0198_replay_bytes`, `xep0198_resume_receipts_ws`, `xep0198_attached_resume_ws`, `graceful_shutdown_ws::sigterm_promotes_live_sm_session_unacked_queue_for_next_startup_delivery` | Not recorded: last changed in #2252 | No |
| 17 | Durable stream management: claim-fenced store | Yes: `S/sm_persistence_fenced.rs` | Yes: `S/server/http.rs` when clustering is enabled | Yes: clustering is enabled in production | `xep0198_cross_node_resume`, `clustering_cluster_e2e::cross_node_resume_live_steal_handshake` | Not recorded: last changed in #2331 | No |
| 18 | Stream management custody and promotion (`sm_custody`, `sm_promotion`) | Yes: `S/sm_custody/`, `S/sm_promotion/` | `sm_promotion`: yes, from `S/server/session_janitors.rs`. `sm_custody`: not determined here | Not determined here for `sm_custody` | `xep0198_promotion_inventory`, `xep0198_pending_delivery_ws`, `sm_revision_participation` | Not recorded: `sm_custody` last changed in #2253 | No |
| 19 | Authority, custody and progress metrics | Yes: `X/telemetry/authority.rs` | Partly: ingress entry, keypair lease, restricted start refusals and attach failures, destroy completions. The pump, actor, relay, stream management and remote-dispatch recorders have no call site ([metrics reference](../operations/server-metrics.md)) | Yes where wired: OTLP export is configured (`telemetry.otlpEndpoint` in the HelmRelease) | Unit tests in `X/telemetry/authority.rs` and `X/telemetry/attributes/authority.rs` | No: added in #2335, extended in this change set | No |
| 20 | Alert rules for row 19 | Not code: `infrastructure/waddle.cloud/rules/staged/mimir/waddle-authority.yaml` | No: the sync uploads `rules/mimir` and `rules/loki` only | No | Fixtures in `rules/staged/tests/authority.test.yaml`; `rules/tests/run.sh` checks the rule file's syntax and does not evaluate the fixtures | No | No |
| 21 | PostgreSQL owner-as-runtime startup (the path rows 1 to 6 replace) | Yes: `S/main.rs`, `bootstrap_shared_owner_postgres` in `S/db/bootstrap.rs` | Yes | Yes: this is how production starts today | `owner_shared_main_pool`, `owner_bootstrap`, and every PostgreSQL suite that starts the server without a handoff | Not recorded | No |

## What the matrix says about #1661, #1662, #1986 and #1948

- **#1661/#1662 (ingress authority and its epoch).** The typed entry is
  wired and serving on the unattested authority (row 7). The attested
  authorities exist and are tested, and nothing in production reaches them
  until the restricted runtime is enabled (rows 1 and 2).
- **#1986 (durable mailbox).** Value types only (row 12). No store, no
  caller, no lane served from it; the ordered relay still carries every
  lane (row 15).
- **#1948 (remote-room acceptance before `h`).** From protocol epoch 2 on,
  a clustered PostgreSQL deployment carries remote room acceptance through
  the durable mailbox and seals clustered room manifests in the owner's
  transaction (`S/ingress/commit_mailbox.rs`, `S/ingress/commit_room.rs`).
  The owner phase activates the epoch. The two-node contract tests are
  written and have not been run
  ([failure matrix](2046-failure-matrix.md), rows 4 and 5).

## Executable evidence

The commands that would turn the "tested" column into "executed", from
`server/`, with a PostgreSQL 17 test server and `clustering` plus
`test-support`:

```sh
export WADDLE_TEST_POSTGRES_URL=<url> WADDLE_TEST_REQUIRE_POSTGRES=1
cargo nextest run --profile ci -p waddle-server --all-features \
  -E 'binary(restricted_runtime_startup) | binary(restricted_clustered_startup)'      # rows 1-5, 7
cargo nextest run --profile ci -p waddle-server --all-features \
  -E 'binary(supervised_io) | binary(websocket_transport) | binary(socket_admission) | binary(websocket_admission_live) | binary(canonical_actor_bridge) | binary(prebind_actor_transcript) | binary(characterization_bind) | binary(graceful_shutdown_ws)'   # rows 8, 9
cargo nextest run --profile ci -p waddle-server --all-features \
  -E 'binary(clustering_ordered_relay) | binary(clustering_cluster_e2e) | binary(xep0045_cross_node_reflection) | binary(xep0198_cross_node_resume) | binary(xep0045_xep0198_remote_acceptance)'   # rows 11, 15, 17
cargo nextest run --profile ci -p waddle-server --all-features \
  -E 'binary(sasl2_dark_ws) | binary(xep0198_replay_bytes) | binary(xep0198_resume_receipts_ws) | binary(xep0198_attached_resume_ws) | binary(xep0198_promotion_inventory) | binary(xep0198_pending_delivery_ws) | binary(sm_revision_participation)'   # rows 13, 16, 18
cargo nextest run --profile ci -p waddle-xmpp-client --tests                       # row 14
cargo nextest run --profile ci -p waddle-xmpp -E 'test(telemetry::) | test(ingress::mailbox::)'   # rows 12, 19
sh ../infrastructure/waddle.cloud/rules/tests/run.sh                                # row 20, syntax only
```

Row 10 and the `lib` part of row 11 need the `waddle-server`
library test crate, which needs about 6 GB to build. Row 6 has no command:
it has no test.

The publish-gating lane lists the tests an image must have executed in
`server/.config/required-cluster-tests.txt`; a test in the "tested" column
that is not in that list can be skipped without failing the lane.
