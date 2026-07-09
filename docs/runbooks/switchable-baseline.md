# Switchable-alternative baseline collection

This runbook collects the live telemetry evidence required by Gate 0 of the
[switchable-alternative program](../planning/switchable-alternative.md). The
machine-readable source of truth for signal names, queries, bounded attributes,
and limitations is
[`switchable-baseline-signals.json`](../observability/switchable-baseline-signals.json).
If this document and the catalog disagree, stop and fix the disagreement before
collecting evidence.

Gate 0 is not ready merely because the metrics exist, a repository contract
passes, or a dashboard looks healthy. A valid telemetry baseline requires:

1. an automated Prometheus artifact from a fixed live window of at least 60
   minutes;
2. all four catalogued Faro normalized aggregate JSON artifacts from that same
   window;
3. hashes that bind the immutable artifacts to a release tuple containing the
   deployed 40-character server and web Git commits; and
4. a reviewed `telemetry-baseline` record in
   [`gate-evidence.json`](../product/gate-evidence.json).

A Prometheus artifact without the Faro aggregates is partial evidence and cannot
complete the `telemetry-baseline` evidence kind or make Gate 0 `ready`.

## Signal truth

Do not give these signals stronger names or interpretations than the catalog
allows.

| Signal | What it establishes | What it does not establish |
| --- | --- | --- |
| `server-deployment-identity-targets` | The complete build-revision set and count of targets with a build-info scrape in every preceding 60-second interval in the exact Prometheus job, environment, cluster, and namespace scope. | Readiness or correct journeys. |
| `server-process-start-continuity` | The aggregate process-generation marker remains constant while the exact target count and revision remain fixed from the one-hour pre-window through the frozen end. | An instance inventory or proof against a pathological aggregate collision. |
| `connection-registry-entries` | Connection-registry entries across replicas. | Connected resources, unique humans, or active members; clustered resources can have a physical entry and a relay mirror. |
| `room-registry-entries` | Production room-registry entries across replicas, sampled every 15 seconds. | Engagement, occupied rooms, or only live actors; cleanup and sampling can briefly leave stale entries. |
| `room-registry-sample-freshness` | The oldest successful room-registry sample remains between zero and 60 seconds old. | That every registry entry represents a live room actor. |
| `xmpp-sasl1-terminal-attempts` | Completed SASL1 OAUTHBEARER and SCRAM-SHA-256 exchanges by the bounded `mechanism` and `outcome` values. Explicit aborts and unfinished exchanges superseded by a new RFC 6120 `<auth/>` record `cancelled`; the replacement's terminal outcome is separate, so a replacement flow intentionally contributes two exchange outcomes. | Pre-handler frame rejection, challenge exchanges or connections abandoned before a terminal response, SASL2, or browser OAuth callback success. |
| `message-archive-attempts` | Typed sender-pass DM attempts and room archive storage attempts by terminal outcome, including the bounded `chain_invalid` invariant failure. | Recipient visibility or a complete accepted-message denominator. Room attempts include server-authored system messages and are not exclusively sender-side; direct `chain_invalid` remains zero; a committed idempotent retry may have found an existing row. |
| `live-delivery-channel-outcomes` | Connection-registry outbound-channel enqueue operations. | A logical message, resource-delivery, XMPP handling, or client-visibility count; clustered delivery can record a relay-forwarder hop and a physical-socket hop. |
| `loss-corruption-safety` | Occurrences on the named permanent-loss, archive-chain invariant, replay-gap, ambiguous-drop, and corruption surfaces. | Proof that every accepted message was delivered. Zero means only that these named counters stayed at zero. |
| `push-pipeline-outcomes` | XEP-0357 candidate and server publication-job outcomes. | Provider acceptance, device receipt, notification display, or a click. |
| `browser-auth-bootstrap` | Browser session bootstrap terminal outcome and duration. | Unique users, identity-provider behavior, or an account-level success rate. |
| `browser-message-ack-latency` | Time from client send bookkeeping, after the WASM send returns, to XMPP acknowledgement. | Send-to-visible latency. An XEP-0198 acknowledgement represents handling or responsibility, not pixels shown to a person. |
| `browser-session-lifecycle` | Successful ready sessions split between fresh bind and XEP-0198 resume. | A reconnect denominator; a fresh bind may be an initial connection or resume fallback. |
| `browser-reconnect-duration` | Time from a reconnecting status to the next online status. | Terminal reconnect failures, which produce no duration sample. |

XEP-0352 client-state indications are operational hints, independent of
presence. They must not be used to infer, label, or expose a person's activity.

## Privacy and cardinality guardrails

Baseline artifacts must contain aggregate series only. Do not query, export,
group, label, or annotate by any account, channel, email, JID, message, peer,
provider, room, session, stanza, token, URL, user, or any identifier derived
from those values. Attribute values must come from the closed sets in the
catalog, and no attribute may have more than 16 possible values.

Never include message content, XMPP payload XML, JIDs, room names or IDs,
message/stanza/stream/session/account IDs, access tokens, authorization codes,
redirect URIs, or query strings. Do not place credentials in commands that will
be committed, terminal captures, artifact metadata, or Markdown. Logs and
traces are outside this baseline until they have a separate PII review; do not
attach them as a substitute for the catalogued aggregate signals.

Raw Faro events or exports are restricted operational material, not Gate 0
artifacts. Keep them only in the approved access-controlled system or encrypted
temporary workspace for the minimum time needed to normalize and review them.
Never commit, attach, paste, or copy them into `docs/evidence`, even when they
appear identifier-free. Only the strict aggregate JSON envelopes described
below may enter the repository.

When an operator needs context, record bounded operational facts such as the
intended replica count, deployment environment, and release SHAs. Describe test
traffic as a scenario and count, never by participant identity.

## Prerequisites

Before choosing the collection window, verify all of the following:

- The intended 40-character server Git commit is deployed on every Waddle
  replica, and the intended 40-character web Git commit identifies the browser
  bundle producing Faro signals. The two commits may differ.
- Prometheus can scrape every intended `waddle-server` target and the expected
  replica count is known.
- Every scraped series has authoritative `job`, `namespace`,
  `deployment_environment`, and `cluster` target labels. The hosted Alloy
  configuration attaches these labels and uses the pod UID as `instance`.
  Self-hosted collectors must provide the same target-label contract with
  bounded, non-`unknown` values and scrape with `honor_labels=false`, preserving
  conflicting build-info values as `exported_*`; labels exposed by an
  individual metric do not substitute for target labels.
- The server `/metrics` endpoint exposes the Waddle metric families named in
  the catalog, including the always-rendered process-start marker.
- The Alloy/OTLP path used by the deployment is healthy where applicable. This
  does not replace the Prometheus scrape check.
- The web client was built with the Faro collector enabled, reports the
  `webCommit` from the frozen release tuple, and the configured Grafana project
  is receiving events and measurements.
- You have read-only Prometheus credentials in
  `GRAFANA_PROMETHEUS_URL`, `GRAFANA_PROMETHEUS_USER`, and
  `GRAFANA_PROMETHEUS_API_KEY`. The URL must use HTTPS and address the
  Prometheus-compatible API; do not include credentials in it.
- You can query all four manual-export signals and produce machine-readable
  aggregate results without downloading raw events into the repository.
- The Faro read/query API is configured separately from `PUBLIC_FARO_URL`.
  `PUBLIC_FARO_URL` is a browser ingest endpoint and is never accepted as the
  evidence read source. The trusted collector must bind the credential-free
  query locator and data-source UID by SHA-256.
- Replica count provenance is available from the same trusted collection. A
  hosted capture binds the Kubernetes Deployment name, namespace, hashed UID,
  exact/observed generation, replica spec, and rendered configuration digest.
  A self-hosted capture binds both the deployment configuration digest and a
  hashed operator artifact. A bare operator-entered replica number is invalid.
- Immutable release provenance is available for the exact deployed artifact
  set: the server image, Helm chart, GitOps OCI artifact, all five canonical
  extension OCI modules, the web build artifact, and the web deployment
  identity. Commit strings embedded in a running binary or browser bundle are
  identity signals, not publication provenance by themselves.

For Kubernetes provenance, the trusted workflow must read the Deployment
directly from the Kubernetes API. `uidSha256` covers the exact UTF-8
`metadata.uid`. `configSha256` covers compact canonical JSON with
lexicographically ordered keys for `apiVersion`, `metadata.name`,
`metadata.namespace`, `metadata.generation`, `status.observedGeneration`,
`spec.replicas`, `spec.selector`, and `spec.template`; no mutable status or
secret value is included beyond the named generation fields. The workflow
derives both hashes after fetching and verifies that generation equals observed
generation. For self-hosted provenance, `configSha256` covers the exact bytes
of the deployment configuration and `operatorArtifactSha256` covers the exact
immutable operator-produced deployment export. The future trusted workflow
must fetch/read and hash these bytes itself; caller-supplied digests, placeholder
digests, and pre-hashed workflow inputs are forbidden.

For a hosted release, the same trusted workflow must read the live Kubernetes
and Flux objects directly. It derives the GitOps OCI digest from
`OCIRepository/flux-system/waddle-server.status.artifact.digest`, derives the
Kustomization source digest from the terminal `sha256:` digest in
`Kustomization/flux-system/infra-waddle-server.status.lastAppliedRevision`, and
requires both values to agree. It derives the chart digest from
`OCIRepository/waddle/waddle-server-chart.status.artifact.digest`. Every object
must be `Ready`, its `status.observedGeneration` must equal
`metadata.generation`, and the HelmRelease and Deployment must be fully ready.
The server image digest comes from the `waddle-server` container image in the
live Deployment. The five extension digests come from the exact
`WADDLE_EXTENSIONS_JSON` value in the ConfigMap named by that Deployment's
`envFrom`; the workflow parses the closed canonical module-name set and then
checks the live pod template's ConfigMap checksum. It never accepts operator
parameters containing these observed values.

The web publication workflow must hash the exact uploaded build bytes and the
credential-free Cloudflare Workers deployment identity returned by the
deployment operation. The evidence collector independently reads the live web
deployment, hashes that identity, and confirms that both it and the served web
commit match the publication. Server and web publication attestations use
separate canonical artifact-set subjects: the server workflow binds
`serverCommit` plus the image, chart, GitOps, and five extension digests; the
web workflow binds `webCommit` plus the web artifact and deployment identity.
The collection subject signs both typed sets and their observed deployment
state.
- Server, browser, and operator clocks are synchronized. All artifact
  timestamps use UTC.
- The window will receive enough privacy-safe test or dogfood traffic to
  exercise authentication, DM and room archiving, live delivery, browser
  session setup, and reconnect/resume. Push data may legitimately be zero, but
  the zero result must still be present.

Prefer a window with no deployment or configuration change. The deployment
must already have been stable for the catalog's maximum one-hour PromQL
lookback plus the identity query's preceding 60-second history interval before
the frozen start. If a change is unavoidable, reject the window and collect a
new one after the deployment has been stable for that full period; do not
splice results from different releases.

## Freeze the collection window

Choose explicit RFC 3339 UTC timestamps before running any query. The interval
must be at least 60 minutes and all Prometheus and Faro results must use the
same fixed start and end timestamps. Record the values exactly; never move a
boundary after seeing the results.

Example:

```text
start: 2026-07-10T09:00:00Z
end:   2026-07-10T10:00:00Z
```

An exact 60-minute window makes the catalog's one-hour counter increases easy
to compare. PromQL range selectors such as `[1h]` are left-open and
right-closed at each evaluation time, and `increase` extrapolates to the range
boundaries from available scrape samples; it is an estimate, not a literal sum
over an inclusive interval. A longer fixed window is allowed, but the
catalogued one-hour queries remain rolling one-hour views; document that fact
in the review note. Point-in-time gauges are sampled across the complete frozen
window. Signals with the catalogued `collectionLookbackSeconds` are additionally
sampled from one hour before the frozen start through the end. This proves both
the target revision/replica set and a constant aggregate process generation for
every counter range used by the evidence queries.

Record both deployed commits before collection. A monorepo deployment can use
the same SHA for both, but the evidence still records two independently named
values:

```sh
jj log -r @ --no-graph -T 'commit_id ++ "\n"'
```

Each result must be a full 40-character SHA. `serverCommit` must match every
server build-info target and `webCommit` must match the release selected by the
Faro query. A dirty working tree or a local commit that is not one of the
deployed artifacts is not valid evidence.

## Collect the live XMPP capability artifact

Run the native collector during the same frozen window. It authenticates over
WSS with the production `waddle-xmpp-client`, issues XEP-0030 `disco#info`
queries to all ten canonical targets, and writes only target slugs,
category/type identities, and sorted feature namespaces. Resolved account and
room JIDs, identity names, forms, access tokens, endpoint details, and raw XML
are never written to the artifact.

Provision these three values into the named environment variables with the
approved secret/session mechanism. Do not put their values in the command,
shell history, task arguments, or a committed env file:

```text
WADDLE_CAPABILITY_ACCESS_TOKEN
WADDLE_CAPABILITY_ACCOUNT_JID
WADDLE_CAPABILITY_REPRESENTATIVE_MUC_ROOM
```

From the repository root, collect the live export with the same release,
window, and deployment scope used by Prometheus:

```sh
cargo run --manifest-path server/Cargo.toml \
  -p waddle-xmpp-client \
  --bin waddle-capability-collector -- \
  --endpoint wss://waddle.example/xmpp-websocket \
  --origin https://waddle.example \
  --xmpp-domain waddle.example \
  --muc-domain muc.waddle.example \
  --spaces-domain spaces.waddle.example \
  --account-env WADDLE_CAPABILITY_ACCOUNT_JID \
  --representative-muc-room-env WADDLE_CAPABILITY_REPRESENTATIVE_MUC_ROOM \
  --access-token-env WADDLE_CAPABILITY_ACCESS_TOKEN \
  --calls-configured \
  --server-commit 0123456789abcdef0123456789abcdef01234567 \
  --window-start 2026-07-10T09:00:00Z \
  --window-end 2026-07-10T10:00:00Z \
  --job waddle-server \
  --environment production \
  --cluster waddle-cloud \
  --namespace waddle \
  --expected-replicas 2 \
  --target-contract server/disco-target-contract.json \
  --output "$PWD/target/switchable-baseline-inputs/capability/live-disco-export.json"
```

The endpoint must use credential-free WSS on the XMPP domain or one of its
subdomains. The account JID must belong to that exact XMPP domain, and an
optional Origin must be credential-free HTTPS on the endpoint's exact host and
effective port. The fixed window must be at least 60 minutes, and collection
must finish inside it. The collector binds the exact window, server commit,
deployment scope, and SHA-256 of the checked-in target contract into its
output. It refuses an existing output, a traversing or symlinked output path,
duplicate/unsafe discovery values, a missing always-available target,
unexpected official namespaces, responder or root-node correlation failures,
and target-local claims synthesized from another entity's feature set.

The binary can explicitly skip only an unconfigured calls mixer or a missing
representative dynamic entity. The current capability manifest declares
features on both targets, so a complete Gate 0 capture supplies the
representative room environment variable and `--calls-configured`; a skipped
claimed target is valid diagnostic output but cannot pass reconciliation.
The live export is a restricted staging input, not a canonical evidence file.
Leave it under `target/switchable-baseline-inputs`; the deterministic finalizer
consumes the exact checked-in contract, this live export, and
`server/capabilities.toml`, then publishes the contract copy, normalized live
export, reconciliation, and manifest together. Do not copy, hash, reconcile, or
assemble those files by hand. `cuenv task collectCapabilityBaseline` from
`server/` runs the same staging collection contract.

## Collect the Prometheus artifact

The collector reads the Prometheus signals and exact PromQL from the catalog,
uses the frozen interval, rejects missing or non-finite results, sorts the
output deterministically, and emits the review artifacts without writing
credentials:

```sh
bun scripts/collect-switchable-baseline.ts \
  --start 2026-07-10T09:00:00Z \
  --end 2026-07-10T10:00:00Z \
  --server-commit 0123456789abcdef0123456789abcdef01234567 \
  --prometheus-job waddle-server \
  --environment production \
  --cluster waddle-cloud \
  --namespace waddle \
  --expected-replicas 2 \
  --output-dir "$PWD/target/switchable-baseline-inputs/prometheus"
```

Use the exact Prometheus target-label values and the Deployment's intended
replica count. Self-hosted installations choose their own bounded job,
environment, cluster, and namespace values; `unknown` and trailing punctuation
are not valid evidence. The collector injects one fixed selector containing all
four target labels directly into every metric selector. It does not use a
build-info join to scope source metrics.

Before querying Prometheus, the collector verifies that the catalog bytes are
exactly the file stored at the asserted server Git commit. It then queries the
full, unfiltered `waddle_build_info` revision set and aggregate
`waddle_process_start_time_seconds` marker in the requested target-label scope
from one hour before the frozen start through the end. The only accepted build
identity is the asserted server commit and exact exported environment/cluster,
the count must equal the intended replicas at every 60-second step, and the
process-start aggregate must remain constant. Each identity evaluation
uses `count_over_time(...[60s])`, so even a short-lived revision scraped between
adjacent evaluation timestamps produces its own identity series and rejects the
artifact. Mixed, unknown, missing, stale, or surplus revisions reject the
artifact.

Expected staging outputs are:

- `target/switchable-baseline-inputs/prometheus/telemetry-baseline.json`
- `target/switchable-baseline-inputs/prometheus/telemetry-baseline.md`

The JSON becomes the machine-readable input to finalization. The staging
Markdown is an operator preview; the finalizer regenerates the canonical review
from the validated JSON and its exact canonical digest. Both staging files bind the catalog
schema version, exact start and end, deployed server commit, Prometheus job,
environment, cluster, namespace, expected replica count, query text, units,
interpretation, limitations, and returned aggregate series. Re-running the
collector against identical API responses and arguments must produce identical
content.

Review the Prometheus artifact before continuing:

- `server-deployment-identity-targets` exposes exactly the asserted build
  identity and equals the intended replica count at every evaluation step from
  the one-hour pre-window through the frozen end. A zero or partial result means
  at least one target was absent, stale, or reported a different commit,
  environment, or cluster. For an exact one-hour evidence window, this identity
  series and `server-process-start-continuity` each have 121 fixed evaluation
  samples; each identity sample covers its preceding 60 seconds,
  so the earliest evaluation inspects scrape history from 61 minutes before the
  frozen start. Every ordinary evidence-window series has 61 samples, with both
  boundaries included.
- `room-registry-sample-freshness` stays between zero and 60 seconds at every
  evaluation step. An absent or older sample invalidates the window.
- `server-process-start-continuity` has one constant value at all 121 samples.
  Any change means process-local counters could have reset and invalidates the
  entire window, even when the build commit and replica count stayed unchanged.
- Every automated catalog signal is present, including legitimate zero values.
- `live-delivery-channel-outcomes` returns delivered, dropped-full, and
  dropped-closed series separately; all three catalogued metric names must be
  present and both drop outcomes must be zero.
- `loss-corruption-safety` is zero, including
  `archive_chain_invalid`. A non-zero result is an incident to investigate, not
  a value to average away.
- Auth and archive results use only the catalogued closed label values.
- No result contains an identifier-bearing label or prohibited payload.

Do not hand-edit generated JSON, normalize a surprising value away, or rerun
with a more favorable time range. Fix collection defects in the collector;
investigate product defects in the product.

## Export the Faro signals

Use the configured Grafana/Faro data source with the same frozen UTC start and
end. Run aggregate queries that select the exact deployed release and return
only the dimensions allowed by the catalog. If the tool first produces a raw
export, keep that file in an access-controlled temporary location outside the
repository, hash its exact bytes, normalize it, then delete it according to the
operator's retention policy. A raw digest proves which restricted input was
reviewed; it does not make raw events safe to commit.

Create one immutable normalized aggregate for each catalogued signal:

1. `browser-auth-bootstrap`: aggregate `chat.journey.auth` counts and
   `chat.journey.auth.duration_ms` distribution grouped only by `outcome`, whose
   allowed values are `ready`, `signed_out`, `expired`, and `failed`.
2. `browser-message-ack-latency`: aggregate
   `chat.xmpp.message.acked.latency_ms` percentiles grouped only by `kind`, whose
   allowed values are `room` and `dm`.
3. `browser-session-lifecycle`: aggregate `chat.xmpp.session.lifecycle` counts
   grouped only by `type`, whose allowed values are `fresh` and `resumed`.
4. `browser-reconnect-duration`: aggregate
   `chat.xmpp.reconnect.duration_ms` percentiles with no grouping dimension.

The frozen window must contain at least one successful `ready` auth bootstrap,
one DM acknowledgement, one room acknowledgement, one fresh session, one
resumed session, and one completed reconnect duration. A present zero-valued
closed-set row is still required for every other outcome, but it does not
substitute for these positive journey samples.

The normalizer writes only these allowlisted staging files:

```text
target/switchable-baseline-inputs/faro/browser-auth-bootstrap.json
target/switchable-baseline-inputs/faro/browser-message-ack-latency.json
target/switchable-baseline-inputs/faro/browser-session-lifecycle.json
target/switchable-baseline-inputs/faro/browser-reconnect-duration.json
```

The restricted aggregate export consumed by the normalizer is a JSON object
with exactly `schemaVersion`, `query`, `source`, and `rows`. `source` contains
only `sourceId: "waddle-chat"`. `query` is the typed plan from the catalog
materialized with that source ID, the full 40-character web release, deployment
environment, cluster, namespace, and UTC start/end. Prometheus `job` is not a
Faro dimension and must not appear in the query, source, scope, or normalized
artifact. `rows` already contains aggregate closed-set rows, never raw Faro
events. Keep this input outside `docs/evidence`.
Normalize each signal with the reviewed executable:

```sh
bun scripts/switchable-baseline/normalize-faro.ts \
  --input /secure/faro/browser-message-ack-latency.aggregate.json \
  --output "$PWD/target/switchable-baseline-inputs/faro/browser-message-ack-latency.json" \
  --signal-id browser-message-ack-latency \
  --server-commit 0123456789abcdef0123456789abcdef01234567 \
  --web-commit 1111111111111111111111111111111111111111 \
  --deployment-environment production \
  --cluster waddle-cloud \
  --namespace waddle \
  --start 2026-07-10T09:00:00Z \
  --end 2026-07-10T10:00:00Z
```

The normalizer verifies the catalog bytes at `--server-commit`, rejects duplicate JSON
keys, wrong filters, unexpected fields, incomplete closed sets, unsafe strings,
and missing required activity, then hashes the restricted input bytes. It will
not read a restricted input from `docs/evidence` or write to a non-canonical
staging path. It refuses to replace an existing staging output; start a clean
capture directory for every window rather than overwriting an earlier run.

Each file is a single strict JSON envelope. It contains no raw event rows, CSV,
screenshots, logs, traces, exemplars, free-form labels, or session metadata. Its
top-level shape is:

```json
{
  "schemaVersion": 1,
  "evidenceKind": "gate-0-faro-aggregate",
  "role": "faro-browser-message-ack-latency",
  "signalId": "browser-message-ack-latency",
  "release": {
    "serverCommit": "0123456789abcdef0123456789abcdef01234567",
    "webCommit": "1111111111111111111111111111111111111111"
  },
  "window": {
    "start": "2026-07-10T09:00:00Z",
    "end": "2026-07-10T10:00:00Z"
  },
  "scope": {
    "sourceId": "waddle-chat",
    "deploymentEnvironment": "production",
    "release": "1111111111111111111111111111111111111111",
    "cluster": "waddle-cloud",
    "namespace": "waddle"
  },
  "source": {
    "sourceId": "waddle-chat",
    "query": {
      "schemaVersion": 1,
      "engine": "grafana-faro-aggregate",
      "sourceId": "waddle-chat",
      "deploymentEnvironment": "production",
      "cluster": "waddle-cloud",
      "namespace": "waddle",
      "release": "1111111111111111111111111111111111111111",
      "window": {
        "start": "2026-07-10T09:00:00Z",
        "end": "2026-07-10T10:00:00Z"
      },
      "signalNames": ["chat.xmpp.message.acked.latency_ms"],
      "groupBy": ["kind"],
      "aggregates": ["count", "latency_ms_p50", "latency_ms_p95"]
    },
    "rawSha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    "rowCount": 2
  },
  "dimensions": {
    "kind": ["dm", "room"]
  },
  "series": []
}
```

The `role` is `faro-<signal-id>`. The release tuple and window must match the
telemetry manifest; its `serverCommit` matches Prometheus and its `webCommit`
matches `scope.release`. Faro and Prometheus share deployment environment,
cluster, and namespace, while Prometheus `job` remains Prometheus-only. `scope`
contains only the fixed source ID plus the bounded environment, release,
cluster, and namespace; a project, tenant, community, browser session,
installation, or user identifier is forbidden. `source.query` must equal the
catalog's typed query plan after its source, web release, environment, cluster,
namespace, and window filters are materialized. `source.rawSha256` is the SHA-256 of the restricted input
bytes used for normalization, and `rowCount` is the number of aggregate rows
read, not a raw-event count.

`dimensions` declares the complete closed dimension set: `outcome` with the
four catalog values for auth, `kind` with `dm` and `room` for acknowledgement
latency, `type` with `fresh` and `resumed` for session lifecycle, and `{}` for
reconnect duration. Every `series` item contains only values from that declared
set and numeric aggregates required by the catalog query: counts for events,
and count plus named millisecond percentiles for measurements. The normalizer
must reject missing or additional keys, dimensions, values, non-finite numbers,
duplicate series, and any string that is not fixed provenance or a catalogued
closed value. Sort object keys, dimension values, percentile names, and series
deterministically before hashing.

Prefer an aggregate Grafana query that never retrieves raw events. The checked-in
normalizer validates the aggregate export; it must not be replaced with an
ad-hoc `jq` pipeline or hand-edited envelope. Never add an identity-bearing
dimension to diagnose a sparse result, and never copy the restricted raw input
into the repository for debugging or review.

An empty Faro result is not automatically a valid zero. Confirm that the Faro
source/web release received other beacons in the same window and that the
journey was exercised. If collection was absent or the journey was not
exercised, mark the corresponding aggregate missing and repeat a new complete
window.

## Freeze the critical-journey baseline

Gate 0 `journey-baseline` completion is not a generic test result or a claimed
CI URL. After the critical-journey contract and its production validator are
committed, generate the typed manifest against that exact full commit:

```sh
bun scripts/build-switchable-journey-baseline.ts \
  --commit 0123456789abcdef0123456789abcdef01234567
```

The builder refuses uncommitted source drift and existing output. It validates
the immutable journey IDs, owners, gates, required evidence kinds, every
client/topology/requirement combination, unique evidence records, and derived
journey/gate readiness. The manifest binds both source files and the commit by
SHA-256 and records a deterministic summary and full scenario-matrix digest.
Repository test source existence, an Actions URL, or a self-authored journey
JSON file do not prove that executable or manual behavior passed. Until trusted
passing-run and kind-specific operational, manual/live, and pilot evidence
attestations are implemented, every Gate 1–5 record remains partial. That
includes `e2e`, interop, security, chaos, accessibility, performance, device,
authorization, audit, metric, SLO, restore, pilot, retention, and isolation
records. A hashed typed `manual-schema` artifact may retain privacy-safe context,
but it cannot complete a record by asserting its own passing booleans or
measurements. Repo-test and CI references likewise remain partial.
Only a hashed `journey-baseline-manifest` reference to
`docs/evidence/journey-baseline.manifest.json` can complete this evidence kind.

## Assemble and review evidence

The evidence package must contain, from one release tuple and one fixed window:

- the canonical Prometheus JSON and its generated Markdown summary;
- four strict Faro normalized aggregate JSON envelopes;
- the signal catalog revision/schema version;
- both full deployed Git SHAs;
- the expected replica count; and
- SHA-256 digests for every machine-readable input used by the review.

Choose one explicit UTC finalization instant no earlier than the frozen window
end. Canonical evidence is accepted only when one trusted default-branch GitHub
Actions workflow collected every live input and produced a GitHub/Sigstore
attestation over the deterministic collection subject. The subject binds the
source-locator digests, release tuple, deployment scope, fixed window, and the
digest of every live artifact. It separately binds Faro ingest and Faro query
sources, typed replica provenance consistent with `expectedReplicas`, and the
exact release-artifact provenance described above. The
production verifier pins the GitHub issuer,
repository, workflow identity, workflow and source commits, default-branch ref,
and GitHub-hosted runner policy, and fails closed when `gh` or bundle
verification is unavailable.

The typed artifact/deployment contract exists, but the repository does not yet
contain a verifier that retrieves and verifies the server and web publication
subjects/bundles and independently observes the Kubernetes/Flux/Cloudflare
state. The production verifier therefore always fails closed with the named
`release-artifact-provenance blocker`. Test-only callers may inject a verifier
for synthetic fixtures; canonical finalization may not. A future trusted live
workflow must provide a pinned JSON document through
`WADDLE_RELEASE_ARTIFACT_PROVENANCE_PATH`, but merely supplying that document
does not satisfy the verifier.

The publication identities are deliberately reserved for dedicated
`gate0-server-release-artifacts.yml` and `gate0-web-release-artifacts.yml`
workflows, neither of which exists yet. The generated server and chat release
workflows are not trusted publishers: they use mutable third-party action tags
and download a cuenv executable without verifying a pinned checksum before
running it with release credentials. A future verifier must reject attestations
from those generated workflows even when their source commit is pinned. The
dedicated workflows must pin every action and tool by immutable digest, verify
downloaded bytes before execution, publish role-separated Sigstore bundles,
and run on GitHub-hosted runners before this blocker can be removed.

Independently, the repository does not yet contain a real Grafana Cloud aggregate read/query
adapter for the four Faro signals. The existing manual aggregate normalizer is
privacy-safe but is not an authenticated live origin, so it cannot produce the
trusted workflow attestation. Until both that adapter and trusted
release-artifact verification are implemented against documented deployed
APIs, **Gate 0 remains not-ready and canonical finalization must not be
attempted**. Do not add a proxy URL, upload prebuilt Faro payloads, or accept
caller-supplied release digests as a shortcut.

Once the real adapter and trusted workflow exist, download the workflow-produced
staging directory without modifying it. Finalization is a single operation; the
old separate capability and telemetry publication modes are intentionally not
supported:

```sh
bun scripts/finalize-switchable-baseline.ts all \
  --live-disco "$PWD/target/switchable-baseline-inputs/capability/live-disco-export.json" \
  --prometheus "$PWD/target/switchable-baseline-inputs/prometheus/telemetry-baseline.json" \
  --faro-browser-auth-bootstrap "$PWD/target/switchable-baseline-inputs/faro/browser-auth-bootstrap.json" \
  --faro-browser-message-ack-latency "$PWD/target/switchable-baseline-inputs/faro/browser-message-ack-latency.json" \
  --faro-browser-session-lifecycle "$PWD/target/switchable-baseline-inputs/faro/browser-session-lifecycle.json" \
  --faro-browser-reconnect-duration "$PWD/target/switchable-baseline-inputs/faro/browser-reconnect-duration.json" \
  --collection-subject "$PWD/target/switchable-baseline-inputs/attestation/live-collection-subject.json" \
  --attestation-bundle "$PWD/target/switchable-baseline-inputs/attestation/live-collection.sigstore.json" \
  --server-commit 0123456789abcdef0123456789abcdef01234567 \
  --web-commit 1111111111111111111111111111111111111111 \
  --start 2026-07-10T09:00:00Z \
  --end 2026-07-10T10:00:00Z \
  --captured-at 2026-07-10T10:05:00Z \
  --job waddle-server \
  --deployment-environment production \
  --cluster waddle-cloud \
  --namespace waddle \
  --expected-replicas 2 \
  --identity-metric waddle_build_info \
  --target-signal-id server-deployment-identity-targets \
  --identity-lookback-seconds 3600
```

The finalizer validates every input and the attestation before writing, builds
and fsyncs one hidden generation, then hard-links its files into a newly created
canonical `gate-0` directory without replacing any existing path. The final
attestation link is the commit record: it is linked and fsynced only after the
fully linked hidden generation passes pre-commit validation and every canonical
path, inode, and byte digest is rechecked. Under cooperating finalizers, a failed
activation removes only paths whose observed inode identities still belong to
that transaction. POSIX does not provide portable conditional unlink-by-inode,
so hostile same-user pathname replacement during the final check/unlink gap is
outside this rollback guarantee. A process crash can leave an uncommitted
canonical directory without the final attestation; consumers must reject it and
an operator must remove it after confirming that no finalizer owns the lock.
Stale activation locks are never removed automatically: an operator must verify
the recorded owner and remove a stale lock before retrying.
It never publishes capability and telemetry separately. Finally validate the
shared release, scope, window, generated Markdown, every digest, attestation,
and sealed directory:

```sh
bun scripts/finalize-switchable-baseline.ts verify
# or, from server/: cuenv task verifySwitchableBaseline
```

The generated telemetry manifest has exact top-level fields
`schemaVersion: 1`, `evidenceKind: "telemetry-baseline"`, `status: "complete"`,
the exact `release` object with `serverCommit` and `webCommit`, the shared
`window`, the explicit `capturedAt`, and `artifacts`. Its artifact list contains
exactly these roles:

- `prometheus-baseline`;
- `faro-browser-auth-bootstrap`;
- `faro-browser-message-ack-latency`;
- `faro-browser-session-lifecycle`; and
- `faro-browser-reconnect-duration`.

Every generated artifact entry repeats the same release tuple and window and supplies a
repository-relative JSON path under `docs/evidence` plus the SHA-256 of those
exact bytes. CSV, raw event exports, separate Faro manifests, hand-authored
digests, and hand-authored reconciliation are invalid. Use the finalizer's
returned typed reference in the ledger; it contains `type: "artifact-manifest"`,
the canonical path, and the generated manifest SHA-256. The repository
validator rejects missing roles, mixed provenance, hash drift, path escapes,
Markdown substitutes, or a non-complete manifest. It also verifies
the complete checked-in Faro emission/scope/schema/privacy source contract at
`webCommit`, and the complete server metric definition/increment,
build-identity, chart-label, Alloy-scrape, trace/privacy, and signal-catalog
source contract at `serverCommit`; one commit cannot silently stand in for the
other.

A complete Gate 0 package is sealed. `docs/evidence/gate-0` must contain
exactly these files and no raw export, scratch file, ad-hoc review note, or
unreferenced directory:

```text
docs/evidence/gate-0/capability-baseline.manifest.json
docs/evidence/gate-0/capability/disco-target-contract.json
docs/evidence/gate-0/capability/live-disco-export.json
docs/evidence/gate-0/capability/capability-reconciliation.json
docs/evidence/gate-0/attestations/live-collection-subject.json
docs/evidence/gate-0/attestations/live-collection.sigstore.json
docs/evidence/gate-0/telemetry-baseline.manifest.json
docs/evidence/gate-0/telemetry-baseline.json
docs/evidence/gate-0/telemetry-baseline.md
docs/evidence/gate-0/faro/browser-auth-bootstrap.json
docs/evidence/gate-0/faro/browser-message-ack-latency.json
docs/evidence/gate-0/faro/browser-session-lifecycle.json
docs/evidence/gate-0/faro/browser-reconnect-duration.json
```

The target-contract artifact must be an exact byte copy of
`server/disco-target-contract.json` at `serverCommit`. The capability manifest
contains exactly the target-contract, live-disco, and reconciliation roles.
Live disco records the exact release server commit, deployment scope, fixed
window, and a capture time inside that window. It records every canonical
target slug in contract order, retains only category/type identities and sorted
feature namespaces, and never retains a resolved JID or identity name. Every
successful observation may contain only that target's `claimable_features`.
Exact and runtime-extensible targets must match `required_features` after
removing any checked-in `independently_optional_features`. Runtime-dependent
targets must match one complete `runtime_feature_variants` entry after removing
those curated extensions; this accepts configured ISR, calls, and MUC room
modes without accepting impossible mixtures such as partial call support or
both `muc_open` and `muc_membersonly`. Any skipped configured or dynamic target must be
explicit; a target named by a capability claim cannot be skipped. Each
reconciliation check compares a capability only with features observed on its
declared target, never with a synthetic union across entities.

The Markdown file is generated from the validated Prometheus JSON and its
digest. Its bytes must exactly match the generator; hand-edited commentary is
not permitted inside the sealed directory.

Use repository-relative paths in the typed Gate 0 evidence record. Mark the
`telemetry-baseline` record complete only after another reviewer verifies the
release tuple, window, hashes, closed dimensions, expected replica count, all fourteen
catalogued signals, and the absence of prohibited data. Repository tests prove
the contracts and instrumentation surfaces; they do not prove that the live
deployment emitted correct data.

Keep Gate 0 `not-ready` when any artifact is missing, unhashed, taken from a
different window or release tuple, based on synthetic repository-only data, or not yet
reviewed. The program tracker may link partial evidence, but it must not claim
the baseline or gate is complete.

## Troubleshooting

### A Prometheus series is absent

Confirm the deployed server commit, target health, metric spelling, and that the
relevant path was exercised. Inspect `/metrics` for the family name without
copying unrelated labels into evidence. A query using `or vector(0)` can report
an explicit zero; it must not be used to conceal a missing required metric
family. Reject the window if instrumentation was not deployed or scraped.

### The target count is wrong

Compare `server-deployment-identity-targets` with the intended replica count and
deployment state. Inspect the per-target `waddle_build_info` identity and check
scrape discovery and readiness separately. Do not sum across commits, stale
jobs, namespaces, environments, or clusters to reach the expected number. The
evidence window also fails if target continuity did not hold for the full
one-hour range-lookback pre-window and the identity query's preceding
60-second history interval.

### Counters reset during the window

Check whether a restart or rollout occurred and whether Prometheus retained
continuous samples around it. PromQL `increase` can account for ordinary
counter resets only after they were scraped; process-local increments can
otherwise disappear during restart. Any change in
`server-process-start-continuity`, any mixed release, or any partially scraped
window is invalid Gate 0 evidence. Collect a new full stable window; do not
accept or normalize the reset.

### Delivery counts do not resemble message counts

This is expected when messages fan out to several resources. The live-delivery
counter is per outbound enqueue attempt, while DM archive attempts are typed
sender-pass storage operations and room attempts include server-authored system
messages. Do not derive message-delivery success or unique-user
activity by dividing these families.

### Faro has no matching samples

Verify the deployed web release, runtime collector configuration, beacon
receipt, the exact metric/event name, UTC window, and that the journey was
actually exercised. Browser privacy controls can reduce samples; record that
limitation, but never backfill production evidence with local test output.

### Faro contains an unexpected attribute or value

Stop collection and treat it as a telemetry privacy/cardinality defect. Do not
simply drop the field during export. Preserve only a sanitized defect report,
fix the emitting code and contract, deploy it, and collect a fresh window.

### A safety or drop counter is non-zero

Open an incident with the aggregate signal, release tuple, and UTC window. Investigate
the named failure surface using access-controlled operational data, but keep
identifiers, payloads, logs, and traces out of the baseline package. Do not
mark Gate 0 ready until the cause and impact are understood and a subsequent
stable window has been reviewed.

### Artifact hashes or commits disagree

Do not repair the metadata by hand. Recompute the digest from the original raw
file, verify which server and web commits were deployed, and recollect the entire shared window
if the evidence came from different releases. Mixed provenance invalidates the
baseline.
