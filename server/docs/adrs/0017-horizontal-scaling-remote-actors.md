# ADR-0017: Horizontal Scaling via Remote Actors with Postgres-Authoritative Ownership

## Status

Draft

## Context

Waddle currently runs as a single stateful replica by design. The Helm chart
pins `replicaCount: 1` and `templates/validations.yaml` fails rendering when
`replicaCount > 1` with a ReadWriteOnce PVC. All real-time routing state is
process-local:

- **Connection routing**: `ConnectionRegistry.connections: DashMap<FullJid,
  ConnectionEntry>` maps JIDs to in-process `mpsc::Sender<OutboundStanza>`
  channels (`waddle-xmpp/src/registry/connection_registry.rs`). There is no
  JID→node lookup; targets with no local connection fall through to
  detached-session queueing or offline storage.
- **MUC rooms**: each room is a single in-memory kameo `RoomActor` owning a
  `MucRoom` with occupant state (`muc/room.rs`, `muc/room_registry.rs`).
- **Presence**: `presence_states: DashMap<FullJid, PresenceState>` in the
  `ConnectionRegistry`, never shared or persisted.
- **XEP-0198 sessions**: `InMemorySmSessionRegistry` with a durable
  SQLite/Postgres mirror (`sm_sessions`/`sm_unacked`) that survives a
  single-node restart but is not cluster-aware; `<resume/>` only consults the
  local in-memory registry.

Restart resilience is already strong: durable XEP-0198 resumption, XEP-0397
ISR tokens, graceful SIGTERM drain that emits RFC 6120 `<system-shutdown/>`
stream errors and RFC 7395 `</close>` frames, Q6 promotion of unacked stanzas
into durable offline delivery, MAM (XEP-0313) catch-up, and a client with
exponential-backoff reconnect and persisted SM resume state.

The gap is any cross-node fabric. Two users on different replicas cannot
exchange stanzas; two replicas would each materialize their own copy of the
same MUC room.

ADR-0008 adopted kameo as the actor framework. Kameo's `remote` feature
(disabled today; `kameo = "0.20"` without libp2p in the lockfile) provides
`RemoteActorRef` lookup by registered name over a libp2p swarm with
serde-serialized typed messages, plus a kademlia-based registry.

## Decision

Scale horizontally using **kameo remote actors as the inter-node transport
and routing cache**, with **Postgres as the authoritative ownership store**.

Key elements:

1. **Postgres-only for multi-replica.** Clustering is gated on the Postgres
   backend; SQLite remains supported for single-node deployments only.
2. **Finish the actor migration first (single-node refactor).** Route live
   delivery through `UserActor` (actor-per-bare-JID, already implemented with
   tests in `waddle-xmpp/src/registry/` but not wired into `waddle-server`)
   and introduce a connection actor per WebSocket pinned to the node owning
   the TCP socket. Retire the `ConnectionRegistry` DashMap delivery path.
3. **Enable kameo `remote`.** Bootstrap the libp2p swarm from a Kubernetes
   headless Service (no mDNS). Register `UserActor` under the bare JID and
   `RoomActor` under the room JID. Remote nodes route stanzas by
   `RemoteActorRef` tell; serde at the remote-actor boundary is the I/O
   boundary permitted by the typed-payloads rule.
4. **Postgres-authoritative ownership claims.** A `(entity, node_id, epoch,
   heartbeat)` claims table is the source of truth for which node owns a
   given `UserActor`, `RoomActor`, or detached SM session. Kameo's kademlia
   registry acts as a routing cache over it. Claim acquisition and steal are
   transactional; stale claims are reaped by heartbeat expiry. This resolves
   registration races and split-brain that kademlia alone cannot.
5. **MUC stays single-writer.** `RoomActor` ownership becomes
   cluster-addressable via the claims table; non-owner nodes proxy joins and
   messages to the owning actor. On node death, ownership is re-elected and
   occupants re-join (the client already handles "fresh" sessions).
6. **Cross-node SM resume via claim steal.** On `<resume/>` for a session
   another node owns, the receiving node steals the claim transactionally,
   loads the durable `sm_sessions`/`sm_unacked` snapshot, and the old owner
   drops its in-memory state. No load-balancer session affinity required.
7. **At-most-once inter-node delivery is acceptable.** XEP-0198 acks provide
   end-to-end reliability; a stanza lost to a node crash mid-flight falls
   through to the Q6/offline-delivery path, which becomes
   correctness-critical rather than a shutdown nicety.
8. **Presence fan-out over the actor transport.** Presence changes are
   broadcast to interested remote `UserActor`s; each node keeps a local view
   for its own connections. No shared presence map.

## Implementation Plan

Phased so each step ships value independently and `replicaCount: 1` remains
the default until the final phase:

- **Phase 0 — restart hardening (independent of clustering):** add a
  `preStop` hook to the Helm deployment; fail hard at startup when SM
  persistence has no DSN in production; finish the Ecdysis listener fd
  hand-off so single-node restarts have zero listen gap.
- **Phase 1 — actor migration (single node):** wire `UserRegistryActor` /
  `UserActor` into the live delivery path; introduce per-connection actors
  wrapping the WebSocket sink; delete the DashMap delivery path.
- **Phase 2 — remote spike:** enable the kameo `remote` feature behind a
  config flag; prove a cross-node `UserActor` tell round-trip; evaluate
  partition behavior, registration churn, and libp2p operational posture in
  Kubernetes.
- **Phase 3 — ownership claims:** claims schema and transactional
  acquire/steal/heartbeat in `waddle-server`; wire SM resume steal; wire
  `RoomActor` ownership.
- **Phase 4 — cross-node routing GA:** DM routing across nodes; MUC proxying;
  presence fan-out; Helm changes (drop RWO PVC requirement in favor of the
  existing `object_store` S3 path, lift the `allowUnsafeRwoScale` guard,
  headless Service for swarm bootstrap).

## Consequences

### Positive

- Coherent with ADR-0008: one concurrency model (actors) locally and across
  nodes; no new message-bus dependency (NATS/Redis) to operate.
- Typed messages end-to-end; serde only at the remote boundary.
- Postgres claims give a single, transactional source of truth for
  ownership; kademlia is only a cache, so partitions degrade to slower
  lookups rather than split-brain.
- Phase 1 is a pure refactor that pays for itself (retires dead-code-in-
  waiting and external locking) even if clustering never ships.

### Negative

- Kameo's `remote` feature is the youngest part of a young library; libp2p
  is a heavy dependency with its own operational surface.
- The socket can never be remote: delivery always terminates at the node
  owning the TCP connection, so node death always drops live sockets and
  relies on client reconnect + SM resume.
- Q6/offline promotion becomes correctness-critical for at-most-once
  inter-node delivery.
- MUC ownership re-election on node death causes occupant re-joins (visible
  as presence churn in large rooms).

## XEP Conformance Notes

- XEP-0198 (Stream Management): resume semantics unchanged on the wire;
  cross-node resume is a server-internal claim steal. `h` counter integrity
  across steal must be preserved from the durable snapshot.
- XEP-0045 (MUC): single owning `RoomActor` preserves total ordering of
  broadcasts per room. Occupant re-join on re-election is standard client
  behavior after `<presence type='unavailable'/>` from the room.
- XEP-0313 (MAM) / XEP-0160 (offline): unchanged; Q6 promotion path already
  stamps XEP-0203 `<delay/>`.
- RFC 6121 §8.5.2.1 resource selection moves from `ConnectionRegistry` into
  `UserActor`, which owns all resources for a bare JID regardless of node.
