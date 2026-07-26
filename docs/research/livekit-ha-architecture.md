# Highly-Available Self-Hosted LiveKit, Single Region, on Kubernetes

Primary-source research for waddle issue #1493 ("wayfinder research"). Our
`livekit-sfu` Helm chart (`infrastructure/waddle.cloud/charts/livekit-sfu`) is
currently a deliberately single-replica SFU with no Redis, a 5-hour
`terminationGracePeriodSeconds` used as a cordon-and-drain crutch, and
NodePort-based RTC/TURN media exposure. This document establishes, from
LiveKit's own docs and source, what an HA topology within one region actually
requires, so a future HA redesign of the chart can be scoped correctly. No
code was changed as part of this research.

## Recommended topology

Run 3+ `livekit-server` replicas (one pod per node, `hostNetwork` or NodePort
media, spread across AZs if the region has them) behind a Redis instance in
**Sentinel or a managed HA mode** (not bare standalone) that is shared with
the egress service's Redis requirement — LiveKit's own docs state egress
Redis "must be the same redis address used by your livekit server," so this
is not optional isolation, it's a documented coupling. Prefer the embedded
TURN/UDP+TLS server per node (not a separate always-on TURN fleet) fronted by
a network load balancer for the TLS port, and roll deployments with
`maxSurge>=1, maxUnavailable=0` (surge-then-drain) rather than our current
`maxSurge=0` in-place recycle, paired with a `preStop` hook that calls
`Drain()`/SIGTERM before the pod is killed, and a much shorter
`terminationGracePeriodSeconds` sized to the intended max call length rather
than an operational worst case of 5 hours.

## 1. Multi-node SFU with Redis — topology and egress coupling

LiveKit's distributed-mode doc states plainly: **"Redis is required as
[a] shared data store and message bus"** for any multi-node deployment — it's
what turns independent `livekit-server` processes into a cluster capable of
room-to-node routing and signaling-message relay ([Distributed multi-region — LiveKit Docs](https://docs.livekit.io/home/self-hosting/distributed/)).
The doc does not itself mandate Sentinel/Cluster over standalone Redis, but
the sample config exposes commented-out Sentinel, Cluster, and TLS blocks
alongside the plain `address` field ([`config-sample.yaml`](https://github.com/livekit/livekit/blob/master/config-sample.yaml)),
and the `livekit-helm` server chart's own `values.yaml` documents the same:
address/db/username/password plus "supports sentinel and cluster
configurations" ([`livekit-helm/livekit-server/values.yaml`](https://github.com/livekit/livekit-helm/blob/master/livekit-server/values.yaml)).
For single-region HA (no cross-region split-brain concerns), Redis
**Sentinel** (3 sentinels + primary/replica) is the standard "don't lose the
room map if the Redis primary dies" answer; a single standalone Redis
instance is a single point of failure for the entire cluster's room routing.

On the egress-sharing question (issue #1023 in this repo): LiveKit's own
Egress docs are explicit that the egress service's Redis `address` **must be
the same Redis address used by your livekit server** — egress uses Redis
pub/sub to receive job requests and coordinate with the SFU, and "your
livekit server cannot connect to an egress instance through redis" any other
way ([Egress service — LiveKit Docs](https://docs.livekit.io/home/self-hosting/egress/)).
So the answer to "can one Redis serve both SFU routing and egress
coordination" is: **yes, and per LiveKit's docs it is the only supported
wiring** — there is no documented "separate Redis for egress" mode. Both
functions are pub/sub + a few hash keys on the same logical Redis; they do
not need separate Redis clusters for correctness or isolation. What they do
need is the shared Redis to be sized/HA'd for both workloads (egress fanout
adds pub/sub traffic on top of the room-map hash reads/writes).

## 2. Room-to-node assignment mechanics

The routing implementation (`pkg/routing/redisrouter.go`) keeps two Redis
hashes:

- **`"nodes"`** — a hash of node ID → protobuf-marshaled node state
  (registration/heartbeat), used for node discovery and for
  `RemoveDeadNodes()` cleanup of stale entries.
- **`"room_node_map"`** — a hash of room name → assigned node ID, read via
  `GetNodeForRoom()` and written via `SetNodeForRoom()`
  ([`pkg/routing/redisrouter.go`](https://github.com/livekit/livekit/blob/master/pkg/routing/redisrouter.go)).

When a client creates/joins a room, the signaling node it happens to connect
to checks `room_node_map`; if unset, it runs the configured **node
selector** to pick a hosting node and writes the mapping, then proxies
signaling to that node for the life of the room ("a room must fit on a
single node" — [Distributed multi-region — LiveKit Docs](https://docs.livekit.io/home/self-hosting/distributed/)).
Selector kinds are `any`, `sysload`, `cpuload`, and `regionaware`, with
`sort_by` options `random`, `sysload`, `cpuload`, `rooms`, `clients`,
`tracks`, `bytespersec`, and a `sysload_limit` gate; `regionaware` layers
region-proximity filtering on top of `sysload` selection, picking the
closest region below the load threshold and choosing randomly among
qualifying nodes in it ([`pkg/routing/selector/regionaware.go`](https://github.com/livekit/livekit/blob/master/pkg/routing/selector/regionaware.go),
config shape in [`config-sample.yaml`](https://github.com/livekit/livekit/blob/master/config-sample.yaml)).
For a single-region deployment, `sysload` or `cpuload` selection is
sufficient — `regionaware` only pays off once nodes carry a `region:` label
across multiple regions.

Neither the fetched docs pages nor the router source describe an automatic
re-pin/rebalance if the assigned node dies mid-room — `RemoveDeadNodes()`
only prunes the `nodes` registry; nothing in the fetched material shows the
`room_node_map` entry being proactively cleared or the room being
re-homed to a live node. In practice this means a room is only freed for
re-assignment once whatever created the stale mapping (server restart,
explicit room-close signal, or a subsequent create-room call after the
mapping's node is gone) causes a fresh selection — this is the load-bearing
gap covered in point 3 below.

**Verified against raw source**
([`pkg/routing/redisrouter.go`](https://raw.githubusercontent.com/livekit/livekit/master/pkg/routing/redisrouter.go)):
there are two Redis hash keys — `nodes` (node_id → Node proto) and
`room_node_map` (room_name → node_id). The mapping is written with a plain
`HSet(ctx, NodeRoomKey, roomName, nodeID)` and read with
`HGet(ctx, NodeRoomKey, roomName)`; it is removed only by `ClearRoomState`,
which does `HDel(ctx, NodeRoomKey, roomName)`. **There is no TTL or Redis
expiry on `room_node_map` entries** — stale mappings persist until something
explicitly calls `ClearRoomState`. Separately, `RemoveDeadNodes()` walks the
`nodes` hash, tests `!selector.IsAvailable(n)`, and `HDel`s the offender from
`NodesKey` *only* — it does not touch `room_node_map`. So a room pinned to a
node that dies keeps a dangling mapping pointing at a node that no longer
exists in the registry, and recovery depends on a `ClearRoomState` happening
on the (re)join path rather than on any proactive repair loop. Plan HA
around "the room is gone and clients must fully rejoin", not around
transparent re-homing.

## 3. In-progress calls on node loss and rolling deploys; client rejoin

**Node loss / restart is disruptive per-room, not per-cluster.** Because a
room is pinned to exactly one node (point 2), losing that node drops every
participant's media on that room; there is no live migration of an
in-progress WebRTC session to another SFU node. This matches our chart's own
`validations.yaml` comment, and nothing in LiveKit's distributed-mode doc
contradicts it — the doc describes cluster-wide capacity and node
selection, never mid-call handover ([Distributed multi-region — LiveKit Docs](https://docs.livekit.io/home/self-hosting/distributed/)).
LiveKit does **not** have a "session migration" / regional relocation
feature at the SFU level; the fetched client-connect documentation
explicitly focuses reconnection logic on recovering the *existing* transport
rather than switching which SFU node/region backs the room ([LiveKit client connect/reconnect docs](https://docs.livekit.io/home/client/connect/)).

**Server-side graceful shutdown**: `pkg/service/server.go`'s `Stop()` method
calls `s.router.Drain()` to stop the node from accepting new room
assignments, then polls every 5 seconds logging "waiting for participants to
exit" until active participants have left (unless a force-shutdown is
requested), before tearing down the debug server, TURN listener, room
manager, signal server, and IO service; `Start()` separately gives the HTTP
listener a 5-second shutdown window ([`pkg/service/server.go`](https://github.com/livekit/livekit/blob/master/pkg/service/server.go)).
This is consistent with the Kubernetes deployment doc's own explanation for
the Helm chart's 5-hour `terminationGracePeriodSeconds` default — sized so
active sessions have time to finish naturally rather than being cut off
mid-upgrade ([Kubernetes — LiveKit Docs](https://docs.livekit.io/home/self-hosting/kubernetes/)).
**Verified against raw source**: `cmd/server/main.go` wires the signal handler
explicitly, and the first signal is a *graceful* stop while a second signal
forces immediate teardown
([`cmd/server/main.go`](https://raw.githubusercontent.com/livekit/livekit/master/cmd/server/main.go)):

```go
sigChan := make(chan os.Signal, 1)
signal.Notify(sigChan, syscall.SIGINT, syscall.SIGTERM, syscall.SIGQUIT)

go func() {
	for i := range 2 {
		sig := <-sigChan
		force := i > 0
		logger.Infow("exit requested, shutting down", "signal", sig, "force", force)
		go server.Stop(force)
	}
}()
```

Operationally this means: Kubernetes' initial `SIGTERM` starts the drain, and
the process keeps serving existing sessions until they end or
`terminationGracePeriodSeconds` expires and the kubelet sends `SIGKILL`.
There is no second `SIGTERM` from Kubernetes, so the `force` path is only
reachable via a manual signal.

**Client-side reconnect**: the SDK first attempts an **ICE restart** —
reconnecting the signaling WebSocket and restarting ICE on the existing
peer connection — which the docs describe as giving "minimal or no
disruption" for transient network issues. If that fails, the SDK falls back
to a full reconnect: other participants' tracks temporarily disappear, local
tracks unpublish, a `Reconnecting` event fires, a fresh room join happens,
`Reconnected` fires, and tracks/participants republish
([LiveKit client connect/reconnect docs](https://docs.livekit.io/home/client/connect/)).
Critically, on real node loss (not just network blip), ICE restart cannot
help because the *server side* of that peer connection is gone — the client
will exhaust ICE-restart attempts and fall into the full-reconnect path,
which re-runs room-node selection and gets a **new** (live) node, at the
cost of a visible rejoin/drop-and-recover for every participant in that
room.

## 4. Zero-downtime rollout with NodePort media (no cloud LB UDP draining)

Because our chart uses NodePort for RTC and TURN/UDP (no cloud UDP load
balancer to hold connections open during a node's termination), the standard
LiveKit-documented pattern is exactly what the k8s guide bakes into the
official Helm chart: **rely on `terminationGracePeriodSeconds` sized to
worst-case call duration plus the router's `Drain()`+wait-for-empty
sequence**, not on load-balancer-level connection draining, since there is
none at the UDP layer for NodePort ([Kubernetes — LiveKit Docs](https://docs.livekit.io/home/self-hosting/kubernetes/),
[`pkg/service/server.go`](https://github.com/livekit/livekit/blob/master/pkg/service/server.go)).
For a multi-replica rollout to be zero-downtime under this constraint, the
safe pattern is:

- **Surge before scale-down**: `maxSurge>=1, maxUnavailable=0` so a new pod
  (and node, if one-pod-per-node scheduling is enforced) is fully Ready and
  taking new rooms *before* an old pod is asked to stop — the inverse of our
  current `maxSurge=0, maxUnavailable=1`, which is only safe today because
  we intentionally run a single replica and treat every rollout as a
  drain-then-replace, not a real rolling update.
- **`preStop` hook + SIGTERM sequencing**: a `preStop` lifecycle hook that
  removes the node from receiving new signaling traffic (e.g., readiness
  probe flips to not-ready, or an explicit drain call) before the kubelet
  sends SIGTERM, so in-flight rooms finish while new rooms land elsewhere in
  the cluster; the node loss/disruption of point 3 becomes bounded to
  whatever calls are still on that node at rollout time.
  the ~5s HTTP shutdown window in `server.go` needs to be understood as
  the *http* server's own budget, not the participant-drain budget, which is
  governed entirely by `terminationGracePeriodSeconds`.
- **Termination grace period sized to the SLA**, not to an unbounded worst
  case: LiveKit's own Helm default is 5 hours specifically because there is
  no failover for an in-progress call in the *upstream* chart either — it is
  not evidence that 5h is a "correct" HA value, only that upstream, like us,
  currently treats a single node's rollout as call-draining rather than
  call-preserving. In a real multi-node HA setup, the grace period should be
  sized to the **product's acceptable max call length before a forced
  reconnect**, not to accommodate zero call loss on a single node.

## 5. TURN HA

LiveKit's embedded TURN server can run standalone-per-node or in "distributed"
mode. The Kubernetes/self-hosting material and sample config show TURN with
`udp_port` (STUN+TURN/UDP, sample default `3478`) and `tls_port` (TURN/TLS,
sample default `5349`, commonly remapped to `443` when no separate LB is
used) plus a `relay_range_start`/`relay_range_end` port range for the
actual media relay allocations, `external_tls`, `domain`, `ttl_seconds`, and
CIDR allow/deny lists for peer relaying ([`config-sample.yaml`](https://github.com/livekit/livekit/blob/master/config-sample.yaml)).
For a distributed/multi-node setup, LiveKit's own guidance is: **"use a
network load balancer in front of the [TURN/TLS] port"** for the
distributed case, and only remap to 443 directly when there's no LB in
front (i.e., a single-instance setup) — implying HA TURN wants an L4 LB
(TCP/TLS passthrough) in front of the TLS listener across nodes, while
TURN/UDP relay traffic is inherently per-allocation and does not need
session affinity beyond "the client keeps talking to whichever node
allocated its relay." Each embedded TURN instance issues its own relay
allocations from its own node's port range, so nodes do **not** need to
share TURN state with each other — the coordination need is purely "route a
given client's TURN traffic consistently to the node that allocated its
relay," which for embedded TURN is naturally satisfied because the client
dials that node's `domain`:port directly (the allocation *is* the affinity).
**Shared realm/secret**: the sample config shows TURN auth is per-participant
(LiveKit issues short-lived TURN credentials tied to the room/participant
via its own auth, not a shared static TURN secret), so there is no
"shared long-term-credential secret across nodes" concern the way there
would be with a bare coturn deployment — each node's embedded TURN server
independently authorizes using LiveKit's own signaling-issued credentials.

## 6. Autoscaling signals

The official `livekit-helm` server chart ships an HPA block, disabled by
default, driven purely by **CPU utilization**: `enabled: false`,
`minReplicas: 1`, `maxReplicas: 5`, `targetCPUUtilizationPercentage: 60`,
with an optional (commented) memory-based target also available
([`livekit-helm/livekit-server/values.yaml`](https://github.com/livekit/livekit-helm/blob/master/livekit-server/values.yaml));
the EKS example (`replicaCount: 2`, same 1–5/60% HPA policy, 7–7.5 CPU /
1–2Gi memory sizing on 8-core nodes) confirms this is the pattern LiveKit
itself recommends for AWS ([`livekit-helm/examples/server-eks.yaml`](https://github.com/livekit/livekit-helm/blob/master/examples/server-eks.yaml)).
Beyond CPU%, the node selector's `sort_by` options (`sysload`, `cpuload`,
`rooms`, `clients`, `tracks`, `bytespersec`) show what livekit-server itself
tracks per-node for *room-placement* load-balancing, which are reasonable
custom-metric HPA candidates if CPU alone under- or over-reacts: **room
count, client/participant count, track count, and bytes/sec** are all
already-computed per-node signals ([`pkg/routing/selector/regionaware.go`](https://github.com/livekit/livekit/blob/master/pkg/routing/selector/regionaware.go)).
Prometheus additionally exposes `room`/`participant`/`track` gauges (point
8) that a KEDA/Prometheus-adapter-based HPA could scrape directly instead of
relying only on kube CPU metrics — useful because SFU CPU is bursty/muxing-bound
and can lag actual load (matching our chart's own comment that CPU limits
are set generously above requests because throttling degrades call quality
more than protecting the node).

## 7. Recommended production sizing

LiveKit's benchmark doc runs on a **16-core Google Cloud `c2-standard-16`**
compute-optimized instance and reports, per scenario:

| Scenario | Publishers | Subscribers | Total participants | CPU | Bandwidth in/out |
|---|---|---|---|---|---|
| Audio only | 10 | 3,000 | 3,010 | 80% | 7.3 kBps / 23 MBps |
| Large video meeting | 150 | 150 | 300 | 85% | 50 MBps / 93 MBps |
| Livestream (1 pub, many subs) | 1 | 3,000 | 3,001 | 92% | 233 kBps / 531 MBps |

([Benchmarking — LiveKit Docs](https://docs.livekit.io/transport/self-hosting/benchmark/)).
The hard constraint restated here (and matching point 1/2): **"each room
must fit within a single node"** — so per-node sizing must be judged against
your single largest expected room, not aggregate cluster capacity. LiveKit's
own guidance is to load-test against your actual traffic pattern (audio vs.
video mix, bitrate) rather than extrapolate directly from these three
reference scenarios, since CPU cost is dominated by media-muxing patterns
specific to your call shapes.

## 8. Alertable Prometheus metrics

livekit-server exposes Prometheus metrics on `/metrics` at the configured
`prometheus_port` (our chart already sets this to `6789`, matching upstream's
`prometheus_port` config field). No special LiveKit-side integration is
needed for Grafana Cloud/Mimir beyond a standard Prometheus scrape/remote-write
target. Metric names found directly in the telemetry source (namespaced
under `livekit_<subsystem>_<name>`, verified against raw source — see
verification note at the end of this section):

- **Room/participant/track load** (`pkg/telemetry/prometheus/rooms.go`):
  `room` subsystem `total` (active room count) and `duration_seconds`;
  `participant` subsystem `total`; `track` subsystem `published_total` /
  `subscribed_total` (gauges by kind) and `publish_counter` /
  `subscribe_counter` (attempt/success/failure/cancel counters); `session`
  subsystem `join_latency_ms`, `start_time_ms`, `duration_ms`;
  `peer_connection` subsystem `state` (transitions by transport/state) —
  useful for alerting on **participant join failures** via the
  subscribe/publish failure counters and on abnormal peer-connection state
  churn ([`pkg/telemetry/prometheus/rooms.go`](https://github.com/livekit/livekit/blob/master/pkg/telemetry/prometheus/rooms.go)).
- **Call-quality / RTP health** (`pkg/telemetry/prometheus/packets.go`):
  packet `total`/`bytes` counters by direction; **`nack` `total`**, **`pli`
  `total`**, **`fir` `total`** counters; **`packet_loss` `total`** and
  **`packet_loss` `percent`** (histogram); `packet_out_of_order` `total` and
  `percent`; jitter (`us`), RTT (`ms`), forward-path `latency`/`jitter`
  gauges, and a forward-latency histogram in nanoseconds — these map
  directly to the "packet loss, NACK/PLI rates, retransmission rates"
  alerting the issue asks about ([`pkg/telemetry/prometheus/packets.go`](https://github.com/livekit/livekit/blob/master/pkg/telemetry/prometheus/packets.go)).
- **Subjective quality** (`pkg/telemetry/prometheus/quality.go`): a `rating`
  histogram (buckets 0–2, LiveKit's internal poor/good/excellent scale) and
  a `score` histogram (buckets across 1.0–4.5) — good top-line SLO signal
  for alerting on aggregate call-quality degradation
  ([`pkg/telemetry/prometheus/quality.go`](https://github.com/livekit/livekit/blob/master/pkg/telemetry/prometheus/quality.go)).
- Community documentation independently corroborates `livekit_room_count`,
  `livekit_participant_count`, and calls out `livekit_packet_loss_ratio` /
  `livekit_packet_loss_percent` as the single most important quality metric
  to alert on ([Monitoring & security hardening — LiveKit Academy](https://www.livekit-academy.com/courses/self-hosting/monitoring-security)) —
  treat this as secondary corroboration, not the source of truth; the
  literal metric name strings above come from the telemetry source files.

No LiveKit-specific ingestion path is required: standard Prometheus
scrape config (or remote-write to Mimir) against each pod's
`prometheus_port` is sufficient — the same pattern already used for the
`/metrics` endpoint conventions elsewhere in this repo's observability
stack.

**Verification status**: the `Namespace` / `Subsystem` / `Name` components
above were re-read from the raw source files
([`packets.go`](https://raw.githubusercontent.com/livekit/livekit/master/pkg/telemetry/prometheus/packets.go),
[`rooms.go`](https://raw.githubusercontent.com/livekit/livekit/master/pkg/telemetry/prometheus/rooms.go))
and match. Namespace is `livekit` throughout; `packets.go` declares the
subsystem/name pairs `packet/total`, `packet/bytes`, `nack/total`,
`pli/total`, `fir/total`, `packet_loss/total`, `packet_loss/percent`,
`packet_out_of_order/total`, `packet_out_of_order/percent`, `jitter/us`,
`rtt/ms`, `participant_join/total`, `connection/total`, `forward/latency`,
`forward/jitter`, `forward_latency/ns`. Note that Prometheus composes the
exposed series as `<namespace>_<subsystem>_<name>`, so e.g. `nack/total`
surfaces as `livekit_nack_total` and `packet_loss/percent` as
`livekit_packet_loss_percent`. Confirm the final rendered names against a
live `/metrics` scrape of our own deployment before committing alert rules,
since a version bump can add or rename series.

## Delta from our current single-node chart

| Finding | Chart change needed |
|---|---|
| Redis required for any multi-replica mode (§1) | New `redis:` values block (address/sentinel/cluster/TLS) threaded into `configmap.yaml`'s `$config`; new optional Secret/ExternalSecret for Redis auth; document that this Redis may be (and per LiveKit's own docs, must be) the same instance used by the egress service from issue #1023 |
| Room-to-node routing needs no chart change (§2) | None — purely a livekit-server internal behavior once Redis is configured; no new chart surface, but validations.yaml's guard on `replicaCount==1` currently forecloses ever exercising it |
| No live migration; Drain()-based shutdown (§3) | Chart needs a `lifecycle.preStop` hook (currently absent from `deployment.yaml`) that signals not-ready / triggers drain before SIGTERM: a plain `sleep`+readiness-flip or an explicit CLI hook. `terminationGracePeriodSeconds` should become a tunable sized per-deployment SLA rather than the fixed 18000s baked into `values.yaml` today |
| Surge-then-drain rollout needed for zero-downtime with NodePort media (§4) | Flip `deploymentStrategy` in `values.yaml` from `maxUnavailable: 1, maxSurge: 0` to `maxUnavailable: 0, maxSurge: 1` (or similar) once `replicaCount>1` is possible; requires one-pod-per-node scheduling (`podAntiAffinity` — currently `affinity: {}` empty) so surge pods land on distinct nodes for NodePort port reuse |
| `validations.yaml` hard guard | The `{{- if ne (int .Values.replicaCount) 1 -}}{{- fail ... -}}{{- end -}}` block must be relaxed/replaced with a guard that instead *requires* Redis config to be present when `replicaCount>1`, mirroring the comment already in that file ("Going multi-replica requires a LiveKit setup with Redis-backed room distribution and node selection — not just bumping replicaCount") |
| TURN HA (§5) | If keeping embedded TURN, no new shared-secret plumbing needed (auth is per-participant via LiveKit signaling) — but `nodePorts.turnUdp`/`rtc-nodeport-service.yaml` need per-node NodePort uniqueness reconsidered once `replicaCount>1` (today's fixed `nodePorts.rtc.tcp`/`udp` values assume one pod cluster-wide); TLS port likely needs an L4 passthrough Service/LB fronting all replicas rather than a single NodePort |
| Autoscaling (§6) | New optional `autoscaling:` HPA block in `values.yaml` (mirroring upstream `livekit-helm`'s `enabled/minReplicas/maxReplicas/targetCPUUtilizationPercentage`), gated behind `replicaCount` no longer being pinned to 1 |
| Sizing (§7) | Chart's `resources` defaults (500m/512Mi request, 2 CPU/2Gi limit) are far below LiveKit's own 16-core benchmark node; no chart-mechanical change required, but HA rollout planning should size node/pod resources per expected max-room shape, not just bump replica count |
| Metrics/alerting (§8) | No chart change required — `livekit.prometheus_port` is already exposed and scraped; only Grafana/Mimir alert-rule additions targeting the metric names above are needed, outside this chart |
