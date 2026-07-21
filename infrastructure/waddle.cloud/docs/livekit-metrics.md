# LiveKit SFU metrics: starter queries

Grafana Alloy scrapes each LiveKit pod every 30 seconds. In Grafana Cloud, start
with `{job="livekit-sfu"}`; `instance` is the Kubernetes pod name. LiveKit also
adds `node_id` and `node_type` to its native metrics.

These queries match the metrics emitted by the deployed LiveKit v1.11.0 server:
the [room and participant gauges][rooms-source] and the [packet-loss and byte
counters][packets-source].

## Active rooms

Cluster total:

```promql
sum(livekit_room_total{job="livekit-sfu"})
```

To spot an imbalanced or unhealthy SFU, break the same gauge out by pod:

```promql
sum by (instance) (livekit_room_total{job="livekit-sfu"})
```

## Active participants

Cluster total:

```promql
sum(livekit_participant_total{job="livekit-sfu"})
```

Per pod:

```promql
sum by (instance) (livekit_participant_total{job="livekit-sfu"})
```

## Packet loss

Lost packets per second, split by media direction, track source, and media type:

```promql
sum by (direction, source, type) (
  rate(livekit_packet_loss_total{job="livekit-sfu"}[5m])
)
```

Approximate p95 of LiveKit's observed packet-loss percentage samples:

```promql
histogram_quantile(
  0.95,
  sum by (le, direction, source, type) (
    rate(livekit_packet_loss_percent_bucket{job="livekit-sfu"}[5m])
  )
)
```

The p95 is computed from LiveKit's fixed percentage buckets, so it is an
approximation rather than a packet-weighted loss ratio. The lost-packets rate is
usually the better alert input; use the percentile to understand severity.

## Bitrate

SFU media bitrate in bits per second, split into traffic entering and leaving
the SFU and excluding retransmissions:

```promql
8 * sum by (direction) (
  rate(livekit_packet_bytes_total{
    job="livekit-sfu",
    transmission="initial"
  }[5m])
)
```

To graph total RTP packet bitrate observed by the SFU, including
retransmissions, remove the `transmission="initial"` selector. This is not
NIC-level throughput and does not include every transport or protocol overhead.
To find a hot pod, add `instance` to the `sum by (...)` grouping.

## Query caveats

- These native server metrics are node-level aggregates. They do not carry a
  room name, participant identity, or track ID, so they cannot produce
  per-call or per-user QoS views.
- LiveKit adds `direction`, `source`, `type`, and `country` only where relevant.
  Use Grafana Explore to inspect the values present before adding filters; enum
  spelling can change between LiveKit releases.
- A newly started or idle pod may not expose every labeled counter series until
  it has handled matching traffic. `rate(...[5m])` also needs at least two
  samples, so a new series can be temporarily absent.
- Re-check the upstream definitions after a LiveKit image upgrade. The metric
  namespace (`livekit`) and constant node labels are initialized in LiveKit's
  [Prometheus registry setup][node-source].

[rooms-source]: https://github.com/livekit/livekit/blob/v1.11.0/pkg/telemetry/prometheus/rooms.go
[packets-source]: https://github.com/livekit/livekit/blob/v1.11.0/pkg/telemetry/prometheus/packets.go
[node-source]: https://github.com/livekit/livekit/blob/v1.11.0/pkg/telemetry/prometheus/node.go
