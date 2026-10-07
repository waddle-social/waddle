# Waddle alerts-as-code

Alert rules live here as plain mimirtool/lokitool rule files and are
the single source of truth for Grafana Cloud alerting (spec #1323,
issue #1324):

- `mimir/` — PromQL rules evaluated by the Grafana Cloud **Mimir
  ruler** against the metrics Alloy ships.
- `loki/` — LogQL rules evaluated by the Grafana Cloud **Loki ruler**
  against the service log streams.
- `notification-policy.yaml` — the severity routing tree (critical →
  `waddle-pager`, warning → `waddle-digest`). Applied by the operator;
  receivers are contact points configured once in Grafana Cloud and
  never live in this repo.

## Pipeline

- **Every PR** touching `rules/**` runs `mimirtool rules lint` and
  `lokitool rules lint` (the `pullRequest` pipeline in `../env.cue`).
  A broken rule file fails the PR, not the pager.
- **Every main push** syncs the trees to the Grafana Cloud rulers via
  `mimirtool rules sync` / `lokitool rules sync` (the `rulesSync` task
  in the `default` pipeline). Sync is authoritative: rules deleted
  here are deleted from the ruler (scoped to the `waddle` namespace).

## Required credentials (operator setup, once)

`rulesSync` resolves its credentials from **1Password** through cuenv's
OnePassword contributor (the same mechanism chat CI uses for
Cloudflare/Faro; the repo-level `OP_SERVICE_ACCOUNT_TOKEN` GitHub
secret already exists). Create one item in the `waddle-production`
vault named **`Grafana-Cloud-Alerting`** with these fields:

| Field | Meaning |
|---|---|
| `mimir-address` | Mimir ruler API base URL for the stack (e.g. `https://mimir-prod-XX.grafana.net`) |
| `mimir-tenant-id` | Mimir tenant (stack metrics instance id) |
| `loki-address` | Loki ruler API base URL |
| `loki-tenant-id` | Loki tenant (stack logs instance id) |
| `ruler-token` | Cloud access-policy token with `rules:read`/`rules:write` on both |

## Adding a rule

1. Add it to the matching concern group (or a new group named
   `waddle-<concern>`) in `mimir/` or `loki/`.
2. Every rule carries `severity` (`critical` | `warning`) and
   `team: waddle` labels, a `summary` annotation, and a `runbook`
   annotation (link the tracking issue when the rule fires on a known
   bug — see SmUnackedEvictions and ClaimStorm).
3. Derive the threshold from measured data and say so in the runbook:
   the P0 set's numbers come from the 2026-07-11/12 production audits
   (heartbeat >60s vs ~10s healthy; error-log rate >25/15m ≈ 10×
   baseline; ClaimStorm >30/30m vs 0 healthy; evictions >100/h
   sustained 2h; zero-tolerance for poison pills, dead-letters, and
   pause timeouts).
4. Open a PR — lint runs automatically; sync happens on merge.

## Known-firing rules

`SmUnackedEvictions` (until #1316) and `ClaimStorm` (until
#1294/#1295) fire on known-open bugs by design — an honest pager
over a silent one. Their runbook annotations link the issues; tighten
or quiet them when the fixes land.

## Legacy-name aliases (#1330 contract phase)

The `waddle-aliases-*` groups in `mimir/waddle-reliability.yaml`
record every retired
`waddle_*` text-family name from its OTel successor's translated
series (`xmpp_*_total`, `waddle_messages_delivered_total`,
`xmpp_connections_active`), so dashboards and the alert rules above
keep answering after the text renderer's deletion. Notes:

- `reason`-labeled families (`waddle_push_suppressed_total`,
  `waddle_push_outbox_retry_scheduled_total`) preserve the label.
- `waddle_messages_per_second` is now a 1m-window rate recording,
  not the old "messages in the last full second" gauge — smoother,
  same intent.
- `waddle_push_suppressed_unknown_reason_total` is retired without an
  alias: the sealed reason enum made it structurally unreachable and
  it was permanently 0.
- `waddle_room_count` was retired earlier (#1136) without an alias: it
  was dead-driven (always 0) and the live signal is the
  `xmpp_muc_rooms_active` gauge.
- The source names assume Grafana Cloud's OTLP→Prometheus name
  normalization (unit annotations dropped, `_total` on monotonic
  sums). Verify against live series before relying on a new alias;
  see the ClusterHeartbeat runbook for the same caveat.

## Ingress alert regression tests

`tests/ingress.test.yaml` evaluates the checked-in rules with synthetic series
using [Prometheus rule tests](https://prometheus.io/docs/prometheus/latest/configuration/unit_testing_rules/).
With `promtool` on PATH, run `sh rules/tests/run.sh` from `infrastructure/waddle.cloud`.
The runner removes only Mimir's top-level `namespace` upload metadata in a
temporary copy; it never edits or uploads the source rules.

Alternatively, from `infrastructure/waddle.cloud`, use an isolated test container:

```sh
docker run --rm --network none --entrypoint /bin/sh \
  -v "$PWD/rules:/rules:ro" prom/prometheus:v3.5.0 /rules/tests/run.sh
```

The suite covers normal live unresolved attempts, flat aged obligations, the
age threshold and hold/reset/resolution, primary failover, missing age telemetry,
and independent per-kind backlog growth. Run it when changing ingress alerts;
the existing PR pipeline lints rule syntax but does not run these behavioral tests.
