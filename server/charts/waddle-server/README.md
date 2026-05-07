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

The chart manages two distinct Secrets:

1. **Bootstrap Secret** (`<release>-waddle-server-bootstrap-secrets`) — created
   out-of-band by a `pre-install` Helm hook Job. Holds chart-managed
   auto-generated keys:
   - `WADDLE_SESSION_KEY` — server session encryption key.
   - `WADDLE_OCCUPANT_ID_SECRET` — per-deployment HMAC key for XEP-0421
     occupant identifiers (≥32 bytes). Only included when
     `secret.manageOccupantIdSecret=true` (default).
2. **Operator Secret** (`<release>-waddle-server-secrets`) — regular Helm
   template, populated only from operator-supplied values:
   - `WADDLE_AUTH_PROVIDERS_JSON` (optional; required to enable `/api/auth/*`
     broker flows).
   - `WADDLE_DATABASE_URL` (optional; preferred location for DB DSN with
     credentials).
   - `WADDLE_XMPP_MAM_DATABASE_URL`, `WADDLE_XMPP_INBOX_DATABASE_URL`,
     `WADDLE_XMPP_PUBSUB_DATABASE_URL` (optional DSN overrides).
   - `WADDLE_SPICEDB_PRESHARED_KEY` (required when `spicedb.enabled=true`
     and not supplied via `extraSecretRefs`).
   - Any of the bootstrap keys above when set explicitly via values
     (operator override; `envFrom` ordering puts the operator Secret
     after the bootstrap Secret so operator values win).

Provider JSON may also be set in `config.authProvidersJson`, but
`secret.authProvidersJson` is recommended because provider definitions usually
include client secrets.

### Why two Secrets?

Earlier versions of this chart used Helm's `lookup` function to preserve
auto-generated values across upgrades. `lookup` only works when Helm has
cluster access, so `helm template`, ArgoCD's `helm template` strategy, and
`helm upgrade --dry-run=server` would render a fresh `randAlphaNum 64` value
on every render — producing spurious diffs and (worst case) silent rotation
of secrets that downstream tooling then applied to the cluster. See
[#303](https://github.com/waddle-social/waddle/issues/303).

The pre-install hook moves generation entirely out of the Helm template
phase. The bootstrap Secret is created by `kubectl` from inside the hook Job
and is owned by neither Helm nor the chart, so subsequent renders never
reference it. `helm template` is now byte-stable across runs.

### Hook lifecycle

- Hook runs on `pre-install` only. It creates the bootstrap Secret if it
  does not exist; if it already exists the Job exits cleanly (idempotent).
- Hook is **not** run on `pre-upgrade` — the bootstrap Secret is
  long-lived. Upgrade flows use whatever the cluster already holds.
- Job, ServiceAccount, Role, and RoleBinding are deleted automatically
  after the Job succeeds (`hook-delete-policy: hook-succeeded`).
- The bootstrap Secret itself has **no Helm ownership labels** and is never
  garbage-collected by `helm uninstall`. Operators must clean it up
  explicitly if they want full namespace teardown.

### Disaster recovery

If the bootstrap Secret is deleted (accidentally or as part of a
namespace wipe) the chart will not regenerate it on `helm upgrade` — the
hook is `pre-install` only, by design, so that occupant-id continuity is
never silently rotated. Pods will fail to start with a clear error from
the server about missing `WADDLE_OCCUPANT_ID_SECRET`. Recover by either:

- Restoring the Secret from your external backup (Velero, sealed-secrets,
  password manager) — preserves XEP-0421 occupant-id continuity.
- Re-running `helm uninstall <release> && helm install <release> …` —
  triggers the hook to regenerate fresh values; **breaks** XEP-0421
  occupant-id continuity for every existing room/user pair.

This is the intended trade-off — a noisy failure that demands operator
attention, rather than a silent regeneration that quietly invalidates
client state.

### Required keys when bringing your own Secret

Setting `secret.create=false` and providing `secret.existingSecret=<name>`
skips both the bootstrap hook and the operator Secret template. The
externally-managed Secret **must** contain at minimum:

- `WADDLE_SESSION_KEY`
- `WADDLE_OCCUPANT_ID_SECRET`

…plus any other keys this chart documents (database DSNs, auth providers,
SpiceDB preshared key) that your deployment relies on. If
`WADDLE_OCCUPANT_ID_SECRET` is missing, the server hard-fails at startup with
a clear error.

### Disabling auto-generation

Set `secret.autoGenerate=false` to suppress the pre-install hook. In this
mode you **must** supply `WADDLE_SESSION_KEY` (and
`WADDLE_OCCUPANT_ID_SECRET` when `manageOccupantIdSecret=true`) through one
of:

- `secret.sessionKey` / `secret.occupantIdSecret` values (rendered into the
  operator Secret).
- `secret.existingSecret` pointing to a pre-existing Secret.
- An entry in `extraSecretRefs`.

The chart fails render with a clear error if none of these are satisfied.

### Default behavior

- `secret.create=true` and `secret.autoGenerate=true` (defaults): the
  pre-install hook generates a 64-character alphanumeric
  `WADDLE_SESSION_KEY` and (when `manageOccupantIdSecret=true`)
  `WADDLE_OCCUPANT_ID_SECRET`, persisted in the bootstrap Secret.
- The operator Secret is only created when at least one operator-supplied
  value is set (auth providers, DB DSNs, spicedb key, or explicit
  session/occupant overrides).
- Set `secret.manageOccupantIdSecret=false` only when
  `WADDLE_OCCUPANT_ID_SECRET` is supplied by an external Secret in
  `extraSecretRefs`; otherwise the server will fail startup because the
  occupant secret is required.

> ⚠️ **Do not rotate `WADDLE_OCCUPANT_ID_SECRET` without an explicit migration plan.**
> The XEP-0421 occupant identifier is the only stable per-(room, user) handle for
> client-side identity continuity (recognising users across nick changes). Rotating
> the secret invalidates every previously-issued occupant-id, severing that continuity.
> The pre-install hook creates the secret once; back up the resulting Kubernetes
> Secret externally (Velero, sealed-secrets export, password manager) so a namespace
> deletion or etcd loss does not silently rotate the key.

Manual rotation (only when intentional):

```bash
kubectl -n <ns> delete secret <release>-waddle-server-bootstrap-secrets
helm upgrade --install <release> ./charts/waddle-server   # hook re-runs
```

### Migrating from chart < 0.2.0

Two breaking behavior changes versus chart 0.1.x:

1. **Auto-generated keys** (`WADDLE_SESSION_KEY`,
   `WADDLE_OCCUPANT_ID_SECRET`) used to live in
   `<release>-waddle-server-secrets` and were preserved across upgrades
   via `lookup`. They now live in a dedicated bootstrap Secret created
   by a pre-install hook (the cause of the change is
   [#303](https://github.com/waddle-social/waddle/issues/303)).
2. **Operator-supplied keys** (`WADDLE_AUTH_PROVIDERS_JSON`,
   `WADDLE_DATABASE_URL`, `WADDLE_XMPP_*_DATABASE_URL`,
   `WADDLE_SPICEDB_PRESHARED_KEY`) no longer use `lookup` to preserve
   values across upgrades when the operator stops passing them. This
   was a bug — the chart should reflect the values you actually pass on
   each upgrade — but the silent "preserve" behavior may have masked
   missing values in your `helm upgrade` invocation. Audit your
   operator scripts and ensure all required values are passed on every
   upgrade.

For the bootstrap Secret split, two migration paths:

**Option A — copy existing values into the new bootstrap Secret (preserves
session/occupant-id state):**

```bash
NS=<namespace>
REL=<release>
SK=$(kubectl -n "$NS" get secret "${REL}-waddle-server-secrets" -o jsonpath='{.data.WADDLE_SESSION_KEY}' | base64 -d)
OID=$(kubectl -n "$NS" get secret "${REL}-waddle-server-secrets" -o jsonpath='{.data.WADDLE_OCCUPANT_ID_SECRET}' | base64 -d)
kubectl -n "$NS" create secret generic "${REL}-waddle-server-bootstrap-secrets" \
  --from-literal=WADDLE_SESSION_KEY="$SK" \
  --from-literal=WADDLE_OCCUPANT_ID_SECRET="$OID"
helm upgrade "$REL" ./charts/waddle-server  # the hook detects the existing Secret and is a no-op
```

**Option B — pin existing values in values.yaml (operator override path,
no bootstrap Secret):**

```bash
helm upgrade "$REL" ./charts/waddle-server \
  --set secret.autoGenerate=false \
  --set secret.sessionKey="$SK" \
  --set secret.occupantIdSecret="$OID"
```

If you skip migration, the pre-install hook will not run on upgrade
(`pre-install` only) and the chart will not regenerate the bootstrap
Secret. Pods will still pick up the existing
`<release>-waddle-server-secrets` keys via the operator Secret `envFrom`
entry, so existing deployments keep working — but the next fresh
`helm install` (e.g. into a new namespace) will use the new bootstrap
flow.

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

## Extensions

`extensions.modules` renders into `WADDLE_EXTENSIONS_JSON`. OCI modules must set
`registry` without a tag and a separate `sha256:<64 hex>` `digest`; chart
rendering fails for mutable tags, missing digests, all-zero placeholder
digests, duplicate names, or official XMPP namespaces used as Waddle-specific
extension namespaces. The chart defaults to no extension modules; production
release automation enables and digest-pins the published extension artifacts.
The production GitOps path wires `ai-chatbot` to OpenRouter through a mounted
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
        api_key: /var/run/secrets/waddle-ai/api-key
      capabilityGrants:
        - message.enrich
        - message.observe
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
