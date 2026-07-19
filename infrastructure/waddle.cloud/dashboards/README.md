# Waddle dashboards-as-code

Grafana dashboards live here as reviewable JSON and are the single source of
truth for the Waddle folder in Grafana Cloud (issue #1327). Each file has a
stable dashboard UID, so publishing it again updates the existing dashboard
and preserves links and history.

## Pipeline

- **Every PR** touching `dashboards/**` runs
  `scripts/validate-dashboards.sh` (the `dashboardsLint` task in the
  `pullRequest` pipeline in `../env.cue`). Invalid JSON, missing required
  fields, duplicate UIDs, and unexpectedly empty panel lists fail the PR.
- **Every main push** touching `dashboards/**` ensures the **Waddle** Grafana
  folder exists and posts every dashboard to `/api/dashboards/db` (the
  `dashboardsSync` task in the `default` pipeline). Stable UIDs and
  `overwrite: true` make the sync idempotent. The sync then prunes any
  dashboard left in the Waddle folder whose uid no longer appears in
  `dashboards/*.json`, so deleting (or re-uid-ing) a board here removes the
  old one from Grafana on the next push.
- A production rollout also posts an organization annotation tagged `deploy`
  and `waddle` after the GitOps artifact push (`deployAnnotation` depends on
  both push tasks, so a failed push never gets a deploy marker). The overview
  dashboard queries the `deploy` tag and renders those events as vertical
  markers.

## Dashboard status

| File | Dashboard | Status |
|---|---|---|
| `01-overview.json` | Waddle overview | Panel-complete. **Message rate by kind ships dark** until #1320 wires the metric label. |
| `02-delivery-reliability.json` | Waddle delivery reliability | Panel-complete. |
| `03-clustering.json` | Waddle clustering | Panel-complete. **Claims released/abandoned on drain ships dark** until #1295 lands. |
| `04-client-experience.json` | Waddle client experience | Panel-complete; all signals come from Faro in Loki. |
| `05-calls.json` | Waddle calls | Deliberate board-wide skeleton, marked by the `skeleton` tag, until #1317, #1318, and #1319 land. |
| `06-state-inventory.json` | Waddle server — long-lived state inventory | Migrated from `scripts/` with its existing `waddle-state-inventory` UID. |

Dark panels are real queries that intentionally render no data until their
producer lands. Only a dashboard whose own `tags` array contains `skeleton`
may have an empty `panels` array.

## Required credentials (operator setup, once)

`dashboardsSync` and `deployAnnotation` resolve their credentials from
**1Password** through cuenv's OnePassword contributor. Create one item in the
`waddle-production` vault named **`Grafana-Cloud-Dashboards`** with these
fields:

| Field | Meaning |
|---|---|
| `url` | Grafana Cloud stack base URL (for example, `https://waddlesocial.grafana.net`) |
| `service-account-token` | Grafana Cloud service-account token with dashboard write and annotation write access on the stack, plus permission to read/create the `Waddle` folder |

The existing `Grafana-Cloud-Alerting` ruler token is intentionally separate:
its Mimir/Loki ruler scopes cannot publish dashboards or annotations.

## Adding a dashboard

1. Add one JSON file with a unique, stable, kebab-case `uid`, a non-empty
   `title`, a `tags` array containing `waddle`, and a non-empty `panels` array.
2. Add the Prometheus and/or Loki datasource template variables used by its
   panels. Keep `schemaVersion` at the repository's current Grafana schema.
3. Open a PR. Dashboard lint runs automatically; sync happens after merge to
   `main` when the dashboard inputs changed.

