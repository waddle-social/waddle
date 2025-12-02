# cert-manager Setup Guide

This guide provides comprehensive instructions for deploying and configuring cert-manager for automated TLS certificate management using Let's Encrypt ACME with Cloudflare DNS01 challenge solver.

## Table of Contents

- [Architecture Overview](#architecture-overview)
- [Prerequisites](#prerequisites)
- [Cloudflare API Token Creation](#cloudflare-api-token-creation)
- [Manual Installation](#manual-installation)
- [ClusterIssuer Configuration](#clusterissuer-configuration)
- [Certificate Request Workflow](#certificate-request-workflow)
- [Troubleshooting](#troubleshooting)
- [Security Best Practices](#security-best-practices)
- [Rate Limits and Quotas](#rate-limits-and-quotas)
- [Integration with Gateway API](#integration-with-gateway-api)
- [References](#references)

## Architecture Overview

### cert-manager Components

cert-manager consists of three main components:

1. **Controller** - Watches for Certificate resources and coordinates certificate issuance with ACME servers
2. **Webhook** - Validates and mutates cert-manager resources before they are persisted
3. **Cainjector** - Injects CA bundles into webhook configurations for secure communication

### ACME Protocol Flow

The Automated Certificate Management Environment (ACME) protocol flow with DNS01 challenge:

```
┌──────────────────────────────────────────────────────────────────────────────┐
│                         Certificate Issuance Flow                             │
├──────────────────────────────────────────────────────────────────────────────┤
│                                                                               │
│  1. User creates Certificate resource in Kubernetes                           │
│       ↓                                                                       │
│  2. cert-manager controller creates CertificateRequest                       │
│       ↓                                                                       │
│  3. CertificateRequest triggers Order creation with Let's Encrypt            │
│       ↓                                                                       │
│  4. Let's Encrypt returns DNS01 challenge token                              │
│       ↓                                                                       │
│  5. cert-manager creates TXT record in Cloudflare:                           │
│      _acme-challenge.waddle.social → <challenge-token>                         │
│       ↓                                                                       │
│  6. Let's Encrypt queries DNS for TXT record                                 │
│       ↓                                                                       │
│  7. Challenge verified → Certificate issued                                  │
│       ↓                                                                       │
│  8. cert-manager stores certificate in Kubernetes Secret                     │
│       ↓                                                                       │
│  9. cert-manager deletes TXT record from Cloudflare                          │
│                                                                               │
│  [Auto-renewal triggers at 60 days before expiry - repeats from step 2]      │
│                                                                               │
└──────────────────────────────────────────────────────────────────────────────┘
```

### DNS01 vs HTTP01 Challenge

| Aspect | DNS01 | HTTP01 |
|--------|-------|--------|
| Wildcard Support | ✅ Yes | ❌ No |
| Port Requirement | None | Port 80 open |
| Internal Domains | ✅ Works | ❌ Requires public access |
| Complexity | Higher (API token) | Lower |
| Use Case | Production, wildcards | Simple deployments |

This setup uses DNS01 with Cloudflare for maximum flexibility, including wildcard certificate support.

## Prerequisites

Before installing cert-manager, ensure:

### Cluster Requirements

1. **Talos Kubernetes cluster with Cilium CNI** (Phase 6 complete)
   ```bash
   # Verify nodes are ready
   kubectl get nodes
   
   # Verify Cilium is operational
   kubectl get pods -n kube-system -l k8s-app=cilium
   ```

2. **kubectl and Helm 3.x installed**
   ```bash
   kubectl version --client
   helm version
   ```

### Cloudflare Requirements

1. **Cloudflare account** with at least one domain
2. **Domain DNS managed by Cloudflare** (nameservers pointing to Cloudflare)
3. **API token** with Zone:DNS:Edit permissions

To verify your domain is using Cloudflare DNS:
```bash
dig NS waddle.social +short
# Should return Cloudflare nameservers like:
# ns1.cloudflare.com
# ns2.cloudflare.com
```

## Cloudflare API Token Creation

### Option 1: Using Edit Zone DNS Template (Recommended)

1. Navigate to https://dash.cloudflare.com/profile/api-tokens
2. Click **"Create Token"**
3. Find **"Edit zone DNS"** template and click **"Use template"**
4. Configure token:
   - **Token name:** `cert-manager-dns01` (or descriptive name)
   - **Permissions:** Zone > DNS > Edit (pre-filled)
   - **Zone Resources:** Include > Specific zone > Select your domain(s)
     - For all domains: Include > All zones
5. Click **"Continue to summary"**
6. Review and click **"Create Token"**
7. **Copy the token immediately** - it's shown only once!

### Option 2: Custom Token

1. Navigate to https://dash.cloudflare.com/profile/api-tokens
2. Click **"Create Token"**
3. Click **"Create Custom Token"**
4. Configure:
   - **Token name:** `cert-manager-dns01`
   - **Permissions:** 
     - Zone > DNS > Edit
   - **Zone Resources:**
     - Include > Specific zone > Select domain(s)
5. Leave other settings at defaults
6. Click **"Continue to summary"**
7. Review and click **"Create Token"**
8. **Copy the token immediately**

### Verify Token

Test your token before using it:

```bash
curl -X GET "https://api.cloudflare.com/client/v4/user/tokens/verify" \
  -H "Authorization: Bearer <your-token>" \
  -H "Content-Type: application/json"
```

Expected response:
```json
{
  "result": {
    "id": "...",
    "status": "active"
  },
  "success": true,
  "errors": [],
  "messages": []
}
```

## Manual Installation

### Step 1: Create Namespace

```bash
kubectl create namespace cert-manager
```

### Step 2: Create Cloudflare API Token Secret

```bash
# Create secret with your Cloudflare API token
kubectl create secret generic cloudflare-api-token \
  --from-literal=api-token=<your-cloudflare-api-token> \
  -n cert-manager

# Verify secret was created
kubectl get secret cloudflare-api-token -n cert-manager
```

**Security Note:** Delete any local files or shell history containing the token after creating the secret.

### Step 3: Add Helm Repository

```bash
helm repo add jetstack https://charts.jetstack.io
helm repo update
```

### Step 4: Install cert-manager

```bash
cd /path/to/waddle-infra

helm install cert-manager jetstack/cert-manager \
  --version v1.16.2 \
  --namespace cert-manager \
  --values infrastructure-k8s/cert-manager/helm-values.yaml
```

### Step 5: Verify Installation

```bash
# Check all pods are running
kubectl get pods -n cert-manager

# Expected output (all pods should be Running):
# NAME                                      READY   STATUS    RESTARTS   AGE
# cert-manager-xxxxxxxxx-xxxxx              1/1     Running   0          1m
# cert-manager-cainjector-xxxxxxxxx-xxxxx   1/1     Running   0          1m
# cert-manager-webhook-xxxxxxxxx-xxxxx      1/1     Running   0          1m

# Check CRDs are installed
kubectl get crds | grep cert-manager

# Expected CRDs:
# certificaterequests.cert-manager.io
# certificates.cert-manager.io
# challenges.acme.cert-manager.io
# clusterissuers.cert-manager.io
# issuers.cert-manager.io
# orders.acme.cert-manager.io
```

### Step 6: Create ClusterIssuers

**Important:** First update the email address in the ClusterIssuer files:

```bash
# Edit both files and replace 'admin@waddle.social' with your email
vim infrastructure-k8s/cert-manager/clusterissuer-letsencrypt-staging.yaml
vim infrastructure-k8s/cert-manager/clusterissuer-letsencrypt-production.yaml
```

Apply ClusterIssuers:

```bash
# Apply staging issuer first (for testing)
kubectl apply -f infrastructure-k8s/cert-manager/clusterissuer-letsencrypt-staging.yaml

# Apply production issuer
kubectl apply -f infrastructure-k8s/cert-manager/clusterissuer-letsencrypt-production.yaml

# Verify ClusterIssuers are ready
kubectl get clusterissuer

# Expected output:
# NAME                     READY   AGE
# letsencrypt-staging      True    1m
# letsencrypt-production   True    1m
```

If ClusterIssuers show `Ready: False`, check the events:
```bash
kubectl describe clusterissuer letsencrypt-staging
kubectl describe clusterissuer letsencrypt-production
```

### Step 7: Test Certificate Issuance

**Important:** Edit the test certificate to use a domain you control:

```bash
# Edit and replace 'test.waddle.social' with your domain
vim infrastructure-k8s/cert-manager/verification/test-certificate.yaml
```

Apply and monitor:

```bash
# Apply test certificate
kubectl apply -f infrastructure-k8s/cert-manager/verification/test-certificate.yaml

# Watch certificate status
kubectl get certificate test-certificate -n cert-manager -w

# Expected progression:
# NAME               READY   SECRET                  AGE
# test-certificate   False   test-certificate-tls    5s
# test-certificate   True    test-certificate-tls    45s
```

Cleanup after testing:
```bash
kubectl delete -f infrastructure-k8s/cert-manager/verification/test-certificate.yaml
```

## ClusterIssuer Configuration

### Staging vs Production

| Aspect | Staging | Production |
|--------|---------|------------|
| Server | `acme-staging-v02.api.letsencrypt.org` | `acme-v02.api.letsencrypt.org` |
| Browser Trust | ❌ Not trusted | ✅ Trusted |
| Rate Limits | Very high (30,000/week) | Strict (50/week per domain) |
| Use Case | Testing, development | Production workloads |
| Certificate Chain | Fake LE Root | ISRG Root X1 (trusted) |

**Best Practice:** Always test with staging first, then switch to production.

### Email Configuration

The email address in ClusterIssuers is used for:
- Certificate expiration warnings (sent 20, 10, and 1 days before expiry)
- Account recovery
- Important Let's Encrypt service announcements

Use the same email as configured in `infrastructure/.env.example`:
- `CERT_MANAGER_EMAIL` (preferred)
- `TELEPORT_LETSENCRYPT_EMAIL` (alternative if already configured)

### Multiple DNS Providers

For domains across multiple DNS providers or Cloudflare accounts:

```yaml
# Create additional secrets for each account
kubectl create secret generic cloudflare-api-token-zone2 \
  --from-literal=api-token=<token-for-zone2> \
  -n cert-manager

# Create issuer with selector for specific domains
apiVersion: cert-manager.io/v1
kind: ClusterIssuer
metadata:
  name: letsencrypt-production-zone2
spec:
  acme:
    server: https://acme-v02.api.letsencrypt.org/directory
    email: admin@zone2.com
    privateKeySecretRef:
      name: letsencrypt-production-zone2
    solvers:
      - selector:
          dnsZones:
            - "zone2.com"
        dns01:
          cloudflare:
            apiTokenSecretRef:
              name: cloudflare-api-token-zone2
              key: api-token
```

## Certificate Request Workflow

### Creating Certificates

**Basic Certificate:**
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

**Wildcard Certificate:**
```yaml
apiVersion: cert-manager.io/v1
kind: Certificate
metadata:
  name: wildcard-example-com
  namespace: default
spec:
  secretName: wildcard-example-com-tls
  issuerRef:
    name: letsencrypt-production
    kind: ClusterIssuer
  dnsNames:
    - "*.waddle.social"
    - "waddle.social"  # Wildcard doesn't cover root domain
```

### Certificate Lifecycle

- **Validity:** 90 days (Let's Encrypt standard)
- **Auto-renewal:** Triggered 30 days before expiry (at day 60)
- **Renewal retries:** Exponential backoff on failure
- **Secret format:** Standard Kubernetes TLS secret (`tls.crt`, `tls.key`)

### Monitoring Certificate Status

```bash
# List all certificates
kubectl get certificates -A

# Describe specific certificate
kubectl describe certificate <name> -n <namespace>

# Check certificate expiry
kubectl get secret <secret-name> -n <namespace> -o jsonpath='{.data.tls\.crt}' | \
  base64 -d | openssl x509 -noout -enddate
```

## Troubleshooting

### Certificate Stuck in "Issuing"

Follow the resource chain to find the issue:

```bash
# 1. Check Certificate status
kubectl describe certificate <name> -n <namespace>

# 2. Check CertificateRequest
kubectl get certificaterequest -n <namespace>
kubectl describe certificaterequest <name> -n <namespace>

# 3. Check Order
kubectl get order -n <namespace>
kubectl describe order <name> -n <namespace>

# 4. Check Challenge
kubectl get challenge -n <namespace>
kubectl describe challenge <name> -n <namespace>
```

### Common Errors and Solutions

| Error | Cause | Solution |
|-------|-------|----------|
| `no such host` | DNS propagation delay | Wait 1-2 minutes, retry |
| `DNS record not found` | Wrong API token permissions | Verify token has Zone:DNS:Edit for the domain |
| `Timeout` | Network/firewall issue | Check cluster can reach Cloudflare API (api.cloudflare.com) |
| `rate limit exceeded` | Too many certificate requests | Wait 1 hour, use staging for testing |
| `invalid api token` | Token expired or revoked | Create new token in Cloudflare |
| `NXDOMAIN` | Domain not in Cloudflare | Verify domain DNS is managed by Cloudflare |

### DNS01 Challenge Debugging

```bash
# Check if TXT record was created in Cloudflare
dig TXT _acme-challenge.waddle.social @1.1.1.1

# Check cert-manager controller logs
kubectl logs -n cert-manager -l app.kubernetes.io/component=controller --tail=100 | grep -i error

# Check webhook logs (for validation issues)
kubectl logs -n cert-manager -l app.kubernetes.io/component=webhook --tail=100
```

### Webhook Connection Issues

If certificates fail with webhook errors:

```bash
# Check webhook pod status
kubectl get pods -n cert-manager -l app.kubernetes.io/component=webhook

# Check webhook service
kubectl get svc -n cert-manager cert-manager-webhook

# Test webhook connectivity
kubectl run -it --rm curl --image=curlimages/curl --restart=Never -- \
  curl -k https://cert-manager-webhook.cert-manager.svc:443/validate
```

## Security Best Practices

### API Token Management

1. **Least Privilege:** Create tokens with minimal required permissions
   - Use specific zone access instead of "All zones" when possible
   
2. **Token Rotation:** Rotate tokens every 90 days
   ```bash
   # Create new token in Cloudflare, then update secret
   kubectl create secret generic cloudflare-api-token \
     --from-literal=api-token=<new-token> \
     -n cert-manager \
     --dry-run=client -o yaml | kubectl apply -f -
   ```

3. **Audit Logging:** Enable Cloudflare audit logs to track API usage

4. **Never Commit:** Never store API tokens in Git

### Secret Encryption

For GitOps workflows, consider encrypting secrets:

1. **Sealed Secrets:** Encrypt secrets client-side for safe Git storage
2. **External Secrets:** Fetch secrets from Vault, AWS Secrets Manager, etc.
3. **SOPS:** Mozilla SOPS with age/GPG encryption

### RBAC Configuration

Limit who can create Certificate resources:

```yaml
apiVersion: rbac.authorization.k8s.io/v1
kind: ClusterRole
metadata:
  name: cert-manager-user
rules:
  - apiGroups: ["cert-manager.io"]
    resources: ["certificates"]
    verbs: ["create", "get", "list", "watch"]
```

## Rate Limits and Quotas

### Let's Encrypt Rate Limits

| Limit | Production | Staging |
|-------|------------|---------|
| Certificates per Registered Domain | 50/week | 30,000/week |
| Duplicate Certificates | 5/week | 30,000/week |
| Failed Validations | 5/hour | Unlimited |
| Pending Authorizations | 300/account | 300/account |
| New Orders | 300/3 hours | Unlimited |

### Avoiding Rate Limits

1. **Test with Staging:** Always use `letsencrypt-staging` for development
2. **Use Wildcards:** `*.waddle.social` counts as 1 certificate
3. **Batch Requests:** Request all SANs in one Certificate resource
4. **Monitor Renewals:** Let cert-manager handle renewals automatically

### Cloudflare API Limits

| Plan | Rate Limit |
|------|------------|
| Free | 1,200 requests/5 minutes |
| Pro | 2,400 requests/5 minutes |
| Business | 4,800 requests/5 minutes |
| Enterprise | Custom |

cert-manager typically uses 2-4 API calls per certificate issuance.

## Integration with Gateway API

### Automatic Certificate Provisioning

When using Cilium Gateway API (Phase 10), cert-manager can automatically provision certificates:

```yaml
apiVersion: gateway.networking.k8s.io/v1
kind: Gateway
metadata:
  name: example-gateway
  namespace: default
  annotations:
    cert-manager.io/cluster-issuer: letsencrypt-production
spec:
  gatewayClassName: cilium
  listeners:
    - name: https
      port: 443
      protocol: HTTPS
      hostname: "*.waddle.social"
      tls:
        mode: Terminate
        certificateRefs:
          - name: wildcard-example-com-tls
```

### Manual Certificate + Gateway

```yaml
# Certificate
apiVersion: cert-manager.io/v1
kind: Certificate
metadata:
  name: api-example-com-tls
  namespace: default
spec:
  secretName: api-example-com-tls
  issuerRef:
    name: letsencrypt-production
    kind: ClusterIssuer
  dnsNames:
    - api.waddle.social
---
# Gateway using the certificate
apiVersion: gateway.networking.k8s.io/v1
kind: Gateway
metadata:
  name: api-gateway
spec:
  gatewayClassName: cilium
  listeners:
    - name: https
      port: 443
      protocol: HTTPS
      hostname: api.waddle.social
      tls:
        mode: Terminate
        certificateRefs:
          - name: api-example-com-tls
```

## References

- [cert-manager Documentation](https://cert-manager.io/docs/)
- [cert-manager Cloudflare DNS01 Solver](https://cert-manager.io/docs/configuration/acme/dns01/cloudflare/)
- [Let's Encrypt Documentation](https://letsencrypt.org/docs/)
- [Let's Encrypt Rate Limits](https://letsencrypt.org/docs/rate-limits/)
- [Cloudflare API Documentation](https://developers.cloudflare.com/api/)
- [ACME Protocol RFC 8555](https://datatracker.ietf.org/doc/html/rfc8555)
- [Kubernetes TLS Secrets](https://kubernetes.io/docs/concepts/configuration/secret/#tls-secrets)
