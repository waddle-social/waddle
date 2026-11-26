# `waddle-next` Preview Verification, Cutover, and Decommission Runbook

This runbook covers bringing up the side-by-side `waddle-next` deployment
(managed by `waddle-operator` on **Turso + OpenFGA on SQLite + Credential Broker
with Scaleway KMS**), verifying it on the preview URL
`https://next.apps.waddle.chat`, cutting over production traffic
(`xmpp.waddle.social` and `51.159.109.202:5269` S2S) from the legacy `waddle`
namespace, and decommissioning the legacy CloudNativePG + SpiceDB stack.

Markers: **[DESTRUCTIVE]** discards state or interrupts live sessions;
**[reversible]** is undone by the stated command or git revert.

## Safety invariant

Until [Section 4 (Production cutover)](#4-production-cutover-waddle--waddle-next),
the legacy `waddle` namespace (`HelmRelease/waddle-server`, CNPG PostgreSQL,
SpiceDB, `xmpp.waddle.social`, `51.159.109.202:5269`, and
`infrastructure/waddle.cloud/gitops/waddle-server/`) remains **100% untouched**.
`waddle-next` runs completely side by side in its own `waddle-next` namespace
with its own Mayastor PVCs (`waddle-data` and `openfga-data`), its own
OpenFGA SQLite store, and its own preview route on `next.apps.waddle.chat`.

---

## Before you start

1. Obtain a kubeconfig for the production cluster (`waddle-social.1password.eu`,
   vault `waddle-infra`):

   ```sh
   cd infrastructure/waddle.cloud
   op signin --account waddle-social.1password.eu
   go run . cluster access --cluster production \
     --kubeconfig-out ~/.kube/waddle-production \
     --talosconfig-out ~/.talos/waddle-production
   export KUBECONFIG=~/.kube/waddle-production
   ```

2. Ensure you have `kubectl`, `flux`, `curl`, `jq`, `openssl`, and `op` in your
   PATH (the repository's `nix develop` shell provides `kubectl`, `flux`, and `jq`).

3. Confirm the legacy production stack is healthy before touching anything:

   ```sh
   kubectl -n waddle get pods,helmrelease,httproute,svc
   ```

---

## 1. Required 1Password items (`waddle-infra` vault)

`ClusterSecretStore/onepassword` syncs from the `waddle-infra` vault in
`waddle-social.1password.eu`. Create the **three new items** below before
reconciling `infra-waddle-next`.

### New items to create

| 1Password Item | Property | Target K8s Secret (`waddle-next`) | Purpose |
| --- | --- | --- | --- |
| `waddle-next-openfga` | `preshared-key` | `openfga-auth` (`api-token`) | Bearer preshared token shared by `Deployment/openfga` (`OPENFGA_AUTHN_PRESHARED_KEYS`) and `WaddleServer/waddle` (`WADDLE_OPENFGA_API_TOKEN`) |
| `waddle-next-scaleway-kms` | `auth-token` | `scaleway-kms-secret` (`auth-token`) | Scaleway IAM API secret key with Key Manager permissions on project `da8a176b-80b5-46af-9401-44147a989863` in region `fr-par` (`WADDLE_SCW_KMS_AUTH_TOKEN`) |
| `waddle-next-runtime` | `session-key` | `waddle-runtime-secrets` (`WADDLE_SESSION_KEY`) | Fresh >=32-byte session signing key isolated from legacy `waddle` |
| `waddle-next-runtime` | `occupant-id-secret` | `waddle-runtime-secrets` (`WADDLE_OCCUPANT_ID_SECRET`) | Fresh >=32-byte XEP-0421 occupant-ID HMAC secret isolated from legacy `waddle` |

> **Important:** Do **NOT** create or mount a static OpenRouter API key in
> `waddle-next`. Inference provider keys are enrolled per-user/tenant through
> the broker over client-sealed `urn:waddle:credential:1` XMPP IQs and envelope-
> encrypted with Scaleway KMS.

Scaleway KMS parameters configured in `WaddleServer/waddle`:
- **Base URL:** `https://api.scaleway.com`
- **Region:** `fr-par`
- **Project ID:** `da8a176b-80b5-46af-9401-44147a989863` (matches `infrastructure/waddle.cloud/clusters/production.yaml`)

Create the three items with the 1Password CLI:

```sh
VAULT="waddle-infra"
ACCOUNT="waddle-social.1password.eu"

# 1. OpenFGA preshared key
op item create --account "$ACCOUNT" --vault "$VAULT" \
  --category "API Credential" \
  --title "waddle-next-openfga" \
  "preshared-key[password]=$(openssl rand -hex 32)"

# 2. Scaleway KMS IAM secret key (replace <SCW_SECRET_KEY> with the real IAM secret key)
op item create --account "$ACCOUNT" --vault "$VAULT" \
  --category "API Credential" \
  --title "waddle-next-scaleway-kms" \
  "auth-token[password]=<SCW_SECRET_KEY>" \
  "project-id[text]=da8a176b-80b5-46af-9401-44147a989863" \
  "region[text]=fr-par"

# 3. Runtime signing secrets (fresh keys isolated from legacy waddle)
op item create --account "$ACCOUNT" --vault "$VAULT" \
  --category "Password" \
  --title "waddle-next-runtime" \
  "session-key[password]=$(openssl rand -hex 32)" \
  "occupant-id-secret[password]=$(openssl rand -hex 32)"
```

### Existing 1Password items reused (read-only)

These already exist in `waddle-infra` and are synced into `waddle-next` by
`ExternalSecret`s without modifying the legacy `waddle` namespace:

| 1Password Item | Properties | Target K8s Secret (`waddle-next`) |
| --- | --- | --- |
| `server-runtime-production` | `r2-access-key-id`, `r2-secret-access-key`, `github-app-webhook-secret` | `waddle-runtime-secrets` |
| `app-apple` | `file/notifications.p8` | `waddle-apns-production` |
| `livekit-sfu` | `api-key`, `api-secret`, `webhook-secret`, `turn-shared-secret` | `waddle-livekit-config` |

---

## 2. Reconcile `waddle-operator` and `waddle-next`

After the branch is merged to `main` and CI has published:
- `ghcr.io/waddle-social/waddle:main`
- `ghcr.io/waddle-social/waddle-operator:main`
- `oci://ghcr.io/waddle-social/waddle/gitops:latest`

Trigger and observe Flux reconciliation:

```sh
flux reconcile source oci bootstrap -n flux-system
flux reconcile kustomization infra-waddle-operator -n flux-system --with-source
flux reconcile kustomization infra-waddle-next -n flux-system --with-source
```

Verify both Flux Kustomizations are `Ready`:

```sh
flux get kustomizations -n flux-system infra-waddle-operator infra-waddle-next
```

---

## 3. Preview verification checklist (`https://next.apps.waddle.chat`)

Complete every check below before scheduling production cutover.

### 3.1 Operator and CRDs

```sh
kubectl get crd waddleservers.waddle.social waddletenants.waddle.social
kubectl -n waddle-operator-system get pods,deploy,svc
kubectl -n waddle-operator-system logs deploy/waddle-operator --tail=100
```

Expected:
- Both CRDs are `Established=True`.
- `Deployment/waddle-operator` is `1/1 Ready`, leader election lease acquired, zero error loops.

### 3.2 ExternalSecrets, PVCs, and OpenFGA

```sh
kubectl -n waddle-next get externalsecret,secret,pvc,pods,svc
```

Expected:
- All 5 `ExternalSecret`s (`openfga-auth`, `scaleway-kms-secret`, `waddle-runtime-secrets`, `waddle-apns-production`, `waddle-livekit-config`) report `SecretSynced / Ready=True`.
- PVCs `openfga-data` (`5Gi`) and `waddle-data` (`20Gi`) are `Bound` on `openebs-mayastor`.
- `Deployment/openfga` is `1/1 Ready` (`Recreate` strategy, SQLite WAL on `/var/lib/openfga/openfga.db`, zero PostgreSQL pods anywhere in `waddle-next`).

### 3.3 `WaddleServer` and `WaddleTenant` status

```sh
kubectl -n waddle-next get waddleserver waddle -o yaml
kubectl -n waddle-next get waddletenant waddle-social -o yaml
kubectl -n waddle-next get configmap -l waddle.social/server=waddle
```

Expected:
- `WaddleServer/waddle`: `status.phase == Ready`, `status.readySummary == 1/1`, OpenFGA + Turso + Secrets dependencies all `Ready`.
- `WaddleTenant/waddle-social`: `status.phase == Ready`, non-empty `status.openfgaStoreId` and `status.openfgaModelId`.
- Tenant projection ConfigMap `waddle-tenant-waddle-social` exists with `tenantId: waddle.social`, `openfgaStoreId`, and `openfgaModelId`.

### 3.4 Verify OpenFGA store and authorization model

```sh
OPENFGA_TOKEN="$(kubectl -n waddle-next get secret openfga-auth -o jsonpath='{.data.api-token}' | base64 -d)"
STORE_ID="$(kubectl -n waddle-next get waddletenant waddle-social -o jsonpath='{.status.openfgaStoreId}')"
MODEL_ID="$(kubectl -n waddle-next get waddletenant waddle-social -o jsonpath='{.status.openfgaModelId}')"

kubectl -n waddle-next port-forward svc/openfga 18080:8080 >/tmp/openfga-pf.log 2>&1 &
PF_PID=$!
trap 'kill "$PF_PID" 2>/dev/null || true' EXIT
sleep 2

curl -fsS http://127.0.0.1:18080/healthz | jq .
curl -fsS -H "Authorization: Bearer ${OPENFGA_TOKEN}" \
  "http://127.0.0.1:18080/stores/${STORE_ID}" | jq .
curl -fsS -H "Authorization: Bearer ${OPENFGA_TOKEN}" \
  "http://127.0.0.1:18080/stores/${STORE_ID}/authorization-models/${MODEL_ID}" | jq '{id: .authorization_model.id, types: [.authorization_model.type_definitions[].type]}'

kill "$PF_PID" 2>/dev/null || true
```

Expected:
- `/healthz` returns `{"status":"SERVING"}`.
- Store name is `waddle-tenant-waddle.social`.
- Authorization model exists with Waddle's type definitions.

### 3.5 Verify zero static OpenRouter key in pod

```sh
POD="$(kubectl -n waddle-next get pod -l app.kubernetes.io/instance=waddle -o jsonpath='{.items[0].metadata.name}')"
kubectl -n waddle-next exec "$POD" -- env | grep -i openrouter && echo "FAIL: static OpenRouter env found" || echo "OK: no static OpenRouter env"
kubectl -n waddle-next exec "$POD" -- test ! -d /var/run/secrets/waddle-ai && echo "OK: no static AI secret mount"
```

### 3.6 Preview HTTPS, Colony OIDC, and WSS XMPP

1. Confirm HTTP→HTTPS redirect and health endpoints:

   ```sh
   curl -sSI http://next.apps.waddle.chat/health | grep -E 'HTTP/|location:'
   curl -fsS https://next.apps.waddle.chat/health
   curl -fsS https://next.apps.waddle.chat/ready
   curl -fsS https://next.apps.waddle.chat/.well-known/host-meta
   ```

2. Open the Waddle web client pointed at `https://next.apps.waddle.chat` and complete **Colony OIDC** login (`https://colony.waddle.social` with DPoP):
   - Confirm OIDC callback completes and mints a session.
   - Confirm WebSocket upgrade to `wss://next.apps.waddle.chat/ws` succeeds.
   - Create a test room, post messages, verify MAM archive fetch and permission checks succeed.

### 3.7 Broker OpenRouter enrollment (`urn:waddle:credential:1`), `/ai`, and `jev-judgments`

1. In an authenticated XMPP client session against `wss://next.apps.waddle.chat/ws`, enroll a test OpenRouter API key via the broker's client-sealed `urn:waddle:credential:1` server-domain IQ flow:
   - Send `<iq type="set" to="waddle.social"><enroll-device xmlns="urn:waddle:credential:1">...</enroll-device></iq>` to register the P-256 device signing key.
   - Send `<iq type="set" to="waddle.social"><enroll-credential xmlns="urn:waddle:credential:1">...</enroll-credential></iq>` with the ECDH-ES sealed OpenRouter key.
   - Query `<iq type="get" to="waddle.social"><status xmlns="urn:waddle:credential:1"/></iq>` and verify the credential shows `active` without ever returning plaintext key material.
2. Verify in the Scaleway Console (or `scw key-manager key list region=fr-par project-id=da8a176b-80b5-46af-9401-44147a989863`) that the tenant KEK and user wrapping key were created in `fr-par`.
3. In a room on `next.apps.waddle.chat`, invoke `/ai <prompt>` and confirm:
   - The response streams back from OpenRouter via the broker with ZDR enforcement (`zdrRequired: true`).
   - Server logs contain zero raw API key strings and show broker admission ledger recording.
4. Trigger a `jev-judgments` room evaluation and confirm it executes through the brokered inference path cleanly.

### 3.8 Pod restart persistence test

Verify all state survives full pod recreation on both Turso and OpenFGA SQLite PVCs:

```sh
kubectl -n waddle-next rollout restart deploy/openfga deploy/waddle
kubectl -n waddle-next rollout status deploy/openfga --timeout=3m
kubectl -n waddle-next rollout status deploy/waddle --timeout=5m
```

After both pods return `1/1 Ready`:
1. Reconnect to `wss://next.apps.waddle.chat/ws` — verify previously created rooms and messages load from `/var/lib/waddle/tenants` (Turso PVC).
2. Verify OpenFGA permissions still evaluate identically from `/var/lib/openfga/openfga.db` without re-seeding.
3. Re-query `<status xmlns="urn:waddle:credential:1"/>` and invoke `/ai` again — verify the broker custody DB (`/var/lib/waddle/broker/`) and Scaleway KMS unwrap work seamlessly across pod restarts.
4. Check `kubectl -n waddle-next logs deploy/waddle --tail=100` for zero database lock, WAL corruption, or KMS errors.

---

## 4. Production cutover (`waddle` → `waddle-next`)

> **Important:** Because `waddle-gateway` cannot bind two `HTTPRoute`s for the
> same hostname (`xmpp.waddle.social`) on the same listener, and Kubernetes
> cannot assign `externalIPs: [51.159.109.202]` + `nodePort: 30269` to two
> `Service`s simultaneously, suspend legacy `infra-waddle-server` and release
> those two bindings immediately before applying the cutover commit.

### 4.1 Pre-stage TLS certificate in `waddle-next`

Before flipping traffic, create `Certificate/waddle-xmpp-tls` in `waddle-next`
(using DNS-01 or after verifying cert-manager can issue for `xmpp.waddle.social`,
`waddle.social`, `muc.waddle.social`, `spaces.waddle.social`) — or copy the
existing valid `Secret/waddle-xmpp-tls` from `waddle` into `waddle-next` for
zero-wait cutover while cert-manager reconciles the new `Certificate`:

```sh
kubectl -n waddle get secret waddle-xmpp-tls -o json \
  | jq 'del(.metadata.uid,.metadata.resourceVersion,.metadata.creationTimestamp,.metadata.ownerReferences,.metadata.annotations) | .metadata.namespace = "waddle-next"' \
  | kubectl apply -f -
```

### 4.2 Exact GitOps cutover diff

Prepare a PR with the following targeted changes:

1. **`infrastructure/waddle.cloud/gitops/waddle-next/xmpp-certificate.yaml`** (new, added to `waddle-next/kustomization.yaml`):

   ```yaml
   apiVersion: cert-manager.io/v1
   kind: Certificate
   metadata:
     name: waddle-xmpp-tls
     namespace: waddle-next
   spec:
     secretName: waddle-xmpp-tls
     issuerRef:
       name: letsencrypt-production
       kind: ClusterIssuer
     dnsNames:
       - xmpp.waddle.social
       - waddle.social
       - muc.waddle.social
       - spaces.waddle.social
   ```

2. **`infrastructure/waddle.cloud/gitops/waddle-next/s2s-configmap.yaml`** and **`s2s-dns-endpoint.yaml`** (new, added to `waddle-next/kustomization.yaml`):
   - Copy `s2s.toml` from `waddle-server/s2s-configmap.yaml` into `ConfigMap/waddle-s2s-config` in namespace `waddle-next` (preserving `local_domains`: `waddle.social`, `muc.waddle.social`, `spaces.waddle.social`, and TLS paths `/etc/waddle/tls/tls.{crt,key}`).
   - Copy `DNSEndpoint/waddle-xmpp-s2s-srv` into namespace `waddle-next`.

3. **`infrastructure/waddle.cloud/gitops/waddle-next/waddleserver.yaml`**:

   ```diff
   -  baseUrl: https://next.apps.waddle.chat
   +  baseUrl: https://xmpp.waddle.social
      xmpp:
        domain: waddle.social
   -    publicWebsocketUrl: wss://next.apps.waddle.chat/ws
   +    publicWebsocketUrl: wss://xmpp.waddle.social/ws
   +    tlsSecretName: waddle-xmpp-tls
   +  s2s:
   +    enabled: true
   +    configMapName: waddle-s2s-config
   +    port: 5269
   +    serviceType: NodePort
   +    nodePort: 30269
   +    externalIps:
   +      - 51.159.109.202
   ```

4. **`infrastructure/waddle.cloud/gitops/waddle-next/httproute.yaml`** and **`httproute-http-redirect.yaml`**:

   Add production `xmpp.waddle.social` routes (attached to `waddle-gateway`
   `sectionName: https-xmpp` and `sectionName: http-xmpp`) pointing to
   `Service/waddle:3000` in `waddle-next`. Keep `next.apps.waddle.chat` or
   remove it after cutover.

   ```yaml
   apiVersion: gateway.networking.k8s.io/v1
   kind: HTTPRoute
   metadata:
     name: waddle-xmpp
     namespace: waddle-next
   spec:
     parentRefs:
       - name: waddle-gateway
         namespace: default
         sectionName: https-xmpp
     hostnames:
       - xmpp.waddle.social
     rules:
       - backendRefs:
           - name: waddle
             port: 3000
   ```

   ```yaml
   apiVersion: gateway.networking.k8s.io/v1
   kind: HTTPRoute
   metadata:
     name: waddle-xmpp-http-redirect
     namespace: waddle-next
   spec:
     parentRefs:
       - name: waddle-gateway
         namespace: default
         sectionName: http-xmpp
     hostnames:
       - xmpp.waddle.social
     rules:
       - filters:
           - type: RequestRedirect
             requestRedirect:
               scheme: https
               statusCode: 301
   ```

### 4.3 Execute the cutover window

1. **[reversible]** Suspend legacy `infra-waddle-server` reconciliation so Flux
   does not fight the release of `HTTPRoute` and S2S IP/NodePort:

   ```sh
   flux suspend kustomization infra-waddle-server -n flux-system
   ```

2. **[DESTRUCTIVE for active legacy sessions]** Release `xmpp.waddle.social`
   HTTPRoutes and `51.159.109.202:5269` / NodePort `30269` from legacy `waddle`:

   ```sh
   kubectl -n waddle delete httproute waddle-server waddle-server-http-redirect --ignore-not-found
   kubectl -n waddle delete svc waddle-server-s2s --ignore-not-found
   kubectl -n waddle delete dnsendpoint waddle-xmpp-s2s-srv --ignore-not-found
   ```

3. Merge the cutover PR and reconcile `infra-waddle-next`:

   ```sh
   flux reconcile source oci bootstrap -n flux-system
   flux reconcile kustomization infra-waddle-next -n flux-system --with-source
   kubectl -n waddle-next rollout status deploy/waddle --timeout=5m
   ```

4. Verify production traffic on `xmpp.waddle.social` and S2S port `5269`:

   ```sh
   curl -fsS https://xmpp.waddle.social/health
   curl -fsS https://xmpp.waddle.social/ready
   kubectl -n waddle-next get httproute,svc,waddleserver,waddletenant
   # Verify S2S TCP listener responds with XMPP stream/STARTTLS on 51.159.109.202:5269
   openssl s_client -connect 51.159.109.202:5269 -starttls xmpp -servername waddle.social </dev/null | head -n 25
   ```

5. Verify LiveKit webhook delivery (`https://xmpp.waddle.social/api/v1/livekit/webhook`)
   by placing a test MUC call and confirming Jingle + LiveKit room join succeed.

### 4.4 Instant rollback (before decommission)

Because legacy CNPG PostgreSQL, SpiceDB, and `HelmRelease/waddle-server` are
still intact in namespace `waddle`:

```sh
flux suspend kustomization infra-waddle-next -n flux-system
kubectl -n waddle-next delete httproute waddle-xmpp waddle-xmpp-http-redirect --ignore-not-found
kubectl -n waddle-next delete svc waddle-s2s --ignore-not-found
kubectl -n waddle-next delete dnsendpoint waddle-xmpp-s2s-srv --ignore-not-found
flux resume kustomization infra-waddle-server -n flux-system
flux reconcile kustomization infra-waddle-server -n flux-system --with-source
```

---

## 5. Decommissioning legacy `waddle` (CNPG + SpiceDB + HelmRelease)

Perform this section **only after a full bake window** (recommended 24–72 hours)
with `waddle-next` serving production `xmpp.waddle.social` cleanly.

### 5.1 Remove legacy GitOps entries

Open a PR removing the legacy stack from `infrastructure/waddle.cloud/gitops/`:
1. From `infrastructure/waddle.cloud/gitops/kustomization.yaml`, remove:
   - `kustomization-infra-waddle-server.yaml`
   - `waddle-server-source.yaml`
   - `kustomization-infra-waddle-namespace.yaml`
   - `kustomization-infra-spicedb-operator-source.yaml`
   - `kustomization-infra-spicedb-operator.yaml`
   - `kustomization-infra-cloudnative-pg.yaml` (confirm no other namespace uses CNPG first: `kubectl get clusters.postgresql.cnpg.io -A`)
2. Delete the legacy files and `infrastructure/waddle.cloud/gitops/waddle-server/`.

### 5.2 Delete legacy in-cluster workloads and PVCs **[DESTRUCTIVE]**

```sh
# 1. Remove Flux Kustomization and HelmRelease
flux delete kustomization infra-waddle-server -n flux-system --silent || true
kubectl -n waddle delete helmrelease waddle-server --wait --timeout=5m --ignore-not-found

# 2. Delete SpiceDB and both CNPG PostgreSQL clusters + PVCs
kubectl -n waddle delete spicedbcluster spicedb --wait --timeout=5m --ignore-not-found
kubectl -n waddle delete cluster.postgresql.cnpg.io postgresql postgresql-spicedb --wait --timeout=10m --ignore-not-found
kubectl -n waddle delete pvc --all --wait --timeout=5m

# 3. Delete legacy waddle namespace
kubectl delete namespace waddle --wait --timeout=5m

# 4. (Only if no other workload uses them) Remove SpiceDB and CNPG operators
flux delete kustomization infra-spicedb-operator infra-spicedb-operator-source -n flux-system --silent || true
flux delete kustomization infra-cloudnative-pg -n flux-system --silent || true
```

### 5.3 Clean up legacy 1Password secrets

Once legacy `waddle` is completely gone:
- Remove the retired static `openrouter-api-key` and `spicedb-preshared-key`
  fields from `server-runtime-production` in the `waddle-infra` vault (keep
  `r2-access-key-id`, `r2-secret-access-key`, and `github-app-webhook-secret`,
  which are still used by `waddle-next`).
- Confirm no orphaned Mayastor PVs remain:

  ```sh
  kubectl get pv,pvc -A
  ```
