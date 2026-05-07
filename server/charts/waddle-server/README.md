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
   out-of-band by a `pre-install` / `pre-upgrade` Helm hook Job. Holds
   chart-managed auto-generated keys:
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

The pre-install / pre-upgrade hook moves generation entirely out of the
Helm template phase. The bootstrap Secret is created by `kubectl` from
inside the hook Job and is owned by neither Helm nor the chart, so
subsequent renders never reference it. `helm template` is now byte-stable
across runs.

### Hook lifecycle

- Hooks run on both `pre-install` and `pre-upgrade`.
- The Job is idempotent: it runs `kubectl get` first; if the bootstrap
  Secret already exists it exits 0 without touching anything.
- On a fresh install (no bootstrap Secret, no legacy operator Secret),
  fresh 64-character alphanumeric values are generated.
- On the first `pre-upgrade` from chart 0.1.x (bootstrap Secret missing
  but legacy `<release>-waddle-server-secrets` exists with
  `WADDLE_SESSION_KEY` / `WADDLE_OCCUPANT_ID_SECRET`), the Job copies
  those values forward into the new bootstrap Secret **before** Helm
  rewrites the operator Secret. This preserves XEP-0421 occupant-id
  continuity automatically across the chart upgrade.
- Job, ServiceAccount, Role, and RoleBinding are deleted automatically
  after the Job succeeds (`hook-delete-policy: hook-succeeded`).
- The bootstrap Secret itself has **no Helm ownership labels** and is never
  garbage-collected by `helm uninstall`. Operators must clean it up
  explicitly if they want full namespace teardown.

### Disaster recovery

If the bootstrap Secret is deleted (accidentally or as part of a
namespace wipe) the chart will regenerate it on the next `helm upgrade`.
The hook first looks for surviving values in the legacy
`<release>-waddle-server-secrets`; if none are present (or the operator
Secret has also been deleted), fresh values are generated. Generating a
fresh `WADDLE_OCCUPANT_ID_SECRET` **breaks XEP-0421 occupant-id
continuity** for every existing room/user pair, so the recommended
recovery path is:

- Restore the bootstrap Secret from your external backup (Velero,
  sealed-secrets, password manager) before the next upgrade —
  preserves XEP-0421 continuity.
- Or, accept the rotation and let `helm upgrade` regenerate the keys.

The Job logs which path it took (`migrated WADDLE_OCCUPANT_ID_SECRET
from <name>` vs. `generated fresh WADDLE_OCCUPANT_ID_SECRET`); audit
that line in your operator's deployment logs after a recovery upgrade.

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

Set `secret.autoGenerate=false` to suppress the pre-install / pre-upgrade
hook. In this mode you **must** supply `WADDLE_SESSION_KEY` (and
`WADDLE_OCCUPANT_ID_SECRET` when `manageOccupantIdSecret=true`) through one
of:

- `secret.sessionKey` / `secret.occupantIdSecret` values (rendered into the
  operator Secret).
- `secret.existingSecret` pointing to a pre-existing Secret.
- An entry in `extraSecretRefs`.

The chart fails render with a clear error if none of these are satisfied.

### Default behavior

- `secret.create=true` and `secret.autoGenerate=true` (defaults): the
  pre-install / pre-upgrade hook generates a 64-character alphanumeric
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
> The pre-install / pre-upgrade hook creates the secret once and is idempotent
> on subsequent runs; back up the resulting Kubernetes Secret externally
> (Velero, sealed-secrets export, password manager) so a namespace deletion or
> etcd loss does not silently rotate the key.

Manual rotation (only when intentional and after you have a recovery
plan for the broken occupant-id continuity):

```bash
# Drop both the bootstrap Secret AND the legacy operator Secret keys
# (otherwise the pre-upgrade hook will helpfully migrate them back in).
kubectl -n <ns> delete secret <release>-waddle-server-bootstrap-secrets
kubectl -n <ns> patch secret <release>-waddle-server-secrets \
  --type=json \
  -p='[{"op":"remove","path":"/data/WADDLE_SESSION_KEY"},{"op":"remove","path":"/data/WADDLE_OCCUPANT_ID_SECRET"}]' \
  || true   # operator Secret may not exist if you supply no operator values

helm upgrade --install <release> ./charts/waddle-server
# pre-upgrade hook regenerates fresh values
```

### Migrating from chart < 0.2.0

Two breaking behavior changes versus chart 0.1.x:

1. **Auto-generated keys** (`WADDLE_SESSION_KEY`,
   `WADDLE_OCCUPANT_ID_SECRET`) used to live in
   `<release>-waddle-server-secrets` and were preserved across upgrades
   via `lookup`. They now live in a dedicated bootstrap Secret created
   by a pre-install / pre-upgrade hook (the cause of the change is
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

For (1), the upgrade is **automatic**. On the first `helm upgrade` from
chart 0.1.x to 0.2.x, the `pre-upgrade` hook runs before Helm rewrites
the operator Secret. It detects the surviving auto-generated keys in
`<release>-waddle-server-secrets`, copies them into the new bootstrap
Secret, and only then does Helm rewrite the operator Secret without
those keys. XEP-0421 occupant-id continuity is preserved without any
operator intervention.

Look for these lines in the Job logs to confirm migration succeeded:

```
migrated WADDLE_SESSION_KEY from <release>-waddle-server-secrets
migrated WADDLE_OCCUPANT_ID_SECRET from <release>-waddle-server-secrets
```

If the chart logs `generated fresh WADDLE_OCCUPANT_ID_SECRET` instead,
that is a signal the legacy Secret was missing those keys (e.g. you ran
with `manageOccupantIdSecret=false` and supplied them via
`extraSecretRefs`, in which case migration is a no-op and the existing
external source remains authoritative).

If you prefer to migrate manually (e.g. to vault-style storage),
disable the auto-migration by deleting the legacy keys from
`<release>-waddle-server-secrets` *before* the upgrade, then either
pre-create the bootstrap Secret yourself (`kubectl create secret
generic <release>-waddle-server-bootstrap-secrets ...`) or set
`secret.autoGenerate=false` and supply the keys explicitly.

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
