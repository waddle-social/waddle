# waddle-server Helm chart

This chart deploys Waddle Social server (`waddle-server`) to Kubernetes.

## Prerequisites

- Kubernetes `1.26+`
- Helm `3.12+`
- A container image for `waddle-server`

Optional:
- A Kubernetes TLS secret for XMPP listener certificates
- An ingress controller (if enabling ingress)

## Image pinning

Set `image.digest` to render the Deployment image as `repository@sha256:...`.
When `image.digest` is empty, the chart falls back to `image.tag` or
`Chart.appVersion`.

## Install

```bash
SESSION_KEY="<stable value from your secret manager>"
OCCUPANT_ID_SECRET="<stable value from your secret manager>"
# Stable per-deployment UUID for database lineage attestation — mint once
# (uuidgen) and never change it across upgrades or rollbacks.
DEPLOYMENT_UUID="<stable UUID from your secret manager>"

helm upgrade --install waddle ./charts/waddle-server \
  --namespace waddle \
  --create-namespace \
  --set config.baseUrl=https://chat.example.com \
  --set xmpp.domain=chat.example.com \
  --set xmpp.publicWebsocketUrl=wss://chat.example.com/ws \
  --set ingress.enabled=true \
  --set ingress.hosts[0].host=chat.example.com \
  --set ingress.tls[0].secretName=chat-example-com-tls \
  --set ingress.tls[0].hosts[0]=chat.example.com \
  --set-string secret.sessionKey="${SESSION_KEY}" \
  --set-string secret.occupantIdSecret="${OCCUPANT_ID_SECRET}" \
  --set-string deployment.uuid="${DEPLOYMENT_UUID}" \
  --set-string deployment.lineageAction=enroll
```

`deployment.lineageAction=enroll` is a one-shot bootstrap for the FIRST
install (or first upgrade to a lineage-aware chart) against a durable
database: it enrolls the database's lineage so pods can become ready.
Remove it in the next upgrade. Without enrollment, pods stay alive but
permanently not-ready. See `docs/operations/db-lineage.md`.

## XMPP TLS secret (recommended)

When `xmpp.enabled=true`, mount a TLS secret and pass it through chart values:

```bash
kubectl create secret tls waddle-xmpp-tls \
  --cert=/path/to/fullchain.pem \
  --key=/path/to/privkey.pem \
  --namespace waddle

helm upgrade --install waddle ./charts/waddle-server \
  --namespace waddle \
  --set xmpp.tls.secretName=waddle-xmpp-tls
```

If `xmpp.tls.secretName` is not set, the chart will not inject `WADDLE_XMPP_TLS_CERT` and
`WADDLE_XMPP_TLS_KEY`, and the server will use its internal defaults.

## Persistence

By default this chart creates a PVC and stores:

- `WADDLE_UPLOAD_DIR`: `<mountPath>/<uploadSubPath>`

Primary database wiring is DSN/driver based for sqlx:

- `WADDLE_DB_DRIVER` (default: `postgres`)
- `WADDLE_DATABASE_URL` (primary DB DSN)
- `WADDLE_XMPP_MAM_DATABASE_URL` (optional MAM DSN override)
- `WADDLE_XMPP_INBOX_DATABASE_URL` (optional inbox DSN override)
- `WADDLE_DB_POOL_SIZE` (`database.poolSize`, default: `10`) — main/shared pool
  connection cap (ADR-0017 element 12).
- `WADDLE_DB_CONTROL_PLANE_POOL_SIZE` (`database.controlPlanePoolSize`, default: `4`)
  — dedicated control-plane pool size for node/claim liveness statements
  (ADR-0017 element 4/12); only opened when `clustering.enabled` is true on a
  Postgres deployment.

Set DSNs with either:

- non-sensitive `database.*` values (ConfigMap), or
- `secret.databaseUrl` / `secret.xmppMamDatabaseUrl` / `secret.xmppInboxDatabaseUrl` (recommended for credentials)

Scaling note:
- The default `accessModes: [ReadWriteOnce]` is typically not compatible with `replicaCount > 1`.
- The chart validates this combination and fails render by default.
- To bypass this guard for storage backends that support your topology, set:
  - `persistence.allowUnsafeRwoScale=true`

To use an existing claim:

```bash
helm upgrade --install waddle ./charts/waddle-server \
  --namespace waddle \
  --set persistence.existingClaim=waddle-data
```

## Secret handling

The chart does not generate secrets. Runtime secrets must be supplied by the
operator through `secret.*` values, `secret.existingSecret`, or
`secret.runtimeSecretName`.

Required runtime keys:

- `WADDLE_SESSION_KEY` — server session token HMAC key and extension launch
  signing key.
- `WADDLE_OCCUPANT_ID_SECRET` — per-deployment HMAC key for XEP-0421 occupant
  identifiers. It must be at least 32 bytes and stable across restarts and
  chart upgrades.

The regular chart Secret (`<release>-waddle-server-secrets`) is rendered only
from explicit `secret.*` values:

- `WADDLE_SESSION_KEY`
- `WADDLE_OCCUPANT_ID_SECRET`
- `WADDLE_AUTH_PROVIDERS_JSON`
- `WADDLE_DATABASE_URL`
- `WADDLE_XMPP_MAM_DATABASE_URL`, `WADDLE_XMPP_INBOX_DATABASE_URL`,
  `WADDLE_XMPP_PUBSUB_DATABASE_URL`
- `WADDLE_SPICEDB_PRESHARED_KEY`

Provider JSON may also be set in `config.authProvidersJson`, but
`secret.authProvidersJson` is recommended because provider definitions usually
include client secrets.

### Required keys

The chart fails rendering when it cannot see an operator-owned source for the
required runtime keys. Use one of:

- `secret.sessionKey` and `secret.occupantIdSecret`, rendered into the chart
  Secret.
- `secret.existingSecret`, pointing at a Secret with both required keys.
- `secret.runtimeSecretName`, pointing at an externally managed Secret with
  both required keys.

> ⚠️ **Do not rotate `WADDLE_OCCUPANT_ID_SECRET` without an explicit migration plan.**
> The XEP-0421 occupant identifier is the only stable per-(room, user) handle for
> client-side identity continuity (recognising users across nick changes). Rotating
> the secret invalidates every previously-issued occupant-id, severing that continuity.
> Store and back up this value in your secret manager. The chart will not
> regenerate it.

Recommended for stable upgrades with values from your secret manager:

```bash
SESSION_KEY="$(op read 'op://waddle-infra/server-runtime-production/session-key')"
OCCUPANT_ID_SECRET="$(op read 'op://waddle-infra/server-runtime-production/occupant-id-secret')"

helm upgrade --install waddle ./charts/waddle-server \
  --namespace waddle \
  --set-string secret.sessionKey="${SESSION_KEY}" \
  --set-string secret.occupantIdSecret="${OCCUPANT_ID_SECRET}"
```

Or use an existing secret:

```bash
helm upgrade --install waddle ./charts/waddle-server \
  --namespace waddle \
  --set secret.create=false \
  --set secret.existingSecret=waddle-app-secrets
```

Or keep the chart Secret for other operator values and source the runtime keys
from an externally managed Secret:

```bash
helm upgrade --install waddle ./charts/waddle-server \
  --namespace waddle \
  --set secret.runtimeSecretName=waddle-runtime-secrets
```

Example provider config (OIDC):

```bash
helm upgrade --install waddle ./charts/waddle-server \
  --namespace waddle \
  --set-json secret.authProvidersJson='[{"id":"google","display_name":"Google","kind":"oidc","issuer":"https://accounts.google.com","client_id":"...","client_secret":"...","scopes":["openid","profile","email"]}]'
```

Example provider config (OIDC public client with PKCE, no client secret):

```bash
helm upgrade --install waddle ./charts/waddle-server \
  --namespace waddle \
  --set-json secret.authProvidersJson='[{"id":"rawkode","display_name":"rawkode.academy","kind":"oidc","issuer":"https://id.rawkode.academy/auth","client_id":"...","token_endpoint_auth_method":"none","scopes":["openid","profile","email"],"subject_claim":"sub","username_claim":"preferred_username","email_claim":"email"}]'
```

Example provider config (Colony + chat custom domain `waddle.chat` with API at `xmpp.waddle.social`):

```bash
helm upgrade --install waddle ./charts/waddle-server \
  --namespace waddle \
  --set config.baseUrl=https://xmpp.waddle.social \
  --set config.corsOrigins='https://waddle.chat,http://localhost:4321' \
  --set xmpp.domain=waddle.social \
  --set xmpp.publicWebsocketUrl=wss://xmpp.waddle.social/ws \
  --set-json secret.authProvidersJson='[{"id":"colony","display_name":"Colony","kind":"oidc","dynamic_client_registration":true,"client_id":"","token_endpoint_auth_method":"none","require_dpop":true,"issuer":"https://colony.waddle.social","scopes":["openid","profile","email"],"subject_claim":"sub","username_claim":"preferred_username","email_claim":"email"}]'
```

## Extensions

`extensions.modules` renders into `WADDLE_EXTENSIONS_JSON`. OCI modules must set
`registry` without a tag and a separate `sha256:<64 hex>` `digest`; chart
rendering fails for mutable tags, missing digests, all-zero placeholder
digests, duplicate names, or official XMPP namespaces used as Waddle-specific
extension namespaces. The chart defaults to no extension modules; production
release automation enables and digest-pins the published extension artifacts.
The production GitOps path wires `ai-chatbot` and `jev-judgments` to OpenRouter through a mounted
1Password-backed Secret file, explicit `capabilityGrants`, and
`allowedHttpOrigins`.

An AI chatbot module must grant only the capabilities it actually uses and
pin the provider origin explicitly, for example:

```yaml
extensions:
  enabled: true
  modules:
    - name: ai-chatbot
      registry: ghcr.io/waddle-social/waddle/extensions/ai-chatbot
      digest: sha256:<published digest>
      namespace: urn:waddle:ai-chatbot:1
      config:
        endpoint: https://api.example.test/v1/chat/completions
        model: waddle-model
      configSecretFiles:
        api_key: /var/run/secrets/waddle-ai/api_key
      capabilityGrants:
        - message.enrich
        - host.mam.read
        - host.members.read
        - host.presence.read
        - host.roster.read
        - host.channels.read
        - host.spaces.read
        - host.message.send
        - outbound.http.request
        - commands
      allowedHttpOrigins:
        - https://api.example.test
```

### Room observation actors

`jev-judgments` is an opt-in room observer. The published module configuration
in `server/deployment.cue` grants only `message.observe`, `room.result.publish`,
and `outbound.http.request`, with an explicit HTTPS origin and mounted secret.
Observation invocations cannot send messages through host imports. The host
archives and publishes returned results through the room owner.

Every observer needs `roomObservation.generation` and an explicit scope:

```yaml
roomObservation:
  generation: 1
  scope:
    kind: rooms
    rooms: [lobby@muc.example.test]
  max_concurrent: 8
runtimeLimits:
  invocation_timeout_ms: 10000
  http_timeout_ms: 4000
  http_max_response_bytes: 65536
  http_max_requests: 1
```

The operator may instead grant `scope: {kind: all-hosted-rooms}`; this is the
explicit production Jev selection. Removing `roomObservation` disables new
observation selection. Concurrency limits apply per server process; replicas
do not share a global provider quota. Admission is fair between queued rooms,
with up to four concurrent invocations in one room and 32 across all observers
in a process. WASM fuel and memory limits are also
bounded by the host.

To revoke an observer, retain its installation and advance its generation with
`scope: { kind: rooms, rooms: [] }`. Deploy that revocation before removing the
module configuration. The shared generation fences older replicas and receipts
obsolete work; simply omitting an installation cannot revoke an older replica.

Increment the generation whenever the WASM artifact, effective configuration
(including a rotated secret), grants, HTTP origins, scope, or runtime limits
change. The host freezes their identity for a generation; reusing a generation
with a different identity fails startup. Update the generation in deployment
configuration and its render assertion together. A rollback must also use a
higher generation. Publish all bundled guests with the server because the
extension WIT contract is version 2.

Room commit notifications start work immediately; a one-second durable sweep
recovers missed/full-mailbox notifications and saved publications. Existing
durable room actors can be restored with their fenced ownership after restart.
Rooms without durable registry storage resume when their actor is opened.
Metrics `waddle.extension.observation.queue.delay`, `.invocation.duration`, and
`.publication.delay` distinguish admission delay, provider time, and publication
delay. The fixed-category completion log includes the plugin, attempt, duration,
and whether the result was saved; it contains no message/provider content.

This cutover removes the bespoke core judgment worker and its environment
flags. It does not drop its existing tables or rewrite archived messages.
Previously queued core-worker rows remain retained for operator inspection;
they are not imported or replayed as extension work. New observations use the
generic durable observer and publication records. Plan the rollout boundary
accordingly if a deployment already has old pending rows.

## Env overrides

This chart supports two extra env mechanisms:

- `extraSecretRefs` injects external Secrets through `envFrom`
- `extraSecretChecksum` can be set from `HelmRelease.spec.valuesFrom` to roll
  pods when externally-managed Secret data changes

- `config.extraEnv`:
  - key/value map rendered into the ConfigMap (non-sensitive env)
- `containerExtraEnv`:
  - list rendered directly into `Deployment.spec.template.spec.containers[0].env`
  - supports `value` and `valueFrom`

## Graceful drain

- `config.drainTimeoutSeconds` sets `WADDLE_DRAIN_TIMEOUT_SECS`.
- `terminationGracePeriodSeconds` controls Kubernetes pod termination grace period.
- The chart validates `terminationGracePeriodSeconds >= config.drainTimeoutSeconds`.

## Probes

- Liveness probe: `GET /health`
- Readiness probe: `GET /ready`

Both are configurable via `probes.*`.
