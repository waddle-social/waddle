# ADR-0017 Phase 2 Plan — Owned libp2p Swarm (Discovery-Only, Flag-Gated)

Companion execution note to [ADR-0017](./0017-horizontal-scaling-remote-actors.md)
and the [Phase 1 completion note](./0017-phase1-completion-authoritative-registration.md).
Status: **Phase 2 implemented — Slices 0–6 landed** (each council-reviewed;
review findings folded in as fix commits). Tracking: issue #1195.

Implementation notes vs. this plan:
- The subsystem lives in `crates/waddle-server/src/clustering/` (config,
  swarm, lease, allowlist, codec, relay, dns, identity, metrics).
- The keypair-slot lease + peer-allowlist tables are self-initializing
  Postgres-only schema (never in the migration stream, so they stay out of
  non-clustering deployments). The dedicated control-plane pool remains a
  Phase 3 deliverable as planned.
- The relay supervisor additionally re-registers its name every 15s: a
  registration performed before the node's first peer connection stores its
  provider record only locally, and kademlia's own republish is 30 minutes
  out — periodic same-name refresh bounds a (re)started node's
  undiscoverability window (found by the Slice 6 churn harness case).
- No dedicated CI check was added: `nixTest` runs the workspace with
  `--all-features` against the Nix-spawned Postgres, which compiles and runs
  the clustering unit tests and the multi-process harness already.

## Objective (this phase)

Stand up the inter-node **transport subsystem** — kameo remote actors as the
transport, over an **owned libp2p swarm** — gated behind a `clustering` flag.
This phase is **node discovery only**: kademlia is demoted to peer discovery,
there is **no cross-node stanza routing** (that is Phase 4). The peer allowlist
is enforced. `replicaCount > 1` and `clustering.enabled` stay **hard-locked in
the Helm chart until Phase 4** — Phase 2 is exercised exclusively by the
multi-process test harness against a shared Postgres.

**Prime directive — strictly additive, flag-gated.** With clustering disabled
(the default) the server's behaviour is **byte-for-byte identical to today's
single-replica path**. The hot delivery path is not touched in this phase.

## Gating decisions (default; open to change)

1. **Build gate: a `clustering` Cargo feature (default OFF).** The feature
   enables `kameo/remote` (which pulls libp2p 0.56) and compiles the
   `swarm`/`codec`/`relay` modules. The default production build links **zero
   libp2p** and is unchanged. CI still lints the code — clippy runs
   `--workspace --all-features --all-targets -D warnings` — and a dedicated
   feature-enabled Postgres check runs the clustering tests. Rationale: libp2p
   is a heavy dependency the team deliberately kept out of the lockfile; making
   it opt-in at compile time keeps the default binary identical and lean while
   the subsystem cannot run in production until Phase 4 regardless. This is a
   compile-time gate *in addition to* — not instead of — the runtime
   `clustering.enabled` config the ADR mandates; a clustering-built binary is
   still inert unless the config flag is set.
2. **Runtime gate: `clustering.enabled` config (default false), Postgres-only.**
   The swarm subsystem starts only when the feature is compiled **and**
   `clustering.enabled` is set **and** the DB driver is Postgres. Enabling the
   flag on SQLite is a hard startup error (clustering requires the Postgres
   control plane).
3. **One draft PR, sliced internally** (mirrors Phase 1 / PR #1177). Each slice
   is council-reviewed (adversarial concurrency + correctness) and CI-green
   before commit.
4. **Harness fencing cases deferred to Phase 3.** The ADR lists lone-survivor
   (N=2) and single-dead-link-of-3 degrade-without-fencing under the Phase 2
   harness, but the heartbeat-fencing + durable-queue logic they assert lands
   in Phase 3. Phase 2 builds the harness and every fault-injection primitive
   (pause heartbeat, kill link, stale record, relay panic/recover,
   revoked-peer-with-live-connection) and asserts everything the Phase 2
   machinery supports; the two fencing-dependent assertions are wired as
   pending/ignored scaffolds and activated with the Phase 3 fencing they
   exercise.

## kameo 0.20 `remote` — verified realities

- Enabling `kameo` feature `remote` pulls `libp2p = 0.56` with features
  `cbor, kad, noise, mdns, quic, request-response, tcp, tokio, yamux`.
- We do **not** use `remote::bootstrap()` (an mDNS dev helper). We compose our
  own `#[derive(NetworkBehaviour)]` struct containing `kameo::remote::Behaviour`,
  build the swarm with `SwarmBuilder` (tcp + noise + yamux + quic), call
  `behaviour.kameo.try_init_global()` to install the process-global
  `ActorSwarm`, and drive the event loop ourselves.
- `messaging::Config` exposes exactly the knobs the ADR names:
  `with_request_timeout` (default 10s — the binding transport cap),
  `with_max_concurrent_streams` (default 100), `with_request_size_maximum`
  (1MB), `with_response_size_maximum` (10MB).
- Kademlia parameters are **hardcoded by kameo** (query timeout 10s, replication
  5, record TTL 1h, republish 30min, Server mode; the field is private) — they
  are documented, not configured.
- `try_init_global()` sets a **process-global** swarm ⇒ two in-process swarms
  are impossible ⇒ multi-node tests must be **multi-process**.
- Neither `xmpp-parsers` nor `minidom` implements serde ⇒ the remote codec is an
  explicit XML-text wrapper deliverable.

## Slice order

Every slice is behind the `clustering` feature + runtime flag; default-off
behaviour is unchanged and asserted by the unchanged e2e suite.

- **Slice 0 — deps + config + inert spawn scaffold.**
  Add the `clustering` Cargo feature (enables `kameo/remote`); add libp2p to the
  lockfile. Add `ClusteringConfig` to `ServerConfig` (`enabled` default false,
  swarm listen addrs, bootstrap headless-DNS name, keypair-pool source, the
  timeout hierarchy values, allowlist/heartbeat refresh intervals) via the
  established `from_env` / typed-error pattern (`WADDLE_CLUSTERING_*`). Add the
  conditional, Postgres-gated spawn point in `start_with_config` that currently
  logs "clustering enabled" and does nothing else. Config parsing unit tests.
  *Acceptance:* default build links no libp2p; feature build compiles; flag-off
  path identical.

- **Slice 1 — owned swarm: event loop, transports, identity, kademlia
  bootstrap (discovery only), headless-DNS dialing.**
  `swarm` module: composed `NetworkBehaviour`, `SwarmBuilder`, explicit
  `messaging::Config` (request_timeout sized above the worst-case fenced-write /
  resume-handshake budget; `reply_timeout ≤ request_timeout` and
  `mailbox_timeout + handler budget ≤ request_timeout` invariants asserted;
  size maxima set). Per-pod keypair from the configured source. Listen, kademlia
  bootstrap, supervised event-loop task on the shared `stop_token`. Headless-DNS
  peer resolution + dial with re-dial on pod churn. Swarm observability
  (connected-peer gauge, kademlia routing-table-size gauge, bootstrap-retry
  counter) via the OTel `metrics.rs` + periodic-task pattern.
  *Exit:* single-node swarm boots, listens, reports PeerId, bootstraps.

- **Slice 2 — keypair-slot Postgres CAS lease + safe multi-connection
  convergence.**
  `keypair_slots` lease (Postgres-only CAS: Acquire `INSERT … ON CONFLICT DO
  NOTHING` + `rows_affected==1`; Heartbeat renewal CAS on freshness; Expire CAS
  (committed `expired` flag); Release on drain — all on Postgres `now()`),
  behind a `KeypairSlotLease` trait. Startup leases one slot from the configured
  keypair pool and selects that keypair; a heartbeat janitor renews; drain
  releases. `rows_affected==0` on renewal ⇒ fencing loss ⇒ self-fence the
  clustering subsystem. Additionally, if a full `lease_ttl` elapses without a
  single successful renewal (e.g. persistent database errors keep every
  heartbeat from landing), the node defensively self-fences the same way — it
  can no longer prove it still holds the slot. Self-fencing cancels a
  clustering-scoped child token, tearing down only the clustering subsystem
  (swarm, relay, janitors) — never the whole server.
  Multiple authenticated transport connections to the same PeerId remain
  valid: simultaneous full-mesh dialing can establish an inbound and outbound
  connection at both endpoints, and independently closing each endpoint's
  locally second connection can make the peers close opposite links and lose
  connectivity entirely. Peer-level bookkeeping therefore changes only on
  the first established and last closed connection. Runs on the shared global
  pool in Phase 2; the **dedicated small control-plane pool/handle** is a
  Phase 3 deliverable (see the implementation notes).
  Postgres-gated CAS tests (acquire, renew, expire-steal, at-most-one
  leaseholder per slot).

- **Slice 3 — peer allowlist enforcement + live-connection revocation.**
  `peer_allowlist` table (enrolled PeerIds); runtime role `SELECT`-only
  (grants documented here, enforced by the Phase 4 chart). `AllowlistStore`
  trait + impl. Swarm-layer enforcement is **connection-level**: reject
  non-allowlisted peers at connection establishment (Noise handshake
  necessary, never sufficient), and a periodic refresh janitor diffs the
  enrolled set and **actively closes/bans live connections** whose PeerId is
  no longer enrolled — so a revoked peer loses its live connections within one
  refresh interval and cannot re-dial. Containment bound asserted: revoked
  peer disconnected within one refresh interval. **Deferred:** per-message
  origin re-validation — kameo's remote ask/tell handlers are not handed the
  transport-level sender PeerId, so re-checking the allowlist per inbound
  message requires sender identity surfaced to handlers; it lands with Phase
  4's cross-node routing (alongside the claim-epoch checks). Until then the
  enforcement guarantee is exactly: denial at connection establishment, plus
  revocation of already-connected peers within one refresh interval.
  Postgres-gated tests + the harness revoked-peer-with-live-connection case.

- **Slice 4 — remote codec (XML-text serde wrappers).**
  `RemoteStanza` / `RemoteElement` newtypes that are `Serialize`/`Deserialize`
  over the XML-text form (serialize via `stanza_to_string(to_element())`, so the
  `ensure_thread_element` fixup is preserved; deserialize re-parses through the
  existing inbound parser). Typed re-parse error path ⇒ NACK to sender, never a
  silent drop. Bounded deserialization: max envelope size, collection-length
  caps, **and max XML nesting depth + attribute/namespace count caps enforced
  before minidom tree construction** (a small deeply-nested payload is a typed
  re-parse NACK, not a stack overflow). Drop metrics. Dedicated Rust tests:
  round-trip, `<thread/>` preservation, oversized / deeply-nested → typed NACK
  not crash.

- **Slice 5 — per-peer relay actors, supervised.**
  Per-node relay actor registered in kademlia under a per-`node_id` relay name
  (the only kademlia registration — O(1) names/node; **never per-entity**).
  Supervised via an owning task: respawn + **mandatory re-registration under the
  same name**; sender-side `ActorNotRunning`/`UnknownActor`/`BadActorType` ⇒
  bounded-backoff kademlia re-lookup (a transport-layer refresh path distinct
  from the Phase 3 `NotOwner` claims-refresh path). **Discovery-only:** the
  relay proves a cross-node ask round-trip (spike exit criterion) but is **not**
  wired into the delivery hot path — cross-node routing is Phase 4.

- **Slice 6 — multi-process cluster harness + spike exit criteria.**
  Extend the `TestServer` binary-spawner to launch N clustering-enabled
  processes over a shared `WADDLE_TEST_POSTGRES_URL`. Fault-injection
  primitives: dropped tells, paused heartbeats, stale node records, relay-actor
  panic + recover, revoked-peer-with-live-connection. Assert the Phase 2 spike
  exit criteria:
  - cross-node `UserActor`/relay ask round-trip;
  - **integrity under concurrent large + small stanzas** (libp2p per-substream
    flow control interleaves naive-parallel requests; every interleaved ask
    comes back intact — per-pair *sequencing* is Phase 4);
  - **the timeout hierarchy, both halves.** (1) Receiver-applied ask budgets:
    a handler that outlives `reply_timeout` fails with the typed
    `ReplyTimeout` classification inside the reply budget. Note that with the
    validated invariant `mailbox + reply <= request` the receiver *always*
    replies (success or `ReplyTimeout`) before the transport cap, so this case
    alone can never exercise `request_timeout`. (2) The transport cap as the
    binding bound: a dedicated ask deliberately inflates the receiver budgets
    past both the handler and the cap — the receiver then cannot proactively
    reply in time, and the sender's libp2p `request_timeout` fails the ask
    with the typed `Transport` classification (kameo `NetworkTimeout`) at
    ≈ the cap, proving the `request_timeout` wiring is live;
  - kademlia re-discovery through a sequential rolling restart of **both**
    bootstrap peers (at most one node down at any instant; one leg hard-kill,
    one leg graceful drain): each replacement — fresh node_id, relay name, and
    leased slot on the same swarm port — is re-discovered, and both
    replacements are reachable once no original peer survives;
  - **measured visibility window of a dead publisher's provider+metadata
    records** against an explicit acceptance threshold (go/no-go: the window is
    dominated by the hardcoded 1h TTL / 30min republish and cannot be tuned;
    graceful stops proactively unregister so the bound applies to hard-killed
    nodes) — carried as an ignored scaffold in the harness and performed as a
    manual out-of-band measurement, since the window is set by kademlia
    constants, not by anything the suite could regress;
  - relay respawn re-registration + peer re-resolution after `ActorNotRunning`;
  - revoked-peer disconnect within one allowlist refresh interval.
  **Deferred to Phase 3** (pending scaffolds): lone-survivor at N=2 keeps
  serving; single-dead-link-of-3 degrades to durable fallback without fencing —
  both require Phase 3 heartbeat fencing + durable queue.

## Test / CI gates

- `bun test && bun run lint` unaffected (no chat changes).
- clippy `--workspace --all-features --all-targets -D warnings` clean (feature
  code included). No `#[allow]`, no `.unwrap()/.expect()` in non-test code.
- `cargo fmt` before every commit.
- The unchanged `xmpp_e2e_cue` suite stays green (proves default-off identity).
- New Postgres-gated clustering tests skip cleanly when
  `WADDLE_TEST_POSTGRES_URL` is unset (existing convention); a dedicated
  feature-enabled Nix check runs them in CI against the Nix-spawned Postgres.
- Every advertised/implemented behaviour carries dedicated Rust tests
  (codec, lease CAS, allowlist/revocation, relay supervision, harness).

## What Phase 2 does NOT do (guardrails)

- Does **not** unlock `replicaCount > 1` or `clustering.enabled` in Helm (Phase
  4).
- Does **not** route any stanza cross-node (Phase 4).
- Does **not** add the `nodes`/`claims`/`steal_intents` ownership schema, SM
  fencing, cross-node resume, or durable MUC state (Phase 3).
- Does **not** touch the hot delivery path's semantics.
