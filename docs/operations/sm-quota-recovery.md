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

## Recorded evidence

The [2026-10-17 production observations on #1427](https://github.com/waddle-social/waddle/issues/1427#issuecomment-6233505522)
record the rollout revision, image digest, configuration, database aggregates,
equal telemetry windows, and acceptance gaps. Keep dated snapshots on the issue;
this runbook describes how to repeat the verification.

## Comparing telemetry windows

Use the `grafanacloud-prom` datasource and evaluate these instant queries at the
end of each equal window:

```promql
sum(
  increase(waddle_pending_delivery_quota_exceeded_total{job="waddle-server"}[15m])
  and on(job,instance)
  max by(job,instance) (
    max_over_time(target_info{
      job="waddle-server",deployment_environment="production"
    }[30m])
  )
)
```

Substitute `waddle_pending_delivery_aged_out_total` for age-out increments.
The counters expose only `job` and `instance`, so `target_info` qualifies the
observed instances as production. Compare the qualified instance count against
all counter instances so missing metadata cannot silently omit a replica. The
metadata lookup extends to 30 minutes because a draining replica's final counter
scrape can outlive its final `target_info` scrape.

Use the `grafanacloud-logs` datasource for the missing-target warnings:

```logql
sum(count_over_time(
  {service_name="waddle-server",deployment_environment="production"}
  |= "bounce target JID missing" [15m]
))
```

Start the post window after verified rollout readiness and use the same length
for the baseline. Prometheus increases are estimates;
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
