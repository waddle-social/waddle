# Gateway API Setup Guide

This guide covers setting up Gateway API ingress with TLS termination using Cilium and cert-manager.

## Table of Contents

- [Architecture Overview](#architecture-overview)
- [Prerequisites](#prerequisites)
- [DNS Configuration](#dns-configuration)
- [Manual Installation](#manual-installation)
- [TLS Certificate Workflow](#tls-certificate-workflow)
- [Verification](#verification)
- [Troubleshooting](#troubleshooting)
- [Usage Patterns](#usage-patterns)
- [Security Best Practices](#security-best-practices)
- [Multi-Domain Setup](#multi-domain-setup)
- [Integration Examples](#integration-examples)
- [References](#references)

## Architecture Overview

### Gateway API Concepts

Gateway API is the next-generation Kubernetes ingress standard, providing:

- **GatewayClass**: Defines the controller implementation (Cilium)
- **Gateway**: Configures listeners (ports/protocols) and TLS termination
- **HTTPRoute**: Routes HTTP/HTTPS traffic to backend services
- **ReferenceGrant**: Allows cross-namespace references

### Component Interaction

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                              External Traffic                                │
└─────────────────────────────────────────────────┬───────────────────────────┘
                                                  │
                                                  ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│  Cloudflare DNS                                                              │
│  ┌──────────────────────────────────────────────────────────────────────┐  │
│  │  A Record: waddle.social → LoadBalancer IP                              │  │
│  │  A Record: *.waddle.social → LoadBalancer IP (wildcard)                 │  │
│  └──────────────────────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────┬───────────────────────────┘
                                                  │
                                                  ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│  Kubernetes Cluster                                                          │
│  ┌──────────────────────────────────────────────────────────────────────┐  │
│  │  Gateway (gateway-ingress namespace)                                   │  │
│  │  ├── HTTP Listener (:80) → Redirect to HTTPS                          │  │
│  │  └── HTTPS Listener (:443) → TLS Termination                          │  │
│  │       └── Certificate: gateway-tls (from cert-manager)                │  │
│  └──────────────────────────────────────────────────────────────────────┘  │
│                                      │                                       │
│                                      ▼                                       │
│  ┌──────────────────────────────────────────────────────────────────────┐  │
│  │  HTTPRoute (application namespace)                                     │  │
│  │  ├── Hostname: app.waddle.social                                        │  │
│  │  └── BackendRef: app-service:8080                                     │  │
│  └──────────────────────────────────────────────────────────────────────┘  │
│                                      │                                       │
│                                      ▼                                       │
│  ┌──────────────────────────────────────────────────────────────────────┐  │
│  │  Service → Pods                                                        │  │
│  └──────────────────────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────────────────────┘
```

### Cilium Integration

Cilium provides the GatewayClass `cilium` which:
- Implements Gateway API specification
- Manages LoadBalancer services
- Handles traffic routing based on HTTPRoute rules
- Integrates with Hubble for observability

### cert-manager TLS Workflow

```
1. Gateway created with cert-manager annotation
2. cert-manager creates Certificate resource
3. cert-manager creates ACME Order with Let's Encrypt
4. Let's Encrypt issues DNS01 challenge
5. cert-manager creates TXT record via Cloudflare API
6. Let's Encrypt verifies DNS record
7. Certificate issued and stored in Kubernetes Secret
8. Gateway uses Secret for TLS termination
```

## Prerequisites

### 1. Cilium with Gateway API (Phase 6)

```bash
# Verify GatewayClass exists
kubectl get gatewayclass cilium

# Expected output:
# NAME     CONTROLLER                      ACCEPTED   AGE
# cilium   io.cilium/gateway-controller   True       ...
```

### 2. cert-manager with ClusterIssuers (Phase 9)

```bash
# Verify ClusterIssuers exist
kubectl get clusterissuer

# Expected output:
# NAME                     READY   AGE
# letsencrypt-staging      True    ...
# letsencrypt-production   True    ...

# Verify Cloudflare API token Secret
kubectl get secret cloudflare-api-token -n cert-manager
```

### 3. Cloudflare Domain

- Domain registered and active in Cloudflare
- DNS managed by Cloudflare (nameservers pointing to Cloudflare)
- API token with Zone:DNS:Edit permission (already configured in Phase 9)

## DNS Configuration

### Step 1: Get LoadBalancer IP

After deploying Gateway, retrieve the assigned LoadBalancer IP:

```bash
# Get Gateway LoadBalancer IP
kubectl get gateway gateway -n gateway-ingress -o jsonpath='{.status.addresses[0].value}'
```

If no IP is assigned, check:
- Cilium IPAM configuration
- MetalLB or cloud LoadBalancer controller

### Step 2: Create A Records in Cloudflare

1. Log into [Cloudflare Dashboard](https://dash.cloudflare.com/)
2. Select your domain
3. Go to **DNS** → **Records**
4. Click **Add Record**

**For root domain:**
- Type: `A`
- Name: `@` (or `waddle.social`)
- IPv4 address: `<LoadBalancer-IP>`
- Proxy status: DNS only (gray cloud) - recommended for direct access
- TTL: Auto

**For wildcard (subdomains):**
- Type: `A`
- Name: `*`
- IPv4 address: `<LoadBalancer-IP>`
- Proxy status: DNS only (gray cloud)
- TTL: Auto

### Step 3: Verify DNS Propagation

```bash
# Check DNS resolution
dig waddle.social +short
# Should show: <LoadBalancer-IP>

dig app.waddle.social +short
# Should show: <LoadBalancer-IP> (wildcard)

# Alternative: nslookup
nslookup waddle.social
```

DNS propagation typically takes 1-5 minutes but can take up to 24 hours globally.

## Manual Installation

### Step 1: Update Configuration

Edit the Gateway manifests with your domain:

```bash
cd infrastructure-k8s/gateway

# Update gateway.yaml
# Replace '*.waddle.social' with your domain
vim gateway.yaml

# Update certificate.yaml
# Replace 'waddle.social' with your domain
vim certificate.yaml
```

### Step 2: Deploy Gateway

```bash
# Create namespace
kubectl apply -f namespace.yaml

# Deploy Gateway
kubectl apply -f gateway.yaml

# Deploy Certificate
kubectl apply -f certificate.yaml
```

### Step 3: Verify Deployment

```bash
# Check Gateway status
kubectl get gateway -n gateway-ingress

# Expected output:
# NAME      CLASS    ADDRESS         PROGRAMMED   AGE
# gateway   cilium   192.168.1.100   True         ...

# Check Certificate status
kubectl get certificate -n gateway-ingress

# Expected output:
# NAME          READY   SECRET        AGE
# gateway-tls   True    gateway-tls   ...
```

### Step 4: Configure DNS

Follow [DNS Configuration](#dns-configuration) steps above.

### Step 5: Test Access

```bash
# Test HTTPS (after DNS propagation)
curl -v https://waddle.social

# Or test with Host header (before DNS)
curl -k -H "Host: waddle.social" https://<LoadBalancer-IP>/
```

## TLS Certificate Workflow

### Staging-First Testing

**Always test with Let's Encrypt staging before production!**

Staging benefits:
- Higher rate limits (unlimited vs 50/week)
- Same challenge workflow
- Certificates not browser-trusted (expected)

```bash
# Deploy staging certificate for testing
cd infrastructure-k8s/gateway/verification
kubectl apply -f namespace.yaml
kubectl apply -f test-certificate-staging.yaml

# Monitor certificate status
kubectl get certificate -n gateway-test -w

# Verify staging certificate issued
kubectl describe certificate test-gateway-tls-staging -n gateway-test

# Cleanup
kubectl delete -f test-certificate-staging.yaml
```

### Production Certificate Issuance

Once staging works, production certificate will use same workflow:

1. Gateway annotation triggers Certificate creation
2. cert-manager contacts Let's Encrypt
3. DNS01 challenge: TXT record created via Cloudflare
4. Certificate issued and stored in Secret
5. Gateway uses Secret for TLS

### Certificate Monitoring

```bash
# Check certificate status
kubectl get certificate -A

# Check certificate expiry
kubectl get certificate gateway-tls -n gateway-ingress -o jsonpath='{.status.notAfter}'

# View certificate details
kubectl get secret gateway-tls -n gateway-ingress -o jsonpath='{.data.tls\.crt}' | \
  base64 -d | openssl x509 -text -noout
```

### Certificate Renewal

cert-manager automatically renews certificates:
- Default renewal window: 30 days before expiry
- Let's Encrypt certificates valid for 90 days
- Renewal is automatic - no action required

## Verification

### Gateway Status

```bash
# List all Gateways
kubectl get gateway -A

# Detailed Gateway status
kubectl describe gateway gateway -n gateway-ingress

# Check conditions:
# - Accepted: Gateway configuration is valid
# - Programmed: Controller has configured data plane
```

### HTTPRoute Status

```bash
# List all HTTPRoutes
kubectl get httproute -A

# Detailed HTTPRoute status
kubectl describe httproute <name> -n <namespace>

# Check conditions:
# - Accepted: Route accepted by Gateway
# - ResolvedRefs: Backend references resolved
```

### External Access Testing

```bash
# Get Gateway IP
export GATEWAY_IP=$(kubectl get gateway gateway -n gateway-ingress -o jsonpath='{.status.addresses[0].value}')

# Test HTTP (should redirect or connect)
curl -v http://$GATEWAY_IP/ -H "Host: waddle.social"

# Test HTTPS with Host header
curl -v -k https://$GATEWAY_IP/ -H "Host: waddle.social"

# Test HTTPS after DNS configured
curl -v https://waddle.social

# Verify TLS certificate
echo | openssl s_client -connect waddle.social:443 -servername waddle.social 2>/dev/null | openssl x509 -noout -text | head -20
```

## Troubleshooting

### Gateway Not Programmed

**Symptom**: Gateway shows `Programmed: False`

**Solutions**:
```bash
# Check Gateway events
kubectl describe gateway gateway -n gateway-ingress

# Check Cilium agent logs
kubectl logs -n kube-system -l k8s-app=cilium -c cilium-agent | grep -i gateway

# Verify GatewayClass
kubectl describe gatewayclass cilium
```

### Certificate Stuck in Issuing

**Symptom**: Certificate shows `Ready: False`, status `Issuing`

**Solutions**:
```bash
# Check certificate details
kubectl describe certificate gateway-tls -n gateway-ingress

# Check challenges
kubectl get challenges -A
kubectl describe challenge <name> -n gateway-ingress

# Check cert-manager logs
kubectl logs -n cert-manager -l app=cert-manager

# Verify Cloudflare API token
kubectl get secret cloudflare-api-token -n cert-manager
```

### DNS Propagation Issues

**Symptom**: DNS01 challenge fails, TXT record not found

**Solutions**:
```bash
# Check TXT record creation
dig _acme-challenge.waddle.social TXT

# Check multiple DNS servers
dig _acme-challenge.waddle.social TXT @1.1.1.1
dig _acme-challenge.waddle.social TXT @8.8.8.8

# Wait for propagation (up to 5 minutes)
# Cloudflare typically propagates in <1 minute
```

### LoadBalancer IP Not Assigned

**Symptom**: Gateway has no external IP

**Solutions**:
```bash
# Check Gateway service
kubectl get svc -n gateway-ingress

# Check Cilium IPAM
kubectl get ciliumloadbalancerippools -A

# Check if MetalLB is configured (if using MetalLB)
kubectl get ipaddresspool -n metallb-system
```

### HTTPRoute Not Working

**Symptom**: Traffic not reaching backend

**Solutions**:
```bash
# Check HTTPRoute status
kubectl describe httproute <name> -n <namespace>

# Verify parentRefs
# Ensure Gateway name/namespace are correct

# Check backend service
kubectl get endpoints <service> -n <namespace>

# Test backend directly
kubectl port-forward svc/<service> -n <namespace> 8080:8080
curl localhost:8080
```

## Usage Patterns

### Path-Based Routing

Route different paths to different backends:

```yaml
apiVersion: gateway.networking.k8s.io/v1
kind: HTTPRoute
metadata:
  name: path-route
spec:
  parentRefs:
    - name: gateway
      namespace: gateway-ingress
  hostnames:
    - api.waddle.social
  rules:
    - matches:
        - path:
            type: PathPrefix
            value: /users
      backendRefs:
        - name: users-service
          port: 8080
    - matches:
        - path:
            type: PathPrefix
            value: /orders
      backendRefs:
        - name: orders-service
          port: 8080
```

### Header-Based Routing

Route based on HTTP headers (e.g., API versioning):

```yaml
apiVersion: gateway.networking.k8s.io/v1
kind: HTTPRoute
metadata:
  name: header-route
spec:
  parentRefs:
    - name: gateway
      namespace: gateway-ingress
  hostnames:
    - api.waddle.social
  rules:
    - matches:
        - headers:
            - name: X-API-Version
              value: v2
      backendRefs:
        - name: api-v2
          port: 8080
    - backendRefs:
        - name: api-v1
          port: 8080
```

### Traffic Splitting (Canary)

Gradually shift traffic between versions:

```yaml
apiVersion: gateway.networking.k8s.io/v1
kind: HTTPRoute
metadata:
  name: canary-route
spec:
  parentRefs:
    - name: gateway
      namespace: gateway-ingress
  hostnames:
    - app.waddle.social
  rules:
    - backendRefs:
        - name: app-stable
          port: 8080
          weight: 90
        - name: app-canary
          port: 8080
          weight: 10
```

### Cross-Namespace Routing with ReferenceGrant

Allow Gateway to route to services in other namespaces:

```yaml
# In target namespace (e.g., my-app)
apiVersion: gateway.networking.k8s.io/v1beta1
kind: ReferenceGrant
metadata:
  name: allow-gateway-routes
  namespace: my-app
spec:
  from:
    - group: gateway.networking.k8s.io
      kind: HTTPRoute
      namespace: my-app
  to:
    - group: ""
      kind: Service
```

## Security Best Practices

### 1. Namespace Isolation

- Keep Gateway in dedicated `gateway-ingress` namespace
- Use RBAC to restrict Gateway/HTTPRoute creation
- Implement ReferenceGrant for cross-namespace routing

### 2. TLS Configuration

- Always use TLS termination at Gateway
- Use ECDSA keys (better performance)
- Monitor certificate expiry

### 3. RBAC for Gateway Resources

```yaml
# Example: Allow dev team to create HTTPRoutes only
apiVersion: rbac.authorization.k8s.io/v1
kind: Role
metadata:
  name: httproute-creator
  namespace: dev
rules:
  - apiGroups: ["gateway.networking.k8s.io"]
    resources: ["httproutes"]
    verbs: ["get", "list", "create", "update", "delete"]
```

### 4. Rate Limiting

Configure rate limiting via Cilium Network Policies or HTTPRoute filters (when supported).

## Multi-Domain Setup

### Multiple Certificates

For separate certificates per domain:

```yaml
# Domain 1
apiVersion: cert-manager.io/v1
kind: Certificate
metadata:
  name: domain1-tls
  namespace: gateway-ingress
spec:
  secretName: domain1-tls
  issuerRef:
    name: letsencrypt-production
    kind: ClusterIssuer
  dnsNames:
    - domain1.com
    - www.domain1.com

---
# Domain 2
apiVersion: cert-manager.io/v1
kind: Certificate
metadata:
  name: domain2-tls
  namespace: gateway-ingress
spec:
  secretName: domain2-tls
  issuerRef:
    name: letsencrypt-production
    kind: ClusterIssuer
  dnsNames:
    - domain2.com
    - www.domain2.com
```

### Gateway with Multiple Listeners

```yaml
apiVersion: gateway.networking.k8s.io/v1
kind: Gateway
metadata:
  name: multi-domain-gateway
  namespace: gateway-ingress
spec:
  gatewayClassName: cilium
  listeners:
    - name: domain1-https
      protocol: HTTPS
      port: 443
      hostname: "*.domain1.com"
      tls:
        mode: Terminate
        certificateRefs:
          - name: domain1-tls
    - name: domain2-https
      protocol: HTTPS
      port: 443
      hostname: "*.domain2.com"
      tls:
        mode: Terminate
        certificateRefs:
          - name: domain2-tls
```

## Integration Examples

### Gateway with Hubble Observability

View Gateway traffic in Hubble UI:

```bash
# Port-forward Hubble UI
kubectl port-forward -n kube-system svc/hubble-ui 12000:80

# Open http://localhost:12000
# Filter by gateway-ingress namespace
```

### Gateway Metrics (Prometheus)

Cilium exposes Gateway metrics for Prometheus:

```bash
# View Cilium agent metrics
kubectl port-forward -n kube-system ds/cilium 9962:9962
curl localhost:9962/metrics | grep gateway
```

## References

- [Gateway API Specification](https://gateway-api.sigs.k8s.io/)
- [Cilium Gateway API Documentation](https://docs.cilium.io/en/stable/network/servicemesh/gateway-api/gateway-api/)
- [cert-manager Gateway API Integration](https://cert-manager.io/docs/usage/gateway/)
- [Let's Encrypt Rate Limits](https://letsencrypt.org/docs/rate-limits/)
- [Cloudflare DNS API](https://developers.cloudflare.com/api/operations/dns-records-for-a-zone-list-dns-records)
