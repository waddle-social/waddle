# cert-manager for Talos Kubernetes

This directory contains configuration for deploying cert-manager to provide automated TLS certificate management for Kubernetes workloads using Let's Encrypt ACME with Cloudflare DNS01 solver.

## Table of Contents

- [Architecture Overview](#architecture-overview)
- [Prerequisites](#prerequisites)
- [Cloudflare API Token Setup](#cloudflare-api-token-setup)
- [Manual Installation (Phase 9)](#manual-installation-phase-9)
- [ClusterIssuer Configuration](#clusterissuer-configuration)
- [Certificate Request Process](#certificate-request-process)
- [Verification](#verification)
- [Troubleshooting](#troubleshooting)
- [Limitations](#limitations)
- [Files in This Directory](#files-in-this-directory)
- [References](#references)

## Architecture Overview

**cert-manager Components:**
- **Controller:** Watches for Certificate resources and coordinates issuance with ACME servers
- **Webhook:** Validates and mutates cert-manager resources
- **Cainjector:** Injects CA bundles into webhook configurations

**Certificate Issuance Flow:**
```
┌─────────────────────────────────────────────────────────────────┐
│                   Certificate Issuance Flow                      │
├─────────────────────────────────────────────────────────────────┤
│                                                                  │
│  1. Certificate resource created                                 │
│       ↓                                                          │
│  2. cert-manager creates CertificateRequest                     │
│       ↓                                                          │
│  3. CertificateRequest creates Order with Let's Encrypt         │
│       ↓                                                          │
│  4. Order creates Challenge (DNS01)                              │
│       ↓                                                          │
│  5. cert-manager creates TXT record in Cloudflare               │
│       ↓                                                          │
│  6. Let's Encrypt verifies TXT record                            │
│       ↓                                                          │
│  7. Certificate issued and stored in Secret                      │
│       ↓                                                          │
│  8. cert-manager deletes TXT record                              │
│                                                                  │
│  [Auto-renewal at 60 days before expiry]                         │
│                                                                  │
└─────────────────────────────────────────────────────────────────┘
```

**Integration Overview:**
```
┌─────────────────────────────────────────────────────────────────┐
│                     Talos Kubernetes Cluster                     │
├─────────────────────────────────────────────────────────────────┤
│  ┌──────────────────────────────────────────────────────────┐   │
│  │                   cert-manager namespace                  │   │
│  │  ┌─────────────────┐  ┌──────────┐  ┌─────────────────┐  │   │
│  │  │   Controller    │  │  Webhook │  │   Cainjector    │  │   │
│  │  │  (Deployment)   │  │          │  │                 │  │   │
│  │  └────────┬────────┘  └──────────┘  └─────────────────┘  │   │
│  │           │                                               │   │
│  └───────────┼───────────────────────────────────────────────┘   │
│              │                                                   │
│              ↓                                                   │
│    Let's Encrypt ACME ←───────────────→ Cloudflare DNS           │
│    (Certificate Authority)          (DNS01 Challenge Solver)     │
│              │                                                   │
│              ↓                                                   │
│    ┌─────────────────────────────────────────────┐              │
│    │           TLS Secrets in Kubernetes          │              │
│    │  (Used by Gateway API, Ingress, Services)   │              │
│    └─────────────────────────────────────────────┘              │
└─────────────────────────────────────────────────────────────────┘
```

## Prerequisites

Before installing cert-manager, ensure:

1. **Talos cluster with Cilium CNI** (Phase 6 complete)
   - Nodes should be in `Ready` state
   - Verify: `kubectl get nodes`

2. **Cloudflare account with domain**
   - Domain DNS must be managed by Cloudflare
   - Verify in Cloudflare dashboard: DNS → Your domain

3. **Cloudflare API token**
   - Create at: https://dash.cloudflare.com/profile/api-tokens
   - Required permission: Zone:DNS:Edit

4. **kubectl and Helm 3.x installed**
   ```bash
   kubectl version
   helm version
   ```

## Cloudflare API Token Setup

### Option 1: CLI Setup (cURL)

```bash
# Not recommended - use the web UI for better security and audit trail
```

### Option 2: Web UI Setup (Recommended)

1. **Navigate to API Tokens:**
   - Go to https://dash.cloudflare.com/profile/api-tokens
   - Click "Create Token"

2. **Use Template or Create Custom:**
   - **Option A:** Use "Edit zone DNS" template (recommended)
   - **Option B:** Create custom token:
     - Permission: Zone > DNS > Edit
     - Zone Resources: Include > All zones (or specific zone)

3. **Copy Token:**
   - Token is shown only once!
   - Store securely (password manager)

4. **Verify Token:**
   ```bash
   curl -X GET "https://api.cloudflare.com/client/v4/user/tokens/verify" \
     -H "Authorization: Bearer <your-token>" \
     -H "Content-Type: application/json"
   ```

   Expected response:
   ```json
   {"result":{"id":"...","status":"active"},"success":true,...}
   ```

### Permission Explanation

| Permission | Purpose |
|------------|---------|
| `Zone:DNS:Edit` | Create/delete TXT records for DNS01 challenge |

### Security Best Practices

- **Least Privilege:** Use specific zone access instead of "All zones" when possible
- **Token Rotation:** Rotate tokens every 90 days
- **Audit Logging:** Enable Cloudflare audit logs to track API usage
- **Never Commit:** Never store API tokens in Git

## Manual Installation (Phase 9)

### Step 1: Create Namespace

```bash
kubectl create namespace cert-manager
```

### Step 2: Create Cloudflare API Token Secret

```bash
kubectl create secret generic cloudflare-api-token \
  --from-literal=api-token=<your-cloudflare-api-token> \
  -n cert-manager
```

**Verify secret was created:**
```bash
kubectl get secret cloudflare-api-token -n cert-manager
```

### Step 3: Add Helm Repository

```bash
helm repo add jetstack https://charts.jetstack.io
helm repo update
```

### Step 4: Install cert-manager

```bash
cd infrastructure-k8s/cert-manager

helm install cert-manager jetstack/cert-manager \
  --version v1.16.2 \
  --namespace cert-manager \
  --values helm-values.yaml
```

### Step 5: Verify Installation

```bash
# Check all pods are running
kubectl get pods -n cert-manager

# Check CRDs installed
kubectl get crds | grep cert-manager

# Expected CRDs:
# - certificaterequests.cert-manager.io
# - certificates.cert-manager.io
# - challenges.acme.cert-manager.io
# - clusterissuers.cert-manager.io
# - issuers.cert-manager.io
# - orders.acme.cert-manager.io
```

### Step 6: Create ClusterIssuers

**First, update email addresses** in the ClusterIssuer files:
```bash
# Edit clusterissuer-letsencrypt-staging.yaml
# Edit clusterissuer-letsencrypt-production.yaml
# Replace 'admin@waddle.social' with your email
```

**Apply ClusterIssuers:**
```bash
kubectl apply -f clusterissuer-letsencrypt-staging.yaml
kubectl apply -f clusterissuer-letsencrypt-production.yaml
```

**Verify ClusterIssuers are ready:**
```bash
kubectl get clusterissuer
kubectl describe clusterissuer letsencrypt-staging
kubectl describe clusterissuer letsencrypt-production
```

## ClusterIssuer Configuration

### Staging vs Production

| Aspect | Staging | Production |
|--------|---------|------------|
| Trust | NOT trusted by browsers | Trusted by browsers |
| Rate Limit | 30,000 certs/week | 50 certs/week |
| Use Case | Testing, development | Production workloads |
| Certificate Chain | Fake LE Root | ISRG Root X1 |

### Email Configuration

The email address in ClusterIssuers is used for:
- Certificate expiration warnings
- Account recovery
- Important Let's Encrypt announcements

Use the same email as `CERT_MANAGER_EMAIL` or `TELEPORT_LETSENCRYPT_EMAIL` from `.env.example`.

## Certificate Request Process

### Creating a Certificate

```yaml
apiVersion: cert-manager.io/v1
kind: Certificate
metadata:
  name: example-com-tls
  namespace: default
spec:
  secretName: example-com-tls
  issuerRef:
    name: letsencrypt-production
    kind: ClusterIssuer
  dnsNames:
    - waddle.social
    - www.waddle.social
```

### Using with Gateway API

```yaml
apiVersion: gateway.networking.k8s.io/v1
kind: Gateway
metadata:
  name: example-gateway
  annotations:
    cert-manager.io/cluster-issuer: letsencrypt-production
spec:
  listeners:
    - name: https
      port: 443
      protocol: HTTPS
      tls:
        mode: Terminate
        certificateRefs:
          - name: example-com-tls
```

### Certificate Lifecycle

- **Validity:** 90 days
- **Auto-renewal:** 30 days before expiry (at 60 days)
- **Retry:** Automatic with exponential backoff on failure

## Verification

### Quick Test

```bash
# Edit test-certificate.yaml - replace waddle.social with your domain
vim verification/test-certificate.yaml

# Apply test certificate
kubectl apply -f verification/test-certificate.yaml

# Watch certificate status
kubectl get certificate test-certificate -n cert-manager -w

# Check certificate events
kubectl describe certificate test-certificate -n cert-manager

# Verify TLS secret created
kubectl get secret test-certificate-tls -n cert-manager

# Cleanup
kubectl delete -f verification/test-certificate.yaml
```

### Check Certificate Details

```bash
# View certificate content
kubectl get secret test-certificate-tls -n cert-manager \
  -o jsonpath='{.data.tls\.crt}' | base64 -d | openssl x509 -noout -text

# Check issuer
kubectl get secret test-certificate-tls -n cert-manager \
  -o jsonpath='{.data.tls\.crt}' | base64 -d | openssl x509 -noout -issuer
```

## Troubleshooting

### Certificate Stuck in "Issuing"

**Check the chain of resources:**
```bash
# 1. Certificate status
kubectl describe certificate <name> -n <namespace>

# 2. CertificateRequest status
kubectl get certificaterequest -n <namespace>
kubectl describe certificaterequest <name> -n <namespace>

# 3. Order status
kubectl get order -n <namespace>
kubectl describe order <name> -n <namespace>

# 4. Challenge status
kubectl get challenge -n <namespace>
kubectl describe challenge <name> -n <namespace>
```

### Common Issues

| Issue | Cause | Solution |
|-------|-------|----------|
| `no such host` | DNS propagation delay | Wait 1-2 minutes, retry |
| `DNS record not found` | Wrong API token permissions | Verify token has Zone:DNS:Edit |
| `Timeout` | Network/firewall issue | Check cluster can reach Cloudflare API |
| `rate limit exceeded` | Too many certificate requests | Wait, use staging, or different domain |
| `invalid api token` | Token expired or revoked | Create new token in Cloudflare |

### Check Logs

```bash
# cert-manager controller logs
kubectl logs -n cert-manager -l app.kubernetes.io/component=controller --tail=100

# cert-manager webhook logs
kubectl logs -n cert-manager -l app.kubernetes.io/component=webhook --tail=100
```

### DNS01 Challenge Debugging

```bash
# Check if TXT record was created
dig TXT _acme-challenge.waddle.social

# Check Cloudflare API directly
curl -X GET "https://api.cloudflare.com/client/v4/zones/<zone-id>/dns_records?type=TXT&name=_acme-challenge.waddle.social" \
  -H "Authorization: Bearer <token>"
```

## Limitations

**Let's Encrypt Rate Limits:**

| Limit | Value |
|-------|-------|
| Certificates per domain per week | 50 |
| Duplicate certificates per week | 5 |
| Pending authorizations per account | 300 |
| New orders per account per 3 hours | 300 |

**cert-manager Limitations:**

| Feature | Support |
|---------|---------|
| DNS01 Challenge | ✅ Yes (Cloudflare) |
| HTTP01 Challenge | ✅ Yes (not configured) |
| Wildcard Certificates | ✅ Yes (DNS01 only) |
| Multi-cluster | ⚠️ Requires additional config |
| Certificate Revocation | ⚠️ Manual process |

**Certificate Validity:**
- Let's Encrypt certificates are valid for 90 days
- cert-manager auto-renews at 60 days
- Renewal failures are retried with exponential backoff

## Files in This Directory

| File | Description |
|------|-------------|
| `README.md` | This documentation file |
| `helm-values.yaml` | Helm chart values for cert-manager v1.16.2 |
| `kustomization.yaml` | Kustomization for Flux GitOps (Phase 9) |
| `cloudflare-api-token-secret.yaml` | Template for Cloudflare API token Secret |
| `clusterissuer-letsencrypt-staging.yaml` | ClusterIssuer for Let's Encrypt staging |
| `clusterissuer-letsencrypt-production.yaml` | ClusterIssuer for Let's Encrypt production |
| `.gitignore` | Prevents committing sensitive files |
| `verification/test-certificate.yaml` | Test Certificate for verification |

## References

- [cert-manager Documentation](https://cert-manager.io/docs/)
- [Let's Encrypt Documentation](https://letsencrypt.org/docs/)
- [Let's Encrypt Rate Limits](https://letsencrypt.org/docs/rate-limits/)
- [Cloudflare API Documentation](https://developers.cloudflare.com/api/)
- [Kubernetes TLS Secrets](https://kubernetes.io/docs/concepts/configuration/secret/#tls-secrets)
- [ACME Protocol RFC 8555](https://datatracker.ietf.org/doc/html/rfc8555)
