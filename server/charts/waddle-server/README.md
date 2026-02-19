# waddle-server Helm chart

This chart deploys Waddle Social server (`waddle-server`) to Kubernetes.

## Prerequisites

- Kubernetes `1.26+`
- Helm `3.12+`
- A container image for `waddle-server`

Optional:
- A Kubernetes TLS secret for XMPP listener certificates
- An ingress controller (if enabling ingress)

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

- `WADDLE_DB_PATH`: `<mountPath>/<dbFileName>`
- `WADDLE_XMPP_MAM_DB`: `<mountPath>/<mamDbFileName>`
- `WADDLE_UPLOAD_DIR`: `<mountPath>/<uploadSubPath>`

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
- `WADDLE_AUTH_PROVIDERS_JSON` (optional; required to enable `/v2/auth/*` broker flows)
- `GITHUB_TOKEN` (optional, for GitHub link enrichment)

Provider JSON may also be set in `config.authProvidersJson`, but `secret.authProvidersJson`
is recommended because provider definitions usually include client secrets.

Default behavior:

- `secret.create=true` creates a chart-managed secret
- If `secret.sessionKey` is empty:
  - on first install, a random key is generated
  - on upgrades, the existing `WADDLE_SESSION_KEY` is preserved (via `lookup`) when present

Recommended for stable upgrades:

```bash
helm upgrade --install waddle ./charts/waddle-server \
  --namespace waddle \
  --set secret.sessionKey="$(openssl rand -hex 32)"
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

## Env overrides

This chart supports two extra env mechanisms:

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
