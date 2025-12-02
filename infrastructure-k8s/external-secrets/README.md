# External Secrets Operator with 1Password

This directory contains manifests for the External Secrets Operator (ESO) integrated with 1Password Connect for secure secret management.

## Overview

Instead of manually creating Kubernetes Secrets, ESO syncs secrets from 1Password:

```
1Password Vault → 1Password Connect → External Secrets Operator → Kubernetes Secrets
```

## Prerequisites

### 1. Create 1Password Connect Server

1. Go to [my.1password.com](https://my.1password.com)
2. Navigate to **Developer Tools** → **Connect Server**
3. Click **New Server**
4. Download the `1password-credentials.json` file
5. Copy the **Connect Token** (shown only once)

### 2. Create 1Password Vault

Create a vault named `waddle-infra` with these items:

| Item Name | Type | Fields |
|-----------|------|--------|
| `proxmox-api` | API Credential | `endpoint`, `api_token`, `ssh_username`, `ssh_password` |
| `proxmox-csi` | API Credential | `endpoint`, `token_id`, `token_secret`, `region` |
| `teleport` | SSH Key | `domain`, `email`, `ssh_public_key`, `ssh_private_key` |
| `cloudflare` | API Credential | `api_token` |
| `grafana` | Login | `username`, `password` |
| `spicedb` | API Credential | `preshared_key` |

## Installation

### Step 1: Create Namespace

```bash
kubectl create namespace external-secrets
```

### Step 2: Create 1Password Credentials Secret

```bash
kubectl create secret generic onepassword-credentials \
  --from-file=1password-credentials.json \
  -n external-secrets
```

### Step 3: Create Connect Token Secret

```bash
kubectl create secret generic onepassword-connect-token \
  --from-literal=token=<your-connect-token> \
  -n external-secrets
```

### Step 4: Apply Base Resources

Using Flux (recommended):
```bash
# Add to clusters/production/infrastructure/kustomization.yaml
```

Or manually:
```bash
kubectl apply -k infrastructure-k8s/external-secrets/base/
```

### Step 5: Apply ExternalSecrets

After ESO and 1Password Connect are running:

```bash
kubectl apply -k infrastructure-k8s/external-secrets/secrets/
```

## Verification

### Check ESO Status

```bash
kubectl get pods -n external-secrets
kubectl logs -n external-secrets -l app.kubernetes.io/name=external-secrets
```

### Check 1Password Connect

```bash
kubectl get pods -n external-secrets -l app.kubernetes.io/name=onepassword-connect
kubectl logs -n external-secrets -l app.kubernetes.io/name=onepassword-connect -c connect-api
```

### Check ExternalSecrets

```bash
kubectl get externalsecrets -A
kubectl get clustersecretstore
```

### Verify Synced Secrets

```bash
# Check if secrets were created
kubectl get secrets -n cert-manager cloudflare-api-token
kubectl get secrets -n csi-proxmox proxmox-csi-credentials
kubectl get secrets -n observability grafana-admin
```

## Troubleshooting

### ExternalSecret Status

```bash
kubectl describe externalsecret <name> -n <namespace>
```

Common issues:
- `SecretSyncedError`: Check 1Password Connect logs
- `SecretNotFound`: Verify item exists in 1Password vault
- `PropertyNotFound`: Check property names match

### 1Password Connect Issues

```bash
# Check API health
kubectl exec -n external-secrets deploy/onepassword-connect -c connect-api -- wget -qO- http://localhost:8080/health
```

### Force Secret Refresh

```bash
kubectl annotate externalsecret <name> -n <namespace> force-sync=$(date +%s) --overwrite
```

## Secret Rotation

Secrets are automatically synced every hour (configurable via `refreshInterval`).

To trigger immediate sync after updating 1Password:

```bash
kubectl annotate externalsecret --all -A force-sync=$(date +%s) --overwrite
```

## Adding New Secrets

1. Add item to 1Password vault `waddle-infra`
2. Create ExternalSecret manifest in `secrets/` directory
3. Add to `secrets/kustomization.yaml`
4. Apply: `kubectl apply -k infrastructure-k8s/external-secrets/secrets/`

Example ExternalSecret:

```yaml
apiVersion: external-secrets.io/v1beta1
kind: ExternalSecret
metadata:
  name: my-secret
  namespace: my-namespace
spec:
  refreshInterval: 1h
  secretStoreRef:
    kind: ClusterSecretStore
    name: onepassword-connect
  target:
    name: my-secret
  data:
    - secretKey: password
      remoteRef:
        key: my-1password-item
        property: password
```
