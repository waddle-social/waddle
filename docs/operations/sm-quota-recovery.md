# Stream management quota recovery verification

Follow-up tracking: [#1971](https://github.com/waddle-social/waddle/issues/1971),
[#1972](https://github.com/waddle-social/waddle/issues/1972), and
[#1427](https://github.com/waddle-social/waddle/issues/1427).

PR #1970 adds durable promotion custody and quota recovery instrumentation.
Operational acceptance requires deployment and eligible traffic; passing local
tests alone does not establish recovery of historical queues.

## Verification plan

1. Establish the server deployment revision and rollout interval. Compare equal
   windows before and after rollout for missing-target warnings, quota rejection
   increments, and pending-delivery age-out increments.
2. Classify historical senderless rows using aggregate metadata and XML envelope
   shapes from [the producer audit](../sm-promotion-producer-audit.md). Never
   publish message contents, recipient addresses, or account credentials.
3. With designated test accounts, fill a recipient queue while its local sender
   is offline. Reconnect the sender and verify exactly one `service-unavailable`
   error, including after another reconnect.
4. Record configured retention, eligible row counts, observation windows, and
   counter deltas. Verify expired rows free recipient quota. Zero age-outs with
   no eligible rows is inconclusive.
5. Record evidence and remaining access gaps on #1427. Propose any historical
   cleanup separately, beginning with aggregate dry-run counts.

The #1972 implementation must keep an expired resume's recorded queue in durable
non-resumable inventory before releasing custody. Regression coverage will
exercise expiry during completion, restart before promotion, exact fencing,
cancellation, successful resume, and portable receipt reclamation on SQLite and
PostgreSQL.

## Production observations, 2026-10-17

The server publishing job for revision
`b69231b5d0ec4b39dac53cae69c4a862a334bc28` is
[117384080586](https://github.com/waddle-social/waddle/actions/runs/39027511029/job/117384080586).
The deployed server image digest is
`sha256:c432221568573376a7f37312814ef3deacf3cd7229aa3ca346038f4877c5026a`.
Read-only Kubernetes inspection verified that deployment generation 600 was
observed, both replicas were ready with that image and `WADDLE_GIT_SHA`, and
`NewReplicaSetAvailable` was reported at 11:28:25 UTC. The 11:11:40 UTC GitHub
deployment record belongs to the chat client and is not server rollout evidence.

At 11:29:49 UTC, a read-only transaction on the primary database found:

| Aggregate | Rows |
| --- | ---: |
| All pending delivery | 0 |
| Pending delivery for the affected recipient | 0 |
| Pending delivery older than 30 days | 0 |
| Canonical SM sessions, all recipients | 1 |
| Canonical SM unacked entries, all recipients | 4 |

A second aggregate at 11:32:50 UTC found zero canonical sessions and zero
unacked rows for the affected recipient. The namespace has no managed Backup,
ScheduledBackup, or VolumeSnapshot objects, and the database cluster reports no
managed recovery point. This does not rule out externally retained dumps.

No message contents or row identities were published. An empty current queue
cannot establish which producer created the July rows, when they disappeared,
or whether retention removed them. Historical attribution requires a snapshot
from the affected period. Current counts justify no historical cleanup.

The effective retention for this rollout is the 30-day source default, clamped
to 1–365 days. The `WADDLE_PENDING_DELIVERY_MAX_AGE_DAYS` key is absent from
explicit container environment, the imported ConfigMap, and each of the four
imported Secrets. Only that exact non-sensitive key was queried; unrelated
Secret values were not retrieved. The observation has no rows eligible for
retention, so zero age-outs is expected to be inconclusive.

### Reproducing the comparison

Use the `grafanacloud-prom` datasource and evaluate these instant queries at the
end of each equal window:

```promql
sum(
  increase(waddle_pending_delivery_quota_exceeded_total{job="waddle-server"}[15m])
  and on(job,instance)
  max by(job,instance) (
    max_over_time(target_info{
      job="waddle-server",deployment_environment="production"
    }[15m])
  )
)
```

Substitute `waddle_pending_delivery_aged_out_total` for age-out increments.
The counters expose only `job` and `instance`, so `target_info` qualifies the
observed instances as production. The baseline instances matched production;
no counter instances lacked matching production metadata in that window.

Use the `grafanacloud-logs` datasource for the missing-target warnings:

```logql
sum(count_over_time(
  {service_name="waddle-server",deployment_environment="production"}
  |= "bounce target JID missing" [15m]
))
```

The equal windows on 2026-10-17 returned:

| Observation | Before, 11:12:30–11:27:30 UTC | After, 11:28:30–11:43:30 UTC |
| --- | ---: | ---: |
| Missing-target warnings | 0 | 0 |
| Quota-rejection increase | 0 | 0 |
| Age-out increase | 0 | 0 |
| Server log lines in the window | 602 | 95 |

The post window begins after verified rollout readiness. Prometheus increases are estimates;
pod churn and missing scrapes can hide increments. A quiet comparison with no
quota pressure establishes observation, not a successful rejection or retention
exercise. Repeat across equivalent busy rollout windows before attributing an
improvement to this deployment.

### Remaining evidence

- Historical snapshot: classify senderless inbox query snapshots, inbox pushes,
  and MAM result envelopes against the producer audit using aggregate counts.
- Designated live sender and recipient: verify one terminal error after sender
  reconnect, acknowledge it, and verify no replay on a second reconnect.
- Eligible designated rows: observe the configured retention cutoff, row counts,
  freed quota, and exported age-out counter together. Do not lower global
  retention or backdate ordinary user rows for this probe.

Keep #1971 and #1427 open until these checks are supported by evidence.
