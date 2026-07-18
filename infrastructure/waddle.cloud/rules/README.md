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

## Required GitHub Actions secrets (operator setup, once)

| Secret | Meaning |
|---|---|
| `GRAFANA_CLOUD_MIMIR_ADDRESS` | Mimir ruler API base URL for the stack (e.g. `https://mimir-prod-XX.grafana.net`) |
| `GRAFANA_CLOUD_MIMIR_TENANT_ID` | Mimir tenant (stack metrics instance id) |
| `GRAFANA_CLOUD_LOKI_ADDRESS` | Loki ruler API base URL |
| `GRAFANA_CLOUD_LOKI_TENANT_ID` | Loki tenant (stack logs instance id) |
| `GRAFANA_CLOUD_RULER_TOKEN` | Cloud access-policy token with `rules:read`/`rules:write` on both |

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
