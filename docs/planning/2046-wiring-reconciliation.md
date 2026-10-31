# #2046 A2: feature and config wiring against #1661, #1662, #1986 and #1948

Status: reconciliation record. Written 2026-10-30 against `main@db6e533`.
Nothing here was built, run or deployed, and nobody writing it had production
access.

[#2046](https://github.com/waddle-social/waddle/issues/2046) package A asks for
current feature and config wiring to be reconciled with #1661, #1662, #1986 and
#1948, "distinguishing compiled, wired, enabled, tested and live-verified
behavior". This is that table. It is evidence for #2046 package A2; it does
not authorize any activation.

## Reading the table

Each row carries five evidence levels. They prove different things.

| Level | Meaning here | Evidence used |
| --- | --- | --- |
| compiled | The code is in a build that ships. | A path in the tree and the build flags that include it. |
| wired | A production caller reaches it when its gate is open. | A caller path in non-test code. |
| enabled | The production configuration opens the gate. | `helmrelease.yaml`, chart defaults, or a non-test constant. |
| tested | A named test lane exercises it. The lane is stated. | The workflow or flake target that runs it. |
| live-verified | Observed working in the running production system. | Needs production access. |

**live-verified is `unknown (no production access)` in every row.** Merged
code, a green CI lane and a runbook do not establish it, and
[`TODO-ACTOR.md`](../../TODO-ACTOR.md) records the same rule ("a merged PR
proves implementation landed; it does not prove a deployment or an operational
check succeeded"). Where a row says "inferred", the inference rests on code and
config, not on an observation of the live database or fleet.

### Test lanes

"Tested" names one of these lanes, because they differ in what they build and
whether they gate the published image.

| Lane | What it builds and runs | Postgres | Gates publish |
| --- | --- | --- | --- |
| **L-publish** | cuenv `test` task: `cargo nextest run --workspace --all-targets --locked --profile ci`, default features (`namespace.cue:46-52`). Runs on push to `main` (`waddle-server-default.yml`, a `needs:` of `publishContainerImage`, `:499-509`) and on PRs (`waddle-server-namespacerustchecks.yml`). | No (no `WADDLE_TEST_POSTGRES_URL`) | Yes |
| **L-pr** | `waddle-server-test-shard-1..4`, all-features nextest archive (`flake.nix:143`, `:259-330`), run by `waddle-server-rusttests.yml`, whose only trigger is `pull_request`. | Yes (`serverPostgresTestArgs.preCheck`, `flake.nix:603-631`) | No, and it does not run on push to `main` |
| **L-xmpp** | `waddle-server-xmpp-server-tests`, `--features test-support` without `clustering` (`flake.nix:816-845`), `waddle-server-xmppservercompliance.yml` on push to `main` and PRs. | Yes | No (not in the `needs:` list above) |

Consequence: no lane that runs on push to `main` or gates publish compiles
`clustering` and has Postgres. Cluster tests (5 files under `server/crates/waddle-server/tests/` carry
`#![cfg(feature = "clustering")]`; 8 more gate individual items) therefore
only run in L-pr, and several return success when `WADDLE_TEST_POSTGRES_URL` is unset
(`clustering_cluster_e2e.rs:665-668` prints "skipping" and returns;
`xep0198_cross_node_resume.rs:58` uses `.ok()?`). A pass in L-pr does not prove
the Postgres precondition was present. This is the gap W1.1 addresses.

## A. Deployed clustering baseline

| # | Feature or config | Compiled | Wired | Enabled in production | Tested | Live-verified |
| --- | --- | --- | --- | --- | --- | --- |
| A1 | Cargo feature `clustering = ["kameo/remote", "dep:libp2p"]` (`waddle-server/Cargo.toml`) | Yes in the image: `--features clustering` at `flake.nix:367` (the package build; `:673` and `:759` are the CI-only deps and `waddle-server-ci-build` targets). Off in a default `cargo` build, so L-publish does not compile it. | n/a (build switch) | Yes (the image carries it) | Compiled and unit-tested in L-pr only. | unknown (no production access) |
| A2 | Runtime switch `WADDLE_CLUSTERING_ENABLED` (`config.rs:579`) rendered from `.Values.clustering.enabled` (`charts/waddle-server/templates/configmap.yaml:25`) | Yes | Yes | Yes: `replicaCount: 2`, `clustering.enabled: true`, `enrolledKeyCount: 4` (`infrastructure/waddle.cloud/gitops/waddle-server/helmrelease.yaml`). Chart defaults are `replicaCount: 1`, `clustering.enabled: false`. | Config parsing in L-publish (`config.rs:1754,1774`). | unknown (no production access) |
| A3 | Postgres ownership claims, node leases, self-fence (ADR-0017 Phase 3): `PostgresClaimStore` (`clustering/claims.rs:267`), `clustering/lease.rs`, `clustering/self_fence.rs` | Yes | Yes, when clustering is enabled | Yes (via A2) | Unit tests in L-pr. Two-process e2e (`clustering_cluster_e2e.rs`) in L-pr only, and it skips silently without Postgres. | unknown (no production access) |
| A4 | Ordered relay `deliver_ordered.v13` and the route bridge (`clustering/relay/implementation.rs:485`, `clustering/route_bridge/`) | Yes | Yes: 37 direct bridge call sites in 18 handler/interpret files plus about 12 inline claim-owner branches (counts from the routing audit, not re-derived here) | Yes (via A2) | `clustering_ordered_relay.rs` and the e2e harness, L-pr only. | unknown (no production access) |
| A5 | Cross-node SM resume (`steal_for_resume`, `ResumeIdentityProof`; `waddle-xmpp/src/ownership/claims.rs:540`) | Yes | Yes | Yes (via A2) | `xep0198_cross_node_resume.rs` (needs `clustering` and Postgres; silent skip), L-pr only. | unknown (no production access) |
| A6 | `muc_admin_query.v1` relay message (#1298) and `retire_after_shutdown` on graceful drain (#1295), merged in #1985 (`fe547218`) | Yes (`relay/implementation.rs:1237`, `clustering/self_fence.rs`) | Yes | Yes (via A2); a new message ID, so no Recreate was needed | Tests exist in `clustering/relay/tests.rs`, `self_fence.rs` and `websocket/tests/muc.rs::remote_owner_admin_list` (all `clustering`-gated: L-pr). | unknown (no production access). The post-deploy checks named in `TODO-ACTOR.md` are not recorded. |
| A7 | Remote-resource route/frame dual-version shim (#1804): live endpoint first, fallback to frozen `remote_resource_route.v8`/`frame.v3` on `UnknownMessage` | Yes (`relay/remote_resource_compat.rs`) | Yes | Yes | Unit tests beside it. | unknown (no production access) |
| A8 | Cutover guard (`cutoverGuard.enabled`) | Yes (chart templates) | Yes | Yes in production, `false` in the chart default | `server/scripts/test_cutover_*.py` in the `renderDeployment` CI task | unknown (no production access) |
| A9 | Rollout strategy | Chart default `Recreate` (`values.yaml:46-47`) | n/a | Production is `RollingUpdate`, `maxSurge: 1`, `maxUnavailable: 0` (restored by #1979, `e3a08248`) | Guard scripts, as A8 | unknown (no production access) |
| A10 | `/health` liveness handler (`server/health.rs:114-151`) | Yes | Yes: chart liveness probe `/health` | Yes (production does not override probes) | none found for outage behaviour | unknown (no production access). The handler runs `Database::health_check` on the main pool, so a database outage fails liveness; whether that restarts pods is inference. W1.2 addresses it. |

## B. Ingress authority, epoch guards and manifests (the P1 base that #1661 and #1948 build on)

| # | Feature or config | Compiled | Wired | Enabled in production | Tested | Live-verified |
| --- | --- | --- | --- | --- | --- | --- |
| B1 | `IngressUnitOfWork` (`ingress_uow/mod.rs:87`), `open_ingress_authority` (`server/http.rs:653-679`) | Yes | Yes. It is `Clone` and coexists with the control-plane pool and per-store MAM/inbox pools. | Yes. Fencing is `Clustered` when clustering is enabled, else `SingleNode` (`ingress/contract.rs:148-172`). | L-pr (Postgres) | unknown (no production access) |
| B2 | Epoch guards (V1009 and later enrolments), 12 tables (`ingress_substrate/substrate.rs:49-62`) | Yes | Triggers installed by migrations | **Inert**: the singleton is seeded at epoch 0 (`db/migrations/waddle.rs:251-258`) and the guard returns early at 0 (`:332`). `supported_protocol_epoch()` is 1. No code flips the epoch. | Catalog test (`ingress_substrate/tests.rs:2152-2184`) and `tests/ingress_schema_epoch0.rs`, both need Postgres: L-pr and L-xmpp. | unknown (no production access). That production is still at epoch 0 is inferred from the seed, the absence of any flip, and the open #1689. |
| B3 | #1689 owner/runtime role split | No | No | No. The CNPG cluster has `owner: waddle` (`postgresql-cluster.yaml`), the same role the application uses. | n/a | unknown (no production access) |
| B4 | Single-node frozen MUC manifests before `h` (#1945) | Yes | Yes | Only for `IngressFencing::SingleNode` (`ingress/commit_room.rs:13-21`); production is clustered | Local-room tests | unknown (no production access) |
| B5 | Clustered frozen room manifests: `allow_clustered_room_manifests()` (`ingress/commit/commit_hooks.rs:23-35`) | Yes | Yes, through `new_manifests_enabled` (`ingress/commit_room.rs`) | **No**: returns `false` under `cfg(not(test))`. There is no config or environment switch. Already-sealed manifests stay authoritative. | Test-only enablement via `ALLOW_CLUSTERED_ROOM_MANIFESTS.scope(true, ...)` (`ingress/xep0045_room_manifest_tests.rs:149`). Test-scoped enablement does not establish production correctness. | n/a (disabled) |
| B6 | Epoch 0 to 1 activation, cluster admission enablement | n/a | n/a | No. `TODO-ACTOR.md` lists both unchecked. "Cluster admission" has no code toggle: `admission_gate` (`server/http.rs:1374`) is the `NodeLifecycle` admit gate on app routes, not an activation flag. | n/a | n/a |

## C. #1661 (P2 durable inter-node transport), #1986 and #1948

| # | Feature or config | Compiled | Wired | Enabled in production | Tested | Live-verified |
| --- | --- | --- | --- | --- | --- | --- |
| C1 | P2.1 main-pool transaction authority | **Partial.** The pieces from P0/P1 exist (B1). The target is a non-Clone, non-static typed service with attestation at physical connect and no split correctness pools; `IngressUnitOfWork` is `Clone`, a control-plane pool remains (`db/database.rs:82-101`), MAM/inbox open their own pools, and attestation is per transaction, not at connect. There is no P2.1 slice issue. | n/a | n/a | n/a | n/a |
| C2 | P2.2 durable mailbox core (#1986) | **No.** Scope document only (`docs/planning/p2-2-durable-mailbox-core.md`, "Nothing here is implemented"). `MailboxLane`, `mailbox_entries` and `durable_mailbox` have no hits under `server/`. There is no mailbox or lane table. | n/a | n/a | n/a | n/a |
| C3 | P2.3 acceptance/consumption UoWs, zero-payload hints, reconciler | **No.** `AcceptanceUow` and `TargetConsumptionUow` have no hits under `server/`. | n/a | n/a | n/a | n/a |
| C4 | P2.4 bounded lane execution, cluster admission | **No** | n/a | n/a | n/a | n/a |
| C5 | Lane-by-lane relay retirement | No. The ordered relay (A4) is still the authoritative transport and carries real effects. | n/a | n/a | n/a | n/a |
| C6 | #1948 remote-owner acceptance before origin SM `h` | **No.** For a remotely owned room the origin plans `IngressEffectIntent::DispatchToRoomRemote` and `RelayMucProxy` (`server/routes/interpret/room_dispatch.rs:1117-1140`), consistent with the issue text that the origin advances `h` before owner acceptance. Remote children are not sealed (B5). | n/a | n/a | n/a | n/a |
| C7 | #2018 remote-dispatch completion backlog | Diagnostic only: `server/docs/operations/remote-dispatch-backlog.sql` (read-only) | n/a | n/a | n/a | unknown (no production access). The #2028 record says two dispatches were pending, one body-bearing and unrepaired. There is no reconciler. |

## D. #1662 (P3 distributed connections and resume)

| # | Feature or config | Compiled | Wired | Enabled in production | Tested | Live-verified |
| --- | --- | --- | --- | --- | --- | --- |
| D1 | P3.1 `ConnectionActor` | **No.** The socket loop and `WsConnState` (`websocket/state.rs:1720`) live in one task per connection (`websocket/connection.rs`); handlers run inline under a 15 s wedge backstop. | n/a | n/a | n/a | n/a |
| D2 | P3 identity vocabulary: `ConnectionRef`, `NodeLeaseIdentity`, `SmBinding`, `WireStanzaBytes`, `UserClaimIdentity`, `SmPrevid` | **None exist.** Each has zero `-w` hits under `server/` (checked 2026-10-30). Present in production: `AuthenticatedPrincipalRef`, `ResumeIdentityProof`/`steal_for_resume`, `CleanupPending`. `UserClaimLease` exists but is private (`waddle-xmpp/src/registry/user_registry.rs:66-70`). | n/a | n/a | n/a | n/a |
| D3 | `ConnectionGeneration` (the type #1662 P3.3 builds its fence on) | A **process-local** `AtomicU64` minted in `WsConnState::new` (`websocket/state.rs:1884-1893`), restarting at 1 on every boot. It tags ingress submissions. No table column named `connection_generation` exists under `server/`. It is not a durable fence. `OccupancySessionGeneration` (`waddle-xmpp-core/src/occupancy_session.rs`) is a separate logical identity that its own docs say survives XEP-0198 resumption. | Yes | Yes (tagging only) | L-publish | unknown (no production access) |
| D4 | P3.2 `user_resources` rows | **No such table exists** in the tree. (A `git log -S` search of the available history only matches an unrelated function name; the local history is shallow.) The only durable per-resource authority is `xmpp_occupancy_authority(full_jid, generation)` (`db/migrations/global.rs:734`, read and written in `occupancy_authority.rs`). #1662's "existing substrate on main: partial `user_resources` rows" is not accurate; P3.2 is new DDL. | Yes (the occupancy table) | Yes (applied by migration) | n/a (a schema object; no lane asserts it beyond the migration ledger) | Applied migration is a ledger fact, not a live check: unknown (no production access) |
| D5 | P3.3 durable SM lifecycle and opaque `SmPrevid` | **Partial.** `PostgresFencedSmPersistence` (`sm_persistence_fenced/`) is the fenced SM store. `sm_sessions` has no `lifecycle` column and the server emits `previd = stream_id`. It also runs `ALTER TABLE sm_sessions ...` on every boot (`sm_persistence_fenced/implementation.rs:241-253`), outside the migration ledger, which matters for #1689. | Yes | Yes | `xep0198_cross_node_resume.rs` (L-pr; needs `clustering` and Postgres, silent skip). `server/crates/waddle-xmpp/tests/xep0198_session_registry.rs` is a separate in-memory SM registry suite (L-publish) and does not exercise the fenced store. | unknown (no production access) |
| D6 | P3.4 SASL2 (XEP-0388) and inline resume (XEP-0198 section 11) | **No.** `server/capabilities.toml` has no XEP-0388 entry and `urn:xmpp:sasl:2` appears in no Rust source. | n/a | n/a | n/a | n/a |

## E. Operations and evidence

| # | Feature or config | Compiled | Wired | Enabled in production | Tested | Live-verified |
| --- | --- | --- | --- | --- | --- | --- |
| E1 | Cluster metrics: `clustering/metrics.rs` uses its own `meter()` with raw instruments, not the `counter_add!`/`gauge_record!`/`histogram_record!` macros. Admission, custody, uncertainty, mailbox bytes/depth/age metrics do not exist. | Yes | Yes | Yes | none found for exported names | unknown (no production access) |
| E2 | Cluster alert rules (`ClusterHeartbeatStale`, `ClusterHeartbeatMissing`; `rules/mimir/waddle-reliability.yaml`) | Yes (rule files) | Uploaded by the rules process (not examined) | unknown | `infrastructure/waddle.cloud/rules/tests/run.sh` (promtool) exists, but no workflow, `env.cue` or `namespace.cue` entry runs it. W1.12. | unknown (no production access) |
| E3 | Production topology | n/a | n/a | One bare-metal node (`clusters/production.yaml`: `control-plane` pool `size: 1`), storage replica count 1, CNPG `instances: 2` on the same node, pod anti-affinity only preferred. Replicas buy rolling deploys and process isolation, not hardware fault tolerance. | n/a | unknown (no production access) |
| E4 | Postgres backup and WAL archive | **No configuration.** `postgresql-cluster.yaml` sets `walStorage` only; it has no backup, barman or archive stanza. | n/a | No | n/a | unknown (no production access); the absence is a config fact, whether backups exist some other way is not known |

## Where this changes what #2046 and #1662 say

- #2046's baseline anchors are from `c9be424`, which is not on `main`. This
  table cites `main@db6e533` paths. The anchors that moved: `RoomRegistryActor`
  is `room_registry_actor/implementation.rs:319`, `UserActor` is
  `user_actor/implementation.rs:22`, `WsConnState` is `websocket/state.rs:1720`,
  ingress authority is `server/http.rs:653-679`.
- #1662 P3.2's "partial `user_resources` rows" (D4) does not match the code:
  that table is new DDL. P3.3's durable `ConnectionGeneration` is target design;
  the existing type of that name is a process-local counter and not that fence
  (D3). The transport and occupancy generations stay distinct.
- "Tested" for the cluster rows A3 to A6 means L-pr, which only runs on
  pull requests and tolerates a missing Postgres. None of those rows is covered
  by a lane that gates publish.
- Rows C2 to C6 and D1, D2, D4 and D6 are absent, not partially wired. Nothing in
  #1986, #1948, #1661 P2.3/P2.4 or #1662 P3.1/P3.2/P3.4 should be described as
  landed.

## How to refresh

Counts and `path:line` anchors drift quickly in this area (#2028 split large
modules; #1893, #1985 and #2040 each added routing branches). When a row
changes, re-check the cited path against the current `main` SHA and update the
SHA in the header. Do not move a row to `live-verified` without a recorded
observation of the running system.
