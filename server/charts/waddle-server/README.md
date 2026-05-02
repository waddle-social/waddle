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
helm upgrade --install waddle ./charts/waddle-server \
  --namespace waddle \
  --create-namespace \
  --set config.baseUrl=https://chat.example.com \
  --set xmpp.domain=chat.example.com \
  --set ingress.enabled=true \
  --set ingress.hosts[0].host=chat.example.com \
  --set ingress.tls[0].secretName=chat-example-com-tls \
  --set ingress.tls[0].hosts[0]=chat.example.com
```

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

The chart supports an app secret containing:

- `WADDLE_SESSION_KEY` (required by server features relying on encrypted session data)
- `WADDLE_OCCUPANT_ID_SECRET` (**required**; per-deployment HMAC key for XEP-0421 occupant identifiers; ≥32 bytes)
- `WADDLE_AUTH_PROVIDERS_JSON` (optional; required to enable `/api/auth/*` broker flows)
- `WADDLE_DATABASE_URL` (optional; preferred location for DB DSN with credentials)
- `WADDLE_XMPP_MAM_DATABASE_URL` (optional MAM DSN override)
- `WADDLE_XMPP_INBOX_DATABASE_URL` (optional inbox DSN override)

Provider JSON may also be set in `config.authProvidersJson`, but `secret.authProvidersJson`
is recommended because provider definitions usually include client secrets.

### Required keys when bringing your own Secret

Setting `secret.create=false` and providing `secret.existingSecret=<name>` skips
chart templating entirely. The externally-managed Secret **must** contain at
minimum:

- `WADDLE_SESSION_KEY`
- `WADDLE_OCCUPANT_ID_SECRET`

…plus any other keys this chart documents (database DSNs, auth providers,
SpiceDB preshared key) that your deployment relies on. If
`WADDLE_OCCUPANT_ID_SECRET` is missing, the server hard-fails at startup with
a clear error.

### Default behavior

- `secret.create=true` creates a chart-managed secret
- `secret.manageOccupantIdSecret=true` makes the chart own `WADDLE_OCCUPANT_ID_SECRET`
- If `secret.sessionKey` / `secret.occupantIdSecret` is empty:
  - on first install, a random 64-character alphanumeric value is generated
  - on upgrades, the existing in-cluster value is preserved (via `lookup`)

Set `secret.manageOccupantIdSecret=false` only when `WADDLE_OCCUPANT_ID_SECRET`
is supplied by an external Secret in `extraSecretRefs`; otherwise the server
will fail startup because the occupant secret is required.

> ⚠️ **Do not rotate `WADDLE_OCCUPANT_ID_SECRET` without an explicit migration plan.**
> The XEP-0421 occupant identifier is the only stable per-(room, user) handle for
> client-side identity continuity (recognising users across nick changes). Rotating
> the secret invalidates every previously-issued occupant-id, severing that continuity.
> The chart auto-generates the secret once on first install; back up the resulting
> Kubernetes Secret externally (Velero, sealed-secrets export, password manager) so a
> namespace deletion or etcd loss does not silently rotate the key.

Recommended for stable upgrades:

```bash
helm upgrade --install waddle ./charts/waddle-server \
  --namespace waddle \
  --set secret.sessionKey="$(openssl rand -hex 32)" \
  --set secret.occupantIdSecret="$(openssl rand -base64 48)"
```

Or use an existing secret:

```bash
helm upgrade --install waddle ./charts/waddle-server \
  --namespace waddle \
  --set secret.create=false \
  --set secret.existingSecret=waddle-app-secrets
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
  --set-json secret.authProvidersJson='[{"id":"colony","display_name":"Colony","kind":"oidc","dynamic_client_registration":true,"client_id":"","token_endpoint_auth_method":"none","require_dpop":true,"issuer":"https://colony.waddle.social","scopes":["openid","profile","email"],"subject_claim":"sub","username_claim":"preferred_username","email_claim":"email"}]'
```

## AI provider

The AI chatbot extension emits a typed provider-fallback response. The server
replaces that fallback with a provider answer when AI provider env is valid.

OpenRouter:

```bash
kubectl -n waddle create secret generic waddle-openrouter \
  --from-literal=OPENROUTER_API_KEY=sk-or-...

helm upgrade --install waddle ./charts/waddle-server \
  --namespace waddle \
  --set ai.provider=openrouter \
  --set ai.openrouterReferer=https://waddle.chat \
  --set ai.openrouterTitle=Waddle \
  --set-json extraSecretRefs='["waddle-openrouter"]'
```

When `ai.provider=openrouter`, `ai.model` defaults to `openrouter/free`. Set
`ai.model` to route to a concrete OpenRouter model.

OpenAI:

```bash
kubectl -n waddle create secret generic waddle-openai \
  --from-literal=OPENAI_API_KEY=sk-...

helm upgrade --install waddle ./charts/waddle-server \
  --namespace waddle \
  --set ai.provider=openai \
  --set ai.model=<openai-model> \
  --set-json extraSecretRefs='["waddle-openai"]'
```

## Extensions

`extensions.modules` renders into `WADDLE_EXTENSIONS_JSON`. OCI modules must set
`registry` without a tag and a separate `sha256:<64 hex>` `digest`; chart
rendering fails for mutable tags, missing digests, all-zero placeholder
digests, duplicate names, or official XMPP namespaces used as Waddle-specific
extension namespaces. The chart defaults to no extension modules; release
automation enables and digest-pins the five sample modules after publishing
their WASM OCI artifacts.

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
- Readiness probe: `GET /api/v1/health`

Both are configurable via `probes.*`.
